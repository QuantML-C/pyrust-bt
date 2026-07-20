from __future__ import annotations

import os
import sys
import unittest
from unittest import mock


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYTHON_DIR = os.path.join(ROOT, "python")
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)

from pyrust_bt import optimize


class _FakeEngine:
    """按策略参数返回预设 total_return 的假引擎。"""

    def __init__(self, cfg):
        self.cfg = cfg

    def run(self, strategy, bars):
        return {"stats": {"total_return": strategy.ret}, "equity": 1000.0}


class _ParamStrategy:
    def __init__(self, ret):
        self.ret = ret


class GridSearchTests(unittest.TestCase):
    def test_zero_score_ranks_before_negative_score(self):
        with mock.patch.object(optimize, "BacktestEngine", _FakeEngine):
            results = optimize.grid_search(
                cfg=None,
                bars=[],
                strategy_class=_ParamStrategy,
                param_grid={"ret": [-5.0, 0.0, 2.0]},
                score_key="total_return",
            )

        scores = [r[1]["score"] for r in results]
        # 降序：2.0 最前；score=0.0 是有效分数，必须排在 -5.0 之前
        self.assertEqual(scores, [2.0, 0.0, -5.0])

    def test_none_score_sorts_last(self):
        class _NoneScoreEngine(_FakeEngine):
            def run(self, strategy, bars):
                if strategy.ret is None:
                    return {"stats": {}, "equity": 1000.0}
                return {"stats": {"total_return": strategy.ret}, "equity": 1000.0}

        with mock.patch.object(optimize, "BacktestEngine", _NoneScoreEngine):
            results = optimize.grid_search(
                cfg=None,
                bars=[],
                strategy_class=_ParamStrategy,
                param_grid={"ret": [None, 1.0, 0.0]},
                score_key="total_return",
            )

        scores = [r[1]["score"] for r in results]
        self.assertEqual(scores, [1.0, 0.0, None])


if __name__ == "__main__":
    unittest.main()
