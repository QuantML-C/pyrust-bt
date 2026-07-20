// Database query and K-line synthesis module
// High-performance Rust implementation for DuckDB operations

use chrono::{DateTime, NaiveDateTime, Timelike};
use duckdb::Connection;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

// 临时表名并发去重计数器（同进程内多次/多线程 save_klines 不再撞名）
static TEMP_TABLE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn unique_temp_table_name(prefix: &str) -> String {
    let seq = TEMP_TABLE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}_{}_{}", prefix, std::process::id(), seq)
}

// K-line bar structure for internal processing
#[derive(Clone, Debug)]
pub struct KlineBar {
    pub datetime: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub symbol: String,
}

// Convert period string to minutes (case-sensitive):
// "1m" = 1 minute, "1M"/"1mo" = 1 month, "1h"/"1d"/"1w"/"1y" as usual.
fn period_to_minutes(period: &str) -> Option<i64> {
    if period.ends_with("mo") {
        period[..period.len() - 2]
            .parse::<i64>()
            .ok()
            .map(|m| m * 43200)
    } else if period.ends_with('M') {
        period[..period.len() - 1]
            .parse::<i64>()
            .ok()
            .map(|m| m * 43200)
    } else if period.ends_with('m') {
        period[..period.len() - 1].parse::<i64>().ok()
    } else if period.ends_with('h') {
        period[..period.len() - 1]
            .parse::<i64>()
            .ok()
            .map(|h| h * 60)
    } else if period.ends_with('d') {
        period[..period.len() - 1]
            .parse::<i64>()
            .ok()
            .map(|d| d * 1440)
    } else if period.ends_with('w') {
        period[..period.len() - 1]
            .parse::<i64>()
            .ok()
            .map(|w| w * 10080)
    } else if period.ends_with('y') {
        period[..period.len() - 1]
            .parse::<i64>()
            .ok()
            .map(|y| y * 525600)
    } else {
        None
    }
}

fn sanitize_period_identifier(period: &str) -> PyResult<String> {
    // 大小写敏感映射：月线 "1M" 规范化为 "1mo"（表名 klines_1mo），
    // 分钟线 "1m" 保持 "1m"（表名 klines_1m），其余（"1d"/"1w" 等）不变。
    let normalized = if period.ends_with('M') && !period.ends_with("mo") {
        format!("{}mo", &period[..period.len() - 1])
    } else {
        period.to_string()
    };
    let mut sanitized = String::with_capacity(normalized.len());
    for ch in normalized.chars() {
        if ch.is_ascii_alphanumeric() {
            sanitized.push(ch.to_ascii_lowercase());
        } else {
            sanitized.push('_');
        }
    }
    let sanitized = sanitized.trim_matches('_').to_string();
    if sanitized.is_empty() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Period must contain at least one alphanumeric character",
        ));
    }
    Ok(sanitized)
}

fn ensure_period_table(conn: &Connection, period: &str) -> PyResult<String> {
    let sanitized_period = sanitize_period_identifier(period)?;
    let table_name = format!("klines_{}", sanitized_period);

    conn.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {} (
                symbol VARCHAR NOT NULL,
                datetime TIMESTAMP NOT NULL,
                open DOUBLE NOT NULL,
                high DOUBLE NOT NULL,
                low DOUBLE NOT NULL,
                close DOUBLE NOT NULL,
                volume DOUBLE NOT NULL
            )",
            table_name
        ),
        [],
    )
    .map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to ensure table {}: {}",
            table_name, e
        ))
    })?;

    conn.execute(
        &format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_{}_symbol_datetime
                ON {} (symbol, datetime)",
            table_name, table_name
        ),
        [],
    )
    .map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to ensure index for {}: {}",
            table_name, e
        ))
    })?;

    Ok(table_name)
}

