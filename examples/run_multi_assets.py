from __future__ import annotations
from typing import Any, Dict, List
import os
import sys
import csv

# Allow running from repo root
sys.path.append(os.path.join(os.path.dirname(__file__), "..", "python"))

from pyrust_bt.api import BacktestEngine, BacktestConfig
from pyrust_bt.strategy import Strategy


def load_multi_csv(path: str) -> Dict[str, List[Dict[str, Any]]]:
    feeds: Dict[str, List[Dict[str, Any]]] = {}
    with open(path, "r", newline="", encoding="utf-8") as f:
        reader = csv.DictReader(f)
        for row in reader:
            sym = row["symbol"]
            feeds.setdefault(sym, []).append(
                {
                    "symbol": sym,
                    "datetime": row["datetime"],
                    "open": float(row["open"]) if row.get("open") else 0.0,
                    "high": float(row["high"]) if row.get("high") else 0.0,
                    "low": float(row["low"]) if row.get("low") else 0.0,
                    "close": float(row["close"]) if row.get("close") else 0.0,
                    "volume": float(row["volume"]) if row.get("volume") else 0.0,
                }
            )
    # Ensure sorted by datetime per feed
    for k in feeds:
        feeds[k].sort(key=lambda x: x["datetime"])  # naive string sort ok for this sample
    return feeds


class MultiAssetSMAStrategy(Strategy):
    def __init__(self, size: float = 1.0) -> None:
        self.size = size
        self.last_price: Dict[str, float] = {}

    def next_multi(self, update_slice: Dict[str, Dict[str, Any]], ctx: Dict[str, Any]) -> List[Dict[str, Any]] | None:
        actions: List[Dict[str, Any]] = []
        for feed_id, bar in update_slice.items():
            sym = bar.get("symbol", feed_id)
            price = float(bar["close"])  # type: ignore[assignment]
            last = self.last_price.get(sym)
            if last is not None:
                # naive momentum: if price up vs last, buy; if down, sell
                if price > last:
                    actions.append({"action": "BUY", "type": "market", "size": self.size, "symbol": sym})
                elif price < last:
                    actions.append({"action": "SELL", "type": "market", "size": self.size, "symbol": sym})
            self.last_price[sym] = price
        return actions or None


def main() -> None:
    cfg = BacktestConfig(
        start="2022-01-01",
        end="2022-07-31",
        cash=10000.0,
        commission_rate=0.,
        slippage_bps=0.,
        batch_size=1000,
    )
    engine = BacktestEngine(cfg)

    data_path = os.path.join(os.path.dirname(__file__), "data", "2022.csv")
    feeds = load_multi_csv(data_path)

    strategy = MultiAssetSMAStrategy(size=1.0)
    result = engine.run_multi(strategy, feeds)

    print("Equity:", result["equity"])
    print("Stats:", result.get("stats"))


if __name__ == "__main__":
    main() 
