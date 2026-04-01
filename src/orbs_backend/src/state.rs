use std::cell::RefCell;

use candid::{CandidType, Principal};
use ic_stable_structures::{
    DefaultMemoryImpl, StableBTreeMap, StableCell,
    memory_manager::{MemoryId, MemoryManager, VirtualMemory},
};
use serde::{Deserialize, Serialize};

use crate::crypto::Cbor;

#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct RoundPlayerSnapshot {
    pub player: [u8; 32],
    pub join_ts: u64,
    pub tp_preset: u8,
    pub payout_lamports: u64,
    pub placement: u8,
    pub kills: u8,
    #[serde(default)]
    pub orb_earned_atoms: u64, // ORB tokens earned from emissions (in atoms)
    #[serde(default)]
    pub player_config_hash: Option<[u8; 32]>, // V2: commitment hash for PlayerConfigRecord lookup
}

#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct RoundSnapshot {
    pub tier_id: u8,
    pub round_id: u64,
    pub season_id: u16,
    pub players: Vec<RoundPlayerSnapshot>,
    #[serde(default)]
    pub did_emit: bool, // Whether ORB emission occurred for this round
    #[serde(default)]
    pub emit_tx_sig: Option<String>, // Solana tx signature for ORB mint (base58)
    #[serde(default = "default_config_version")]
    pub config_version: String, // Engine config version used for this round (e.g. "1.2.2")
    #[serde(default = "default_payout_model")]
    pub payout_model: String, // Payout model used: "v1_inherit" or "v2_top3"
}

fn default_payout_model() -> String {
    "v1_inherit".to_string()
}

fn default_config_version() -> String {
    "1.2.2".to_string()
}

/// Player join data for matrix game (spawn position, skill allocation)
/// Stored per player per round, keyed by (player[32], tier[1], round[8])
/// DEPRECATED: Use PlayerConfigRecord for new rounds (v2)
#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct PlayerJoinData {
    pub spawn_x_norm: f32,      // Normalized spawn X [-1, 1]
    pub spawn_y_norm: f32,      // Normalized spawn Y [-1, 1]
    pub spawn_rot_rad: f32,     // Spawn direction in radians [0, 2π]
    pub earned_sp: u8,          // Skill points earned from matrix game
    pub alloc_split_aggro: u8,  // Points allocated to split aggro
    pub alloc_tether_res: u8,   // Points allocated to tether resistance
    pub alloc_orb_power: u8,    // Points allocated to orb power
    pub stored_at: u64,         // Timestamp when stored (for cleanup)
}

/// Key for PlayerJoinData storage: player(32) + tier(1) + round(8) = 41 bytes
pub type PlayerJoinKey = [u8; 41];

/// Create key for player join data storage
pub fn player_join_key(player: &[u8; 32], tier_id: u8, round_id: u64) -> PlayerJoinKey {
    let mut key = [0u8; 41];
    key[0..32].copy_from_slice(player);
    key[32] = tier_id;
    key[33..41].copy_from_slice(&round_id.to_be_bytes());
    key
}

/// Parse components from player join key
pub fn parse_player_join_key(key: &PlayerJoinKey) -> ([u8; 32], u8, u64) {
    let mut player = [0u8; 32];
    player.copy_from_slice(&key[0..32]);
    let tier_id = key[32];
    let round_id = u64::from_be_bytes(key[33..41].try_into().unwrap());
    (player, tier_id, round_id)
}

#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct RoundHistoryEntry {
    pub tier_id: u8,
    pub round_id: u64,
    pub season_id: u16,
}

#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct PlayerRoundRef {
    pub round_key: u128,          // composite key (tier_id, round_id)
    pub join_ts: u64,             // when player joined
    pub placement: u8,            // 1 = first, 2 = second, etc. 0 = participation only
    pub kills: u8,                // number of kills
    pub sol_earned_lamports: u64, // SOL prize earned (in lamports)
    #[serde(default)]
    pub orb_earned_atoms: u64,    // ORB tokens earned from emissions (in atoms)
}

/// Game engine configuration stored on-chain.
/// Immutable once written - new configs get new version numbers.
/// Used for deterministic replay verification.
#[derive(CandidType, Serialize, Deserialize, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct EngineConfig {
    /// Version string (unique identifier, e.g. "1.2.2")
    pub version: String,
    /// JSON string containing the full engine configuration
    pub config_json: String,
    /// Unix timestamp when this config was added
    pub created_at: u64,
}

