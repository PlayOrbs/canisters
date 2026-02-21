//! Player config record management (v2).
//!
//! Stores quantized spawn position and skill allocation with commitment hash.
//! Data is keyed by (round_id, tier_id, player_config_hash) for efficient lookup.
//! Immutable once written - engine verifies hash matches Solana commitment.

use candid::{CandidType, Deserialize};
use ic_cdk::api::time;
use serde::Serialize;

use crate::admin::ensure_admin;
use crate::crypto::Cbor;
use crate::shared::vec_to_pk32;
#[cfg(test)]
use crate::state::parse_player_config_key;
use crate::state::{PLAYER_CONFIGS, PlayerConfigRecord, player_config_key, player_config_prefix};

/// Input struct for storing player config record
#[derive(CandidType, Deserialize, Serialize, Clone, Debug)]
pub struct PlayerConfigInput {
    pub player_config_hash: Vec<u8>, // 32 bytes (key + commitment)
    pub round_id: u64,
    pub tier_id: u8,
    pub player_pubkey: Vec<u8>, // 32 bytes
    pub tp_preset: u16,
    pub spawn_x_q: i16,
    pub spawn_y_q: i16,
    pub spawn_rot_q: u16,
    pub alloc_split: u8,
    pub alloc_tether: u8,
    pub alloc_power: u8,
}

/// Output struct for player config record
#[derive(CandidType, Deserialize, Serialize, Clone, Debug)]
pub struct PlayerConfigOutput {
    pub player_config_hash: Vec<u8>, // 32 bytes (key + commitment)
    pub round_id: u64,
    pub tier_id: u8,
    pub player_pubkey: Vec<u8>, // 32 bytes
    pub tp_preset: u16,
    pub spawn_x_q: i16,
    pub spawn_y_q: i16,
    pub spawn_rot_q: u16,
    pub alloc_split: u8,
    pub alloc_tether: u8,
    pub alloc_power: u8,
    pub created_at: u64,
}

impl From<PlayerConfigRecord> for PlayerConfigOutput {
    fn from(r: PlayerConfigRecord) -> Self {
        PlayerConfigOutput {
            player_config_hash: r.player_config_hash.to_vec(),
            round_id: r.round_id,
            tier_id: r.tier_id,
            player_pubkey: r.player_pubkey.to_vec(),
            tp_preset: r.tp_preset,
            spawn_x_q: r.spawn_x_q,
            spawn_y_q: r.spawn_y_q,
            spawn_rot_q: r.spawn_rot_q,
            alloc_split: r.alloc_split,
            alloc_tether: r.alloc_tether,
            alloc_power: r.alloc_power,
            created_at: r.created_at,
        }
    }
}

/// Store player config record.
/// Called by Matrix Worker when player submits matrix results.
/// Admin only. Immutable - rejects overwrites for same player_config_hash.
pub fn set_player_config(input: PlayerConfigInput) -> Result<(), String> {
    ensure_admin();

    // Validate player_config_hash
    if input.player_config_hash.len() != 32 {
        return Err(format!(
            "player_config_hash must be 32 bytes, got {}",
            input.player_config_hash.len()
        ));
    }
    let mut player_config_hash = [0u8; 32];
    player_config_hash.copy_from_slice(&input.player_config_hash);

    // Validate player pubkey
    let player_pubkey: [u8; 32] = vec_to_pk32(input.player_pubkey)?;

    // Note: spawn_x_q and spawn_y_q are i16, spawn_rot_q is u16
    // All values are inherently within valid ranges due to type limits

    // Validate tp_preset
    if input.tp_preset > 4 {
        return Err(format!("tp_preset must be 0-4, got {}", input.tp_preset));
    }

    let key = player_config_key(input.round_id, input.tier_id, &player_config_hash);

    // Check for existing record - immutability enforcement
    let exists = PLAYER_CONFIGS.with(|m| m.borrow().contains_key(&key));
    if exists {
        return Err("player config already exists for this hash (immutable)".to_string());
    }

    let record = PlayerConfigRecord {
        player_config_hash,
        round_id: input.round_id,
        tier_id: input.tier_id,
        player_pubkey,
        tp_preset: input.tp_preset,
        spawn_x_q: input.spawn_x_q,
        spawn_y_q: input.spawn_y_q,
        spawn_rot_q: input.spawn_rot_q,
        alloc_split: input.alloc_split,
        alloc_tether: input.alloc_tether,
        alloc_power: input.alloc_power,
        created_at: time() / 1_000_000_000, // Convert nanoseconds to seconds
    };

    PLAYER_CONFIGS.with(|m| {
        m.borrow_mut().insert(key, Cbor(record));
    });

    Ok(())
}

