//! Merkle-tree-based seed chunk management.
//!
//! Seeds are generated in chunks (CHUNK_SIZE seeds per chunk).
//! Each chunk has a Merkle tree built over the seeds, and the root is signed.
//! When a round needs a seed, we provide the seed + Merkle proof + chunk signature.

use candid::CandidType;
use ic_cdk::management_canister::{SignWithEcdsaArgs, raw_rand, sign_with_ecdsa};
use serde::{Deserialize, Serialize};
use sha2::Digest;

use crate::crypto::{create_derivation_path, get_ecdsa_key_id};

/// Number of seeds per chunk (must match Solana program's CHUNK_SIZE)
pub const CHUNK_SIZE: u64 = 50;

/// Validate that signature is 64 bytes (r||s compact format).
/// ICP's sign_with_ecdsa returns raw 64-byte signatures, not DER-encoded.
/// This function validates the length and converts to a fixed-size array.
pub fn validate_compact_signature(sig: &[u8]) -> Result<[u8; 64], String> {
    if sig.len() != 64 {
        return Err(format!(
            "Invalid signature length: expected 64 bytes, got {}",
            sig.len()
        ));
    }
    let mut compact = [0u8; 64];
    compact.copy_from_slice(sig);
    Ok(compact)
}

/// A generated seed chunk with Merkle tree data
#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct SeedChunk {
    /// Tier ID this chunk belongs to
    pub tier_id: u8,
    /// Chunk ID (sequential within tier)
    pub chunk_id: u64,
    /// The seeds in this chunk
    pub seeds: Vec<[u8; 32]>,
    /// Merkle tree layers (layer 0 = leaves, last layer = root)
    pub merkle_layers: Vec<Vec<[u8; 32]>>,
    /// ECDSA signature over the chunk root message
    pub root_signature: Vec<u8>,
}

/// Proof data for a single seed within a chunk
#[derive(CandidType, Serialize, Deserialize, Clone, Debug)]
pub struct SeedProof {
    /// The seed value
    pub seed: [u8; 32],
    /// Chunk ID
    pub chunk_id: u64,
    /// Merkle root of the chunk
    pub merkle_root: [u8; 32],
    /// ECDSA signature over the chunk root message
    pub root_signature: Vec<u8>,
    /// Sibling hashes for Merkle proof (from leaf to root)
    pub proof_siblings: Vec<[u8; 32]>,
    /// Position flags: false = current is left child, true = current is right child
    pub proof_positions: Vec<bool>,
}

/// Compute the leaf hash for a seed.
/// leaf = sha256("orbs-leaf" || tier_id || round_id || seed)
///
/// The round_id cryptographically binds the seed to a specific round,
/// ensuring the Merkle proof can only verify this seed for that exact round.
/// Note: season_id is NOT included - round IDs are globally unique per tier.
pub fn compute_leaf_hash(tier_id: u8, round_id: u64, seed: &[u8; 32]) -> [u8; 32] {
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"orbs-leaf");
    hasher.update(tier_id.to_le_bytes());
    hasher.update(round_id.to_le_bytes());
    hasher.update(seed);
    let result = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Compute round_id from chunk_id and offset within chunk.
/// round_id = chunk_id * CHUNK_SIZE + offset + 1 (rounds are 1-indexed)
pub fn compute_round_id(chunk_id: u64, offset: u64) -> u64 {
    chunk_id * CHUNK_SIZE + offset + 1
}

/// Build a Merkle tree from leaves.
/// Returns all layers (layer 0 = leaves, last layer = [root]).
/// Pads to next power of two with zero hashes.
pub fn build_merkle_tree(leaves: &[[u8; 32]]) -> Vec<Vec<[u8; 32]>> {
    if leaves.is_empty() {
        return vec![vec![[0u8; 32]]];
    }

    // Pad to next power of two
    let n = leaves.len().next_power_of_two();
    let mut layer: Vec<[u8; 32]> = leaves.to_vec();
    while layer.len() < n {
        layer.push([0u8; 32]);
    }

    let mut layers = vec![layer.clone()];

    // Build tree bottom-up
    while layer.len() > 1 {
        let mut next_layer = Vec::with_capacity(layer.len() / 2);
        for i in (0..layer.len()).step_by(2) {
            let left = layer[i];
            let right = layer.get(i + 1).copied().unwrap_or([0u8; 32]);

            let mut hasher = sha2::Sha256::new();
            hasher.update(left);
            hasher.update(right);
            let result = hasher.finalize();
            let mut parent = [0u8; 32];
            parent.copy_from_slice(&result);
            next_layer.push(parent);
        }
        layers.push(next_layer.clone());
        layer = next_layer;
    }

    layers
}

/// Get Merkle root from layers
pub fn get_merkle_root(layers: &[Vec<[u8; 32]>]) -> [u8; 32] {
    layers
        .last()
        .and_then(|l| l.first())
        .copied()
        .unwrap_or([0u8; 32])
}

