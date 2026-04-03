#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("another daemon is already running (PID {0})")]
    AlreadyRunning(i32),
    #[error("no daemon is running")]
    NotRunning,
    #[error("failed to create Unix socket: {0}")]
    SocketFailed(#[source] std::io::Error),
    #[error("unsupported log schema version {0} (this version of replayfs supports up to version {1})")]
    UnsupportedSchemaVersion(u64, u64),
}
