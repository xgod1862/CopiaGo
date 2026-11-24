//! The SQLite calls used by history, resolved from the Windows system DLL.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::io::{self, ErrorKind};
use std::path::Path;
use std::ptr::{NonNull, null, null_mut};
use std::sync::OnceLock;

use windows_sys::Win32::Foundation::{FreeLibrary, HMODULE};
use windows_sys::Win32::System::LibraryLoader::{
    GetProcAddress, LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW,
};
use windows_sys::w;

const OK: c_int = 0;
const ROW: c_int = 100;
const DONE: c_int = 101;
const OPEN_READWRITE: c_int = 0x0000_0002;
const OPEN_CREATE: c_int = 0x0000_0004;

#[repr(C)]
struct DbHandle {
    _opaque: [u8; 0],
}

#[repr(C)]
struct StmtHandle {
    _opaque: [u8; 0],
}

type Destructor = Option<unsafe extern "C" fn(*mut c_void)>;
type Export = unsafe extern "system" fn() -> isize;
const STATIC: Destructor = None;

// Keep each export name next to its SQLite C signature.
macro_rules! sqlite_api {
    ($($name:ident: $signature:ty),+ $(,)?) => {
        struct Api {
            $($name: $signature),+
        }

        impl Api {
            fn load() -> io::Result<Self> {
                let module = SystemDll::load()?;
                let api = Self {
                    $($name: {
                        let name = concat!(stringify!($name), "\0");
                        // SAFETY: The module is loaded and the export name is NUL-terminated.
                        let symbol = unsafe { GetProcAddress(module.0, name.as_ptr()) }
                            .ok_or_else(io::Error::last_os_error)?;
                        // SAFETY: This named SQLite export has the C signature declared below.
                        unsafe { std::mem::transmute::<Export, $signature>(symbol) }
                    }),+
                };
                // The cached function pointers must stay valid for the life of the process.
                std::mem::forget(module);
                Ok(api)
            }
        }
    };
}

sqlite_api! {
    sqlite3_open_v2: unsafe extern "C" fn(*const c_char, *mut *mut DbHandle, c_int, *const c_char) -> c_int,
    sqlite3_close: unsafe extern "C" fn(*mut DbHandle) -> c_int,
    sqlite3_busy_timeout: unsafe extern "C" fn(*mut DbHandle, c_int) -> c_int,
    sqlite3_prepare_v2: unsafe extern "C" fn(*mut DbHandle, *const c_char, c_int, *mut *mut StmtHandle, *mut *const c_char) -> c_int,
    sqlite3_finalize: unsafe extern "C" fn(*mut StmtHandle) -> c_int,
    sqlite3_step: unsafe extern "C" fn(*mut StmtHandle) -> c_int,
    sqlite3_bind_int64: unsafe extern "C" fn(*mut StmtHandle, c_int, i64) -> c_int,
    sqlite3_bind_text: unsafe extern "C" fn(*mut StmtHandle, c_int, *const c_char, c_int, Destructor) -> c_int,
    sqlite3_bind_blob: unsafe extern "C" fn(*mut StmtHandle, c_int, *const c_void, c_int, Destructor) -> c_int,
    sqlite3_data_count: unsafe extern "C" fn(*mut StmtHandle) -> c_int,
    sqlite3_column_int64: unsafe extern "C" fn(*mut StmtHandle, c_int) -> i64,
    sqlite3_column_text: unsafe extern "C" fn(*mut StmtHandle, c_int) -> *const u8,
    sqlite3_column_blob: unsafe extern "C" fn(*mut StmtHandle, c_int) -> *const c_void,
    sqlite3_column_bytes: unsafe extern "C" fn(*mut StmtHandle, c_int) -> c_int,
}

struct SystemDll(HMODULE);

impl SystemDll {
    fn load() -> io::Result<Self> {
        // SAFETY: The name is a static NUL-terminated string. The search is restricted to System32.
        let module = unsafe {
            LoadLibraryExW(
                w!("winsqlite3.dll"),
                null_mut(),
                LOAD_LIBRARY_SEARCH_SYSTEM32,
            )
        };
        if module.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(module))
    }
}

