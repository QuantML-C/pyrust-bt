from __future__ import annotations
from typing import Any, Dict, List

import os
import sys

# Allow running from repo root
sys.path.append(os.path.join(os.path.dirname(__file__), "..", "python"))

from pyrust_bt.api import BacktestEngine, BacktestConfig
from pyrust_bt.strategy import Strategy
from pyrust_bt.data import load_csv_to_bars


class DummySMAStrategy(Strategy):
    def __init__(self, window: int = 5, size: float = 1.0) -> None:
        self.window = window
        self.size = size
        self._closes: List[float] = []

    def next(self, bar: Dict[str, Any]) -> str | Dict[str, Any] | None:
        close = float(bar["close"])  # type: ignore[assignment]
        self._closes.append(close)
        if len(self._closes) < self.window:
            return None
        sma = sum(self._closes[-self.window :]) / self.window
        if close > sma:
            # Market buy order
            return {"action": "BUY", "type": "market", "size": self.size}
        elif close < sma:
            # Limit sell order (using current price as limit, will execute immediately in this example)
            return {"action": "SELL", "type": "limit", "size": self.size, "price": close}
        return None

    def on_order(self, event: Dict[str, Any]) -> None:
        # print("on_order:", event)
        pass

    def on_trade(self, event: Dict[str, Any]) -> None:
        # print("on_trade:", event)
        pass

def main() -> None:
    # Prepare config with commission and slippage
    cfg = BacktestConfig(
        start="2016-01-01",
        end="2025-12-31",
        cash=10000.0,
        commission_rate=0.0005,  # 5 bps commission
        slippage_bps=2.0,        # 2 bps slippage
    )
    engine = BacktestEngine(cfg)

    # Prepare data (expects a CSV at examples/data/sample.csv)
    data_path = os.path.join(os.path.dirname(__file__), "data", "sh600000_min.csv")
    if not os.path.exists(data_path):
        print(f"Sample data not found: {data_path}\nPlease place a CSV with columns: datetime,open,high,low,close,volume")
        sys.exit(1)

    bars = load_csv_to_bars(data_path, symbol="SAMPLE")

    # Run
    strategy = DummySMAStrategy(window=60, size=1.0)
    result = engine.run(strategy, bars)

    # Format output for readability
    print("\n" + "=" * 60)
    print("Backtest Results")
    print("=" * 60)
    
    # Account information
    print("\n[Account Information]")
    print(f"  Cash Balance:      {result.get('cash', 0):>15,.2f}")
    print(f"  Position:          {result.get('position', 0):>15,.2f}")
    print(f"  Average Cost:      {result.get('avg_cost', 0):>15,.4f}")
    print(f"  Total Equity:      {result.get('equity', 0):>15,.2f}")
    print(f"  Realized P&L:      {result.get('realized_pnl', 0):>15,.2f}")
    
    # Statistics
    stats = result.get("stats", {})
    if stats:
        print("\n[Return Metrics]")
        print(f"  Start Equity:      {stats.get('start_equity', 0):>15,.2f}")
        print(f"  End Equity:        {stats.get('end_equity', 0):>15,.2f}")
        print(f"  Total Return:      {stats.get('total_return', 0):>15,.2%}")
        print(f"  Annualized Return: {stats.get('annualized_return', 0):>15,.2%}")
        
        print("\n[Risk Metrics]")
        print(f"  Volatility:        {stats.get('volatility', 0):>15,.4f}")
        print(f"  Sharpe Ratio:      {stats.get('sharpe', 0):>15,.4f}")
        print(f"  Calmar Ratio:      {stats.get('calmar', 0):>15,.4f}")
        print(f"  Max Drawdown:      {stats.get('max_drawdown', 0):>15,.4f}")
        print(f"  Max DD Duration:   {stats.get('max_dd_duration', 0):>15,} bars")
        
        print("\n[Trade Statistics]")
        print(f"  Total Trades:      {stats.get('total_trades', 0):>15,}")
        print(f"  Winning Trades:    {stats.get('winning_trades', 0):>15,}")
        print(f"  Losing Trades:     {stats.get('losing_trades', 0):>15,}")
        print(f"  Win Rate:          {stats.get('win_rate', 0):>15,.2%}")
        print(f"  Total P&L:         {stats.get('total_pnl', 0):>15,.2f}")
    
    print("\n" + "=" * 60 + "\n")


if __name__ == "__main__":
    main() 