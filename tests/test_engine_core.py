from __future__ import annotations

import os
import sys
import unittest


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYTHON_DIR = os.path.join(ROOT, "python")
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)

try:
    from engine_rust import compute_sma
    from pyrust_bt.api import BacktestConfig, BacktestEngine
    from pyrust_bt.strategy import Strategy

    IMPORT_ERROR = None
except Exception as exc:  # pragma: no cover - depends on local build state
    compute_sma = None
    BacktestConfig = None
    BacktestEngine = None
    Strategy = object
    IMPORT_ERROR = exc


def make_bars(symbol: str = "TEST"):
    return [
        {
            "datetime": "2024-01-01 09:30:00",
            "open": 10.0,
            "high": 10.5,
            "low": 9.5,
            "close": 10.0,
            "volume": 1000.0,
            "symbol": symbol,
        },
        {
            "datetime": "2024-01-01 09:31:00",
            "open": 10.0,
            "high": 12.5,
            "low": 9.8,
            "close": 12.0,
            "volume": 1000.0,
            "symbol": symbol,
        },
    ]


@unittest.skipIf(IMPORT_ERROR is not None, f"engine_rust is not built: {IMPORT_ERROR}")
class EngineCoreTests(unittest.TestCase):
    def make_engine(self):
        cfg = BacktestConfig(
            start="2024-01-01",
            end="2024-01-02",
            cash=1000.0,
            commission_rate=0.001,
            slippage_bps=0.0,
        )
        return BacktestEngine(cfg)

    def test_compute_sma_window_alignment(self):
        self.assertEqual(compute_sma([1.0, 2.0, 3.0, 4.0], 3), [None, None, 2.0, 3.0])

    def test_single_asset_final_equity_uses_last_equity_curve_value(self):
        class BuyOnceStrategy(Strategy):
            def __init__(self):
                self.done = False

            def next(self, bar, ctx):
                if self.done:
                    return None
                self.done = True
                return {"action": "BUY", "type": "market", "size": 1.0}

        result = self.make_engine().run(BuyOnceStrategy(), make_bars())
        self.assertAlmostEqual(result["equity"], result["equity_curve"][-1]["equity"])

    def test_strategy_exception_is_propagated(self):
        class RaisingStrategy(Strategy):
            def next(self, bar, ctx):
                raise ValueError("intentional strategy failure")

        with self.assertRaisesRegex(ValueError, "intentional strategy failure"):
            self.make_engine().run(RaisingStrategy(), make_bars())

    def test_one_arg_strategy_still_works(self):
        class OneArgStrategy(Strategy):
            def next(self, bar):
                return {"action": "BUY", "type": "market", "size": 1.0}

        result = self.make_engine().run(OneArgStrategy(), make_bars())
        self.assertGreaterEqual(len(result["trades"]), 1)

    def test_trade_records_include_metadata(self):
        class BuyOnceStrategy(Strategy):
            def __init__(self):
                self.done = False

            def next(self, bar):
                if self.done:
                    return None
                self.done = True
                return {"action": "BUY", "type": "market", "size": 1.0}

        result = self.make_engine().run(BuyOnceStrategy(), make_bars(symbol="SINGLE"))
        trade = result["trades"][0]

        for key in (
            "order_id",
            "side",
            "price",
            "size",
            "symbol",
            "datetime",
            "commission",
            "realized_pnl",
        ):
            self.assertIn(key, trade)
        self.assertEqual(trade["symbol"], "SINGLE")
        self.assertEqual(trade["datetime"], "2024-01-01 09:30:00")
        self.assertAlmostEqual(trade["commission"], 0.01)
        self.assertAlmostEqual(trade["realized_pnl"], 0.0)

    def test_multi_asset_trade_records_include_symbol(self):
        class MultiBuyStrategy(Strategy):
            def __init__(self):
                self.done = False

            def next_multi(self, update_slice, ctx):
                if self.done:
                    return None
                self.done = True
                return [
                    {"action": "BUY", "type": "market", "size": 1.0, "symbol": "AAA"},
                    {"action": "BUY", "type": "market", "size": 1.0, "symbol": "BBB"},
                ]

        feeds = {
            "AAA": make_bars(symbol="AAA"),
            "BBB": make_bars(symbol="BBB"),
        }
        result = self.make_engine().run_multi(MultiBuyStrategy(), feeds)
        symbols = {trade["symbol"] for trade in result["trades"]}

        self.assertEqual(symbols, {"AAA", "BBB"})
        for trade in result["trades"]:
            self.assertIn("datetime", trade)
            self.assertIn("commission", trade)
            self.assertIn("realized_pnl", trade)

    def test_config_start_end_filter_single_asset_bars(self):
        class NoopStrategy(Strategy):
            def next(self, bar):
                return None

        cfg = BacktestConfig(
            start="2024-01-01 09:31:00",
            end="2024-01-01 09:31:00",
            cash=1000.0,
            commission_rate=0.0,
            slippage_bps=0.0,
        )
        result = BacktestEngine(cfg).run(NoopStrategy(), make_bars())

        self.assertEqual(len(result["equity_curve"]), 1)
        self.assertEqual(result["equity_curve"][0]["datetime"], "2024-01-01 09:31:00")

    def test_config_date_only_end_includes_entire_day(self):
        class NoopStrategy(Strategy):
            def next(self, bar):
                return None

        bars = [
            {
                "datetime": "2024-01-01 15:00:00",
                "open": 10.0,
                "high": 10.0,
                "low": 10.0,
                "close": 10.0,
                "volume": 1000.0,
                "symbol": "TEST",
            },
            {
                "datetime": "2024-01-02 09:30:00",
                "open": 11.0,
                "high": 11.0,
                "low": 11.0,
                "close": 11.0,
                "volume": 1000.0,
                "symbol": "TEST",
            },
        ]
        cfg = BacktestConfig(
            start="2024-01-01",
            end="2024-01-01",
            cash=1000.0,
            commission_rate=0.0,
            slippage_bps=0.0,
        )
        result = BacktestEngine(cfg).run(NoopStrategy(), bars)

        self.assertEqual(len(result["equity_curve"]), 1)
        self.assertEqual(result["equity_curve"][0]["datetime"], "2024-01-01 15:00:00")

    def test_config_start_end_filter_multi_asset_timeline(self):
        class NoopStrategy(Strategy):
            def next_multi(self, update_slice, ctx):
                return None

        feeds = {
            "AAA": [
                {
                    "datetime": "2024-01-01 09:30:00",
                    "open": 10.0,
                    "high": 10.0,
                    "low": 10.0,
                    "close": 10.0,
                    "volume": 1000.0,
                    "symbol": "AAA",
                },
                {
                    "datetime": "2024-01-02 09:30:00",
                    "open": 11.0,
                    "high": 11.0,
                    "low": 11.0,
                    "close": 11.0,
                    "volume": 1000.0,
                    "symbol": "AAA",
                },
            ],
            "BBB": [
                {
                    "datetime": "2024-01-01 09:30:00",
                    "open": 20.0,
                    "high": 20.0,
                    "low": 20.0,
                    "close": 20.0,
                    "volume": 1000.0,
                    "symbol": "BBB",
                },
                {
                    "datetime": "2024-01-02 09:30:00",
                    "open": 21.0,
                    "high": 21.0,
                    "low": 21.0,
                    "close": 21.0,
                    "volume": 1000.0,
                    "symbol": "BBB",
                },
            ],
        }
        cfg = BacktestConfig(
            start="2024-01-02",
            end="2024-01-02",
            cash=1000.0,
            commission_rate=0.0,
            slippage_bps=0.0,
        )
        result = BacktestEngine(cfg).run_multi(NoopStrategy(), feeds)

        self.assertEqual(len(result["equity_curve"]), 1)
        self.assertEqual(result["equity_curve"][0]["datetime"], "2024-01-02 09:30:00")

    def test_annualized_return_uses_total_return_and_elapsed_time(self):
        class BuyOnceStrategy(Strategy):
            def __init__(self):
                self.done = False

            def next(self, bar):
                if self.done:
                    return None
                self.done = True
                return {"action": "BUY", "type": "market", "size": 100.0}

        bars = [
            {
                "datetime": "2024-01-01",
                "open": 10.0,
                "high": 10.0,
                "low": 10.0,
                "close": 10.0,
                "volume": 1000.0,
                "symbol": "TEST",
            },
            {
                "datetime": "2024-07-01",
                "open": 14.0,
                "high": 14.0,
                "low": 14.0,
                "close": 14.0,
                "volume": 1000.0,
                "symbol": "TEST",
            },
        ]
        cfg = BacktestConfig(
            start="2024-01-01",
            end="2024-07-01",
            cash=1000.0,
            commission_rate=0.0,
            slippage_bps=0.0,
        )
        result = BacktestEngine(cfg).run(BuyOnceStrategy(), bars)
        expected = (1400.0 / 1000.0) ** (365.25 / 182.0) - 1.0

        self.assertAlmostEqual(result["stats"]["total_return"], 0.4)
        self.assertAlmostEqual(result["stats"]["annualized_return"], expected)
        self.assertGreater(result["stats"]["annualized_return"], 0.0)

    def test_stats_total_pnl_uses_equity_and_trade_stats_use_realized_pnl(self):
        class RoundTripStrategy(Strategy):
            def __init__(self):
                self.index = 0

            def next(self, bar):
                self.index += 1
                if self.index == 1:
                    return {"action": "BUY", "type": "market", "size": 10.0}
                if self.index == 2:
                    return {"action": "SELL", "type": "market", "size": 10.0}
                return None

        bars = [
            {
                "datetime": "2024-01-01",
                "open": 10.0,
                "high": 10.0,
                "low": 10.0,
                "close": 10.0,
                "volume": 1000.0,
                "symbol": "TEST",
            },
            {
                "datetime": "2024-01-02",
                "open": 15.0,
                "high": 15.0,
                "low": 15.0,
                "close": 15.0,
                "volume": 1000.0,
                "symbol": "TEST",
            },
        ]
        cfg = BacktestConfig(
            start="2024-01-01",
            end="2024-01-02",
            cash=1000.0,
            commission_rate=0.0,
            slippage_bps=0.0,
        )
        result = BacktestEngine(cfg).run(RoundTripStrategy(), bars)
        stats = result["stats"]

        self.assertAlmostEqual(stats["total_pnl"], 50.0)
        self.assertAlmostEqual(stats["realized_pnl"], 50.0)
        self.assertAlmostEqual(stats["unrealized_pnl"], 0.0)
        self.assertEqual(stats["total_trades"], 2)
        self.assertEqual(stats["closed_trades"], 1)
        self.assertEqual(stats["winning_trades"], 1)
        self.assertEqual(stats["losing_trades"], 0)
        self.assertAlmostEqual(stats["win_rate"], 1.0)
        self.assertAlmostEqual(result["trades"][1]["realized_pnl"], 50.0)


if __name__ == "__main__":
    unittest.main()
