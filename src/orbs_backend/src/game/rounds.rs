//! Round snapshot and player history management.

use crate::admin::ensure_admin;
use crate::crypto::Cbor;
use crate::state::{
    PlayerRoundRef, RoundHistoryEntry, RoundSnapshot, PLAYER_ROUND_REFS,
    ROUND_HISTORY_ENTRIES, ROUND_SNAPSHOTS, round_snapshot_key, get_current_config_version,
};
use crate::shared::{PlayerRoundHistoryEntry, PlayerRoundHistoryPage, RoundPlayerSnapshotInput, RoundSnapshotPage, vec_to_pk32};

/// Store a round snapshot with player data.
/// Supports batched calls - will append to existing snapshot if it exists.
/// Uses the current global config version for new snapshots.
pub fn store_round_snapshot(
    tier_id: u8,
    round_id: u64,
    season_id: u16,
    players: Vec<RoundPlayerSnapshotInput>,
    did_emit: bool,
    emit_tx_sig: Option<String>,
) -> Result<(), String> {
    ensure_admin();

    const MAX_PLAYERS_PER_CALL: usize = 50;
    if players.len() > MAX_PLAYERS_PER_CALL {
        return Err(format!(
            "too many players in batch: {} (max {}). Call multiple times to append.",
            players.len(),
            MAX_PLAYERS_PER_CALL
        ));
    }

    let key = round_snapshot_key(tier_id, round_id);

    let new_players: Result<Vec<_>, String> =
        players.into_iter().map(|p| p.into_storage()).collect();

    let new_players = new_players?;

    ROUND_SNAPSHOTS.with(|m| {
        let mut map = m.borrow_mut();

        if let Some(existing) = map.get(&key) {
            // Append to existing snapshot, deduplicating by player pubkey
            let mut snapshot = existing.0.clone();
            for new_player in new_players.clone() {
                // Only add if player not already in snapshot
                if !snapshot
                    .players
                    .iter()
                    .any(|p| p.player == new_player.player)
                {
                    snapshot.players.push(new_player);
                }
            }
            // Update did_emit if true (once emitted, always emitted)
            if did_emit {
                snapshot.did_emit = true;
            }
            // Update emit_tx_sig if provided
            if emit_tx_sig.is_some() {
                snapshot.emit_tx_sig = emit_tx_sig.clone();
            }
            map.insert(key, Cbor(snapshot));
        } else {
            // Create new snapshot with current global config version
            let config_version = get_current_config_version();
            let snapshot = RoundSnapshot {
                tier_id,
                round_id,
                season_id,
                players: new_players.clone(),
                did_emit,
                emit_tx_sig,
                config_version,
            };
            map.insert(key, Cbor(snapshot));
        }
    });

    // Create or update shared round history entry
    let round_key = round_snapshot_key(tier_id, round_id);
    ROUND_HISTORY_ENTRIES.with(|m| {
        let mut map = m.borrow_mut();
        if !map.contains_key(&round_key) {
            let entry = RoundHistoryEntry {
                tier_id,
                round_id,
                season_id,
            };
            map.insert(round_key, Cbor(entry));
        }
    });

    // Update player round references
    PLAYER_ROUND_REFS.with(|m| {
        let mut map = m.borrow_mut();

        for player in new_players {
            let player_ref = PlayerRoundRef {
                round_key,
                join_ts: player.join_ts,
                placement: player.placement,
                kills: player.kills,
                sol_earned_lamports: player.payout_lamports,
                orb_earned_atoms: player.orb_earned_atoms,
            };

            if let Some(existing) = map.get(&player.player) {
                let mut refs = existing.0.clone();
                // Only add if this round not already referenced
                if !refs.iter().any(|r| r.round_key == round_key) {
                    refs.push(player_ref);
                    // Keep sorted by join_ts (most recent first)
                    refs.sort_by(|a, b| b.join_ts.cmp(&a.join_ts));
                    map.insert(player.player, Cbor(refs));
                }
            } else {
                // First round for this player
                map.insert(player.player, Cbor(vec![player_ref]));
            }
        }
    });

    Ok(())
}