// Parse datetime string to NaiveDateTime
fn parse_datetime(dt_str: &str) -> Option<NaiveDateTime> {
    // Try ISO format first (RFC3339)
    if dt_str.contains('T') || dt_str.contains('Z') || dt_str.contains('+') {
        if let Ok(dt) = DateTime::parse_from_rfc3339(dt_str) {
            return Some(dt.naive_utc());
        }
        // Try without timezone
        if let Ok(dt) = NaiveDateTime::parse_from_str(dt_str, "%Y-%m-%dT%H:%M:%S") {
            return Some(dt);
        }
        if let Ok(dt) = NaiveDateTime::parse_from_str(dt_str, "%Y-%m-%dT%H:%M:%S%.f") {
            return Some(dt);
        }
    }

    // Try common formats
    let formats = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M:%S%.f", "%Y-%m-%d"];

    for fmt in &formats {
        if let Ok(dt) = NaiveDateTime::parse_from_str(dt_str, fmt) {
            return Some(dt);
        }
    }

    None
}

// Round down datetime to period boundary
fn round_down_to_period(dt: NaiveDateTime, minutes: i64) -> NaiveDateTime {
    if minutes >= 1440 {
        // Daily or larger: round to start of day
        dt.date().and_hms_opt(0, 0, 0).unwrap_or(dt)
    } else {
        // Intraday: round down to minute boundary
        let total_minutes = dt.hour() as i64 * 60 + dt.minute() as i64;
        let rounded_minutes = (total_minutes / minutes) * minutes;
        let new_hour = (rounded_minutes / 60) as u32;
        let new_minute = (rounded_minutes % 60) as u32;
        dt.date().and_hms_opt(new_hour, new_minute, 0).unwrap_or(dt)
    }
}

// Aggregate bars into one bar (OHLCV)
fn aggregate_bars(bars: &[KlineBar], group_time: &NaiveDateTime) -> PyResult<KlineBar> {
    if bars.is_empty() {
        return Err(PyErr::new::<pyo3::exceptions::PyValueError, _>(
            "Cannot aggregate empty bar list",
        ));
    }

    let open = bars[0].open;
    let high = bars
        .iter()
        .map(|b| b.high)
        .fold(f64::NEG_INFINITY, f64::max);
    let low = bars.iter().map(|b| b.low).fold(f64::INFINITY, f64::min);
    let close = bars[bars.len() - 1].close;
    let volume = bars.iter().map(|b| b.volume).sum();
    let symbol = bars[0].symbol.clone();
    let datetime = group_time.format("%Y-%m-%d %H:%M:%S").to_string();

    Ok(KlineBar {
        datetime,
        open,
        high,
        low,
        close,
        volume,
        symbol,
    })
}

// Resample K-line data to target period
pub fn resample_klines_rust(bars: Vec<KlineBar>, target_period: &str) -> PyResult<Vec<KlineBar>> {
    if bars.is_empty() {
        return Ok(Vec::new());
    }

    let target_minutes = period_to_minutes(target_period).ok_or_else(|| {
        PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
            "Unsupported period: {}",
            target_period
        ))
    })?;

    let mut resampled = Vec::new();
    let mut current_group = Vec::new();
    let mut current_group_time: Option<NaiveDateTime> = None;

    for bar in bars {
        let dt = parse_datetime(&bar.datetime).ok_or_else(|| {
            PyErr::new::<pyo3::exceptions::PyValueError, _>(format!(
                "Invalid datetime format: {}",
                bar.datetime
            ))
        })?;

        let group_time = round_down_to_period(dt, target_minutes);

        match current_group_time {
            None => {
                current_group.push(bar);
                current_group_time = Some(group_time);
            }
            Some(ct) => {
                if group_time != ct {
                    // Finalize previous group
                    if !current_group.is_empty() {
                        resampled.push(aggregate_bars(&current_group, &ct)?);
                    }
                    // Start new group
                    current_group = vec![bar];
                    current_group_time = Some(group_time);
                } else {
                    // Add to current group
                    current_group.push(bar);
                }
            }
        }
    }

    // Finalize last group
    if !current_group.is_empty() {
        if let Some(ct) = current_group_time {
            resampled.push(aggregate_bars(&current_group, &ct)?);
        }
    }

    Ok(resampled)
}

