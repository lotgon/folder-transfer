//! C-ABI surface for the cdylib (`ft.dll` / `libft.so`), callable from .NET
//! (P/Invoke), C, C++, etc. All strings are UTF-8, null-terminated. Functions
//! return 0 on success and non-zero on error; `ft_last_error` fetches the message
//! for the current thread.
//!
//! Typical use (source pushes, destination pulls):
//!   void* h = ft_serve_start("D:/data", 8722, 4, NULL, 0, 1, tok, 64, fp, 128);
//!   // hand `tok` + `fp` (+ your IP/port) to the other machine, which calls:
//!   ft_get("10.0.0.1", 8722, tok, fp, "E:/incoming", NULL, 0);
//!   ft_serve_wait(h);   // blocks until that client finished; frees the handle

use std::cell::RefCell;
use std::os::raw::{c_char, c_void};
use std::ffi::CStr;

use crate::server::ServeConfig;

thread_local! {
    static LAST_ERR: RefCell<String> = const { RefCell::new(String::new()) };
}

fn set_err(e: impl ToString) {
    LAST_ERR.with(|c| *c.borrow_mut() = e.to_string());
}

/// Borrow a required UTF-8 C string; `None` (with an error set) if null/invalid.
unsafe fn cstr_req(p: *const c_char, what: &str) -> Option<String> {
    if p.is_null() {
        set_err(format!("{what} must not be null"));
        return None;
    }
    match CStr::from_ptr(p).to_str() {
        Ok(s) => Some(s.to_string()),
        Err(_) => {
            set_err(format!("{what} is not valid UTF-8"));
            None
        }
    }
}

/// Borrow an optional C string; null or empty -> `None`.
unsafe fn cstr_opt(p: *const c_char) -> Option<String> {
    if p.is_null() {
        return None;
    }
    match CStr::from_ptr(p).to_str() {
        Ok(s) if !s.is_empty() => Some(s.to_string()),
        _ => None,
    }
}

/// Copy `s` (+ NUL) into a caller buffer, truncating to `len`.
unsafe fn copy_out(s: &str, buf: *mut c_char, len: usize) {
    if buf.is_null() || len == 0 {
        return;
    }
    let bytes = s.as_bytes();
    let n = bytes.len().min(len - 1);
    std::ptr::copy_nonoverlapping(bytes.as_ptr(), buf as *mut u8, n);
    *buf.add(n) = 0;
}

/// Download from a server into `to_folder`. Returns 0 on success.
/// `ignore` may be null; `streams` <= 0 lets the server choose the mode.
///
/// # Safety
/// All non-null pointers must be valid, NUL-terminated UTF-8 strings.
#[no_mangle]
pub unsafe extern "C" fn ft_get(
    server: *const c_char,
    port: u16,
    token: *const c_char,
    fingerprint: *const c_char,
    to_folder: *const c_char,
    ignore: *const c_char,
    streams: i32,
) -> i32 {
    let Some(server) = cstr_req(server, "server") else { return -1 };
    let token = cstr_opt(token).unwrap_or_default();
    let Some(fingerprint) = cstr_req(fingerprint, "fingerprint") else { return -1 };
    let Some(to) = cstr_req(to_folder, "to_folder") else { return -1 };
    let ignore_override = cstr_opt(ignore);
    let streams_override = if streams > 0 { Some(streams) } else { None };

    match crate::client::run(&server, port, &token, &to, &fingerprint, ignore_override, streams_override) {
        Ok(_) => 0,
        Err(e) => {
            set_err(e);
            -1
        }
    }
}

/// Background server handle returned by `ft_serve_start`.
struct ServeHandle {
    join: std::thread::JoinHandle<Result<(), String>>,
}

