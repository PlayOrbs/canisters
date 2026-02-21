//! ICP Canister entry points.
//!
//! This module exposes all canister endpoints and delegates to service modules:
//! - `admin` - Admin helpers and initialization
//! - `players` - Player management
//! - `rounds` - Round snapshots and history
//! - `seeds` - Seed generation and Merkle proofs
//! - `engine_config` - Game engine configuration
//! - `data` - Data management utilities

use candid::Principal;
use ic_cdk::api::msg_caller;
use ic_cdk::{export_candid, init, post_upgrade, query, update};

use crate::admin::ensure_admin;
use crate::state::{EngineConfig, RoundSnapshot};
use crate::shared::{
    PlayerPage, PlayerRoundHistoryPage, RoundPlayerSnapshotInput, RoundSnapshotPage,
};
use crate::game::{PlayerJoinDataInput, PlayerJoinDataOutput, PlayerConfigInput, PlayerConfigOutput};

// ==================== SECURITY CONSTANTS ====================

/// Maximum items per pagination request
const MAX_PAGE_LIMIT: u64 = 100;
/// Maximum players per batch add
const MAX_PLAYERS_BATCH: usize = 200;
/// Maximum pubkey size (Solana = 32 bytes)
const PUBKEY_SIZE: usize = 32;
/// Maximum engine config JSON size (64 KB)
const MAX_CONFIG_JSON_SIZE: usize = 65_536;
/// Maximum admins during init
const MAX_INIT_ADMINS: usize = 10;
/// Maximum players per round snapshot batch
const MAX_SNAPSHOT_PLAYERS: usize = 50;
/// Maximum engine config versions to list
const MAX_CONFIG_VERSIONS_LIST: usize = 100;
/// Maximum player configs to list per round
const MAX_PLAYER_CONFIGS_LIST: usize = 100;
/// Hash size (32 bytes for SHA-256)
const HASH_SIZE: usize = 32;

// ==================== VALIDATION HELPERS ====================

/// Reject anonymous callers for sensitive operations.
#[inline]
fn reject_anonymous() {
    if msg_caller() == Principal::anonymous() {
        ic_cdk::trap("anonymous caller not allowed");
    }
}

/// Clamp pagination limit to safe bounds.
#[inline]
fn clamp_limit(limit: u64) -> u64 {
    limit.min(MAX_PAGE_LIMIT)
}

// Service modules
pub mod admin;
pub mod crypto;
pub mod game;
pub mod seeds;
pub mod shared;
pub mod state;

// ==================== INITIALIZATION ====================

#[init]
fn init(admins: Vec<Principal>) {
    // Bound admin list size to prevent DoS during init
    if admins.len() > MAX_INIT_ADMINS {
        ic_cdk::trap("too many initial admins");
    }
    // Filter out anonymous principal
    let valid_admins: Vec<Principal> = admins
        .into_iter()
        .filter(|p| *p != Principal::anonymous())
        .collect();
    admin::init_admins(valid_admins);
}

#[post_upgrade]
fn post_upgrade() {
    // Migration: Update round 283 tier 0 to use config version 2.0.0
    // This round was created before we set the current config version to 2.0.0
    let _ = game::update_round_config_version(0, 283, "2.0.0".to_string());
}

// ==================== ADMIN API ====================

#[update]
fn set_admin(new_admin: Principal) {
    reject_anonymous();
    if new_admin == Principal::anonymous() {
        ic_cdk::trap("cannot add anonymous as admin");
    }
    // Note: add_admin internally calls ensure_admin()
    admin::add_admin(new_admin);
}

#[query]
fn list_admins() -> Vec<Principal> {
    state::ADMINS.with(|a| {
        let map = a.borrow();
        let mut admins = Vec::new();
        for entry in map.iter() {
            admins.push(entry.key().clone());
        }
        admins
    })
}

#[update]
async fn init_orbs_ic_pubkey() -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    admin::init_pubkey().await
}

