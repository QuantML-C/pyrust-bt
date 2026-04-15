use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyAny, PyDict, PyList};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Database module for high-performance K-line operations
mod database;
pub use database::{get_market_data, resample_klines, save_klines, save_klines_from_csv};

// 预提取的bar数据结构
#[derive(Clone, Debug)]
struct BarData {
    datetime: Option<String>,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    symbol: Option<String>,
}

#[pyclass]
#[derive(Clone)]
pub struct BacktestConfig {
    #[pyo3(get)]
    pub start: String,
    #[pyo3(get)]
    pub end: String,
    #[pyo3(get)]
    pub cash: f64,
    #[pyo3(get)]
    pub commission_rate: f64,
    #[pyo3(get)]
    pub slippage_bps: f64,
    #[pyo3(get)]
    pub batch_size: usize, // 新增：批处理大小
    #[pyo3(get)]
    pub allow_short: bool,
    #[pyo3(get)]
    pub reject_on_insufficient_cash: bool,
    #[pyo3(get)]
    pub max_leverage: Option<f64>,
}

#[pymethods]
impl BacktestConfig {
    #[new]
    #[pyo3(signature = (start, end, cash, commission_rate=0.0, slippage_bps=0.0, batch_size=1000, allow_short=true, reject_on_insufficient_cash=false, max_leverage=None))]
    fn new(
        start: String,
        end: String,
        cash: f64,
        commission_rate: f64,
        slippage_bps: f64,
        batch_size: usize,
        allow_short: bool,
        reject_on_insufficient_cash: bool,
        max_leverage: Option<f64>,
    ) -> Self {
        Self {
            start,
            end,
            cash,
            commission_rate,
            slippage_bps,
            batch_size,
            allow_short,
            reject_on_insufficient_cash,
            max_leverage,
        }
    }
}

#[derive(Clone, Debug)]
struct DateRange {
    start: Option<NaiveDateTime>,
    end: Option<NaiveDateTime>,
}

impl DateRange {
    fn from_config(cfg: &BacktestConfig) -> PyResult<Self> {
        Ok(Self {
            start: parse_config_datetime(&cfg.start, false, "start")?,
            end: parse_config_datetime(&cfg.end, true, "end")?,
        })
    }

    fn is_active(&self) -> bool {
        self.start.is_some() || self.end.is_some()
    }

    fn contains_bar(&self, bar: &BarData) -> PyResult<bool> {
        if !self.is_active() {
            return Ok(true);
        }

        let dt_text = bar.datetime.as_deref().ok_or_else(|| {
            PyValueError::new_err("BacktestConfig start/end requires bar datetime values")
        })?;
        let dt = parse_datetime_value(dt_text, false).ok_or_else(|| {
            PyValueError::new_err(format!("invalid bar datetime value: {dt_text}"))
        })?;

        if let Some(start) = self.start {
            if dt < start {
                return Ok(false);
            }
        }
        if let Some(end) = self.end {
            if dt > end {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn parse_config_datetime(
    value: &str,
    date_only_is_end: bool,
    field_name: &str,
) -> PyResult<Option<NaiveDateTime>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    parse_datetime_value(trimmed, date_only_is_end)
        .map(Some)
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "invalid BacktestConfig {field_name} datetime value: {trimmed}"
            ))
        })
}

fn parse_datetime_value(value: &str, date_only_is_end: bool) -> Option<NaiveDateTime> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.naive_utc());
    }

    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(dt);
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let time = if date_only_is_end {
            NaiveTime::from_hms_nano_opt(23, 59, 59, 999_999_999)?
        } else {
            NaiveTime::from_hms_opt(0, 0, 0)?
        };
        return Some(date.and_time(time));
    }

    None
}

fn filter_bars_by_date_range(
    bars_data: Vec<BarData>,
    date_range: &DateRange,
) -> PyResult<Vec<BarData>> {
    if !date_range.is_active() {
        return Ok(bars_data);
    }

    let mut filtered = Vec::with_capacity(bars_data.len());
    for bar in bars_data {
        if date_range.contains_bar(&bar)? {
            filtered.push(bar);
        }
    }
    Ok(filtered)
}

