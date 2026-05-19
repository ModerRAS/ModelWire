//! ModelWire Core
//!
//! Provider-neutral types, configuration, and ID generation.

pub mod canonical;
pub mod config;
pub mod error;
pub mod id;
pub mod ssrf;

pub use canonical::*;
pub use config::*;
pub use error::*;
pub use id::*;
pub use ssrf::*;
