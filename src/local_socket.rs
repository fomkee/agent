use std::{
    io::ErrorKind,
    os::unix::{fs::FileTypeExt, fs::PermissionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
    sync::{Semaphore, watch},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::model::Snapshot;

const MAX_FRAME_BYTES: usize = 2_097_152;
const MAX_CLIENTS: usize = 8;

#[derive(Debug, Error)]
pub enum LocalSocketError {
    #[error("local snapshot socket path is not an existing socket: {0}")]
    UnsafeExistingPath(PathBuf),
    #[error("local snapshot socket I/O failed")]
    Io(#[source] std::io::Error),
    #[error("local snapshot serialization failed")]
    Serialize(#[source] serde_json::Error),
    #[error("local snapshot frame exceeds {MAX_FRAME_BYTES} bytes")]
    FrameTooLarge,
    #[error("local snapshot stream closed")]
    SnapshotStreamClosed,
    #[error("local snapshot client task failed: {0}")]
    ClientTask(String),
}

pub async fn serve(
    path: PathBuf,
    snapshots: watch::Receiver<Arc<Snapshot>>,
    cancellation: CancellationToken,
) -> Result<(), LocalSocketError> {
    remove_stale_socket(&path).await?;
    let listener = UnixListener::bind(&path).map_err(LocalSocketError::Io)?;
    tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660))
        .await
        .map_err(LocalSocketError::Io)?;
    let _socket_guard = SocketGuard(path.clone());
    let limiter = Arc::new(Semaphore::new(MAX_CLIENTS));
    let mut clients = JoinSet::new();

    loop {
        tokio::select! {
            _ = cancellation.cancelled() => break,
            joined = clients.join_next(), if !clients.is_empty() => {
                inspect_client(joined)?;
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(LocalSocketError::Io)?;
                let Ok(permit) = Arc::clone(&limiter).try_acquire_owned() else {
                    continue;
                };
                let receiver = snapshots.clone();
                let client_cancellation = cancellation.child_token();
                clients.spawn(async move {
                    let _permit = permit;
                    stream_snapshots(stream, receiver, client_cancellation).await
                });
            }
        }
    }
    while let Some(joined) = clients.join_next().await {
        inspect_client(Some(joined))?;
    }
    Ok(())
}

pub async fn read_snapshot(stream: &mut UnixStream) -> Result<Snapshot, LocalSocketError> {
    let length = stream.read_u32().await.map_err(LocalSocketError::Io)?;
    let length = usize::try_from(length).map_err(|_| LocalSocketError::FrameTooLarge)?;
    if length > MAX_FRAME_BYTES {
        return Err(LocalSocketError::FrameTooLarge);
    }
    let mut frame = vec![0_u8; length];
    stream
        .read_exact(&mut frame)
        .await
        .map_err(LocalSocketError::Io)?;
    serde_json::from_slice(&frame).map_err(LocalSocketError::Serialize)
}

async fn stream_snapshots(
    mut stream: UnixStream,
    mut snapshots: watch::Receiver<Arc<Snapshot>>,
    cancellation: CancellationToken,
) -> Result<(), LocalSocketError> {
    let initial = snapshots.borrow().clone();
    write_snapshot_or_cancel(&mut stream, &initial, &cancellation).await?;
    loop {
        tokio::select! {
            _ = cancellation.cancelled() => return Ok(()),
            changed = snapshots.changed() => {
                changed.map_err(|_| LocalSocketError::SnapshotStreamClosed)?;
                let snapshot = snapshots.borrow().clone();
                if let Err(error) = write_snapshot_or_cancel(
                    &mut stream,
                    &snapshot,
                    &cancellation,
                ).await {
                    if is_disconnect(&error) {
                        return Ok(());
                    }
                    return Err(error);
                }
            }
        }
    }
}

async fn write_snapshot_or_cancel(
    stream: &mut UnixStream,
    snapshot: &Snapshot,
    cancellation: &CancellationToken,
) -> Result<(), LocalSocketError> {
    tokio::select! {
        _ = cancellation.cancelled() => Ok(()),
        result = write_snapshot(stream, snapshot) => result,
    }
}

async fn write_snapshot(
    stream: &mut UnixStream,
    snapshot: &Snapshot,
) -> Result<(), LocalSocketError> {
    let frame = serde_json::to_vec(snapshot).map_err(LocalSocketError::Serialize)?;
    if frame.len() > MAX_FRAME_BYTES {
        return Err(LocalSocketError::FrameTooLarge);
    }
    let length = u32::try_from(frame.len()).map_err(|_| LocalSocketError::FrameTooLarge)?;
    stream
        .write_u32(length)
        .await
        .map_err(LocalSocketError::Io)?;
    stream.write_all(&frame).await.map_err(LocalSocketError::Io)
}

async fn remove_stale_socket(path: &Path) -> Result<(), LocalSocketError> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.file_type().is_socket() => tokio::fs::remove_file(path)
            .await
            .map_err(LocalSocketError::Io),
        Ok(_) => Err(LocalSocketError::UnsafeExistingPath(path.to_path_buf())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LocalSocketError::Io(error)),
    }
}