impl Drop for SystemDll {
    fn drop(&mut self) {
        // SAFETY: This wrapper owns one reference from a successful LoadLibraryExW.
        unsafe { FreeLibrary(self.0) };
    }
}

fn api() -> io::Result<&'static Api> {
    static API: OnceLock<io::Result<Api>> = OnceLock::new();
    API.get_or_init(Api::load)
        .as_ref()
        .map_err(|error| io::Error::new(error.kind(), format!("loading winsqlite3.dll: {error}")))
}

pub(super) struct Connection {
    handle: NonNull<DbHandle>,
    api: &'static Api,
}

pub(super) enum Param<'a> {
    Integer(i64),
    Text(&'a str),
    Blob(&'a [u8]),
}

impl Connection {
    pub(super) fn open(path: &Path, create: bool) -> io::Result<Self> {
        // Use an absolute filename so SQLite cannot interpret a relative name as a URI.
        let path = std::path::absolute(path)?;
        let path = path.to_str().ok_or_else(|| {
            io::Error::new(ErrorKind::InvalidInput, "history path is not valid Unicode")
        })?;
        let filename = CString::new(path)
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "history path contains NUL"))?;
        let api = api()?;
        let flags = OPEN_READWRITE | if create { OPEN_CREATE } else { 0 };
        let mut handle = null_mut();
        // SAFETY: filename is terminated and handle is a writable output. NULL selects the default VFS.
        let code = unsafe { (api.sqlite3_open_v2)(filename.as_ptr(), &mut handle, flags, null()) };
        // An unsuccessful open can still allocate a handle that must be closed.
        let db = NonNull::new(handle).map(|handle| Self { handle, api });
        check(code, "open")?;
        let db = db.ok_or_else(|| io::Error::other("SQLite did not return a connection"))?;
        // SAFETY: The handle is open and no statements have been prepared yet.
        let code = unsafe { (api.sqlite3_busy_timeout)(db.handle.as_ptr(), 5000) };
        check(code, "set busy timeout")?;
        Ok(db)
    }

    pub(super) fn prepare<'a>(
        &'a self,
        sql: &'static CStr,
        params: &[Param<'a>],
    ) -> io::Result<Statement<'a>> {
        let mut handle = null_mut();
        // SAFETY: self and the NUL-terminated SQL stay live and handle is writable.
        let code = unsafe {
            (self.api.sqlite3_prepare_v2)(
                self.handle.as_ptr(),
                sql.as_ptr(),
                -1,
                &mut handle,
                null_mut(),
            )
        };
        check(code, "prepare")?;
        let handle = NonNull::new(handle)
            .ok_or_else(|| io::Error::other("SQLite did not return a statement"))?;
        let mut statement = Statement { db: self, handle };
        for (index, param) in params.iter().enumerate() {
            statement.bind(index as c_int + 1, param)?;
        }
        Ok(statement)
    }

    pub(super) fn execute(&self, sql: &'static CStr, params: &[Param<'_>]) -> io::Result<()> {
        let mut statement = self.prepare(sql, params)?;
        while statement.next()?.is_some() {}
        Ok(())
    }

    pub(super) fn transaction<T>(
        &self,
        write: impl FnOnce(&Self) -> io::Result<T>,
    ) -> io::Result<T> {
        self.execute(c"BEGIN IMMEDIATE", &[])?;
        let result = write(self).and_then(|value| {
            self.execute(c"COMMIT", &[])?;
            Ok(value)
        });
        if result.is_err() {
            let _ = self.execute(c"ROLLBACK", &[]);
        }
        result
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // SAFETY: Every statement borrows this handle and has already been finalized.
        unsafe { (self.api.sqlite3_close)(self.handle.as_ptr()) };
    }
}

pub(super) struct Statement<'a> {
    db: &'a Connection,
    handle: NonNull<StmtHandle>,
}

