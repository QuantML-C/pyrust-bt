# Strategy Guide

Strategies are Python classes that inherit from `pyrust_bt.strategy.Strategy`. The Rust engine calls them while it advances through bars.

## Lifecycle

```text
on_start(ctx)
  -> next(bar) or next(bar, ctx)
  -> on_order(event)
  -> on_trade(event)
  -> on_stop()
```

For multi-asset backtests, the engine prefers:

```python
next_multi(update_slice, ctx)
```

and falls back to `next()` when `next_multi()` is not available.

## Single-Asset Strategy

```python
from pyrust_bt.strategy import Strategy


class SMAStrategy(Strategy):
    def __init__(self, window=5, size=1.0):
        self.window = window
        self.size = size
        self.closes = []

    def next(self, bar):
        close = float(bar["close"])
        self.closes.append(close)

        if len(self.closes) < self.window:
            return None

        sma = sum(self.closes[-self.window:]) / self.window

        if close > sma:
            return {"action": "BUY", "type": "market", "size": self.size}
        if close < sma:
            return {"action": "SELL", "type": "market", "size": self.size}
        return None
```

## Context-Aware Strategy

The Rust engine first tries `next(bar, ctx)`. If that call fails because your strategy only accepts one argument, it falls back to `next(bar)`.

```python
class CashAwareStrategy(Strategy):
    def next(self, bar, ctx):
        if ctx.cash <= 0:
            return None

        if bar["close"] > bar["open"]:
            return {"action": "BUY", "type": "market", "size": 1.0}
        return None
```

Single-asset `ctx` exposes:

| Field | Meaning |
|---|---|
| `position` | Current position |
| `avg_cost` | Average cost |
| `cash` | Current cash |
| `equity` | Current equity |
| `bar_index` | Zero-based bar index |

## Action Format

String actions are shorthand market orders:

```python
return "BUY"
return "SELL"
```

Dictionary actions are explicit orders:

```python
return {
    "action": "BUY",
    "type": "market",
    "size": 10.0,
}
```

```python
return {
    "action": "SELL",
    "type": "limit",
    "size": 10.0,
    "price": 105.0,
}
```

Supported fields:

| Field | Required | Meaning |
|---|---:|---|
| `action` | yes | `BUY` or `SELL` |
| `type` | no | `market` or `limit`, defaults to `market` |
| `size` | no | Order size, defaults to `1.0` |
| `price` | for limit | Limit price |
| `symbol` | multi-asset | Target symbol |

## Order and Trade Callbacks

Use callbacks for logging, diagnostics, or external state.

```python
class LoggingStrategy(Strategy):
    def on_order(self, event):
        print("order:", event)

    def on_trade(self, event):
        print("trade:", event)
```

Order events include submission and fill notifications. Trade events include `order_id`, `side`, `price`, `size`, and `symbol` when available.

## Multi-Asset Strategy

`run_multi()` receives feeds as a dictionary:

```python
feeds = {
    "SPY": spy_bars,
    "QQQ": qqq_bars,
}
```

Implement `next_multi()` to trade multiple symbols at a shared timeline step:

```python
class EqualWeightStrategy(Strategy):
    def __init__(self, symbols):
        self.symbols = symbols
        self.rebalanced = False

    def next_multi(self, update_slice, ctx):
        if self.rebalanced:
            return None

        equity = float(ctx["equity"])
        target_value = equity / len(self.symbols)
        actions = []

        for symbol in self.symbols:
            bar = update_slice.get(symbol)
            if not bar:
                continue
            price = float(bar["close"])
            size = target_value / price
            actions.append({
                "action": "BUY",
                "type": "market",
                "size": size,
                "symbol": symbol,
            })

        self.rebalanced = True
        return actions
```

Multi-asset `ctx` includes:

| Field | Meaning |
|---|---|
| `positions` | Per-symbol position and average cost |
| `cash` | Portfolio cash |
| `equity` | Portfolio equity |
| `bar_index` | Timeline step |
| `last_prices` | Latest known price by symbol |

## Matching Model

The current matching model is simple by design:

- Market orders fill at the current bar close.
- Limit buys fill when current price is less than or equal to the limit.
- Limit sells fill when current price is greater than or equal to the limit.
- Slippage is applied by side.
- Commission is applied to executed notional.

This is suitable for fast research and strategy prototyping. It is not a market microstructure simulator.
