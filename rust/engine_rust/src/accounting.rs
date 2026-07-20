// 持仓与现金记账

use crate::model::{Order, OrderSide, PositionState};

/// 更新单资产持仓状态与现金，返回本次成交产生的已实现盈亏。
pub(crate) fn update_position(
    pos: &mut PositionState,
    order: &Order,
    exec_price: f64,
    fill_size: f64,
    commission: f64,
) -> f64 {
    match order.side {
        OrderSide::Buy => {
            let cost = exec_price * fill_size + commission;
            pos.cash -= cost;
        }
        OrderSide::Sell => {
            let proceeds = exec_price * fill_size - commission;
            pos.cash += proceeds;
        }
    }
    let realized = apply_position_fill(
        &mut pos.position,
        &mut pos.avg_cost,
        order.side,
        exec_price,
        fill_size,
    );
    pos.realized_pnl += realized;
    realized
}

/// 对 (position, avg_cost) 应用一笔成交，返回已实现盈亏。
/// 支持加仓（加权平均成本）、部分/全部平仓与反手。
pub(crate) fn apply_position_fill(
    position: &mut f64,
    avg_cost: &mut f64,
    side: OrderSide,
    exec_price: f64,
    fill_size: f64,
) -> f64 {
    if fill_size <= 0.0 {
        return 0.0;
    }

    let signed_fill = match side {
        OrderSide::Buy => fill_size,
        OrderSide::Sell => -fill_size,
    };
    let old_pos = *position;

    if old_pos.abs() < f64::EPSILON {
        *position = signed_fill;
        *avg_cost = if signed_fill.abs() < f64::EPSILON {
            0.0
        } else {
            exec_price
        };
        return 0.0;
    }

    if old_pos.signum() == signed_fill.signum() {
        let old_abs = old_pos.abs();
        let new_abs = old_abs + fill_size;
        *avg_cost = ((*avg_cost * old_abs) + (exec_price * fill_size)) / new_abs;
        *position = old_pos + signed_fill;
        return 0.0;
    }

    let closing = fill_size.min(old_pos.abs());
    let realized = if old_pos > 0.0 {
        (exec_price - *avg_cost) * closing
    } else {
        (*avg_cost - exec_price) * closing
    };

    let new_pos = old_pos + signed_fill;
    if new_pos.abs() < f64::EPSILON {
        *position = 0.0;
        *avg_cost = 0.0;
    } else if new_pos.signum() == old_pos.signum() {
        *position = new_pos;
    } else {
        *position = new_pos;
        *avg_cost = exec_price;
    }

    realized
}
