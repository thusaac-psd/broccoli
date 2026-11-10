//! The one place the server decides who the client is.
//!
//! [`client_ip_source_middleware`] resolves the client IP once per request and
//! stores it as a [`ClientIp`] extension. Every consumer that keys on client IP
//! (the governor rate limiters, the login throttle) reads that extension, so
//! they can never disagree about which address a request came from.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::HeaderMap;
use axum::http::request::Parts;
use axum::middleware::Next;
use axum::response::Response;
use axum_client_ip::ClientIpSource;
use ipnet::IpNet;
use tracing::warn;

const X_FORWARDED_FOR: &str = "x-forwarded-for";

/// The resolved client IP for this request, set by
/// [`client_ip_source_middleware`].
///
/// As an extractor the rejection is [`Infallible`] and the value is an
/// `Option`: when the IP cannot be determined callers FAIL OPEN rather than
/// break the request. This is deliberately NOT `axum_client_ip::ClientIp`,
/// whose extraction failure rejects the request - a login endpoint must never
/// 4xx merely because a proxy header was missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

impl ClientIp {
    /// The resolved IP of a request that went through the middleware, or the
    /// socket peer for one that did not (unit-tested routers).
    pub fn of<B>(request: &axum::http::Request<B>) -> Option<IpAddr> {
        Self::from_extensions(request.extensions())
    }

    fn from_extensions(extensions: &axum::http::Extensions) -> Option<IpAddr> {
        match extensions.get::<ClientIp>() {
            Some(ClientIp(ip)) => *ip,
            None => peer_ip(extensions),
        }
    }
}

impl<S> FromRequestParts<S> for ClientIp
where
    S: Send + Sync,
{
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Infallible> {
        Ok(ClientIp(ClientIp::from_extensions(&parts.extensions)))
    }
}

fn peer_ip(extensions: &axum::http::Extensions) -> Option<IpAddr> {
    extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(addr)| addr.ip())
}

fn is_trusted(ip: &IpAddr, trusted_proxies: &[IpNet]) -> bool {
    trusted_proxies.iter().any(|net| net.contains(ip))
}

/// Resolve the client IP of a request whose socket peer is `peer`.
///
/// Forwarded headers are believed only when the peer is a trusted proxy. The
/// `X-Forwarded-For` hops (all header lines, in order) are then walked
/// right-to-left: each hop that is itself a trusted proxy is skipped, and the
/// first untrusted hop is the client. If a hop cannot be parsed, the walk stops
/// at the last address it could vouch for (a trusted proxy), never at an entry
/// further left, which the client may have written. If every hop is trusted,
/// the leftmost one is the best answer.
fn resolve_client_ip(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    trusted_proxies: &[IpNet],
) -> Option<IpAddr> {
    let mut client = peer?;
    if !is_trusted(&client, trusted_proxies) {
        return Some(client);
    }
    let lines: Vec<&str> = headers
        .get_all(X_FORWARDED_FOR)
        .iter()
        .map(|value| value.to_str().unwrap_or(""))
        .collect();
    for hop in lines.iter().rev().flat_map(|line| line.rsplit(',')) {
        let Some(ip) = parse_hop(hop) else {
            break;
        };
        client = ip;
        if !is_trusted(&ip, trusted_proxies) {
            break;
        }
    }
    Some(client)
}

