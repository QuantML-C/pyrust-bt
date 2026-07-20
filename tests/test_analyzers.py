from __future__ import annotations

import os
import sys
import unittest
from unittest import mock


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYTHON_DIR = os.path.join(ROOT, "python")
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)

from pyrust_bt import analyzers
from pyrust_bt.analyzers import round_trips_from_trades


def trade(side, price, size, dt=None, symbol=None):
    t = {"side": side, "price": float(price), "size": float(size)}
    if dt is not None:
        t["datetime"] = dt
    if symbol is not None:
        t["symbol"] = symbol
    return t


class RoundTripsTests(unittest.TestCase):
    def test_single_asset_full_round_trip(self):
        trades = [
            trade("BUY", 10.0, 2.0, dt="2024-01-01 09:30:00", symbol="TEST"),
            trade("SELL", 12.0, 2.0, dt="2024-01-02 09:30:00", symbol="TEST"),
        ]
        rts = round_trips_from_trades(trades)

        self.assertEqual(len(rts), 1)
        rt = rts[0]
        self.assertEqual(rt.side, "LONG")
        self.assertAlmostEqual(rt.entry_price, 10.0)
        self.assertAlmostEqual(rt.exit_price, 12.0)
        self.assertAlmostEqual(rt.size, 2.0)
        self.assertAlmostEqual(rt.pnl, 4.0)
        self.assertAlmostEqual(rt.return_ratio, 0.2)
        self.assertEqual(rt.entry_datetime, "2024-01-01 09:30:00")
        self.assertEqual(rt.exit_datetime, "2024-01-02 09:30:00")

    def test_partial_close_produces_two_round_trips_and_keeps_remainder(self):
        trades = [
            trade("BUY", 10.0, 10.0, dt="2024-01-01", symbol="TEST"),
            trade("SELL", 12.0, 4.0, dt="2024-01-02", symbol="TEST"),
            trade("SELL", 13.0, 6.0, dt="2024-01-03", symbol="TEST"),
        ]
        rts = round_trips_from_trades(trades)

        self.assertEqual(len(rts), 2)
        # 第一笔部分平仓 4 股，剩余 6 股继续配对
        self.assertAlmostEqual(rts[0].size, 4.0)
        self.assertAlmostEqual(rts[0].pnl, (12.0 - 10.0) * 4.0)
        self.assertEqual(rts[0].exit_datetime, "2024-01-02")
        # 余量 6 股按同一入场价继续配对
        self.assertAlmostEqual(rts[1].size, 6.0)
        self.assertAlmostEqual(rts[1].entry_price, 10.0)
        self.assertAlmostEqual(rts[1].pnl, (13.0 - 10.0) * 6.0)
        self.assertEqual(rts[1].exit_datetime, "2024-01-03")

    def test_oversized_exit_flips_into_opposite_position(self):
        trades = [
            trade("BUY", 10.0, 4.0, dt="2024-01-01", symbol="TEST"),
            trade("SELL", 12.0, 6.0, dt="2024-01-02", symbol="TEST"),
            trade("BUY", 11.0, 2.0, dt="2024-01-03", symbol="TEST"),
        ]
        rts = round_trips_from_trades(trades)

        self.assertEqual(len(rts), 2)
        self.assertEqual(rts[0].side, "LONG")
        self.assertAlmostEqual(rts[0].size, 4.0)
        self.assertAlmostEqual(rts[0].pnl, (12.0 - 10.0) * 4.0)
        # 超出部分 2 股反向开空，之后被 BUY 平掉
        self.assertEqual(rts[1].side, "SHORT")
        self.assertAlmostEqual(rts[1].size, 2.0)
        self.assertAlmostEqual(rts[1].entry_price, 12.0)
        self.assertAlmostEqual(rts[1].exit_price, 11.0)
        self.assertAlmostEqual(rts[1].pnl, (12.0 - 11.0) * 2.0)

    def test_multi_asset_interleaved_trades_pair_per_symbol(self):
        trades = [
            trade("BUY", 10.0, 1.0, dt="2024-01-01 09:30:00", symbol="AAA"),
            trade("BUY", 20.0, 1.0, dt="2024-01-01 09:30:00", symbol="BBB"),
            trade("SELL", 12.0, 1.0, dt="2024-01-02 09:30:00", symbol="AAA"),
            trade("SELL", 18.0, 1.0, dt="2024-01-02 09:30:00", symbol="BBB"),
        ]
        rts = round_trips_from_trades(trades)

        self.assertEqual(len(rts), 2)
        # AAA 多头盈利，BBB 多头亏损；若跨 symbol 错配结果会完全不同
        self.assertAlmostEqual(rts[0].pnl, 2.0)
        self.assertAlmostEqual(rts[0].entry_price, 10.0)
        self.assertAlmostEqual(rts[0].exit_price, 12.0)
        self.assertAlmostEqual(rts[1].pnl, -2.0)
        self.assertAlmostEqual(rts[1].entry_price, 20.0)
        self.assertAlmostEqual(rts[1].exit_price, 18.0)

    def test_datetimes_come_from_trades_not_bars_indexing(self):
        # 旧实现用 trades 下标索引 bars，下标语义错位；现在不需要 bars
        trades = [
            trade("SELL", 15.0, 1.0, dt="2024-01-01", symbol="TEST"),
            trade("BUY", 11.0, 1.0, dt="2024-01-05", symbol="TEST"),
        ]
        rts = round_trips_from_trades(trades, bars=None)

        self.assertEqual(len(rts), 1)
        self.assertEqual(rts[0].side, "SHORT")
        self.assertAlmostEqual(rts[0].pnl, 4.0)
        self.assertEqual(rts[0].entry_datetime, "2024-01-01")
        self.assertEqual(rts[0].exit_datetime, "2024-01-05")

    def test_trades_without_symbol_group_together(self):
        trades = [
            trade("BUY", 10.0, 1.0, dt="2024-01-01"),
            trade("SELL", 11.0, 1.0, dt="2024-01-02"),
        ]
        rts = round_trips_from_trades(trades)

        self.assertEqual(len(rts), 1)
        self.assertAlmostEqual(rts[0].pnl, 1.0)


class FactorBacktestTests(unittest.TestCase):
    def test_small_dataset_uses_rust_fast_path(self):
        if analyzers._factor_backtest_fast is None:
            self.skipTest("engine_rust is not built")

        calls = []
        real = analyzers._factor_backtest_fast

        def spy(closes, factors, quantiles, forward):
            calls.append(len(closes))
            return real(closes, factors, quantiles, forward)

        # 小样本（远低于旧的 5000 行阈值）也必须走 Rust 快路径
        bars = [
            {
                "datetime": f"2024-01-01 09:{i:02d}:00",
                "close": 10.0 + i * 0.1,
                "factor": float(i),
            }
            for i in range(60)
        ]
        with mock.patch.object(analyzers, "_factor_backtest_fast", spy):
            result = analyzers.factor_backtest(bars, "factor", quantiles=5, forward=1)

        self.assertEqual(calls, [60])
        self.assertEqual(result["quantiles"], [1, 2, 3, 4, 5])
        self.assertEqual(len(result["mean_returns"]), 5)
        # 因子递增、收益递减：分位均值严格递减，IC 为负
        self.assertIsNotNone(result["ic"])
        self.assertLess(result["ic"], 0.0)
        self.assertAlmostEqual(result["monotonicity"], -1.0)
        self.assertEqual(len(result["q_bounds"]), 4)
        self.assertIn("factor_stats", result)


if __name__ == "__main__":
    unittest.main()
