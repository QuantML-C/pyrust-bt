// 多资产/多周期（联合时间线）回测

use chrono::NaiveDateTime;
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use std::collections::HashMap;

use crate::accounting;
use crate::config::{filter_bars_by_date_range, DateRange};
use crate::engine_single::{extract_bars_data, BacktestEngine};
use crate::model::{BarData, EngineContext, Order, OrderRecord, OrderSide, PositionState, TradeRecord};
use crate::stats::build_result;

impl BacktestEngine {
    /// 多资产/多周期（按联合时间线）回测。feeds: Dict[str, List[bar]]，bar 至少包含 datetime/close，可选 symbol。
    pub(crate) fn _run_multi_impl<'py>(
        &self,
        py: Python<'py>,
        strategy: PyObject,
        feeds: &'py PyAny,
    ) -> PyResult<PyObject> {
        let feeds_dict: &PyDict = feeds.downcast()?;
        let date_range = DateRange::from_config(&self.cfg)?;
        // 预提取每个 feed 的数据
        let mut feed_ids: Vec<String> = Vec::with_capacity(feeds_dict.len());
        let mut feed_bars: Vec<Vec<BarData>> = Vec::with_capacity(feeds_dict.len());
        let mut raw_bar_count: usize = 0;
        let mut filtered_bar_count: usize = 0;
        for (k, v) in feeds_dict.iter() {
            let fid: String = k.extract()?;
            let blist: &PyList = v.downcast()?;
            let raw_bars = extract_bars_data(blist)
                .map_err(|e| PyValueError::new_err(format!("feed '{fid}': {e}")))?;
            raw_bar_count += raw_bars.len();
            let bars_vec = filter_bars_by_date_range(raw_bars, &date_range)?;
            filtered_bar_count += bars_vec.len();
            feed_ids.push(fid);
            feed_bars.push(bars_vec);
        }
        if date_range.is_active() && raw_bar_count > 0 && filtered_bar_count == 0 {
            return Err(PyValueError::new_err(format!(
                "BacktestConfig start/end produced no bars across feeds: start='{}', end='{}'",
                self.cfg.start, self.cfg.end
            )));
        }

        let n_feeds = feed_ids.len();
        let mut idxs: Vec<usize> = vec![0; n_feeds];
        let mut last_snapshot: Vec<Option<BarData>> = vec![None; n_feeds];

        // 投资组合状态
        let mut cash: f64 = self.cfg.cash;
        let mut realized_pnl: f64 = 0.0;
        let mut positions: HashMap<String, (f64, f64)> = HashMap::new(); // symbol -> (position, avg_cost)
        let mut last_price_map: HashMap<String, f64> = HashMap::new();

        // 结果容器
        let mut equity_curve: Vec<(Option<String>, f64)> = Vec::new();
        let mut trades: Vec<TradeRecord> = Vec::new();
        let mut orders_records: Vec<OrderRecord> = Vec::new();
        let mut order_seq: u64 = 1;
        let has_next_multi = strategy.as_ref(py).hasattr("next_multi")?;
        let next_accepts_ctx = self.method_accepts_ctx(py, &strategy, "next")?;

        // 整个 run 复用同一个 ctx 实例（与单资产同为 EngineContext），每个时间步就地更新
        let ctx = Py::new(
            py,
            EngineContext {
                position: 0.0,
                avg_cost: 0.0,
                cash,
                equity: cash,
                bar_index: 0,
                positions: PyDict::new_bound(py).unbind().into_any(),
                last_prices: PyDict::new_bound(py).unbind().into_any(),
            },
        )?;
        strategy.call_method1(py, "on_start", (ctx.bind(py),))?;

        let mut step: usize = 0;
        loop {
            // 找到下一个最小的 datetime（用提取阶段解析好的 NaiveDateTime 比较，
            // 不依赖字符串零填充格式）
            let mut min_dt: Option<NaiveDateTime> = None;
            for f in 0..n_feeds {
                if idxs[f] < feed_bars[f].len() {
                    let dt = feed_bars[f][idxs[f]].parsed_dt;
                    match min_dt {
                        None => min_dt = Some(dt),
                        Some(cur) => {
                            if dt < cur {
                                min_dt = Some(dt);
                            }
                        }
                    }
                }
            }
            let Some(cur_parsed_dt) = min_dt else {
                break;
            };

            // 本步更新的 bars 切片（原始 bar 对象直通，不重建 dict）
            let update_slice = PyDict::new_bound(py);
            let mut cur_dt: Option<String> = None;
            for f in 0..n_feeds {
                if idxs[f] < feed_bars[f].len() {
                    if feed_bars[f][idxs[f]].parsed_dt == cur_parsed_dt {
                        let b = &feed_bars[f][idxs[f]];
                        // 对外保留原始 datetime 字符串
                        if cur_dt.is_none() {
                            cur_dt = b.datetime.clone();
                        }
                        // 更新 last
                        last_snapshot[f] = Some(b.clone());
                        if let Some(sym) = &b.symbol {
                            last_price_map.insert(sym.clone(), b.close);
                        }
                        update_slice.set_item(&feed_ids[f], b.py_obj.clone_ref(py))?;
                        idxs[f] += 1;
                    }
                }
            }
            // 提取阶段已保证 datetime 存在且可解析，cur_dt 必有值
            let cur_dt = cur_dt.unwrap_or_default();

            // 主 feed symbol（与订单解析的默认 symbol 规则一致）
            let default_symbol = if let Some(Some(b)) = last_snapshot.get(0) {
                b.symbol.clone().unwrap_or_else(|| "DEFAULT".to_string())
            } else {
                "DEFAULT".to_string()
            };

            // 汇总净值
            let mut equity: f64 = cash;
            for (sym, (p, _)) in positions.iter() {
                if let Some(lp) = last_price_map.get(sym) {
                    equity += p * lp;
                }
            }

            // 就地更新 ctx：汇总字段 + 逐 symbol 头寸 + last_prices
            {
                let mut c = ctx.bind(py).borrow_mut();
                let (primary_pos, primary_cost) =
                    positions.get(&default_symbol).copied().unwrap_or((0.0, 0.0));
                c.position = primary_pos;
                c.avg_cost = primary_cost;
                c.cash = cash;
                c.equity = equity;
                c.bar_index = step;
                let pd = c.positions.bind(py).downcast::<PyDict>()?;
                pd.clear();
                for (sym, (p, ac)) in positions.iter() {
                    let entry = PyDict::new_bound(py);
                    entry.set_item("position", *p)?;
                    entry.set_item("avg_cost", *ac)?;
                    pd.set_item(sym, entry)?;
                }
                let lp = c.last_prices.bind(py).downcast::<PyDict>()?;
                lp.clear();
                for (k, v) in last_price_map.iter() {
                    lp.set_item(k, v)?;
                }
            }

            // 调用策略：仅当 next_multi 不存在时回退到 next，策略内部异常直接传播
            let action_obj = if has_next_multi {
                strategy.call_method1(py, "next_multi", (update_slice.as_any(), ctx.bind(py)))?
            } else if let Some(Some(b)) = last_snapshot.get(0) {
                let pb = b.py_obj.bind(py);
                if next_accepts_ctx {
                    strategy.call_method1(py, "next", (pb, ctx.bind(py)))?
                } else {
                    strategy.call_method1(py, "next", (pb,))?
                }
            } else {
                py.None()
            };

            // 解析并执行指令（支持 list）
            let orders =
                self.parse_actions_any(action_obj.as_ref(py), &mut order_seq, &default_symbol)?;
            for order in orders {
                let submitted_evt = PyDict::new_bound(py);
                submitted_evt.set_item("event", "submitted")?;
                submitted_evt.set_item("order_id", order.id)?;
                submitted_evt.set_item("side", Self::side_str(order.side))?;
                submitted_evt.set_item("type", Self::order_type_str(order.otype))?;
                submitted_evt.set_item("size", order.size)?;
                submitted_evt.set_item("symbol", &order.symbol)?;
                if let Some(lp) = order.limit_price {
                    submitted_evt.set_item("limit_price", lp)?;
                }
                strategy.call_method1(py, "on_order", (submitted_evt.as_any(),))?;

                // 获取该 symbol 的 last_price
                let Some(lp) = last_price_map.get(&order.symbol).copied() else {
                    let reason = format!("no market price for symbol {}", order.symbol);
                    orders_records.push(Self::order_record(
                        &order,
                        Some(cur_dt.clone()),
                        "rejected",
                        None,
                        0.0,
                        0.0,
                        0.0,
                        Some(reason.clone()),
                    ));
                    let rejected_evt = PyDict::new_bound(py);
                    rejected_evt.set_item("event", "rejected")?;
                    rejected_evt.set_item("order_id", order.id)?;
                    rejected_evt.set_item("reason", reason)?;
                    strategy.call_method1(py, "on_order", (rejected_evt.as_any(),))?;
                    continue;
                };
                if let Some((fill_price, fill_size)) = self.try_match(&order, lp) {
                    let slip = self.cfg.slippage_bps / 10_000.0;
                    let sign = match order.side {
                        OrderSide::Buy => 1.0,
                        OrderSide::Sell => -1.0,
                    };
                    let exec_price = fill_price * (1.0 + sign * slip);
                    let commission = exec_price * fill_size * self.cfg.commission_rate;

                    if let Some(reason) = self.validate_multi_fill(
                        cash,
                        &positions,
                        &last_price_map,
                        &order,
                        exec_price,
                        fill_size,
                        commission,
                    ) {
                        orders_records.push(Self::order_record(
                            &order,
                            Some(cur_dt.clone()),
                            "rejected",
                            None,
                            0.0,
                            0.0,
                            0.0,
                            Some(reason.clone()),
                        ));
                        let rejected_evt = PyDict::new_bound(py);
                        rejected_evt.set_item("event", "rejected")?;
                        rejected_evt.set_item("order_id", order.id)?;
                        rejected_evt.set_item("reason", reason)?;
                        strategy.call_method1(py, "on_order", (rejected_evt.as_any(),))?;
                        continue;
                    }

                    // 更新该 symbol 头寸与组合现金
                    let sp = positions
                        .entry(order.symbol.clone())
                        .or_insert((0.0_f64, 0.0_f64));
                    match order.side {
                        OrderSide::Buy => {
                            let cost = exec_price * fill_size + commission;
                            cash -= cost;
                        }
                        OrderSide::Sell => {
                            let proceeds = exec_price * fill_size - commission;
                            cash += proceeds;
                        }
                    }
                    let realized_delta = accounting::apply_position_fill(
                        &mut sp.0, &mut sp.1, order.side, exec_price, fill_size,
                    );
                    realized_pnl += realized_delta;

                    // 记录交易与回调
                    let side = match order.side {
                        OrderSide::Buy => "BUY".to_string(),
                        OrderSide::Sell => "SELL".to_string(),
                    };
                    trades.push(TradeRecord {
                        order_id: order.id,
                        symbol: order.symbol.clone(),
                        side: side.clone(),
                        price: exec_price,
                        size: fill_size,
                        datetime: Some(cur_dt.clone()),
                        commission,
                        realized_pnl: realized_delta,
                    });
                    orders_records.push(Self::order_record(
                        &order,
                        Some(cur_dt.clone()),
                        "filled",
                        Some(exec_price),
                        fill_size,
                        commission,
                        realized_delta,
                        None,
                    ));
                    let trade_evt = PyDict::new_bound(py);
                    trade_evt.set_item("order_id", order.id)?;
                    trade_evt.set_item("side", side)?;
                    trade_evt.set_item("price", exec_price)?;
                    trade_evt.set_item("size", fill_size)?;
                    trade_evt.set_item("symbol", &order.symbol)?;
                    trade_evt.set_item("datetime", &cur_dt)?;
                    trade_evt.set_item("commission", commission)?;
                    trade_evt.set_item("realized_pnl", realized_delta)?;
                    strategy.call_method1(py, "on_trade", (trade_evt.as_any(),))?;

                    let filled_evt = PyDict::new_bound(py);
                    filled_evt.set_item("event", "filled")?;
                    filled_evt.set_item("order_id", order.id)?;
                    strategy.call_method1(py, "on_order", (filled_evt.as_any(),))?;
                } else {
                    orders_records.push(Self::order_record(
                        &order,
                        Some(cur_dt.clone()),
                        "unfilled",
                        None,
                        0.0,
                        0.0,
                        0.0,
                        None,
                    ));
                }
            }

            // 汇总净值并记录
            let mut equity_step: f64 = cash;
            for (sym, (p, _)) in positions.iter() {
                if let Some(lp) = last_price_map.get(sym) {
                    equity_step += p * lp;
                }
            }
            equity_curve.push((Some(cur_dt.clone()), equity_step));
            step += 1;
        }

        strategy.call_method0(py, "on_stop")?;

        // 复用统一的结果构建（多资产结果的 position/avg_cost 固定为 0，
        // 逐 symbol 头寸由策略通过 ctx / on_trade 获取）
        let pos = PositionState {
            position: 0.0,
            avg_cost: 0.0,
            cash,
            realized_pnl,
        };
        build_result(py, pos, equity_curve, trades, orders_records)
    }

    // 解析多指令：支持 list/tuple；若为单个则返回单元素
    fn parse_actions_any(
        &self,
        action_obj: &PyAny,
        order_seq: &mut u64,
        default_symbol: &str,
    ) -> PyResult<Vec<Order>> {
        if let Ok(seq) = action_obj.downcast::<pyo3::types::PyList>() {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq.iter() {
                // 优先读 item 自带的 symbol 作为默认 symbol
                let mut sym = default_symbol.to_string();
                if let Ok(d) = item.downcast::<PyDict>() {
                    if let Ok(Some(val)) = d.get_item("symbol") {
                        if let Ok(s) = val.extract::<String>() {
                            sym = s;
                        }
                    }
                }
                if let Some(o) = self.parse_action_fast(item, order_seq, &sym)? {
                    out.push(o);
                }
            }
            return Ok(out);
        }
        // Single
        if let Some(o) = self.parse_action_fast(action_obj, order_seq, default_symbol)? {
            return Ok(vec![o]);
        }
        Ok(Vec::new())
    }

    fn validate_multi_fill(
        &self,
        cash: f64,
        positions: &HashMap<String, (f64, f64)>,
        last_price_map: &HashMap<String, f64>,
        order: &Order,
        exec_price: f64,
        fill_size: f64,
        commission: f64,
    ) -> Option<String> {
        if !last_price_map.contains_key(&order.symbol) {
            return Some(format!("no market price for symbol {}", order.symbol));
        }

        let current_position = positions.get(&order.symbol).map(|(p, _)| *p).unwrap_or(0.0);
        let signed_fill = match order.side {
            OrderSide::Buy => fill_size,
            OrderSide::Sell => -fill_size,
        };
        let next_symbol_position = current_position + signed_fill;

        if !self.cfg.allow_short && next_symbol_position < -f64::EPSILON {
            return Some(format!(
                "short selling is disabled for symbol {}",
                order.symbol
            ));
        }

        if self.cfg.reject_on_insufficient_cash && order.side == OrderSide::Buy {
            let cost = exec_price * fill_size + commission;
            if cost > cash + f64::EPSILON {
                return Some(format!(
                    "insufficient cash: required {cost}, available {cash}"
                ));
            }
        }

        if let Some(max_leverage) = self.cfg.max_leverage {
            if max_leverage.is_finite() && max_leverage >= 0.0 {
                let next_cash = match order.side {
                    OrderSide::Buy => cash - exec_price * fill_size - commission,
                    OrderSide::Sell => cash + exec_price * fill_size - commission,
                };

                let mut equity = next_cash;
                let mut exposure = 0.0;
                for (sym, (position, _)) in positions {
                    let next_position = if sym == &order.symbol {
                        next_symbol_position
                    } else {
                        *position
                    };
                    if let Some(price) = last_price_map.get(sym) {
                        equity += next_position * price;
                        exposure += (next_position * price).abs();
                    }
                }
                if !positions.contains_key(&order.symbol) {
                    if let Some(price) = last_price_map.get(&order.symbol) {
                        equity += next_symbol_position * price;
                        exposure += (next_symbol_position * price).abs();
                    }
                }
                if equity <= 0.0 {
                    return Some("order would make account equity non-positive".to_string());
                }
                let leverage = exposure / equity;
                if leverage > max_leverage + f64::EPSILON {
                    return Some(format!(
                        "max leverage exceeded: {leverage} > {max_leverage}"
                    ));
                }
            }
        }

        None
    }
}