/// Parse one `X-Forwarded-For` hop. Besides a bare address, accept the
/// unambiguous `ip:port`, `[v6]` and `[v6]:port` forms some proxies emit.
fn parse_hop(hop: &str) -> Option<IpAddr> {
    let hop = hop.trim();
    hop.parse::<IpAddr>()
        .or_else(|_| hop.parse::<SocketAddr>().map(|addr| addr.ip()))
        .ok()
        .or_else(|| {
            hop.strip_prefix('[')
                .and_then(|h| h.strip_suffix(']'))
                .and_then(|h| h.parse::<IpAddr>().ok())
        })
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

/// Resolve the client IP once and record it (and whether the peer is a trusted
/// proxy, which also governs other forwarded headers) on the request.
pub async fn client_ip_source_middleware(
    State(trusted_proxies): State<Arc<Vec<IpNet>>>,
    mut request: Request,
    next: Next,
) -> Response {
    let peer = peer_ip(request.extensions());
    let client = resolve_client_ip(request.headers(), peer, &trusted_proxies);
    let source = select_client_ip_source(&request, &trusted_proxies);
    if source == ClientIpSource::ConnectInfo && request.headers().contains_key(X_FORWARDED_FOR) {
        warn_once_if_untrusted_private_proxy(peer);
    }
    request.extensions_mut().insert(source);
    request.extensions_mut().insert(ClientIp(client));
    next.run(request).await
}

fn select_client_ip_source<B>(
    request: &axum::http::Request<B>,
    trusted_proxies: &[IpNet],
) -> ClientIpSource {
    if !request.headers().contains_key(X_FORWARDED_FOR) {
        return ClientIpSource::ConnectInfo;
    }
    match peer_ip(request.extensions()) {
        Some(ip) if is_trusted(&ip, trusted_proxies) => ClientIpSource::RightmostXForwardedFor,
        _ => ClientIpSource::ConnectInfo,
    }
}

/// A forwarded header from a private-network peer that is not trusted usually
/// means `TRUSTED_PROXIES` is missing the gateway, so every client is keyed on
/// the gateway's IP. Say so once per process; it is not worth more than an
/// atomic load per request.
fn warn_once_if_untrusted_private_proxy(peer: Option<IpAddr>) {
    static WARNED: AtomicBool = AtomicBool::new(false);
    let Some(peer) = peer else { return };
    if !is_private(&peer) || WARNED.load(Ordering::Relaxed) || WARNED.swap(true, Ordering::Relaxed)
    {
        return;
    }
    warn!(
        peer = %peer,
        "Received X-Forwarded-For from an untrusted private-network peer; if this is your \
         reverse proxy or gateway, add it to server.trusted_proxies, otherwise every client \
         behind it shares one IP for rate limiting and login throttling"
    );
}

fn is_private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local() || v6.is_unicast_link_local(),
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

    fn headers(lines: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for line in lines {
            h.append(X_FORWARDED_FOR, line.parse().unwrap());
        }
        h
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn nets(entries: &[&str]) -> Vec<IpNet> {
        entries.iter().map(|e| e.parse().unwrap()).collect()
    }

    const PROXY: &str = "10.0.0.4";

    fn resolve(lines: &[&str], trusted: &[&str]) -> Option<IpAddr> {
        resolve_client_ip(&headers(lines), Some(ip(PROXY)), &nets(trusted))
    }

    #[test]
    fn untrusted_peer_ignores_the_forwarded_header() {
        let resolved = resolve_client_ip(
            &headers(&["203.0.113.10"]),
            Some(ip("198.51.100.9")),
            &nets(&["10.0.0.0/8"]),
        );
        assert_eq!(resolved, Some(ip("198.51.100.9")));
        assert_eq!(resolve(&["203.0.113.10"], &[]), Some(ip(PROXY)));
    }

    #[test]
    fn trusted_peer_takes_the_rightmost_untrusted_hop() {
        assert_eq!(
            resolve(&["203.0.113.10, 198.51.100.5"], &["10.0.0.0/8"]),
            Some(ip("198.51.100.5"))
        );
    }

    #[test]
    fn trusted_hops_are_skipped_right_to_left() {
        // client -> edge proxy (10.0.0.7) -> gateway (peer): the gateway
        // appends the edge proxy, which must be skipped, not used as the key.
        assert_eq!(
            resolve(&["6.6.6.6, 198.51.100.5, 10.0.0.7"], &["10.0.0.0/8"]),
            Some(ip("198.51.100.5"))
        );
        // Across header lines too: lines are one ordered list.
        assert_eq!(
            resolve(&["6.6.6.6, 198.51.100.5", "10.0.0.7"], &["10.0.0.0/8"]),
            Some(ip("198.51.100.5"))
        );
    }

    #[test]
    fn all_trusted_hops_yield_the_leftmost() {
        assert_eq!(
            resolve(&["10.0.0.8, 10.0.0.7"], &["10.0.0.0/8"]),
            Some(ip("10.0.0.8"))
        );
    }

    #[test]
    fn ports_and_brackets_are_stripped() {
        let trusted = &["10.0.0.0/8"];
        assert_eq!(
            resolve(&["203.0.113.5:443"], trusted),
            Some(ip("203.0.113.5"))
        );
        assert_eq!(
            resolve(&["[2001:db8::1]:443"], trusted),
            Some(ip("2001:db8::1"))
        );
        assert_eq!(
            resolve(&["[2001:db8::1]"], trusted),
            Some(ip("2001:db8::1"))
        );
        assert_eq!(resolve(&["2001:db8::1"], trusted), Some(ip("2001:db8::1")));
    }

    #[test]
    fn unparseable_header_falls_back_to_peer() {
        assert_eq!(resolve(&["not-an-ip"], &["10.0.0.0/8"]), Some(ip(PROXY)));
    }

    /// Kernel audit (Nov 2026, #75): an unparseable rightmost hop used to be
    /// SKIPPED, falling through to the next token to the left, which the
    /// client wrote; rotating it gave unlimited login guesses. The walk now
    /// stops at the last address it can vouch for.
    #[test]
    fn unparseable_rightmost_hop_does_not_trust_client_supplied_entries() {
        let resolved = resolve(&["6.6.6.6, unknown"], &["10.0.0.0/8"]);
        assert_eq!(resolved, Some(ip(PROXY)));
        // The audit's original input: the port is now stripped instead.
        let resolved = resolve(&["6.6.6.6, 203.0.113.5:443"], &["10.0.0.0/8"]);
        assert_eq!(resolved, Some(ip("203.0.113.5")));
        // An unparseable hop behind a trusted hop stops at that trusted hop.
        let resolved = resolve(&["6.6.6.6, _hidden, 10.0.0.7"], &["10.0.0.0/8"]);
        assert_eq!(resolved, Some(ip("10.0.0.7")));
    }

    #[tokio::test]
    async fn middleware_records_the_resolved_ip_for_every_consumer() {
        use tower::ServiceExt;
        let app = axum::Router::new()
            .route(
                "/",
                axum::routing::get(|ClientIp(ip): ClientIp| async move {
                    ip.map(|ip| ip.to_string()).unwrap_or_default()
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                parse_trusted_proxy_networks(&["10.0.0.0/8".to_string()]),
                client_ip_source_middleware,
            ));
        let req = request(
            SocketAddr::new(ip(PROXY), 1),
            Some("6.6.6.6, 203.0.113.5:443"),
        );
        let res = app.oneshot(req).await.unwrap();
        let body = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
        assert_eq!(&body[..], b"203.0.113.5");

        let bare = request("192.0.2.4:1".parse().unwrap(), None);
        assert_eq!(
            ClientIp::of(&bare),
            Some(ip("192.0.2.4")),
            "falls back to the peer"
        );
    }
}
