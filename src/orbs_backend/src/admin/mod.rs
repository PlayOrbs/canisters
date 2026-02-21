//! Admin operations module.
//!
//! Contains admin helpers, initialization, and data management.

mod core;
mod data;

pub use core::{add_admin, ensure_admin, get_pubkey_hex, init_admins, init_pubkey};
pub use data::clear_all_data;
