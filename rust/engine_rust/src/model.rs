// 核心数据结构：bar / 订单 / 成交 / 持仓 / 策略上下文

use chrono::NaiveDateTime;
use pyo3::exceptions::PyKeyError;
use pyo3::prelude::*;
use pyo3::types::PyAny;

// 预提取的bar数据结构
#[derive(Clone, Debug)]
pub(crate) struct BarData {
    pub datetime: Option<String>,
    // 提取阶段解析出的时间，用于排序/比较（提取时保证可解析）
    pub parsed_dt: NaiveDateTime,
    pub close: f64,
    pub symbol: Option<String>,
    // 用户传入的原始 bar 对象，调用策略时直通（不重建 dict）
    pub py_obj: PyObject,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum OrderSide {
    Buy,
    Sell,
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum OrderType {
    Market,
    Limit,
}

#[derive(Clone, Debug)]
pub(crate) struct Order {
    pub id: u64,
    pub side: OrderSide,
    pub otype: OrderType,
    pub size: f64,
    pub limit_price: Option<f64>,
    pub symbol: String,
}

#[derive(Clone, Debug)]
pub(crate) struct TradeRecord {
    pub order_id: u64,
    pub symbol: String,
    pub side: String,
    pub price: f64,
    pub size: f64,
    pub datetime: Option<String>,
    pub commission: f64,
    pub realized_pnl: f64,
}

#[derive(Clone, Debug)]
pub(crate) struct OrderRecord {
    pub order_id: u64,
    pub symbol: String,
    pub side: String,
    pub otype: String,
    pub size: f64,
    pub limit_price: Option<f64>,
    pub status: String,
    pub datetime: Option<String>,
    pub filled_price: Option<f64>,
    pub filled_size: f64,
    pub commission: f64,
    pub realized_pnl: f64,
    pub reject_reason: Option<String>,
}

#[derive(Default, Clone, Debug)]
pub(crate) struct PositionState {
    pub position: f64,
    pub avg_cost: f64,
    pub cash: f64,
    pub realized_pnl: f64,
}

impl PositionState {
    pub fn new(cash: f64) -> Self {
        Self {
            position: 0.0,
            avg_cost: 0.0,
            cash,
            realized_pnl: 0.0,
        }
    }
}

/// 单/多资产共用的策略上下文。
///
/// 同时支持属性访问（ctx.cash）与字典访问（ctx["cash"]），向后兼容。
/// 整个回测只创建一个实例，引擎每 bar / 每个时间步就地更新字段；
/// 策略若在回调之间持有该对象引用，读到的是最新快照。
#[pyclass]
#[derive(Clone)]
pub struct EngineContext {
    /// 当前持仓（多资产模式下为主 feed symbol 的持仓）
    #[pyo3(get, set)]
    pub position: f64,
    /// 平均成本（多资产模式下为主 feed symbol 的成本）
    #[pyo3(get, set)]
    pub avg_cost: f64,
    #[pyo3(get, set)]
    pub cash: f64,
    #[pyo3(get, set)]
    pub equity: f64,
    #[pyo3(get, set)]
    pub bar_index: usize,
    /// dict: symbol -> {"position": float, "avg_cost": float}
    #[pyo3(get, set)]
    pub positions: PyObject,
    /// dict: symbol -> 最新价
    #[pyo3(get, set)]
    pub last_prices: PyObject,
}

#[pymethods]
impl EngineContext {
    fn __getitem__(&self, py: Python, key: &str) -> PyResult<PyObject> {
        match key {
            "position" => Ok(self.position.into_py(py)),
            "avg_cost" => Ok(self.avg_cost.into_py(py)),
            "cash" => Ok(self.cash.into_py(py)),
            "equity" => Ok(self.equity.into_py(py)),
            "bar_index" => Ok(self.bar_index.into_py(py)),
            "positions" => Ok(self.positions.clone_ref(py)),
            "last_prices" => Ok(self.last_prices.clone_ref(py)),
            other => Err(PyKeyError::new_err(other.to_string())),
        }
    }

    fn __setitem__(&mut self, key: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
        match key {
            "position" => self.position = value.extract()?,
            "avg_cost" => self.avg_cost = value.extract()?,
            "cash" => self.cash = value.extract()?,
            "equity" => self.equity = value.extract()?,
            "bar_index" => self.bar_index = value.extract()?,
            "positions" => self.positions = value.clone().unbind(),
            "last_prices" => self.last_prices = value.clone().unbind(),
            other => return Err(PyKeyError::new_err(other.to_string())),
        }
        Ok(())
    }
}
