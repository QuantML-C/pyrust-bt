// 回测结果构建与统计指标

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

use crate::config::parse_datetime_value;
use crate::model::{OrderRecord, PositionState, TradeRecord};

pub(crate) fn equity_curve_elapsed_years(equity_curve: &[(Option<String>, f64)]) -> Option<f64> {
    let start_text = equity_curve.first()?.0.as_deref()?;
    let end_text = equity_curve.last()?.0.as_deref()?;
    let start = parse_datetime_value(start_text, false)?;
    let end = parse_datetime_value(end_text, false)?;
    let seconds = (end - start).num_seconds();
    if seconds <= 0 {
        return None;
    }
    Some(seconds as f64 / (365.25 * 24.0 * 60.0 * 60.0))
}

/// 统一的结果 dict 构建（单资产 run 与多资产 run_multi 共用）。
pub(crate) fn build_result<'py>(
    py: Python<'py>,
    pos: PositionState,
    equity_curve: Vec<(Option<String>, f64)>,
    trades: Vec<TradeRecord>,
    orders: Vec<OrderRecord>,
) -> PyResult<PyObject> {
    let result = PyDict::new_bound(py);
    result.set_item("cash", pos.cash)?;
    result.set_item("position", pos.position)?;
    result.set_item("avg_cost", pos.avg_cost)?;
    let final_equity = equity_curve.last().map_or(pos.cash, |(_, eq)| *eq);
    result.set_item("equity", final_equity)?;
    result.set_item("realized_pnl", pos.realized_pnl)?;

    // 高效构建净值曲线
    let eq_list = PyList::empty_bound(py);
    for (dt, eq) in &equity_curve {
        let row = PyDict::new_bound(py);
        if let Some(d) = dt {
            row.set_item("datetime", d)?;
        } else {
            row.set_item("datetime", py.None())?;
        }
        row.set_item("equity", eq)?;
        eq_list.append(row)?;
    }
    result.set_item("equity_curve", eq_list)?;

    // 高效构建交易列表
    let tr_list = PyList::empty_bound(py);
    for trade in &trades {
        let t = PyDict::new_bound(py);
        t.set_item("order_id", trade.order_id)?;
        t.set_item("side", &trade.side)?;
        t.set_item("price", trade.price)?;
        t.set_item("size", trade.size)?;
        t.set_item("symbol", &trade.symbol)?;
        if let Some(dt) = &trade.datetime {
            t.set_item("datetime", dt)?;
        } else {
            t.set_item("datetime", py.None())?;
        }
        t.set_item("commission", trade.commission)?;
        t.set_item("realized_pnl", trade.realized_pnl)?;
        tr_list.append(t)?;
    }
    result.set_item("trades", tr_list)?;

    let order_list = PyList::empty_bound(py);
    for order in &orders {
        let row = PyDict::new_bound(py);
        row.set_item("order_id", order.order_id)?;
        row.set_item("symbol", &order.symbol)?;
        row.set_item("side", &order.side)?;
        row.set_item("type", &order.otype)?;
        row.set_item("size", order.size)?;
        if let Some(limit_price) = order.limit_price {
            row.set_item("limit_price", limit_price)?;
        } else {
            row.set_item("limit_price", py.None())?;
        }
        row.set_item("status", &order.status)?;
        if let Some(dt) = &order.datetime {
            row.set_item("datetime", dt)?;
        } else {
            row.set_item("datetime", py.None())?;
        }
        if let Some(filled_price) = order.filled_price {
            row.set_item("filled_price", filled_price)?;
        } else {
            row.set_item("filled_price", py.None())?;
        }
        row.set_item("filled_size", order.filled_size)?;
        row.set_item("commission", order.commission)?;
        row.set_item("realized_pnl", order.realized_pnl)?;
        if let Some(reason) = &order.reject_reason {
            row.set_item("reject_reason", reason)?;
        } else {
            row.set_item("reject_reason", py.None())?;
        }
        order_list.append(row)?;
    }
    result.set_item("orders", order_list)?;

    // 增强的统计分析
    let stats = compute_enhanced_stats(py, &equity_curve, &trades, &orders, pos.realized_pnl)?;
    result.set_item("stats", stats)?;

    Ok(result.into())
}

