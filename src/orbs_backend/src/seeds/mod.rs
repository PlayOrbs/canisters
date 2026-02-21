//! Seed management module.
//!
//! Contains Merkle tree generation, chunk management, and seed revelation logic.

pub mod chunks;
pub mod reveal;

pub use chunks::{
    SeedChunk, SeedProof, CHUNK_SIZE,
    build_merkle_tree, compute_leaf_hash, compute_round_id,
    format_chunk_root_message, generate_merkle_proof, generate_seed_chunk,
    get_merkle_root, get_seed_proof, get_seed_proof_by_offset,
};

pub use reveal::{
    RevealValidation,
    chunk_exists, chunk_needs_regen, clear_revealed_seed,
    ensure_chunk, generate_chunk, get_chunk_offset, get_last_settled_round,
    get_seed_proof as get_reveal_seed_proof, get_raw_seed_for_round, increment_offset,
    refresh_chunks_for_tier, regenerate_chunk, reset_last_settled_round,
    reveal_seed, set_chunk_offset, set_last_settled_round, validate_reveal_round,
};
