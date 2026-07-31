//! Bounded, stable-order item-key collections stored inside a Rust profile.
//!
//! The C# server uses a separate sidecar for this state. Rust profiles use an
//! explicitly namespaced field instead; importing the C# sidecar is an
//! intentionally separate migration concern.

use std::{
    collections::{HashMap, HashSet},
    fmt, slice,
};

pub use p5136_core::item_state_protocol::DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS;
use p5136_core::item_state_protocol::{
    FavoriteItemChange, FavoriteItemKey, FavoriteItemOperation, MAX_FAVORITE_ITEM_UPDATE_RECORDS,
};
use serde::{
    Deserialize, Deserializer, Serialize, Serializer,
    de::{Error as _, SeqAccess, Visitor},
    ser::SerializeSeq,
};
use thiserror::Error;

/// A bounded, insertion-ordered collection of unique P5136 item keys.
///
/// Mutation is exposed only as a pure whole-batch operation. That keeps the
/// original value available for an atomic profile transaction if validation
/// rejects any part of the proposed result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FavoriteItems {
    items: Vec<FavoriteItemKey>,
}

/// Locked items have the same key, stable-order, deduplication, batch, and wire
/// bounds as favorite items. Keeping one value abstraction prevents their
/// persistence rules from drifting while profile fields retain distinct names.
pub type LockedItems = FavoriteItems;

/// Projects an absent, not-yet-canonicalized profile field as an empty wire
/// snapshot.
#[must_use]
pub fn favorite_item_snapshot(items: Option<&FavoriteItems>) -> &[FavoriteItemKey] {
    item_collection_snapshot(items)
}

#[must_use]
pub fn item_collection_snapshot(items: Option<&FavoriteItems>) -> &[FavoriteItemKey] {
    items.map_or(&[], FavoriteItems::as_slice)
}

/// Purely applies one update to an optional persisted collection.
///
/// A successful result should be stored as `Some(result)`, including when the
/// input is absent and the update is empty or idempotent. That explicit empty
/// marker distinguishes a canonical empty collection from a legacy profile
/// whose external-import decision has not yet been made.
pub fn apply_favorite_item_changes(
    current: Option<&FavoriteItems>,
    changes: &[FavoriteItemChange],
    effective_maximum: usize,
) -> Result<FavoriteItems, FavoriteItemStateError> {
    apply_item_collection_changes(current, changes, effective_maximum)
}

pub fn apply_item_collection_changes(
    current: Option<&FavoriteItems>,
    changes: &[FavoriteItemChange],
    effective_maximum: usize,
) -> Result<FavoriteItems, FavoriteItemStateError> {
    current
        .cloned()
        .unwrap_or_default()
        .try_apply(changes, effective_maximum)
}

impl FavoriteItems {
    /// Constructs a collection after validating its persisted-state
    /// invariants.
    pub fn try_from_items(
        items: impl IntoIterator<Item = FavoriteItemKey>,
    ) -> Result<Self, FavoriteItemStateError> {
        let mut builder = FavoriteItemsBuilder::new(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS);
        for item in items {
            builder.push(item)?;
        }
        Ok(Self {
            items: builder.items,
        })
    }

    /// Returns the keys in their stable insertion order.
    #[must_use]
    pub fn as_slice(&self) -> &[FavoriteItemKey] {
        &self.items
    }

