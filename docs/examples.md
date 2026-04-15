# Examples

The examples are designed as a learning path. Run them from the repository root after building the Rust extension.

## First Run

| Example | Command | What you learn |
|---|---|---|
| Minimal backtest | `python examples/run_mvp.py` | Load CSV bars, run a strategy, inspect stats |
| Analysis demo | `python examples/run_analyzers.py` | Drawdowns, round trips, reports |
| Performance test | `python examples/run_performance_test.py` | Batch size and throughput behavior |

## Strategy Research

| Example | Command | What you learn |
|---|---|---|
| Grid search | `python examples/run_grid_search.py` | Parameter combinations and score sorting |
| Multi-asset SMA | `python examples/run_multi_assets.py` | `run_multi()` with multiple feeds |
| Portfolio backtest | `python examples/run_portfolio_backtest.py` | Portfolio-level workflow and reporting |

## Market Data

| Example | Command | What you learn |
|---|---|---|
| CSV to DuckDB | `python examples/import_csv_to_db.py --help` | High-performance local data import |
| Multi-asset rebalance with data service | `python examples/run_multi_asset_rebalance_strategy.py` | DuckDB-first data, optional QMT / xtdata fallback |

## Factor Research

| Example | Command | What you learn |
|---|---|---|
| Cross-sectional momentum sample | `python examples/run_cs_momentum_sample.py` | Build factor data and run multi-factor analysis |
| Quantile portfolios | `python examples/run_cs_quantile_portfolios.py` | Convert factor ranks into portfolio simulations |

## Recommended Order

1. `python examples/run_mvp.py`
2. `python examples/run_analyzers.py`
3. `python examples/run_grid_search.py`
4. `python examples/run_multi_assets.py`
5. `python examples/run_performance_test.py`

Then choose either:

- Market data workflow: `examples/import_csv_to_db.py` and `examples/run_multi_asset_rebalance_strategy.py`
- Factor workflow: `examples/run_cs_momentum_sample.py` and `examples/run_cs_quantile_portfolios.py`

## Common Example Pattern

Most examples follow the same pattern:

```python
cfg = BacktestConfig(...)
engine = BacktestEngine(cfg)
bars = load_csv_to_bars(...)
strategy = MyStrategy(...)
result = engine.run(strategy, bars)
print(result["stats"])
```

Multi-asset examples use:

```python
feeds = {
    "asset1": bars1,
    "asset2": bars2,
}

result = engine.run_multi(strategy, feeds)
```

## Output Files

Some examples export CSV or JSON files for inspection. These outputs are generated artifacts and can usually be deleted safely after experimentation.
