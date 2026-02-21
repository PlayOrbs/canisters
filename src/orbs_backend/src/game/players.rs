//! Player management functions.

use crate::admin::ensure_admin;
use crate::state::PLAYERS;
use crate::shared::{PlayerPage, vec_to_pk32};

/// Batch add players: takes Vec<Vec<u8>> and inserts all valid 32-byte keys.
pub fn add_players(pubkeys: Vec<Vec<u8>>) -> Result<(), String> {
    ensure_admin();
    let keys: Vec<[u8; 32]> = pubkeys
        .into_iter()
        .map(vec_to_pk32)
        .collect::<Result<Vec<_>, _>>()?;

    PLAYERS.with(|m| {
        let mut map = m.borrow_mut();
        for k in keys {
            map.insert(k, true);
        }
    });

    Ok(())
}

/// Check if a player exists in the registry.
pub fn player_exists(pubkey: [u8; 32]) -> bool {
    PLAYERS.with_borrow(|map| map.contains_key(&pubkey))
}

/// Get paginated list of players.
pub fn get_players(offset: u64, limit: u64) -> PlayerPage {
    let skip = offset as usize;
    let take = limit as usize;

    PLAYERS.with_borrow(|map| {
        let total = map.len() as u64;
        if take == 0 {
            return PlayerPage {
                total,
                players: vec![],
            };
        }

        let mut out = Vec::with_capacity(take);
        let mut idx = 0;

        for key in map.keys() {
            if idx < skip {
                idx += 1;
                continue;
            }

            if out.len() >= take {
                break;
            }

            out.push(key.to_vec());
            idx += 1;
        }

        PlayerPage {
            total,
            players: out,
        }
    })
}
