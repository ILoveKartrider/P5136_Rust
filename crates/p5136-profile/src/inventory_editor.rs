//! Operator-facing, nickname-scoped inventory editing.
//!
//! The stock P5136 inventory already grants one serial-1 instance of every
//! usable catalog kart. Additional copies therefore only need durable
//! `(kart_id, serial)` grants. Floater/plant/level/parts sidecars and rider equipment
//! use the same pair, which lets each copy retain different enhancement data.

use std::collections::{BTreeMap, HashSet};

use p5136_core::floater_physics::p5136_floater_spec;
use thiserror::Error;

use crate::{
    CatalogInventory, EquipmentExceptions, EquipmentProfileError, GrantedKart, Profile,
    ProfileMutation, ProfileStore, ProfileStoreError, ProfileTransaction, RaceRunLease,
    SavedProfile,
};

pub const MAX_ADDITIONAL_KARTS_PER_PROFILE: usize = 4_096;
pub const MAX_KART_SEARCH_QUERY_CHARS: usize = 64;
pub const MAX_KART_SEARCH_RESULTS: usize = 50;
const MAX_CLIENT_SAFE_KART_SERIAL: u16 = 32_767;
pub const FLOATER_333_CODES: [i16; 3] = [603, 903, 703];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KartGrantOptions {
    pub apply_floater: bool,
    pub floater_codes: [i16; 3],
    pub apply_grade_five: bool,
}

impl Default for KartGrantOptions {
    fn default() -> Self {
        Self {
            apply_floater: false,
            floater_codes: FLOATER_333_CODES,
            apply_grade_five: false,
        }
    }
}

