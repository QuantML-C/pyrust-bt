// 单资产回测主循环 + bar 提取 + 订单解析/撮合

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};

use crate::accounting;
use crate::config::{filter_bars_by_date_range, parse_datetime_value, BacktestConfig, DateRange};
use crate::model::{
    BarData, EngineContext, Order, OrderRecord, OrderSide, OrderType, PositionState, TradeRecord,
};
use crate::stats::build_result;

// 批量提取bar数据，减少Python调用
// 严格校验：datetime/close 缺失或无法解析时返回带 bar 索引的错误；
// open/high/low/volume 不再预提取（bar 对象直通，策略自行从原始 dict 读取）。
// 同时保留用户传入的原始 bar 对象，调用策略时直通（不重建 dict）。
pub(crate) fn extract_bars_data(bars: &PyList) -> PyResult<Vec<BarData>> {
    let mut bars_data = Vec::with_capacity(bars.len());

    for (bar_idx, item) in bars.iter().enumerate() {
        let bar: &PyDict = item.downcast()?;

        let datetime = match bar.get_item("datetime")? {
            Some(v) if !v.is_none() => Some(v.extract::<String>().map_err(|_| {
                PyValueError::new_err(format!(
                    "bar[{bar_idx}]: 'datetime' must be a string"
                ))
            })?),
            _ => None,
        };
        let dt_text = datetime.as_deref().ok_or_else(|| {
            PyValueError::new_err(format!("bar[{bar_idx}]: missing 'datetime' field"))
        })?;
        let parsed_dt = parse_datetime_value(dt_text, false).ok_or_else(|| {
            PyValueError::new_err(format!(
                "bar[{bar_idx}]: unparseable 'datetime' value: {dt_text}"
            ))
        })?;

        let close = bar
            .get_item("close")?
            .and_then(|v| v.extract::<f64>().ok())
            .ok_or_else(|| {
                PyValueError::new_err(format!(
                    "bar[{bar_idx}]: missing or invalid 'close' field"
                ))
            })?;
        let symbol = bar
            .get_item("symbol")?
            .and_then(|v| v.extract::<String>().ok());

        bars_data.push(BarData {
            datetime,
            parsed_dt,
            close,
            symbol,
            py_obj: PyObject::from(item),
        });
    }

    Ok(bars_data)
}

#[pyclass]
pub struct BacktestEngine {
    pub(crate) cfg: BacktestConfig,
}

#[pymethods]
impl BacktestEngine {
    #[new]
    fn new(cfg: BacktestConfig) -> Self {
        Self { cfg }
    }

