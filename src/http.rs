//! Shared HTTP client with TLS configured correctly.
//!
//! Uses native-tls (SecureTransport on macOS, OpenSSL on Linux) with
//! `RootCerts::PlatformVerifier`, which delegates certificate trust entirely
//! to the OS trust store. The `native-tls` feature still compiles in the
//! `webpki-root-certs` bundle (ureq hardwires the dep), but we never load
//! it — `PlatformVerifier` bypasses it at runtime.

use std::sync::OnceLock;

use ureq::middleware::MiddlewareNext;
use ureq::{Body, SendBody};

static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

fn log_middleware(
    req: ureq::http::Request<SendBody>,
    next: MiddlewareNext,
) -> Result<ureq::http::Response<Body>, ureq::Error> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    crate::log::debug(&format!("http {} {}", method, uri));
    let res = next.handle(req)?;
    crate::log::debug(&format!(
        "http {} {} → {} content-type: {}",
        method,
        uri,
        res.status(),
        res.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
    ));
    Ok(res)
}

/// Get a shared ureq agent with native-tls. Status codes are NOT treated
/// as errors, so callers can read the response body on 4xx/5xx.
pub fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .root_certs(ureq::tls::RootCerts::PlatformVerifier)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .user_agent("nerv/1.0.0")
            .middleware(log_middleware)
            .build()
            .new_agent()
    })
}