/// Player config record for v2 rounds (quantized, no floats)
/// Stored per session, keyed by (round_id, tier_id, player_config_hash)
/// Immutable once written - engine verifies hash matches Solana commitment
#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct PlayerConfigRecord {
    pub player_config_hash: [u8; 32], // commitment hash (key + stored on Solana)
    pub round_id: u64,
    pub tier_id: u8,
    pub player_pubkey: [u8; 32],
    pub tp_preset: u16,
    pub spawn_x_q: i16,             // quantized X: [-32767, 32767] -> [-1, 1]
    pub spawn_y_q: i16,             // quantized Y: [-32767, 32767] -> [-1, 1]
    pub spawn_rot_q: u16,           // quantized rot: [0, 65535] -> [0, 2*PI)
    pub alloc_split: u8,
    pub alloc_tether: u8,
    pub alloc_power: u8,
    pub created_at: u64,            // timestamp for cleanup
}

/// Key for PlayerConfigRecord storage: round_id(8) + tier_id(1) + player_config_hash(32) = 41 bytes
pub type PlayerConfigKey = [u8; 41];

/// Create key for player config storage
pub fn player_config_key(round_id: u64, tier_id: u8, player_config_hash: &[u8; 32]) -> PlayerConfigKey {
    let mut key = [0u8; 41];
    key[0..8].copy_from_slice(&round_id.to_be_bytes());
    key[8] = tier_id;
    key[9..41].copy_from_slice(player_config_hash);
    key
}

/// Parse components from player config key
pub fn parse_player_config_key(key: &PlayerConfigKey) -> (u64, u8, [u8; 32]) {
    let round_id = u64::from_be_bytes(key[0..8].try_into().unwrap());
    let tier_id = key[8];
    let mut player_config_hash = [0u8; 32];
    player_config_hash.copy_from_slice(&key[9..41]);
    (round_id, tier_id, player_config_hash)
}

/// Create prefix key for iterating player configs by (round_id, tier_id)
pub fn player_config_prefix(round_id: u64, tier_id: u8) -> [u8; 9] {
    let mut prefix = [0u8; 9];
    prefix[0..8].copy_from_slice(&round_id.to_be_bytes());
    prefix[8] = tier_id;
    prefix
}

pub enum MemoryIndex {
    AdminMemory = 0,
    PlayersMemory = 1,
    PubKey = 2,
    RoundSnapshots = 3,
    RoundHistoryEntries = 4,
    PlayerRoundRefs = 5,
    SeedChunks = 6,        // Merkle-based seed chunks (hidden)
    ChunkOffsets = 7,      // Chunk offset tracking
    RevealedSeeds = 8,     // Publicly revealed seed proofs
    LastSettledRounds = 9, // Last settled round per (season, tier)
    EngineConfigs = 10,    // Game engine configs (version -> JSON, immutable)
    CurrentConfigVersion = 11, // Current active config version string
    PlayerJoinData = 12,   // Player join data (spawn, allocation) per player per round (DEPRECATED)
    PlayerConfigs = 13,    // Player config records for v2 rounds (quantized, immutable)
}

type VM = VirtualMemory<DefaultMemoryImpl>;

