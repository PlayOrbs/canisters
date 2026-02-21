//! Admin helpers and initialization functions.

use candid::Principal;
use ic_cdk::api::msg_caller;
use k256::PublicKey;
use k256::elliptic_curve::sec1::ToEncodedPoint;

use crate::crypto::get_public_key;
use crate::state::{ADMINS, PUBKEY_CELL};

/// Check if the caller is an admin. Traps if not.
pub fn ensure_admin() {
    let caller = msg_caller();
    ADMINS.with(|a| {
        let a = a.borrow();
        let is_admin = a.get(&caller).is_some();
        if !is_admin {
            ic_cdk::trap("unauthorized: caller is not admin");
        }
    });
}

/// Initialize admins from a list of principals.
pub fn init_admins(admins: Vec<Principal>) {
    let caller = msg_caller();
    ADMINS.with(|a| {
        let mut a = a.borrow_mut();
        for admin in admins {
            a.insert(admin, true);
        }
        a.insert(caller, true);
    });
}

/// Add a new admin.
pub fn add_admin(new_admin: Principal) {
    ensure_admin();
    ADMINS.with(|a| {
        let mut a = a.borrow_mut();
        a.insert(new_admin, true);
    });
}

/// Initialize and cache ECDSA pubkey once.
/// Note: Caller must be verified as admin by the entrypoint (lib.rs).
pub async fn init_pubkey() -> Result<(), String> {
    // ensure_admin() is called by the entrypoint in lib.rs

    // Do not allow overwriting once set
    let already_initialized = PUBKEY_CELL.with(|cell| {
        let cell = cell.borrow();
        !cell.get().is_empty()
    });

    if already_initialized {
        return Err("pubkey already initialized".to_string());
    }

    let pk = get_public_key(None)
        .await
        .map_err(|e| format!("failed to fetch pubkey: {:?}", e))?;

    // Convert to uncompressed SEC1 encoding (65 bytes: 0x04 + x + y)
    let encoded_point = pk.to_encoded_point(false); // false = uncompressed
    let uncompressed = encoded_point.as_bytes().to_vec();

    PUBKEY_CELL.with(|cell| cell.borrow_mut().set(uncompressed));

    Ok(())
}

/// Get cached pubkey as hex string.
pub fn get_pubkey_hex() -> Result<String, String> {
    let bytes = PUBKEY_CELL.with_borrow(|cell| cell.get().clone());
    if bytes.is_empty() {
        return Err("pubkey not initialized".to_string());
    }

    let pub_key = PublicKey::from_sec1_bytes(&bytes)
        .map_err(|e| format!("failed to parse pubkey: {:?}", e))?;

    // Return compressed SEC1 encoding (33 bytes)
    let encoded_point = pub_key.to_encoded_point(true); // true = compressed
    Ok(hex::encode(encoded_point.as_bytes()))
}
