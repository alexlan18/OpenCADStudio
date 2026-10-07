//! Shared native HTTP client configuration.
//!
//! Desktop requests use the operating system's certificate verifier so roots
//! installed by administrators, corporate proxies, and security software are
//! honoured without weakening TLS verification.

#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;
use ureq::tls::{RootCerts, TlsConfig};

pub(crate) fn agent(timeout: Duration) -> ureq::Agent {
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .tls_config(tls)
        .build()
        .into()
}

/// Like [`agent`], but 4xx/5xx answers come back as responses instead of
/// errors, so the caller can read the body: APIs put the actual reason
/// ("invalid api key", "model not found") there.
pub(crate) fn agent_keeping_status_bodies(timeout: Duration) -> ureq::Agent {
    let tls = TlsConfig::builder()
        .root_certs(RootCerts::PlatformVerifier)
        .build();
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .tls_config(tls)
        .build()
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_uses_platform_verifier_without_disabling_tls() {
        let agent = agent(Duration::from_secs(1));
        let tls = agent.config().tls_config();

        assert!(matches!(tls.root_certs(), &RootCerts::PlatformVerifier));
        assert!(!tls.disable_verification());
    }
}
