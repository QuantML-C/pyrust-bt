# Factor Analysis

pyrust-bt includes two factor research paths:

1. Simple factor backtest on bar lists.
2. Cross-sectional factor and quantile portfolio analysis with pandas.

## Simple Factor Backtest

Use `factor_backtest()` when each bar already contains a factor value.

```python
from pyrust_bt.analyzers import factor_backtest

result = factor_backtest(
    bars,
    factor_key="momentum_20",
    quantiles=5,
    forward=5,
)

print(result["ic"])
print(result["mean_returns"])
```

The result includes:

| Field | Meaning |
|---|---|
| `quantiles` | Quantile labels |
| `mean_returns` | Mean forward return by quantile |
| `ic` | Pearson correlation between factor and forward returns |
| `monotonicity` | Directional consistency across quantile returns |
| `q_bounds` | Quantile boundaries |
| `factor_stats` | Mean, standard deviation, min, max |

For larger datasets, the Python function automatically tries the Rust fast path `factor_backtest_fast`.

## Multi-Factor Analyzer

`MultiFactorAnalyzer` evaluates multiple factor columns and builds a ranking.

```python
from pyrust_bt.multi_factor_analyzer import MultiFactorAnalyzer

analyzer = MultiFactorAnalyzer(
    bars,
    window_size=60,
    nan_policy="drop",
)

report = analyzer.analyze_all_factors()
analyzer.export_report(report, output_dir="factor_reports")
```

It can compute:

- IC and ICIR
- IC win rate
- Rank IC
- Quantile mean returns
- Monotonicity
- Turnover
- Factor decay
- Stability score
- Factor correlation matrix
- Factor ranking

## Cross-Sectional Quantile Backtester

Use `CrossSectionFactorBacktester` when your input is a pandas DataFrame with one row per `(datetime, symbol)`.

Required columns:

```text
datetime, symbol, factor, ret_next
```

Example:

```python
from pyrust_bt.cs_factor_backtester import BacktestConfigCS, CrossSectionFactorBacktester

cfg = BacktestConfigCS(
    factor_col="factor",
    ret_next_col="ret_next",
    datetime_col="datetime",
    symbol_col="symbol",
    quantiles=5,
    winsorize=(0.01, 0.99),
    standardize=True,
)

bt = CrossSectionFactorBacktester(df, cfg)
result = bt.run(compute_long_short=True)
bt.export(result, out_dir="examples/results/cs_factor")
```

Outputs include:

- Return series by quantile.
- Equity series by quantile.
- Statistics by quantile.
- Optional long-short return and equity.
- Turnover by quantile.

## When to Use Which Tool

| Tool | Best for |
|---|---|
| `factor_backtest()` | Quick single-factor checks on one bar list |
| `MultiFactorAnalyzer` | Ranking many factor columns and exporting reports |
| `CrossSectionFactorBacktester` | Daily cross-sectional factor research and quantile portfolios |
| `engine.run_multi()` | Trading simulation after factor signals have been converted to orders |

## Important Assumptions

- `ret_next` should be prepared before cross-sectional backtesting.
- Standardization and winsorization are done within each date when configured.
- Quantile portfolio returns are equal-weighted by default.
- The cross-sectional backtester does not model order fills, slippage, or commission. Use `run_multi()` for trading simulation.
