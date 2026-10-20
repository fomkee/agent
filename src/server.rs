use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{
    clock::Clock,
    config::BearerToken,
    model::Snapshot,
    render::{json, openmetrics},
};

#[derive(Debug, Error)]
pub enum ServerError {
    #[error("failed to bind agent HTTP listener")]
    Bind(#[source] std::io::Error),
    #[error("agent HTTP server failed")]
    Serve(#[source] std::io::Error),
}

#[derive(Clone)]
pub struct HttpState {
    snapshots: watch::Receiver<Arc<Snapshot>>,
    token: BearerToken,
    metrics_enabled: bool,
    max_snapshot_age: Duration,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for HttpState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpState")
            .field("token", &"[REDACTED]")
            .field("metrics_enabled", &self.metrics_enabled)
            .field("max_snapshot_age", &self.max_snapshot_age)
            .field("clock", &self.clock)
            .finish_non_exhaustive()
    }
}

pub async fn serve(
    listen: SocketAddr,
    state: HttpState,
    cancellation: CancellationToken,
) -> Result<(), ServerError> {
    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .map_err(ServerError::Bind)?;
    axum::serve(listener, router(state))
        .with_graceful_shutdown(cancellation.cancelled_owned())
        .await
        .map_err(ServerError::Serve)
}

pub fn router(state: HttpState) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/v1/snapshot", get(snapshot_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state)
}

async fn health_handler(State(state): State<HttpState>) -> Response {
    health_service(&state)
}

fn health_service(state: &HttpState) -> Response {
    let snapshot = state.snapshots.borrow();
    let fresh = state
        .clock
        .now()
        .signed_duration_since(snapshot.generated_at)
        .to_std()
        .is_ok_and(|age| age <= state.max_snapshot_age);
    if snapshot.sequence == 0 || !fresh {
        (StatusCode::SERVICE_UNAVAILABLE, "starting\n").into_response()
    } else {
        (StatusCode::OK, "ok\n").into_response()
    }
}

async fn snapshot_handler(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    snapshot_service(&state, &headers)
}

fn snapshot_service(state: &HttpState, headers: &HeaderMap) -> Response {
    if !authorized(headers, &state.token) {
        return unauthorized();
    }
    let snapshot = state.snapshots.borrow().clone();
    match serde_json::to_vec(&json::compact(&snapshot)) {
        Ok(body) => response(StatusCode::OK, "application/json", body),
        Err(error) => {
            tracing::error!(error = %error, "failed to serialize agent snapshot");
            (StatusCode::INTERNAL_SERVER_ERROR, "serialization failure\n").into_response()
        }
    }
}

async fn metrics_handler(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    metrics_service(&state, &headers)
}

fn metrics_service(state: &HttpState, headers: &HeaderMap) -> Response {
    if !state.metrics_enabled {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !authorized(headers, &state.token) {
        return unauthorized();
    }
    let snapshot = state.snapshots.borrow().clone();
    match openmetrics::render(&snapshot) {
        Ok(body) => response(
            StatusCode::OK,
            "application/openmetrics-text; version=1.0.0; charset=utf-8",
            body.into_bytes(),
        ),
        Err(error) => {
            tracing::error!(error = %error, "failed to render agent metrics");
            (StatusCode::INTERNAL_SERVER_ERROR, "rendering failure\n").into_response()
        }
    }
}

fn authorized(headers: &HeaderMap, token: &BearerToken) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    bool::from(value.as_bytes().ct_eq(token.expose().as_bytes()))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, "authentication required\n").into_response()
}

fn response(status: StatusCode, content_type: &'static str, body: Vec<u8>) -> Response {
    match Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
    {
        Ok(response) => response,
        Err(error) => {
            tracing::error!(error = %error, "failed to construct HTTP response");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

impl HttpState {
    pub fn new(
        snapshots: watch::Receiver<Arc<Snapshot>>,
        token: BearerToken,
        metrics_enabled: bool,
        max_snapshot_age: Duration,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            snapshots,
            token,
            metrics_enabled,
            max_snapshot_age,
            clock,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::arithmetic_side_effects)]
mod tests {
    use axum::{body::to_bytes, http::Request};
    use chrono::Utc;
    use tower::ServiceExt;

    use super::*;

    #[derive(Debug)]
    struct FixedClock(chrono::DateTime<Utc>);

    impl Clock for FixedClock {
        fn now(&self) -> chrono::DateTime<Utc> {
            self.0
        }
    }

    fn state() -> HttpState {
        let now = Utc::now();
        let snapshot = Arc::new(Snapshot {
            schema_version: 1,
            sequence: 1,
            generated_at: now,
            observations: Vec::new(),
        });
        let (_, receiver) = watch::channel(snapshot);
        HttpState::new(
            receiver,
            BearerToken::new("secret".to_owned()).expect("valid fixture"),
            true,
            Duration::from_secs(90),
            Arc::new(FixedClock(now)),
        )
    }

    #[tokio::test]
    async fn test_snapshot_rejects_missing_bearer_token() {
        // Arrange
        let request = Request::get("/v1/snapshot")
            .body(Body::empty())
            .expect("valid request");

        // Act
        let response = router(state())
            .oneshot(request)
            .await
            .expect("router responds");

        // Assert
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn test_snapshot_returns_cached_json_when_authorized() {
        // Arrange
        let request = Request::get("/v1/snapshot")
            .header(header::AUTHORIZATION, "Bearer secret")
            .body(Body::empty())
            .expect("valid request");

        // Act
        let response = router(state())
            .oneshot(request)
            .await
            .expect("router responds");
        let status = response.status();
        let body = to_bytes(response.into_body(), 65_536)
            .await
            .expect("body is readable");

        // Assert
        assert_eq!(status, StatusCode::OK);
        assert!(
            std::str::from_utf8(&body)
                .expect("utf8")
                .contains("schema_version")
        );
    }

    #[tokio::test]
    async fn test_health_rejects_stale_snapshot() {
        // Arrange
        let now = Utc::now();
        let snapshot = Arc::new(Snapshot {
            schema_version: 1,
            sequence: 1,
            generated_at: now - chrono::Duration::minutes(10),
            observations: Vec::new(),
        });
        let (_, receiver) = watch::channel(snapshot);
        let state = HttpState::new(
            receiver,
            BearerToken::new("secret".to_owned()).expect("valid fixture"),
            true,
            Duration::from_secs(90),
            Arc::new(FixedClock(now)),
        );
        let request = Request::get("/health")
            .body(Body::empty())
            .expect("valid request");

        // Act
        let response = router(state)
            .oneshot(request)
            .await
            .expect("router responds");

        // Assert
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
