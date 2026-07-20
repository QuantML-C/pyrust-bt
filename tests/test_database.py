from __future__ import annotations

import os
import sys
import tempfile
import unittest


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYTHON_DIR = os.path.join(ROOT, "python")
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)

try:
    from engine_rust import (
        get_market_data,
        load_and_synthesize_klines,
        resample_klines,
        save_klines,
    )

    IMPORT_ERROR = None
except Exception as exc:  # pragma: no cover - depends on local build state
    get_market_data = None
    load_and_synthesize_klines = None
    resample_klines = None
    save_klines = None
    IMPORT_ERROR = exc


def make_bar(dt, close, symbol="TEST"):
    return {
        "datetime": dt,
        "open": close,
        "high": close,
        "low": close,
        "close": close,
        "volume": 100.0,
        "symbol": symbol,
    }


@unittest.skipIf(IMPORT_ERROR is not None, f"engine_rust is not built: {IMPORT_ERROR}")
class DatabaseTests(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.db_path = os.path.join(self._tmp.name, "test_klines.duckdb")

    def tearDown(self):
        self._tmp.cleanup()

    def test_minute_and_monthly_periods_use_separate_tables(self):
        minute_bar = make_bar("2024-01-02 09:30:00", 10.0)
        monthly_bar = make_bar("2024-01-01 00:00:00", 99.0)

        save_klines(self.db_path, "TEST", "1m", [minute_bar], True)
        save_klines(self.db_path, "TEST", "1M", [monthly_bar], True)

        minute_rows = get_market_data(self.db_path, "TEST", "1m")
        monthly_rows = get_market_data(self.db_path, "TEST", "1M")

        # "1m"（分钟）与 "1M"（月线）必须落在不同的表，互不串数据
        self.assertEqual(len(minute_rows), 1)
        self.assertEqual(minute_rows[0]["close"], 10.0)
        self.assertEqual(len(monthly_rows), 1)
        self.assertEqual(monthly_rows[0]["close"], 99.0)

    def test_repeated_save_klines_calls_do_not_conflict_on_temp_table(self):
        bars1 = [make_bar("2024-01-02 09:30:00", 10.0)]
        bars2 = [make_bar("2024-01-02 09:31:00", 11.0)]

        save_klines(self.db_path, "TEST", "1m", bars1, True)
        # 同进程连续调用：临时表名带计数器后缀，不会撞名
        save_klines(self.db_path, "TEST", "1m", bars2, False)

        rows = get_market_data(self.db_path, "TEST", "1m")
        self.assertEqual(len(rows), 2)

    def test_resample_empty_list_returns_empty_instead_of_panicking(self):
        self.assertEqual(resample_klines([], "1h"), [])

    def test_load_and_synthesize_klines_is_registered(self):
        bars = [make_bar("2024-01-02 09:30:00", 10.0), make_bar("2024-01-02 09:31:00", 11.0)]
        save_klines(self.db_path, "TEST", "1m", bars, True)

        rows = load_and_synthesize_klines(self.db_path, "TEST", "1m")
        self.assertEqual(len(rows), 2)
        self.assertEqual(rows[0]["close"], 10.0)
        self.assertEqual(rows[1]["close"], 11.0)


if __name__ == "__main__":
    unittest.main()
