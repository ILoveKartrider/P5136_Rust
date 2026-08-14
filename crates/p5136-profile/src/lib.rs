//! Cross-platform P5136 profile model and persistence.

pub mod catalog;
pub mod emblem_catalog;
pub mod equipment;
pub mod favorite_items;
pub mod inventory;
pub mod inventory_editor;
pub mod model;
pub mod myroom_items;
pub mod progression;
pub mod store;

pub use catalog::{
    CatalogInventory, CatalogInventoryError, CatalogInventoryItem, CatalogInventoryStats,
    CatalogItemTransformRule, CatalogKartSpecStats, CatalogXunProfile, MAX_CATALOG_BYTES,
    MAX_CATALOG_EMBLEMS, MAX_CATALOG_ITEM_TRANSFORMS, P5136KartSpecSnapshot, is_grant_category,
    is_grant_item, is_stock_item_safe_for_implicit_grant,
};
pub use emblem_catalog::{EmblemCatalog, EmblemCatalogError, MAX_EMBLEM_XML_BYTES};
pub use equipment::{
    EquipmentExceptions, EquipmentLoadWarning, EquipmentMutationOutcome, EquipmentProfileError,
    EquipmentSidecar, EquipmentStateError, FloaterResetOutcome, LenientEquipmentLoad,
};
pub use favorite_items::{
    DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, FavoriteItemStateError, FavoriteItems, LockedItems,
    apply_favorite_item_changes, apply_item_collection_changes, favorite_item_snapshot,
    item_collection_snapshot,
};
pub use inventory::{
    InventoryBuildError, apply_rider_item_selection, build_inventory_snapshot,
    build_inventory_snapshot_with_equipment, generated_x_part_is_granted, rider_item_snapshot,
};
pub use inventory_editor::{
    AddKartOutcome, AdditionalKart, AppliedKartGrantEnhancements, FLOATER_333_CODES,
    KartCatalogSearchResult, KartGrantOptions, KartInventoryEditError,
    MAX_ADDITIONAL_KARTS_PER_PROFILE, MAX_KART_SEARCH_QUERY_CHARS, MAX_KART_SEARCH_RESULTS,
    add_kart, add_kart_during_race_run, add_kart_during_race_run_with_options,
    add_kart_with_options, additional_karts, search_karts,
};
pub use model::{
    ExtraFields, GameOptions, GrantedKart, MyRoom, Profile, Rider, RiderItems, RiderSchoolProgress,
    ServerSettings,
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
    FavoriteItemImportError, FavoriteItemStateOrigin, LegacyItemCollectionImportError,
    LegacyItemCollectionStateOrigin, LoadedProfile, LockedItemImportError, LockedItemStateOrigin,
    ProfileMutation, ProfileStore, ProfileStoreError, ProfileStoreId, ProfileTransaction,
    ProfileTransactionContext, RaceRunGeneration, RaceRunLease, SavedProfile,
};
