//! system — host-file management, elevated writes, and OS port forwarding.
//!
//! Faithful port of Go's `internal/system` package.

mod elevated;
mod hostfile;
mod portfwd;

pub use elevated::*;
pub use hostfile::*;
pub use portfwd::*;