/// Start serving `folder` on a background thread and return a handle (null on
/// error). The freshly minted token and certificate fingerprint are written to
/// the caller's buffers so they can be handed to the receiver's `ft_get`.
/// With `once != 0` the server exits after one client finishes.
///
/// # Safety
/// Pointers must be valid; the out buffers must have the given capacities.
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub unsafe extern "C" fn ft_serve_start(
    folder: *const c_char,
    port: u16,
    streams: i32,
    ignore: *const c_char,
    no_compress: i32,
    once: i32,
    out_token: *mut c_char,
    out_token_len: usize,
    out_fingerprint: *mut c_char,
    out_fingerprint_len: usize,
) -> *mut c_void {
    let Some(folder) = cstr_req(folder, "folder") else { return std::ptr::null_mut() };
    let ignore_spec = cstr_opt(ignore).unwrap_or_default();
    let streams = if streams < 1 { 1 } else { streams };

    let identity = match crate::tls::make_server_identity() {
        Ok(id) => id,
        Err(e) => {
            set_err(e);
            return std::ptr::null_mut();
        }
    };
    let token = crate::token::generate();
    copy_out(&token, out_token, out_token_len);
    copy_out(&identity.fingerprint, out_fingerprint, out_fingerprint_len);

    let cfg = ServeConfig {
        folders: vec![folder],
        port,
        idle_seconds: 600,
        stall_timeout: 300,
        once: once != 0,
        cutover: false,
        use_compress: no_compress == 0,
        ignore_spec,
        allow_ip: None,
    };

    let join = std::thread::spawn(move || {
        let r = if streams > 1 {
            crate::server::run_serve_parallel(cfg, &identity, &token, streams)
        } else {
            crate::server::run_serve_single(cfg, &identity, &token)
        };
        r.map_err(|e| e.to_string())
    });
    Box::into_raw(Box::new(ServeHandle { join })) as *mut c_void
}

/// Wait for a server started by `ft_serve_start` to finish, then free the handle.
/// Returns 0 on a clean finish, non-zero otherwise.
///
/// # Safety
/// `handle` must come from `ft_serve_start` and be used only once.
#[no_mangle]
pub unsafe extern "C" fn ft_serve_wait(handle: *mut c_void) -> i32 {
    if handle.is_null() {
        set_err("handle must not be null");
        return -1;
    }
    let h = Box::from_raw(handle as *mut ServeHandle);
    match h.join.join() {
        Ok(Ok(())) => 0,
        Ok(Err(e)) => {
            set_err(e);
            -1
        }
        Err(_) => {
            set_err("server thread panicked");
            -2
        }
    }
}

/// Copy the current thread's last error message into `buf` (UTF-8, NUL-terminated).
/// Returns the message's byte length (which may exceed `len` if truncated).
///
/// # Safety
/// `buf` must point to at least `len` writable bytes (or be null to just query length).
#[no_mangle]
pub unsafe extern "C" fn ft_last_error(buf: *mut c_char, len: usize) -> i32 {
    LAST_ERR.with(|c| {
        let s = c.borrow();
        copy_out(&s, buf, len);
        s.len() as i32
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    // End-to-end through the C ABI: start a server, pull from it, verify.
    #[test]
    fn ffi_round_trip() {
        let dir = std::env::temp_dir().join(format!("ft_ffi_{}", std::process::id()));
        let src = dir.join("Share");
        let dst = dir.join("dst");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("a.txt"), b"hello ffi").unwrap();
        std::fs::write(src.join("sub/b.bin"), vec![7u8; 50_000]).unwrap();

        let folder = CString::new(src.to_string_lossy().to_string()).unwrap();
        let mut tok = [0i8; 64];
        let mut fp = [0i8; 128];
        let h = unsafe {
            ft_serve_start(
                folder.as_ptr(), 18999, 1, std::ptr::null(), 0, 1,
                tok.as_mut_ptr(), tok.len(), fp.as_mut_ptr(), fp.len(),
            )
        };
        assert!(!h.is_null(), "serve_start failed");
        let token = unsafe { CStr::from_ptr(tok.as_ptr()) }.to_str().unwrap().to_string();
        let finger = unsafe { CStr::from_ptr(fp.as_ptr()) }.to_str().unwrap().to_string();
        assert_eq!(finger.len(), 64);

        let server = CString::new("127.0.0.1").unwrap();
        let ctoken = CString::new(token).unwrap();
        let cfinger = CString::new(finger).unwrap();
        let cto = CString::new(dst.to_string_lossy().to_string()).unwrap();
        let rc = unsafe {
            ft_get(server.as_ptr(), 18999, ctoken.as_ptr(), cfinger.as_ptr(), cto.as_ptr(), std::ptr::null(), 1)
        };
        assert_eq!(rc, 0, "ft_get failed");
        assert_eq!(unsafe { ft_serve_wait(h) }, 0, "serve_wait failed");

        assert_eq!(std::fs::read(dst.join("Share/a.txt")).unwrap(), b"hello ffi");
        assert_eq!(std::fs::read(dst.join("Share/sub/b.bin")).unwrap(), vec![7u8; 50_000]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
