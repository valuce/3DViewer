//! Minimal HTTPS GET on top of WinHTTP (ships with Windows, so no TLS stack in the exe).
//! Uses the system proxy settings and follows redirects.

use anyhow::{Context, Result, bail};
use std::ffi::c_void;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Networking::WinHttp::*;

struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the handle came from WinHttpOpen/Connect/OpenRequest and is closed once.
            unsafe { WinHttpCloseHandle(self.0) };
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn check(ok: bool, what: &str) -> Result<()> {
    if ok { Ok(()) } else { Err(std::io::Error::last_os_error()).with_context(|| what.to_string()) }
}

fn user_agent() -> String {
    format!("3DViewer/{}", env!("CARGO_PKG_VERSION"))
}

/// GETs `url` and feeds the body to `sink` chunk by chunk together with the total size
/// (when the server sends Content-Length). Returning an error from `sink` aborts.
pub fn get(url: &str, accept: &str, mut sink: impl FnMut(&[u8], Option<u64>) -> Result<()>) -> Result<()> {
    let rest = url.strip_prefix("https://").context("only https:// URLs are supported")?;
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    // SAFETY: every pointer passed below is either null where the API allows it or points
    // to a live, NUL-terminated buffer / correctly sized out-parameter.
    unsafe {
        let session = Handle(WinHttpOpen(wide(&user_agent()).as_ptr(), WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, null(), null(), 0));
        check(!session.0.is_null(), "WinHttpOpen")?;
        let protocols = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2 | WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_3;
        if WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, (&raw const protocols).cast(), 4) == 0 {
            // Windows 10 without TLS 1.3 support rejects the flag; TLS 1.2 is enough.
            let tls12 = WINHTTP_FLAG_SECURE_PROTOCOL_TLS1_2;
            WinHttpSetOption(session.0, WINHTTP_OPTION_SECURE_PROTOCOLS, (&raw const tls12).cast(), 4);
        }
        WinHttpSetTimeouts(session.0, 10_000, 10_000, 15_000, 30_000);

        let conn = Handle(WinHttpConnect(session.0, wide(host).as_ptr(), INTERNET_DEFAULT_HTTPS_PORT, 0));
        check(!conn.0.is_null(), "WinHttpConnect")?;
        let req =
            Handle(WinHttpOpenRequest(conn.0, wide("GET").as_ptr(), wide(path).as_ptr(), null(), null(), null(), WINHTTP_FLAG_SECURE));
        check(!req.0.is_null(), "WinHttpOpenRequest")?;
        let headers = wide(&format!("Accept: {accept}\r\nX-GitHub-Api-Version: 2022-11-28\r\n"));
        WinHttpAddRequestHeaders(req.0, headers.as_ptr(), u32::MAX, WINHTTP_ADDREQ_FLAG_ADD);
        check(WinHttpSendRequest(req.0, null(), 0, null(), 0, 0, 0) != 0, "cannot reach the server")?;
        check(WinHttpReceiveResponse(req.0, null_mut()) != 0, "no response from the server")?;

        let mut status: u32 = 0;
        let mut len = 4u32;
        WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            null(),
            (&raw mut status).cast(),
            &mut len,
            null_mut(),
        );
        if status != 200 {
            bail!("HTTP {status} for {url}");
        }
        let mut total: u64 = 0;
        let mut len = 8u32;
        let has_len = WinHttpQueryHeaders(
            req.0,
            WINHTTP_QUERY_CONTENT_LENGTH | WINHTTP_QUERY_FLAG_NUMBER64,
            null(),
            (&raw mut total).cast(),
            &mut len,
            null_mut(),
        ) != 0;
        let total = has_len.then_some(total);

        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let mut read = 0u32;
            check(WinHttpReadData(req.0, buf.as_mut_ptr().cast(), buf.len() as u32, &mut read) != 0, "download interrupted")?;
            if read == 0 {
                break;
            }
            sink(&buf[..read as usize], total)?;
        }
    }
    Ok(())
}

/// GETs a small document into memory.
pub fn get_bytes(url: &str, accept: &str, limit: usize) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    get(url, accept, |chunk, _| {
        out.extend_from_slice(chunk);
        if out.len() > limit {
            bail!("response is larger than {limit} bytes");
        }
        Ok(())
    })?;
    Ok(out)
}
