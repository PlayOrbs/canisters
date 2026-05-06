//! Two-slot seed chunk management with Merkle proofs.
//!
//! Each tier has TWO chunk slots:
//! - CURRENT (`SEED_CHUNKS`): holds the chunk containing the round most recently
//!   revealed (and any rounds we are about to reveal in the same chunk).
//! - NEXT (`SEED_CHUNKS_NEXT`): holds the chunk that will become CURRENT after
//!   the upcoming chunk boundary. Prefetched inside `reveal_seed`.
//!
//! Read paths (`get_raw_seed_for_round`, indirectly `get_player_round_seed`)
//! consult both slots, so player-seed derivation works seamlessly across
//! chunk boundaries — even when matrix-worker queries arrive after the previous
//! round settled but before the current round has been revealed.
//!
//! Seeds move to REVEALED_SEEDS as they're revealed.
//!
//! Note: season_id has been removed - round IDs are globally unique per tier.

use crate::crypto::Cbor;
use super::chunks::{self, SeedProof, CHUNK_SIZE};
use crate::state::{seed_chunk_key, chunk_offset_key, revealed_seed_key, SEED_CHUNKS, SEED_CHUNKS_NEXT, CHUNK_OFFSETS, LAST_SETTLED_ROUNDS, REVEALED_SEEDS};

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

/// Get raw seed bytes for a round from the internal chunks (not revealed yet).
/// Used for deriving player-specific seeds for matrix game.
///
/// Consults both CURRENT (`SEED_CHUNKS`) and NEXT (`SEED_CHUNKS_NEXT`) — whichever
/// one holds `chunk_id = (round_id - 1) / CHUNK_SIZE` returns the seed. This is
/// what unblocks matrix-worker queries for the first round of a new chunk
/// (round 51, 101, …) before the boundary reveal has promoted NEXT into CURRENT.
///
/// Lookup order:
/// 1. CURRENT — the live chunk for the round most recently revealed.
/// 2. NEXT    — the prefetched chunk for the round-after-the-next-boundary.
/// 3. Otherwise return a structured error so matrix-worker can tell "not ready"
///    apart from "actually broken".
pub fn get_raw_seed_for_round(tier_id: u8, round_id: u64) -> Result<[u8; 32], String> {
    // Rounds are 1-indexed; round_id == 0 is never a valid request.
    if round_id == 0 {
        return Err("round_id must be >= 1".to_string());
    }

    // Decompose the round into (chunk_id, offset within chunk):
    //   chunk_id = (round_id - 1) / CHUNK_SIZE   (rounds 1..50 → chunk 0)
    //   offset   = (round_id - 1) % CHUNK_SIZE   (round 1 → offset 0; round 50 → offset 49)
    let expected_chunk_id = (round_id - 1) / CHUNK_SIZE;
    let offset = ((round_id - 1) % CHUNK_SIZE) as usize;
    // Both slots use the same per-tier key (only the map differs).
    let key = seed_chunk_key(tier_id);

    // ---- Try CURRENT slot first ----
    // Copy a single 32B seed inside the RefCell borrow — no 1.6KB Vec clone.
    // and_then short-circuits when the slot is empty or holds the wrong chunk.
    // seeds.get(offset).copied() returns None on out-of-bounds, which falls
    // through to NEXT and ultimately the structured error block. Chunks are
    // always generated with exactly CHUNK_SIZE seeds, so OOB on a chunk-matched
    // read indicates corruption — the structured error reports it descriptively.
    let from_current = SEED_CHUNKS.with(|m| {
        m.borrow().get(&key).and_then(|cbor| {
            if cbor.0.chunk_id == expected_chunk_id {
                cbor.0.seeds.get(offset).copied()
            } else {
                None
            }
        })
    });
    if let Some(seed) = from_current {
        return Ok(seed);
    }

    // ---- Fall through to NEXT slot ----
    // Same shape. NEXT is the chunk reveal_seed warmed up ahead of the boundary;
    // before promotion it's the only place the seeds for the upcoming chunk live.
    let from_next = SEED_CHUNKS_NEXT.with(|m| {
        m.borrow().get(&key).and_then(|cbor| {
            if cbor.0.chunk_id == expected_chunk_id {
                cbor.0.seeds.get(offset).copied()
            } else {
                None
            }
        })
    });
    if let Some(seed) = from_next {
        return Ok(seed);
    }

    // ---- Neither slot has the chunk we need ----
    // Build a precise error so matrix-worker's matcher can distinguish the
    // "totally cold" case (admin must seed chunks) from "wrong chunks loaded"
    // (race or sequencing issue).
    let current_id = get_stored_chunk_id(tier_id);
    let next_id = get_stored_chunk_id_next(tier_id);
    match (current_id, next_id) {
        // Cold start: no chunks exist yet for this tier. Bootstrap is automatic
        // via reveal_seed_for_round (engine-runner calls it on every settlement);
        // both CURRENT and NEXT get populated as a side-effect of the first
        // successful reveal. refresh_chunks_for_tier is the (dangerous)
        // CURRENT-regen recovery path and only relevant for corrupt-state repair.
        (None, None) => Err(format!(
            "no seed chunk for tier {}. Call reveal_seed_for_round to bootstrap.",
            tier_id
        )),
        // At least one slot is populated but neither holds the requested
        // chunk_id. Surface what's loaded so it's debuggable from logs.
        _ => Err(format!(
            "seed chunk mismatch: have current={:?} next={:?} but need chunk {} for round {}",
            current_id, next_id, expected_chunk_id, round_id
        )),
    }
}

