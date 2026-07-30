//! Exact, atomic persistence for P5136 favorite-item batches.
//!
//! A stock update is one-way, so there is no wire acknowledgement that can be
//! delayed until storage succeeds. The accepted profile job nevertheless
//! applies the complete batch in one optimistic transaction and confirms the
//! exact immutable revision before session state may be refreshed.
//!
//! An absent Rust marker is resolved through the lease-bound, no-follow C#
//! `Favorite.json` importer. Its parsed state, the migration marker, and an
//! incoming stock update are published together in one profile transaction.

use p5136_core::item_state_protocol::FavoriteItemChange;
use p5136_profile::{
    DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, FavoriteItemImportError, FavoriteItemStateError,
    FavoriteItems, ProfileStore, ProfileStoreError, RaceRunLease, SavedProfile,
    apply_favorite_item_changes,
};
use thiserror::Error;

use crate::profile_durability::{ExactDurabilityError, ExactProfileTransaction};

pub(crate) const FAVORITE_ITEM_UPDATE_OPERATION: &str = "persist favorite-item update";

#[derive(Debug, Error)]
pub enum FavoriteItemPersistError {
    #[error(transparent)]
    Import(#[from] FavoriteItemImportError),

    #[error(transparent)]
    State(#[from] FavoriteItemStateError),

    #[error("favorite-item profile persistence failed")]
    Store {
        #[source]
        source: Box<ProfileStoreError>,
    },

    #[error("favorite-item transaction completed without an immutable durable revision")]
    MissingDurableRevision,

    #[error("favorite-item transaction completed without canonical favorite-item state")]
    MissingCanonicalState,

    #[error(
        "favorite-item durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "favorite-item profile revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },
}

impl From<ProfileStoreError> for FavoriteItemPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store {
            source: Box::new(source),
        }
    }
}

impl ExactDurabilityError for FavoriteItemPersistError {
    fn durability_unconfirmed(
        revision: u64,
        initial: ProfileStoreError,
        confirmation: ProfileStoreError,
    ) -> Self {
        Self::DurabilityUnconfirmed {
            revision,
            initial: Box::new(initial),
            confirmation: Box::new(confirmation),
        }
    }

    fn durability_receipt_changed(expected: SavedProfile, actual: Option<SavedProfile>) -> Self {
        Self::DurabilityReceiptChanged {
            expected: Box::new(expected),
            actual: actual.map(Box::new),
        }
    }
}

#[derive(Debug)]
pub(crate) struct DurableFavoriteItems {
    saved: SavedProfile,
    items: FavoriteItems,
}

impl DurableFavoriteItems {
    pub(crate) const fn revision(&self) -> u64 {
        self.saved.revision
    }

    pub(crate) const fn items(&self) -> &FavoriteItems {
        &self.items
    }
}

