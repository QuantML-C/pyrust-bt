from __future__ import annotations

import os
import sys
import unittest


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYTHON_DIR = os.path.join(ROOT, "python")
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)

try:
    from pyrust_bt.api import BacktestConfig, BacktestEngine
    from pyrust_bt.strategy import Strategy

    IMPORT_ERROR = None
except Exception as exc:  # pragma: no cover - depends on local build state
    BacktestConfig = None
    BacktestEngine = None
    Strategy = object
    IMPORT_ERROR = exc


def bars_from_closes(closes, symbol="TEST"):
    return [
        {
            "datetime": f"2024-01-01 09:{30 + idx:02d}:00",
            "open": float(close),
            "high": float(close),
            "low": float(close),
            "close": float(close),
            "volume": 1000.0,
            "symbol": symbol,
        }
        for idx, close in enumerate(closes)
    ]


class ScriptedStrategy(Strategy):
    def __init__(self, actions):
        self.actions = list(actions)
        self.index = 0

    def next(self, bar):
        if self.index >= len(self.actions):
            return None
        action = self.actions[self.index]
        self.index += 1
        return action


@unittest.skipIf(IMPORT_ERROR is not None, f"engine_rust is not built: {IMPORT_ERROR}")
class PositionAccountingTests(unittest.TestCase):
    def make_engine(self):
        return BacktestEngine(
            BacktestConfig(
                start="2024-01-01",
                end="2024-01-02",
                cash=1000.0,
                commission_rate=0.0,
                slippage_bps=0.0,
            )
        )

    def run_actions(self, closes, actions):
        return self.make_engine().run(ScriptedStrategy(actions), bars_from_closes(closes))

    def test_long_add_then_partial_sell_keeps_remaining_avg_cost(self):
        result = self.run_actions(
            [10.0, 12.0, 15.0],
            [
                {"action": "BUY", "type": "market", "size": 1.0},
                {"action": "BUY", "type": "market", "size": 1.0},
                {"action": "SELL", "type": "market", "size": 1.0},
            ],
        )

        self.assertAlmostEqual(result["position"], 1.0)
        self.assertAlmostEqual(result["avg_cost"], 11.0)
        self.assertAlmostEqual(result["realized_pnl"], 4.0)
        self.assertAlmostEqual(result["equity"], 1008.0)

    def test_long_flip_to_short_opens_remainder_at_execution_price(self):
        result = self.run_actions(
            [10.0, 12.0],
            [
                {"action": "BUY", "type": "market", "size": 1.0},
                {"action": "SELL", "type": "market", "size": 3.0},
            ],
        )

        self.assertAlmostEqual(result["position"], -2.0)
        self.assertAlmostEqual(result["avg_cost"], 12.0)
        self.assertAlmostEqual(result["realized_pnl"], 2.0)
        self.assertAlmostEqual(result["equity"], 1002.0)

    def test_short_add_then_partial_cover_keeps_remaining_avg_cost(self):
        result = self.run_actions(
            [10.0, 8.0, 6.0],
            [
                {"action": "SELL", "type": "market", "size": 1.0},
                {"action": "SELL", "type": "market", "size": 1.0},
                {"action": "BUY", "type": "market", "size": 1.0},
            ],
        )

        self.assertAlmostEqual(result["position"], -1.0)
        self.assertAlmostEqual(result["avg_cost"], 9.0)
        self.assertAlmostEqual(result["realized_pnl"], 3.0)
        self.assertAlmostEqual(result["equity"], 1006.0)

    def test_short_flip_to_long_opens_remainder_at_execution_price(self):
        result = self.run_actions(
            [10.0, 8.0],
            [
                {"action": "SELL", "type": "market", "size": 1.0},
                {"action": "BUY", "type": "market", "size": 3.0},
            ],
        )

        self.assertAlmostEqual(result["position"], 2.0)
        self.assertAlmostEqual(result["avg_cost"], 8.0)
        self.assertAlmostEqual(result["realized_pnl"], 2.0)
        self.assertAlmostEqual(result["equity"], 1002.0)

    def test_full_close_resets_avg_cost(self):
        result = self.run_actions(
            [10.0, 11.0],
            [
                {"action": "BUY", "type": "market", "size": 1.0},
                {"action": "SELL", "type": "market", "size": 1.0},
            ],
        )

        self.assertAlmostEqual(result["position"], 0.0)
        self.assertAlmostEqual(result["avg_cost"], 0.0)
        self.assertAlmostEqual(result["realized_pnl"], 1.0)
        self.assertAlmostEqual(result["equity"], 1001.0)

    def test_multi_asset_position_accounting_uses_same_flip_logic(self):
        class MultiFlipStrategy(Strategy):
            def __init__(self):
                self.index = 0

            def next_multi(self, update_slice, ctx):
                self.index += 1
                if self.index == 1:
                    return {"action": "SELL", "type": "market", "size": 1.0, "symbol": "AAA"}
                if self.index == 2:
                    return {"action": "BUY", "type": "market", "size": 3.0, "symbol": "AAA"}
                return None

        feeds = {
            "AAA": bars_from_closes([10.0, 8.0], symbol="AAA"),
            "BBB": bars_from_closes([20.0, 20.0], symbol="BBB"),
        }
        result = self.make_engine().run_multi(MultiFlipStrategy(), feeds)

        self.assertAlmostEqual(result["realized_pnl"], 2.0)
        self.assertAlmostEqual(result["equity"], 1002.0)


if __name__ == "__main__":
    unittest.main()
