//! Streaming WinHTTP GET requests for use on a worker thread.

use std::cell::UnsafeCell;
use std::error;
use std::ffi::c_void;
use std::fmt;
use std::ptr::{self, NonNull, null, null_mut};
use std::slice;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError};
use windows_sys::Win32::Networking::WinHttp::{
    ERROR_WINHTTP_HEADER_NOT_FOUND, ERROR_WINHTTP_TIMEOUT, ICU_REJECT_USERPWD, URL_COMPONENTS,
    WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_ASYNC_RESULT,
    WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE, WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
    WINHTTP_CALLBACK_STATUS_HANDLE_CREATED, WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE,
    WINHTTP_CALLBACK_STATUS_READ_COMPLETE, WINHTTP_CALLBACK_STATUS_REQUEST_ERROR,
    WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE, WINHTTP_DISABLE_AUTHENTICATION,
    WINHTTP_DISABLE_COOKIES, WINHTTP_FLAG_ASYNC, WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTP,
    WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPTION_CONTEXT_VALUE, WINHTTP_OPTION_DISABLE_FEATURE,
    WINHTTP_QUERY_CUSTOM, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen,
    WinHttpOpenRequest, WinHttpQueryDataAvailable, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetOption, WinHttpSetStatusCallback,
    WinHttpSetTimeouts,
};

const CANCEL_POLL: Duration = Duration::from_millis(50);
const BUFFER_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Canceled,
    Deadline,
    Timeout,
    WinHttp(u32),
    InvalidInput,
    InvalidResponse,
}

impl Error {
    fn native(code: u32) -> Self {
        match code {
            ERROR_WINHTTP_TIMEOUT => Self::Timeout,
            code => Self::WinHttp(code),
        }
    }

    fn last() -> Self {
        // SAFETY: Called immediately following a failed WinHTTP function on this thread.
        Self::native(unsafe { GetLastError() })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WinHttp(code) => write!(f, "WinHTTP error {code}"),
            Self::Canceled => f.write_str("HTTP request canceled"),
            Self::Deadline => f.write_str("HTTP deadline expired"),
            Self::Timeout => f.write_str("HTTP request stalled or timed out"),
            Self::InvalidInput => f.write_str("invalid HTTP URL or header"),
            Self::InvalidResponse => f.write_str("invalid WinHTTP response or request state"),
        }
    }
}

impl error::Error for Error {}

/// Absolute deadline, connection timeout, time allowed for the next response data, and cancellation.
#[derive(Clone, Copy)]
pub struct RequestLimits<'a> {
    pub deadline: Instant,
    pub connect_timeout: Duration,
    pub idle_timeout: Duration,
    pub cancel: &'a AtomicBool,
}

impl RequestLimits<'_> {
    pub fn check(self) -> Result<(), Error> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Canceled);
        }
        if Instant::now() >= self.deadline {
            return Err(Error::Deadline);
        }
        Ok(())
    }
}

pub struct Client(Handle);