/// Generate Merkle proof for a leaf at given index
pub fn generate_merkle_proof(
    layers: &[Vec<[u8; 32]>],
    leaf_index: usize,
) -> (Vec<[u8; 32]>, Vec<bool>) {
    let mut siblings = Vec::new();
    let mut positions = Vec::new();
    let mut idx = leaf_index;

    // Walk up the tree (skip the root layer)
    for layer in layers.iter().take(layers.len().saturating_sub(1)) {
        let is_right = idx % 2 == 1;
        let sibling_idx = if is_right { idx - 1 } else { idx + 1 };

        if let Some(&sibling) = layer.get(sibling_idx) {
            siblings.push(sibling);
            positions.push(is_right);
        }

        idx /= 2;
    }

    (siblings, positions)
}

/// Format the chunk root message for signing.
/// Message: "OrbsChunkRoot\nTier:<tier_id>\nChunk:<chunk_id>\nRoot:<root_hex>"
/// Note: season_id removed - round IDs are globally unique per tier.
pub fn format_chunk_root_message(
    tier_id: u8,
    chunk_id: u64,
    merkle_root: &[u8; 32],
) -> String {
    let root_hex: String = merkle_root.iter().map(|b| format!("{:02x}", b)).collect();
    format!(
        "OrbsChunkRoot\nTier:{}\nChunk:{}\nRoot:{}",
        tier_id, chunk_id, root_hex
    )
}

/// Generate a new seed chunk for a tier.
/// Returns the chunk with seeds, Merkle tree, and signature.
/// Note: season_id removed - round IDs are globally unique per tier.
pub async fn generate_seed_chunk(
    tier_id: u8,
    chunk_id: u64,
) -> Result<SeedChunk, String> {
    // Get master seed from ICP randomness
    let raw = raw_rand().await.map_err(|res| res.to_string())?;
    if raw.len() < 32 {
        return Err("raw_rand returned < 32 bytes".to_string());
    }

    let mut master_seed = [0u8; 32];
    master_seed.copy_from_slice(&raw[..32]);

    // Derive CHUNK_SIZE seeds using SHA256(master || index)
    let mut seeds: Vec<[u8; 32]> = Vec::with_capacity(CHUNK_SIZE as usize);
    for i in 0..CHUNK_SIZE {
        let mut hasher = sha2::Sha256::new();
        hasher.update(&master_seed);
        hasher.update(&i.to_le_bytes());
        let result = hasher.finalize();
        let mut derived = [0u8; 32];
        derived.copy_from_slice(&result[..32]);
        seeds.push(derived);
    }

    // Master seed is now out of scope - never stored

    // Compute leaf hashes with round_id = chunk_id * CHUNK_SIZE + offset
    let leaves: Vec<[u8; 32]> = seeds
        .iter()
        .enumerate()
        .map(|(i, seed)| {
            let round_id = compute_round_id(chunk_id, i as u64);
            compute_leaf_hash(tier_id, round_id, seed)
        })
        .collect();

    // Build Merkle tree
    let merkle_layers = build_merkle_tree(&leaves);
    let merkle_root = get_merkle_root(&merkle_layers);

    // Sign the chunk root
    let message = format_chunk_root_message(tier_id, chunk_id, &merkle_root);
    let mut hasher = sha2::Sha256::new();
    hasher.update(message.as_bytes());
    let message_hash: [u8; 32] = hasher.finalize().into();

    let sign_args = SignWithEcdsaArgs {
        message_hash: message_hash.to_vec(),
        derivation_path: create_derivation_path(0),
        key_id: get_ecdsa_key_id(),
    };

    let sig_result = sign_with_ecdsa(&sign_args)
        .await
        .map_err(|e| format!("ECDSA signing failed: {:?}", e))?;

    // Validate signature is 64 bytes (ICP returns raw r||s format)
    let compact_sig = validate_compact_signature(&sig_result.signature)?;

    Ok(SeedChunk {
        tier_id,
        chunk_id,
        seeds,
        merkle_layers,
        root_signature: compact_sig.to_vec(),
    })
}

/// Get seed proof for a specific round from a chunk.
pub fn get_seed_proof(chunk: &SeedChunk, round_id: u64) -> Result<SeedProof, String> {
    // Rounds are 1-indexed, so we need to subtract 1 for chunk_id calculation
    let expected_chunk_id = if round_id > 0 {
        (round_id - 1) / CHUNK_SIZE
    } else {
        0
    };
    
    if chunk.chunk_id != expected_chunk_id {
        return Err(format!(
            "Chunk ID mismatch: expected {} for round {}, got {}",
            expected_chunk_id, round_id, chunk.chunk_id
        ));
    }

    // Offset calculation must also account for 1-indexed rounds
    let offset = if round_id > 0 {
        ((round_id - 1) % CHUNK_SIZE) as usize
    } else {
        0
    };
    if offset >= chunk.seeds.len() {
        return Err(format!("Offset {} out of bounds for chunk", offset));
    }

    let seed = chunk.seeds[offset];
    let merkle_root = get_merkle_root(&chunk.merkle_layers);
    let (proof_siblings, proof_positions) = generate_merkle_proof(&chunk.merkle_layers, offset);

    Ok(SeedProof {
        seed,
        chunk_id: chunk.chunk_id,
        merkle_root,
        root_signature: chunk.root_signature.clone(),
        proof_siblings,
        proof_positions,
    })
}