fn inspect_client(
    joined: Option<Result<Result<(), LocalSocketError>, tokio::task::JoinError>>,
) -> Result<(), LocalSocketError> {
    match joined {
        Some(Ok(Ok(()))) | None => Ok(()),
        Some(Ok(Err(LocalSocketError::Io(error)))) if is_disconnect_kind(error.kind()) => Ok(()),
        Some(Ok(Err(error))) => Err(error),
        Some(Err(error)) => Err(LocalSocketError::ClientTask(error.to_string())),
    }
}

fn is_disconnect(error: &LocalSocketError) -> bool {
    matches!(error, LocalSocketError::Io(source) if is_disconnect_kind(source.kind()))
}

fn is_disconnect_kind(kind: ErrorKind) -> bool {
    matches!(
        kind,
        ErrorKind::BrokenPipe | ErrorKind::ConnectionReset | ErrorKind::ConnectionAborted
    )
}

struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use chrono::Utc;

    use super::*;

    #[tokio::test]
    async fn test_streams_latest_snapshot_and_shuts_down() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("agent.sock");
        let (sender, receiver) = watch::channel(Arc::new(Snapshot::empty(Utc::now())));
        let cancellation = CancellationToken::new();
        let server_cancellation = cancellation.clone();
        let server_path = path.clone();
        let server =
            tokio::spawn(async move { serve(server_path, receiver, server_cancellation).await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let mut client = UnixStream::connect(&path).await.expect("socket connects");
        let mode = tokio::fs::metadata(&path)
            .await
            .expect("socket metadata")
            .permissions()
            .mode();

        // Act
        let initial = read_snapshot(&mut client).await.expect("initial snapshot");
        sender
            .send(Arc::new(Snapshot {
                schema_version: 1,
                sequence: 2,
                generated_at: Utc::now(),
                observations: Vec::new(),
            }))
            .expect("snapshot receiver exists");
        let updated = read_snapshot(&mut client).await.expect("updated snapshot");
        cancellation.cancel();
        let result = server.await.expect("server joins");

        // Assert
        assert_eq!(initial.sequence, 0);
        assert_eq!(updated.sequence, 2);
        assert_eq!(mode & 0o777, 0o660);
        assert!(result.is_ok());
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn test_reports_disconnection_while_reading_snapshot() {
        // Arrange
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        drop(server);

        // Act
        let result = read_snapshot(&mut client).await;

        // Assert
        assert!(matches!(result, Err(LocalSocketError::Io(_))));
    }

    #[tokio::test]
    async fn test_refuses_to_replace_non_socket_path() {
        // Arrange
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("agent.sock");
        tokio::fs::write(&path, b"owned by someone else")
            .await
            .expect("fixture file");
        let (_sender, receiver) = watch::channel(Arc::new(Snapshot::empty(Utc::now())));

        // Act
        let result = serve(path.clone(), receiver, CancellationToken::new()).await;

        // Assert
        assert!(matches!(
            result,
            Err(LocalSocketError::UnsafeExistingPath(rejected)) if rejected == path
        ));
        assert!(path.exists());
    }
}
