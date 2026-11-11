use anyhow::Result;
use axum_extra::extract::cookie::{Cookie, SameSite};

pub const REFRESH_TOKEN_EXPIRY_DAYS: i64 = 7;
pub const REFRESH_COOKIE_NAME: &str = "broccoli_refresh";

pub fn construct_refresh_token(selector: &str, validator: &str) -> String {
    format!("{}:{}", selector, validator)
}

pub fn parse_refresh_token(refresh_token: &str) -> Result<(&str, &str)> {
    refresh_token
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("Invalid refresh token format"))
}

/// Cookie carrying an opaque `selector:validator` refresh token.
pub fn build_refresh_cookie(refresh_token: &str, secure: bool) -> Cookie<'static> {
    Cookie::build((REFRESH_COOKIE_NAME, refresh_token.to_owned()))
        .http_only(true)
        .secure(secure)
        .same_site(if secure {
            SameSite::Strict
        } else {
            SameSite::Lax
        })
        .path("/")
        .max_age(time::Duration::days(REFRESH_TOKEN_EXPIRY_DAYS))
        .build()
}

pub fn build_removal_cookie(secure: bool) -> Cookie<'static> {
    let mut cookie = Cookie::build((REFRESH_COOKIE_NAME, ""))
        .http_only(true)
        .secure(secure)
        .same_site(if secure {
            SameSite::Strict
        } else {
            SameSite::Lax
        })
        .path("/")
        .build();
    cookie.make_removal();
    cookie
}
