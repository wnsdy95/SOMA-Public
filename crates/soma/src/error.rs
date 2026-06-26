//! `SomaError` — v1 error taxonomy. Engine-level only. No HTTP
//! / transport variants survive the v1 reboot.

use thiserror::Error;

use crate::storage::StorageError;

#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SomaError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("storage: {0}")]
    Storage(#[from] StorageError),

    #[error("config: {0}")]
    Config(String),

    #[error("capture: {0}")]
    Capture(String),

    #[error("memory: {0}")]
    Memory(String),

    #[error("runtime: {0}")]
    Runtime(String),

    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, SomaError>;
