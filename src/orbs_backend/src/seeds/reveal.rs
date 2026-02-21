//! Single-chunk seed management with Merkle proofs.
//!
//! Each tier has ONE chunk holding 50 unrevealed seeds.
//! Seeds move to REVEALED_SEEDS as they're revealed.
//! When chunk is empty (offset reaches 50), it's regenerated fresh.
//!
//! Note: season_id has been removed - round IDs are globally unique per tier.

use crate::crypto::Cbor;
use super::chunks::{self, SeedProof, CHUNK_SIZE};
use crate::state::{seed_chunk_key, chunk_offset_key, revealed_seed_key, SEED_CHUNKS, CHUNK_OFFSETS, LAST_SETTLED_ROUNDS, REVEALED_SEEDS};

/// Generate and store a fresh chunk for a tier.
/// The chunk_id is derived from the round_id to ensure correct Merkle tree generation.
pub async fn generate_chunk(tier_id: u8, round_id: u64) -> Result<(), String> {
    // chunk_id = (round_id - 1) / CHUNK_SIZE
    // Subtract 1 because rounds are 1-indexed but chunk calculation is 0-based
    let chunk_id = if round_id > 0 {
        (round_id - 1) / CHUNK_SIZE
    } else {
        0
    };
    
    // Generate the chunk with Merkle tree and signature
    let chunk = chunks::generate_seed_chunk(tier_id, chunk_id).await?;
    
    // Store in bucket (one chunk per tier)
    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS.with(|m| {
        m.borrow_mut().insert(key, Cbor(chunk));
    });
    
    Ok(())
}

/// Get a seed proof for the next seed in the chunk.
/// offset_in_chunk determines which seed (0-49) to extract.
pub fn get_seed_proof(tier_id: u8, offset_in_chunk: u64) -> Result<SeedProof, String> {
    let key = seed_chunk_key(tier_id);
    
    SEED_CHUNKS.with(|m| {
        let map = m.borrow();
        let chunk = map.get(&key)
            .ok_or_else(|| format!("Chunk not found for tier={}", tier_id))?;
        
        // Extract proof for the specific offset within the chunk
        chunks::get_seed_proof_by_offset(&chunk.0, offset_in_chunk)
    })
}

/// Check if a chunk exists for the tier.
pub fn chunk_exists(tier_id: u8) -> bool {
    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS.with(|m| m.borrow().contains_key(&key))
}

/// Get raw seed bytes for a round from the internal chunk (not revealed yet).
/// Used for deriving player-specific seeds for matrix game.
/// Returns error if chunk doesn't exist or doesn't contain the round.
pub fn get_raw_seed_for_round(tier_id: u8, round_id: u64) -> Result<[u8; 32], String> {
    if round_id == 0 {
        return Err("round_id must be >= 1".to_string());
    }

    let expected_chunk_id = (round_id - 1) / CHUNK_SIZE;
    let offset = ((round_id - 1) % CHUNK_SIZE) as usize;
    let key = seed_chunk_key(tier_id);

    SEED_CHUNKS.with(|m| {
        let map = m.borrow();
        match map.get(&key) {
            Some(cbor) => {
                let chunk = &cbor.0;
                if chunk.chunk_id != expected_chunk_id {
                    return Err(format!(
                        "seed chunk mismatch: have chunk {} but need chunk {} for round {}",
                        chunk.chunk_id, expected_chunk_id, round_id
                    ));
                }
                if offset >= chunk.seeds.len() {
                    return Err(format!(
                        "seed offset {} out of bounds for chunk (size {})",
                        offset, chunk.seeds.len()
                    ));
                }
                Ok(chunk.seeds[offset])
            }
            None => Err(format!(
                "no seed chunk for tier {}. Call refresh_chunks_for_tier first.",
                tier_id
            )),
        }
    })
}

/// Get the chunk_id of the currently stored chunk for a tier.
/// Returns None if no chunk exists.
pub fn get_stored_chunk_id(tier_id: u8) -> Option<u64> {
    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS.with(|m| m.borrow().get(&key).map(|cbor| cbor.0.chunk_id))
}

/// Get the current offset within the chunk (0-49).
pub fn get_chunk_offset(tier_id: u8) -> u64 {
    let offset_key = chunk_offset_key(tier_id);
    CHUNK_OFFSETS.with(|m| m.borrow().get(&offset_key).unwrap_or(0))
}

/// Increment the chunk offset. Returns the new offset.
/// When offset reaches CHUNK_SIZE, it wraps to 0 (chunk needs regeneration).
pub fn increment_offset(tier_id: u8) -> u64 {
    let offset_key = chunk_offset_key(tier_id);
    CHUNK_OFFSETS.with(|m| {
        let mut map = m.borrow_mut();
        let current = map.get(&offset_key).unwrap_or(0);
        let new_offset = current + 1;
        map.insert(offset_key, new_offset);
        new_offset
    })
}

