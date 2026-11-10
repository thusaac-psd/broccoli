//! Redaction for values that are safe to log only in part.

/// `url` with any password in its userinfo replaced by `***`, for logging.
///
/// Connection URLs such as `redis://:secret@host:6379` or
/// `postgres://user:secret@db/broccoli` carry credentials; logs must never.
/// The username, host, port and path are kept so the line stays useful.
/// Anything that does not look like `scheme://userinfo@...` is returned
/// unchanged.
pub fn redact_url(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let authority_start = scheme_end + 3;
    let rest = &url[authority_start..];
    let authority_len = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &rest[..authority_len];
    let Some(at) = authority.rfind('@') else {
        return url.to_string();
    };
    let userinfo = &authority[..at];
    let Some(colon) = userinfo.find(':') else {
        return url.to_string();
    };
    format!(
        "{}{}:***{}",
        &url[..authority_start],
        &userinfo[..colon],
        &url[authority_start + at..]
    )
}

#[cfg(test)]
mod tests {
    use super::redact_url;

    #[test]
    fn hides_passwords_and_keeps_the_rest() {
        assert_eq!(
            redact_url("redis://:qa_redis@127.0.0.1:6379"),
            "redis://:***@127.0.0.1:6379"
        );
        assert_eq!(
            redact_url("postgres://postgres:p%40ss@db:5432/broccoli?sslmode=disable"),
            "postgres://postgres:***@db:5432/broccoli?sslmode=disable"
        );
        assert_eq!(
            redact_url("redis://user:a:b@host/0"),
            "redis://user:***@host/0"
        );
    }

    #[test]
    fn leaves_urls_without_a_password_alone() {
        for url in [
            "redis://127.0.0.1:6379",
            "redis://user@host:6379",
            "http://host/path@with-at",
            "not a url",
        ] {
            assert_eq!(redact_url(url), url);
        }
    }
}
