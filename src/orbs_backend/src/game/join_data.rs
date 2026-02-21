//! Player join data management.
//!
//! Stores spawn position and skill allocation from the matrix game.
//! Data is keyed by (player, tier, round) for efficient lookup.

use candid::{CandidType, Deserialize};
use ic_cdk::api::time;
use serde::Serialize;
use sha2::Digest;

use crate::admin::ensure_admin;
use crate::crypto::Cbor;
use crate::seeds::get_raw_seed_for_round;
use crate::state::{
    parse_player_join_key, player_join_key, PlayerJoinData,
    PLAYER_JOIN_DATA,
};
use crate::shared::vec_to_pk32;

/// Input struct for storing player join data
#[derive(CandidType, Deserialize, Serialize, Clone, Debug)]
pub struct PlayerJoinDataInput {
    pub player: Vec<u8>, // 32 bytes pubkey
    pub tier_id: u8,
    pub round_id: u64,
    pub spawn_x_norm: f32,
    pub spawn_y_norm: f32,
    pub spawn_rot_rad: f32,
    pub earned_sp: u8,
    pub alloc_split_aggro: u8,
    pub alloc_tether_res: u8,
    pub alloc_orb_power: u8,
}

/// Output struct for player join data with player pubkey included
#[derive(CandidType, Deserialize, Serialize, Clone, Debug)]
pub struct PlayerJoinDataOutput {
    pub player: Vec<u8>, // 32 bytes pubkey
    pub spawn_x_norm: f32,
    pub spawn_y_norm: f32,
    pub spawn_rot_rad: f32,
    pub earned_sp: u8,
    pub alloc_split_aggro: u8,
    pub alloc_tether_res: u8,
    pub alloc_orb_power: u8,
    pub stored_at: u64,
}

/// Store player join data (spawn position, skill allocation).
/// Called by Matrix Worker when player submits matrix results.
/// Admin only.
pub fn store_player_join_data(input: PlayerJoinDataInput) -> Result<(), String> {
    ensure_admin();

    // Validate player pubkey
    let player: [u8; 32] = vec_to_pk32(input.player)?;

    // Validate spawn position is within unit circle
    let dist_sq = input.spawn_x_norm * input.spawn_x_norm + input.spawn_y_norm * input.spawn_y_norm;
    if dist_sq > 1.0 {
        return Err("spawn position outside unit circle".to_string());
    }

    // Validate rotation is finite
    if !input.spawn_rot_rad.is_finite() {
        return Err("spawn rotation must be finite".to_string());
    }

    // Validate allocation doesn't exceed earned SP
    let total_alloc = input.alloc_split_aggro as u16
        + input.alloc_tether_res as u16
        + input.alloc_orb_power as u16;
    if total_alloc > input.earned_sp as u16 {
        return Err(format!(
            "allocation sum {} exceeds earned_sp {}",
            total_alloc, input.earned_sp
        ));
    }

    let key = player_join_key(&player, input.tier_id, input.round_id);
    let data = PlayerJoinData {
        spawn_x_norm: input.spawn_x_norm,
        spawn_y_norm: input.spawn_y_norm,
        spawn_rot_rad: input.spawn_rot_rad,
        earned_sp: input.earned_sp,
        alloc_split_aggro: input.alloc_split_aggro,
        alloc_tether_res: input.alloc_tether_res,
        alloc_orb_power: input.alloc_orb_power,
        stored_at: time() / 1_000_000_000, // Convert nanoseconds to seconds
    };

    PLAYER_JOIN_DATA.with(|m| {
        m.borrow_mut().insert(key, Cbor(data));
    });

    Ok(())
}

/// Get player join data for a specific player in a round.
pub fn get_player_join_data(
    player: Vec<u8>,
    tier_id: u8,
    round_id: u64,
) -> Result<Option<PlayerJoinData>, String> {
    let player: [u8; 32] = vec_to_pk32(player)?;
    let key = player_join_key(&player, tier_id, round_id);

    Ok(PLAYER_JOIN_DATA.with(|m| m.borrow().get(&key).map(|cbor| cbor.0.clone())))
}

/// Get all player join data for a round.
/// Returns Vec of (player_pubkey, data) pairs.
/// Uses full scan with filter (acceptable for small datasets).
pub fn get_round_join_data(tier_id: u8, round_id: u64) -> Vec<PlayerJoinDataOutput> {
    PLAYER_JOIN_DATA.with(|m| {
        let map = m.borrow();
        let mut results = Vec::new();

        for key in map.keys() {
            let (player, t, r) = parse_player_join_key(&key);
            if t == tier_id && r == round_id {
                if let Some(cbor) = map.get(&key) {
                    results.push(PlayerJoinDataOutput {
                        player: player.to_vec(),
                        spawn_x_norm: cbor.0.spawn_x_norm,
                        spawn_y_norm: cbor.0.spawn_y_norm,
                        spawn_rot_rad: cbor.0.spawn_rot_rad,
                        earned_sp: cbor.0.earned_sp,
                        alloc_split_aggro: cbor.0.alloc_split_aggro,
                        alloc_tether_res: cbor.0.alloc_tether_res,
                        alloc_orb_power: cbor.0.alloc_orb_power,
                        stored_at: cbor.0.stored_at,
                    });
                }
            }
        }

        results
    })
}

/// Get player-specific matrix seed derived from round seed.
/// seed = sha256(round_seed || tier_id || player || round_id)
///
/// Uses INTERNAL seed from SEED_CHUNKS (not publicly revealed yet).
/// Returns error if seed not ready for this round.
pub fn get_player_round_seed(
    tier_id: u8,
    round_id: u64,
    player: Vec<u8>,
) -> Result<Vec<u8>, String> {
    // Validate player pubkey
    let player: [u8; 32] = vec_to_pk32(player)?;

    // Get internal round seed from SEED_CHUNKS
    let round_seed = get_internal_round_seed(tier_id, round_id)?;

    // Derive player-specific seed
    // seed = sha256(round_seed || tier_id || player || round_id)
    let mut hasher = sha2::Sha256::new();
    hasher.update(&round_seed);
    hasher.update(&[tier_id]);
    hasher.update(&player);
    hasher.update(&round_id.to_le_bytes());
    let result = hasher.finalize();

    Ok(result.to_vec())
}

/// Get round seed from internal SEED_CHUNKS storage.
/// Delegates to the shared function in seeds module to avoid duplication.
fn get_internal_round_seed(tier_id: u8, round_id: u64) -> Result<[u8; 32], String> {
    get_raw_seed_for_round(tier_id, round_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_player_join_key_roundtrip() {
        let player = [42u8; 32];
        let tier_id = 2u8;
        let round_id = 12345u64;

        let key = player_join_key(&player, tier_id, round_id);
        let (p, t, r) = parse_player_join_key(&key);

        assert_eq!(p, player);
        assert_eq!(t, tier_id);
        assert_eq!(r, round_id);
    }
}
