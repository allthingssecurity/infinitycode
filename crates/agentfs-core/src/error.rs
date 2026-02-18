use std::path::PathBuf;

/// All errors produced by agentfs-core.
#[derive(Debug, thiserror::Error)]
pub enum AgentFSError {
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("database not found: {}", path.display())]
    DatabaseNotFound { path: PathBuf },

    #[error("database already exists: {}", path.display())]
    DatabaseExists { path: PathBuf },

    #[error("schema version mismatch: expected {expected}, found {found}")]
    SchemaMismatch { expected: u32, found: u32 },

    #[error("file not found: {path}")]
    FileNotFound { path: String },

    #[error("not a directory: {path}")]
    NotADirectory { path: String },

    #[error("not a file: {path}")]
    NotAFile { path: String },

    #[error("directory not empty: {path}")]
    DirectoryNotEmpty { path: String },

    #[error("already exists: {path}")]
    AlreadyExists { path: String },

    #[error("invalid path: {path}")]
    InvalidPath { path: String },

    #[error("checksum mismatch at ino={ino} chunk={chunk_index}: expected {expected:#018x}, got {actual:#018x}")]
    ChecksumMismatch {
        ino: i64,
        chunk_index: i64,
        expected: u64,
        actual: u64,
    },

    #[error("connection pool shut down")]
    PoolShutDown,

    #[error("key not found: {key}")]
    KeyNotFound { key: String },

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, AgentFSError>;