/// Check if chunk needs regeneration (all seeds used).
pub fn chunk_needs_regen(tier_id: u8) -> bool {
    let offset = get_chunk_offset(tier_id);
    offset >= CHUNK_SIZE
}

/// Regenerate the chunk (called when all seeds are revealed).
/// Uses the next round_id to generate the correct chunk.
pub async fn regenerate_chunk(tier_id: u8, next_round_id: u64) -> Result<(), String> {
    generate_chunk(tier_id, next_round_id).await
}

/// Ensure the correct chunk exists for the given round_id, generating if needed.
/// This checks both existence AND that the chunk_id matches what's needed for the round.
pub async fn ensure_chunk(tier_id: u8, round_id: u64) -> Result<(), String> {
    let expected_chunk_id = if round_id > 0 {
        (round_id - 1) / CHUNK_SIZE
    } else {
        0
    };
    
    // Check if we have the correct chunk, not just any chunk
    let needs_generation = match get_stored_chunk_id(tier_id) {
        None => true,
        Some(stored_chunk_id) => stored_chunk_id != expected_chunk_id,
    };
    
    if needs_generation {
        generate_chunk(tier_id, round_id).await?;
    }
    
    Ok(())
}

/// Emergency refresh: regenerate the chunk.
pub async fn refresh_chunks_for_tier(
    tier_id: u8,
    round_id: u64,
) -> Result<(), String> {
    regenerate_chunk(tier_id, round_id).await
}

/// Result of reveal validation
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevealValidation {
    /// Valid: round can be revealed
    Valid,
    /// Invalid: wrong round requested
    InvalidRound {
        expected: u64,
        last_settled: u64,
        requested: u64,
    },
}

/// Validate that a round can be revealed (sequential access check).
/// Returns Ok(()) if valid, Err with details if not.
pub fn validate_reveal_round(
    tier_id: u8,
    round_id: u64,
) -> RevealValidation {
    let tier_key = chunk_offset_key(tier_id);

    let last_settled = LAST_SETTLED_ROUNDS.with(|m| m.borrow().get(&tier_key).unwrap_or(0));

    // Simple sequential access - no cross-season logic needed since round IDs are global
    let expected_round = if last_settled == 0 {
        // No settled rounds yet - allow round 0 or 1
        if round_id <= 1 { round_id } else { 1 }
    } else {
        // Normal sequential access
        last_settled + 1
    };

    if round_id != expected_round {
        RevealValidation::InvalidRound {
            expected: expected_round,
            last_settled,
            requested: round_id,
        }
    } else {
        RevealValidation::Valid
    }
}

/// Reveal a seed for a specific round.
/// 
/// This is the main entry point for seed revelation. It:
/// 1. Checks if already revealed (idempotent - returns existing proof)
/// 2. Validates sequential access (round must be last_settled + 1)
/// 3. Ensures chunk exists (generates if needed)
/// 4. Extracts the seed proof
/// 5. Stores in REVEALED_SEEDS for public access
/// 6. Updates last_settled_round
/// 7. Increments offset (triggers chunk regeneration if exhausted)
///
/// **Security**: Only allows requesting the next sequential round.
/// This prevents pre-fetching future seeds.
/// 
/// **Idempotency**: If the round was already revealed, returns the existing
/// proof instead of failing. This handles race conditions gracefully.
pub async fn reveal_seed(
    tier_id: u8,
    round_id: u64,
) -> Result<SeedProof, String> {
    // Step 0: Check if already revealed (idempotent handling for race conditions)
    let revealed_key = revealed_seed_key(tier_id, round_id);
    if let Some(existing_proof) = REVEALED_SEEDS.with(|m| m.borrow().get(&revealed_key).map(|cbor| cbor.0.clone())) {
        return Ok(existing_proof);
    }

    // Step 1: Validate sequential access
    match validate_reveal_round(tier_id, round_id) {
        RevealValidation::Valid => {},
        RevealValidation::InvalidRound { expected, last_settled, requested } => {
            return Err(format!(
                "Invalid round_id: expected {} (last settled: {}), got {}",
                expected, last_settled, requested
            ));
        }
    }

    // Step 2: Ensure chunk exists for this round_id
    ensure_chunk(tier_id, round_id).await?;

    // Step 3: Compute offset from round_id (not from stored counter)
    // offset_in_chunk = (round_id - 1) % CHUNK_SIZE because rounds are 1-indexed
    let offset_in_chunk = (round_id - 1) % CHUNK_SIZE;
    let proof = get_seed_proof(tier_id, offset_in_chunk)?;

    // Step 4: Store in REVEALED_SEEDS for public access
    REVEALED_SEEDS.with(|m| {
        m.borrow_mut().insert(revealed_key, Cbor(proof.clone()));
    });

    // Step 5: Update last settled round
    let tier_key = chunk_offset_key(tier_id);
    LAST_SETTLED_ROUNDS.with(|m| {
        m.borrow_mut().insert(tier_key, round_id);
    });

    // Step 6: Increment offset (move to next seed in chunk)
    let new_offset = increment_offset(tier_id);

    // If chunk is exhausted (all 50 seeds used), regenerate it for the next round
    if new_offset % CHUNK_SIZE == 0 {
        let next_round = round_id + 1;
        // Spawn async regeneration (fire-and-forget)
        ic_cdk::futures::spawn(async move {
            let _ = regenerate_chunk(tier_id, next_round).await;
        });
    }

    Ok(proof)
}