pub(crate) fn persist_favorite_item_changes(
    store: &ProfileStore,
    lease: &RaceRunLease,
    nickname: &str,
    changes: &[FavoriteItemChange],
    maximum_records: usize,
) -> Result<DurableFavoriteItems, FavoriteItemPersistError> {
    let transaction = store.transaction_with_legacy_favorite_items(
        lease,
        nickname,
        |current_items, origin| {
            let next = apply_favorite_item_changes(Some(current_items), changes, maximum_records)?;
            let maximum = maximum_records.min(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS);
            if origin.is_legacy_sidecar() && next.len() > maximum {
                return Err(FavoriteItemStateError::TooManyItems {
                    count: next.len(),
                    maximum,
                });
            }
            Ok(((), next))
        },
    )?;
    let (state, profile, durability) = ExactProfileTransaction::from(transaction).into_parts();
    state?;
    let saved = durability
        .confirm_exact::<FavoriteItemPersistError>(store, nickname)?
        .ok_or(FavoriteItemPersistError::MissingDurableRevision)?;
    let items = profile
        .favorite_items
        .ok_or(FavoriteItemPersistError::MissingCanonicalState)?;
    Ok(DurableFavoriteItems { saved, items })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use p5136_core::item_state_protocol::{
        FavoriteItemChange, FavoriteItemKey, FavoriteItemOperation,
    };
    use p5136_profile::{
        FavoriteItemImportError, FavoriteItemStateError, FavoriteItemStateOrigin, FavoriteItems,
        ProfileMutation, ProfileStore,
    };
    use serde_json::json;

    use super::{FavoriteItemPersistError, persist_favorite_item_changes};

    fn add(serial: u16) -> FavoriteItemChange {
        FavoriteItemChange::new(
            FavoriteItemKey::new(3, 1_450, serial),
            FavoriteItemOperation::Add,
        )
    }

    fn canonicalize_empty(store: &ProfileStore, nickname: &str) {
        store.load_or_create(nickname).unwrap();
        store
            .update(nickname, |profile| {
                profile.favorite_items = Some(FavoriteItems::default());
            })
            .unwrap();
    }

    #[test]
    fn complete_batches_commit_atomically_and_retries_reuse_the_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        canonicalize_empty(&store, "FavoriteRider");
        let changes = [add(1), add(2)];

        let first = persist_favorite_item_changes(&store, &lease, "FavoriteRider", &changes, 1_000)
            .unwrap();
        let first_profile = store.reload("FavoriteRider").unwrap().profile;
        assert_eq!(
            first_profile
                .favorite_items
                .as_ref()
                .expect("successful update canonicalizes the field")
                .as_slice()
                .len(),
            2
        );

        let retry = persist_favorite_item_changes(&store, &lease, "favoriterider", &changes, 1_000)
            .unwrap();
        let retry_profile = store.reload("FavoriteRider").unwrap().profile;
        assert_eq!(retry_profile.favorite_items, first_profile.favorite_items);
        assert_eq!(retry.revision(), first.revision());
    }

    #[test]
    fn missing_legacy_sidecar_becomes_a_canonical_empty_marker() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let initial = store.load_or_create("EmptyFavoriteRider").unwrap();
        assert!(initial.profile.favorite_items.is_none());

        let receipt =
            persist_favorite_item_changes(&store, &lease, "EmptyFavoriteRider", &[], 1_000)
                .unwrap();
        let after = store.reload("EmptyFavoriteRider").unwrap();
        assert_eq!(after.revision, Some(receipt.revision()));
        assert_ne!(after.revision, initial.revision);
        assert_eq!(after.profile.favorite_items, Some(FavoriteItems::default()));
    }

    #[test]
    fn sidecar_import_and_first_update_share_one_immutable_revision() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("SidecarFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        let sidecar = br#"[{"ItemCatID":3,"ItemID":1450,"ItemSN":1}]"#;
        fs::write(directory.join("Favorite.json"), sidecar).unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();

        let receipt =
            persist_favorite_item_changes(&store, &lease, "SidecarFavoriteRider", &[add(2)], 1_000)
                .unwrap();

        assert_eq!(receipt.revision(), 1);
        assert_eq!(
            receipt.items().as_slice(),
            &[
                FavoriteItemKey::new(3, 1_450, 1),
                FavoriteItemKey::new(3, 1_450, 2),
            ]
        );
        assert_eq!(fs::read(directory.join("Favorite.json")).unwrap(), sidecar);
        assert_eq!(
            store
                .reload("SidecarFavoriteRider")
                .unwrap()
                .profile
                .favorite_items,
            Some(receipt.items().clone())
        );

        fs::write(
            directory.join("Favorite.json"),
            b"malformed after migration",
        )
        .unwrap();
        let second =
            persist_favorite_item_changes(&store, &lease, "SidecarFavoriteRider", &[add(3)], 1_000)
                .unwrap();
        assert_eq!(second.revision(), 2);
        assert_eq!(
            second.items().as_slice(),
            &[
                FavoriteItemKey::new(3, 1_450, 1),
                FavoriteItemKey::new(3, 1_450, 2),
                FavoriteItemKey::new(3, 1_450, 3),
            ]
        );
    }

    #[test]
    fn malformed_sidecar_fails_closed_without_sealing_the_marker() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("BrokenSidecarFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        fs::write(directory.join("Favorite.json"), b"null").unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();

        assert!(matches!(
            persist_favorite_item_changes(&store, &lease, "BrokenSidecarFavoriteRider", &[], 1_000,),
            Err(FavoriteItemPersistError::Import(
                FavoriteItemImportError::Json { .. }
            ))
        ));
        let after = store.reload("BrokenSidecarFavoriteRider").unwrap();
        assert_eq!(after.revision, None);
        assert!(after.profile.favorite_items.is_none());
    }

    #[test]
    fn imported_sidecar_must_fit_the_current_favorite_reply_limit() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("OversizedSidecarFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        fs::write(
            directory.join("Favorite.json"),
            br#"[
                {"ItemCatID":3,"ItemID":1450,"ItemSN":1},
                {"ItemCatID":3,"ItemID":1450,"ItemSN":2}
            ]"#,
        )
        .unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();

        assert!(matches!(
            persist_favorite_item_changes(&store, &lease, "OversizedSidecarFavoriteRider", &[], 1,),
            Err(FavoriteItemPersistError::State(
                FavoriteItemStateError::TooManyItems {
                    count: 2,
                    maximum: 1,
                }
            ))
        ));
        assert!(
            store
                .reload("OversizedSidecarFavoriteRider")
                .unwrap()
                .profile
                .favorite_items
                .is_none()
        );
    }

    #[test]
    fn oversized_sidecar_bytes_fail_before_sealing_the_marker() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("ByteCapFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        fs::write(directory.join("Favorite.json"), b"[\n]").unwrap();
        let store = ProfileStore::with_maximum_bytes(root.path(), 2);
        let lease = store.acquire_race_run_lease().unwrap();

        assert!(matches!(
            persist_favorite_item_changes(&store, &lease, "ByteCapFavoriteRider", &[], 1_000),
            Err(FavoriteItemPersistError::Import(
                FavoriteItemImportError::TooLarge {
                    length: 3,
                    maximum: 2,
                    ..
                }
            ))
        ));
        let after = store.reload("ByteCapFavoriteRider").unwrap();
        assert_eq!(after.revision, None);
        assert!(after.profile.favorite_items.is_none());
    }

    #[test]
    fn cas_retry_reuses_the_first_sidecar_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("CasFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        let imported = FavoriteItems::try_from_items([FavoriteItemKey::new(3, 1_450, 1)]).unwrap();
        let replaced = FavoriteItems::try_from_items([FavoriteItemKey::new(3, 1_450, 2)]).unwrap();
        fs::write(
            directory.join("Favorite.json"),
            serde_json::to_vec(&imported).unwrap(),
        )
        .unwrap();
        let store = ProfileStore::new(root.path());
        let competing_store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let mut inject_conflict = true;
        let mut observed = Vec::new();

        let outcome = store
            .transaction_with_legacy_favorite_items(&lease, "CasFavoriteRider", |items, _origin| {
                observed.push(items.clone());
                if inject_conflict {
                    inject_conflict = false;
                    fs::write(
                        directory.join("Favorite.json"),
                        serde_json::to_vec(&replaced).unwrap(),
                    )
                    .unwrap();
                    competing_store
                        .transaction("CasFavoriteRider", |profile| {
                            let mut profile = profile.clone();
                            profile.rider.lucci += 1;
                            ProfileMutation::changed((), profile)
                        })
                        .unwrap();
                }
                Ok::<_, FavoriteItemStateError>(((), items.clone()))
            })
            .unwrap();

        assert_eq!(observed, vec![imported.clone(), imported.clone()]);
        assert!(matches!(
            outcome,
            p5136_profile::ProfileTransaction::Committed { .. }
        ));
        assert_eq!(
            store
                .reload("CasFavoriteRider")
                .unwrap()
                .profile
                .favorite_items,
            Some(imported)
        );
    }

    #[test]
    fn cas_retry_prefers_a_competing_canonical_marker_over_a_stale_sidecar() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("CanonicalCasFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        let imported = FavoriteItems::try_from_items([FavoriteItemKey::new(3, 1_450, 1)]).unwrap();
        let winner = FavoriteItems::try_from_items([FavoriteItemKey::new(3, 1_450, 2)]).unwrap();
        fs::write(
            directory.join("Favorite.json"),
            serde_json::to_vec(&imported).unwrap(),
        )
        .unwrap();
        let store = ProfileStore::new(root.path());
        let competing_store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let mut inject_conflict = true;
        let mut observed = Vec::new();

        let outcome = store
            .transaction_with_legacy_favorite_items(
                &lease,
                "CanonicalCasFavoriteRider",
                |items, origin| {
                    observed.push((items.clone(), origin));
                    if inject_conflict {
                        inject_conflict = false;
                        competing_store
                            .transaction("CanonicalCasFavoriteRider", |profile| {
                                let mut profile = profile.clone();
                                profile.favorite_items = Some(winner.clone());
                                ProfileMutation::changed((), profile)
                            })
                            .unwrap();
                    }
                    Ok::<_, FavoriteItemStateError>(((), items.clone()))
                },
            )
            .unwrap();

        assert_eq!(
            observed,
            vec![
                (imported, FavoriteItemStateOrigin::LegacySidecar),
                (winner.clone(), FavoriteItemStateOrigin::Canonical),
            ]
        );
        assert!(matches!(
            outcome,
            p5136_profile::ProfileTransaction::Unchanged { .. }
        ));
        assert_eq!(
            store
                .reload("CanonicalCasFavoriteRider")
                .unwrap()
                .profile
                .favorite_items,
            Some(winner)
        );
    }

    #[cfg(unix)]
    #[test]
    fn fifo_sidecar_fails_closed_without_blocking_the_profile_worker() {
        use std::{
            process::Command,
            sync::{Arc, mpsc},
            thread,
            time::Duration,
        };

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("FifoFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        assert!(
            Command::new("mkfifo")
                .arg(directory.join("Favorite.json"))
                .status()
                .unwrap()
                .success()
        );
        let store = Arc::new(ProfileStore::new(root.path()));
        let lease = Arc::new(store.acquire_race_run_lease().unwrap());
        let worker_store = Arc::clone(&store);
        let worker_lease = Arc::clone(&lease);
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(persist_favorite_item_changes(
                &worker_store,
                &worker_lease,
                "FifoFavoriteRider",
                &[],
                1_000,
            ));
        });

        assert!(matches!(
            receiver
                .recv_timeout(Duration::from_secs(1))
                .expect("FIFO sidecar import must not block"),
            Err(FavoriteItemPersistError::Import(
                FavoriteItemImportError::InvalidStorageEntry { .. }
            ))
        ));
        assert!(
            store
                .reload("FifoFavoriteRider")
                .unwrap()
                .profile
                .favorite_items
                .is_none()
        );
    }

    #[cfg(unix)]
    #[test]
    fn symbolic_link_sidecar_is_never_followed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("LinkedFavoriteRider");
        let outside = root.path().join("outside-favorite.json");
        fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("Launcher.json"), b"{}").unwrap();
        fs::write(&outside, br#"[{"ItemCatID":3,"ItemID":1450,"ItemSN":1}]"#).unwrap();
        symlink(&outside, directory.join("Favorite.json")).unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();

        assert!(matches!(
            persist_favorite_item_changes(&store, &lease, "LinkedFavoriteRider", &[], 1_000),
            Err(FavoriteItemPersistError::Import(
                FavoriteItemImportError::InvalidStorageEntry { .. }
            ))
        ));
        assert!(
            store
                .reload("LinkedFavoriteRider")
                .unwrap()
                .profile
                .favorite_items
                .is_none()
        );
    }

    #[test]
    fn idempotent_update_canonicalizes_a_matching_legacy_snapshot_once() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("LegacyFavoriteRider");
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("Launcher.json"),
            serde_json::to_vec(&json!({
                "P5136RustFavoriteItems": [
                    {"ItemCatID": 3, "ItemID": 1450, "ItemSN": 1}
                ]
            }))
            .unwrap(),
        )
        .unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        let legacy = store.load_or_create("LegacyFavoriteRider").unwrap();
        assert_eq!(legacy.revision, None);

        let first =
            persist_favorite_item_changes(&store, &lease, "LegacyFavoriteRider", &[add(1)], 1_000)
                .unwrap();
        assert_eq!(first.revision(), 1);
        assert_eq!(
            store
                .reload("LegacyFavoriteRider")
                .unwrap()
                .profile
                .favorite_items
                .as_ref()
                .unwrap()
                .as_slice(),
            &[FavoriteItemKey::new(3, 1_450, 1)]
        );

        let retry =
            persist_favorite_item_changes(&store, &lease, "LegacyFavoriteRider", &[add(1)], 1_000)
                .unwrap();
        assert_eq!(retry.revision(), first.revision());
    }

    #[test]
    fn several_stock_sized_batches_can_grow_the_collection_beyond_two_hundred() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        canonicalize_empty(&store, "LargeFavoriteRider");
        let first = (0..200).map(add).collect::<Vec<_>>();
        let second = (200..400).map(add).collect::<Vec<_>>();

        persist_favorite_item_changes(&store, &lease, "LargeFavoriteRider", &first, 1_000).unwrap();
        let durable =
            persist_favorite_item_changes(&store, &lease, "LargeFavoriteRider", &second, 1_000)
                .unwrap();
        let loaded = store.reload("LargeFavoriteRider").unwrap();
        assert_eq!(
            loaded
                .profile
                .favorite_items
                .expect("successful update canonicalizes the field")
                .as_slice()
                .len(),
            400
        );
        assert_eq!(loaded.revision, Some(durable.revision()));
    }

    #[test]
    fn final_cap_rejection_leaves_the_exact_profile_revision_unchanged() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let lease = store.acquire_race_run_lease().unwrap();
        canonicalize_empty(&store, "BoundedFavoriteRider");
        let initial = store.reload("BoundedFavoriteRider").unwrap();
        persist_favorite_item_changes(&store, &lease, "BoundedFavoriteRider", &[add(1), add(2)], 2)
            .unwrap();
        let before = store.reload("BoundedFavoriteRider").unwrap();
        assert_ne!(before.revision, initial.revision);

        assert!(matches!(
            persist_favorite_item_changes(&store, &lease, "BoundedFavoriteRider", &[add(3)], 2),
            Err(FavoriteItemPersistError::State(
                FavoriteItemStateError::TooManyItems {
                    count: 3,
                    maximum: 2
                }
            ))
        ));
        let after = store.reload("BoundedFavoriteRider").unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.profile, before.profile);
    }
}
