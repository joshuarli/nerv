//! Shared HTTP client with TLS configured correctly.
//!
//! Uses native-tls (SecureTransport on macOS, OpenSSL on Linux), which
//! delegates certificate trust to the system CA store. The `native-tls`
//! feature also pulls in `webpki-root-certs` as a fallback root store —
//! there's no supported way to drop that without forking ureq's connector.

use std::sync::OnceLock;

static AGENT: OnceLock<ureq::Agent> = OnceLock::new();

/// Get a shared ureq agent with native-tls. Status codes are NOT treated
/// as errors, so callers can read the response body on 4xx/5xx.
pub fn agent() -> &'static ureq::Agent {
    AGENT.get_or_init(|| {
        let tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build();
        ureq::Agent::config_builder()
            .tls_config(tls)
            .http_status_as_error(false)
            .user_agent("nerv/1.0.0")
            .build()
            .new_agent()
    })
}
