//! Cross-platform P5136 profile model and persistence.

pub mod catalog;
pub mod emblem_catalog;
pub mod equipment;
pub mod favorite_items;
pub mod inventory;
pub mod model;
pub mod myroom_items;
pub mod progression;
pub mod store;

pub use catalog::{
    CatalogInventory, CatalogInventoryError, CatalogInventoryItem, CatalogInventoryStats,
    CatalogKartSpecStats, MAX_CATALOG_EMBLEMS, P5136KartSpecSnapshot, is_grant_category,
    is_grant_item,
};
pub use emblem_catalog::{EmblemCatalog, EmblemCatalogError, MAX_EMBLEM_XML_BYTES};
pub use equipment::{EquipmentExceptions, EquipmentStateError};
pub use favorite_items::{
    DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, FavoriteItemStateError, FavoriteItems,
    apply_favorite_item_changes, favorite_item_snapshot,
};
pub use inventory::{
    InventoryBuildError, apply_rider_item_selection, build_inventory_snapshot,
    build_inventory_snapshot_with_equipment, rider_item_snapshot,
};
pub use model::{
    ExtraFields, GameOptions, GrantedKart, MyRoom, Profile, Rider, RiderItems, ServerSettings,
};
pub use myroom_items::{
    MAX_MYROOM_ITEM_RECORDS, MAX_MYROOM_ITEM_STATE_BYTES, MyRoomItemFileType, MyRoomItemStateError,
    MyRoomOwnerInventory,
};
pub use progression::{
    AppliedTimeReward, DEFAULT_RP, GlobalRaceEpoch, InvalidStoredReceiptError,
    MAX_TIME_REWARD_LUCCI_ROLL, MAX_TIME_REWARD_RP_ROLL, PersistedRaceRewardReceipt,
    RaceRewardBindingError, RaceRewardKey, RaceRewardKeyError, RaceRewardOrderError,
    RaceRewardPersistenceError, RaceRewardRecipient, RaceRewardRecipientError, RaceRunId,
    RewardAmountError, RewardRollError, TIME_REWARD_BASELINE_RANK, TimeReward,
    apply_race_reward_once, apply_time_reward, finish_reward, time_reward_from_rolls,
};
pub use store::{
    FavoriteItemImportError, FavoriteItemStateOrigin, LoadedProfile, ProfileMutation, ProfileStore,
    ProfileStoreError, ProfileStoreId, ProfileTransaction, ProfileTransactionContext,
    RaceRunGeneration, RaceRunLease, SavedProfile,
};
