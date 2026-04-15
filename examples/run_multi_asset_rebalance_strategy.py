from __future__ import annotations
from typing import Any, Dict, List, Optional

from datetime import datetime

import os
import sys

# Allow running from repo root
sys.path.append(os.path.join(os.path.dirname(__file__), "..", "python"))

from pyrust_bt.api import BacktestEngine, BacktestConfig
from pyrust_bt.strategy import Strategy
from pyrust_bt.market_data import (  # type: ignore[attr-defined]
    DataRequest,
    MarketDataConfig,
    MarketDataService,
)

class MultiAssetRebalanceStrategy(Strategy):
    """
    简单的多资产等权策略：每个月首个交易日进行再平衡。
    """

    def __init__(self, symbols: List[str], min_trade_threshold: float = 0.01) -> None:
        self.symbols = symbols
        self.target_weight = 1.0 / len(symbols) if symbols else 0.0
        self.min_trade_threshold = min_trade_threshold
        self._last_rebalance_key: Optional[tuple[int, int]] = None

    def on_start(self, ctx: Dict[str, Any]) -> None:
        print("多资产配置再平衡策略初始化完成")
        print(f"目标资产: {self.symbols}")
        print(f"等权权重: {self.target_weight:.2%}")

    def next_multi(
        self,
        update_slice: Dict[str, Dict[str, Any]],
        ctx: Dict[str, Any],
    ) -> List[Dict[str, Any]] | None:
        if not update_slice or not self.symbols:
            return None

        # 取任意一个bar的日期（多源同步）
        any_bar = next(iter(update_slice.values()))
        dt_raw = str(any_bar.get("datetime"))
        current_dt = self._parse_datetime(dt_raw)

        month_key = (current_dt.year, current_dt.month)
        if self._last_rebalance_key == month_key:
            return None

        self._last_rebalance_key = month_key

        equity: float = float(ctx.get("equity", 0.0))
        if equity <= 0:
            return None

        positions = ctx.get("positions", {})
        actions: List[Dict[str, Any]] = []

        #print(f"[{current_dt.date()}] 触发月度再平衡，总资产 {equity:,.2f}")

        target_value = equity * self.target_weight

        for symbol in self.symbols:
            bar = update_slice.get(symbol)
            if not bar:
                print(f"  警告: 缺少 {symbol} 当日行情，跳过")
                continue

            price = float(bar.get("close", 0.0))
            if price <= 0:
                print(f"  警告: {symbol} 价格无效({price})，跳过")
                continue

            target_shares = target_value / price
            current_position_info = positions.get(symbol, {})
            if isinstance(current_position_info, dict):
                current_shares = float(current_position_info.get("position", 0.0))
            else:
                current_shares = float(current_position_info or 0.0)

            diff = target_shares - current_shares
            if abs(diff) < self.min_trade_threshold:
                continue

            action = "BUY" if diff > 0 else "SELL"
            actions.append(
                {
                    "action": action,
                    "type": "market",
                    "size": abs(diff),
                    "symbol": symbol,
                }
            )
            # print(
            #     f"  {action} {symbol}: 目标 {target_shares:.4f} 手, 当前 {current_shares:.4f} 手, 调整 {diff:.4f}"
            # )

        return actions or None

    @staticmethod
    def _parse_datetime(dt_str: str) -> datetime:
        sanitized = dt_str.replace("Z", "")
        try:
            return datetime.fromisoformat(sanitized)
        except ValueError:
            for fmt in ("%Y-%m-%d %H:%M:%S", "%Y-%m-%d"):
                try:
                    return datetime.strptime(sanitized, fmt)
                except ValueError:
                    continue
        # 如果解析失败，默认返回当前日期（避免中断）
        return datetime.now()


def main() -> None:
    # Prepare config with commission and slippage
    cfg = BacktestConfig(
        start="2022-01-01",
        end="2025-12-31",
        cash=100000.0,
        commission_rate=0.0005,  # 5 bps commission
        slippage_bps=1.0,        # 2 bps slippage
    )
    engine = BacktestEngine(cfg)

    symbol_list = ["513500.SH", "159941.SZ", "518880.SH", "511090.SH", "512890.SH"]
    period = "1d"
    db_path = os.path.join(os.path.dirname(__file__), "..", "data", "backtest.db")
    xtdata_dir = os.environ.get("XTDATA_DIR", r"D:\国金证券QMT交易端\userdata_mini")

    config = MarketDataConfig(
        db_path=db_path,
        xtdata_enabled=True,
        xtdata_data_dir=xtdata_dir,
    )

    service = MarketDataService(config)

    feeds: Dict[str, List[Dict[str, Any]]] = {}

    for symbol in symbol_list:
        request = DataRequest(
            symbols=[symbol],
            period=period,
            start_time=cfg.start,
            end_time=cfg.end,
            count=-1,
        )
        try:
            bars = service.fetch_bars(request, symbol=symbol)
        except RuntimeError as exc:
            print(f"[{symbol}] Failed to fetch data via xtdata: {exc}")
            continue

        if not bars:
            print(f"[{symbol}] No data returned from xtdata/DB.")
            continue

        feeds[symbol] = bars

    if not feeds:
        print("未能获取任何标的的数据，退出。")
        return

    strategy = MultiAssetRebalanceStrategy(symbols=list(feeds.keys()))
    result = engine.run_multi(strategy, feeds)

    print("\n" + "=" * 60)
    print("投资组合回测结果")
    print("=" * 60)

    stats = result.get("stats", {})
    if stats:
        print("\n[收益指标]")
        print(f"  起始净值:          {stats.get('start_equity', 0):>15,.2f}")
        print(f"  结束净值:          {stats.get('end_equity', 0):>15,.2f}")
        print(f"  总收益:            {stats.get('total_return', 0):>15,.2%}")
        print(f"  年化收益:          {stats.get('annualized_return', 0):>15,.2%}")

        print("\n[风险指标]")
        print(f"  波动率:            {stats.get('volatility', 0):>15,.4f}")
        print(f"  夏普比:            {stats.get('sharpe', 0):>15,.4f}")
        print(f"  卡玛比:            {stats.get('calmar', 0):>15,.4f}")
        print(f"  最大回撤:          {stats.get('max_drawdown', 0):>15,.4f}")
        print(f"  最大回撤时长:      {stats.get('max_dd_duration', 0):>15,} bars")

        print("\n[交易统计]")
        print(f"  总交易数:          {stats.get('total_trades', 0):>15,}")
        print(f"  盈利笔数:          {stats.get('winning_trades', 0):>15,}")
        print(f"  亏损笔数:          {stats.get('losing_trades', 0):>15,}")
        print(f"  胜率:              {stats.get('win_rate', 0):>15,.2%}")
        realized_pnl = result.get("realized_pnl", stats.get("total_pnl", 0))
        print(f"  总盈亏:            {realized_pnl:>15,.2f}")

    print("\n" + "=" * 60 + "\n")


if __name__ == "__main__":
    main()