    /// Returns an iterator in stable insertion order.
    pub fn iter(&self) -> slice::Iter<'_, FavoriteItemKey> {
        self.items.iter()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    #[must_use]
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Applies one stock-bounded update batch to a clone of this collection.
    ///
    /// `effective_maximum` is capped at
    /// [`DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS`], so a caller cannot create
    /// state that the default one-megabyte reply limit cannot serialize. The
    /// limit is checked against the final whole-batch result: a batch may add
    /// and then remove an item while no intermediate state is observable.
    ///
    /// Adding an existing key and removing an absent key are idempotent
    /// no-ops. Repeated changes are evaluated in packet order, new or re-added
    /// keys are appended, and removals preserve the relative order of all
    /// remaining keys.
    ///
    /// If an operator lowers the effective limit below already-persisted
    /// state, an over-limit result is rejected only when the batch grows the
    /// collection. Same-size results remain valid so replaying a successful
    /// shrinking batch is idempotent.
    pub fn try_apply(
        &self,
        changes: &[FavoriteItemChange],
        effective_maximum: usize,
    ) -> Result<Self, FavoriteItemStateError> {
        if changes.len() > MAX_FAVORITE_ITEM_UPDATE_RECORDS {
            return Err(FavoriteItemStateError::TooManyChanges {
                count: changes.len(),
                maximum: MAX_FAVORITE_ITEM_UPDATE_RECORDS,
            });
        }

        let maximum = effective_maximum.min(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS);
        let original = self.items.iter().copied().collect::<HashSet<_>>();
        let mut present = original.clone();
        let mut removed_original = HashSet::with_capacity(changes.len());
        let mut additions = Vec::with_capacity(changes.len());
        let mut latest_addition = HashMap::with_capacity(changes.len());

        for change in changes {
            let item = change.item();
            match change.operation() {
                FavoriteItemOperation::Add => {
                    if present.insert(item) {
                        latest_addition.insert(item, additions.len());
                        additions.push(item);
                    }
                }
                FavoriteItemOperation::Remove => {
                    if present.remove(&item) {
                        if original.contains(&item) {
                            removed_original.insert(item);
                        }
                        latest_addition.remove(&item);
                    }
                }
            }
        }

        let mut items = Vec::with_capacity(present.len());
        items.extend(
            self.items
                .iter()
                .copied()
                .filter(|item| present.contains(item) && !removed_original.contains(item)),
        );
        items.extend(
            additions
                .into_iter()
                .enumerate()
                .filter_map(|(index, item)| {
                    (latest_addition.get(&item) == Some(&index)).then_some(item)
                }),
        );

        if items.len() > maximum && items.len() > self.items.len() {
            return Err(FavoriteItemStateError::TooManyItems {
                count: items.len(),
                maximum,
            });
        }

        Ok(Self { items })
    }
}

impl AsRef<[FavoriteItemKey]> for FavoriteItems {
    fn as_ref(&self) -> &[FavoriteItemKey] {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a FavoriteItems {
    type Item = &'a FavoriteItemKey;
    type IntoIter = slice::Iter<'a, FavoriteItemKey>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Serialize for FavoriteItems {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.items.len()))?;
        for item in &self.items {
            sequence.serialize_element(&FavoriteItemRecord::from(*item))?;
        }
        sequence.end()
    }
}

impl<'de> Deserialize<'de> for FavoriteItems {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(FavoriteItemsVisitor)
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FavoriteItemStateError {
    #[error(
        "favorite-item record {duplicate_index} duplicates record {first_index} \
         ({category}:{item_id}:{serial})"
    )]
    DuplicateItem {
        first_index: usize,
        duplicate_index: usize,
        category: u16,
        item_id: u16,
        serial: u16,
    },

    #[error("favorite-item collection has {count} records; maximum is {maximum}")]
    TooManyItems { count: usize, maximum: usize },

    #[error("favorite-item update has {count} changes; stock maximum is {maximum}")]
    TooManyChanges { count: usize, maximum: usize },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FavoriteItemRecord {
    #[serde(rename = "ItemCatID")]
    category: u16,
    #[serde(rename = "ItemID")]
    item_id: u16,
    #[serde(rename = "ItemSN")]
    serial: u16,
}

impl From<FavoriteItemKey> for FavoriteItemRecord {
    fn from(item: FavoriteItemKey) -> Self {
        Self {
            category: item.category(),
            item_id: item.item_id(),
            serial: item.serial(),
        }
    }
}

impl From<FavoriteItemRecord> for FavoriteItemKey {
    fn from(record: FavoriteItemRecord) -> Self {
        Self::new(record.category, record.item_id, record.serial)
    }
}

struct FavoriteItemsVisitor;

impl<'de> Visitor<'de> for FavoriteItemsVisitor {
    type Value = FavoriteItems;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "at most {DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS} unique favorite-item records"
        )
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let capacity = sequence
            .size_hint()
            .unwrap_or(0)
            .min(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS);
        let mut builder =
            FavoriteItemsBuilder::with_capacity(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, capacity);

        while let Some(record) = sequence.next_element::<FavoriteItemRecord>()? {
            builder.push(record.into()).map_err(A::Error::custom)?;
        }

        Ok(FavoriteItems {
            items: builder.items,
        })
    }
}

struct FavoriteItemsBuilder {
    maximum: usize,
    items: Vec<FavoriteItemKey>,
    first_positions: HashMap<FavoriteItemKey, usize>,
}

impl FavoriteItemsBuilder {
    fn new(maximum: usize) -> Self {
        Self::with_capacity(maximum, 0)
    }

    fn with_capacity(maximum: usize, capacity: usize) -> Self {
        Self {
            maximum,
            items: Vec::with_capacity(capacity.min(maximum)),
            first_positions: HashMap::with_capacity(capacity.min(maximum)),
        }
    }

