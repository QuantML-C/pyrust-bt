// pyrust-bt Rust 引擎：PyO3 模块注册入口。
// 具体实现按职责拆分在各子模块：
//   config.rs        回测配置与日期范围解析
//   model.rs         BarData / Order / TradeRecord / PositionState / EngineContext
//   indicators.rs    向量化 SMA / RSI
//   engine_single.rs 单资产回测主循环 + bar 提取 + 订单解析/撮合
//   engine_multi.rs  多资产/多周期（联合时间线）回测
//   accounting.rs    持仓与现金记账
//   stats.rs         结果构建与统计指标
//   factor.rs        因子分层回测快速路径
//   database.rs      DuckDB K 线存取

use pyo3::prelude::*;

mod accounting;
mod config;
mod database;
mod engine_multi;
mod engine_single;
mod factor;
mod indicators;
mod model;
mod stats;

#[pymodule]
fn engine_rust(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<config::BacktestConfig>()?;
    m.add_class::<engine_single::BacktestEngine>()?;
    m.add_class::<model::EngineContext>()?;
    m.add_function(wrap_pyfunction!(indicators::compute_sma, m)?)?;
    m.add_function(wrap_pyfunction!(indicators::compute_rsi, m)?)?;
    m.add_function(wrap_pyfunction!(factor::factor_backtest_fast, m)?)?;
    // Database functions
    m.add_function(wrap_pyfunction!(database::get_market_data, m)?)?;
    m.add_function(wrap_pyfunction!(database::resample_klines, m)?)?;
    m.add_function(wrap_pyfunction!(database::save_klines, m)?)?;
    m.add_function(wrap_pyfunction!(database::save_klines_from_csv, m)?)?;
    m.add_function(wrap_pyfunction!(database::load_and_synthesize_klines, m)?)?;
    Ok(())
}