impl KartGrantOptions {
    #[must_use]
    pub const fn requests_enhancements(self) -> bool {
        self.apply_floater || self.apply_grade_five
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AppliedKartGrantEnhancements {
    pub floater_codes: Option<[i16; 3]>,
    pub grade_five: bool,
    pub durability_warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KartCatalogSearchResult {
    pub kart_id: u16,
    pub name: String,
    pub auto_granted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdditionalKart {
    pub kart_id: u16,
    pub serial: u16,
    pub name: String,
}

#[derive(Debug)]
pub enum AddKartOutcome {
    Durable {
        kart: AdditionalKart,
        additional_karts: Vec<AdditionalKart>,
        saved: SavedProfile,
        enhancements: AppliedKartGrantEnhancements,
    },
    DurabilityUncertain {
        kart: AdditionalKart,
        additional_karts: Vec<AdditionalKart>,
        saved: SavedProfile,
        error: ProfileStoreError,
        enhancements: AppliedKartGrantEnhancements,
    },
}

impl AddKartOutcome {
    #[must_use]
    pub const fn kart(&self) -> &AdditionalKart {
        match self {
            Self::Durable { kart, .. } | Self::DurabilityUncertain { kart, .. } => kart,
        }
    }

    #[must_use]
    pub const fn saved(&self) -> &SavedProfile {
        match self {
            Self::Durable { saved, .. } | Self::DurabilityUncertain { saved, .. } => saved,
        }
    }

    #[must_use]
    pub fn additional_karts(&self) -> &[AdditionalKart] {
        match self {
            Self::Durable {
                additional_karts, ..
            }
            | Self::DurabilityUncertain {
                additional_karts, ..
            } => additional_karts,
        }
    }

    #[must_use]
    pub const fn is_durability_uncertain(&self) -> bool {
        matches!(self, Self::DurabilityUncertain { .. })
    }

    #[must_use]
    pub const fn enhancements(&self) -> &AppliedKartGrantEnhancements {
        match self {
            Self::Durable { enhancements, .. } | Self::DurabilityUncertain { enhancements, .. } => {
                enhancements
            }
        }
    }

    fn set_enhancements(&mut self, applied: AppliedKartGrantEnhancements) {
        match self {
            Self::Durable { enhancements, .. } | Self::DurabilityUncertain { enhancements, .. } => {
                *enhancements = applied;
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum KartInventoryEditError {
    #[error(transparent)]
    Store(#[from] ProfileStoreError),

    #[error(transparent)]
    Equipment(#[from] EquipmentProfileError),

    #[error("kart ID {kart_id} is not a known category-3 kart in this catalog")]
    KartNotGrantable { kart_id: u16 },

    #[error("profile already contains the maximum {maximum} additional kart records")]
    TooManyAdditionalKarts { maximum: usize },

    #[error("kart ID {kart_id} has no free client-safe serial")]
    SerialExhausted { kart_id: u16 },

    #[error(
        "kart ID {kart_id} does not expose the P5136 DescEnchantCap required for Floater/grade-five enhancements"
    )]
    LegacyEnhancementUnsupported { kart_id: u16 },

    #[error("kart ID {kart_id} cannot be represented by the signed P5136 enhancement codec")]
    EnhancementTargetOutOfRange { kart_id: u16 },

    #[error("Floater grant codes {codes:?} are not a valid distinct P5136 three-slot combination")]
    InvalidFloaterGrantCodes { codes: [i16; 3] },

    #[error(
        "kart {kart_id}:{serial} was granted, but the requested {stage} enhancement was not fully applied: {source}"
    )]
    EnhancementAfterGrant {
        kart_id: u16,
        serial: u16,
        stage: &'static str,
        #[source]
        source: EquipmentProfileError,
    },

    #[error("inventory transaction unexpectedly returned unchanged after a successful mutation")]
    UnexpectedUnchanged,
}

/// Searches usable catalog karts by display-name fragment or decimal ID.
///
/// Whitespace and letter case are ignored, so `세베크v1` finds `세베크 V1`.
/// Results are ranked exact ID/name, prefix, then substring.
#[must_use]
pub fn search_karts(
    catalog: &CatalogInventory,
    query: &str,
    limit: usize,
) -> Vec<KartCatalogSearchResult> {
    let trimmed = query.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_KART_SEARCH_QUERY_CHARS || limit == 0 {
        return Vec::new();
    }
    let normalized_query = normalize_search_text(trimmed);
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let numeric_query = trimmed.parse::<u16>().ok();
    let names = if numeric_query.is_some() {
        known_kart_names(catalog)
    } else {
        grantable_kart_names(catalog)
    };
    let mut matches = names
        .into_iter()
        .filter_map(|(kart_id, name)| {
            let normalized_name = normalize_search_text(&name);
            let rank = if numeric_query == Some(kart_id) {
                0
            } else if numeric_query.is_some() {
                return None;
            } else if normalized_name == normalized_query {
                1
            } else if normalized_name.starts_with(&normalized_query) {
                2
            } else if normalized_name.contains(&normalized_query) {
                3
            } else {
                return None;
            };
            Some((rank, normalized_name.len(), name, kart_id))
        })
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        (left.0, left.1, &left.2, left.3).cmp(&(right.0, right.1, &right.2, right.3))
    });
    matches
        .into_iter()
        .take(limit.min(MAX_KART_SEARCH_RESULTS))
        .map(|(_, _, name, kart_id)| KartCatalogSearchResult {
            kart_id,
            name,
            auto_granted: catalog.grants_item(3, kart_id),
        })
        .collect()
}

/// Returns only the additional serial-2+ copies that the inventory serializer
/// will expose to the client. Invalid legacy records remain preserved on disk
/// but are omitted from this operator view, matching runtime behavior.
#[must_use]
pub fn additional_karts(catalog: &CatalogInventory, profile: &Profile) -> Vec<AdditionalKart> {
    let names = known_kart_names(catalog);
    let mut seen = HashSet::new();
    let mut karts = profile
        .granted_karts
        .iter()
        .filter_map(|grant| {
            let name = names.get(&grant.kart_id)?;
            (grant.serial >= 2
                && grant.serial <= MAX_CLIENT_SAFE_KART_SERIAL
                && seen.insert((grant.kart_id, grant.serial)))
            .then(|| AdditionalKart {
                kart_id: grant.kart_id,
                serial: grant.serial,
                name: name.clone(),
            })
        })
        .collect::<Vec<_>>();
    karts.sort_by(|left, right| {
        (&left.name, left.kart_id, left.serial).cmp(&(&right.name, right.kart_id, right.serial))
    });
    karts
}

/// Adds one additional copy to a nickname's immutable-revision profile.
///
/// The transaction closure is retry-safe: serial allocation is recomputed
/// from each compare-and-swap snapshot and performs no external side effects.
pub fn add_kart(
    store: &ProfileStore,
    catalog: &CatalogInventory,
    nickname: &str,
    kart_id: u16,
) -> Result<AddKartOutcome, KartInventoryEditError> {
    add_kart_with_options(
        store,
        catalog,
        nickname,
        kart_id,
        KartGrantOptions::default(),
    )
}

pub fn add_kart_with_options(
    store: &ProfileStore,
    catalog: &CatalogInventory,
    nickname: &str,
    kart_id: u16,
    options: KartGrantOptions,
) -> Result<AddKartOutcome, KartInventoryEditError> {
    validate_grant_enhancement_options(catalog, kart_id, options)?;
    let lease = store.acquire_offline_edit_lease()?;
    let transaction =
        store.transaction_with_equipment_exceptions(&lease, nickname, |profile, equipment| {
            let mut next = profile.clone();
            match add_kart_to_profile(catalog, equipment, &mut next, kart_id) {
                Ok(grant) => ProfileMutation::changed(Ok(grant), next),
                Err(error) => ProfileMutation::Unchanged(Err(error)),
            }
        })?;

    let mut outcome = finish_add_kart_transaction(catalog, transaction)?;
    let applied =
        apply_offline_grant_enhancements(store, &lease, nickname, outcome.kart(), options)?;
    outcome.set_enhancements(applied);
    Ok(outcome)
}

/// Adds one kart through an active server's run lease.
///
/// The caller must submit this operation through the server's nickname-scoped
/// profile lane. That lane keeps this equipment snapshot ordered with live
/// Floater/plant/level/parts mutations, while the run lease prevents an
/// independent offline editor from racing the active server.
pub fn add_kart_during_race_run(
    store: &ProfileStore,
    lease: &RaceRunLease,
    catalog: &CatalogInventory,
    nickname: &str,
    kart_id: u16,
) -> Result<AddKartOutcome, KartInventoryEditError> {
    add_kart_during_race_run_with_options(
        store,
        lease,
        catalog,
        nickname,
        kart_id,
        KartGrantOptions::default(),
    )
}

pub fn add_kart_during_race_run_with_options(
    store: &ProfileStore,
    lease: &RaceRunLease,
    catalog: &CatalogInventory,
    nickname: &str,
    kart_id: u16,
    options: KartGrantOptions,
) -> Result<AddKartOutcome, KartInventoryEditError> {
    validate_grant_enhancement_options(catalog, kart_id, options)?;
    let equipment = if store.profile_exists(nickname)? {
        store.load_equipment_exceptions(lease, nickname)?
    } else {
        EquipmentExceptions::default()
    };
    let transaction = store.transaction(nickname, |profile| {
        let mut next = profile.clone();
        match add_kart_to_profile(catalog, &equipment, &mut next, kart_id) {
            Ok(grant) => ProfileMutation::changed(Ok(grant), next),
            Err(error) => ProfileMutation::Unchanged(Err(error)),
        }
    })?;
    let mut outcome = finish_add_kart_transaction(catalog, transaction)?;
    let applied = apply_live_grant_enhancements(store, lease, nickname, outcome.kart(), options)?;
    outcome.set_enhancements(applied);
    Ok(outcome)
}

fn finish_add_kart_transaction(
    catalog: &CatalogInventory,
    transaction: ProfileTransaction<Result<GrantedKart, KartInventoryEditError>>,
) -> Result<AddKartOutcome, KartInventoryEditError> {
    match transaction {
        ProfileTransaction::Unchanged { value, .. } => match value {
            Ok(_) => Err(KartInventoryEditError::UnexpectedUnchanged),
            Err(error) => Err(error),
        },
        ProfileTransaction::Committed {
            value,
            profile,
            saved,
        } => {
            let grant = value?;
            let (kart, additional_karts) = describe_committed_grant(catalog, &profile, grant);
            Ok(AddKartOutcome::Durable {
                kart,
                additional_karts,
                saved,
                enhancements: AppliedKartGrantEnhancements::default(),
            })
        }
        ProfileTransaction::CommittedButDurabilityUncertain {
            value,
            profile,
            saved,
            error,
        } => {
            let grant = value?;
            let (kart, additional_karts) = describe_committed_grant(catalog, &profile, grant);
            Ok(AddKartOutcome::DurabilityUncertain {
                kart,
                additional_karts,
                saved,
                error,
                enhancements: AppliedKartGrantEnhancements::default(),
            })
        }
    }
}

fn validate_grant_enhancement_options(
    catalog: &CatalogInventory,
    kart_id: u16,
    options: KartGrantOptions,
) -> Result<(), KartInventoryEditError> {
    if !options.requests_enhancements() {
        return Ok(());
    }
    if !catalog.supports_legacy_kart_enhancements(kart_id) {
        return Err(KartInventoryEditError::LegacyEnhancementUnsupported { kart_id });
    }
    if options.apply_floater && p5136_floater_spec(options.floater_codes).is_none() {
        return Err(KartInventoryEditError::InvalidFloaterGrantCodes {
            codes: options.floater_codes,
        });
    }
    i16::try_from(kart_id)
        .map(|_| ())
        .map_err(|_| KartInventoryEditError::EnhancementTargetOutOfRange { kart_id })
}

fn apply_offline_grant_enhancements(
    store: &ProfileStore,
    lease: &crate::store::OfflineProfileEditLease,
    nickname: &str,
    kart: &AdditionalKart,
    options: KartGrantOptions,
) -> Result<AppliedKartGrantEnhancements, KartInventoryEditError> {
    apply_grant_enhancements(
        kart,
        options,
        |kart_id, serial| {
            store.set_floater_codes_offline(lease, nickname, kart_id, serial, options.floater_codes)
        },
        |kart_id, serial| store.upgrade_kart_level_offline(lease, nickname, kart_id, serial),
    )
}

fn apply_live_grant_enhancements(
    store: &ProfileStore,
    lease: &RaceRunLease,
    nickname: &str,
    kart: &AdditionalKart,
    options: KartGrantOptions,
) -> Result<AppliedKartGrantEnhancements, KartInventoryEditError> {
    apply_grant_enhancements(
        kart,
        options,
        |kart_id, serial| {
            store.set_floater_codes(lease, nickname, kart_id, serial, options.floater_codes)
        },
        |kart_id, serial| store.upgrade_kart_level(lease, nickname, kart_id, serial),
    )
}

fn apply_grant_enhancements<FTune, FLevel>(
    kart: &AdditionalKart,
    options: KartGrantOptions,
    apply_tune: FTune,
    apply_level: FLevel,
) -> Result<AppliedKartGrantEnhancements, KartInventoryEditError>
where
    FTune: FnOnce(
        i16,
        i16,
    ) -> Result<
        crate::EquipmentMutationOutcome<p5136_core::inventory::TuneExcRecord>,
        EquipmentProfileError,
    >,
    FLevel: FnOnce(
        i16,
        i16,
    ) -> Result<
        crate::EquipmentMutationOutcome<p5136_core::inventory::KartLevelExcRecord>,
        EquipmentProfileError,
    >,
{
    let kart_id = i16::try_from(kart.kart_id).map_err(|_| {
        KartInventoryEditError::EnhancementTargetOutOfRange {
            kart_id: kart.kart_id,
        }
    })?;
    let serial = i16::try_from(kart.serial).expect("allocated kart serial is client-safe");
    let mut applied = AppliedKartGrantEnhancements::default();
    if options.apply_floater {
        let outcome = apply_tune(kart_id, serial).map_err(|source| {
            KartInventoryEditError::EnhancementAfterGrant {
                kart_id: kart.kart_id,
                serial: kart.serial,
                stage: "operator-selected Floater",
                source,
            }
        })?;
        applied.floater_codes = Some(options.floater_codes);
        if let Some(warning) = outcome.durability_warning {
            applied.durability_warnings.push(warning.to_string());
        }
    }
    if options.apply_grade_five {
        let outcome = apply_level(kart_id, serial).map_err(|source| {
            KartInventoryEditError::EnhancementAfterGrant {
                kart_id: kart.kart_id,
                serial: kart.serial,
                stage: "grade-five",
                source,
            }
        })?;
        applied.grade_five = true;
        if let Some(warning) = outcome.durability_warning {
            applied.durability_warnings.push(warning.to_string());
        }
    }
    Ok(applied)
}

fn add_kart_to_profile(
    catalog: &CatalogInventory,
    equipment: &EquipmentExceptions,
    profile: &mut Profile,
    kart_id: u16,
) -> Result<GrantedKart, KartInventoryEditError> {
    if !catalog.contains_kart(kart_id) {
        return Err(KartInventoryEditError::KartNotGrantable { kart_id });
    }
    if profile.granted_karts.len() >= MAX_ADDITIONAL_KARTS_PER_PROFILE {
        return Err(KartInventoryEditError::TooManyAdditionalKarts {
            maximum: MAX_ADDITIONAL_KARTS_PER_PROFILE,
        });
    }

    let mut used = profile
        .granted_karts
        .iter()
        .filter(|grant| grant.kart_id == kart_id)
        .map(|grant| grant.serial)
        .collect::<HashSet<_>>();
    if profile.rider_item.kart == kart_id && profile.rider_item.kart_serial >= 2 {
        used.insert(profile.rider_item.kart_serial);
    }
    if let Ok(signed_kart_id) = i16::try_from(kart_id) {
        used.extend(
            equipment
                .tune
                .iter()
                .filter(|record| record.id == signed_kart_id)
                .filter_map(|record| u16::try_from(record.serial).ok()),
        );
        used.extend(
            equipment
                .plant
                .iter()
                .filter(|record| record.id == signed_kart_id)
                .filter_map(|record| u16::try_from(record.serial).ok()),
        );
        used.extend(
            equipment
                .kart_level
                .iter()
                .filter(|record| record.id == signed_kart_id)
                .filter_map(|record| u16::try_from(record.serial).ok()),
        );
        used.extend(
            equipment
                .parts
                .iter()
                .filter(|record| record.id == signed_kart_id)
                .filter_map(|record| u16::try_from(record.serial).ok()),
        );
    }
    let serial = (2_u16..=MAX_CLIENT_SAFE_KART_SERIAL)
        .find(|serial| !used.contains(serial))
        .ok_or(KartInventoryEditError::SerialExhausted { kart_id })?;
    let grant = GrantedKart { kart_id, serial };
    profile.granted_karts.push(grant);
    Ok(grant)
}

fn describe_committed_grant(
    catalog: &CatalogInventory,
    profile: &Profile,
    grant: GrantedKart,
) -> (AdditionalKart, Vec<AdditionalKart>) {
    let additional_karts = additional_karts(catalog, profile);
    let kart = additional_karts
        .iter()
        .find(|kart| kart.kart_id == grant.kart_id && kart.serial == grant.serial)
        .cloned()
        .unwrap_or_else(|| AdditionalKart {
            kart_id: grant.kart_id,
            serial: grant.serial,
            name: format!("카트 {}", grant.kart_id),
        });
    (kart, additional_karts)
}

fn grantable_kart_names(catalog: &CatalogInventory) -> BTreeMap<u16, String> {
    catalog
        .grant_items()
        .filter(|item| item.category == 3)
        .map(|item| {
            let name = item.name.trim();
            let name = if name.is_empty() {
                catalog.kart_name(item.id).unwrap_or("이름 없는 카트")
            } else {
                name
            };
            (item.id, name.to_owned())
        })
        .collect()
}

fn known_kart_names(catalog: &CatalogInventory) -> BTreeMap<u16, String> {
    catalog
        .category(3)
        .filter(|item| catalog.contains_kart(item.id))
        .map(|item| {
            let name = item.name.trim();
            let name = if name.is_empty() {
                catalog.kart_name(item.id).unwrap_or("이름 없는 카트")
            } else {
                name
            };
            (item.id, name.to_owned())
        })
        .collect()
}

fn normalize_search_text(value: &str) -> String {
    value
        .chars()
        .flat_map(char::to_lowercase)
        .filter(|character| character.is_alphanumeric())
        .collect()
}

#[cfg(test)]
mod tests {
    use std::{fs, io};

    use tempfile::tempdir;

    use crate::{CatalogInventory, EquipmentExceptions, Profile, ProfileStore};

    use super::{
        AddKartOutcome, FLOATER_333_CODES, KartGrantOptions, KartInventoryEditError,
        MAX_ADDITIONAL_KARTS_PER_PROFILE, add_kart, add_kart_during_race_run,
        add_kart_during_race_run_with_options, add_kart_to_profile, add_kart_with_options,
        additional_karts, search_karts,
    };

    fn catalog() -> CatalogInventory {
        CatalogInventory::from_structural_xml_for_tests(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names><Kart id="764" name="marathon13" /></Names>
                <Specs><Spec name="marathon13"><BodyParam DescEnchantCap="25" /></Spec></Specs>
                <Inventory total="5" categories="1">
                    <Item category="3" id="764" name="뉴 마라톤" />
                    <Item category="3" id="1395" name="세베크 V1" />
                    <Item category="3" id="1410" name="기간테스 V1" />
                    <Item category="3" id="1430" name="흑기사 V1" />
                    <Item category="3" id="1500" name="수동 확인 카트" autoGrant="false" />
                </Inventory>
            </KartCatalog>"#
                .as_bytes(),
        )
        .unwrap()
    }

    #[test]
    fn searches_korean_names_without_whitespace_and_accepts_decimal_ids() {
        let catalog = catalog();

        let by_name = search_karts(&catalog, "세베크v1", 10);
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].kart_id, 1_395);
        assert_eq!(by_name[0].name, "세베크 V1");

        let by_fragment = search_karts(&catalog, "v1", 2);
        assert_eq!(by_fragment.len(), 2);
        let by_id = search_karts(&catalog, "1410", 10);
        assert_eq!(by_id[0].name, "기간테스 V1");
    }

    #[test]
    fn quarantined_kart_is_hidden_by_name_but_can_be_added_by_exact_id() {
        let catalog = catalog();
        assert!(search_karts(&catalog, "수동 확인", 10).is_empty());

        let by_id = search_karts(&catalog, "1500", 10);
        assert_eq!(by_id.len(), 1);
        assert_eq!(by_id[0].kart_id, 1_500);
        assert_eq!(by_id[0].name, "수동 확인 카트");
        assert!(!by_id[0].auto_granted);

        let mut profile = Profile::default();
        let grant = add_kart_to_profile(
            &catalog,
            &EquipmentExceptions::default(),
            &mut profile,
            1_500,
        )
        .unwrap();
        assert_eq!(grant.serial, 2);
        assert_eq!(additional_karts(&catalog, &profile)[0].kart_id, 1_500);
        assert!(!catalog.grants_item(3, 1_500));
    }

    #[test]
    fn allocates_serials_per_kart_and_persists_each_nickname_independently() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();

        let first = add_kart(&store, &catalog, "Alice", 1_410).unwrap();
        let second = add_kart(&store, &catalog, "Alice", 1_410).unwrap();
        let other = add_kart(&store, &catalog, "Bob", 1_410).unwrap();
        assert!(matches!(first, AddKartOutcome::Durable { .. }));
        assert_eq!(first.kart().serial, 2);
        assert_eq!(second.kart().serial, 3);
        assert_eq!(other.kart().serial, 2);

        let alice = store.reload("alice").unwrap();
        assert_eq!(
            additional_karts(&catalog, &alice.profile)
                .iter()
                .map(|kart| kart.serial)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        let bob = store.reload("BOB").unwrap();
        assert_eq!(additional_karts(&catalog, &bob.profile)[0].serial, 2);
    }

    #[test]
    fn active_run_grant_uses_the_server_lease_and_supports_a_new_nickname() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        let lease = store.acquire_race_run_lease().unwrap();

        let first = add_kart_during_race_run(&store, &lease, &catalog, "LiveRider", 1_410).unwrap();
        let second =
            add_kart_during_race_run(&store, &lease, &catalog, "LiveRider", 1_410).unwrap();
        assert_eq!(first.kart().serial, 2);
        assert_eq!(second.kart().serial, 3);
        assert_eq!(
            additional_karts(&catalog, &store.reload("liverider").unwrap().profile)
                .iter()
                .map(|kart| kart.serial)
                .collect::<Vec<_>>(),
            vec![2, 3]
        );
        assert!(matches!(
            add_kart(&store, &catalog, "OfflineRacer", 1_410),
            Err(KartInventoryEditError::Store(
                crate::ProfileStoreError::RaceRunLeaseBusy { .. }
            ))
        ));
    }

    #[test]
    fn offline_grant_can_apply_exact_333_and_grade_five_sidecars() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        let outcome = add_kart_with_options(
            &store,
            &catalog,
            "EnhancedRider",
            764,
            KartGrantOptions {
                apply_floater: true,
                floater_codes: FLOATER_333_CODES,
                apply_grade_five: true,
            },
        )
        .unwrap();

        assert_eq!(outcome.kart().serial, 2);
        assert_eq!(
            outcome.enhancements().floater_codes,
            Some(FLOATER_333_CODES)
        );
        assert!(outcome.enhancements().grade_five);
        assert!(outcome.enhancements().durability_warnings.is_empty());
        let loaded = store.reload("enhancedrider").unwrap();
        let rider_directory = loaded.source_path.parent().unwrap();
        let equipment = EquipmentExceptions::load(root.path(), rider_directory).unwrap();
        let tune = equipment
            .tune
            .iter()
            .find(|record| record.id == 764 && record.serial == 2)
            .unwrap();
        assert_eq!([tune.tune1, tune.tune2, tune.tune3], FLOATER_333_CODES);
        let level = equipment
            .kart_level
            .iter()
            .find(|record| record.id == 764 && record.serial == 2)
            .unwrap();
        assert_eq!(level.grade, 5);
        assert_eq!(level.points, 35);
        assert_eq!(
            [level.level1, level.level2, level.level3, level.level4],
            [0; 4]
        );
    }

