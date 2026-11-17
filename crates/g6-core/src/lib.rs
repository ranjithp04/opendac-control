//! Device library for the Creative Sound BlasterX G6 (USB `041e:3256`).
//!
//! See `docs/protocol-g6.md` §2 for the protocol reference.

pub mod direct;
pub mod eq_import;
pub mod events;
pub mod features;
pub mod preset;
pub mod protocol;
pub mod session;
pub mod state;
pub mod store;
pub mod transport;
