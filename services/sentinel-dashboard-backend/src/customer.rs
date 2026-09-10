//! Customer-only browser sessions. Operator sessions and credentials never enter this path.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    body::Bytes,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{
    auth::{cookie_value, ClientIp},
    AppState,
};

const COOKIE: &str = "sentinel_customer_session";
const TTL: Duration = Duration::from_secs(3600);
const MAX_RESPONSE: usize = 4 * 1024 * 1024;

pub async fn no_store(request: axum::extract::Request, next: axum::middleware::Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store"),
    );
    response
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Identity {
    schema_version: u16,
    principal_id: String,
    tenant_id: String,
    customer_id: String,
}

#[derive(Clone)]
struct Session {
    credential: String,
    identity: Identity,
    expires: Instant,
}

#[derive(Clone, Default)]
pub struct CustomerSessions(Arc<Mutex<HashMap<String, Session>>>);

impl CustomerSessions {
    fn get(&self, token: Option<&str>) -> Option<Session> {
        let mut sessions = self.0.lock().ok()?;
        sessions.retain(|_, session| session.expires > Instant::now());
        sessions.get(token?).cloned()
    }
    fn revoke(&self, token: Option<&str>) {
        if let (Some(token), Ok(mut sessions)) = (token, self.0.lock()) {
            sessions.remove(token);
        }
    }
    fn create(&self, credential: String, identity: Identity) -> Option<String> {
        let mut sessions = self.0.lock().ok()?;
        sessions.retain(|_, session| session.expires > Instant::now());
        if sessions.len() >= 256 {
            return None;
        }
        let token = uuid::Uuid::new_v4().to_string();
        sessions.insert(
            token.clone(),
            Session {
                credential,
                identity,
                expires: Instant::now() + TTL,
            },
        );
        Some(token)
    }
}

fn error(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({"error": code}))).into_response()
}

fn json_request(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim() == "application/json")
}

async fn upstream(
    st: &AppState,
    credential: &str,
    path: &str,
    query: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<(StatusCode, Value), ()> {
    let base = reqwest::Url::parse(&st.config.operator_url).map_err(|_| ())?;
    if !matches!(base.host_str(), Some("127.0.0.1" | "localhost" | "[::1]"))
        || !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
    {
        return Err(());
    }
    let url = base.join(path).map_err(|_| ())?;
    let client = &st.customer_http;
    let mut request = if body.is_some() {
        client.post(url)
    } else {
        client.get(url)
    };
    request = request.bearer_auth(credential).query(query);
    if let Some(body) = body {
        request = request
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_vec());
    }
    let mut response = request.send().await.map_err(|_| ())?;
    let status = response.status();
    if status.is_redirection() {
        return Err(());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok((status, serde_json::from_slice(&bytes).map_err(|_| ())?))
}

async fn identity(st: &AppState, credential: &str) -> Result<Option<Identity>, ()> {
    let (status, value) =
        upstream(st, credential, "/customer/workflow/identity", &[], None).await?;
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        return Ok(None);
    }
    if status != StatusCode::OK {
        return Err(());
    }
    let identity: Identity = serde_json::from_value(value).map_err(|_| ())?;
    if identity.schema_version != 1
        || identity.principal_id.is_empty()
        || identity.tenant_id.is_empty()
        || identity.customer_id.is_empty()
    {
        return Err(());
    }
    Ok(Some(identity))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    key: String,
}

pub async fn login(
    State(st): State<AppState>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if st.login_limiter.blocked_secs(ip).is_some() {
        return error(StatusCode::TOO_MANY_REQUESTS, "login_rate_limited");
    }
    if body.len() > 4096 || !json_request(&headers) {
        return error(StatusCode::BAD_REQUEST, "invalid_login");
    }
    let value: Login = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_login"),
    };
    if value.key.len() < 32 || value.key.len() > 512 {
        st.login_limiter.record_failure(ip);
        return error(StatusCode::UNAUTHORIZED, "customer_authentication_failed");
    }
    let identity = match identity(&st, &value.key).await {
        Ok(Some(identity)) => identity,
        Ok(None) => {
            st.login_limiter.record_failure(ip);
            return error(StatusCode::UNAUTHORIZED, "customer_authentication_failed");
        }
        Err(()) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "customer_workflow_unavailable",
            )
        }
    };
    let Some(token) = st.customer_sessions.create(value.key, identity.clone()) else {
        return error(StatusCode::SERVICE_UNAVAILABLE, "customer_sessions_full");
    };
    st.customer_sessions
        .revoke(cookie_value(&headers, COOKIE).as_deref());
    st.login_limiter.reset(ip);
    let secure = if st.config.cookie_secure {
        "; Secure"
    } else {
        ""
    };
    let cookie = format!(
        "{COOKIE}={token}; HttpOnly; SameSite=Strict{secure}; Path=/api/customer; Max-Age=3600"
    );
    (
        [(header::SET_COOKIE, cookie)],
        Json(json!({"authenticated": true, "identity": identity})),
    )
        .into_response()
}

pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Response {
    st.customer_sessions
        .revoke(cookie_value(&headers, COOKIE).as_deref());
    (
        [(
            header::SET_COOKIE,
            format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/api/customer; Max-Age=0"),
        )],
        Json(json!({"authenticated": false})),
    )
        .into_response()
}

async fn session(st: &AppState, headers: &HeaderMap) -> Result<Option<Session>, ()> {
    let token = cookie_value(headers, COOKIE);
    let Some(session) = st.customer_sessions.get(token.as_deref()) else {
        return Ok(None);
    };
    if identity(st, &session.credential).await?.as_ref() != Some(&session.identity) {
        st.customer_sessions.revoke(token.as_deref());
        return Ok(None);
    }
    Ok(Some(session))
}

pub async fn status(State(st): State<AppState>, headers: HeaderMap) -> Response {
    match session(&st, &headers).await {
        Ok(Some(value)) => {
            Json(json!({"authenticated": true, "identity": value.identity})).into_response()
        }
        Ok(None) => Json(json!({"authenticated": false})).into_response(),
        Err(()) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "customer_workflow_unavailable",
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverviewQuery {
    request_id: Option<String>,
}

pub async fn overview(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<OverviewQuery>,
) -> Response {
    let session = match session(&st, &headers).await {
        Ok(Some(session)) => session,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "customer_authentication_required"),
        Err(()) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "customer_workflow_unavailable",
            )
        }
    };
    let query: Vec<_> = query
        .request_id
        .as_deref()
        .map(|id| ("request_id", id))
        .into_iter()
        .collect();
    match upstream(
        &st,
        &session.credential,
        "/customer/workflow/overview",
        &query,
        None,
    )
    .await
    {
        Ok((status, value)) => (status, Json(value)).into_response(),
        Err(_) => error(StatusCode::BAD_GATEWAY, "customer_workflow_unavailable"),
    }
}

