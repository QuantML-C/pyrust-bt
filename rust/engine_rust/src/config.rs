// 回测配置与日期范围解析

use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime};
use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;

use crate::model::BarData;

#[pyclass]
#[derive(Clone)]
pub struct BacktestConfig {
    #[pyo3(get)]
    pub start: String,
    #[pyo3(get)]
    pub end: String,
    #[pyo3(get)]
    pub cash: f64,
    #[pyo3(get)]
    pub commission_rate: f64,
    #[pyo3(get)]
    pub slippage_bps: f64,
    /// DEPRECATED：批处理已移除，该字段不再影响执行，仅为向后兼容保留。
    #[pyo3(get)]
    pub batch_size: usize,
    #[pyo3(get)]
    pub allow_short: bool,
    #[pyo3(get)]
    pub reject_on_insufficient_cash: bool,
    #[pyo3(get)]
    pub max_leverage: Option<f64>,
}

#[pymethods]
impl BacktestConfig {
    #[new]
    #[pyo3(signature = (start, end, cash, commission_rate=0.0, slippage_bps=0.0, batch_size=1000, allow_short=true, reject_on_insufficient_cash=false, max_leverage=None))]
    fn new(
        start: String,
        end: String,
        cash: f64,
        commission_rate: f64,
        slippage_bps: f64,
        batch_size: usize,
        allow_short: bool,
        reject_on_insufficient_cash: bool,
        max_leverage: Option<f64>,
    ) -> Self {
        Self {
            start,
            end,
            cash,
            commission_rate,
            slippage_bps,
            batch_size,
            allow_short,
            reject_on_insufficient_cash,
            max_leverage,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct DateRange {
    pub start: Option<NaiveDateTime>,
    pub end: Option<NaiveDateTime>,
}

impl DateRange {
    pub(crate) fn from_config(cfg: &BacktestConfig) -> PyResult<Self> {
        Ok(Self {
            start: parse_config_datetime(&cfg.start, false, "start")?,
            end: parse_config_datetime(&cfg.end, true, "end")?,
        })
    }

    pub(crate) fn is_active(&self) -> bool {
        self.start.is_some() || self.end.is_some()
    }

    pub(crate) fn contains_bar(&self, bar: &BarData) -> bool {
        if !self.is_active() {
            return true;
        }

        let dt = bar.parsed_dt;
        if let Some(start) = self.start {
            if dt < start {
                return false;
            }
        }
        if let Some(end) = self.end {
            if dt > end {
                return false;
            }
        }
        true
    }
}

fn parse_config_datetime(
    value: &str,
    date_only_is_end: bool,
    field_name: &str,
) -> PyResult<Option<NaiveDateTime>> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    parse_datetime_value(trimmed, date_only_is_end)
        .map(Some)
        .ok_or_else(|| {
            PyValueError::new_err(format!(
                "invalid BacktestConfig {field_name} datetime value: {trimmed}"
            ))
        })
}

pub(crate) fn parse_datetime_value(value: &str, date_only_is_end: bool) -> Option<NaiveDateTime> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.naive_utc());
    }

    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(dt) = NaiveDateTime::parse_from_str(trimmed, fmt) {
            return Some(dt);
        }
    }

    if let Ok(date) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        let time = if date_only_is_end {
            NaiveTime::from_hms_nano_opt(23, 59, 59, 999_999_999)?
        } else {
            NaiveTime::from_hms_opt(0, 0, 0)?
        };
        return Some(date.and_time(time));
    }

    None
}

pub(crate) fn filter_bars_by_date_range(
    bars_data: Vec<BarData>,
    date_range: &DateRange,
) -> PyResult<Vec<BarData>> {
    if !date_range.is_active() {
        return Ok(bars_data);
    }

    let mut filtered = Vec::with_capacity(bars_data.len());
    for bar in bars_data {
        if date_range.contains_bar(&bar) {
            filtered.push(bar);
        }
    }
    Ok(filtered)
}