// Convert KlineBar to Python dict
fn kline_bar_to_pydict<'py>(py: Python<'py>, bar: &KlineBar) -> PyResult<Py<PyDict>> {
    let dict = PyDict::new(py);
    dict.set_item("datetime", &bar.datetime)?;
    dict.set_item("open", bar.open)?;
    dict.set_item("high", bar.high)?;
    dict.set_item("low", bar.low)?;
    dict.set_item("close", bar.close)?;
    dict.set_item("volume", bar.volume)?;
    dict.set_item("symbol", &bar.symbol)?;
    Ok(dict.into())
}

// Python-exposed function: Resample K-lines (high-performance Rust implementation)
#[pyfunction]
pub fn resample_klines(py: Python, bars: &PyList, target_period: String) -> PyResult<PyObject> {
    // Convert Python list of dicts to KlineBar
    let mut kline_bars = Vec::with_capacity(bars.len());
    for item in bars.iter() {
        let bar_dict: &PyDict = item.downcast()?;
        let datetime: String = bar_dict
            .get_item("datetime")?
            .and_then(|v| v.extract().ok())
            .unwrap_or_else(|| "".to_string());
        let open: f64 = bar_dict
            .get_item("open")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let high: f64 = bar_dict
            .get_item("high")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let low: f64 = bar_dict
            .get_item("low")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let close: f64 = bar_dict
            .get_item("close")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let volume: f64 = bar_dict
            .get_item("volume")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let symbol: String = bar_dict
            .get_item("symbol")?
            .and_then(|v| v.extract().ok())
            .unwrap_or_else(|| "UNKNOWN".to_string());

        kline_bars.push(KlineBar {
            datetime,
            open,
            high,
            low,
            close,
            volume,
            symbol,
        });
    }

    // Resample using Rust (high performance)
    let resampled = resample_klines_rust(kline_bars, &target_period)?;

    // Convert back to Python list
    let py_list = PyList::empty(py);
    for bar in resampled {
        let py_dict = kline_bar_to_pydict(py, &bar)?;
        py_list.append(py_dict)?;
    }

    Ok(py_list.into())
}

// ============================================================================
// Direct DuckDB Operations (High Performance - Eliminates Python Conversion)
// ============================================================================

/// Load K-line data directly from DuckDB (Rust implementation)
/// This eliminates Python query result conversion overhead
pub fn load_klines_rust(
    db_path: &str,
    symbol: &str,
    period: &str,
    start: Option<&str>,
    end: Option<&str>,
    count: i64,

) -> PyResult<Vec<KlineBar>> {

    // Connect to database
    let conn = Connection::open(Path::new(db_path)).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to connect to database: {}",
            e
        ))
    })?;

    // Ensure target table exists and retrieve its name
    let table_name = ensure_period_table(&conn, period)?;

    // Build query with parameters
    // When count > 0, ignore start parameter (query most recent N bars)
    let use_limit = count > 0;
    let effective_start = if use_limit { None } else { start };

    // Build WHERE clause
    let mut where_parts = vec!["symbol = ?".to_string()];

    if let Some(_s) = effective_start {
        where_parts.push("datetime >= ?".to_string());
    }
    if let Some(_e) = end {
        where_parts.push("datetime <= ?".to_string());
    }

    // Build ORDER BY and LIMIT clauses
    let order_direction = if use_limit { " DESC" } else { "" };
    let limit_clause = if use_limit { " LIMIT ?" } else { "" };

    // Build final query
    let where_clause = where_parts.join(" AND ");
    let query = format!(
        "SELECT strftime(datetime, '%Y-%m-%d %H:%M:%S.%f') AS datetime_str, open, high, low, close, volume FROM {} WHERE {} ORDER BY datetime{}{}",
        table_name, where_clause, order_direction, limit_clause
    );

    // Execute query with parameters
    let mut stmt = conn.prepare(&query).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to prepare query: {}", e))
    })?;

    // Helper function to map row to KlineBar
    let map_row = |row: &duckdb::Row| -> duckdb::Result<KlineBar> {
        let mut datetime: String = row.get(0)?;

        if let Some(stripped) = datetime.strip_suffix(".000000") {
            datetime = stripped.to_string();
        } else if datetime.contains('.') {
            while datetime.ends_with('0') {
                datetime.pop();
            }
            if datetime.ends_with('.') {
                datetime.pop();
            }
        }

        Ok(KlineBar {
            datetime,
            open: row.get::<_, f64>(1)?,
            high: row.get::<_, f64>(2)?,
            low: row.get::<_, f64>(3)?,
            close: row.get::<_, f64>(4)?,
            volume: row.get::<_, f64>(5)?,
            symbol: symbol.to_string(),
        })
    };

    // Execute query with dynamic parameters
    let rows = if use_limit {
        match (effective_start, end) {
            (Some(s), Some(e)) => stmt.query_map(duckdb::params![symbol, s, e, count], map_row),
            (Some(s), None) => stmt.query_map(duckdb::params![symbol, s, count], map_row),
            (None, Some(e)) => stmt.query_map(duckdb::params![symbol, e, count], map_row),
            (None, None) => stmt.query_map(duckdb::params![symbol, count], map_row),
        }
    } else {
        match (effective_start, end) {
            (Some(s), Some(e)) => stmt.query_map(duckdb::params![symbol, s, e], map_row),
            (Some(s), None) => stmt.query_map(duckdb::params![symbol, s], map_row),
            (None, Some(e)) => stmt.query_map(duckdb::params![symbol, e], map_row),
            (None, None) => stmt.query_map(duckdb::params![symbol], map_row),
        }
    }
    .map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to execute query: {}", e))
    })?;

    // Collect results
    let mut bars = Vec::new();
    for row_result in rows {
        bars.push(row_result.map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!("Failed to read row: {}", e))
        })?);
    }

    // If count was used, results are in DESC order (newest first), reverse to ASC order
    if use_limit {
        bars.reverse();
    }

    Ok(bars)
}

