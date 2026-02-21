// Helper: convert Vec<u8> to [u8; 32]
pub fn vec_to_pk32(pk: Vec<u8>) -> Result<[u8; 32], String> {
    if pk.len() != 32 {
        return Err("invalid pubkey length, expected 32 bytes".to_string());
    }
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&pk[..]);
    Ok(arr)
}
