use thiserror::Error;

#[non_exhaustive]
#[derive(Debug, Error)]
pub enum VncError {
    #[error("Auth is required but no password provided")]
    NoPassword,
    #[error("No VNC encoding selected")]
    NoEncoding,
    #[error("Unknown VNC security type: {0}")]
    InvalidSecurityType(u32),
    #[error("Unknown VNC security result: {0}")]
    InvalidSecurityResult(u32),
    #[error("Unknown VNC encoding: {0}")]
    InvalidEncoding(i32),
    #[error("Unsupported VNC security type")]
    UnsupportedSecurityType,
    #[error("Server did not offer the required VNC security type: {0}")]
    RequiredSecurityTypeUnavailable(&'static str),
    #[error("Wrong password")]
    WrongPassword,
    #[error("Server rejected the connection: {0}")]
    SecurityFailure(String),
    #[error("Protocol limit exceeded for {field}: {actual} > {limit}")]
    LimitExceeded {
        field: &'static str,
        actual: u64,
        limit: u64,
    },
    #[error("Invalid framebuffer or rectangle dimensions")]
    InvalidDimensions,
    #[error("Integer overflow while calculating {0}")]
    IntegerOverflow(&'static str),
    #[error("Connect error with unknown reason")]
    ConnectError,
    #[error("Unknown pixel format")]
    WrongPixelFormat,
    #[error("Unkonw server message")]
    WrongServerMessage,
    #[error("Image data cannot be decoded correctly")]
    InvalidImageData,
    #[error("The VNC client isn't started. Or it is already closed")]
    ClientNotRunning,
    #[error(transparent)]
    IoError(#[from] std::io::Error),
    #[error("VNC Error with message: {0}")]
    General(String),
}

impl<T> From<tokio::sync::mpsc::error::SendError<T>> for VncError {
    fn from(_value: tokio::sync::mpsc::error::SendError<T>) -> Self {
        VncError::General("Channel closed".to_string())
    }
}
