use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::ptr;
use std::sync::Mutex;

use mysql::prelude::*;
use mysql::{OptsBuilder, Pool, Row, TxOpts, Value};

// ============================================================
// Internal types
// ============================================================

struct RustMySQLInner {
    pool: Option<Pool>,
    opts: OptsBuilder,
    last_error_cstr: Option<CString>,
}

impl RustMySQLInner {
    fn set_error(&mut self, msg: String) {
        self.last_error_cstr = CString::new(msg).ok();
    }

    fn get_or_create_pool(&mut self) -> Result<Pool, ()> {
        if self.pool.is_none() {
            match Pool::new(self.opts.clone()) {
                Ok(p) => self.pool = Some(p),
                Err(e) => {
                    self.set_error(format!("Failed to create pool: {}", e));
                    return Err(());
                }
            }
        }
        Ok(self.pool.as_ref().unwrap().clone())
    }

    fn connect(&mut self) -> bool {
        let pool = match self.get_or_create_pool() {
            Ok(p) => p,
            Err(_) => return false,
        };
        match pool.get_conn() {
            Ok(mut conn) => {
                conn.exec_drop("SELECT 1", ()).ok();
                true
            }
            Err(e) => {
                self.set_error(format!("Connection failed: {}", e));
                false
            }
        }
    }

    fn disconnect(&mut self) -> bool {
        self.pool = None;
        self.last_error_cstr = None;
        true
    }

    fn is_connected(&self) -> bool {
        match &self.pool {
            Some(pool) => match pool.get_conn() {
                Ok(mut conn) => conn.exec_drop("SELECT 1", ()).is_ok(),
                Err(_) => false,
            },
            None => false,
        }
    }
}

/// Top-level opaque handle. Stored behind a raw pointer in C++.
struct RustMySQL {
    inner: Mutex<RustMySQLInner>,
}

/// Opaque result — all strings stored as CString so pointers
/// remain valid until rust_mysql_free_result.
struct OpaqueResult {
    cols: Vec<CString>,
    rows: Vec<Vec<CString>>,
}

// ============================================================
// Helpers
// ============================================================

unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> &'a str {
    if ptr.is_null() {
        return "";
    }
    CStr::from_ptr(ptr).to_str().unwrap_or("")
}

fn make_params(raw: *const *const c_char, count: i32) -> Vec<Value> {
    if raw.is_null() || count <= 0 {
        return vec![];
    }
    let slice = unsafe { std::slice::from_raw_parts(raw, count as usize) };
    slice
        .iter()
        .map(|&p| Value::from(unsafe { cstr_to_str(p) }))
        .collect()
}

/// Convert any mysql::Value to a string representation.
fn mysql_value_to_string(val: &Value) -> String {
    match val {
        Value::NULL => String::new(),
        Value::Int(i) => i.to_string(),
        Value::UInt(u) => u.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Double(d) => d.to_string(),
        Value::Bytes(b) => String::from_utf8_lossy(b).to_string(),
        Value::Date(y, m, d, h, min, s, _us) => {
            if *y == 0 {
                // time-only
                format!("{:02}:{:02}:{:02}", h, min, s)
            } else {
                format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, min, s)
            }
        }
        _ => format!("{:?}", val),
    }
}

/// Collect rows from an exec_iter result into OpaqueResult.
fn collect_rows(
    result: &mut mysql::QueryResult<'_, '_, '_, mysql::Binary>,
) -> Result<OpaqueResult, String> {
    let columns = result.columns().as_ref().to_vec();
    let cols: Vec<CString> = columns
        .iter()
        .map(|c| CString::new(c.name_str().as_bytes()).unwrap_or_default())
        .collect();

    let col_count = cols.len();
    let mut rows = Vec::new();

    for row_res in result.by_ref() {
        let row: Row = row_res.map_err(|e| format!("Row: {}", e))?;
        let mut r = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let val: Value = row.get(i).flatten().unwrap_or(Value::NULL);
            let s = mysql_value_to_string(&val);
            r.push(CString::new(s).unwrap_or_default());
        }
        rows.push(r);
    }

    Ok(OpaqueResult { cols, rows })
}