impl Client {
    /// Uses Windows proxy resolution and the standard Windows TLS certificate checks.
    pub fn new(user_agent: &str) -> Result<Self, Error> {
        let agent = wide(user_agent)?;
        // SAFETY: agent is NUL-terminated. Automatic proxy mode needs no explicit proxy strings.
        let session = unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                null(),
                null(),
                WINHTTP_FLAG_ASYNC,
            )
        };
        Ok(Self(Handle::new(session)?))
    }

    /// Returns the response after its headers arrive. Use public headers such as Accept or Range.
    pub fn get<'a>(
        &'a self,
        url: &str,
        headers: &[(&str, &str)],
        limits: RequestLimits<'a>,
    ) -> Result<Response<'a>, Error> {
        limits.check()?;
        let url = Url::parse(url)?;
        // SAFETY: Host is NUL-terminated and self owns the live session.
        let connection =
            Handle::new(unsafe { WinHttpConnect(self.0.raw(), url.host.as_ptr(), url.port, 0) })?;
        // SAFETY: The connection is live, object name is NUL-terminated, and optional inputs are null.
        let handle = Handle::new(unsafe {
            WinHttpOpenRequest(
                connection.raw(),
                null(), // default method is GET
                url.object.as_ptr(),
                null(),
                null(),
                null(),
                url.flags,
            )
        })?;

        let connect = milliseconds(limits.connect_timeout);
        let receive = milliseconds(limits.idle_timeout);
        // SAFETY: handle is live and all timeout values are finite positive milliseconds.
        check(unsafe { WinHttpSetTimeouts(handle.raw(), connect, connect, connect, receive) })?;
        let disabled = WINHTTP_DISABLE_COOKIES | WINHTTP_DISABLE_AUTHENTICATION;
        handle.option(WINHTTP_OPTION_DISABLE_FEATURE, &disabled)?;
        for &(name, value) in headers {
            if name.is_empty() || name.contains([':', '\r', '\n']) || value.contains(['\r', '\n']) {
                return Err(Error::InvalidInput);
            }
            let header = wide(&format!("{name}: {value}"))?;
            // SAFETY: handle is live and this synchronous call copies the NUL-terminated header.
            check(unsafe {
                WinHttpAddRequestHeaders(
                    handle.raw(),
                    header.as_ptr(),
                    u32::MAX,
                    WINHTTP_ADDREQ_FLAG_ADD,
                )
            })?;
        }

        let request = Request::new(handle)?;
        limits.check()?;
        // SAFETY: request and callback context are live. There is no body or extra header buffer.
        check(unsafe {
            WinHttpSendRequest(
                request.raw(),
                null(),
                0,
                null(),
                0,
                0,
                request.context_value(),
            )
        })?;
        request.expect(limits, limits.connect_timeout, Completion::Sent)?;
        // SAFETY: The preceding send completed and the reserved pointer is null.
        check(unsafe { WinHttpReceiveResponse(request.raw(), null_mut()) })?;
        request.expect(limits, limits.idle_timeout, Completion::Headers)?;

        let mut status = 0u32;
        let mut bytes = size_of::<u32>() as u32;
        // SAFETY: Headers are ready, status is writable, and bytes is its size in bytes.
        check(unsafe {
            WinHttpQueryHeaders(
                request.raw(),
                WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
                null(),
                ptr::from_mut(&mut status).cast(),
                &mut bytes,
                null_mut(),
            )
        })?;
        Ok(Response {
            request: Some(request),
            _connection: connection,
            _client: self,
            limits,
            status: u16::try_from(status).map_err(|_| Error::InvalidResponse)?,
            eof: false,
        })
    }
}

pub struct Response<'a> {
    request: Option<Request>,
    _connection: Handle,
    _client: &'a Client,
    limits: RequestLimits<'a>,
    status: u16,
    eof: bool,
}

impl Response<'_> {
    pub fn status(&self) -> u16 {
        self.status
    }

    pub fn header(&self, name: &str) -> Result<Option<String>, Error> {
        let request = self.request.as_ref().ok_or(Error::InvalidResponse)?;
        let name = wide(name)?;
        let mut bytes = 0;
        // SAFETY: A null output buffer queries the required size. The name and length are valid.
        let result = unsafe {
            WinHttpQueryHeaders(
                request.raw(),
                WINHTTP_QUERY_CUSTOM,
                name.as_ptr(),
                null_mut(),
                &mut bytes,
                null_mut(),
            )
        };
        if result != 0 {
            return Ok(Some(String::new()));
        }
        match Error::last() {
            Error::WinHttp(ERROR_WINHTTP_HEADER_NOT_FOUND) => return Ok(None),
            Error::WinHttp(ERROR_INSUFFICIENT_BUFFER) => {}
            error => return Err(error),
        }
        let mut value = vec![0u16; (bytes as usize).div_ceil(size_of::<u16>())];
        // SAFETY: The output is allocated for the reported number of bytes including its NUL.
        check(unsafe {
            WinHttpQueryHeaders(
                request.raw(),
                WINHTTP_QUERY_CUSTOM,
                name.as_ptr(),
                value.as_mut_ptr().cast(),
                &mut bytes,
                null_mut(),
            )
        })?;
        let value = value
            .get(..bytes as usize / size_of::<u16>())
            .ok_or(Error::InvalidResponse)?;
        Ok(Some(
            String::from_utf16(value).map_err(|_| Error::InvalidResponse)?,
        ))
    }

    /// Reads the next available bytes. An empty slice indicates EOF. An error closes the request.
    pub fn read_chunk(&mut self) -> Result<&[u8], Error> {
        if self.eof {
            return Ok(&[]);
        }
        let request = self.request.as_ref().ok_or(Error::InvalidResponse)?;
        let buffer = request.buffer();
        let length = match request.read(self.limits) {
            Ok(length) => length,
            Err(error) => {
                self.request = None;
                return Err(error);
            }
        };
        self.eof = length == 0;
        // SAFETY: A read completion initializes length bytes. The buffer is owned by the live
        // request, and another read or dropping the request requires the mutable borrow to end.
        Ok(unsafe { slice::from_raw_parts(buffer, length) })
    }
}

struct Handle(NonNull<c_void>);

