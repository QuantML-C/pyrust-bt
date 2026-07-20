// 因子分层回测快速路径（Rust 实现）

use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};

#[pyfunction]
pub fn factor_backtest_fast(
    py: Python<'_>,
    closes: Vec<f64>,
    factors: Vec<f64>,
    quantiles: usize,
    forward: usize,
) -> PyResult<PyObject> {
    let n = closes.len().min(factors.len());
    if quantiles < 2 || forward == 0 || n <= forward {
        let empty = PyDict::new_bound(py);
        empty.set_item("quantiles", PyList::empty_bound(py))?;
        empty.set_item("mean_returns", PyList::empty_bound(py))?;
        empty.set_item("ic", py.None())?;
        empty.set_item("monotonicity", 0.0)?;
        empty.set_item("q_bounds", PyList::empty_bound(py))?;
        empty.set_item("factor_stats", PyDict::new_bound(py))?;
        return Ok(empty.into());
    }

    let m = n - forward;

    // Forward returns
    let mut fwd_returns: Vec<f64> = Vec::with_capacity(m);
    for i in 0..m {
        let c0 = closes[i];
        let c1 = closes[i + forward];
        let r = if c0 != 0.0 { (c1 / c0) - 1.0 } else { 0.0 };
        fwd_returns.push(r);
    }

    // Trimmed factors
    let fac_trim: Vec<f64> = factors[..m].to_vec();

    // Quantile bounds
    let mut sorted = fac_trim.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut q_bounds: Vec<f64> = Vec::with_capacity(quantiles.saturating_sub(1));
    for q in 1..quantiles {
        let idx = (sorted.len() * q) / quantiles;
        let idx = idx.min(sorted.len().saturating_sub(1));
        q_bounds.push(sorted[idx]);
    }

    // Group stats (sums & counts)
    let mut sums: Vec<f64> = vec![0.0; quantiles];
    let mut counts: Vec<usize> = vec![0; quantiles];

    for (val, ret) in fac_trim.iter().zip(fwd_returns.iter()) {
        // Find group by linear scan (quantiles is small, typically <= 10)
        let mut gi = 0usize;
        while gi < q_bounds.len() && *val > q_bounds[gi] {
            gi += 1;
        }
        sums[gi] += *ret;
        counts[gi] += 1;
    }

    // Mean returns per quantile
    let mut mean_returns: Vec<f64> = Vec::with_capacity(quantiles);
    for i in 0..quantiles {
        if counts[i] > 0 {
            mean_returns.push(sums[i] / counts[i] as f64);
        } else {
            mean_returns.push(0.0);
        }
    }

    // IC: Pearson correlation between fac_trim and fwd_returns
    let sum_f: f64 = fac_trim.iter().sum();
    let sum_r: f64 = fwd_returns.iter().sum();
    let mean_f = sum_f / m as f64;
    let mean_r = sum_r / m as f64;
    let mut cov = 0.0_f64;
    let mut var_f = 0.0_f64;
    let mut var_r = 0.0_f64;
    for i in 0..m {
        let df = fac_trim[i] - mean_f;
        let dr = fwd_returns[i] - mean_r;
        cov += df * dr;
        var_f += df * df;
        var_r += dr * dr;
    }
    let denom = (var_f * var_r).sqrt() + 1e-12;
    let ic = cov / denom;

    // Monotonicity of mean returns across quantiles
    let mut inc = 0i32;
    let mut dec = 0i32;
    if mean_returns.len() > 1 {
        for i in 1..mean_returns.len() {
            if mean_returns[i] > mean_returns[i - 1] {
                inc += 1;
            }
            if mean_returns[i] < mean_returns[i - 1] {
                dec += 1;
            }
        }
    }
    let denom_m = (mean_returns.len().saturating_sub(1)) as f64;
    let monotonicity = if denom_m > 0.0 {
        (inc - dec) as f64 / denom_m
    } else {
        0.0
    };

    // Factor stats
    let min_f = fac_trim
        .iter()
        .cloned()
        .fold(f64::INFINITY, |a, b| if b < a { b } else { a });
    let max_f = fac_trim
        .iter()
        .cloned()
        .fold(f64::NEG_INFINITY, |a, b| if b > a { b } else { a });
    let mean_f_all = mean_f;
    let std_f = if m > 1 {
        let mut vs = 0.0_f64;
        for v in fac_trim.iter() {
            let d = *v - mean_f_all;
            vs += d * d;
        }
        (vs / m as f64).sqrt()
    } else {
        0.0
    };

    // Build Python result dict
    let out = PyDict::new_bound(py);
    let q_list = PyList::empty_bound(py);
    for i in 1..=quantiles {
        q_list.append(i as i32)?;
    }
    out.set_item("quantiles", q_list)?;

    let mr_list = PyList::empty_bound(py);
    for v in mean_returns.iter() {
        mr_list.append(*v)?;
    }
    out.set_item("mean_returns", mr_list)?;

    out.set_item("ic", ic)?;
    out.set_item("monotonicity", monotonicity)?;

    let qb_list = PyList::empty_bound(py);
    for v in q_bounds.iter() {
        qb_list.append(*v)?;
    }
    out.set_item("q_bounds", qb_list)?;

    let fs = PyDict::new_bound(py);
    fs.set_item("mean", mean_f_all)?;
    fs.set_item("std", std_f)?;
    fs.set_item("min", min_f)?;
    fs.set_item("max", max_f)?;
    out.set_item("factor_stats", fs)?;

    Ok(out.into())
}
