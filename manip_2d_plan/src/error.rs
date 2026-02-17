//! Error types for the floorplan geometry library

use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("Stroke has too few points (minimum 3 required)")]
    InsufficientPoints,
    
    #[error("Failed to close polygon")]
    PolygonClosureError,
    
    #[error("Invalid polygon: {0}")]
    InvalidPolygon(String),
    
    #[error("Orthogonal conversion failed: {0}")]
    OrthogonalError(String),
    
    #[error("Snapping failed: {0}")]
    SnappingError(String),
    
    #[error("Constraint conflict: {0}")]
    ConstraintConflict(String),
    
    #[error("Invalid edge index: {0}")]
    InvalidEdgeIndex(usize),
    
    #[error("Dimension application failed: {0}")]
    DimensionError(String),
    
    #[error("Boolean operation failed: {0}")]
    BooleanOpError(String),
    
    #[error("Room detection failed: {0}")]
    RoomDetectionError(String),
    
    #[error("Geometry error: {0}")]
    GeometryError(String),
}
