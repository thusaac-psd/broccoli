//! TLS trust config. Mode from `$BROCCOLI_TLS` (takes precedence) or the `tls`
//! config key. The default trusts the OS certificate store and the bundled
//! Mozilla (webpki) roots together, so a server whose internal CA has been
//! installed into the OS store (the air-gap contestant install does this)
//! works out of the box, and so does a public-CA server on a machine whose OS
//! store is missing or stale.

use std::sync::{Arc, Once, OnceLock};
use std::time::Duration;

use console::style;
use ureq::tls::{Certificate, RootCerts, TlsConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// OS trust store plus bundled Mozilla roots.
    Default,
    /// Bundled Mozilla roots only.
    WebPki,
    /// The OS verifier only (rustls-platform-verifier).
    Platform,
    Insecure,
}

/// TLS mode from `$BROCCOLI_TLS`, then config, else the secure default.
pub fn resolve_tls_mode() -> TlsMode {
    let raw = std::env::var("BROCCOLI_TLS")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| crate::config::load_user_config().tls);
    parse_tls_mode(raw.as_deref())
}

/// Unknown or empty values fall back to the secure default.
fn parse_tls_mode(raw: Option<&str>) -> TlsMode {
    match raw.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("webpki" | "mozilla" | "bundled") => TlsMode::WebPki,
        Some("system" | "platform" | "native" | "os") => TlsMode::Platform,
        Some("insecure" | "danger" | "danger-accept-invalid" | "no-verify" | "skip") => {
            TlsMode::Insecure
        }
        _ => TlsMode::Default,
    }
}

/// Non-2xx returns `Ok` so callers can read the error body.
pub fn build_agent(connect: Option<Duration>, global: Option<Duration>) -> ureq::Agent {
    let mut builder = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(connect)
        .timeout_global(global);

    match resolve_tls_mode() {
        TlsMode::Default => {
            builder = builder.tls_config(TlsConfig::builder().root_certs(default_roots()).build());
        }
        TlsMode::WebPki => {
            builder =
                builder.tls_config(TlsConfig::builder().root_certs(RootCerts::WebPki).build());
        }
        TlsMode::Platform => {
            builder = builder.tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            );
        }
        TlsMode::Insecure => {
            warn_insecure_once();
            builder = builder.tls_config(TlsConfig::builder().disable_verification(true).build());
        }
    }

    builder.build().into()
}

/// The OS store's roots plus the bundled Mozilla roots, loaded once per
/// process. An unreadable or empty OS store just leaves the Mozilla roots.
fn default_roots() -> RootCerts {
    static ROOTS: OnceLock<Arc<Vec<Certificate<'static>>>> = OnceLock::new();
    let roots = ROOTS.get_or_init(|| {
        let native = rustls_native_certs::load_native_certs().certs;
        Arc::new(combine_roots(native.iter().map(|c| c.as_ref())))
    });
    RootCerts::Specific(Arc::clone(roots))
}

fn combine_roots<'a>(os_roots: impl Iterator<Item = &'a [u8]>) -> Vec<Certificate<'static>> {
    webpki_root_certs::TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|c| c.as_ref())
        .chain(os_roots)
        .map(|der| Certificate::from_der(der).to_owned())
        .collect()
}

fn warn_insecure_once() {
    static WARNED: Once = Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "{} TLS certificate verification is DISABLED (BROCCOLI_TLS=insecure). \
             Your connection is not secure.",
            style("warning:").yellow().bold()
        );
    });
}

#[cfg(test)]
mod tests {
    use super::{TlsMode, combine_roots, parse_tls_mode};

    #[test]
    fn defaults_to_os_plus_mozilla_roots() {
        assert_eq!(parse_tls_mode(None), TlsMode::Default);
        assert_eq!(parse_tls_mode(Some("")), TlsMode::Default);
        assert_eq!(parse_tls_mode(Some("default")), TlsMode::Default);
        assert_eq!(parse_tls_mode(Some("nonsense")), TlsMode::Default);
    }

    #[test]
    fn explicit_webpki_keeps_only_the_bundled_roots() {
        for v in ["webpki", "WebPKI", " mozilla ", "bundled"] {
            assert_eq!(parse_tls_mode(Some(v)), TlsMode::WebPki, "{v}");
        }
    }

    #[test]
    fn recognizes_platform_synonyms_case_insensitively() {
        for v in ["system", "Platform", " NATIVE ", "os"] {
            assert_eq!(parse_tls_mode(Some(v)), TlsMode::Platform, "{v}");
        }
    }

    #[test]
    fn recognizes_insecure_synonyms() {
        for v in ["insecure", "no-verify", "skip", "DANGER"] {
            assert_eq!(parse_tls_mode(Some(v)), TlsMode::Insecure, "{v}");
        }
    }

    #[test]
    fn default_roots_keep_every_mozilla_root_and_add_os_roots() {
        let internal_ca: &[u8] = b"an internal CA from the OS store";
        let roots = combine_roots([internal_ca].into_iter());
        let mozilla = webpki_root_certs::TLS_SERVER_ROOT_CERTS.len();
        assert!(mozilla > 100, "bundled Mozilla roots missing");
        assert_eq!(roots.len(), mozilla + 1);
        assert!(roots.iter().any(|c| c.der() == internal_ca));
    }
}