impl Handle {
    fn new(raw: *mut c_void) -> Result<Self, Error> {
        NonNull::new(raw).map(Self).ok_or_else(Error::last)
    }

    fn raw(&self) -> *mut c_void {
        self.0.as_ptr()
    }

    fn option<T>(&self, option: u32, value: &T) -> Result<(), Error> {
        // SAFETY: Only plain WinHTTP option values are used with their documented option IDs.
        check(unsafe {
            WinHttpSetOption(
                self.raw(),
                option,
                ptr::from_ref(value).cast(),
                size_of::<T>() as u32,
            )
        })
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: Each Handle owns exactly one live WinHTTP handle. Async teardown is permitted.
        unsafe { WinHttpCloseHandle(self.raw()) };
    }
}

struct Url {
    host: Vec<u16>,
    object: Vec<u16>,
    port: u16,
    flags: u32,
}

impl Url {
    fn parse(url: &str) -> Result<Self, Error> {
        let url = wide(url.split_once('#').map_or(url, |(url, _)| url))?;
        // SAFETY: URL_COMPONENTS can be initialized with null pointers and zero integers.
        let mut parts: URL_COMPONENTS = unsafe { std::mem::zeroed() };
        parts.dwStructSize = size_of::<URL_COMPONENTS>() as u32;
        parts.dwHostNameLength = 1;
        parts.dwUrlPathLength = 1;
        parts.dwExtraInfoLength = 1;
        // SAFETY: url is NUL-terminated and parts requests borrowed pointers into url.
        check(unsafe { WinHttpCrackUrl(url.as_ptr(), 0, ICU_REJECT_USERPWD, &mut parts) })?;
        let flags = match parts.nScheme {
            WINHTTP_INTERNET_SCHEME_HTTP => 0,
            WINHTTP_INTERNET_SCHEME_HTTPS => WINHTTP_FLAG_SECURE,
            _ => return Err(Error::InvalidInput),
        };
        if parts.dwHostNameLength == 0 || parts.nPort == 0 {
            return Err(Error::InvalidInput);
        }
        let component = |pointer, len: u32| {
            if len == 0 {
                return &[][..];
            }
            // SAFETY: On success, each requested nonempty component is within the live url buffer.
            unsafe { slice::from_raw_parts(pointer, len as usize) }
        };
        let path = component(parts.lpszUrlPath, parts.dwUrlPathLength);
        let path = if path.is_empty() {
            &[u16::from(b'/')]
        } else {
            path
        };
        Ok(Self {
            host: component(parts.lpszHostName, parts.dwHostNameLength)
                .iter()
                .copied()
                .chain([0])
                .collect(),
            object: path
                .iter()
                .chain(component(parts.lpszExtraInfo, parts.dwExtraInfoLength))
                .copied()
                .chain([0])
                .collect(),
            port: parts.nPort,
            flags,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Completion {
    Sent,
    Headers,
    Available(u32),
    Read(u32),
    Failed(Error),
}

/// WinHTTP owns the box after the callback is registered. Only the final closing callback frees it.
struct CallbackContext {
    events: Sender<Completion>,
    buffer: UnsafeCell<[u8; BUFFER_SIZE]>,
}

struct Request {
    handle: Handle,
    context: NonNull<CallbackContext>,
    events: Receiver<Completion>,
}

impl Request {
    fn new(handle: Handle) -> Result<Self, Error> {
        let (sender, events) = mpsc::channel();
        let context = Box::new(CallbackContext {
            events: sender,
            buffer: UnsafeCell::new([0; BUFFER_SIZE]),
        });
        let context = NonNull::from(Box::leak(context));
        let value = context.as_ptr().expose_provenance();
        let register = || {
            handle.option(WINHTTP_OPTION_CONTEXT_VALUE, &value)?;
            // SAFETY: The handle and context are live. No request is in flight yet.
            let previous = unsafe {
                WinHttpSetStatusCallback(
                    handle.raw(),
                    Some(callback),
                    WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE
                        | WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE
                        | WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE
                        | WINHTTP_CALLBACK_STATUS_READ_COMPLETE
                        | WINHTTP_CALLBACK_STATUS_REQUEST_ERROR
                        | WINHTTP_CALLBACK_STATUS_HANDLE_CREATED
                        | WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING,
                    0,
                )
            };
            if previous.is_some_and(|callback| callback as usize == usize::MAX) {
                return Err(Error::last());
            }
            Ok(())
        };
        if let Err(error) = register() {
            // SAFETY: Callback registration failed, so only this thread can free the box.
            unsafe { drop(Box::from_raw(context.as_ptr())) };
            return Err(error);
        }
        Ok(Self {
            handle,
            context,
            events,
        })
    }

    fn raw(&self) -> *mut c_void {
        self.handle.raw()
    }

    fn context_value(&self) -> usize {
        self.context.as_ptr().expose_provenance()
    }

    fn buffer(&self) -> *mut u8 {
        // SAFETY: The live handle owns the context. UnsafeCell allows WinHTTP to write the buffer.
        unsafe { (*self.context.as_ptr()).buffer.get() }.cast()
    }

    fn wait(&self, limits: RequestLimits<'_>, timeout: Duration) -> Result<Completion, Error> {
        let started = Instant::now();
        loop {
            limits.check()?;
            let remaining = timeout
                .checked_sub(started.elapsed())
                .filter(|duration| !duration.is_zero())
                .ok_or(Error::Timeout)?;
            let interval = remaining
                .min(limits.deadline.saturating_duration_since(Instant::now()))
                .min(CANCEL_POLL);
            match self.events.recv_timeout(interval) {
                Ok(Completion::Failed(error)) => {
                    limits.check()?;
                    return Err(error);
                }
                Ok(event) => {
                    limits.check()?;
                    return Ok(event);
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Err(Error::InvalidResponse),
            }
        }
    }

    fn expect(
        &self,
        limits: RequestLimits<'_>,
        timeout: Duration,
        expected: Completion,
    ) -> Result<(), Error> {
        if self.wait(limits, timeout)? != expected {
            return Err(Error::InvalidResponse);
        }
        Ok(())
    }

    fn read(&self, limits: RequestLimits<'_>) -> Result<usize, Error> {
        limits.check()?;
        // SAFETY: The response headers are ready. Async completion returns the size via callback.
        check(unsafe { WinHttpQueryDataAvailable(self.raw(), null_mut()) })?;
        let Completion::Available(available) = self.wait(limits, limits.idle_timeout)? else {
            return Err(Error::InvalidResponse);
        };

        // WinHttpReadData tries to fill its buffer. Read only available bytes to save a prefix
        // before the server stalls. A one-byte read when available is zero confirms EOF.
        let count = available.clamp(1, BUFFER_SIZE as u32);
        // SAFETY: The buffer is writable for count bytes and remains owned by the callback context
        // through completion or cancellation. Only one operation on this request is in flight.
        check(unsafe { WinHttpReadData(self.raw(), self.buffer().cast(), count, null_mut()) })?;
        match self.wait(limits, limits.idle_timeout)? {
            Completion::Read(read) if read <= count => Ok(read as usize),
            _ => Err(Error::InvalidResponse),
        }
    }
}

unsafe extern "system" fn callback(
    _handle: *mut c_void,
    context: usize,
    status: u32,
    details: *mut c_void,
    length: u32,
) {
    if context == 0 {
        return;
    }
    let context = ptr::with_exposed_provenance_mut::<CallbackContext>(context);
    let event = match status {
        WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING => {
            // SAFETY: HANDLE_CLOSING is the last callback. WinHTTP no longer uses the context or
            // read buffer, even if the caller abandoned an in-flight operation.
            unsafe { drop(Box::from_raw(context)) };
            return;
        }
        WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => Completion::Sent,
        WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => Completion::Headers,
        WINHTTP_CALLBACK_STATUS_DATA_AVAILABLE => {
            // SAFETY: For DATA_AVAILABLE, details points to a DWORD for this callback.
            Completion::Available(unsafe { details.cast::<u32>().read_unaligned() })
        }
        WINHTTP_CALLBACK_STATUS_READ_COMPLETE => Completion::Read(length),
        WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
            // SAFETY: For REQUEST_ERROR, details points to a WINHTTP_ASYNC_RESULT for this callback.
            let result = unsafe { details.cast::<WINHTTP_ASYNC_RESULT>().read_unaligned() };
            Completion::Failed(Error::native(result.dwError))
        }
        _ => return,
    };
    // SAFETY: Before the closing callback, the sender is alive. The read buffer is separate.
    let _ = unsafe { (*context).events.send(event) };
}

fn wide(text: &str) -> Result<Vec<u16>, Error> {
    if text.contains('\0') {
        return Err(Error::InvalidInput);
    }
    Ok(text.encode_utf16().chain([0]).collect())
}

fn milliseconds(duration: Duration) -> i32 {
    duration.as_millis().clamp(1, i32::MAX as u128) as i32
}

fn check(result: i32) -> Result<(), Error> {
    if result == 0 {
        Err(Error::last())
    } else {
        Ok(())
    }
}
