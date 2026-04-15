# Market Data

pyrust-bt supports three practical data paths:

1. CSV for quick experiments.
2. DuckDB for local historical data.
3. DuckDB plus QMT / xtdata fallback for missing ranges.

## CSV

The basic loader is `load_csv_to_bars()`:

```python
from pyrust_bt.data import load_csv_to_bars

bars = load_csv_to_bars("examples/data/sample.csv", symbol="SAMPLE")
```

Expected columns:

```text
datetime,open,high,low,close,volume
```

Use CSV when the dataset is small, portable, or part of an example.

## DuckDB Local Store

The Rust extension exposes DuckDB functions:

- `get_market_data`
- `save_klines`
- `save_klines_from_csv`
- `resample_klines`

Use `examples/import_csv_to_db.py` to import a CSV:

```powershell
python examples/import_csv_to_db.py data/sh600000_min.csv --symbol 600000.SH --period 1m --db data/backtest.db
```

Use `--no-direct-csv` when you want Python to parse the CSV first before writing through Rust.

DuckDB tables are created per period, such as:

```text
klines_1m
klines_1d
```

Each table stores:

```text
symbol, datetime, open, high, low, close, volume
```

## DB-First Service

The high-level service is `MarketDataService`.

```python
from pyrust_bt.market_data import DataRequest, MarketDataConfig, MarketDataService

config = MarketDataConfig(
    db_path="data/backtest.db",
    xtdata_enabled=False,
)

service = MarketDataService(config)

request = DataRequest(
    symbols=["513500.SH"],
    period="1d",
    start_time="2022-01-01",
    end_time="2025-12-31",
)

bars = service.fetch_bars(request, symbol="513500.SH")
```

When `xtdata_enabled=False`, missing data raises an error instead of downloading.

## QMT / xtdata Fallback

When QMT support is enabled, the service flow is:

```text
read DuckDB
  -> detect missing ranges
  -> download missing data from xtdata
  -> save to DuckDB
  -> read DuckDB again
```

Typical configuration:

```python
config = MarketDataConfig(
    db_path="data/backtest.db",
    xtdata_enabled=True,
    xtdata_data_dir=r"D:\path\to\userdata_mini",
)
```

Environment checklist:

- The QMT / MiniQmt client is installed and configured.
- The `xtquant` Python package is importable.
- `XTDATA_DIR` points to the correct `userdata_mini` directory when used by examples.
- The requested symbol format matches the data vendor, such as `513500.SH` or `159941.SZ`.

## Choosing a Data Path

| Use case | Recommended path |
|---|---|
| First demo | CSV |
| Small experiment | CSV or DuckDB |
| Repeated research | DuckDB |
| Multi-asset local research | DuckDB |
| China A-share or ETF workflow with QMT | DuckDB + xtdata fallback |
| Large production pipeline | Extend the data layer with Parquet / Arrow or a dedicated feed |

## Data Quality Notes

The service sanitizes data frames by sorting timestamps and removing duplicate index entries. It does not currently handle trading calendars, symbol suspensions, corporate actions, or timezone normalization as full first-class concepts. Add those checks in the data preparation layer when your research depends on them.
