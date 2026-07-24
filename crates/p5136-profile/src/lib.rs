//! Cross-platform P5136 profile model and persistence.

pub mod catalog;
pub mod equipment;
pub mod inventory;
pub mod model;
pub mod myroom_items;
pub mod progression;
pub mod store;

pub use catalog::{
    CatalogInventory, CatalogInventoryError, CatalogInventoryItem, CatalogInventoryStats,
    CatalogKartSpecStats, P5136KartSpecSnapshot, is_grant_category, is_grant_item,
};
pub use equipment::{EquipmentExceptions, EquipmentStateError};
pub use inventory::{
    InventoryBuildError, apply_rider_item_selection, build_inventory_snapshot,
    build_inventory_snapshot_with_equipment, rider_item_snapshot,
};
pub use model::{
    ExtraFields, GameOptions, GrantedKart, MyRoom, Profile, Rider, RiderItems, ServerSettings,
};
pub use myroom_items::{
    MAX_MYROOM_ITEM_RECORDS, MAX_MYROOM_ITEM_STATE_BYTES, MyRoomItemStateError,
    MyRoomOwnerInventory,
};
pub use progression::{
    AppliedTimeReward, DEFAULT_RP, MAX_TIME_REWARD_LUCCI_ROLL, MAX_TIME_REWARD_RP_ROLL,
    RewardRollError, TIME_REWARD_BASELINE_RANK, TimeReward, apply_time_reward, finish_reward,
    time_reward_from_rolls,
};
pub use store::{
    LoadedProfile, ProfileMutation, ProfileStore, ProfileStoreError, ProfileTransaction,
    SavedProfile,
};
