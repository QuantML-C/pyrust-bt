# Troubleshooting

This page lists common setup and runtime issues.

## `engine_rust extension is not built`

Symptom:

```text
RuntimeError: engine_rust extension is not built
```

Fix:

```powershell
cd rust/engine_rust
maturin develop --release
cd ../..
```

Then verify:

```powershell
python -c "import engine_rust; print(engine_rust)"
```

## Python Cannot Import `pyrust_bt`

When running from the repository root, examples add `python/` to `sys.path`. In your own scripts, use one of these approaches:

```powershell
$env:PYTHONPATH="python"
python your_script.py
```

or install the package layout in your environment if you add packaging metadata later.

## Rust Build Fails

Try:

```powershell
rustup update
cd rust/engine_rust
cargo clean
maturin develop --release
```

Also check that your Python version is compatible with the PyO3 `abi3-py38` configuration.

## `maturin` Is Missing

```powershell
pip install maturin
```

If you use a virtual environment, activate it before running `maturin develop`.

## CSV Loader Errors

The default loader expects:

```text
datetime,open,high,low,close,volume
```

Check:

- The file path exists.
- The header names match exactly.
- Numeric columns can be parsed as floats.
- The file is encoded as UTF-8.

## Empty Backtest Results

Check:

- `bars` is not empty.
- `bar["close"]` is present and non-zero.
- Your strategy returns an action.
- `size` is greater than zero.
- The symbol in a multi-asset action exists in the feed or latest price map.

## Limit Orders Do Not Fill

The current limit model is same-bar simplified execution:

- Buy limit fills if current price is less than or equal to the limit.
- Sell limit fills if current price is greater than or equal to the limit.

There is no persistent pending order book yet.

## QMT / xtdata Errors

Check:

- `xtquant` is importable in the active Python environment.
- QMT / MiniQmt has downloaded the requested market data.
- `XTDATA_DIR` points to the correct `userdata_mini` directory.
- Symbol suffixes match vendor conventions, for example `.SH` or `.SZ`.
- `MarketDataConfig.xtdata_enabled` is set correctly.

## Streamlit Cannot Reach API

Start FastAPI:

```powershell
python -m uvicorn python.server_main:app --reload
```

Set the frontend endpoint:

```powershell
set PYRUST_BT_API=http://127.0.0.1:8000
streamlit run frontend/streamlit_app.py
```

Open the API in a browser:

```text
http://127.0.0.1:8000/runs
```

## Performance Is Lower Than Expected

Try:

- Use `maturin develop --release`, not a debug build.
- Avoid printing from `next()` on every bar. (`batch_size` is deprecated and no longer affects execution.)
- Keep expensive pandas operations outside the strategy loop.
- Use Rust vectorized indicators for large arrays.

## Before Reporting an Issue

Include:

- Operating system.
- Python version.
- Rust version.
- `maturin` version.
- Exact command that failed.
- Full error message.
- Whether `python -c "import engine_rust"` works.