/// Load and synthesize K-line data directly from DuckDB
/// This is the high-performance version that eliminates all Python conversions
pub fn load_and_synthesize_klines_rust(
    db_path: &str,
    symbol: &str,
    target_period: &str,
    start: Option<&str>,
    end: Option<&str>,
    count: i64,
) -> PyResult<Vec<KlineBar>> {
    load_klines_rust(db_path, symbol, target_period, start, end, count)
}

/// Python-exposed function: Fetch market data directly from DuckDB (high performance)
#[pyfunction]
#[pyo3(signature = (db_path, symbol, period, start=None, end=None, count=-1))]
pub fn get_market_data(
    py: Python,
    db_path: String,
    symbol: String,
    period: String,
    start: Option<String>,
    end: Option<String>,
    count: i64,
) -> PyResult<PyObject> {
    let bars = load_klines_rust(
        &db_path,
        &symbol,
        &period,
        start.as_deref(),
        end.as_deref(),
        count,
    )?;

    let py_list = PyList::empty(py);
    for bar in bars {
        let py_dict = kline_bar_to_pydict(py, &bar)?;
        py_list.append(py_dict)?;
    }

    Ok(py_list.into())
}

/// Python-exposed function: Load and synthesize K-lines directly from DuckDB
#[pyfunction]
#[pyo3(signature = (db_path, symbol, target_period, start=None, end=None, count=-1))]
pub fn load_and_synthesize_klines(
    py: Python,
    db_path: String,
    symbol: String,
    target_period: String,
    start: Option<String>,
    end: Option<String>,
    count: i64,
) -> PyResult<PyObject> {
    let bars = load_and_synthesize_klines_rust(
        &db_path,
        &symbol,
        &target_period,
        start.as_deref(),
        end.as_deref(),
        count,
    )?;

    // Convert to Python list (only once at the end)
    let py_list = PyList::empty(py);
    for bar in bars {
        let py_dict = kline_bar_to_pydict(py, &bar)?;
        py_list.append(py_dict)?;
    }

    Ok(py_list.into())
}