    fn push(&mut self, item: FavoriteItemKey) -> Result<(), FavoriteItemStateError> {
        let duplicate_index = self.items.len();
        if let Some(&first_index) = self.first_positions.get(&item) {
            return Err(FavoriteItemStateError::DuplicateItem {
                first_index,
                duplicate_index,
                category: item.category(),
                item_id: item.item_id(),
                serial: item.serial(),
            });
        }
        if duplicate_index >= self.maximum {
            return Err(FavoriteItemStateError::TooManyItems {
                count: duplicate_index + 1,
                maximum: self.maximum,
            });
        }

        self.first_positions.insert(item, duplicate_index);
        self.items.push(item);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use p5136_core::item_state_protocol::{
        FavoriteItemChange, FavoriteItemKey, FavoriteItemOperation,
        MAX_FAVORITE_ITEM_UPDATE_RECORDS,
    };
    use serde_json::json;

    use super::{
        DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, FavoriteItemRecord, FavoriteItemStateError,
        FavoriteItems, apply_favorite_item_changes, favorite_item_snapshot,
    };

    fn item(value: usize) -> FavoriteItemKey {
        FavoriteItemKey::new(
            u16::try_from(value >> 16).expect("test value category fits"),
            u16::try_from(value & usize::from(u16::MAX)).expect("masked test item ID fits"),
            1,
        )
    }

    fn add(value: usize) -> FavoriteItemChange {
        FavoriteItemChange::new(item(value), FavoriteItemOperation::Add)
    }

    fn remove(value: usize) -> FavoriteItemChange {
        FavoriteItemChange::new(item(value), FavoriteItemOperation::Remove)
    }

    #[test]
    fn json_roundtrip_uses_exact_namespaced_record_fields_and_stable_order() {
        let items = FavoriteItems::try_from_items([item(2), item(1)]).unwrap();

        let encoded = serde_json::to_value(&items).unwrap();
        assert_eq!(
            encoded,
            json!([
                {"ItemCatID": 0, "ItemID": 2, "ItemSN": 1},
                {"ItemCatID": 0, "ItemID": 1, "ItemSN": 1}
            ])
        );
        let decoded: FavoriteItems = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, items);
    }

    #[test]
    fn malformed_persisted_duplicates_return_a_typed_error() {
        let duplicate = item(7);
        assert_eq!(
            FavoriteItems::try_from_items([item(1), duplicate, duplicate]),
            Err(FavoriteItemStateError::DuplicateItem {
                first_index: 1,
                duplicate_index: 2,
                category: 0,
                item_id: 7,
                serial: 1,
            })
        );

        let json_error = serde_json::from_value::<FavoriteItems>(json!([
            {"ItemCatID": 3, "ItemID": 1450, "ItemSN": 2},
            {"ItemCatID": 3, "ItemID": 1450, "ItemSN": 2}
        ]))
        .unwrap_err();
        assert!(
            json_error
                .to_string()
                .contains("favorite-item record 1 duplicates record 0 (3:1450:2)")
        );
    }

    #[test]
    fn malformed_persisted_oversize_returns_a_typed_error() {
        let excessive = (0..=DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)
            .map(item)
            .collect::<Vec<_>>();
        assert_eq!(
            FavoriteItems::try_from_items(excessive.iter().copied()),
            Err(FavoriteItemStateError::TooManyItems {
                count: DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS + 1,
                maximum: DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS,
            })
        );

        let persisted = excessive
            .into_iter()
            .map(FavoriteItemRecord::from)
            .collect::<Vec<_>>();
        let encoded = serde_json::to_vec(&persisted).unwrap();
        let json_error = serde_json::from_slice::<FavoriteItems>(&encoded).unwrap_err();
        assert!(json_error.to_string().contains(&format!(
            "favorite-item collection has {} records; maximum is {}",
            DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS + 1,
            DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS
        )));
    }

    #[test]
    fn apply_is_stable_and_idempotent() {
        let source = FavoriteItems::try_from_items([item(1), item(2)]).unwrap();
        let applied = source
            .try_apply(
                &[add(1), remove(9), add(3), remove(2), add(2)],
                DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS,
            )
            .unwrap();

        assert_eq!(applied.as_slice(), &[item(1), item(3), item(2)]);
        assert_eq!(source.as_slice(), &[item(1), item(2)]);
    }

    #[test]
    fn repeated_changes_have_deterministic_sequential_order() {
        let source = FavoriteItems::try_from_items([item(1), item(2)]).unwrap();
        let applied = source
            .try_apply(
                &[remove(1), add(1), add(3), remove(3), add(3)],
                DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS,
            )
            .unwrap();

        assert_eq!(applied.as_slice(), &[item(2), item(1), item(3)]);
        assert_eq!(source.as_slice(), &[item(1), item(2)]);
    }

