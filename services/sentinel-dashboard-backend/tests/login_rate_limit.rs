//! Integration test (#474): per-IP login brute-force limiter on `POST /api/auth/login`.
//!
//! Verified via `build_app` + `tower::ServiceExt::oneshot` (no live server). The client IP is
//! injected as a `ConnectInfo<SocketAddr>` request extension (the same extension
//! `into_make_service_with_connect_info` installs in production). Time-based block expiry is
//! covered by the unit tests in `auth.rs` (injected `now`); here we only assert the HTTP behavior.

use std::net::SocketAddr;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{header, Request, StatusCode};
use sentinel_dashboard_backend::{build_app, AppState, Config};
use tower::ServiceExt;

const CORRECT_KEY: &str = "test-key";

/// Test state with a known key and a small failure threshold (fast to trip).
fn test_state() -> AppState {
    let mut config = Config::from_env();
    config.dashboard_api_key = Some(CORRECT_KEY.into());
    config.projection_db = "/nonexistent/dashboard-test-projection.db".into();
    config.events_db = "/nonexistent/dashboard-test-events.db".into();
    config.gateway_proxy_url = "http://127.0.0.1:1".into();
    config.prometheus_url = "http://127.0.0.1:1".into();
    config.login_max_fails = 3;
    config.login_window_secs = 60;
    config.login_block_secs = 300;
    AppState::new(config).unwrap()
}

fn login_req(key: &str, ip: Option<&str>) -> Request<Body> {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(format!("{{\"key\":\"{key}\"}}")))
        .unwrap();
    if let Some(ip) = ip {
        let addr: SocketAddr = format!("{ip}:40000").parse().unwrap();
        req.extensions_mut().insert(ConnectInfo(addr));
    }
    req
}

/// (status, has_set_cookie, retry_after)
async fn post_login(
    state: &AppState,
    key: &str,
    ip: Option<&str>,
) -> (StatusCode, bool, Option<String>) {
    let app = build_app(state.clone());
    let resp = app.oneshot(login_req(key, ip)).await.unwrap();
    let status = resp.status();
    let has_cookie = resp.headers().contains_key(header::SET_COOKIE);
    let retry = resp
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(String::from);
    (status, has_cookie, retry)
}

#[tokio::test]
async fn n_failures_return_401_then_429_with_retry_after() {
    let state = test_state(); // max_fails = 3
    for i in 1..=3 {
        let (status, cookie, _) = post_login(&state, "wrong", Some("10.0.0.1")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "attempt {i} -> 401");
        assert!(!cookie, "no cookie on failure");
    }
    // 4th attempt is blocked.
    let (status, cookie, retry) = post_login(&state, "wrong", Some("10.0.0.1")).await;
    assert_eq!(
        status,
        StatusCode::TOO_MANY_REQUESTS,
        "over threshold -> 429"
    );
    assert!(!cookie, "no cookie when blocked");
    let retry = retry.expect("Retry-After header present");
    let secs: u64 = retry.parse().expect("Retry-After is a number");
    assert!(secs > 0 && secs <= 300, "retry-after within block: {secs}");
}

#[tokio::test]
async fn correct_key_during_block_still_429_no_cookie() {
    let state = test_state();
    for _ in 0..3 {
        post_login(&state, "wrong", Some("10.0.0.2")).await;
    }
    // Even the CORRECT key is rejected while blocked (no oracle) and mints no session.
    let (status, cookie, _) = post_login(&state, CORRECT_KEY, Some("10.0.0.2")).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        !cookie,
        "block must not mint a session even with the right key"
    );
}

#[tokio::test]
async fn other_ip_unaffected_by_block() {
    let state = test_state();
    for _ in 0..3 {
        post_login(&state, "wrong", Some("10.0.0.3")).await;
    }
    // A different IP can still log in with the correct key.
    let (status, cookie, _) = post_login(&state, CORRECT_KEY, Some("10.0.0.4")).await;
    assert_eq!(status, StatusCode::OK, "other IP unaffected");
    assert!(cookie, "successful login sets the session cookie");
}

#[tokio::test]
async fn success_resets_counter() {
    let state = test_state();
    // 2 failures (below the threshold of 3).
    for _ in 0..2 {
        post_login(&state, "wrong", Some("10.0.0.5")).await;
    }
    // A success resets the counter...
    let (status, cookie, _) = post_login(&state, CORRECT_KEY, Some("10.0.0.5")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(cookie);
    // ...so it takes a full 3 new failures to block again (the next 2 stay 401, not 429).
    for _ in 0..2 {
        let (status, _, _) = post_login(&state, "wrong", Some("10.0.0.5")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "counter was reset");
    }
}

#[tokio::test]
async fn login_without_connect_info_does_not_500() {
    let state = test_state();
    // No ConnectInfo extension (e.g. exotic setups) -> ClientIp falls back to 0.0.0.0, no 500.
    let (status_ok, cookie_ok, _) = post_login(&state, CORRECT_KEY, None).await;
    assert_eq!(status_ok, StatusCode::OK);
    assert!(cookie_ok);
    let (status_bad, _, _) = post_login(&state, "wrong", None).await;
    assert_eq!(status_bad, StatusCode::UNAUTHORIZED);
}