fn equity_curve_elapsed_years(equity_curve: &[(Option<String>, f64)]) -> Option<f64> {
    let start_text = equity_curve.first()?.0.as_deref()?;
    let end_text = equity_curve.last()?.0.as_deref()?;
    let start = parse_datetime_value(start_text, false)?;
    let end = parse_datetime_value(end_text, false)?;
    let seconds = (end - start).num_seconds();
    if seconds <= 0 {
        return None;
    }
    Some(seconds as f64 / (365.25 * 24.0 * 60.0 * 60.0))
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum OrderSide {
    Buy,
    Sell,
}

#[derive(Copy, Clone, Debug, PartialEq)]
enum OrderType {
    Market,
    Limit,
}

#[derive(Clone, Debug)]
struct Order {
    id: u64,
    side: OrderSide,
    otype: OrderType,
    size: f64,
    limit_price: Option<f64>,
    symbol: String,
}

#[derive(Clone, Debug)]
struct TradeRecord {
    order_id: u64,
    symbol: String,
    side: String,
    price: f64,
    size: f64,
    datetime: Option<String>,
    commission: f64,
    realized_pnl: f64,
}

#[derive(Clone, Debug)]
struct OrderRecord {
    order_id: u64,
    symbol: String,
    side: String,
    otype: String,
    size: f64,
    limit_price: Option<f64>,
    status: String,
    datetime: Option<String>,
    filled_price: Option<f64>,
    filled_size: f64,
    commission: f64,
    realized_pnl: f64,
    reject_reason: Option<String>,
}

#[derive(Default, Clone, Debug)]
struct PositionState {
    position: f64,
    avg_cost: f64,
    cash: f64,
    realized_pnl: f64,
}

impl PositionState {
    fn new(cash: f64) -> Self {
        Self {
            position: 0.0,
            avg_cost: 0.0,
            cash,
            realized_pnl: 0.0,
        }
    }
}

// 向量化指标计算（优化版）
pub fn vectorized_sma(prices: &[f64], window: usize) -> Vec<Option<f64>> {
    if prices.is_empty() || window == 0 {
        return vec![None; prices.len()];
    }

    let mut result = Vec::with_capacity(prices.len());
    let mut sum = 0.0;

    for i in 0..prices.len() {
        sum += prices[i];
        if i >= window {
            sum -= prices[i - window];
        }

        if i + 1 >= window {
            result.push(Some(sum / window as f64));
        } else {
            result.push(None);
        }
    }
    result
}

pub fn vectorized_rsi(prices: &[f64], window: usize) -> Vec<Option<f64>> {
    if prices.len() < 2 || window == 0 {
        return vec![None; prices.len()];
    }

    let mut result = Vec::with_capacity(prices.len());
    result.push(None); // 第一个价格没有变化

    let mut gains = Vec::with_capacity(prices.len());
    let mut losses = Vec::with_capacity(prices.len());

    // 计算价格变化
    for i in 1..prices.len() {
        let change = prices[i] - prices[i - 1];
        if change > 0.0 {
            gains.push(change);
            losses.push(0.0);
        } else {
            gains.push(0.0);
            losses.push(-change);
        }
    }

    // 计算RSI
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;

    for i in 0..gains.len() {
        if i < window - 1 {
            result.push(None);
        } else if i == window - 1 {
            // 初始平均
            avg_gain = gains[0..window].iter().sum::<f64>() / window as f64;
            avg_loss = losses[0..window].iter().sum::<f64>() / window as f64;

            let rsi = if avg_loss == 0.0 {
                100.0
            } else {
                100.0 - (100.0 / (1.0 + avg_gain / avg_loss))
            };
            result.push(Some(rsi));
        } else {
            // Wilder的平滑方法
            avg_gain = ((avg_gain * (window - 1) as f64) + gains[i]) / window as f64;
            avg_loss = ((avg_loss * (window - 1) as f64) + losses[i]) / window as f64;

            let rsi = if avg_loss == 0.0 {
                100.0
            } else {
                100.0 - (100.0 / (1.0 + avg_gain / avg_loss))
            };
            result.push(Some(rsi));
        }
    }

    result
}

#[pyfunction]
fn compute_sma(prices: Vec<f64>, window: usize) -> Vec<Option<f64>> {
    vectorized_sma(&prices, window)
}

#[pyfunction]
fn compute_rsi(prices: Vec<f64>, window: usize) -> Vec<Option<f64>> {
    vectorized_rsi(&prices, window)
}

// 批量提取bar数据，减少Python调用
fn extract_bars_data(bars: &PyList) -> PyResult<Vec<BarData>> {
    let mut bars_data = Vec::with_capacity(bars.len());

    for item in bars.iter() {
        let bar: &PyDict = item.downcast()?;

        let datetime = match bar.get_item("datetime")? {
            Some(v) => v.extract::<String>().ok(),
            None => None,
        };

        let open = bar
            .get_item("open")?
            .and_then(|v| v.extract::<f64>().ok())
            .unwrap_or(0.0);
        let high = bar
            .get_item("high")?
            .and_then(|v| v.extract::<f64>().ok())
            .unwrap_or(0.0);
        let low = bar
            .get_item("low")?
            .and_then(|v| v.extract::<f64>().ok())
            .unwrap_or(0.0);
        let close = bar
            .get_item("close")?
            .and_then(|v| v.extract::<f64>().ok())
            .unwrap_or(0.0);
        let volume = bar
            .get_item("volume")?
            .and_then(|v| v.extract::<f64>().ok())
            .unwrap_or(0.0);
        let symbol = bar
            .get_item("symbol")?
            .and_then(|v| v.extract::<String>().ok());

        bars_data.push(BarData {
            datetime,
            open,
            high,
            low,
            close,
            volume,
            symbol,
        });
    }

    Ok(bars_data)
}

#[pyclass]
#[derive(Clone)]
pub struct EngineContext {
    #[pyo3(get)]
    pub position: f64,
    #[pyo3(get)]
    pub avg_cost: f64,
    #[pyo3(get)]
    pub cash: f64,
    #[pyo3(get)]
    pub equity: f64,
    #[pyo3(get)]
    pub bar_index: usize,
}

#[pyclass]
pub struct BacktestEngine {
    cfg: BacktestConfig,
}

#[pymethods]
impl BacktestEngine {
    #[new]
    fn new(cfg: BacktestConfig) -> Self {
        Self { cfg }
    }

    /// 高性能回测循环：预提取数据、批量处理、减少Python调用
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

        // 初始上下文（无价格时以现金估算净值）
        let init_ctx = Py::new(
            py,
            EngineContext {
                position: 0.0,
                avg_cost: 0.0,
                cash: self.cfg.cash,
                equity: self.cfg.cash,
                bar_index: 0,
            },
        )?;
        let _ = strategy.call_method1(py, "on_start", (init_ctx.as_ref(py),));
        let next_accepts_ctx = self.method_accepts_ctx(py, &strategy, "next")?;

        let mut pos = PositionState::new(self.cfg.cash);
        let mut order_seq: u64 = 1;

        // 预分配容量
        let mut equity_curve: Vec<(Option<String>, f64)> = Vec::with_capacity(n_bars);
        let mut trades: Vec<TradeRecord> = Vec::with_capacity(n_bars / 100);
        let mut orders: Vec<OrderRecord> = Vec::with_capacity(n_bars / 100);

        // 批量处理策略调用，减少Python GIL争用
        let batch_size = self.cfg.batch_size.max(1).min(n_bars.max(1));

        for chunk_start in (0..n_bars).step_by(batch_size) {
            let chunk_end = (chunk_start + batch_size).min(n_bars);

            // 处理当前批次
            for i in chunk_start..chunk_end {
                let bar_data = &bars_data[i];
                let last_price = bar_data.close;

                // 重新构造PyDict给策略（只在需要时）
                let bar_dict = PyDict::new_bound(py);
                if let Some(ref dt) = bar_data.datetime {
                    bar_dict.set_item("datetime", dt)?;
                }
                bar_dict.set_item("open", bar_data.open)?;
                bar_dict.set_item("high", bar_data.high)?;
                bar_dict.set_item("low", bar_data.low)?;
                bar_dict.set_item("close", bar_data.close)?;
                bar_dict.set_item("volume", bar_data.volume)?;

                // 上下文快照传入策略
                let equity_snapshot = pos.cash + pos.position * last_price;
                let ctx = Py::new(
                    py,
                    EngineContext {
                        position: pos.position,
                        avg_cost: pos.avg_cost,
                        cash: pos.cash,
                        equity: equity_snapshot,
                        bar_index: i,
                    },
                )?;
                let action_obj = if next_accepts_ctx {
                    strategy.call_method1(py, "next", (bar_dict.as_any(), ctx.as_ref(py)))?
                } else {
                    strategy.call_method1(py, "next", (bar_dict.as_any(),))?
                };

                // 快速订单处理
                let default_symbol = bar_data.symbol.as_deref().unwrap_or("DEFAULT");
                if let Some(order) = self.parse_action_fast(
                    action_obj.as_ref(py),
                    &mut order_seq,
                    last_price,
                    default_symbol,
                )? {
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
                    let _ = strategy.call_method1(py, "on_order", (evt.as_any(),));

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
                            let _ = strategy.call_method1(py, "on_order", (rejected_evt.as_any(),));
                            let equity = pos.cash + pos.position * last_price;
                            equity_curve.push((bar_data.datetime.clone(), equity));
                            continue;
                        }

                        // 快速持仓更新
                        let realized_delta = self
                            .update_position(&mut pos, &order, exec_price, fill_size, commission);
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
                        let _ = strategy.call_method1(py, "on_trade", (trade_evt.as_any(),));

                        // 订单完成回调
                        let evt2 = PyDict::new_bound(py);
                        evt2.set_item("event", "filled")?;
                        evt2.set_item("order_id", order.id)?;
                        let _ = strategy.call_method1(py, "on_order", (evt2.as_any(),));
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
        }

        let _ = strategy.call_method0(py, "on_stop");

        // 构建结果（优化版）
        self.build_result(py, pos, equity_curve, trades, orders)
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
    fn side_str(side: OrderSide) -> &'static str {
        match side {
            OrderSide::Buy => "BUY",
            OrderSide::Sell => "SELL",
        }
    }

    fn order_type_str(otype: OrderType) -> &'static str {
        match otype {
            OrderType::Market => "market",
            OrderType::Limit => "limit",
        }
    }

    fn order_record(
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

    fn method_accepts_ctx<'py>(
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
    fn parse_action_fast<'py>(
        &self,
        action_obj: &PyAny,
        order_seq: &mut u64,
        _last_price: f64,
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

    // 解析多指令：支持 list/tuple；若为单个则返回单元素
    fn parse_actions_any<'py>(
        &self,
        py: Python<'py>,
        action_obj: &PyAny,
        order_seq: &mut u64,
        last_price_map: &HashMap<String, f64>,
        default_symbol: &str,
    ) -> PyResult<Vec<Order>> {
        if let Ok(seq) = action_obj.downcast::<pyo3::types::PyList>() {
            let mut out = Vec::with_capacity(seq.len());
            for item in seq.iter() {
                // Try to read symbol first to get better last_price
                let mut sym = default_symbol.to_string();
                if let Ok(d) = item.downcast::<PyDict>() {
                    if let Ok(Some(val)) = d.get_item("symbol") {
                        if let Ok(s) = val.extract::<String>() {
                            sym = s;
                        }
                    }
                }
                let lp = *last_price_map.get(&sym).unwrap_or(&0.0);
                if let Some(o) = self.parse_action_fast(item, order_seq, lp, &sym)? {
                    out.push(o);
                }
            }
            return Ok(out);
        }
        // Single
        let lp = *last_price_map.get(default_symbol).unwrap_or(&0.0);
        if let Some(o) = self.parse_action_fast(action_obj, order_seq, lp, default_symbol)? {
            return Ok(vec![o]);
        }
        Ok(Vec::new())
    }

    #[inline]
    fn try_match(&self, order: &Order, last_price: f64) -> Option<(f64, f64)> {
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

    #[inline]
    fn update_position(
        &self,
        pos: &mut PositionState,
        order: &Order,
        exec_price: f64,
        fill_size: f64,
        commission: f64,
    ) -> f64 {
        match order.side {
            OrderSide::Buy => {
                let cost = exec_price * fill_size + commission;
                pos.cash -= cost;
            }
            OrderSide::Sell => {
                let proceeds = exec_price * fill_size - commission;
                pos.cash += proceeds;
            }
        }
        let realized = Self::apply_position_fill(
            &mut pos.position,
            &mut pos.avg_cost,
            order.side,
            exec_price,
            fill_size,
        );
        pos.realized_pnl += realized;
        realized
    }

    fn apply_position_fill(
        position: &mut f64,
        avg_cost: &mut f64,
        side: OrderSide,
        exec_price: f64,
        fill_size: f64,
    ) -> f64 {
        if fill_size <= 0.0 {
            return 0.0;
        }

        let signed_fill = match side {
            OrderSide::Buy => fill_size,
            OrderSide::Sell => -fill_size,
        };
        let old_pos = *position;

        if old_pos.abs() < f64::EPSILON {
            *position = signed_fill;
            *avg_cost = if signed_fill.abs() < f64::EPSILON {
                0.0
            } else {
                exec_price
            };
            return 0.0;
        }

        if old_pos.signum() == signed_fill.signum() {
            let old_abs = old_pos.abs();
            let new_abs = old_abs + fill_size;
            *avg_cost = ((*avg_cost * old_abs) + (exec_price * fill_size)) / new_abs;
            *position = old_pos + signed_fill;
            return 0.0;
        }

        let closing = fill_size.min(old_pos.abs());
        let realized = if old_pos > 0.0 {
            (exec_price - *avg_cost) * closing
        } else {
            (*avg_cost - exec_price) * closing
        };

        let new_pos = old_pos + signed_fill;
        if new_pos.abs() < f64::EPSILON {
            *position = 0.0;
            *avg_cost = 0.0;
        } else if new_pos.signum() == old_pos.signum() {
            *position = new_pos;
        } else {
            *position = new_pos;
            *avg_cost = exec_price;
        }

        realized
    }

    fn build_result<'py>(
        &self,
        py: Python<'py>,
        pos: PositionState,
        equity_curve: Vec<(Option<String>, f64)>,
        trades: Vec<TradeRecord>,
        orders: Vec<OrderRecord>,
    ) -> PyResult<PyObject> {
        let result = PyDict::new_bound(py);
        result.set_item("cash", pos.cash)?;
        result.set_item("position", pos.position)?;
        result.set_item("avg_cost", pos.avg_cost)?;
        let final_equity = equity_curve.last().map_or(pos.cash, |(_, eq)| *eq);
        result.set_item("equity", final_equity)?;
        result.set_item("realized_pnl", pos.realized_pnl)?;

        // 高效构建净值曲线
        let eq_list = PyList::empty_bound(py);
        for (dt, eq) in &equity_curve {
            let row = PyDict::new_bound(py);
            if let Some(d) = dt {
                row.set_item("datetime", d)?;
            } else {
                row.set_item("datetime", py.None())?;
            }
            row.set_item("equity", eq)?;
            eq_list.append(row)?;
        }
        result.set_item("equity_curve", eq_list)?;

        // 高效构建交易列表
        let tr_list = PyList::empty_bound(py);
        for trade in &trades {
            let t = PyDict::new_bound(py);
            t.set_item("order_id", trade.order_id)?;
            t.set_item("side", &trade.side)?;
            t.set_item("price", trade.price)?;
            t.set_item("size", trade.size)?;
            t.set_item("symbol", &trade.symbol)?;
            if let Some(dt) = &trade.datetime {
                t.set_item("datetime", dt)?;
            } else {
                t.set_item("datetime", py.None())?;
            }
            t.set_item("commission", trade.commission)?;
            t.set_item("realized_pnl", trade.realized_pnl)?;
            tr_list.append(t)?;
        }
        result.set_item("trades", tr_list)?;

        let order_list = PyList::empty_bound(py);
        for order in &orders {
            let row = PyDict::new_bound(py);
            row.set_item("order_id", order.order_id)?;
            row.set_item("symbol", &order.symbol)?;
            row.set_item("side", &order.side)?;
            row.set_item("type", &order.otype)?;
            row.set_item("size", order.size)?;
            if let Some(limit_price) = order.limit_price {
                row.set_item("limit_price", limit_price)?;
            } else {
                row.set_item("limit_price", py.None())?;
            }
            row.set_item("status", &order.status)?;
            if let Some(dt) = &order.datetime {
                row.set_item("datetime", dt)?;
            } else {
                row.set_item("datetime", py.None())?;
            }
            if let Some(filled_price) = order.filled_price {
                row.set_item("filled_price", filled_price)?;
            } else {
                row.set_item("filled_price", py.None())?;
            }
            row.set_item("filled_size", order.filled_size)?;
            row.set_item("commission", order.commission)?;
            row.set_item("realized_pnl", order.realized_pnl)?;
            if let Some(reason) = &order.reject_reason {
                row.set_item("reject_reason", reason)?;
            } else {
                row.set_item("reject_reason", py.None())?;
            }
            order_list.append(row)?;
        }
        result.set_item("orders", order_list)?;

        // 增强的统计分析
        let stats =
            self.compute_enhanced_stats(py, &equity_curve, &trades, &orders, pos.realized_pnl)?;
        result.set_item("stats", stats)?;

        Ok(result.into())
    }

    fn compute_enhanced_stats<'py>(
        &self,
        py: Python<'py>,
        equity_curve: &[(Option<String>, f64)],
        trades: &[TradeRecord],
        orders: &[OrderRecord],
        realized_pnl: f64,
    ) -> PyResult<PyObject> {
        if equity_curve.is_empty() {
            return Ok(PyDict::new_bound(py).into());
        }

        let start_equity = equity_curve.first().unwrap().1;
        let end_equity = equity_curve.last().unwrap().1;
        let total_return = if start_equity != 0.0 {
            (end_equity / start_equity) - 1.0
        } else {
            0.0
        };
        let total_pnl = end_equity - start_equity;
        let unrealized_pnl = total_pnl - realized_pnl;

        // 向量化收益率计算
        let mut returns: Vec<f64> = Vec::with_capacity(equity_curve.len().saturating_sub(1));
        for i in 1..equity_curve.len() {
            let prev = equity_curve[i - 1].1;
            let curr = equity_curve[i].1;
            if prev.abs() > f64::EPSILON {
                returns.push((curr - prev) / prev.abs());
            }
        }

        let mean_return = if returns.is_empty() {
            0.0
        } else {
            returns.iter().sum::<f64>() / returns.len() as f64
        };
        let var = if returns.len() > 1 {
            let sum_sq_diff: f64 = returns.iter().map(|r| (r - mean_return).powi(2)).sum();
            sum_sq_diff / (returns.len() - 1) as f64
        } else {
            0.0
        };
        let std = var.sqrt();
        let elapsed_years = equity_curve_elapsed_years(equity_curve);
        let periods_per_year = elapsed_years
            .filter(|years| *years > 0.0)
            .map(|years| returns.len() as f64 / years)
            .filter(|factor| factor.is_finite() && *factor > 0.0)
            .unwrap_or(252.0);
        let annualized_return = if let Some(years) = elapsed_years {
            if start_equity > 0.0 && end_equity > 0.0 {
                (end_equity / start_equity).powf(1.0 / years) - 1.0
            } else {
                total_return / years
            }
        } else if returns.is_empty() {
            total_return
        } else {
            mean_return * periods_per_year
        };
        let annualized_volatility = std * periods_per_year.sqrt();
        let sharpe = if annualized_volatility > 0.0 {
            annualized_return / annualized_volatility
        } else {
            0.0
        };

        // 高效最大回撤计算
        let mut peak = start_equity;
        let mut max_dd: f64 = 0.0;
        let mut dd_duration = 0;
        let mut max_dd_duration = 0;

        for &(_, eq) in equity_curve {
            if eq > peak {
                peak = eq;
                dd_duration = 0;
            } else {
                dd_duration += 1;
                let current_dd = 1.0 - eq / peak;
                if current_dd > max_dd {
                    max_dd = current_dd;
                }
                if dd_duration > max_dd_duration {
                    max_dd_duration = dd_duration;
                }
            }
        }

        // 交易统计
        let total_trades = trades.len();
        let commission_total: f64 = trades.iter().map(|trade| trade.commission).sum();
        let (closed_trades, winning_trades, losing_trades, gross_profit, gross_loss) = {
            let mut closed = 0;
            let mut win = 0;
            let mut lose = 0;
            let mut profit = 0.0;
            let mut loss = 0.0;

            for trade in trades {
                if trade.realized_pnl.abs() <= f64::EPSILON {
                    continue;
                }
                closed += 1;
                if trade.realized_pnl > 0.0 {
                    win += 1;
                    profit += trade.realized_pnl;
                } else {
                    lose += 1;
                    loss += -trade.realized_pnl;
                }
            }
            (closed, win, lose, profit, loss)
        };

        let win_rate = if closed_trades > 0 {
            winning_trades as f64 / closed_trades as f64
        } else {
            0.0
        };
        let avg_win = if winning_trades > 0 {
            gross_profit / winning_trades as f64
        } else {
            0.0
        };
        let avg_loss = if losing_trades > 0 {
            gross_loss / losing_trades as f64
        } else {
            0.0
        };
        let profit_factor = if gross_loss > 0.0 {
            gross_profit / gross_loss
        } else if gross_profit > 0.0 {
            f64::INFINITY
        } else {
            0.0
        };
        let filled_orders = orders
            .iter()
            .filter(|order| order.status == "filled")
            .count();
        let rejected_orders = orders
            .iter()
            .filter(|order| order.status == "rejected")
            .count();
        let unfilled_orders = orders
            .iter()
            .filter(|order| order.status == "unfilled")
            .count();
        let calmar = if max_dd > 0.0 {
            annualized_return / max_dd
        } else {
            0.0
        };

        let stats = PyDict::new_bound(py);
        stats.set_item("start_equity", start_equity)?;
        stats.set_item("end_equity", end_equity)?;
        stats.set_item("total_return", total_return)?;
        stats.set_item("annualized_return", annualized_return)?;
        stats.set_item("volatility", annualized_volatility)?;
        stats.set_item("sharpe", sharpe)?;
        stats.set_item("calmar", calmar)?;
        stats.set_item("max_drawdown", max_dd)?;
        stats.set_item("max_dd_duration", max_dd_duration)?;
        stats.set_item("total_trades", total_trades)?;
        stats.set_item("closed_trades", closed_trades)?;
        stats.set_item("winning_trades", winning_trades)?;
        stats.set_item("losing_trades", losing_trades)?;
        stats.set_item("win_rate", win_rate)?;
        stats.set_item("total_pnl", total_pnl)?;
        stats.set_item("realized_pnl", realized_pnl)?;
        stats.set_item("unrealized_pnl", unrealized_pnl)?;
        stats.set_item("commission_total", commission_total)?;
        stats.set_item("gross_profit", gross_profit)?;
        stats.set_item("gross_loss", gross_loss)?;
        stats.set_item("profit_factor", profit_factor)?;
        stats.set_item("avg_win", avg_win)?;
        stats.set_item("avg_loss", avg_loss)?;
        stats.set_item("total_orders", orders.len())?;
        stats.set_item("filled_orders", filled_orders)?;
        stats.set_item("rejected_orders", rejected_orders)?;
        stats.set_item("unfilled_orders", unfilled_orders)?;

        Ok(stats.into())
    }
}