    #[test]
    fn apply_checks_the_final_whole_batch_and_is_atomic_on_rejection() {
        let source = FavoriteItems::try_from_items([item(1), item(2)]).unwrap();

        let replacement = source.try_apply(&[add(3), remove(1)], 2).unwrap();
        assert_eq!(replacement.as_slice(), &[item(2), item(3)]);

        assert_eq!(
            source.try_apply(&[add(3)], 2),
            Err(FavoriteItemStateError::TooManyItems {
                count: 3,
                maximum: 2,
            })
        );
        assert_eq!(source.as_slice(), &[item(1), item(2)]);
    }

    #[test]
    fn consecutive_stock_sized_batches_accumulate_beyond_two_hundred() {
        let first_batch = (0..MAX_FAVORITE_ITEM_UPDATE_RECORDS)
            .map(add)
            .collect::<Vec<_>>();
        let first = FavoriteItems::default()
            .try_apply(&first_batch, DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)
            .unwrap();
        assert_eq!(first.len(), 200);

        let two_hundred_and_one = first
            .try_apply(&[add(200)], DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)
            .unwrap();
        assert_eq!(two_hundred_and_one.len(), 201);

        let second_batch = (200..400).map(add).collect::<Vec<_>>();
        let four_hundred = first
            .try_apply(&second_batch, DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)
            .unwrap();
        assert_eq!(four_hundred.len(), 400);
        assert_eq!(four_hundred.as_slice()[0], item(0));
        assert_eq!(four_hundred.as_slice()[399], item(399));
    }

    #[test]
    fn absent_state_projects_empty_and_first_noop_update_becomes_canonical_empty() {
        assert!(favorite_item_snapshot(None).is_empty());

        let canonical =
            apply_favorite_item_changes(None, &[remove(7)], DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)
                .unwrap();
        assert!(canonical.is_empty());
        assert!(favorite_item_snapshot(Some(&canonical)).is_empty());
    }

    #[test]
    fn lowered_limit_recovery_is_idempotent_but_cannot_grow() {
        let source = FavoriteItems::try_from_items([item(1), item(2), item(3), item(4)]).unwrap();

        let reduced = source.try_apply(&[remove(1)], 2).unwrap();
        assert_eq!(reduced.as_slice(), &[item(2), item(3), item(4)]);
        let replay = reduced.try_apply(&[remove(1)], 2).unwrap();
        assert_eq!(replay, reduced);
        assert_eq!(source.try_apply(&[], 2).unwrap(), source);
        assert_eq!(
            source.try_apply(&[add(5)], 2),
            Err(FavoriteItemStateError::TooManyItems {
                count: 5,
                maximum: 2,
            })
        );
        assert_eq!(source.as_slice(), &[item(1), item(2), item(3), item(4)]);
    }

    #[test]
    fn apply_clamps_the_caller_limit_to_the_default_reply_capacity() {
        let source =
            FavoriteItems::try_from_items((0..DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS).map(item))
                .unwrap();

        assert_eq!(
            source.try_apply(&[add(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)], usize::MAX),
            Err(FavoriteItemStateError::TooManyItems {
                count: DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS + 1,
                maximum: DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS,
            })
        );
        assert_eq!(source.len(), DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS);
    }

    #[test]
    fn rejects_a_non_stock_sized_batch_without_changing_the_source() {
        let source = FavoriteItems::try_from_items([item(1)]).unwrap();
        let changes = vec![add(2); MAX_FAVORITE_ITEM_UPDATE_RECORDS + 1];

        assert_eq!(
            source.try_apply(&changes, DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS),
            Err(FavoriteItemStateError::TooManyChanges {
                count: MAX_FAVORITE_ITEM_UPDATE_RECORDS + 1,
                maximum: MAX_FAVORITE_ITEM_UPDATE_RECORDS,
            })
        );
        assert_eq!(source.as_slice(), &[item(1)]);
    }

    #[test]
    fn maximum_collection_pretty_profile_json_fits_the_default_profile_cap() {
        let maximum = FavoriteItems::try_from_items(
            (0..DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS).map(|index| {
                FavoriteItemKey::new(
                    u16::MAX - u16::try_from(index >> 16).expect("test category fits"),
                    u16::MAX
                        - u16::try_from(index & usize::from(u16::MAX))
                            .expect("masked test item ID fits"),
                    u16::MAX,
                )
            }),
        )
        .unwrap();
        let profile = crate::Profile {
            favorite_items: Some(maximum),
            ..crate::Profile::default()
        };
        let pretty = serde_json::to_vec_pretty(&profile).unwrap();

        assert!(
            u64::try_from(pretty.len()).unwrap() < crate::store::DEFAULT_MAX_PROFILE_BYTES,
            "maximum pretty profile JSON is {} bytes, profile cap is {}",
            pretty.len(),
            crate::store::DEFAULT_MAX_PROFILE_BYTES
        );
    }
}