pub(crate) fn compute_enhanced_stats<'py>(
    py: Python<'py>,
    equity_curve: &[(Option<String>, f64)],
    trades: &[TradeRecord],
    orders: &[OrderRecord],
    realized_pnl: f64,
) -> PyResult<PyObject> {
    if equity_curve.is_empty() {
        return Ok(PyDict::new_bound(py).into());
    }

    let start_equity = equity_curve.first().unwrap().1;
    let end_equity = equity_curve.last().unwrap().1;
    let total_return = if start_equity != 0.0 {
        (end_equity / start_equity) - 1.0
    } else {
        0.0
    };
    let total_pnl = end_equity - start_equity;
    let unrealized_pnl = total_pnl - realized_pnl;

    // 向量化收益率计算
    let mut returns: Vec<f64> = Vec::with_capacity(equity_curve.len().saturating_sub(1));
    for i in 1..equity_curve.len() {
        let prev = equity_curve[i - 1].1;
        let curr = equity_curve[i].1;
        if prev.abs() > f64::EPSILON {
            returns.push((curr - prev) / prev.abs());
        }
    }

    let mean_return = if returns.is_empty() {
        0.0
    } else {
        returns.iter().sum::<f64>() / returns.len() as f64
    };
    let var = if returns.len() > 1 {
        let sum_sq_diff: f64 = returns.iter().map(|r| (r - mean_return).powi(2)).sum();
        sum_sq_diff / (returns.len() - 1) as f64
    } else {
        0.0
    };
    let std = var.sqrt();
    let elapsed_years = equity_curve_elapsed_years(equity_curve);
    let periods_per_year = elapsed_years
        .filter(|years| *years > 0.0)
        .map(|years| returns.len() as f64 / years)
        .filter(|factor| factor.is_finite() && *factor > 0.0)
        .unwrap_or(252.0);
    let annualized_return = if let Some(years) = elapsed_years {
        if start_equity > 0.0 && end_equity > 0.0 {
            (end_equity / start_equity).powf(1.0 / years) - 1.0
        } else {
            total_return / years
        }
    } else if returns.is_empty() {
        total_return
    } else {
        mean_return * periods_per_year
    };
    let annualized_volatility = std * periods_per_year.sqrt();
    let sharpe = if annualized_volatility > 0.0 {
        annualized_return / annualized_volatility
    } else {
        0.0
    };

    // 高效最大回撤计算
    let mut peak = start_equity;
    let mut max_dd: f64 = 0.0;
    let mut dd_duration = 0;
    let mut max_dd_duration = 0;

    for &(_, eq) in equity_curve {
        if eq > peak {
            peak = eq;
            dd_duration = 0;
        } else {
            dd_duration += 1;
            let current_dd = 1.0 - eq / peak;
            if current_dd > max_dd {
                max_dd = current_dd;
            }
            if dd_duration > max_dd_duration {
                max_dd_duration = dd_duration;
            }
        }
    }

    // 交易统计
    let total_trades = trades.len();
    let commission_total: f64 = trades.iter().map(|trade| trade.commission).sum();
    let (closed_trades, winning_trades, losing_trades, gross_profit, gross_loss) = {
        let mut closed = 0;
        let mut win = 0;
        let mut lose = 0;
        let mut profit = 0.0;
        let mut loss = 0.0;

        for trade in trades {
            if trade.realized_pnl.abs() <= f64::EPSILON {
                continue;
            }
            closed += 1;
            if trade.realized_pnl > 0.0 {
                win += 1;
                profit += trade.realized_pnl;
            } else {
                lose += 1;
                loss += -trade.realized_pnl;
            }
        }
        (closed, win, lose, profit, loss)
    };

    let win_rate = if closed_trades > 0 {
        winning_trades as f64 / closed_trades as f64
    } else {
        0.0
    };
    let avg_win = if winning_trades > 0 {
        gross_profit / winning_trades as f64
    } else {
        0.0
    };
    let avg_loss = if losing_trades > 0 {
        gross_loss / losing_trades as f64
    } else {
        0.0
    };
    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };
    let filled_orders = orders
        .iter()
        .filter(|order| order.status == "filled")
        .count();
    let rejected_orders = orders
        .iter()
        .filter(|order| order.status == "rejected")
        .count();
    let unfilled_orders = orders
        .iter()
        .filter(|order| order.status == "unfilled")
        .count();
    let calmar = if max_dd > 0.0 {
        annualized_return / max_dd
    } else {
        0.0
    };

    let stats = PyDict::new_bound(py);
    stats.set_item("start_equity", start_equity)?;
    stats.set_item("end_equity", end_equity)?;
    stats.set_item("total_return", total_return)?;
    stats.set_item("annualized_return", annualized_return)?;
    stats.set_item("volatility", annualized_volatility)?;
    stats.set_item("sharpe", sharpe)?;
    stats.set_item("calmar", calmar)?;
    stats.set_item("max_drawdown", max_dd)?;
    stats.set_item("max_dd_duration", max_dd_duration)?;
    stats.set_item("total_trades", total_trades)?;
    stats.set_item("closed_trades", closed_trades)?;
    stats.set_item("winning_trades", winning_trades)?;
    stats.set_item("losing_trades", losing_trades)?;
    stats.set_item("win_rate", win_rate)?;
    stats.set_item("total_pnl", total_pnl)?;
    stats.set_item("realized_pnl", realized_pnl)?;
    stats.set_item("unrealized_pnl", unrealized_pnl)?;
    stats.set_item("commission_total", commission_total)?;
    stats.set_item("gross_profit", gross_profit)?;
    stats.set_item("gross_loss", gross_loss)?;
    stats.set_item("profit_factor", profit_factor)?;
    stats.set_item("avg_win", avg_win)?;
    stats.set_item("avg_loss", avg_loss)?;
    stats.set_item("total_orders", orders.len())?;
    stats.set_item("filled_orders", filled_orders)?;
    stats.set_item("rejected_orders", rejected_orders)?;
    stats.set_item("unfilled_orders", unfilled_orders)?;

    Ok(stats.into())
}
