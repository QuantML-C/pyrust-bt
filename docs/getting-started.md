# Getting Started

This guide builds the Rust extension and runs the first backtest from the repository root.

## Requirements

- Python 3.8 or newer
- Rust installed through `rustup`
- `maturin`
- A shell that can run PowerShell commands on Windows

Install `maturin`:

```powershell
pip install maturin
```

Verify Rust:

```powershell
rustc --version
cargo --version
```

## Build the Rust Extension

The Python package imports `engine_rust`, which is produced by the Rust crate under `rust/engine_rust`.

```powershell
cd rust/engine_rust
maturin develop --release
cd ../..
```

Use the debug build only when you are actively debugging Rust:

```powershell
cd rust/engine_rust
maturin develop
cd ../..
```

## Run the Minimal Example

```powershell
python examples/run_mvp.py
```

This example:

- Loads CSV bars from `examples/data/sh600000_min.csv`.
- Creates a `BacktestConfig`.
- Runs a simple SMA strategy.
- Prints account state, return metrics, risk metrics, and trade statistics.

## Minimal Python Script

If you run this as your own script from the repository root, make sure Python can see the local package:

```powershell
$env:PYTHONPATH="python"
```

```python
from pyrust_bt.api import BacktestConfig, BacktestEngine
from pyrust_bt.data import load_csv_to_bars
from pyrust_bt.strategy import Strategy


class MyStrategy(Strategy):
    def next(self, bar):
        if bar["close"] > 100:
            return {"action": "BUY", "type": "market", "size": 1.0}
        return None


cfg = BacktestConfig(
    start="2020-01-01",
    end="2025-12-31",
    cash=100000.0,
    commission_rate=0.0005,
    slippage_bps=2.0,
    batch_size=1000,
)

bars = load_csv_to_bars("examples/data/sample.csv", symbol="SAMPLE")
result = BacktestEngine(cfg).run(MyStrategy(), bars)

print(result["cash"])
print(result["equity"])
print(result["stats"])
```

## Input Data Format

The basic CSV loader expects these columns:

```text
datetime,open,high,low,close,volume
```

The loader returns a list of dictionaries:

```python
{
    "datetime": "2024-01-02 09:30:00",
    "open": 100.0,
    "high": 101.0,
    "low": 99.5,
    "close": 100.5,
    "volume": 100000.0,
    "symbol": "SAMPLE",
}
```

## Result Shape

`engine.run()` returns a dictionary with:

| Key | Meaning |
|---|---|
| `cash` | Ending cash balance |
| `position` | Ending single-asset position |
| `avg_cost` | Average cost of the open position |
| `equity` | Ending equity |
| `realized_pnl` | Realized PnL from closed quantity |
| `equity_curve` | List of `{datetime, equity}` rows |
| `trades` | Filled trades |
| `stats` | Summary metrics |

Common statistics include `total_return`, `annualized_return`, `volatility`, `sharpe`, `calmar`, `max_drawdown`, `max_dd_duration`, `total_trades`, and `win_rate`.

## Next Steps

- Read [Strategy Guide](strategy-guide.md) to write real strategies.
- Read [Examples](examples.md) to pick a runnable workflow.
- Read [Troubleshooting](troubleshooting.md) if the Rust extension cannot be imported.
