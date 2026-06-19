//! Error types for the floorplan geometry library

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Stroke has too few points (minimum 3 required)")]
    InsufficientPoints,

    #[error("Orthogonal conversion failed: {0}")]
    OrthogonalError(String),

    #[error("Invalid edge index: {0}")]
    InvalidEdgeIndex(usize),

    #[error("Dimension application failed: {0}")]
    DimensionError(String),

    #[error("Extend failed: {0}")]
    ExtendError(String),

    #[error("Trim failed: {0}")]
    TrimError(String),

    #[error("can't complete this cut without changing its shape: it can't stay orthogonal and still reach the walls")]
    CutWouldChangeShape,
}
