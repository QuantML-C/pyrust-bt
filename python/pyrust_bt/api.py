from __future__ import annotations
from typing import Any, Dict, List

try:
    from engine_rust import BacktestEngine as _RustBacktestEngine, BacktestConfig  # type: ignore
except Exception as exc:  # pragma: no cover
    raise RuntimeError("engine_rust extension is not built. Run 'maturin develop' under rust/engine_rust'.") from exc


class BacktestEngine:
    """
    Python-side wrapper around the Rust BacktestEngine for convenience.
    """

    def __init__(self, cfg: BacktestConfig) -> None:
        self._engine = _RustBacktestEngine(cfg)

    def run(self, strategy: Any, bars: List[Dict[str, Any]]) -> Dict[str, Any]:
        return self._engine.run(strategy, bars)  # type: ignore[no-any-return] 