/// Get the chunk_id of the currently stored chunk for a tier.
/// Returns None if no chunk exists.
pub fn get_stored_chunk_id(tier_id: u8) -> Option<u64> {
    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS.with(|m| m.borrow().get(&key).map(|cbor| cbor.0.chunk_id))
}

/// Get the chunk_id of the prefetched NEXT chunk for a tier (if any).
pub fn get_stored_chunk_id_next(tier_id: u8) -> Option<u64> {
    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS_NEXT.with(|m| m.borrow().get(&key).map(|cbor| cbor.0.chunk_id))
}

/// Check if the NEXT slot is populated for a tier.
pub fn next_chunk_exists(tier_id: u8) -> bool {
    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS_NEXT.with(|m| m.borrow().contains_key(&key))
}

/// Generate-into-NEXT if NEXT is empty or holds the wrong `chunk_id`.
/// Idempotent when NEXT already matches the expected `chunk_id`.
///
/// Reuses `chunks::generate_seed_chunk` so signing logic and Merkle layout are
/// identical to the existing CURRENT-slot generation. NEXT is never referenced
/// by outstanding `SeedProof` entries in `REVEALED_SEEDS`, so overwriting it is
/// safe — unlike CURRENT, where overwriting an in-use chunk would invalidate
/// every proof derived from it.
///
/// **Concurrency**: re-checks the slot AFTER the await on `generate_seed_chunk`
/// to catch the case where another in-flight call already populated NEXT with
/// the desired `chunk_id` while we were signing. The standard ic-cdk idiom for
/// idempotent updates: at most one redundant ECDSA sign per concurrent batch
/// (the loser drops their freshly-signed chunk on the floor instead of fighting
/// the winner's slot).
pub async fn ensure_next_chunk(
    tier_id: u8,
    expected_next_chunk_id: u64,
) -> Result<(), String> {
    // Pre-check: skip the sign entirely if NEXT already matches.
    if get_stored_chunk_id_next(tier_id) == Some(expected_next_chunk_id) {
        return Ok(());
    }

    let chunk = chunks::generate_seed_chunk(tier_id, expected_next_chunk_id).await?;

    // Post-await re-check: a concurrent reveal may have won the race while we
    // were awaiting sign_with_ecdsa. If so, our chunk goes to /dev/null —
    // safer than fighting their write (different chunks have different bytes
    // / signatures, so a clobber would invalidate any proof they already gave
    // out from that chunk).
    if get_stored_chunk_id_next(tier_id) == Some(expected_next_chunk_id) {
        return Ok(());
    }

    let key = seed_chunk_key(tier_id);
    SEED_CHUNKS_NEXT.with(|m| {
        m.borrow_mut().insert(key, Cbor(chunk));
    });

    Ok(())
}

