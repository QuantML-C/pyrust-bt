# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Project Overview

pyrust-bt is a hybrid backtesting framework that combines Python for strategy development with Rust for high-performance execution. The framework is designed for quantitative trading research and small-team production use, balancing researcher productivity with engine throughput.

## Architecture

### Core Components

- **Rust Engine** (`rust/engine_rust/`): High-performance backtesting core with PyO3 bindings
  - Time-based advancement over bars/ticks
  - Order matching (market/limit with same-bar simplified execution)
  - Portfolio management with position tracking
  - Vectorized indicators (SMA, RSI) optimized for sliding windows
  - Enhanced statistics and performance metrics
  - Batch processing for reduced Python round-trips

- **Python API** (`python/pyrust_bt/`): User-facing interface for strategy development
  - `Strategy` base class with lifecycle methods (`on_start`, `next`, `on_stop`, `on_order`, `on_trade`)
  - Data loading utilities (CSV support, extensible to Parquet/Arrow)
  - Analysis tools for drawdowns, round-trips, performance metrics
  - Grid search optimizer for parameter tuning
  - Multi-asset backtesting support via `run_multi()` method

- **Examples** (`examples/`): Demonstrations of framework usage
  - Basic backtesting (`run_mvp.py`)
  - Advanced analysis (`run_analyzers.py`)
  - Parameter optimization (`run_grid_search.py`)
  - Performance testing (`run_performance_test.py`)
  - Multi-asset backtesting (`run_multi_assets.py`)
  - External holdings support (`run_external_holdings.py`)

- **Web Interface**: FastAPI backend with Streamlit frontend for remote backtesting

## Build System

### Prerequisites
- Python 3.8+
- Rust (installed via `rustup`)
- `maturin` for building Rust extensions

### Build Commands
```powershell
# Build the Rust engine
cd rust/engine_rust
maturin develop --release

# Alternative: Install in development mode
pip install maturin
maturin develop
```

### Running Examples
```powershell
# Basic backtest
python examples/run_mvp.py

# Analysis with enhanced metrics
python examples/run_analyzers.py

# Grid search optimization
python examples/run_grid_search.py

# Performance benchmarking
python examples/run_performance_test.py

# Multi-asset backtesting
python examples/run_multi_assets.py
```

### Starting Web Services
```powershell
# API server (FastAPI)
python -m uvicorn python.server_main:app --reload

# Frontend (Streamlit) - set API endpoint first
set PYRUST_BT_API=http://127.0.0.1:8000
streamlit run frontend/streamlit_app.py
```

## Key Design Patterns

### Strategy Implementation
Strategies extend the `Strategy` base class and implement the `next()` method:

```python
class MyStrategy(Strategy):
    def __init__(self, param1: float = 1.0):
        self.param1 = param1

    def next(self, bar: Dict[str, Any]) -> Optional[Union[str, Dict[str, Any]]]:
        # Return "BUY"/"SELL" strings or detailed action dicts
        return {"action": "BUY", "type": "market", "size": 1.0}
```

### Action Format
- String format: `"BUY"` or `"SELL"` (market orders, size=1)
- Dict format: `{"action": "BUY"|"SELL", "type": "market"|"limit", "size": float, "price"?: float}`

### Multi-Asset Support
Use `run_multi()` with feeds dictionary:
```python
feeds = {
    "asset1": bars1,
    "asset2": bars2,
}
result = engine.run_multi(strategy, feeds)
```

## Performance Optimization

### Batch Processing
The Rust engine uses configurable `batch_size` (default: 1000) to reduce Python GIL contention. Larger batches (1000-5000) are recommended for better performance.

### Vectorized Indicators
Prefer Rust-based indicators over Python implementations:
```python
from pyrust_bt import compute_sma, compute_rsi
sma_values = compute_sma(prices, window=20)
rsi_values = compute_rsi(prices, window=14)
```

### Data Format
For optimal performance with large datasets:
- Use Parquet/Arrow formats when possible
- Partition data by symbol and time
- Pre-allocate buffers and use columnar processing

## Data Structure

### Bar Data Format
Expected columns: `datetime,open,high,low,close,volume`
Optional: `symbol` (for multi-asset backtesting)

### Result Structure
Backtest results contain:
- Portfolio state: `cash`, `position`, `avg_cost`, `equity`, `realized_pnl`
- Time series: `equity_curve`, `trades`
- Statistics: performance metrics, drawdown analysis, trade statistics

## Common Issues

### Build Failures
- Ensure Rust toolchain is up to date: `rustup update`
- Clean build artifacts: `maturin build --release` after `cargo clean`
- Check Python-Rust version compatibility in `Cargo.toml`

### Import Errors
- The Rust extension must be built before importing: `maturin develop --release`
- Verify the Python path includes the project root

### Performance Bottlenecks
- Increase `batch_size` in BacktestConfig for better throughput
- Use dict actions instead of strings for complex order types
- Leverage Rust vectorized functions when possible

## Testing Strategy

### Unit Tests
- Test individual strategy logic in isolation
- Validate indicator calculations against known results
- Check edge cases for order matching and position updates

### Integration Tests
- End-to-end backtesting with sample data
- Multi-asset scenario validation
- Performance regression testing

### Benchmarking
Use `run_performance_test.py` to measure:
- Bars per second processing rate
- Memory usage patterns
- Scaling with different batch sizes

## Extension Points

### Custom Indicators
Add new indicators to `rust/engine_rust/src/lib.rs` and expose via PyO3.

### Data Sources
Extend `python/pyrust_bt/data.py` to support additional formats (Parquet, Arrow, databases).

### Analysis Tools
Add new analyzers in `python/pyrust_bt/analyzers.py` for specialized metrics.

### Order Types
Implement additional order types (stop, iceberg, conditional) in the Rust engine.