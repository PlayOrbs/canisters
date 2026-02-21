use candid::{CandidType, Deserialize};
use serde::Serialize;

use crate::state::{RoundPlayerSnapshot, RoundSnapshot};

// ------------------ Player API Types ------------------

#[derive(CandidType, Deserialize, Debug)]
pub struct PlayerPage {
    pub total: u64,
    pub players: Vec<Vec<u8>>,
}

// ------------------ Round Snapshot API Types ------------------

#[derive(CandidType, Deserialize, Serialize)]
pub struct RoundPlayerSnapshotInput {
    pub player: String,  // base58-encoded pubkey
    pub join_ts: u64,
    pub tp_preset: u8,
    pub payout_lamports: u64,
    pub placement: u8,
    pub kills: u8,
    #[serde(default)]
    pub orb_earned_atoms: u64, // ORB tokens earned from emissions (in atoms)
    #[serde(default)]
    pub player_config_hash: Option<String>, // V2: hex-encoded 32-byte commitment hash
}

impl RoundPlayerSnapshotInput {
    pub fn into_storage(self) -> Result<RoundPlayerSnapshot, String> {
        let bytes = bs58::decode(&self.player)
            .into_vec()
            .map_err(|e| format!("invalid base58 for player: {}", e))?;
        
        let player: [u8; 32] = bytes
            .try_into()
            .map_err(|_| "player pubkey must be 32 bytes".to_string())?;
        
        // Parse player_config_hash if provided (hex string -> [u8; 32])
        let player_config_hash = if let Some(hash_hex) = self.player_config_hash {
            let hash_bytes = hex::decode(&hash_hex)
                .map_err(|e| format!("invalid hex for player_config_hash: {}", e))?;
            let hash: [u8; 32] = hash_bytes
                .try_into()
                .map_err(|_| "player_config_hash must be 32 bytes".to_string())?;
            Some(hash)
        } else {
            None
        };
        
        Ok(RoundPlayerSnapshot {
            player,
            join_ts: self.join_ts,
            tp_preset: self.tp_preset,
            payout_lamports: self.payout_lamports,
            placement: self.placement,
            kills: self.kills,
            orb_earned_atoms: self.orb_earned_atoms,
            player_config_hash,
        })
    }
}

#[derive(CandidType, Deserialize)]
pub struct RoundSnapshotPage {
    pub total: u64,
    pub snapshots: Vec<RoundSnapshot>,
}

// ------------------ Player Round History API Types ------------------

#[derive(CandidType, Deserialize, Serialize, Clone)]
pub struct PlayerRoundHistoryEntry {
    pub tier_id: u8,
    pub round_id: u64,
    pub season_id: u16,
    pub join_ts: u64,
    pub placement: u8,
    pub kills: u8,
    pub sol_earned_lamports: u64,
    #[serde(default)]
    pub orb_earned_atoms: u64, // ORB tokens earned from emissions (in atoms)
}

#[derive(CandidType, Deserialize)]
pub struct PlayerRoundHistoryPage {
    pub total: u64,
    pub rounds: Vec<PlayerRoundHistoryEntry>,
}