type CancelCb = unsafe extern "C" fn(*mut c_void) -> bool;

/// Owns every callback invocation. Dropping this guard wakes and joins the
/// watcher before the borrowed C++ cancellation context may be destroyed.
struct CancelWatcher {
    stop: std::sync::mpsc::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl CancelWatcher {
    fn start(mut poll: impl FnMut() -> bool + Send + 'static) -> Result<Self, String> {
        let (stop, rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("tianyan-query-cancel".into())
            .spawn(move || {
                while matches!(
                    rx.recv_timeout(std::time::Duration::from_millis(100)),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout)
                ) {
                    if poll() {
                        break;
                    }
                }
            })
            .map_err(|e| format!("cancel watcher: {}", e))?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for CancelWatcher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Run a query with true cancellation support.
///
/// Executes the query in the current thread, streaming rows one at a time
/// and checking the cancel callback between each row.
///
/// A watcher thread monitors the cancel flag while the main thread is
/// blocked on I/O. If cancellation is requested, it sends `KILL QUERY`
/// to the MySQL server via a separate connection, which:
///   1. Interrupts the server-side query execution
///   2. Causes the blocking `exec_iter` / `next()` to return an error
///   3. Allows us to exit promptly instead of waiting for the query to finish
fn query_with_cancel(
    pool: Pool,
    sql: String,
    params: Vec<Value>,
    cancel_fn: Option<CancelCb>,
    cancel_ctx: *mut c_void,
) -> Result<OpaqueResult, String> {
    use std::time::Duration;

    let is_cancelled = || -> bool { cancel_fn.map_or(false, |cb| unsafe { cb(cancel_ctx) }) };
    if is_cancelled() {
        return Err("Cancelled".to_string());
    }

    let mut conn = pool.get_conn().map_err(|e| format!("conn: {}", e))?;
    let conn_id = conn.connection_id();
    // Keep the query's connection leased until the watcher has joined. Otherwise
    // a late KILL QUERY could target another caller reusing that connection.
    let watch_ctx = cancel_ctx as usize;
    let _watcher = CancelWatcher::start(move || {
        if cancel_fn.map_or(false, |cb| unsafe { cb(watch_ctx as *mut c_void) }) {
            if let Ok(mut kill_conn) = pool.try_get_conn(Duration::from_secs(1)) {
                let _ = kill_conn.exec_drop(format!("KILL QUERY {}", conn_id), ());
            }
            true
        } else {
            false
        }
    })?;

    // Execute query — this may block if the query is complex.
    // The watcher thread can KILL QUERY to interrupt this.
    let mut result = match conn.exec_iter(&sql, params) {
        Ok(r) => r,
        Err(e) => {
            if is_cancelled() {
                return Err("Cancelled".to_string());
            }
            return Err(format!("exec_iter: {}", e));
        }
    };

    // One more check after query starts
    if is_cancelled() {
        return Err("Cancelled".to_string());
    }

    let columns = result.columns().as_ref().to_vec();
    let cols: Vec<CString> = columns
        .iter()
        .map(|c| CString::new(c.name_str().as_bytes()).unwrap_or_default())
        .collect();
    let col_count = cols.len();

    // Safety cap: never accumulate more than this many rows in memory.
    // Prevents unbounded memory growth for extremely large result sets.
    const MAX_ROWS: usize = 100_000;

    let mut rows = Vec::new();

    // Stream rows one at a time, checking cancel between each row.
    // This lets us exit early on cancellation instead of pulling
    // the entire result set into memory only to discard it.
    for row_res in result.by_ref() {
        if is_cancelled() {
            return Err("Cancelled".to_string());
        }

        // Hard cap to prevent OOM on extremely large queries
        if rows.len() >= MAX_ROWS {
            break;
        }

        let row = match row_res {
            Ok(r) => r,
            Err(e) => {
                if is_cancelled() {
                    // exec_iter was interrupted by KILL QUERY — expected
                    return Err("Cancelled".to_string());
                }
                return Err(format!("Row: {}", e));
            }
        };

        let mut r = Vec::with_capacity(col_count);
        for i in 0..col_count {
            let val: Value = row.get(i).flatten().unwrap_or(Value::NULL);
            let s = mysql_value_to_string(&val);
            r.push(CString::new(s).unwrap_or_default());
        }
        rows.push(r);
    }

    Ok(OpaqueResult { cols, rows })
}

// ============================================================
// FFI: lifecycle
// ============================================================

#[no_mangle]
pub extern "C" fn rust_mysql_new(
    host: *const c_char,
    port: u16,
    user: *const c_char,
    pass: *const c_char,
    db: *const c_char,
) -> *mut c_void {
    let opts = OptsBuilder::new()
        .ip_or_hostname(Some(unsafe { cstr_to_str(host) }))
        .tcp_port(port)
        .user(Some(unsafe { cstr_to_str(user) }))
        .pass(Some(unsafe { cstr_to_str(pass) }))
        .db_name(Some(unsafe { cstr_to_str(db) }));

    let m = RustMySQL {
        inner: Mutex::new(RustMySQLInner {
            pool: None,
            opts,
            last_error_cstr: None,
        }),
    };
    Box::into_raw(Box::new(m)) as *mut c_void
}

#[no_mangle]
pub extern "C" fn rust_mysql_free(handle: *mut c_void) {
    if !handle.is_null() {
        unsafe {
            drop(Box::from_raw(handle as *mut RustMySQL));
        }
    }
}

// ============================================================
// FFI: connection
// ============================================================

#[no_mangle]
pub extern "C" fn rust_mysql_connect(handle: *mut c_void) -> bool {
    if handle.is_null() {
        return false;
    }
    let m = unsafe { &*(handle as *const RustMySQL) };
    match m.inner.lock() {
        Ok(mut g) => g.connect(),
        Err(_) => false,
    }
}

#[no_mangle]
pub extern "C" fn rust_mysql_disconnect(handle: *mut c_void) -> bool {
    if handle.is_null() {
        return false;
    }
    let m = unsafe { &*(handle as *const RustMySQL) };
    match m.inner.lock() {
        Ok(mut g) => g.disconnect(),
        Err(_) => false,
    }
}

#[no_mangle]
pub extern "C" fn rust_mysql_is_connected(handle: *const c_void) -> bool {
    if handle.is_null() {
        return false;
    }
    let m = unsafe { &*(handle as *const RustMySQL) };
    match m.inner.lock() {
        Ok(g) => g.is_connected(),
        Err(_) => false,
    }
}

// ============================================================
// FFI: query / execute
// ============================================================

/// Extract a Pool from the handle. Returns Err on mutex poison.
fn get_pool(handle: *mut c_void) -> Result<Pool, ()> {
    let m = unsafe { &*(handle as *const RustMySQL) };
    match m.inner.lock() {
        Ok(mut g) => g.get_or_create_pool(),
        Err(e) => {
            e.into_inner().set_error("Mutex poisoned".to_string());
            Err(())
        }
    }
}

/// Helper: get pool, run a closure returning *mut c_void, store error on failure.
fn with_pool<F>(handle: *mut c_void, f: F) -> *mut c_void
where
    F: FnOnce(Pool) -> Result<*mut c_void, String>,
{
    let pool = match get_pool(handle) {
        Ok(p) => p,
        Err(_) => return ptr::null_mut(),
    };

    match f(pool) {
        Ok(r) => r,
        Err(e) => {
            let m = unsafe { &*(handle as *const RustMySQL) };
            if let Ok(mut guard) = m.inner.lock() {
                guard.set_error(e);
            }
            ptr::null_mut()
        }
    }
}

/// Helper: same but for execute (returns i64).
fn with_pool_i64<F>(handle: *mut c_void, f: F) -> i64
where
    F: FnOnce(Pool) -> Result<i64, String>,
{
    let pool = match get_pool(handle) {
        Ok(p) => p,
        Err(_) => return -1,
    };

    match f(pool) {
        Ok(r) => r,
        Err(e) => {
            let m = unsafe { &*(handle as *const RustMySQL) };
            if let Ok(mut guard) = m.inner.lock() {
                guard.set_error(e);
            }
            -1
        }
    }
}

#[no_mangle]
pub extern "C" fn rust_mysql_query(handle: *mut c_void, sql: *const c_char) -> *mut c_void {
    if handle.is_null() || sql.is_null() {
        return ptr::null_mut();
    }
    let sql_str = unsafe { cstr_to_str(sql) }.to_string();

    with_pool(handle, |pool| {
        let mut conn = pool.get_conn().map_err(|e| format!("conn: {}", e))?;
        let empty: Vec<Value> = vec![];
        let mut result = conn
            .exec_iter(&sql_str, empty)
            .map_err(|e| format!("query: {}", e))?;
        let opaque = collect_rows(&mut result)?;
        Ok(Box::into_raw(Box::new(opaque)) as *mut c_void)
    })
}

#[no_mangle]
pub extern "C" fn rust_mysql_query_params(
    handle: *mut c_void,
    sql: *const c_char,
    params: *const *const c_char,
    param_count: i32,
) -> *mut c_void {
    if handle.is_null() || sql.is_null() {
        return ptr::null_mut();
    }
    let sql_str = unsafe { cstr_to_str(sql) }.to_string();
    let values = make_params(params, param_count);

    with_pool(handle, |pool| {
        let mut conn = pool.get_conn().map_err(|e| format!("conn: {}", e))?;
        let mut result = conn
            .exec_iter(&sql_str, values)
            .map_err(|e| format!("query: {}", e))?;
        let opaque = collect_rows(&mut result)?;
        Ok(Box::into_raw(Box::new(opaque)) as *mut c_void)
    })
}

#[no_mangle]
pub extern "C" fn rust_mysql_execute(handle: *mut c_void, sql: *const c_char) -> i64 {
    if handle.is_null() || sql.is_null() {
        return -1;
    }
    let sql_str = unsafe { cstr_to_str(sql) }.to_string();

    with_pool_i64(handle, |pool| {
        let mut conn = pool.get_conn().map_err(|e| format!("conn: {}", e))?;
        conn.exec_drop(&sql_str, ())
            .map_err(|e| format!("exec: {}", e))?;
        Ok(conn.affected_rows() as i64)
    })
}

#[no_mangle]
pub extern "C" fn rust_mysql_execute_params(
    handle: *mut c_void,
    sql: *const c_char,
    params: *const *const c_char,
    param_count: i32,
) -> i64 {
    if handle.is_null() || sql.is_null() {
        return -1;
    }
    let sql_str = unsafe { cstr_to_str(sql) }.to_string();
    let values = make_params(params, param_count);

    with_pool_i64(handle, |pool| {
        let mut conn = pool.get_conn().map_err(|e| format!("conn: {}", e))?;
        conn.exec_drop(&sql_str, values)
            .map_err(|e| format!("exec: {}", e))?;
        Ok(conn.affected_rows() as i64)
    })
}

/// One statement in an atomic batch. All input buffers are borrowed only
/// during the synchronous call; each statement executes on the SAME lease.
#[repr(C)]
pub struct BatchStatement {
    sql: *const c_char,
    params: *const *const c_char,
    param_count: i32,
}

#[no_mangle]
pub extern "C" fn rust_mysql_execute_batch(
    handle: *mut c_void,
    statements: *const BatchStatement,
    count: i32,
) -> i64 {
    if handle.is_null() || count < 0 || (count > 0 && statements.is_null()) {
        return -1;
    }
    if count == 0 {
        return 0;
    }
    let batch = unsafe { std::slice::from_raw_parts(statements, count as usize) };
    if batch
        .iter()
        .any(|s| s.sql.is_null() || s.param_count < 0 || (s.param_count > 0 && s.params.is_null()))
    {
        return -1;
    }
    with_pool_i64(handle, |pool| {
        let mut conn = pool.get_conn().map_err(|e| format!("conn: {}", e))?;
        let mut tx = conn
            .start_transaction(TxOpts::default())
            .map_err(|e| format!("begin: {}", e))?;
        let mut affected = 0_i64;
        for statement in batch {
            tx.exec_drop(
                unsafe { cstr_to_str(statement.sql) },
                make_params(statement.params, statement.param_count),
            )
            .map_err(|e| format!("batch: {}", e))?;
            affected = affected.saturating_add(tx.affected_rows() as i64);
        }
        // Transaction's Drop rolls back on every earlier error; commit errors
        // are returned, never reported to the caller as success.
        tx.commit().map_err(|e| format!("commit: {}", e))?;
        Ok(affected)
    })
}

#[no_mangle]
pub extern "C" fn rust_mysql_query_params_with_cancel(
    handle: *mut c_void,
    sql: *const c_char,
    params: *const *const c_char,
    param_count: i32,
    cancel_fn: Option<CancelCb>,
    cancel_ctx: *mut c_void,
) -> *mut c_void {
    if handle.is_null() || sql.is_null() {
        return ptr::null_mut();
    }
    let sql_str = unsafe { cstr_to_str(sql) }.to_string();
    let values = make_params(params, param_count);

    let pool = match get_pool(handle) {
        Ok(p) => p,
        Err(_) => return ptr::null_mut(),
    };

    match query_with_cancel(pool, sql_str, values, cancel_fn, cancel_ctx) {
        Ok(opaque) => Box::into_raw(Box::new(opaque)) as *mut c_void,
        Err(e) => {
            let m = unsafe { &*(handle as *const RustMySQL) };
            if let Ok(mut guard) = m.inner.lock() {
                guard.set_error(e);
            }
            ptr::null_mut()
        }
    }
}

// ============================================================
// FFI: result access
// ============================================================

fn with_result<R: Default>(result: *const c_void, f: impl FnOnce(&OpaqueResult) -> R) -> R {
    if result.is_null() {
        return R::default();
    }
    let r = unsafe { &*(result as *const OpaqueResult) };
    f(r)
}

#[no_mangle]
pub extern "C" fn rust_mysql_result_rows(result: *const c_void) -> i32 {
    with_result(result, |r| r.rows.len() as i32)
}

#[no_mangle]
pub extern "C" fn rust_mysql_result_cols(result: *const c_void) -> i32 {
    with_result(result, |r| r.cols.len() as i32)
}

#[no_mangle]
pub extern "C" fn rust_mysql_result_col_name(result: *const c_void, col: i32) -> *const c_char {
    with_result(result, |r| {
        let idx = col as usize;
        if idx < r.cols.len() {
            r.cols[idx].as_ptr()
        } else {
            ptr::null()
        }
    })
}

#[no_mangle]
pub extern "C" fn rust_mysql_result_value(
    result: *const c_void,
    row: i32,
    col: i32,
) -> *const c_char {
    with_result(result, |r| {
        let ri = row as usize;
        let ci = col as usize;
        if ri < r.rows.len() && ci < r.cols.len() {
            r.rows[ri][ci].as_ptr()
        } else {
            ptr::null()
        }
    })
}

#[no_mangle]
pub extern "C" fn rust_mysql_free_result(result: *mut c_void) {
    if !result.is_null() {
        unsafe {
            drop(Box::from_raw(result as *mut OpaqueResult));
        }
    }
}

// ============================================================
// FFI: error
// ============================================================

#[no_mangle]
/// Returns an owned copy; the caller must use rust_mysql_free_string.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
pub extern "C" fn rust_mysql_last_error(handle: *const c_void) -> *mut c_char {
    if handle.is_null() {
        return ptr::null_mut();
    }
    let m = unsafe { &*(handle as *const RustMySQL) };
    match m.inner.lock() {
        Ok(g) => match &g.last_error_cstr {
            Some(cs) => cs.clone().into_raw(),
            None => ptr::null_mut(),
        },
        Err(_) => ptr::null_mut(),
    }
}

// ============================================================
// FFI: uuid
// ============================================================

#[no_mangle]
pub extern "C" fn rust_mysql_generate_uuid() -> *mut c_char {
    let uuid = uuid::Uuid::new_v4().to_string();
    CString::new(uuid).unwrap_or_default().into_raw()
}

#[no_mangle]
pub extern "C" fn rust_mysql_free_string(s: *mut c_char) {
    if !s.is_null() {
        unsafe {
            drop(CString::from_raw(s));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    #[test]
    fn watcher_drop_joins_an_inflight_callback() {
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let flag = finished.clone();
        let watcher = CancelWatcher::start(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
            flag.store(true, Ordering::Release);
            true
        })
        .unwrap();
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let (dropped_tx, dropped_rx) = mpsc::channel();
        let dropper = std::thread::spawn(move || {
            drop(watcher);
            dropped_tx.send(()).unwrap();
        });
        assert!(dropped_rx.recv_timeout(Duration::from_millis(20)).is_err());
        release_tx.send(()).unwrap();
        dropped_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        dropper.join().unwrap();
        assert!(finished.load(Ordering::Acquire));
    }

    #[test]
    fn watcher_stop_does_not_poll_borrowed_context_after_return() {
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let p = polls.clone();
        let watcher = CancelWatcher::start(move || {
            p.fetch_add(1, Ordering::Relaxed);
            false
        })
        .unwrap();
        drop(watcher);
        let count = polls.load(Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(150));
        assert_eq!(polls.load(Ordering::Relaxed), count);
    }

    #[test]
    fn error_copy_survives_replacement_and_handle_destruction() {
        let handle = Box::into_raw(Box::new(RustMySQL {
            inner: Mutex::new(RustMySQLInner {
                pool: None,
                opts: OptsBuilder::new(),
                last_error_cstr: Some(CString::new("first error").unwrap()),
            }),
        })) as *mut c_void;
        let first = rust_mysql_last_error(handle);
        let m = unsafe { &*(handle as *const RustMySQL) };
        m.inner.lock().unwrap().set_error("new error".into());
        rust_mysql_free(handle);
        assert_eq!(
            unsafe { CStr::from_ptr(first) }.to_str().unwrap(),
            "first error"
        );
        rust_mysql_free_string(first);
    }

    #[test]
    #[ignore = "requires isolated MySQL with TIANYAN_MYSQL_TEST_URL"]
    fn mysql_batch_rolls_back_and_commits_on_one_connection() {
        let url = std::env::var("TIANYAN_MYSQL_TEST_URL").expect("isolated test database URL");
        let opts = OptsBuilder::from_opts(mysql::Opts::from_url(&url).unwrap());
        let pool = Pool::new(opts.clone()).unwrap();
        let handle = Box::into_raw(Box::new(RustMySQL {
            inner: Mutex::new(RustMySQLInner {
                pool: Some(pool.clone()),
                opts,
                last_error_cstr: None,
            }),
        })) as *mut c_void;
        let table = format!("batch_test_{}", uuid::Uuid::new_v4().simple());
        let mut conn = pool.get_conn().unwrap();
        conn.query_drop(format!(
            "CREATE TABLE {} (id INT PRIMARY KEY) ENGINE=InnoDB",
            table
        ))
        .unwrap();
        let insert = CString::new(format!("INSERT INTO {} VALUES (?)", table)).unwrap();
        let value = CString::new("1").unwrap();
        let params = [value.as_ptr()];
        let statements = [
            BatchStatement {
                sql: insert.as_ptr(),
                params: params.as_ptr(),
                param_count: 1,
            },
            BatchStatement {
                sql: insert.as_ptr(),
                params: params.as_ptr(),
                param_count: 1,
            },
        ];
        // A duplicate key in the second statement must roll back the first.
        assert_eq!(rust_mysql_execute_batch(handle, statements.as_ptr(), 2), -1);
        let count: Option<u64> = conn
            .query_first(format!("SELECT COUNT(*) FROM {}", table))
            .unwrap();
        assert_eq!(count, Some(0));
        assert_eq!(rust_mysql_execute_batch(handle, statements.as_ptr(), 1), 1);
        let count: Option<u64> = conn
            .query_first(format!("SELECT COUNT(*) FROM {}", table))
            .unwrap();
        assert_eq!(count, Some(1));
        conn.query_drop(format!("DROP TABLE {}", table)).unwrap();
        rust_mysql_free(handle);
    }
}