/// Get the last settled round for a tier.
pub fn get_last_settled_round(tier_id: u8) -> u64 {
    let tier_key = chunk_offset_key(tier_id);
    LAST_SETTLED_ROUNDS.with(|m| m.borrow().get(&tier_key).unwrap_or(0))
}

/// Reset the last settled round for a tier (for testing/admin purposes).
pub fn reset_last_settled_round(tier_id: u8) {
    let tier_key = chunk_offset_key(tier_id);
    LAST_SETTLED_ROUNDS.with(|m| {
        m.borrow_mut().remove(&tier_key);
    });
}

/// Set the chunk offset for a tier (admin recovery).
/// Use this to fix offset after bugs or to align with last_settled_round.
/// offset should be: last_settled_round (since rounds are 1-indexed and offset is 0-indexed)
pub fn set_chunk_offset(tier_id: u8, offset: u64) {
    let offset_key = chunk_offset_key(tier_id);
    CHUNK_OFFSETS.with(|m| {
        m.borrow_mut().insert(offset_key, offset);
    });
}

/// Clear a revealed seed (admin recovery).
/// Use this to force re-reveal of a seed after chunk regeneration.
pub fn clear_revealed_seed(tier_id: u8, round_id: u64) {
    let key = revealed_seed_key(tier_id, round_id);
    REVEALED_SEEDS.with(|m| {
        m.borrow_mut().remove(&key);
    });
}

