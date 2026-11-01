//! Java 767-specific transport. Not a native Rust plugin ABI.
pub mod physics;
mod transport;
pub use minecraft_sdk_wire::Packet;
pub use transport::*;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    Wire(minecraft_sdk_wire::Error),
    Invalid(String),
    Deadline,
    Closed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for Error {}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<minecraft_sdk_wire::Error> for Error {
    fn from(e: minecraft_sdk_wire::Error) -> Self {
        Self::Wire(e)
    }
}
pub(crate) fn invalid(s: impl Into<String>) -> Error {
    Error::Invalid(s.into())
}
