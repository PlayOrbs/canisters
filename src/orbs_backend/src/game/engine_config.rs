//! Game engine configuration management.
//!
//! Stores immutable versioned engine configs for deterministic replay verification.
//! Once a config is added, it cannot be modified - new configs get new version numbers.

use crate::admin::ensure_admin;
use crate::crypto::Cbor;
use crate::state::{ENGINE_CONFIGS, EngineConfig, version_to_key};

/// Add a new engine config (admin only).
/// Configs are immutable - once added, they cannot be modified.
/// Returns error if version already exists.
pub fn add_engine_config(version: String, config_json: String) -> Result<(), String> {
    ensure_admin();

    // Validate version string length
    if version.len() > 16 {
        return Err(format!("Version string too long: {} (max 16 chars)", version.len()));
    }
    if version.is_empty() {
        return Err("Version string cannot be empty".to_string());
    }

    let key = version_to_key(&version);

    ENGINE_CONFIGS.with(|m| {
        let mut map = m.borrow_mut();

        // Check if version already exists (immutable - cannot overwrite)
        if map.contains_key(&key) {
            return Err(format!("Engine config version {} already exists", version));
        }

        let config = EngineConfig {
            version,
            config_json,
            created_at: ic_cdk::api::time() / 1_000_000_000, // Convert nanoseconds to seconds
        };

        map.insert(key, Cbor(config));
        Ok(())
    })
}

/// Update an existing engine config (admin only).
/// This overwrites the config JSON for an existing version.
pub fn update_engine_config(version: String, config_json: String) -> Result<(), String> {
    ensure_admin();

    let key = version_to_key(&version);

    ENGINE_CONFIGS.with(|m| {
        let mut map = m.borrow_mut();

        // Check if version exists
        if !map.contains_key(&key) {
            return Err(format!("Engine config version {} does not exist", version));
        }

        let config = EngineConfig {
            version,
            config_json,
            created_at: ic_cdk::api::time() / 1_000_000_000,
        };

        map.insert(key, Cbor(config));
        Ok(())
    })
}

/// Get an engine config by version string (public query).
pub fn get_engine_config(version: String) -> Option<EngineConfig> {
    let key = version_to_key(&version);
    ENGINE_CONFIGS.with(|m| m.borrow().get(&key).map(|c| c.0.clone()))
}

/// Get the latest engine config version string (public query).
pub fn get_latest_engine_config_version() -> Option<String> {
    ENGINE_CONFIGS.with(|m| m.borrow().last_key_value().map(|(_, v)| v.0.version.clone()))
}

/// Get the latest engine config (public query).
pub fn get_latest_engine_config() -> Option<EngineConfig> {
    ENGINE_CONFIGS.with(|m| m.borrow().last_key_value().map(|(_, v)| v.0.clone()))
}

/// List all engine config versions (public query).
/// Returns list of EngineConfig records.
pub fn list_engine_config_versions() -> Vec<EngineConfig> {
    ENGINE_CONFIGS.with(|m| {
        let map = m.borrow();
        let mut result = Vec::new();
        for key in map.keys() {
            if let Some(config) = map.get(&key) {
                result.push(config.0.clone());
            }
        }
        result
    })
}
