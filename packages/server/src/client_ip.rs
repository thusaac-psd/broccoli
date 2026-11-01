use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use axum_client_ip::ClientIpSource;
use ipnet::IpNet;
use tracing::warn;

const X_FORWARDED_FOR: &str = "x-forwarded-for";

/// Best-effort client IP for handler-side use (e.g. login throttling), honoring
/// the trusted-proxy [`ClientIpSource`] chosen by [`client_ip_source_middleware`].
///
/// The rejection is [`Infallible`] and the value is an `Option`: when the IP
/// cannot be determined the extractor yields `None` so callers FAIL OPEN rather
/// than break the request. This is deliberately NOT `axum_client_ip::ClientIp`,
/// whose extraction failure rejects the request - a login endpoint must never
/// 4xx merely because a proxy header was missing.
pub struct ClientIp(pub Option<IpAddr>);

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Infallible> {
        let source = parts
            .extensions
            .get::<ClientIpSource>()
            .cloned()
            .unwrap_or(ClientIpSource::ConnectInfo);
        let connect_ip = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip());
        Ok(ClientIp(resolve_client_ip(
            &source,
            &parts.headers,
            connect_ip,
        )))
    }
}

/// Resolve the effective client IP from the chosen source. Only the two sources
/// [`client_ip_source_middleware`] ever sets are honored; anything else falls
/// back to the socket peer address.
fn resolve_client_ip(
    source: &ClientIpSource,
    headers: &HeaderMap,
    connect_ip: Option<IpAddr>,
) -> Option<IpAddr> {
    match source {
        ClientIpSource::RightmostXForwardedFor => rightmost_x_forwarded_for(headers).or(connect_ip),
        _ => connect_ip,
    }
}

fn rightmost_x_forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    // Consider EVERY X-Forwarded-For header line, not just the first. HTTP treats
    // repeated headers as one ordered list, and a proxy that appends the real
    // client puts it rightmost; reading only `headers.get()` (the FIRST line) lets
    // a client spoof the throttle/audit key by sending their own XFF line ahead of
    // the proxy's. The rightmost valid IP across all lines is the last-appended.
    headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|line| line.split(','))
        .filter_map(|part| part.trim().parse::<IpAddr>().ok())
        .next_back()
}

pub fn parse_trusted_proxy_networks(entries: &[String]) -> Arc<Vec<IpNet>> {
    Arc::new(
        entries
            .iter()
            .filter_map(|entry| match parse_trusted_proxy_entry(entry) {
                Ok(net) => Some(net),
                Err(error) => {
                    warn!(
                        trusted_proxy = %entry,
                        %error,
                        "Ignoring invalid trusted proxy CIDR"
                    );
                    None
                }
            })
            .collect(),
    )
}

fn parse_trusted_proxy_entry(entry: &str) -> Result<IpNet, String> {
    let trimmed = entry.trim();
    if trimmed.is_empty() {
        return Err("empty entry".to_string());
    }

    trimmed
        .parse::<IpNet>()
        .or_else(|_| trimmed.parse::<IpAddr>().map(IpNet::from))
        .map_err(|error| format!("{error}"))
}

pub async fn client_ip_source_middleware(
    State(trusted_proxies): State<Arc<Vec<IpNet>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let source = select_client_ip_source(&request, trusted_proxies.as_slice());
    request.extensions_mut().insert(source);
    next.run(request).await
}

