//! The address of a Broccoli server, in the one form every part of the CLI
//! uses: request URLs are built from it and saved credentials are keyed by it,
//! so `http://judge:3000/`, `HTTP://Judge:3000` and `http://judge:3000` are
//! the same server everywhere.

use std::fmt;
use std::str::FromStr;

/// A server's base URL: `http` or `https`, a lowercase host, the port unless
/// it is the scheme's default, and the path the server is mounted under (if
/// any) without a trailing slash. Constructed only by parsing, so an
/// unnormalised address cannot reach a request or the credentials file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerUrl(String);

impl ServerUrl {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The URL of `path`, an absolute path on the server (`/api/v1/...`).
    pub fn join(&self, path: &str) -> String {
        debug_assert!(path.starts_with('/'), "server paths are absolute: {path}");
        format!("{}{}", self.0, path)
    }
}

/// Why a server address cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServerUrlError {
    #[error("The server address is empty.")]
    Empty,
    #[error("'{0}' has no scheme. Use http://{0} or https://{0}.")]
    MissingScheme(String),
    #[error("'{input}' uses {scheme}://; a Broccoli server is reached over http:// or https://.")]
    UnsupportedScheme { input: String, scheme: String },
    #[error("'{0}' is not a server address: {1}.")]
    Invalid(String, String),
}

impl FromStr for ServerUrl {
    type Err = ServerUrlError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let input = input.trim();
        if input.is_empty() {
            return Err(ServerUrlError::Empty);
        }
        let Some((scheme, _)) = input.split_once("://") else {
            return Err(ServerUrlError::MissingScheme(input.to_string()));
        };
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => {
                return Err(ServerUrlError::UnsupportedScheme {
                    input: input.to_string(),
                    scheme,
                });
            }
        };
        let invalid = |why: &str| ServerUrlError::Invalid(input.to_string(), why.to_string());
        let uri: http::Uri = input.parse().map_err(|e: http::uri::InvalidUri| {
            ServerUrlError::Invalid(input.to_string(), e.to_string())
        })?;
        let authority = uri.authority().ok_or_else(|| invalid("it names no host"))?;
        if authority.as_str().contains('@') {
            return Err(invalid("it must not carry a user name or password"));
        }
        if uri.query().is_some() || input.contains('#') {
            return Err(invalid("it must not carry a query or fragment"));
        }
        let host = authority.host().to_ascii_lowercase();
        if host.is_empty() {
            return Err(invalid("it names no host"));
        }
        let port = match authority.port_u16() {
            Some(port) if port != default_port => format!(":{port}"),
            _ => String::new(),
        };
        let base = uri.path().trim_end_matches('/');
        Ok(Self(format!("{scheme}://{host}{port}{base}")))
    }
}

impl fmt::Display for ServerUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> String {
        s.parse::<ServerUrl>().unwrap().to_string()
    }

    #[test]
    fn spellings_of_one_server_normalise_to_one_address() {
        for spelling in [
            "http://127.0.0.1:3100",
            "http://127.0.0.1:3100/",
            "  HTTP://127.0.0.1:3100//  ",
        ] {
            assert_eq!(url(spelling), "http://127.0.0.1:3100", "{spelling}");
        }
        assert_eq!(url("https://Judge.Example:443/"), "https://judge.example");
        assert_eq!(url("http://judge:80"), "http://judge");
    }

    #[test]
    fn a_base_path_is_kept() {
        let server: ServerUrl = "https://lab.example/broccoli/".parse().unwrap();
        assert_eq!(server.as_str(), "https://lab.example/broccoli");
        assert_eq!(
            server.join("/api/v1/auth/login"),
            "https://lab.example/broccoli/api/v1/auth/login"
        );
    }

    #[test]
    fn an_address_without_a_scheme_names_the_fix() {
        let err = "10.0.0.5:3000".parse::<ServerUrl>().unwrap_err();
        assert_eq!(err, ServerUrlError::MissingScheme("10.0.0.5:3000".into()));
        assert!(err.to_string().contains("http://10.0.0.5:3000"));
    }

    #[test]
    fn unusable_addresses_are_refused() {
        assert_eq!("".parse::<ServerUrl>(), Err(ServerUrlError::Empty));
        assert!(matches!(
            "ftp://judge".parse::<ServerUrl>(),
            Err(ServerUrlError::UnsupportedScheme { .. })
        ));
        for bad in ["http://", "http://judge/?x=1", "http://u:p@judge"] {
            assert!(
                matches!(bad.parse::<ServerUrl>(), Err(ServerUrlError::Invalid(..))),
                "{bad}"
            );
        }
    }
}
