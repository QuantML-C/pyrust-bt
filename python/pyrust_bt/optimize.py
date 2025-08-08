from __future__ import annotations
from typing import Any, Dict, Iterable, List, Tuple
import itertools

from .api import BacktestEngine, BacktestConfig


def grid_search(
    cfg: BacktestConfig,
    bars: List[Dict[str, Any]],
    strategy_class: Any,
    param_grid: Dict[str, Iterable[Any]],
    score_key: str = "total_return",
) -> List[Tuple[Dict[str, Any], Dict[str, Any]]]:
    """
    朴素网格搜索：遍历参数组合，返回 [(params, result_dict)] 排序列表。
    result_dict 里包含 'stats' 字段，默认按 stats[score_key] 降序排序。
    """
    items = sorted(param_grid.items(), key=lambda kv: kv[0])
    keys = [k for k, _ in items]
    values = [list(v) for _, v in items]

    results: List[Tuple[Dict[str, Any], Dict[str, Any]]] = []
    engine = BacktestEngine(cfg)

    for combo in itertools.product(*values):
        params = {k: v for k, v in zip(keys, combo)}
        strategy = strategy_class(**params)
        res = engine.run(strategy, bars)  # type: ignore[assignment]
        stats = res.get("stats", {}) or {}
        results.append((params, {**res, "score": stats.get(score_key)}))

    results.sort(key=lambda x: (x[1].get("score") is None, -(x[1].get("score") or float("-inf"))))
    return results 