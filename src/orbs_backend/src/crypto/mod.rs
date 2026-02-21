//! Cryptographic operations module.
//!
//! Contains ECDSA key derivation and CBOR serialization utilities.

pub mod address;
pub mod cbor;

pub use address::{AddressError, create_derivation_path, get_ecdsa_key_id, get_public_key};
pub use cbor::Cbor;