thread_local! {
    // List of principals that are allowed to call the canister
    pub static ADMINS: RefCell<StableBTreeMap<Principal, bool, VM>> = RefCell::new(
            StableBTreeMap::init(
                MEM_MGR.with(|m| m.borrow().get(MemoryId::new(MemoryIndex::AdminMemory as u8))),
            )
    );

    pub static MEM_MGR: RefCell<MemoryManager<DefaultMemoryImpl>> =
        RefCell::new(MemoryManager::init(DefaultMemoryImpl::default()));

    pub static PLAYERS: RefCell<StableBTreeMap<[u8; 32], bool, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::PlayersMemory as u8));
            StableBTreeMap::init(mem)
        })
    });

     // Cached ECDSA public key (uncompressed) as raw bytes.
    // Empty vec means "not cached yet".
    pub static PUBKEY_CELL: RefCell<StableCell<Vec<u8>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::PubKey as u8));
            StableCell::init(mem, Vec::new())
        })
    });

    // Round snapshots: (tier_id, round_id) -> RoundSnapshot
    // Key is u128: upper 8 bits = tier_id, lower 64 bits = round_id
    pub static ROUND_SNAPSHOTS: RefCell<StableBTreeMap<u128, Cbor<RoundSnapshot>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::RoundSnapshots as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Round history entries: (tier_id, round_id) -> RoundHistoryEntry
    // Shared entries that multiple players can reference
    pub static ROUND_HISTORY_ENTRIES: RefCell<StableBTreeMap<u128, Cbor<RoundHistoryEntry>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::RoundHistoryEntries as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Player round references: player_pubkey -> Vec<PlayerRoundRef>
    // Maps each player to their rounds with join timestamps
    pub static PLAYER_ROUND_REFS: RefCell<StableBTreeMap<[u8; 32], Cbor<Vec<PlayerRoundRef>>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::PlayerRoundRefs as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Single seed chunk per (season_id, tier_id) - holds 50 unrevealed seeds
    // Key: (season_id, tier_id) encoded as u32
    // Seeds move to REVEALED_SEEDS as they're revealed, chunk regenerates when empty
    pub static SEED_CHUNKS: RefCell<StableBTreeMap<u32, Cbor<crate::seeds::SeedChunk>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::SeedChunks as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Tracks the next seed index to use within the current chunk for each (season_id, tier_id)
    // Key: (season_id << 8) | tier_id, Value: next_offset within current chunk (0-49)
    // When offset reaches CHUNK_SIZE (50), chunk is regenerated and offset resets to 0
    pub static CHUNK_OFFSETS: RefCell<StableBTreeMap<u32, u64, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::ChunkOffsets as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Tracks the last settled round per (season_id, tier_id)
    // Key: (season_id << 8) | tier_id, Value: last_settled_round_id
    // Used to enforce sequential seed proof requests
    // MUST be in stable storage to survive canister upgrades!
    pub static LAST_SETTLED_ROUNDS: RefCell<StableBTreeMap<u32, u64, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::LastSettledRounds as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Publicly revealed seed proofs - only these are exposed via queries
    // Key: (season_id, tier_id, round_id) encoded as u128
    // Populated when get_seed_proof_for_round is called (after countdown ends)
    pub static REVEALED_SEEDS: RefCell<StableBTreeMap<u128, Cbor<crate::seeds::SeedProof>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::RevealedSeeds as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Game engine configs: version string -> EngineConfig
    // Immutable once written - new configs get new version numbers
    // Used to store deterministic game engine configuration for replay verification
    // Key is version string stored as fixed 16-byte array (padded with zeros)
    pub static ENGINE_CONFIGS: RefCell<StableBTreeMap<[u8; 16], Cbor<EngineConfig>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::EngineConfigs as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Current active config version - all new rounds use this version
    // Stored as Vec<u8> (UTF-8 bytes of version string like "1.2.2")
    // Default is "1.2.2" for backwards compatibility
    pub static CURRENT_CONFIG_VERSION: RefCell<StableCell<Vec<u8>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::CurrentConfigVersion as u8));
            StableCell::init(mem, "1.2.2".as_bytes().to_vec())
        })
    });

    // Player join data: (player, tier, round) -> PlayerJoinData
    // Stores spawn position and skill allocation from matrix game
    // Key is 41 bytes: player(32) + tier(1) + round(8)
    // DEPRECATED: Use PLAYER_CONFIGS for new rounds (v2)
    pub static PLAYER_JOIN_DATA: RefCell<StableBTreeMap<PlayerJoinKey, Cbor<PlayerJoinData>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::PlayerJoinData as u8));
            StableBTreeMap::init(mem)
        })
    });

    // Player config records for v2 rounds: (round_id, tier_id, start_token_hash) -> PlayerConfigRecord
    // Stores quantized spawn position and skill allocation with commitment hash
    // Key is 41 bytes: round_id(8) + tier_id(1) + start_token_hash(32)
    // Immutable once written - engine verifies hash matches Solana commitment
    pub static PLAYER_CONFIGS: RefCell<StableBTreeMap<PlayerConfigKey, Cbor<PlayerConfigRecord>, VM>> = RefCell::new({
        MEM_MGR.with(|m| {
            let mem = m.borrow().get(MemoryId::new(MemoryIndex::PlayerConfigs as u8));
            StableBTreeMap::init(mem)
        })
    });
}

/// Convert version string to fixed 16-byte key for ENGINE_CONFIGS storage
pub fn version_to_key(version: &str) -> [u8; 16] {
    let mut key = [0u8; 16];
    let bytes = version.as_bytes();
    let len = bytes.len().min(16);
    key[..len].copy_from_slice(&bytes[..len]);
    key
}

/// Create key for seed chunk storage: tier_id - one chunk per tier
/// Note: season_id removed - round IDs are globally unique per tier
pub fn seed_chunk_key(tier_id: u8) -> u32 {
    tier_id as u32
}

/// Create key for chunk offset tracking: tier_id
/// Note: season_id removed - round IDs are globally unique per tier
pub fn chunk_offset_key(tier_id: u8) -> u32 {
    tier_id as u32
}

// Helper to create composite key for round snapshots
pub fn round_snapshot_key(tier_id: u8, round_id: u64) -> u128 {
    ((tier_id as u128) << 64) | (round_id as u128)
}

/// Create key for revealed seed storage: (tier_id, round_id)
/// Note: season_id removed - round IDs are globally unique per tier
pub fn revealed_seed_key(tier_id: u8, round_id: u64) -> u128 {
    ((tier_id as u128) << 64) | (round_id as u128)
}

/// Get the current active config version string
pub fn get_current_config_version() -> String {
    CURRENT_CONFIG_VERSION.with(|c| {
        let bytes = c.borrow().get().clone();
        String::from_utf8(bytes).unwrap_or_else(|_| "1.2.2".to_string())
    })
}

/// Set the current active config version string (admin only - caller must verify)
pub fn set_current_config_version(version: String) {
    CURRENT_CONFIG_VERSION.with(|c| {
        let _ = c.borrow_mut().set(version.into_bytes());
    });
}