/// Move the prefetched NEXT chunk into the CURRENT slot.
///
/// Used by `reveal_seed` at chunk boundaries: after the last seed of CURRENT
/// has been revealed, the chunk that was sitting in NEXT becomes the new CURRENT
/// and a fresh chunk is generated into NEXT. Errors if NEXT is empty (caller
/// falls back to inline regeneration via `ensure_chunk`).
pub fn promote_next_to_current(tier_id: u8) -> Result<(), String> {
    let key = seed_chunk_key(tier_id);
    let next_chunk = SEED_CHUNKS_NEXT.with(|m| m.borrow_mut().remove(&key));
    match next_chunk {
        Some(cbor) => {
            SEED_CHUNKS.with(|m| {
                m.borrow_mut().insert(key, cbor);
            });
            Ok(())
        }
        None => Err(format!(
            "promote_next_to_current: NEXT slot empty for tier {}",
            tier_id
        )),
    }
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

/// Emergency refresh: regenerate the CURRENT chunk for the tier.
///
/// **Dangerous — admin-only recovery path.** Unconditionally overwrites the
/// CURRENT slot (`SEED_CHUNKS`) with a freshly-generated chunk. Because master
/// seeds aren't stored, the new chunk has different bytes / Merkle root /
/// signature than whatever was there before. Any outstanding seed derivation
/// or reveal proof pinned to the previous CURRENT chunk is INVALIDATED.
///
/// Use only when CURRENT is actually corrupt or during fresh-deployment chunk
/// reset. NEXT-slot maintenance is handled automatically inside `reveal_seed`
/// (prefetch + promote on chunk boundary) — there's no operator-facing endpoint
/// for it because the auto-flow covers steady state and self-heals on the next
/// reveal after any panic.
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
/// 3. Ensures CURRENT chunk exists (generates if needed - cold start path)
/// 4. Extracts the seed proof from CURRENT
/// 5. Stores in REVEALED_SEEDS for public access
/// 6. Updates last_settled_round
/// 7. Increments offset
/// 8. **Boundary**: if we just exhausted CURRENT, promote NEXT → CURRENT and
///    generate a fresh NEXT (chunk_id + 1 ahead). All awaited - no fire-and-forget.
///    **Mid-chunk**: ensure NEXT holds the chunk after CURRENT (idempotent;
///    pays for one ECDSA sign on the first mid-chunk reveal of a chunk lifetime,
///    no-op thereafter).
///
/// **Security**: Only allows requesting the next sequential round (validated
/// in step 2). This prevents pre-fetching future seed proofs even though NEXT
/// is now warmed ahead of time — the prefetched chunk is only used internally
/// to derive `get_player_round_seed`, never returned as a `SeedProof`.
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

    // Step 2: Ensure CURRENT chunk exists for this round_id (cold-start path).
    // Mid-chunk this is a no-op; only the very first reveal of a tier ever
    // triggers generation here — all subsequent CURRENT updates happen via
    // promotion from NEXT in step 8.
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

    // Step 7: Compute the chunk_id of the round we just revealed.
    let current_chunk_id = (round_id - 1) / CHUNK_SIZE;

    // Step 8: Maintain the two-slot invariant.
    if new_offset % CHUNK_SIZE == 0 {
        // Boundary just crossed: CURRENT is exhausted. Promote NEXT → CURRENT
        // and generate the chunk after that into NEXT.
        //
        // Steady-state: NEXT already holds chunk_id == current_chunk_id + 1
        // from the previous mid-chunk reveal, so the pre-promote check is a
        // cheap map lookup (no ECDSA sign). The fallback ensure_next_chunk is
        // for cold-upgrade / race recovery only.
        if get_stored_chunk_id_next(tier_id) != Some(current_chunk_id + 1) {
            ensure_next_chunk(tier_id, current_chunk_id + 1).await?;
        }
        promote_next_to_current(tier_id)?;
        ensure_next_chunk(tier_id, current_chunk_id + 2).await?;
    } else {
        // Mid-chunk: keep NEXT warm so the upcoming boundary is a no-op promote.
        // Idempotent: only generates on the first mid-chunk reveal of a chunk
        // lifetime; every subsequent call sees the right chunk_id and returns.
        ensure_next_chunk(tier_id, current_chunk_id + 1).await?;
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

    // ==================== Two-slot (CURRENT + NEXT) tests ====================
    //
    // These tests construct SeedChunk values directly in the maps so they don't
    // need raw_rand / sign_with_ecdsa (which require an IC environment). The
    // Merkle tree + signature fields aren't relevant for read-path or promote
    // logic, so we leave them empty.

    fn build_test_chunk(tier_id: u8, chunk_id: u64, seed_byte: u8) -> super::super::chunks::SeedChunk {
        let mut seeds = Vec::with_capacity(CHUNK_SIZE as usize);
        for i in 0..CHUNK_SIZE {
            // Distinct, deterministic seed per offset, also encoding chunk_id
            // so different chunks have different seed bytes.
            let mut seed = [0u8; 32];
            seed[0] = seed_byte;
            seed[1] = chunk_id as u8;
            seed[2] = i as u8;
            seeds.push(seed);
        }
        super::super::chunks::SeedChunk {
            tier_id,
            chunk_id,
            seeds,
            merkle_layers: Vec::new(),
            root_signature: Vec::new(),
        }
    }

    fn clear_slots(tier_id: u8) {
        let key = seed_chunk_key(tier_id);
        SEED_CHUNKS.with(|m| {
            m.borrow_mut().remove(&key);
        });
        SEED_CHUNKS_NEXT.with(|m| {
            m.borrow_mut().remove(&key);
        });
    }

    fn put_current(tier_id: u8, chunk: super::super::chunks::SeedChunk) {
        let key = seed_chunk_key(tier_id);
        SEED_CHUNKS.with(|m| {
            m.borrow_mut().insert(key, Cbor(chunk));
        });
    }

    fn put_next(tier_id: u8, chunk: super::super::chunks::SeedChunk) {
        let key = seed_chunk_key(tier_id);
        SEED_CHUNKS_NEXT.with(|m| {
            m.borrow_mut().insert(key, Cbor(chunk));
        });
    }

    #[test]
    fn test_get_stored_chunk_id_next_none_when_empty() {
        let tier_id = 220;
        clear_slots(tier_id);
        assert_eq!(get_stored_chunk_id_next(tier_id), None);
        assert!(!next_chunk_exists(tier_id));
    }

    #[test]
    fn test_get_stored_chunk_id_next_returns_value() {
        let tier_id = 221;
        clear_slots(tier_id);
        put_next(tier_id, build_test_chunk(tier_id, 7, 0xaa));
        assert_eq!(get_stored_chunk_id_next(tier_id), Some(7));
        assert!(next_chunk_exists(tier_id));
    }

    #[test]
    fn test_promote_next_to_current_moves_chunk() {
        let tier_id = 222;
        clear_slots(tier_id);

        // Seed CURRENT with chunk 0 and NEXT with chunk 1.
        put_current(tier_id, build_test_chunk(tier_id, 0, 0x11));
        put_next(tier_id, build_test_chunk(tier_id, 1, 0x22));

        promote_next_to_current(tier_id).expect("promote should succeed");

        // CURRENT now holds the chunk that was in NEXT
        assert_eq!(get_stored_chunk_id(tier_id), Some(1));
        // NEXT is empty
        assert_eq!(get_stored_chunk_id_next(tier_id), None);
        assert!(!next_chunk_exists(tier_id));

        // Verify the actual seed bytes propagated (chunk 1, offset 0 → seed[1]=1)
        let seed = get_raw_seed_for_round(tier_id, 51).expect("lookup");
        assert_eq!(seed[0], 0x22);
        assert_eq!(seed[1], 1);
    }

    #[test]
    fn test_promote_errors_when_next_empty() {
        let tier_id = 223;
        clear_slots(tier_id);
        put_current(tier_id, build_test_chunk(tier_id, 0, 0x11));

        let err = promote_next_to_current(tier_id)
            .expect_err("should error when NEXT is empty");
        assert!(err.contains("NEXT slot empty"), "unexpected error: {}", err);

        // CURRENT untouched
        assert_eq!(get_stored_chunk_id(tier_id), Some(0));
    }

    #[test]
    fn test_get_raw_seed_for_round_finds_in_current() {
        let tier_id = 224;
        clear_slots(tier_id);
        put_current(tier_id, build_test_chunk(tier_id, 0, 0x33));
        put_next(tier_id, build_test_chunk(tier_id, 1, 0x44));

        // Round 25 → chunk 0, offset 24 → seed[2] == 24
        let seed = get_raw_seed_for_round(tier_id, 25).expect("lookup");
        assert_eq!(seed[0], 0x33);
        assert_eq!(seed[1], 0); // chunk_id 0
        assert_eq!(seed[2], 24);
    }

    #[test]
    fn test_get_raw_seed_for_round_finds_in_next_slot() {
        let tier_id = 225;
        clear_slots(tier_id);
        put_current(tier_id, build_test_chunk(tier_id, 0, 0x55));
        put_next(tier_id, build_test_chunk(tier_id, 1, 0x66));

        // Round 75 → chunk 1, offset 24 → must be served from NEXT
        let seed = get_raw_seed_for_round(tier_id, 75).expect("lookup");
        assert_eq!(seed[0], 0x66);
        assert_eq!(seed[1], 1); // chunk_id 1
        assert_eq!(seed[2], 24);
    }

    #[test]
    fn test_get_raw_seed_for_round_mismatch_when_neither_matches() {
        let tier_id = 226;
        clear_slots(tier_id);
        put_current(tier_id, build_test_chunk(tier_id, 0, 0x77));
        put_next(tier_id, build_test_chunk(tier_id, 1, 0x88));

        // Round 200 → chunk 3, neither slot has it
        let err = get_raw_seed_for_round(tier_id, 200)
            .expect_err("should error when no slot matches");
        assert!(err.contains("seed chunk mismatch"), "unexpected error: {}", err);
        assert!(err.contains("current=Some(0)"), "should report current chunk_id: {}", err);
        assert!(err.contains("next=Some(1)"), "should report next chunk_id: {}", err);
    }

    #[test]
    fn test_get_raw_seed_for_round_no_chunk_when_both_empty() {
        let tier_id = 227;
        clear_slots(tier_id);

        let err = get_raw_seed_for_round(tier_id, 1)
            .expect_err("should error when both slots empty");
        assert!(err.contains("no seed chunk"), "unexpected error: {}", err);
    }

    #[test]
    fn test_get_raw_seed_for_round_zero_rejected() {
        let tier_id = 228;
        clear_slots(tier_id);
        let err = get_raw_seed_for_round(tier_id, 0)
            .expect_err("round_id 0 should be rejected");
        assert!(err.contains("must be >= 1"), "unexpected error: {}", err);
    }
}