#[query]
fn get_orbs_ic_pubkey() -> Result<String, String> {
    admin::get_pubkey_hex()
}

// ==================== PLAYERS API ====================

#[update]
fn add_players(pubkeys: Vec<Vec<u8>>) -> Result<(), String> {
    reject_anonymous();
    // Bound batch size
    if pubkeys.len() > MAX_PLAYERS_BATCH {
        return Err(format!(
            "batch too large: {} (max {})",
            pubkeys.len(),
            MAX_PLAYERS_BATCH
        ));
    }
    // Validate each pubkey size before passing to service
    for pk in &pubkeys {
        if pk.len() != PUBKEY_SIZE {
            return Err(format!(
                "invalid pubkey size: {} (expected {})",
                pk.len(),
                PUBKEY_SIZE
            ));
        }
    }
    game::add_players(pubkeys)
}

#[query]
fn player_exists(pubkey: Vec<u8>) -> bool {
    if pubkey.len() != PUBKEY_SIZE {
        ic_cdk::trap("invalid pubkey size");
    }
    let mut pk = [0u8; 32];
    pk.copy_from_slice(&pubkey);
    game::player_exists(pk)
}

#[query]
fn get_players(offset: u64, limit: u64) -> PlayerPage {
    game::get_players(offset, clamp_limit(limit))
}

// ==================== SEED API ====================

#[query]
fn get_chunk_size() -> u64 {
    seeds::CHUNK_SIZE
}

#[update]
async fn refresh_chunks_for_tier(
    tier_id: u8,
    round_id: u64,
) -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    seeds::refresh_chunks_for_tier(tier_id, round_id).await
}

#[update]
async fn reveal_seed_for_round(
    tier_id: u8,
    round_id: u64,
) -> Result<seeds::SeedProof, String> {
    reject_anonymous();
    ensure_admin();
    seeds::reveal_seed(tier_id, round_id).await
}

#[query]
fn chunk_exists(tier_id: u8) -> bool {
    seeds::chunk_exists(tier_id)
}

#[query]
fn get_revealed_seed(tier_id: u8, round_id: u64) -> Option<seeds::SeedProof> {
    let key = state::revealed_seed_key(tier_id, round_id);
    state::REVEALED_SEEDS.with(|m| m.borrow().get(&key).map(|cbor| cbor.0.clone()))
}

#[query]
fn get_chunk_offset(tier_id: u8) -> u64 {
    seeds::get_chunk_offset(tier_id)
}

#[update]
fn set_chunk_offset(tier_id: u8, offset: u64) -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    seeds::set_chunk_offset(tier_id, offset);
    Ok(())
}

#[update]
fn clear_revealed_seed(tier_id: u8, round_id: u64) -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    seeds::clear_revealed_seed(tier_id, round_id);
    Ok(())
}

#[query]
fn get_last_settled_round(tier_id: u8) -> u64 {
    seeds::get_last_settled_round(tier_id)
}

#[update]
fn set_last_settled_round(tier_id: u8, round_id: u64) -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    seeds::set_last_settled_round(tier_id, round_id);
    Ok(())
}

// ==================== ROUNDS API ====================

#[update]
fn store_round_snapshot(
    tier_id: u8,
    round_id: u64,
    season_id: u16,
    players: Vec<RoundPlayerSnapshotInput>,
    did_emit: bool,
    emit_tx_sig: Option<String>,
) -> Result<(), String> {
    reject_anonymous();
    // Bound players per call (service also checks, but fail fast here)
    if players.len() > MAX_SNAPSHOT_PLAYERS {
        return Err(format!(
            "too many players: {} (max {})",
            players.len(),
            MAX_SNAPSHOT_PLAYERS
        ));
    }
    game::store_round_snapshot(tier_id, round_id, season_id, players, did_emit, emit_tx_sig)
}

