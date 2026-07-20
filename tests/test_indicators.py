from __future__ import annotations

import os
import random
import sys
import unittest


ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
PYTHON_DIR = os.path.join(ROOT, "python")
if PYTHON_DIR not in sys.path:
    sys.path.insert(0, PYTHON_DIR)

from pyrust_bt.indicators import SMA


class SMATests(unittest.TestCase):
    def test_window_alignment(self):
        sma = SMA(3)
        values = [sma.update(p) for p in [1.0, 2.0, 3.0, 4.0, 5.0]]
        self.assertEqual(values, [None, None, 2.0, 3.0, 4.0])

    def test_rolling_sum_matches_naive_over_long_series(self):
        random.seed(7)
        data = [random.random() for _ in range(5000)]
        sma = SMA(50)
        for i, p in enumerate(data):
            value = sma.update(p)
            if i >= 49:
                expected = sum(data[i - 49 : i + 1]) / 50
                self.assertAlmostEqual(value, expected, places=9)

    def test_batch_matches_update(self):
        data = [float(i) for i in range(100)]
        sma = SMA(10)
        self.assertEqual(SMA.batch(data, 10), [sma.update(p) for p in data])


if __name__ == "__main__":
    unittest.main()
