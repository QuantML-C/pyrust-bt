// 向量化指标计算（优化版）

use pyo3::prelude::*;

pub fn vectorized_sma(prices: &[f64], window: usize) -> Vec<Option<f64>> {
    if prices.is_empty() || window == 0 {
        return vec![None; prices.len()];
    }

    let mut result = Vec::with_capacity(prices.len());
    let mut sum = 0.0;

    for i in 0..prices.len() {
        sum += prices[i];
        if i >= window {
            sum -= prices[i - window];
        }

        if i + 1 >= window {
            result.push(Some(sum / window as f64));
        } else {
            result.push(None);
        }
    }
    result
}

pub fn vectorized_rsi(prices: &[f64], window: usize) -> Vec<Option<f64>> {
    if prices.len() < 2 || window == 0 {
        return vec![None; prices.len()];
    }

    let mut result = Vec::with_capacity(prices.len());
    result.push(None); // 第一个价格没有变化

    let mut gains = Vec::with_capacity(prices.len());
    let mut losses = Vec::with_capacity(prices.len());

    // 计算价格变化
    for i in 1..prices.len() {
        let change = prices[i] - prices[i - 1];
        if change > 0.0 {
            gains.push(change);
            losses.push(0.0);
        } else {
            gains.push(0.0);
            losses.push(-change);
        }
    }

    // 计算RSI
    let mut avg_gain = 0.0;
    let mut avg_loss = 0.0;

    for i in 0..gains.len() {
        if i < window - 1 {
            result.push(None);
        } else if i == window - 1 {
            // 初始平均
            avg_gain = gains[0..window].iter().sum::<f64>() / window as f64;
            avg_loss = losses[0..window].iter().sum::<f64>() / window as f64;

            let rsi = if avg_loss == 0.0 {
                100.0
            } else {
                100.0 - (100.0 / (1.0 + avg_gain / avg_loss))
            };
            result.push(Some(rsi));
        } else {
            // Wilder的平滑方法
            avg_gain = ((avg_gain * (window - 1) as f64) + gains[i]) / window as f64;
            avg_loss = ((avg_loss * (window - 1) as f64) + losses[i]) / window as f64;

            let rsi = if avg_loss == 0.0 {
                100.0
            } else {
                100.0 - (100.0 / (1.0 + avg_gain / avg_loss))
            };
            result.push(Some(rsi));
        }
    }

    result
}

#[pyfunction]
pub fn compute_sma(prices: Vec<f64>, window: usize) -> Vec<Option<f64>> {
    vectorized_sma(&prices, window)
}

#[pyfunction]
pub fn compute_rsi(prices: Vec<f64>, window: usize) -> Vec<Option<f64>> {
    vectorized_rsi(&prices, window)
}