impl BacktestEngine {
    /// 多资产/多周期（按联合时间线）回测。feeds: Dict[str, List[bar]]，bar 至少包含 datetime/close，可选 symbol。
    fn _run_multi_impl<'py>(
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
            let raw_bars = extract_bars_data(blist)?;
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

        // on_start 传入汇总 ctx（Python dict）
        let start_ctx = PyDict::new_bound(py);
        start_ctx.set_item("cash", cash)?;
        start_ctx.set_item("equity", cash)?;
        start_ctx.set_item("positions", PyDict::new_bound(py))?;
        start_ctx.set_item("bar_index", 0usize)?;
        let _ = strategy.call_method1(py, "on_start", (start_ctx.as_any(),));

        let mut step: usize = 0;
        loop {
            // 找到下一个最小的 datetime
            let mut min_dt: Option<String> = None;
            for f in 0..n_feeds {
                if idxs[f] < feed_bars[f].len() {
                    if let Some(dt) = &feed_bars[f][idxs[f]].datetime {
                        match &min_dt {
                            None => min_dt = Some(dt.clone()),
                            Some(cur) => {
                                if dt < cur {
                                    min_dt = Some(dt.clone());
                                }
                            }
                        }
                    }
                }
            }
            if min_dt.is_none() {
                break;
            }
            let cur_dt = min_dt.unwrap();

            // 本步更新的 bars 切片
            let update_slice = PyDict::new_bound(py);
            for f in 0..n_feeds {
                if idxs[f] < feed_bars[f].len() {
                    if feed_bars[f][idxs[f]].datetime.as_ref() == Some(&cur_dt) {
                        let b = &feed_bars[f][idxs[f]];
                        // 更新 last
                        last_snapshot[f] = Some(b.clone());
                        if let Some(sym) = &b.symbol {
                            last_price_map.insert(sym.clone(), b.close);
                        }
                        // 构造 bar dict
                        let bd = PyDict::new_bound(py);
                        if let Some(dt) = &b.datetime {
                            bd.set_item("datetime", dt)?;
                        }
                        if let Some(sym) = &b.symbol {
                            bd.set_item("symbol", sym)?;
                        }
                        bd.set_item("open", b.open)?;
                        bd.set_item("high", b.high)?;
                        bd.set_item("low", b.low)?;
                        bd.set_item("close", b.close)?;
                        bd.set_item("volume", b.volume)?;
                        update_slice.set_item(&feed_ids[f], bd)?;
                        idxs[f] += 1;
                    }
                }
            }

            // 构造 ctx：汇总 + 头寸 + last_prices
            let ctx = PyDict::new_bound(py);
            let pos_dict = PyDict::new_bound(py);
            for (sym, (p, ac)) in positions.iter() {
                let pd = PyDict::new_bound(py);
                pd.set_item("position", *p)?;
                pd.set_item("avg_cost", *ac)?;
                pos_dict.set_item(sym, pd)?;
            }
            // 汇总净值
            let mut equity: f64 = cash;
            for (sym, (p, _)) in positions.iter() {
                if let Some(lp) = last_price_map.get(sym) {
                    equity += p * lp;
                }
            }
            ctx.set_item("positions", pos_dict)?;
            ctx.set_item("cash", cash)?;
            ctx.set_item("equity", equity)?;
            ctx.set_item("bar_index", step)?;
            ctx.set_item("last_prices", {
                let lp = PyDict::new_bound(py);
                for (k, v) in last_price_map.iter() {
                    lp.set_item(k, v)?;
                }
                lp
            })?;

            // 调用策略：仅当 next_multi 不存在时回退到 next，策略内部异常直接传播
            let action_obj = if has_next_multi {
                strategy.call_method1(py, "next_multi", (update_slice.as_any(), ctx.as_any()))?
            } else {
                let primary_bar = if let Some(Some(b)) = last_snapshot.get(0) {
                    let bd = PyDict::new_bound(py);
                    if let Some(dt) = &b.datetime {
                        bd.set_item("datetime", dt)?;
                    }
                    if let Some(sym) = &b.symbol {
                        bd.set_item("symbol", sym)?;
                    }
                    bd.set_item("open", b.open)?;
                    bd.set_item("high", b.high)?;
                    bd.set_item("low", b.low)?;
                    bd.set_item("close", b.close)?;
                    bd.set_item("volume", b.volume)?;
                    Some(bd)
                } else {
                    None
                };
                if let Some(pb) = primary_bar {
                    if next_accepts_ctx {
                        strategy.call_method1(py, "next", (pb.as_any(), ctx.as_any()))?
                    } else {
                        strategy.call_method1(py, "next", (pb.as_any(),))?
                    }
                } else {
                    py.None()
                }
            };

            // 解析并执行指令（支持 list）
            let default_symbol = if let Some(Some(b)) = last_snapshot.get(0) {
                b.symbol.clone().unwrap_or_else(|| "DEFAULT".to_string())
            } else {
                "DEFAULT".to_string()
            };
            let orders = self.parse_actions_any(
                py,
                action_obj.as_ref(py),
                &mut order_seq,
                &last_price_map,
                &default_symbol,
            )?;
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
                let _ = strategy.call_method1(py, "on_order", (submitted_evt.as_any(),));

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
                    let _ = strategy.call_method1(py, "on_order", (rejected_evt.as_any(),));
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
                        let _ = strategy.call_method1(py, "on_order", (rejected_evt.as_any(),));
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
                    let realized_delta = Self::apply_position_fill(
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
                    let _ = strategy.call_method1(py, "on_trade", (trade_evt.as_any(),));

                    let filled_evt = PyDict::new_bound(py);
                    filled_evt.set_item("event", "filled")?;
                    filled_evt.set_item("order_id", order.id)?;
                    let _ = strategy.call_method1(py, "on_order", (filled_evt.as_any(),));
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

        let _ = strategy.call_method0(py, "on_stop");

        // 构建结果
        let result = PyDict::new_bound(py);
        // 汇总头寸（简化：不返回逐 symbol 持仓，用户可在 on_trade / ctx 中获取）
        result.set_item("cash", cash)?;
        result.set_item("position", 0.0_f64)?;
        result.set_item("avg_cost", 0.0_f64)?;
        let last_eq = equity_curve.last().map(|(_, e)| *e).unwrap_or(cash);
        result.set_item("equity", last_eq)?;
        result.set_item("realized_pnl", realized_pnl)?;

        let eq_list = PyList::empty_bound(py);
        for (dt, eq) in &equity_curve {
            let row = PyDict::new_bound(py);
            if let Some(d) = dt {
                row.set_item("datetime", d)?;
            } else {
                row.set_item("datetime", py.None())?;
            }
            row.set_item("equity", eq)?;
            eq_list.append(row)?;
        }
        result.set_item("equity_curve", eq_list)?;

        let tr_list = PyList::empty_bound(py);
        for trade in &trades {
            let t = PyDict::new_bound(py);
            t.set_item("order_id", trade.order_id)?;
            t.set_item("side", &trade.side)?;
            t.set_item("price", trade.price)?;
            t.set_item("size", trade.size)?;
            t.set_item("symbol", &trade.symbol)?;
            if let Some(dt) = &trade.datetime {
                t.set_item("datetime", dt)?;
            } else {
                t.set_item("datetime", py.None())?;
            }
            t.set_item("commission", trade.commission)?;
            t.set_item("realized_pnl", trade.realized_pnl)?;
            tr_list.append(t)?;
        }
        result.set_item("trades", tr_list)?;

        let order_list = PyList::empty_bound(py);
        for order in &orders_records {
            let row = PyDict::new_bound(py);
            row.set_item("order_id", order.order_id)?;
            row.set_item("symbol", &order.symbol)?;
            row.set_item("side", &order.side)?;
            row.set_item("type", &order.otype)?;
            row.set_item("size", order.size)?;
            if let Some(limit_price) = order.limit_price {
                row.set_item("limit_price", limit_price)?;
            } else {
                row.set_item("limit_price", py.None())?;
            }
            row.set_item("status", &order.status)?;
            if let Some(dt) = &order.datetime {
                row.set_item("datetime", dt)?;
            } else {
                row.set_item("datetime", py.None())?;
            }
            if let Some(filled_price) = order.filled_price {
                row.set_item("filled_price", filled_price)?;
            } else {
                row.set_item("filled_price", py.None())?;
            }
            row.set_item("filled_size", order.filled_size)?;
            row.set_item("commission", order.commission)?;
            row.set_item("realized_pnl", order.realized_pnl)?;
            if let Some(reason) = &order.reject_reason {
                row.set_item("reject_reason", reason)?;
            } else {
                row.set_item("reject_reason", py.None())?;
            }
            order_list.append(row)?;
        }
        result.set_item("orders", order_list)?;

        let stats =
            self.compute_enhanced_stats(py, &equity_curve, &trades, &orders_records, realized_pnl)?;
        result.set_item("stats", stats)?;

        Ok(result.into())
    }
}

#[pyfunction]
fn factor_backtest_fast(
    py: Python<'_>,
    closes: Vec<f64>,
    factors: Vec<f64>,
    quantiles: usize,
    forward: usize,
) -> PyResult<PyObject> {
    let n = closes.len().min(factors.len());
    if quantiles < 2 || forward == 0 || n <= forward {
        let empty = PyDict::new_bound(py);
        empty.set_item("quantiles", PyList::empty_bound(py))?;
        empty.set_item("mean_returns", PyList::empty_bound(py))?;
        empty.set_item("ic", py.None())?;
        empty.set_item("monotonicity", 0.0)?;
        empty.set_item("q_bounds", PyList::empty_bound(py))?;
        empty.set_item("factor_stats", PyDict::new_bound(py))?;
        return Ok(empty.into());
    }

    let m = n - forward;

    // Forward returns
    let mut fwd_returns: Vec<f64> = Vec::with_capacity(m);
    for i in 0..m {
        let c0 = closes[i];
        let c1 = closes[i + forward];
        let r = if c0 != 0.0 { (c1 / c0) - 1.0 } else { 0.0 };
        fwd_returns.push(r);
    }

    // Trimmed factors
    let mut fac_trim: Vec<f64> = factors[..m].to_vec();

    // Quantile bounds
    let mut sorted = fac_trim.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut q_bounds: Vec<f64> = Vec::with_capacity(quantiles.saturating_sub(1));
    for q in 1..quantiles {
        let idx = (sorted.len() * q) / quantiles;
        let idx = idx.min(sorted.len().saturating_sub(1));
        q_bounds.push(sorted[idx]);
    }

    // Group stats (sums & counts)
    let mut sums: Vec<f64> = vec![0.0; quantiles];
    let mut counts: Vec<usize> = vec![0; quantiles];

    for (val, ret) in fac_trim.iter().zip(fwd_returns.iter()) {
        // Find group by linear scan (quantiles is small, typically <= 10)
        let mut gi = 0usize;
        while gi < q_bounds.len() && *val > q_bounds[gi] {
            gi += 1;
        }
        sums[gi] += *ret;
        counts[gi] += 1;
    }

    // Mean returns per quantile
    let mut mean_returns: Vec<f64> = Vec::with_capacity(quantiles);
    for i in 0..quantiles {
        if counts[i] > 0 {
            mean_returns.push(sums[i] / counts[i] as f64);
        } else {
            mean_returns.push(0.0);
        }
    }

    // IC: Pearson correlation between fac_trim and fwd_returns
    let sum_f: f64 = fac_trim.iter().sum();
    let sum_r: f64 = fwd_returns.iter().sum();
    let mean_f = sum_f / m as f64;
    let mean_r = sum_r / m as f64;
    let mut cov = 0.0_f64;
    let mut var_f = 0.0_f64;
    let mut var_r = 0.0_f64;
    for i in 0..m {
        let df = fac_trim[i] - mean_f;
        let dr = fwd_returns[i] - mean_r;
        cov += df * dr;
        var_f += df * df;
        var_r += dr * dr;
    }
    let denom = (var_f * var_r).sqrt() + 1e-12;
    let ic = cov / denom;

    // Monotonicity of mean returns across quantiles
    let mut inc = 0i32;
    let mut dec = 0i32;
    if mean_returns.len() > 1 {
        for i in 1..mean_returns.len() {
            if mean_returns[i] > mean_returns[i - 1] {
                inc += 1;
            }
            if mean_returns[i] < mean_returns[i - 1] {
                dec += 1;
            }
        }
    }
    let denom_m = (mean_returns.len().saturating_sub(1)) as f64;
    let monotonicity = if denom_m > 0.0 {
        (inc - dec) as f64 / denom_m
    } else {
        0.0
    };

    // Factor stats
    let min_f = fac_trim
        .iter()
        .cloned()
        .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
    let max_f = fac_trim
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
    let mean_f_all = mean_f;
    let std_f = if m > 1 {
        let mut vs = 0.0_f64;
        for v in fac_trim.iter() {
            let d = *v - mean_f_all;
            vs += d * d;
        }
        (vs / m as f64).sqrt()
    } else {
        0.0
    };

    // Build Python result dict
    let out = PyDict::new_bound(py);
    let q_list = PyList::empty_bound(py);
    for i in 1..=quantiles {
        q_list.append(i as i32)?;
    }
    out.set_item("quantiles", q_list)?;

    let mr_list = PyList::empty_bound(py);
    for v in mean_returns.iter() {
        mr_list.append(*v)?;
    }
    out.set_item("mean_returns", mr_list)?;

    out.set_item("ic", ic)?;
    out.set_item("monotonicity", monotonicity)?;

    let qb_list = PyList::empty_bound(py);
    for v in q_bounds.iter() {
        qb_list.append(*v)?;
    }
    out.set_item("q_bounds", qb_list)?;

    let fs = PyDict::new_bound(py);
    fs.set_item("mean", mean_f_all)?;
    fs.set_item("std", std_f)?;
    fs.set_item("min", min_f)?;
    fs.set_item("max", max_f)?;
    out.set_item("factor_stats", fs)?;

    Ok(out.into())
}

#[pymodule]
fn engine_rust(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<BacktestConfig>()?;
    m.add_class::<BacktestEngine>()?;
    m.add_class::<EngineContext>()?;
    m.add_function(wrap_pyfunction!(compute_sma, m)?)?;
    m.add_function(wrap_pyfunction!(compute_rsi, m)?)?;
    m.add_function(wrap_pyfunction!(factor_backtest_fast, m)?)?;
    // Database functions
    m.add_function(wrap_pyfunction!(database::get_market_data, m)?)?;
    m.add_function(wrap_pyfunction!(database::resample_klines, m)?)?;
    m.add_function(wrap_pyfunction!(database::save_klines, m)?)?;
    m.add_function(wrap_pyfunction!(database::save_klines_from_csv, m)?)?;
    Ok(())
}
