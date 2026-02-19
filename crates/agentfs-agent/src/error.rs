use std::fmt;

/// All errors produced by the infinity-agent.
#[derive(Debug)]
#[allow(dead_code)]
pub enum AgentError {
    /// HTTP/network errors from reqwest.
    Http(reqwest::Error),
    /// JSON serialization/deserialization errors.
    Json(serde_json::Error),
    /// I/O errors.
    Io(std::io::Error),
    /// AgentFS database errors.
    AgentFS(agentfs_core::error::AgentFSError),
    /// Authentication errors.
    Auth(String),
    /// API errors (non-200 responses).
    Api { status: u16, message: String },
    /// Tool execution errors.
    Tool(String),
    /// Stream parsing errors.
    Stream(String),
    /// Configuration errors.
    Config(String),
    /// MCP protocol/server errors.
    Mcp(String),
    /// Memory system errors.
    Memory(String),
    /// Generic errors.
    Other(String),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Http(e) => write!(f, "HTTP error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::AgentFS(e) => write!(f, "AgentFS error: {e}"),
            Self::Auth(msg) => write!(f, "Auth error: {msg}"),
            Self::Api { status, message } => write!(f, "API error ({status}): {message}"),
            Self::Tool(msg) => write!(f, "Tool error: {msg}"),
            Self::Stream(msg) => write!(f, "Stream error: {msg}"),
            Self::Config(msg) => write!(f, "Config error: {msg}"),
            Self::Mcp(msg) => write!(f, "MCP error: {msg}"),
            Self::Memory(msg) => write!(f, "Memory error: {msg}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for AgentError {}

impl From<reqwest::Error> for AgentError {
    fn from(e: reqwest::Error) -> Self {
        Self::Http(e)
    }
}

impl From<serde_json::Error> for AgentError {
    fn from(e: serde_json::Error) -> Self {
        Self::Json(e)
    }
}

impl From<std::io::Error> for AgentError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<agentfs_core::error::AgentFSError> for AgentError {
    fn from(e: agentfs_core::error::AgentFSError) -> Self {
        Self::AgentFS(e)
    }
}

pub type Result<T> = std::result::Result<T, AgentError>;