/// Save K-line data directly to DuckDB (Rust implementation)
#[pyfunction]
pub fn save_klines(
    db_path: String,
    symbol: String,
    period: String,
    bars: &PyList,
    replace: bool,
) -> PyResult<()> {

    // Connect to database
    let conn = Connection::open(Path::new(&db_path)).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to connect to database: {}",
            e
        ))
    })?;

    let table_name = ensure_period_table(&conn, &period)?;

    // Delete old data if replace is true
    if replace {
        conn.execute(
            &format!("DELETE FROM {} WHERE symbol = ?", table_name),
            duckdb::params![symbol],
        )
        .map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Failed to delete old data: {}",
                e
            ))
        })?;
    }

    // Convert Python bars to Rust KlineBar
    let mut kline_bars = Vec::with_capacity(bars.len());
    for item in bars.iter() {
        let bar_dict: &PyDict = item.downcast()?;
        let datetime: String = bar_dict
            .get_item("datetime")?
            .and_then(|v| v.extract().ok())
            .unwrap_or_else(|| "".to_string());
        let open: f64 = bar_dict
            .get_item("open")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let high: f64 = bar_dict
            .get_item("high")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let low: f64 = bar_dict
            .get_item("low")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let close: f64 = bar_dict
            .get_item("close")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);
        let volume: f64 = bar_dict
            .get_item("volume")?
            .and_then(|v| v.extract().ok())
            .unwrap_or(0.0);

        kline_bars.push(KlineBar {
            datetime,
            open,
            high,
            low,
            close,
            volume,
            symbol: symbol.clone(),
        });
    }

    // Use transaction for better performance with large datasets
    conn.execute("BEGIN TRANSACTION", []).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to begin transaction: {}",
            e
        ))
    })?;

    // Ultra-fast batch insert using temporary table
    // Strategy: Create temp table -> Bulk insert -> Insert into target table -> Drop temp table
    // This is much faster than individual batch inserts
    let temp_table = unique_temp_table_name("temp_klines");

    // 事务内任何失败路径：先 ROLLBACK（并尽力清理临时表）再返回 Err
    if let Err(e) = save_klines_in_transaction(&conn, &table_name, &temp_table, &kline_bars) {
        let _ = conn.execute("ROLLBACK", []);
        let _ = conn.execute(&format!("DROP TABLE IF EXISTS {}", temp_table), []);
        return Err(e);
    }

    // Commit transaction (all inserts happen atomically)
    conn.execute("COMMIT", []).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to commit transaction: {}",
            e
        ))
    })?;

    Ok(())
}

/// 事务体：临时表批量写入 + 合并进目标表。任何步骤失败都返回 Err，
/// 由调用方负责 ROLLBACK。
fn save_klines_in_transaction(
    conn: &Connection,
    table_name: &str,
    temp_table: &str,
    kline_bars: &[KlineBar],
) -> PyResult<()> {
    // Create temporary table with same structure
    conn.execute(
        &format!(
            "CREATE TEMP TABLE {} (
                symbol VARCHAR NOT NULL,
                datetime TIMESTAMP NOT NULL,
                open DOUBLE NOT NULL,
                high DOUBLE NOT NULL,
                low DOUBLE NOT NULL,
                close DOUBLE NOT NULL,
                volume DOUBLE NOT NULL
            )",
            temp_table
        ),
        []
    ).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to create temporary table: {}",
            e
        ))
    })?;

    // Helper function to escape SQL string
    fn escape_sql_string(s: &str) -> String {
        s.replace("'", "''")
    }

    // Batch insert into temp table in large chunks (50000 records per batch)
    const BATCH_SIZE: usize = 50000;
    let total = kline_bars.len();
    
    for batch_start in (0..total).step_by(BATCH_SIZE) {
        let batch_end = std::cmp::min(batch_start + BATCH_SIZE, total);
        let batch = &kline_bars[batch_start..batch_end];
        
        // Pre-allocate capacity for better performance
        let mut values_parts = Vec::with_capacity(batch.len());
        for bar in batch.iter() {
            // Escape strings and format SQL values
            let symbol_escaped = escape_sql_string(&bar.symbol);
            let datetime_escaped = escape_sql_string(&bar.datetime);
            values_parts.push(format!(
                "('{}', '{}', {}, {}, {}, {}, {})",
                symbol_escaped,
                datetime_escaped,
                bar.open,
                bar.high,
                bar.low,
                bar.close,
                bar.volume
            ));
        }
        let values_clause = values_parts.join(", ");
        
        // Insert batch into temporary table (no conflict checking needed)
        let insert_query = format!(
            "INSERT INTO {} (symbol, datetime, open, high, low, close, volume) 
             VALUES {}",
            temp_table, values_clause
        );
        
        conn.execute(&insert_query, []).map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Failed to insert batch into temp table at index {}: {}",
                batch_start, e
            ))
        })?;
    }

    // Now insert from temp table to target table in one operation
    // This is much faster than individual inserts with conflict checking
    conn.execute(
        &format!(
            "INSERT INTO {} (symbol, datetime, open, high, low, close, volume)
             SELECT symbol, datetime, open, high, low, close, volume
             FROM {}
             ON CONFLICT (symbol, datetime) DO NOTHING",
            table_name, temp_table
        ),
        []
    ).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to insert from temp table to target table: {}",
            e
        ))
    })?;

    // Drop temporary table
    conn.execute(&format!("DROP TABLE {}", temp_table), []).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to drop temporary table: {}",
            e
        ))
    })?;

    Ok(())
}

