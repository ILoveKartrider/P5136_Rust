//! Cross-platform P5136 profile model and persistence.

pub mod model;
pub mod store;

pub use model::{
    ExtraFields, GameOptions, GrantedKart, MyRoom, Profile, Rider, RiderItems, ServerSettings,
};
pub use store::{LoadedProfile, ProfileStore, ProfileStoreError, SavedProfile};