#[query]
fn get_round_snapshot(tier_id: u8, round_id: u64) -> Option<RoundSnapshot> {
    game::get_round_snapshot(tier_id, round_id)
}

#[update]
fn update_round_config_version(
    tier_id: u8,
    round_id: u64,
    config_version: String,
) -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    // Validate version string length
    if config_version.len() > 32 {
        return Err("config version too long (max 32 chars)".to_string());
    }
    game::update_round_config_version(tier_id, round_id, config_version)
}

#[query]
fn get_round_snapshots_by_tier(tier_id: u8, offset: u64, limit: u64) -> RoundSnapshotPage {
    game::get_round_snapshots_by_tier(tier_id, offset, clamp_limit(limit))
}

#[query]
fn get_player_round_history(
    player_pubkey: Vec<u8>,
    offset: u64,
    limit: u64,
) -> Result<PlayerRoundHistoryPage, String> {
    // Validate pubkey size at entry
    if player_pubkey.len() != PUBKEY_SIZE {
        return Err(format!(
            "invalid pubkey size: {} (expected {})",
            player_pubkey.len(),
            PUBKEY_SIZE
        ));
    }
    game::get_player_round_history(player_pubkey, offset, clamp_limit(limit))
}

// ==================== PLAYER JOIN DATA API ====================

#[update]
fn store_player_join_data(input: PlayerJoinDataInput) -> Result<(), String> {
    reject_anonymous();
    // Validate pubkey size at entry
    if input.player.len() != PUBKEY_SIZE {
        return Err(format!(
            "invalid pubkey size: {} (expected {})",
            input.player.len(),
            PUBKEY_SIZE
        ));
    }
    game::store_player_join_data(input)
}

#[query]
fn get_player_join_data(
    player: Vec<u8>,
    tier_id: u8,
    round_id: u64,
) -> Result<Option<state::PlayerJoinData>, String> {
    if player.len() != PUBKEY_SIZE {
        return Err(format!(
            "invalid pubkey size: {} (expected {})",
            player.len(),
            PUBKEY_SIZE
        ));
    }
    game::get_player_join_data(player, tier_id, round_id)
}

#[query]
fn get_round_join_data(tier_id: u8, round_id: u64) -> Vec<PlayerJoinDataOutput> {
    game::get_round_join_data(tier_id, round_id)
}

#[query]
fn get_player_round_seed(
    tier_id: u8,
    round_id: u64,
    player: Vec<u8>,
) -> Result<Vec<u8>, String> {
    if player.len() != PUBKEY_SIZE {
        return Err(format!(
            "invalid pubkey size: {} (expected {})",
            player.len(),
            PUBKEY_SIZE
        ));
    }
    game::get_player_round_seed(tier_id, round_id, player)
}

// ==================== PLAYER CONFIG API (v2) ====================

#[update]
fn set_player_config(input: PlayerConfigInput) -> Result<(), String> {
    reject_anonymous();
    // Validate hash and pubkey sizes at entry
    if input.player_config_hash.len() != HASH_SIZE {
        return Err(format!(
            "invalid player_config_hash size: {} (expected {})",
            input.player_config_hash.len(),
            HASH_SIZE
        ));
    }
    if input.player_pubkey.len() != PUBKEY_SIZE {
        return Err(format!(
            "invalid player_pubkey size: {} (expected {})",
            input.player_pubkey.len(),
            PUBKEY_SIZE
        ));
    }
    game::set_player_config(input)
}

#[query]
fn get_player_config(
    round_id: u64,
    tier_id: u8,
    player_config_hash: Vec<u8>,
) -> Result<Option<PlayerConfigOutput>, String> {
    if player_config_hash.len() != HASH_SIZE {
        return Err(format!(
            "invalid player_config_hash size: {} (expected {})",
            player_config_hash.len(),
            HASH_SIZE
        ));
    }
    game::get_player_config(round_id, tier_id, player_config_hash)
}