    /// 高性能回测循环：预提取数据、ctx 对象复用、bar 对象直通
    fn run<'py>(
        &self,
        py: Python<'py>,
        strategy: PyObject,
        data: &'py PyAny,
    ) -> PyResult<PyObject> {
        let bars: &PyList = data.downcast()?;

        // 预提取所有bar数据到Rust结构中
        let date_range = DateRange::from_config(&self.cfg)?;
        let raw_bars_data = extract_bars_data(bars)?;
        let raw_bar_count = raw_bars_data.len();
        let bars_data = filter_bars_by_date_range(raw_bars_data, &date_range)?;
        let n_bars = bars_data.len();
        if date_range.is_active() && raw_bar_count > 0 && n_bars == 0 {
            return Err(PyValueError::new_err(format!(
                "BacktestConfig start/end produced no bars: start='{}', end='{}'",
                self.cfg.start, self.cfg.end
            )));
        }

        // 整个 run 复用同一个 ctx 实例，每 bar 就地更新字段
        let ctx = Py::new(
            py,
            EngineContext {
                position: 0.0,
                avg_cost: 0.0,
                cash: self.cfg.cash,
                equity: self.cfg.cash,
                bar_index: 0,
                positions: PyDict::new_bound(py).unbind().into_any(),
                last_prices: PyDict::new_bound(py).unbind().into_any(),
            },
        )?;
        strategy.call_method1(py, "on_start", (ctx.bind(py),))?;
        let next_accepts_ctx = self.method_accepts_ctx(py, &strategy, "next")?;

        let mut pos = PositionState::new(self.cfg.cash);
        let mut order_seq: u64 = 1;

        // 预分配容量
        let mut equity_curve: Vec<(Option<String>, f64)> = Vec::with_capacity(n_bars);
        let mut trades: Vec<TradeRecord> = Vec::with_capacity(n_bars / 100);
        let mut orders: Vec<OrderRecord> = Vec::with_capacity(n_bars / 100);

        for i in 0..n_bars {
            let bar_data = &bars_data[i];
            let last_price = bar_data.close;
            let default_symbol = bar_data.symbol.as_deref().unwrap_or("DEFAULT");

            // 就地更新 ctx 快照（含 positions / last_prices 的单 symbol 视图）
            {
                let mut c = ctx.bind(py).borrow_mut();
                c.position = pos.position;
                c.avg_cost = pos.avg_cost;
                c.cash = pos.cash;
                c.equity = pos.cash + pos.position * last_price;
                c.bar_index = i;
                let pd = c.positions.bind(py).downcast::<PyDict>()?;
                pd.clear();
                let entry = PyDict::new_bound(py);
                entry.set_item("position", pos.position)?;
                entry.set_item("avg_cost", pos.avg_cost)?;
                pd.set_item(default_symbol, entry)?;
                let lp = c.last_prices.bind(py).downcast::<PyDict>()?;
                lp.clear();
                lp.set_item(default_symbol, last_price)?;
            }

            // 原始 bar 对象直通，不再每 bar 重建 PyDict
            let bar_obj = bar_data.py_obj.bind(py);
            let action_obj = if next_accepts_ctx {
                strategy.call_method1(py, "next", (bar_obj, ctx.bind(py)))?
            } else {
                strategy.call_method1(py, "next", (bar_obj,))?
            };

            // 快速订单处理
            if let Some(order) =
                self.parse_action_fast(action_obj.as_ref(py), &mut order_seq, default_symbol)?
            {
                // 订单提交回调
                let evt = PyDict::new_bound(py);
                evt.set_item("event", "submitted")?;
                evt.set_item("order_id", order.id)?;
                evt.set_item("side", Self::side_str(order.side))?;
                evt.set_item("type", Self::order_type_str(order.otype))?;
                evt.set_item("size", order.size)?;
                evt.set_item("symbol", &order.symbol)?;
                if let Some(lp) = order.limit_price {
                    evt.set_item("limit_price", lp)?;
                }
                strategy.call_method1(py, "on_order", (evt.as_any(),))?;

                if let Some((fill_price, fill_size)) = self.try_match(&order, last_price) {
                    let slip = self.cfg.slippage_bps / 10_000.0;
                    let sign = match order.side {
                        OrderSide::Buy => 1.0,
                        OrderSide::Sell => -1.0,
                    };
                    let exec_price = fill_price * (1.0 + sign * slip);
                    let commission = exec_price * fill_size * self.cfg.commission_rate;

                    if let Some(reason) = self.validate_single_fill(
                        &pos, &order, exec_price, fill_size, commission, last_price,
                    ) {
                        orders.push(Self::order_record(
                            &order,
                            bar_data.datetime.clone(),
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
                        let equity = pos.cash + pos.position * last_price;
                        equity_curve.push((bar_data.datetime.clone(), equity));
                        continue;
                    }

                    // 快速持仓更新
                    let realized_delta = accounting::update_position(
                        &mut pos, &order, exec_price, fill_size, commission,
                    );
                    let side = Self::side_str(order.side).to_string();
                    trades.push(TradeRecord {
                        order_id: order.id,
                        symbol: order.symbol.clone(),
                        side: side.clone(),
                        price: exec_price,
                        size: fill_size,
                        datetime: bar_data.datetime.clone(),
                        commission,
                        realized_pnl: realized_delta,
                    });
                    orders.push(Self::order_record(
                        &order,
                        bar_data.datetime.clone(),
                        "filled",
                        Some(exec_price),
                        fill_size,
                        commission,
                        realized_delta,
                        None,
                    ));

                    // 成交回调
                    let trade_evt = PyDict::new_bound(py);
                    trade_evt.set_item("order_id", order.id)?;
                    trade_evt.set_item("side", side)?;
                    trade_evt.set_item("price", exec_price)?;
                    trade_evt.set_item("size", fill_size)?;
                    trade_evt.set_item("symbol", &order.symbol)?;
                    if let Some(dt) = &bar_data.datetime {
                        trade_evt.set_item("datetime", dt)?;
                    }
                    trade_evt.set_item("commission", commission)?;
                    trade_evt.set_item("realized_pnl", realized_delta)?;
                    strategy.call_method1(py, "on_trade", (trade_evt.as_any(),))?;

                    // 订单完成回调
                    let evt2 = PyDict::new_bound(py);
                    evt2.set_item("event", "filled")?;
                    evt2.set_item("order_id", order.id)?;
                    strategy.call_method1(py, "on_order", (evt2.as_any(),))?;
                } else {
                    orders.push(Self::order_record(
                        &order,
                        bar_data.datetime.clone(),
                        "unfilled",
                        None,
                        0.0,
                        0.0,
                        0.0,
                        None,
                    ));
                }
            }

            let equity = pos.cash + pos.position * last_price;
            equity_curve.push((bar_data.datetime.clone(), equity));
        }

        strategy.call_method0(py, "on_stop")?;

        // 构建结果（单/多资产共用）
        build_result(py, pos, equity_curve, trades, orders)
    }

    /// 多资产/多周期（按联合时间线）回测（Python 暴露方法）
    fn run_multi<'py>(
        &self,
        py: Python<'py>,
        strategy: PyObject,
        feeds: &'py PyAny,
    ) -> PyResult<PyObject> {
        self._run_multi_impl(py, strategy, feeds)
    }
}

