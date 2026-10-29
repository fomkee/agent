use std::{sync::Arc, time::Duration};

use reqwest::{Client, StatusCode, redirect::Policy};
use thiserror::Error;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::{config::HeartbeatEndpoint, model::Snapshot, render::json};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_RETRY_DELAY: Duration = Duration::from_millis(500);
const SECOND_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_ATTEMPTS: u8 = 3;

#[derive(Debug, Error)]
pub enum HeartbeatPublisherError {
    #[error("failed to build heartbeat HTTP client: {0}")]
    Client(String),
    #[error("snapshot channel closed unexpectedly")]
    SnapshotChannelClosed,
}

enum DeliveryOutcome {
    Delivered { attempts: u8 },
    Rejected { status: StatusCode, attempts: u8 },
    Failed { attempts: u8 },
    Cancelled,
}

pub async fn run(
    endpoint: HeartbeatEndpoint,
    mut snapshots: watch::Receiver<Arc<Snapshot>>,
    cancellation: CancellationToken,
) -> Result<(), HeartbeatPublisherError> {
    let client = Client::builder()
        .redirect(Policy::none())
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| HeartbeatPublisherError::Client(error.to_string()))?;

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            changed = snapshots.changed() => {
                changed.map_err(|_| HeartbeatPublisherError::SnapshotChannelClosed)?;
            }
        }
        let snapshot = snapshots.borrow_and_update().clone();
        match deliver(&client, endpoint.expose(), &snapshot, &cancellation).await {
            DeliveryOutcome::Delivered { attempts } => {
                tracing::debug!(
                    sequence = snapshot.sequence,
                    attempts,
                    "published Fomkee heartbeat"
                );
            }
            DeliveryOutcome::Rejected { status, attempts } => {
                tracing::warn!(
                    sequence = snapshot.sequence,
                    attempts,
                    status = %status,
                    "Fomkee heartbeat endpoint rejected snapshot"
                );
            }
            DeliveryOutcome::Failed { attempts } => {
                tracing::warn!(
                    sequence = snapshot.sequence,
                    attempts,
                    "failed to publish Fomkee heartbeat after bounded retries"
                );
            }
            DeliveryOutcome::Cancelled => return Ok(()),
        }
    }
}

async fn deliver(
    client: &Client,
    endpoint: &url::Url,
    snapshot: &Snapshot,
    cancellation: &CancellationToken,
) -> DeliveryOutcome {
    let payload = json::compact(snapshot);
    let mut attempts = 0_u8;

    loop {
        attempts = attempts.saturating_add(1);
        let result = tokio::select! {
            _ = cancellation.cancelled() => return DeliveryOutcome::Cancelled,
            result = client.post(endpoint.clone()).json(&payload).send() => result,
        };

        match result {
            Ok(response) if response.status().is_success() => {
                return DeliveryOutcome::Delivered { attempts };
            }
            Ok(response) if !is_transient_status(response.status()) => {
                return DeliveryOutcome::Rejected {
                    status: response.status(),
                    attempts,
                };
            }
            Ok(response) if attempts >= MAX_ATTEMPTS => {
                return DeliveryOutcome::Rejected {
                    status: response.status(),
                    attempts,
                };
            }
            Err(error) if attempts >= MAX_ATTEMPTS => {
                tracing::debug!(
                    timeout = error.is_timeout(),
                    connection = error.is_connect(),
                    "Fomkee heartbeat delivery attempt failed"
                );
                return DeliveryOutcome::Failed { attempts };
            }
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    timeout = error.is_timeout(),
                    connection = error.is_connect(),
                    "Fomkee heartbeat delivery attempt failed"
                );
            }
        }

        let delay = if attempts == 1 {
            FIRST_RETRY_DELAY
        } else {
            SECOND_RETRY_DELAY
        };
        tokio::select! {
            _ = cancellation.cancelled() => return DeliveryOutcome::Cancelled,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

fn is_transient_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT | StatusCode::TOO_EARLY | StatusCode::TOO_MANY_REQUESTS
    ) || status.is_server_error()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use axum::{Router, extract::State, http::StatusCode, routing::post};
    use chrono::Utc;
    use tokio::net::TcpListener;
    use url::Url;

    use super::*;
    use crate::model::Snapshot;

    #[tokio::test]
    async fn test_retries_transient_responses_until_delivery_succeeds() {
        // Arrange
        let attempts = Arc::new(AtomicUsize::new(0));
        let endpoint = retrying_endpoint(Arc::clone(&attempts)).await;
        let client = Client::new();
        let snapshot = Snapshot::empty(Utc::now());

        // Act
        let outcome = deliver(&client, &endpoint, &snapshot, &CancellationToken::new()).await;

        // Assert
        assert!(matches!(
            outcome,
            DeliveryOutcome::Delivered { attempts: 3 }
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_does_not_retry_permanent_client_rejection() {
        // Arrange
        let attempts = Arc::new(AtomicUsize::new(0));
        let endpoint = rejecting_endpoint(Arc::clone(&attempts)).await;
        let client = Client::new();
        let snapshot = Snapshot::empty(Utc::now());

        // Act
        let outcome = deliver(&client, &endpoint, &snapshot, &CancellationToken::new()).await;

        // Assert
        assert!(matches!(
            outcome,
            DeliveryOutcome::Rejected {
                status: StatusCode::BAD_REQUEST,
                attempts: 1
            }
        ));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    async fn retrying_endpoint(attempts: Arc<AtomicUsize>) -> Url {
        let app = Router::new()
            .route("/heartbeat", post(retry_twice))
            .with_state(attempts);
        spawn_endpoint(app).await
    }

    async fn rejecting_endpoint(attempts: Arc<AtomicUsize>) -> Url {
        let app = Router::new()
            .route("/heartbeat", post(reject))
            .with_state(attempts);
        spawn_endpoint(app).await
    }

    async fn spawn_endpoint(app: Router) -> Url {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Url::parse(&format!("http://{address}/heartbeat")).unwrap()
    }

    async fn retry_twice(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        let attempt = attempts.fetch_add(1, Ordering::SeqCst);
        if attempt < 2 {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::NO_CONTENT
        }
    }

    async fn reject(State(attempts): State<Arc<AtomicUsize>>) -> StatusCode {
        attempts.fetch_add(1, Ordering::SeqCst);
        StatusCode::BAD_REQUEST
    }
}
