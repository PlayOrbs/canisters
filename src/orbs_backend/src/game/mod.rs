//! Game-related data module.
//!
//! Contains player management, round snapshots, engine configuration, and join data.

pub mod engine_config;
pub mod join_data;
pub mod player_config;
pub mod players;
pub mod rounds;

pub use engine_config::{
    add_engine_config, update_engine_config, get_engine_config, get_latest_engine_config,
    get_latest_engine_config_version, list_engine_config_versions,
};

pub use join_data::{
    get_player_join_data, get_player_round_seed, get_round_join_data,
    store_player_join_data, PlayerJoinDataInput, PlayerJoinDataOutput,
};

pub use player_config::{
    get_player_config, list_player_configs, list_player_configs_if_revealed,
    set_player_config, PlayerConfigInput, PlayerConfigOutput,
};

pub use players::{add_players, get_players, player_exists};

pub use rounds::{
    get_player_round_history, get_round_snapshot, get_round_snapshots_by_tier,
    store_round_snapshot, update_round_config_version,
};