impl BacktestEngine {
    pub(crate) fn side_str(side: OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }

    pub(crate) fn order_type_str(otype: OrderType) -> &'static str {
        match otype {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
        }
    }

    pub(crate) fn order_record(
        order: &Order,
        datetime: Option<String>,
        status: &str,
        filled_price: Option<f64>,
        filled_size: f64,
        commission: f64,
        realized_pnl: f64,
        reject_reason: Option<String>,
    ) -> OrderRecord {
        OrderRecord {
            order_id: order.id,
            symbol: order.symbol.clone(),
            side: Self::side_str(order.side).to_string(),
            otype: Self::order_type_str(order.otype).to_string(),
            size: order.size,
            limit_price: order.limit_price,
            status: status.to_string(),
            datetime,
            filled_price,
            filled_size,
            commission,
            realized_pnl,
            reject_reason,
        }
    }

    fn validate_action_side(action: &str) -> PyResult<OrderSide> {
        match action.trim().to_ascii_uppercase().as_str() {
            "BUY" => Ok(OrderSide::Buy),
            "SELL" => Ok(OrderSide::Sell),
            other => Err(PyValueError::new_err(format!(
                "invalid action '{other}'; expected BUY or SELL"
            ))),
        }
    }

    fn validate_order_type(otype: &str) -> PyResult<OrderType> {
        match otype.trim().to_ascii_lowercase().as_str() {
            "market" => Ok(OrderType::Market),
            "limit" => Ok(OrderType::Limit),
            other => Err(PyValueError::new_err(format!(
                "invalid order type '{other}'; expected market or limit"
            ))),
        }
    }

    fn validate_size(size: f64) -> PyResult<f64> {
        if size.is_finite() && size > 0.0 {
            Ok(size)
        } else {
            Err(PyValueError::new_err(format!(
                "order size must be a positive finite number, got {size}"
            )))
        }
    }

    fn validate_price(price: f64, field_name: &str) -> PyResult<f64> {
        if price.is_finite() && price > 0.0 {
            Ok(price)
        } else {
            Err(PyValueError::new_err(format!(
                "{field_name} must be a positive finite number, got {price}"
            )))
        }
    }

    pub(crate) fn method_accepts_ctx<'py>(
        &self,
        py: Python<'py>,
        strategy: &PyObject,
        method_name: &str,
    ) -> PyResult<bool> {
        let method = strategy.getattr(py, method_name)?;
        let inspect = py.import_bound("inspect")?;
        let signature = inspect.call_method1("signature", (method,))?;
        let parameters = signature.getattr("parameters")?;
        let builtins = py.import_bound("builtins")?;
        let count: usize = builtins.getattr("len")?.call1((parameters,))?.extract()?;
        Ok(count >= 2)
    }

    // 优化的动作解析，减少类型检查（单资产路径）
    pub(crate) fn parse_action_fast(
        &self,
        action_obj: &PyAny,
        order_seq: &mut u64,
        default_symbol: &str,
    ) -> PyResult<Option<Order>> {
        if action_obj.is_none() {
            return Ok(None);
        }

        if let Ok(act) = action_obj.extract::<String>() {
            if act.trim().is_empty() {
                return Ok(None);
            }
            let id = *order_seq;
            *order_seq += 1;
            return Ok(Some(Order {
                id,
                side: Self::validate_action_side(&act)?,
                otype: OrderType::Market,
                size: 1.0,
                limit_price: None,
                symbol: default_symbol.to_string(),
            }));
        }

        if let Ok(d) = action_obj.downcast::<PyDict>() {
            let act = match d.get_item("action")? {
                Some(v) => v
                    .extract::<String>()
                    .map_err(|_| PyValueError::new_err("action must be a string: BUY or SELL"))?,
                None => return Ok(None),
            };
            if act.trim().is_empty() {
                return Ok(None);
            }

            let side = Self::validate_action_side(&act)?;
            let otype = match d.get_item("type")? {
                Some(v) => {
                    let otype_str = v.extract::<String>().map_err(|_| {
                        PyValueError::new_err("order type must be a string: market or limit")
                    })?;
                    Self::validate_order_type(&otype_str)?
                }
                None => OrderType::Market,
            };
            let size = match d.get_item("size")? {
                Some(v) => Self::validate_size(
                    v.extract::<f64>()
                        .map_err(|_| PyValueError::new_err("order size must be numeric"))?,
                )?,
                None => 1.0,
            };
            let price = match d.get_item("price")? {
                Some(v) => Some(Self::validate_price(
                    v.extract::<f64>()
                        .map_err(|_| PyValueError::new_err("order price must be numeric"))?,
                    "order price",
                )?),
                None => None,
            };
            let symbol = match d.get_item("symbol")? {
                Some(v) => {
                    let s = v.extract::<String>().map_err(|_| {
                        PyValueError::new_err("order symbol must be a non-empty string")
                    })?;
                    if s.trim().is_empty() {
                        return Err(PyValueError::new_err(
                            "order symbol must be a non-empty string",
                        ));
                    }
                    s
                }
                None => default_symbol.to_string(),
            };

            let id = *order_seq;
            *order_seq += 1;
            let limit_price = if otype == OrderType::Limit {
                Some(price.ok_or_else(|| {
                    PyValueError::new_err("limit order requires a positive price")
                })?)
            } else {
                None
            };
            return Ok(Some(Order {
                id,
                side,
                otype,
                size,
                limit_price,
                symbol,
            }));
        }

        Err(PyValueError::new_err(
            "strategy action must be None, BUY/SELL string, action dict, or list of action dicts",
        ))
    }

    #[inline]
    pub(crate) fn try_match(&self, order: &Order, last_price: f64) -> Option<(f64, f64)> {
        match order.otype {
            OrderType::Market => Some((last_price, order.size)),
            OrderType::Limit => {
                let lp = order.limit_price.unwrap_or(last_price);
                match order.side {
                    OrderSide::Buy => {
                        if last_price <= lp {
                            Some((lp, order.size))
                        } else {
                            None
                        }
                    }
                    OrderSide::Sell => {
                        if last_price >= lp {
                            Some((lp, order.size))
                        } else {
                            None
                        }
                    }
                }
            }
        }
    }

    fn validate_single_fill(
        &self,
        pos: &PositionState,
        order: &Order,
        exec_price: f64,
        fill_size: f64,
        commission: f64,
        last_price: f64,
    ) -> Option<String> {
        if !self.cfg.allow_short && order.side == OrderSide::Sell {
            let next_pos = pos.position - fill_size;
            if next_pos < -f64::EPSILON {
                return Some("short selling is disabled".to_string());
            }
        }

        if self.cfg.reject_on_insufficient_cash && order.side == OrderSide::Buy {
            let cost = exec_price * fill_size + commission;
            if cost > pos.cash + f64::EPSILON {
                return Some(format!(
                    "insufficient cash: required {cost}, available {}",
                    pos.cash
                ));
            }
        }

        if let Some(max_leverage) = self.cfg.max_leverage {
            if max_leverage.is_finite() && max_leverage >= 0.0 {
                let mut next_cash = pos.cash;
                let mut next_position = pos.position;
                match order.side {
                    OrderSide::Buy => {
                        next_cash -= exec_price * fill_size + commission;
                        next_position += fill_size;
                    }
                    OrderSide::Sell => {
                        next_cash += exec_price * fill_size - commission;
                        next_position -= fill_size;
                    }
                }
                let equity = next_cash + next_position * last_price;
                let exposure = (next_position * last_price).abs();
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
