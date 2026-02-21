//! Data management utilities (clear, reset, etc.)

use crate::state::{PLAYERS, PLAYER_ROUND_REFS, ROUND_HISTORY_ENTRIES, ROUND_SNAPSHOTS, SEED_CHUNKS, CHUNK_OFFSETS, REVEALED_SEEDS, LAST_SETTLED_ROUNDS};

/// Clear all data from the canister (admin only).
/// Use this when switching to a new Solana program.
/// Note: Does NOT clear PUBKEY_CELL as the ECDSA key should remain the same.
/// Note: Caller must be verified as admin by the entrypoint (lib.rs).
pub fn clear_all_data() -> Result<(), String> {
    // ensure_admin() is called by the entrypoint in lib.rs

    // Clear players
    PLAYERS.with_borrow_mut(|m| {
        m.clear_new();
    });

    // Clear round snapshots
    ROUND_SNAPSHOTS.with_borrow_mut(|m| {
        m.clear_new();
    });

    // Clear round history entries
    ROUND_HISTORY_ENTRIES.with_borrow_mut(|m| {
        m.clear_new();
    });

    // Clear player round refs
    PLAYER_ROUND_REFS.with_borrow_mut(|m| {
        m.clear_new();
    });

    // Clear seed-related data
    SEED_CHUNKS.with_borrow_mut(|m| {
        m.clear_new();
    });

    CHUNK_OFFSETS.with_borrow_mut(|m| {
        m.clear_new();
    });

    REVEALED_SEEDS.with_borrow_mut(|m| {
        m.clear_new();
    });

    LAST_SETTLED_ROUNDS.with_borrow_mut(|m| {
        m.clear_new();
    });

    Ok(())
}
