use ic_cdk::management_canister::{ecdsa_public_key, EcdsaKeyId, EcdsaPublicKeyArgs};

use k256::PublicKey;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AddressError {
    #[error("Failed to get public key: {0}")]
    PublicKeyError(String),

    #[error("Invalid public key: {0}")]
    InvalidPublicKey(String),

    #[error("Address error: {0}")]
    AddressError(String),

    #[error("Invalid k256 public key: {0}")]
    InvalidK256Key(String),
}

// Creates derivation path from index using little-endian encoding
pub fn create_derivation_path(index: u32) -> Vec<Vec<u8>> {
    let mut path = vec![0u8; 4];
    path[0] = (index & 0xFF) as u8;
    path[1] = ((index >> 8) & 0xFF) as u8;
    path[2] = ((index >> 16) & 0xFF) as u8;
    path[3] = ((index >> 24) & 0xFF) as u8;
    vec![path]
}

// Gets ECDSA key ID from state configuration
pub fn get_ecdsa_key_id() -> EcdsaKeyId {
    let key_name = "key_1";
    EcdsaKeyId {
        curve: ic_cdk::management_canister::EcdsaCurve::Secp256k1,
        name: key_name.to_string(),
    }
}

/// Gets public key via threshold ECDSA
pub async fn get_public_key(index: Option<u32>) -> Result<PublicKey, AddressError> {
    let index = index.unwrap_or(0);
    let key_id = get_ecdsa_key_id();
    let derivation_path = create_derivation_path(index);

    let arg = EcdsaPublicKeyArgs {
        canister_id: None,
        derivation_path,
        key_id,
    };

    let response = ecdsa_public_key(&arg)
        .await
        .map_err(|e| AddressError::PublicKeyError(format!("{:?}", e)))?;

    PublicKey::from_sec1_bytes(&response.public_key)
        .map_err(|e| AddressError::InvalidPublicKey(format!("{:?}", e)))
}