/// Save K-line data directly from CSV file to DuckDB (Ultra-fast, bypasses Python parsing)
/// This is the fastest method as DuckDB reads CSV directly
#[pyfunction]
pub fn save_klines_from_csv(
    db_path: String,
    csv_path: String,
    symbol: String,
    period: String,
    replace: bool,
) -> PyResult<()> {

    // Connect to database
    let conn = Connection::open(Path::new(&db_path)).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to connect to database: {}",
            e
        ))
    })?;

    let table_name = ensure_period_table(&conn, &period)?;

    // Delete old data if replace is true
    if replace {
        conn.execute(
            &format!("DELETE FROM {} WHERE symbol = ?", table_name),
            duckdb::params![symbol],
        )
        .map_err(|e| {
            PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
                "Failed to delete old data: {}",
                e
            ))
        })?;
    }

    // Escape CSV path for SQL (handle single quotes)
    let csv_path_escaped = csv_path.replace("'", "''");

    // Use transaction
    conn.execute("BEGIN TRANSACTION", []).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to begin transaction: {}",
            e
        ))
    })?;

    // Create temporary table and load CSV directly
    let temp_table = unique_temp_table_name("temp_csv_import");

    // 事务内任何失败路径：先 ROLLBACK（并尽力清理临时表）再返回 Err
    macro_rules! txn_fail {
        ($e:expr) => {{
            let err = $e;
            let _ = conn.execute("ROLLBACK", []);
            let _ = conn.execute(&format!("DROP TABLE IF EXISTS {}", temp_table), []);
            return Err(err);
        }};
    }
    
    // DuckDB can read CSV directly and infer schema
    // Expected CSV format: datetime,open,high,low,close,volume
    // Escape symbol for SQL
    let symbol_escaped = symbol.replace("'", "''");
    let create_temp_sql = format!(
        "CREATE TEMP TABLE {} AS 
         SELECT 
             '{}' as symbol,
             CAST(datetime AS TIMESTAMP) as datetime,
             CAST(open AS DOUBLE) as open,
             CAST(high AS DOUBLE) as high,
             CAST(low AS DOUBLE) as low,
             CAST(close AS DOUBLE) as close,
             CAST(volume AS DOUBLE) as volume
         FROM read_csv('{}', 
             header=true,
             auto_detect=true)",
        temp_table, symbol_escaped, csv_path_escaped
    );

    if let Err(e) = conn.execute(&create_temp_sql, []) {
        txn_fail!(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to read CSV file: {}. Make sure CSV has headers: datetime,open,high,low,close,volume",
            e
        )));
    }

    // Insert from temp table to target table
    if let Err(e) = conn.execute(
        &format!(
            "INSERT INTO {} (symbol, datetime, open, high, low, close, volume)
             SELECT symbol, datetime, open, high, low, close, volume
             FROM {}
             ON CONFLICT (symbol, datetime) DO NOTHING",
            table_name, temp_table
        ),
        [],
    ) {
        txn_fail!(PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to insert from temp table: {}",
            e
        )));
    }

    // Drop temporary table
    conn.execute(&format!("DROP TABLE {}", temp_table), []).ok();

    // Commit transaction
    conn.execute("COMMIT", []).map_err(|e| {
        PyErr::new::<pyo3::exceptions::PyRuntimeError, _>(format!(
            "Failed to commit transaction: {}",
            e
        ))
    })?;

    Ok(())
}