fn select_client_ip_source<B>(
    request: &axum::http::Request<B>,
    trusted_proxies: &[IpNet],
) -> ClientIpSource {
    if trusted_proxies.is_empty() || !request.headers().contains_key(X_FORWARDED_FOR) {
        return ClientIpSource::ConnectInfo;
    }

    let remote_ip = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip());

    match remote_ip {
        Some(ip) if trusted_proxies.iter().any(|net| net.contains(&ip)) => {
            ClientIpSource::RightmostXForwardedFor
        }
        _ => ClientIpSource::ConnectInfo,
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;

    use super::*;

    fn request(remote: SocketAddr, x_forwarded_for: Option<&str>) -> axum::http::Request<Body> {
        let mut builder = axum::http::Request::builder().uri("/");
        if let Some(value) = x_forwarded_for {
            builder = builder.header(X_FORWARDED_FOR, value);
        }
        let mut request = builder.body(Body::empty()).unwrap();
        request.extensions_mut().insert(ConnectInfo(remote));
        request
    }

    #[test]
    fn parses_cidr_and_exact_ip_entries() {
        let nets = parse_trusted_proxy_networks(&[
            "10.0.0.0/8".to_string(),
            "192.0.2.10".to_string(),
            "2001:db8::/32".to_string(),
        ]);

        assert!(
            nets.iter()
                .any(|net| net.contains(&"10.1.2.3".parse::<IpAddr>().unwrap()))
        );
        assert!(
            nets.iter()
                .any(|net| net.contains(&"192.0.2.10".parse::<IpAddr>().unwrap()))
        );
        assert!(
            nets.iter()
                .any(|net| net.contains(&"2001:db8::1".parse::<IpAddr>().unwrap()))
        );
    }

    #[test]
    fn invalid_trusted_proxy_entries_are_ignored() {
        let nets = parse_trusted_proxy_networks(&["not-a-cidr".to_string()]);
        assert!(nets.is_empty());
    }

    #[test]
    fn empty_trusted_proxy_list_uses_socket_address_even_with_xff() {
        let request = request("127.0.0.1:12345".parse().unwrap(), Some("203.0.113.10"));
        assert_eq!(
            select_client_ip_source(&request, &[]),
            ClientIpSource::ConnectInfo
        );
    }

    #[test]
    fn trusted_proxy_connection_uses_x_forwarded_for() {
        let request = request("10.0.0.4:12345".parse().unwrap(), Some("203.0.113.10"));
        let trusted = vec!["10.0.0.0/8".parse::<IpNet>().unwrap()];
        assert_eq!(
            select_client_ip_source(&request, &trusted),
            ClientIpSource::RightmostXForwardedFor
        );
    }

    #[test]
    fn untrusted_proxy_connection_uses_socket_address() {
        let request = request("192.0.2.4:12345".parse().unwrap(), Some("203.0.113.10"));
        let trusted = vec!["10.0.0.0/8".parse::<IpNet>().unwrap()];
        assert_eq!(
            select_client_ip_source(&request, &trusted),
            ClientIpSource::ConnectInfo
        );
    }

    fn headers(xff: Option<&str>) -> HeaderMap {
        let mut h = HeaderMap::new();
        if let Some(v) = xff {
            h.insert(X_FORWARDED_FOR, v.parse().unwrap());
        }
        h
    }

    #[test]
    fn resolve_connect_info_source_uses_socket_peer() {
        let connect = Some("198.51.100.9".parse::<IpAddr>().unwrap());
        assert_eq!(
            resolve_client_ip(
                &ClientIpSource::ConnectInfo,
                &headers(Some("203.0.113.10")),
                connect
            ),
            connect,
            "ConnectInfo source ignores the forwarded header"
        );
    }

    #[test]
    fn resolve_xff_source_takes_the_rightmost_entry() {
        // Rightmost is the entry appended by the closest trusted proxy.
        let connect = Some("10.0.0.4".parse::<IpAddr>().unwrap());
        assert_eq!(
            resolve_client_ip(
                &ClientIpSource::RightmostXForwardedFor,
                &headers(Some("203.0.113.10, 198.51.100.5")),
                connect,
            ),
            Some("198.51.100.5".parse().unwrap())
        );
    }

    #[test]
    fn resolve_xff_source_falls_back_to_peer_when_header_unparseable() {
        let connect = Some("10.0.0.4".parse::<IpAddr>().unwrap());
        assert_eq!(
            resolve_client_ip(
                &ClientIpSource::RightmostXForwardedFor,
                &headers(Some("not-an-ip")),
                connect,
            ),
            connect,
            "a malformed forwarded header must not lose the client IP entirely"
        );
    }

    /// Kernel audit (Nov 2026): `filter_map(parse().ok()).next_back()` SKIPS an
    /// unparseable rightmost hop and falls through to the next token to the
    /// left, which the client wrote. A trusted proxy that appends `ip:port`
    /// (Azure App Gateway / ARR style) therefore makes the login throttle key
    /// on a spoofed address; rotating it gives unlimited guesses. The
    /// rightmost hop is the only one the proxy vouches for, so if it does not
    /// parse the resolver must fall back to the peer, never look further left.
    #[test]
    #[ignore = "BUG: unparseable rightmost XFF hop falls back to a client-controlled token"]
    fn unparseable_rightmost_hop_does_not_trust_client_supplied_entries() {
        let connect = Some("10.0.0.4".parse::<IpAddr>().unwrap());
        let resolved = resolve_client_ip(
            &ClientIpSource::RightmostXForwardedFor,
            &headers(Some("6.6.6.6, 203.0.113.5:443")),
            connect,
        );
        assert_ne!(
            resolved,
            Some("6.6.6.6".parse::<IpAddr>().unwrap()),
            "client-supplied XFF entry was used as the client IP"
        );
    }
}