/// Get seed proof by offset within chunk (for single-chunk model).
/// offset_in_chunk: which seed (0-49) to extract
pub fn get_seed_proof_by_offset(
    chunk: &SeedChunk,
    offset_in_chunk: u64,
) -> Result<SeedProof, String> {
    let offset = offset_in_chunk as usize;
    if offset >= chunk.seeds.len() {
        return Err(format!(
            "Offset {} out of bounds for chunk (max {})",
            offset,
            chunk.seeds.len()
        ));
    }

    let seed = chunk.seeds[offset];
    let merkle_root = get_merkle_root(&chunk.merkle_layers);
    let (proof_siblings, proof_positions) = generate_merkle_proof(&chunk.merkle_layers, offset);

    Ok(SeedProof {
        seed,
        chunk_id: chunk.chunk_id,
        merkle_root,
        root_signature: chunk.root_signature.clone(),
        proof_siblings,
        proof_positions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_merkle_tree_single_leaf() {
        let leaf = [1u8; 32];
        let layers = build_merkle_tree(&[leaf]);

        // Should have 1 layer (just the padded leaves becoming root)
        assert!(!layers.is_empty());
        // Root should exist
        let root = get_merkle_root(&layers);
        assert_ne!(root, [0u8; 32]);
    }

    #[test]
    fn test_build_merkle_tree_multiple_leaves() {
        let leaves: Vec<[u8; 32]> = (0..4)
            .map(|i| {
                let mut l = [0u8; 32];
                l[0] = i;
                l
            })
            .collect();

        let layers = build_merkle_tree(&leaves);

        // 4 leaves -> 2 layers above leaves -> 3 total layers
        assert_eq!(layers.len(), 3);
        assert_eq!(layers[0].len(), 4); // leaves
        assert_eq!(layers[1].len(), 2); // intermediate
        assert_eq!(layers[2].len(), 1); // root
    }

    #[test]
    fn test_merkle_proof_verification() {
        let tier_id: u8 = 0;
        let chunk_id: u64 = 0;

        // Create seeds
        let seeds: Vec<[u8; 32]> = (0..4)
            .map(|i| {
                let mut s = [0u8; 32];
                s[0] = i;
                s
            })
            .collect();

        // Compute leaves with round_id = chunk_id * CHUNK_SIZE + offset
        let leaves: Vec<[u8; 32]> = seeds
            .iter()
            .enumerate()
            .map(|(i, seed)| {
                let round_id = compute_round_id(chunk_id, i as u64);
                compute_leaf_hash(tier_id, round_id, seed)
            })
            .collect();

        let layers = build_merkle_tree(&leaves);
        let root = get_merkle_root(&layers);

        // Verify each leaf's proof
        for (i, leaf) in leaves.iter().enumerate() {
            let (siblings, positions) = generate_merkle_proof(&layers, i);

            // Manually verify the proof
            let mut current = *leaf;
            for (sibling, &is_right) in siblings.iter().zip(positions.iter()) {
                let mut hasher = sha2::Sha256::new();
                if is_right {
                    hasher.update(sibling);
                    hasher.update(current);
                } else {
                    hasher.update(current);
                    hasher.update(sibling);
                }
                let result = hasher.finalize();
                current.copy_from_slice(&result);
            }

            assert_eq!(current, root, "Proof verification failed for leaf {}", i);
        }
    }

    #[test]
    fn test_compute_leaf_hash_deterministic() {
        let seed = [42u8; 32];
        let round_id = 5;
        let hash1 = compute_leaf_hash(0, round_id, &seed);
        let hash2 = compute_leaf_hash(0, round_id, &seed);
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_compute_round_id() {
        // Rounds are 1-indexed: round_id = chunk_id * CHUNK_SIZE + offset + 1
        // chunk 0, offset 0 -> round 1
        assert_eq!(compute_round_id(0, 0), 1);
        // chunk 0, offset 49 -> round 50
        assert_eq!(compute_round_id(0, 49), 50);
        // chunk 1, offset 0 -> round 51
        assert_eq!(compute_round_id(1, 0), 51);
        // chunk 2, offset 5 -> round 106
        assert_eq!(compute_round_id(2, 5), 106);
    }

    #[test]
    fn test_leaf_hash_different_for_different_rounds() {
        let seed = [42u8; 32];
        let hash_round_0 = compute_leaf_hash(0, 0, &seed);
        let hash_round_1 = compute_leaf_hash(0, 1, &seed);
        assert_ne!(
            hash_round_0, hash_round_1,
            "Same seed should produce different hashes for different rounds"
        );
    }

    #[test]
    fn test_format_chunk_root_message() {
        let root = [0xab; 32];
        let msg = format_chunk_root_message(2, 3, &root);
        assert!(msg.starts_with("OrbsChunkRoot\n"));
        assert!(msg.contains("Tier:2"));
        assert!(msg.contains("Chunk:3"));
        assert!(msg.contains("Root:abab")); // starts with repeated ab
    }
}