/// List player configs for a round (admin only).
/// ADMIN ONLY - spawn positions are hidden until seed reveal to prevent gaming.
/// Engine-runner uses this during settlement.
#[query]
fn list_player_configs(round_id: u64, tier_id: u8) -> Result<Vec<PlayerConfigOutput>, String> {
    ensure_admin();
    let all = game::list_player_configs(round_id, tier_id);
    // Bound response size
    if all.len() > MAX_PLAYER_CONFIGS_LIST {
        Ok(all.into_iter().take(MAX_PLAYER_CONFIGS_LIST).collect())
    } else {
        Ok(all)
    }
}

/// List player configs for a revealed round (public).
/// Only returns configs if the round's seed has been revealed.
/// Frontend uses this to load spawn positions for replay.
#[query]
fn list_player_configs_if_revealed(round_id: u64, tier_id: u8) -> Result<Vec<PlayerConfigOutput>, String> {
    let all = game::list_player_configs_if_revealed(round_id, tier_id)?;
    // Bound response size
    if all.len() > MAX_PLAYER_CONFIGS_LIST {
        Ok(all.into_iter().take(MAX_PLAYER_CONFIGS_LIST).collect())
    } else {
        Ok(all)
    }
}

// ==================== DATA MANAGEMENT ====================

#[update]
fn clear_all_data() -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    admin::clear_all_data()
}

// ==================== ENGINE CONFIG API ====================

#[update]
fn add_engine_config(version: String, config_json: String) -> Result<(), String> {
    reject_anonymous();
    // Bound config JSON size to prevent memory exhaustion
    if config_json.len() > MAX_CONFIG_JSON_SIZE {
        return Err(format!(
            "config_json too large: {} bytes (max {})",
            config_json.len(),
            MAX_CONFIG_JSON_SIZE
        ));
    }
    // Validate version string length
    if version.len() > 16 {
        return Err(format!(
            "version too long: {} (max 16)",
            version.len()
        ));
    }
    game::add_engine_config(version, config_json)
}

#[update]
fn update_engine_config(version: String, config_json: String) -> Result<(), String> {
    reject_anonymous();
    if config_json.len() > MAX_CONFIG_JSON_SIZE {
        return Err(format!(
            "config_json too large: {} bytes (max {})",
            config_json.len(),
            MAX_CONFIG_JSON_SIZE
        ));
    }
    if version.len() > 16 {
        return Err(format!(
            "version too long: {} (max 16)",
            version.len()
        ));
    }
    game::update_engine_config(version, config_json)
}

#[query]
fn get_engine_config(version: String) -> Option<EngineConfig> {
    game::get_engine_config(version)
}

#[query]
fn get_latest_engine_config_version() -> Option<String> {
    game::get_latest_engine_config_version()
}

#[query]
fn get_latest_engine_config() -> Option<EngineConfig> {
    game::get_latest_engine_config()
}

#[query]
fn list_engine_config_versions() -> Vec<EngineConfig> {
    // Bound response size - return at most MAX_CONFIG_VERSIONS_LIST
    let all = game::list_engine_config_versions();
    if all.len() > MAX_CONFIG_VERSIONS_LIST {
        all.into_iter().take(MAX_CONFIG_VERSIONS_LIST).collect()
    } else {
        all
    }
}

#[query]
fn get_current_config_version() -> String {
    state::get_current_config_version()
}

#[update]
fn set_current_config_version(version: String) -> Result<(), String> {
    reject_anonymous();
    ensure_admin();
    // Validate version string length
    if version.len() > 16 {
        return Err(format!(
            "version too long: {} (max 16)",
            version.len()
        ));
    }
    // Verify the config version exists
    if game::get_engine_config(version.clone()).is_none() {
        return Err(format!(
            "Engine config version {} does not exist. Add it first with add_engine_config.",
            version
        ));
    }
    state::set_current_config_version(version);
    Ok(())
}

export_candid!();