pub async fn commands(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if body.len() > 65536 || !json_request(&headers) {
        return error(StatusCode::BAD_REQUEST, "invalid_command");
    }
    forward_command(st, headers, &body, "/customer/workflow/commands").await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeliveryReference {
    id: String,
    generation: u64,
    digest: String,
}

impl DeliveryReference {
    fn valid(&self) -> bool {
        !self.id.is_empty()
            && self.id.len() <= 512
            && self.generation > 0
            && self.digest.len() == 64
            && self
                .digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            && self.digest.bytes().any(|byte| byte != b'0')
    }
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum CustomerDeliveryIntent {
    ConfirmDelivery {
        project_id: String,
        delivery: DeliveryReference,
        release: DeliveryReference,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CustomerDeliveryEnvelope {
    operation_id: uuid::Uuid,
    intent: CustomerDeliveryIntent,
}

pub async fn delivery(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if body.len() > 4096 || !json_request(&headers) {
        return error(StatusCode::BAD_REQUEST, "invalid_delivery_confirmation");
    }
    let value: CustomerDeliveryEnvelope = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::BAD_REQUEST, "invalid_delivery_confirmation"),
    };
    let CustomerDeliveryIntent::ConfirmDelivery {
        project_id,
        delivery,
        release,
    } = value.intent;
    if value.operation_id.is_nil()
        || project_id.is_empty()
        || project_id.len() > 512
        || !delivery.valid()
        || !release.valid()
    {
        return error(StatusCode::BAD_REQUEST, "invalid_delivery_confirmation");
    }
    // Forward the original envelope. No browser authority or timestamp is minted
    // here; the daemon owns receipt validation and durable replay.
    forward_command(st, headers, &body, "/company/delivery/intents").await
}

async fn forward_command(st: AppState, headers: HeaderMap, body: &[u8], path: &str) -> Response {
    let session = match session(&st, &headers).await {
        Ok(Some(session)) => session,
        Ok(None) => return error(StatusCode::UNAUTHORIZED, "customer_authentication_required"),
        Err(()) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "customer_workflow_unavailable",
            )
        }
    };
    match upstream(&st, &session.credential, path, &[], Some(body)).await {
        Ok((status, value)) => (status, Json(value)).into_response(),
        Err(_) => error(StatusCode::BAD_GATEWAY, "customer_command_outcome_unknown"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{body::Body, http::Request, routing::get, Router};
    use tower::ServiceExt;

    fn fixture_identity() -> Identity {
        Identity {
            schema_version: 1,
            principal_id: "customer-principal".into(),
            tenant_id: "tenant-one".into(),
            customer_id: "customer-one".into(),
        }
    }

    fn state() -> AppState {
        let mut config = crate::Config::from_env();
        config.events_db = "/nonexistent/customer-tests/events.db".into();
        config.operator_url = "http://127.0.0.1:1".into();
        AppState::new(config).unwrap()
    }

    fn confirmation() -> Value {
        json!({
            "operation_id": "018f3f32-4f01-7f2c-a6c1-f6f4a81b2809",
            "intent": {
                "action": "confirm_delivery", "project_id": "project-one",
                "delivery": {"id":"delivery-one","generation":1,"digest":"a".repeat(64)},
                "release": {"id":"release-one","generation":1,"digest":"b".repeat(64)},
            }
        })
    }

    #[tokio::test]
    async fn customer_delivery_proxy_rejects_privilege_and_unbound_intents_before_io() {
        let mut cases = Vec::new();
        for action in ["accept", "release", "assign_qa", "execute_qa", "closeout"] {
            let mut value = confirmation();
            value["intent"]["action"] = json!(action);
            cases.push(value);
        }
        for field in ["principal", "tenant_id", "effective_at_ms"] {
            let mut value = confirmation();
            value[field] = json!("injected");
            cases.push(value);
        }
        let mut missing = confirmation();
        missing["intent"].as_object_mut().unwrap().remove("release");
        cases.push(missing);
        for digest in ["bad".to_string(), "0".repeat(64), "A".repeat(64)] {
            let mut value = confirmation();
            value["intent"]["delivery"]["digest"] = json!(digest);
            cases.push(value);
        }
        let app = crate::build_app(state());
        for value in cases {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/customer/delivery")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(serde_json::to_vec(&value).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/customer/delivery")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(confirmation().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn customer_delivery_proxy_preserves_exact_body_and_server_credential() {
        let recorded = Arc::new(Mutex::new(Vec::<Vec<u8>>::new()));
        let captures = recorded.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new()
            .route(
                "/customer/workflow/identity",
                get(|| async { Json(fixture_identity()) }),
            )
            .route(
                "/company/delivery/intents",
                axum::routing::post(move |headers: HeaderMap, body: Bytes| {
                    let captures = captures.clone();
                    async move {
                        assert_eq!(
                            headers[header::AUTHORIZATION],
                            "Bearer server-customer-credential"
                        );
                        captures.lock().unwrap().push(body.to_vec());
                        Json(json!({"replayed": true, "action": "accept"}))
                    }
                }),
            );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut st = state();
        Arc::make_mut(&mut st.config).operator_url = url;
        let token = st
            .customer_sessions
            .create("server-customer-credential".into(), fixture_identity())
            .unwrap();
        let app = crate::build_app(st);
        let body = confirmation().to_string();
        for _ in 0..2 {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/customer/delivery")
                        .header(header::CONTENT_TYPE, "application/json")
                        .header(header::COOKIE, format!("{COOKIE}={token}"))
                        .body(Body::from(body.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let response = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert!(!String::from_utf8_lossy(&response).contains("server-customer-credential"));
        }
        assert_eq!(*recorded.lock().unwrap(), vec![body.as_bytes().to_vec(); 2]);
        server.abort();
        let _ = server.await;
    }

    #[test]
    fn customer_sessions_are_bounded_revocable_and_expiring() {
        let sessions = CustomerSessions::default();
        let token = sessions
            .create("credential".into(), fixture_identity())
            .unwrap();
        assert!(sessions.get(Some(&token)).is_some());
        sessions.revoke(Some(&token));
        assert!(sessions.get(Some(&token)).is_none());
        for _ in 0..256 {
            assert!(sessions
                .create("credential".into(), fixture_identity())
                .is_some());
        }
        assert!(sessions
            .create("credential".into(), fixture_identity())
            .is_none());
        for session in sessions.0.lock().unwrap().values_mut() {
            session.expires = Instant::now();
        }
        assert!(sessions
            .create("credential".into(), fixture_identity())
            .is_some());
    }

    #[tokio::test]
    async fn customer_and_operator_cookies_are_not_interchangeable() {
        let st = state();
        let operator = st.sessions.create();
        let customer = st
            .customer_sessions
            .create("credential".into(), fixture_identity())
            .unwrap();
        let app = crate::build_app(st);
        for (path, cookie) in [
            (
                "/api/customer/overview",
                format!("{}={operator}", crate::auth::SESSION_COOKIE),
            ),
            ("/api/agents", format!("{COOKIE}={customer}")),
            ("/api/customer/overview", format!("{COOKIE}={operator}")),
            (
                "/api/agents",
                format!("{}={customer}", crate::auth::SESSION_COOKIE),
            ),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::COOKIE, cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        }
    }

    #[tokio::test]
    async fn daemon_outage_preserves_session_but_does_not_authorize_io() {
        let st = state();
        let token = st
            .customer_sessions
            .create("credential".into(), fixture_identity())
            .unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(header::COOKIE, format!("{COOKIE}={token}").parse().unwrap());
        assert!(session(&st, &headers).await.is_err());
        assert!(st.customer_sessions.get(Some(&token)).is_some());
        headers.insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        let response = commands(State(st), headers, Bytes::from_static(b"{}")).await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn customer_login_mints_only_scoped_http_only_cookie_without_secret_echo() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let router = Router::new().route(
            "/customer/workflow/identity",
            get(|headers: HeaderMap| async move {
                assert_eq!(
                    headers[header::AUTHORIZATION],
                    "Bearer 01234567890123456789012345678901"
                );
                Json(fixture_identity())
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let mut st = state();
        Arc::make_mut(&mut st.config).operator_url = url;
        Arc::make_mut(&mut st.config).cookie_secure = true;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            axum::http::HeaderValue::from_static("application/json"),
        );
        let response = login(
            State(st.clone()),
            ClientIp("127.0.0.1".parse().unwrap()),
            headers,
            Bytes::from_static(br#"{"key":"01234567890123456789012345678901"}"#),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        for attribute in [
            "HttpOnly",
            "SameSite=Strict",
            "Secure",
            "Path=/api/customer",
            "Max-Age=3600",
        ] {
            assert!(cookie.contains(attribute));
        }
        assert!(cookie.starts_with(COOKIE));
        let token = cookie.split(';').next().unwrap().split_once('=').unwrap().1;
        assert!(st.customer_sessions.get(Some(token)).is_some());
        assert!(!st.sessions.validate(Some(token)));
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("01234567890123456789012345678901"));
        server.abort();
        let _ = server.await;
    }

    #[tokio::test]
    async fn revoked_or_changed_identity_invalidates_customer_session() {
        for reply in [
            (StatusCode::FORBIDDEN, json!({"error":"forbidden"})),
            (
                StatusCode::OK,
                json!({"schema_version":1,"principal_id":"different","tenant_id":"tenant-one","customer_id":"customer-one"}),
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let router = Router::new().route(
                "/customer/workflow/identity",
                get(move || async move { (reply.0, Json(reply.1)) }),
            );
            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });
            let mut st = state();
            Arc::make_mut(&mut st.config).operator_url = url;
            let token = st
                .customer_sessions
                .create("credential".into(), fixture_identity())
                .unwrap();
            let mut headers = HeaderMap::new();
            headers.insert(header::COOKIE, format!("{COOKIE}={token}").parse().unwrap());
            assert!(session(&st, &headers).await.unwrap().is_none());
            assert!(st.customer_sessions.get(Some(&token)).is_none());
            server.abort();
            let _ = server.await;
        }
    }
}
