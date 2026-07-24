//! Cross-platform P5136 profile model and persistence.

pub mod catalog;
pub mod equipment;
pub mod inventory;
pub mod model;
pub mod store;

pub use catalog::{
    CatalogInventory, CatalogInventoryError, CatalogInventoryItem, CatalogInventoryStats,
    is_grant_category, is_grant_item,
};
pub use equipment::{EquipmentExceptions, EquipmentStateError};
pub use inventory::{
    InventoryBuildError, build_inventory_snapshot, build_inventory_snapshot_with_equipment,
    rider_item_snapshot,
};
pub use model::{
    ExtraFields, GameOptions, GrantedKart, MyRoom, Profile, Rider, RiderItems, ServerSettings,
};
pub use store::{LoadedProfile, ProfileStore, ProfileStoreError, SavedProfile};