/// Set the last settled round for a tier (admin recovery).
pub fn set_last_settled_round(tier_id: u8, round_id: u64) {
    let tier_key = chunk_offset_key(tier_id);
    LAST_SETTLED_ROUNDS.with(|m| {
        m.borrow_mut().insert(tier_key, round_id);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reset_state(tier_id: u8) {
        // Reset last settled round
        reset_last_settled_round(tier_id);
        
        // Reset chunk offset
        let offset_key = chunk_offset_key(tier_id);
        CHUNK_OFFSETS.with(|m| {
            m.borrow_mut().remove(&offset_key);
        });
    }

    // ==================== validate_reveal_round tests ====================

    #[test]
    fn test_validate_first_round_zero() {
        let tier_id = 100; // Use unique tier_id for test isolation
        reset_state(tier_id);

        // First round (0) should be valid when last_settled is 0
        let result = validate_reveal_round(tier_id, 0);
        assert_eq!(result, RevealValidation::Valid);
    }

    #[test]
    fn test_validate_first_round_one() {
        let tier_id = 101;
        reset_state(tier_id);

        // First round (1) should be valid when last_settled is 0
        let result = validate_reveal_round(tier_id, 1);
        assert_eq!(result, RevealValidation::Valid);
    }

    #[test]
    fn test_validate_sequential_access() {
        let tier_id = 102;
        reset_state(tier_id);

        // Simulate that round 5 was last settled
        let tier_key = chunk_offset_key(tier_id);
        LAST_SETTLED_ROUNDS.with(|m| {
            m.borrow_mut().insert(tier_key, 5);
        });

        // Round 6 should be valid (last_settled + 1)
        let result = validate_reveal_round(tier_id, 6);
        assert_eq!(result, RevealValidation::Valid);
    }

    #[test]
    fn test_validate_reject_skip_ahead() {
        let tier_id = 103;
        reset_state(tier_id);

        // Simulate that round 5 was last settled
        let tier_key = chunk_offset_key(tier_id);
        LAST_SETTLED_ROUNDS.with(|m| {
            m.borrow_mut().insert(tier_key, 5);
        });

        // Round 7 should be rejected (skipping round 6)
        let result = validate_reveal_round(tier_id, 7);
        assert_eq!(
            result,
            RevealValidation::InvalidRound {
                expected: 6,
                last_settled: 5,
                requested: 7,
            }
        );
    }

    #[test]
    fn test_validate_reject_replay() {
        let tier_id = 104;
        reset_state(tier_id);

        // Simulate that round 5 was last settled
        let tier_key = chunk_offset_key(tier_id);
        LAST_SETTLED_ROUNDS.with(|m| {
            m.borrow_mut().insert(tier_key, 5);
        });

        // Round 5 should be rejected (already settled)
        let result = validate_reveal_round(tier_id, 5);
        assert_eq!(
            result,
            RevealValidation::InvalidRound {
                expected: 6,
                last_settled: 5,
                requested: 5,
            }
        );
    }

    #[test]
    fn test_validate_different_tiers_independent() {
        let tier_id_0 = 105;
        let tier_id_1 = 106;
        reset_state(tier_id_0);
        reset_state(tier_id_1);

        // Set tier 0 at round 5
        let tier_key_0 = chunk_offset_key(tier_id_0);
        LAST_SETTLED_ROUNDS.with(|m| {
            m.borrow_mut().insert(tier_key_0, 5);
        });

        // Set tier 1 at round 10
        let tier_key_1 = chunk_offset_key(tier_id_1);
        LAST_SETTLED_ROUNDS.with(|m| {
            m.borrow_mut().insert(tier_key_1, 10);
        });

        // Tier 0 should expect round 6
        let result_0 = validate_reveal_round(tier_id_0, 6);
        assert_eq!(result_0, RevealValidation::Valid);

        // Tier 1 should expect round 11
        let result_1 = validate_reveal_round(tier_id_1, 11);
        assert_eq!(result_1, RevealValidation::Valid);
    }

    // ==================== Offset tracking tests ====================

    #[test]
    fn test_chunk_offset_starts_at_zero() {
        let tier_id = 200;
        reset_state(tier_id);

        let offset = get_chunk_offset(tier_id);
        assert_eq!(offset, 0);
    }

    #[test]
    fn test_increment_offset() {
        let tier_id = 201;
        reset_state(tier_id);

        assert_eq!(get_chunk_offset(tier_id), 0);
        
        let new_offset = increment_offset(tier_id);
        assert_eq!(new_offset, 1);
        assert_eq!(get_chunk_offset(tier_id), 1);

        let new_offset = increment_offset(tier_id);
        assert_eq!(new_offset, 2);
    }

    #[test]
    fn test_chunk_needs_regen_at_boundary() {
        let tier_id = 202;
        reset_state(tier_id);

        // Set offset to CHUNK_SIZE - 1
        let offset_key = chunk_offset_key(tier_id);
        CHUNK_OFFSETS.with(|m| {
            m.borrow_mut().insert(offset_key, CHUNK_SIZE - 1);
        });

        assert!(!chunk_needs_regen(tier_id));

        // Increment to CHUNK_SIZE
        increment_offset(tier_id);
        assert!(chunk_needs_regen(tier_id));
    }

    // ==================== get_last_settled_round tests ====================

    #[test]
    fn test_get_last_settled_round_default() {
        let tier_id = 203;
        reset_state(tier_id);

        let last = get_last_settled_round(tier_id);
        assert_eq!(last, 0);
    }

    #[test]
    fn test_get_last_settled_round_after_set() {
        let tier_id = 204;
        reset_state(tier_id);

        set_last_settled_round(tier_id, 42);
        let last = get_last_settled_round(tier_id);
        assert_eq!(last, 42);
    }

    // ==================== chunk_exists tests ====================

    #[test]
    fn test_chunk_exists_false_initially() {
        let tier_id = 205;
        reset_state(tier_id);

        // Clear any existing chunk
        let key = seed_chunk_key(tier_id);
        SEED_CHUNKS.with(|m| {
            m.borrow_mut().remove(&key);
        });

        assert!(!chunk_exists(tier_id));
    }

    // ==================== get_stored_chunk_id tests ====================

    #[test]
    fn test_get_stored_chunk_id_none_when_empty() {
        let tier_id = 206;
        reset_state(tier_id);

        // Clear any existing chunk
        let key = seed_chunk_key(tier_id);
        SEED_CHUNKS.with(|m| {
            m.borrow_mut().remove(&key);
        });

        assert_eq!(get_stored_chunk_id(tier_id), None);
    }

    #[test]
    fn test_expected_chunk_id_calculation() {
        // Verify chunk_id calculation matches what ensure_chunk expects
        // Round 1-50 -> chunk 0
        assert_eq!((1 - 1) / CHUNK_SIZE, 0);
        assert_eq!((50 - 1) / CHUNK_SIZE, 0);
        // Round 51-100 -> chunk 1
        assert_eq!((51 - 1) / CHUNK_SIZE, 1);
        assert_eq!((100 - 1) / CHUNK_SIZE, 1);
        // Round 201-250 -> chunk 4
        assert_eq!((201 - 1) / CHUNK_SIZE, 4);
    }
}