/// Get a single round snapshot by tier and round ID.
pub fn get_round_snapshot(tier_id: u8, round_id: u64) -> Option<RoundSnapshot> {
    let key = round_snapshot_key(tier_id, round_id);
    ROUND_SNAPSHOTS.with(|m| m.borrow().get(&key).map(|cbor_data| cbor_data.0.clone()))
}

/// Get paginated round snapshots for a tier.
pub fn get_round_snapshots_by_tier(tier_id: u8, offset: u64, limit: u64) -> RoundSnapshotPage {
    let skip = offset as usize;
    let take = limit as usize;

    ROUND_SNAPSHOTS.with_borrow(|map| {
        // Filter by tier_id by checking each snapshot
        let mut all_snapshots: Vec<RoundSnapshot> = Vec::new();

        for key in map.keys() {
            // Extract tier_id from key (upper 8 bits)
            let key_tier = (key >> 64) as u8;
            if key_tier == tier_id {
                if let Some(cbor) = map.get(&key) {
                    all_snapshots.push(cbor.0.clone());
                }
            }
        }

        let total = all_snapshots.len() as u64;

        if take == 0 {
            return RoundSnapshotPage {
                total,
                snapshots: vec![],
            };
        }

        let snapshots: Vec<RoundSnapshot> =
            all_snapshots.into_iter().skip(skip).take(take).collect();

        RoundSnapshotPage { total, snapshots }
    })
}

/// Update the config version of a specific round snapshot (admin only).
/// Used to fix rounds that were created with the wrong config version.
pub fn update_round_config_version(
    tier_id: u8,
    round_id: u64,
    config_version: String,
) -> Result<(), String> {
    ensure_admin();

    let key = round_snapshot_key(tier_id, round_id);

    ROUND_SNAPSHOTS.with(|m| {
        let mut map = m.borrow_mut();
        if let Some(existing) = map.get(&key) {
            let mut snapshot = existing.0.clone();
            snapshot.config_version = config_version;
            map.insert(key, Cbor(snapshot));
            Ok(())
        } else {
            Err(format!(
                "Round snapshot not found for tier {} round {}",
                tier_id, round_id
            ))
        }
    })
}

/// Get paginated round history for a player.
pub fn get_player_round_history(
    player_pubkey: Vec<u8>,
    offset: u64,
    limit: u64,
) -> Result<PlayerRoundHistoryPage, String> {
    let player: [u8; 32] = vec_to_pk32(player_pubkey)?;

    let skip = offset as usize;
    let take = limit as usize;

    PLAYER_ROUND_REFS.with_borrow(|refs_map| {
        if let Some(refs_cbor) = refs_map.get(&player) {
            let refs = &refs_cbor.0;
            let total = refs.len() as u64;

            if take == 0 {
                return Ok(PlayerRoundHistoryPage {
                    total,
                    rounds: vec![],
                });
            }

            // Get the player refs for this page
            let page_refs: Vec<&PlayerRoundRef> = refs.iter().skip(skip).take(take).collect();

            // Combine PlayerRoundRef data with RoundHistoryEntry data
            let rounds: Vec<PlayerRoundHistoryEntry> = ROUND_HISTORY_ENTRIES
                .with_borrow(|entries_map| {
                    page_refs
                        .iter()
                        .filter_map(|r| {
                            entries_map.get(&r.round_key).map(|entry| {
                                PlayerRoundHistoryEntry {
                                    tier_id: entry.0.tier_id,
                                    round_id: entry.0.round_id,
                                    season_id: entry.0.season_id,
                                    join_ts: r.join_ts,
                                    placement: r.placement,
                                    kills: r.kills,
                                    sol_earned_lamports: r.sol_earned_lamports,
                                    orb_earned_atoms: r.orb_earned_atoms,
                                }
                            })
                        })
                        .collect()
                });

            Ok(PlayerRoundHistoryPage { total, rounds })
        } else {
            // Player has no history
            Ok(PlayerRoundHistoryPage {
                total: 0,
                rounds: vec![],
            })
        }
    })
}