impl<'a> Statement<'a> {
    fn bind(&mut self, index: c_int, param: &Param<'a>) -> io::Result<()> {
        let api = self.db.api;
        let handle = self.handle.as_ptr();
        // SAFETY: The handle is live. The bound bytes share this statement's lifetime,
        // as required by SQLITE_STATIC, which avoids copying encrypted payloads.
        let code = unsafe {
            match param {
                Param::Integer(value) => (api.sqlite3_bind_int64)(handle, index, *value),
                Param::Text(value) => (api.sqlite3_bind_text)(
                    handle,
                    index,
                    value.as_ptr().cast(),
                    bind_length(value.len())?,
                    STATIC,
                ),
                Param::Blob(value) => (api.sqlite3_bind_blob)(
                    handle,
                    index,
                    value.as_ptr().cast(),
                    bind_length(value.len())?,
                    STATIC,
                ),
            }
        };
        check(code, "bind")
    }

    pub(super) fn next(&mut self) -> io::Result<Option<Row<'_, 'a>>> {
        // SAFETY: This is a live, exclusively borrowed statement.
        match unsafe { (self.db.api.sqlite3_step)(self.handle.as_ptr()) } {
            ROW => Ok(Some(Row(self))),
            DONE => Ok(None),
            code => Err(sqlite_error(code, "step")),
        }
    }
}

impl Drop for Statement<'_> {
    fn drop(&mut self) {
        // SAFETY: This statement owns the handle and its connection is still live.
        unsafe { (self.db.api.sqlite3_finalize)(self.handle.as_ptr()) };
    }
}

pub(super) struct Row<'a, 'db>(&'a mut Statement<'db>);

impl Row<'_, '_> {
    pub(super) fn integer(&mut self, index: c_int) -> io::Result<i64> {
        let handle = self.column(index)?;
        // SAFETY: column verified the index and the row exclusively borrows the active statement.
        Ok(unsafe { (self.0.db.api.sqlite3_column_int64)(handle, index) })
    }

    pub(super) fn text(&mut self, index: c_int) -> io::Result<&str> {
        std::str::from_utf8(self.bytes(index, true)?)
            .map_err(|_| io::Error::new(ErrorKind::InvalidData, "invalid SQLite text"))
    }

    pub(super) fn blob(&mut self, index: c_int) -> io::Result<&[u8]> {
        self.bytes(index, false)
    }

    fn column(&self, index: c_int) -> io::Result<*mut StmtHandle> {
        let handle = self.0.handle.as_ptr();
        // SAFETY: The row exists only after SQLITE_ROW and prevents stepping the statement again.
        let count = unsafe { (self.0.db.api.sqlite3_data_count)(handle) };
        if !(0..count).contains(&index) {
            return Err(io::Error::other("invalid SQLite column index"));
        }
        Ok(handle)
    }

    fn bytes(&mut self, index: c_int, text: bool) -> io::Result<&[u8]> {
        let api = self.0.db.api;
        let handle = self.column(index)?;
        // SAFETY: The column exists and length is requested after selecting the column's format.
        let (data, len) = unsafe {
            let data = if text {
                (api.sqlite3_column_text)(handle, index)
            } else {
                (api.sqlite3_column_blob)(handle, index).cast()
            };
            (data, (api.sqlite3_column_bytes)(handle, index) as usize)
        };
        if len == 0 {
            return Ok(&[]);
        }
        if data.is_null() {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "missing SQLite value",
            ));
        }
        // SAFETY: SQLite owns len bytes until step/finalize or a column conversion. The
        // mutable row borrow prevents those calls while the returned slice is live.
        Ok(unsafe { std::slice::from_raw_parts(data, len) })
    }
}

fn bind_length(len: usize) -> io::Result<c_int> {
    c_int::try_from(len)
        .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "SQLite parameter is too large"))
}

fn check(code: c_int, operation: &str) -> io::Result<()> {
    if code != OK {
        return Err(sqlite_error(code, operation));
    }
    Ok(())
}

fn sqlite_error(code: c_int, operation: &str) -> io::Error {
    let kind = match code & 0xff {
        3 | 8 | 23 => ErrorKind::PermissionDenied,
        5 | 6 => ErrorKind::WouldBlock,
        11 | 26 => ErrorKind::InvalidData,
        13 => ErrorKind::StorageFull,
        _ => ErrorKind::Other,
    };
    // Raw SQLite messages can contain parts of the schema or SQL values.
    io::Error::new(kind, format!("SQLite {operation} failed (code {code})"))
}
