use ort::session::Session;
use std::path::Path;

mod embedding;
mod melspectrogram;
mod wakeword;

pub use wakeword::WakeWordModel;

#[derive(Debug, thiserror::Error)]
pub enum WakeWordError {
    #[error(transparent)]
    Ort(#[from] ort::Error),
    #[error(transparent)]
    Shape(#[from] ndarray::ShapeError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("wake word model not found: {0}")]
    ModelNotFound(String),
    #[error("invalid wake word model contract: {0}")]
    InvalidModelContract(String),
    #[error("unsupported sample rate: {0} Hz")]
    UnsupportedSampleRate(u32),
}

pub const SAMPLE_RATE: usize = 16_000;
pub const EMBEDDING_WINDOW: usize = 76;
pub const EMBEDDING_STRIDE: usize = 8;
pub const EMBEDDING_DIM: usize = 96;
pub const MIN_EMBEDDINGS: usize = 16;

pub(crate) fn build_session_from_memory(bytes: &[u8]) -> Result<Session, WakeWordError> {
    Ok(Session::builder()?.commit_from_memory(bytes)?)
}

pub(crate) fn build_session_from_file(path: impl AsRef<Path>) -> Result<Session, WakeWordError> {
    let bytes = std::fs::read(path)?;
    Ok(Session::builder()?.commit_from_memory(&bytes)?)
}