/// Get player config record by player_config_hash.
pub fn get_player_config(
    round_id: u64,
    tier_id: u8,
    player_config_hash: Vec<u8>,
) -> Result<Option<PlayerConfigOutput>, String> {
    if player_config_hash.len() != 32 {
        return Err(format!(
            "player_config_hash must be 32 bytes, got {}",
            player_config_hash.len()
        ));
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&player_config_hash);

    let key = player_config_key(round_id, tier_id, &hash);

    Ok(PLAYER_CONFIGS.with(|m| m.borrow().get(&key).map(|cbor| cbor.0.clone().into())))
}

/// Get all player config records for a round (admin only).
/// Uses range scan with prefix for efficiency.
pub fn list_player_configs(round_id: u64, tier_id: u8) -> Vec<PlayerConfigOutput> {
    ensure_admin();
    list_player_configs_internal(round_id, tier_id)
}

/// Get all player config records for a revealed round (public).
/// Only returns configs if the round's seed has been revealed.
pub fn list_player_configs_if_revealed(round_id: u64, tier_id: u8) -> Result<Vec<PlayerConfigOutput>, String> {
    use crate::state::{revealed_seed_key, REVEALED_SEEDS};
    
    // Check if seed is revealed for this round
    let key = revealed_seed_key(tier_id, round_id);
    let is_revealed = REVEALED_SEEDS.with(|m| m.borrow().contains_key(&key));
    
    if !is_revealed {
        return Err(format!(
            "Round {} tier {} seed not yet revealed - configs are private",
            round_id, tier_id
        ));
    }
    
    Ok(list_player_configs_internal(round_id, tier_id))
}

/// Internal implementation for listing player configs.
fn list_player_configs_internal(round_id: u64, tier_id: u8) -> Vec<PlayerConfigOutput> {
    let prefix = player_config_prefix(round_id, tier_id);

    PLAYER_CONFIGS.with(|m| {
        let map = m.borrow();
        let mut results = Vec::new();

        // Iterate through all keys and filter by prefix
        // StableBTreeMap doesn't have range() so we use full scan with prefix check
        for key in map.keys() {
            // Check if key starts with our prefix (round_id + tier_id)
            if key[0..9] == prefix {
                if let Some(cbor) = map.get(&key) {
                    results.push(cbor.0.clone().into());
                }
            }
        }

        results
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_player_config_key_roundtrip() {
        let round_id = 12345u64;
        let tier_id = 2u8;
        let player_config_hash = [42u8; 32];

        let key = player_config_key(round_id, tier_id, &player_config_hash);
        let (r, t, h) = parse_player_config_key(&key);

        assert_eq!(r, round_id);
        assert_eq!(t, tier_id);
        assert_eq!(h, player_config_hash);
    }

    #[test]
    fn test_player_config_prefix() {
        let round_id = 12345u64;
        let tier_id = 2u8;

        let prefix = player_config_prefix(round_id, tier_id);
        let key = player_config_key(round_id, tier_id, &[0u8; 32]);

        assert_eq!(&key[0..9], &prefix);
    }

    #[test]
    fn test_key_derivation_different_hashes() {
        let round_id = 100u64;
        let tier_id = 0u8;
        
        let hash1 = [1u8; 32];
        let hash2 = [2u8; 32];
        
        let key1 = player_config_key(round_id, tier_id, &hash1);
        let key2 = player_config_key(round_id, tier_id, &hash2);
        
        // Keys should be different for different hashes
        assert_ne!(key1, key2);
        
        // But prefix should be same (same round + tier)
        assert_eq!(&key1[0..9], &key2[0..9]);
    }

    #[test]
    fn test_key_derivation_different_rounds() {
        let hash = [42u8; 32];
        
        let key1 = player_config_key(100, 0, &hash);
        let key2 = player_config_key(101, 0, &hash);
        
        // Keys should be different for different rounds
        assert_ne!(key1, key2);
        
        // Prefixes should also be different
        assert_ne!(&key1[0..9], &key2[0..9]);
    }

    #[test]
    fn test_key_derivation_different_tiers() {
        let hash = [42u8; 32];
        
        let key1 = player_config_key(100, 0, &hash);
        let key2 = player_config_key(100, 1, &hash);
        
        // Keys should be different for different tiers
        assert_ne!(key1, key2);
        
        // Prefixes should also be different
        assert_ne!(&key1[0..9], &key2[0..9]);
    }
}