    #[test]
    fn offline_grant_accepts_custom_distinct_floater_slots_and_rejects_conflicts() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        let codes = [103, 202, 10_901];
        let outcome = add_kart_with_options(
            &store,
            &catalog,
            "CustomFloater",
            764,
            KartGrantOptions {
                apply_floater: true,
                floater_codes: codes,
                apply_grade_five: false,
            },
        )
        .unwrap();
        assert_eq!(outcome.enhancements().floater_codes, Some(codes));

        let loaded = store.reload("customfloater").unwrap();
        let equipment =
            EquipmentExceptions::load(root.path(), loaded.source_path.parent().unwrap()).unwrap();
        assert!(equipment.tune.iter().any(|record| {
            record.id == 764
                && record.serial == 2
                && [record.tune1, record.tune2, record.tune3] == codes
        }));

        assert!(matches!(
            add_kart_with_options(
                &store,
                &catalog,
                "InvalidFloater",
                764,
                KartGrantOptions {
                    apply_floater: true,
                    floater_codes: [601, 603, 0],
                    apply_grade_five: false,
                },
            ),
            Err(KartInventoryEditError::InvalidFloaterGrantCodes {
                codes: [601, 603, 0]
            })
        ));
        assert!(!store.profile_exists("InvalidFloater").unwrap());
    }

    #[test]
    fn live_grant_applies_enhancements_through_the_run_lease() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        let lease = store.acquire_race_run_lease().unwrap();
        let outcome = add_kart_during_race_run_with_options(
            &store,
            &lease,
            &catalog,
            "LiveEnhanced",
            764,
            KartGrantOptions {
                apply_floater: true,
                floater_codes: FLOATER_333_CODES,
                apply_grade_five: true,
            },
        )
        .unwrap();
        assert_eq!(outcome.kart().serial, 2);
        let equipment = store
            .load_equipment_exceptions(&lease, "LiveEnhanced")
            .unwrap();
        assert!(equipment.tune.iter().any(|record| {
            record.id == 764
                && record.serial == 2
                && [record.tune1, record.tune2, record.tune3] == FLOATER_333_CODES
        }));
        assert!(equipment.kart_level.iter().any(|record| {
            record.id == 764 && record.serial == 2 && record.grade == 5 && record.points == 35
        }));
    }

    #[test]
    fn rejects_enhancements_for_a_client_incompatible_kart_before_granting() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        assert!(matches!(
            add_kart_with_options(
                &store,
                &catalog,
                "V1Rider",
                1_410,
                KartGrantOptions {
                    apply_floater: true,
                    floater_codes: FLOATER_333_CODES,
                    apply_grade_five: false,
                },
            ),
            Err(KartInventoryEditError::LegacyEnhancementUnsupported { kart_id: 1_410 })
        ));
        assert!(!store.profile_exists("V1Rider").unwrap());
    }

    #[test]
    fn reserves_an_equipped_legacy_serial_and_rejects_unknown_or_full_profiles() {
        let catalog = catalog();
        let mut profile = Profile::default();
        let equipment = EquipmentExceptions::default();
        profile.rider_item.kart = 1_410;
        profile.rider_item.kart_serial = 2;
        let grant = add_kart_to_profile(&catalog, &equipment, &mut profile, 1_410).unwrap();
        assert_eq!(grant.serial, 3);

        assert!(matches!(
            add_kart_to_profile(&catalog, &equipment, &mut profile, 9_999),
            Err(KartInventoryEditError::KartNotGrantable { kart_id: 9_999 })
        ));
        profile.granted_karts = vec![grant; MAX_ADDITIONAL_KARTS_PER_PROFILE];
        assert!(matches!(
            add_kart_to_profile(&catalog, &equipment, &mut profile, 1_410),
            Err(KartInventoryEditError::TooManyAdditionalKarts { .. })
        ));
    }

    #[test]
    fn reports_a_published_grant_when_directory_durability_is_uncertain() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        store.fail_next_directory_sync(io::ErrorKind::Other);

        let outcome = add_kart(&store, &catalog, "Rider", 1_395).unwrap();
        assert!(matches!(
            outcome,
            AddKartOutcome::DurabilityUncertain { .. }
        ));
        assert_eq!(outcome.kart().serial, 2);
        assert_eq!(outcome.additional_karts(), [outcome.kart().clone()]);
        assert_eq!(
            additional_karts(&catalog, &store.reload("Rider").unwrap().profile),
            [outcome.kart().clone()]
        );
    }

    #[test]
    fn reserves_orphaned_tune_plant_level_and_parts_sidecar_serials() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        let loaded = store.load_or_create("Rider").unwrap();
        let rider_directory = loaded.source_path.parent().unwrap();
        fs::write(
            rider_directory.join("TuneData.json"),
            r#"[{"ID":1410,"SN":6}]"#,
        )
        .unwrap();
        fs::write(
            rider_directory.join("PlantData.json"),
            r#"[{"ID":1410,"SN":2}]"#,
        )
        .unwrap();
        fs::write(
            rider_directory.join("PartsData.json"),
            r#"[{"ID":1410,"SN":5}]"#,
        )
        .unwrap();
        fs::write(
            rider_directory.join("LevelData.json"),
            r#"[{"ID":1410,"SN":4,"Grade":5,"Points":35}]"#,
        )
        .unwrap();

        let first = add_kart(&store, &catalog, "Rider", 1_410).unwrap();
        let second = add_kart(&store, &catalog, "Rider", 1_410).unwrap();
        let third = add_kart(&store, &catalog, "Rider", 1_410).unwrap();
        assert_eq!(first.kart().serial, 3);
        assert_eq!(second.kart().serial, 7);
        assert_eq!(third.kart().serial, 8);
    }

    #[test]
    fn refuses_an_offline_edit_while_a_server_holds_the_profile_root_lease() {
        let root = tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        let catalog = catalog();
        let _server_lease = store.acquire_race_run_lease().unwrap();

        assert!(matches!(
            add_kart(&store, &catalog, "Rider", 1_410),
            Err(KartInventoryEditError::Store(
                crate::ProfileStoreError::RaceRunLeaseBusy { .. }
            ))
        ));
    }

    #[test]
    #[ignore = "requires P5136_KART_CATALOG pointing to the stock KartCatalog.xml"]
    fn stock_client_catalog_name_search_smoke() {
        let path = std::env::var_os("P5136_KART_CATALOG")
            .expect("P5136_KART_CATALOG must point to KartCatalog.xml");
        let catalog = CatalogInventory::load(path).unwrap();

        let gigantes = search_karts(&catalog, "기간테스v1", 10);
        assert_eq!(gigantes.len(), 1);
        assert_eq!(gigantes[0].kart_id, 1_410);
        let sebek = search_karts(&catalog, "1395", 10);
        assert_eq!(sebek[0].name, "세베크 V1");
    }
}
