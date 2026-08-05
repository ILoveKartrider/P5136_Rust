//! Cancellation-independent rider-equipment persistence.
//!
//! A World ticket is registered before the blocking profile transaction is
//! submitted. The accepted job retains both its canonical profile lane and a
//! pre-reserved completion capability, so dropping the requesting session
//! cannot leave durable equipment newer than the actor-owned room caches.

use std::{fmt, sync::Arc};

use p5136_core::{
    equipment_protocol::{EquipmentProtocolError, RiderItemSelection},
    startup::RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
};
use p5136_profile::{
    CatalogInventory, Profile, ProfileMutation, ProfileStore, ProfileStoreError, SavedProfile,
    apply_rider_item_selection, rider_item_snapshot,
};
use thiserror::Error;

use crate::{
    myroom_hub::MyRoomProfilePresentation,
    myroom_hub::{MyRoomCommitError, MyRoomHubError},
    myroom_persistence::{MyRoomCompletionSlot, MyRoomProfileCompletion, MyRoomProfileTicketId},
    profile_durability::{ExactDurabilityError, ExactProfileTransaction},
    profile_io::{
        ProfileIoCompletion, ProfileIoError, ProfileJobAdmission, myroom_profile_presentation,
    },
    world::MyRoomLifecycleError,
};

pub(crate) const RIDER_EQUIPMENT_WRITE_OPERATION: &str = "persist rider equipment";

#[cfg(test)]
pub(crate) type RiderEquipmentPersistenceTestHook = Arc<dyn Fn() + Send + Sync>;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiderEquipmentValidationError {
    #[error("rider item {item_id} in category {category} is not granted by the P5136 inventory")]
    RiderItemNotGranted { category: u16, item_id: u16 },

    #[error("kart {kart_id} serial {serial} is not granted by the P5136 inventory")]
    KartNotGranted { kart_id: u16, serial: u16 },
}

#[derive(Debug, Error)]
pub(crate) enum RiderEquipmentPersistError {
    #[error(transparent)]
    Store(Box<ProfileStoreError>),

    #[error(transparent)]
    Validation(#[from] RiderEquipmentValidationError),

    #[error(
        "rider-equipment durability confirmation changed immutable receipt from {expected:?} to {actual:?}"
    )]
    DurabilityReceiptChanged {
        expected: Box<SavedProfile>,
        actual: Option<Box<SavedProfile>>,
    },

    #[error(
        "rider-equipment profile revision {revision} remained durability-uncertain: initial commit: {initial}; confirmation: {confirmation}"
    )]
    DurabilityUnconfirmed {
        revision: u64,
        initial: Box<ProfileStoreError>,
        #[source]
        confirmation: Box<ProfileStoreError>,
    },

    #[error("the durable rider-equipment transaction did not resolve an immutable revision")]
    MissingDurableRevision,
}

impl From<ProfileStoreError> for RiderEquipmentPersistError {
    fn from(source: ProfileStoreError) -> Self {
        Self::Store(Box::new(source))
    }
}

impl ExactDurabilityError for RiderEquipmentPersistError {
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

#[derive(Debug, Error)]
pub(crate) enum RiderEquipmentWriteError {
    #[error("the World actor stopped before the rider-equipment write completed")]
    WorldStopped,

    #[error("the rider-equipment write used an identity operation minted by another World actor")]
    ForeignIdentityOperation,

    #[error("the profile-to-World completion mailbox is closed")]
    CompletionMailboxClosed,

    #[error("the World is quiescing and no longer accepts rider-equipment writes")]
    WorldQuiescing,

    #[error("session {session:?} is not authenticated for a rider-equipment write")]
    UnauthenticatedSession { session: crate::SessionId },

    #[error("profile admission for {admitted:?} does not match active rider identity {active:?}")]
    ProfileSubjectMismatch { admitted: String, active: String },

    #[error("identity {user_no:?} already has a pending rider-equipment write")]
    AlreadyPending { user_no: crate::UserNo },

    #[error("the registered rider-equipment write was abandoned before profile submission")]
    AbortedBeforeSubmission,

    #[error(transparent)]
    Persistence(#[from] RiderEquipmentPersistError),
}

#[derive(Debug, Error)]
pub(crate) enum RiderEquipmentPublicationInvariantError {
    #[error("rider-equipment profile ticket ID space is exhausted")]
    TicketIdExhausted,

    #[error("completion referenced unknown rider-equipment profile ticket {ticket}")]
    UnknownTicket { ticket: u64 },

    #[error("accepted rider-equipment profile ticket {ticket} lost its completion capability")]
    AcceptedOutcomeLost { ticket: u64 },

    #[error(
        "rider-equipment profile ticket {ticket} expected subject {expected:?}, completed as {actual:?}"
    )]
    CompletionSubjectMismatch {
        ticket: u64,
        expected: String,
        actual: String,
    },

    #[error("rider-equipment profile ticket {ticket} returned a different durable selection")]
    DurableValueMismatch { ticket: u64 },

    #[error(
        "rider-equipment pending-user index for {user_no:?} expected ticket {expected}, found {actual:?}"
    )]
    PendingIndexMismatch {
        user_no: crate::UserNo,
        expected: u64,
        actual: Option<u64>,
    },

    #[error("rider-equipment profile completion infrastructure failed for ticket {ticket}")]
    ProfileInfrastructure {
        ticket: u64,
        #[source]
        source: ProfileIoError,
    },

    #[error("rider-equipment MyRoom refresh failed for ticket {ticket}")]
    Hub {
        ticket: u64,
        #[source]
        source: MyRoomHubError,
    },

    #[error("rider-equipment MyRoom refresh commit failed for ticket {ticket}")]
    Commit {
        ticket: u64,
        #[source]
        source: MyRoomCommitError,
    },

    #[error("rider-equipment room packet serialization failed for ticket {ticket}")]
    Protocol {
        ticket: u64,
        #[source]
        source: EquipmentProtocolError,
    },

    #[error(
        "rider-equipment protocol membership diverged for ticket {ticket}, room {room_id}, user {user_no:?}"
    )]
    ProtocolMembership {
        ticket: u64,
        room_id: u32,
        user_no: crate::UserNo,
    },

    #[error("rider-equipment room delivery failed for ticket {ticket}")]
    Delivery {
        ticket: u64,
        #[source]
        source: Box<MyRoomLifecycleError>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RiderEquipmentPublication {
    ActiveCachesUpdated,
    OwnerlessCachesUpdated,
    PersistedAfterSupersession,
    PersistedAfterRelease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RiderEquipmentWriteReceipt {
    revision: u64,
    publication: RiderEquipmentPublication,
}

impl RiderEquipmentWriteReceipt {
    pub(crate) const fn revision(&self) -> u64 {
        self.revision
    }

    pub(crate) const fn publication(&self) -> RiderEquipmentPublication {
        self.publication
    }
}

#[derive(Debug)]
pub(crate) struct DurableRiderEquipment {
    selection: RiderItemSelection,
    snapshot: [u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH],
    presentation: MyRoomProfilePresentation,
    saved: SavedProfile,
}

impl DurableRiderEquipment {
    pub(crate) const fn selection(&self) -> RiderItemSelection {
        self.selection
    }

    pub(crate) const fn snapshot(&self) -> &[u8; RIDER_ITEM_SNAPSHOT_WIRE_LENGTH] {
        &self.snapshot
    }

    pub(crate) fn presentation(&self) -> &MyRoomProfilePresentation {
        &self.presentation
    }

    pub(crate) fn into_receipt(
        self,
        publication: RiderEquipmentPublication,
    ) -> RiderEquipmentWriteReceipt {
        RiderEquipmentWriteReceipt {
            revision: self.saved.revision,
            publication,
        }
    }
}

pub(crate) type RiderEquipmentProfileJobResult = Result<
    ProfileIoCompletion<Result<DurableRiderEquipment, RiderEquipmentPersistError>>,
    ProfileIoError,
>;

#[derive(Debug)]
pub(crate) enum RiderEquipmentProfileCompletion {
    AbortedBeforeSubmission {
        ticket: MyRoomProfileTicketId,
    },
    AcceptedOutcomeLost {
        ticket: MyRoomProfileTicketId,
    },
    Finished {
        ticket: MyRoomProfileTicketId,
        result: Box<RiderEquipmentProfileJobResult>,
    },
}

pub(crate) struct PreparedRiderEquipmentWrite {
    admission: ProfileJobAdmission,
    selection: RiderItemSelection,
    catalog: Arc<CatalogInventory>,
    completion: MyRoomCompletionSlot,
    #[cfg(test)]
    test_hook: Option<RiderEquipmentPersistenceTestHook>,
}

impl fmt::Debug for PreparedRiderEquipmentWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedRiderEquipmentWrite")
            .field("admission", &self.admission)
            .field("selection", &self.selection)
            .field("completion", &self.completion)
            .finish_non_exhaustive()
    }
}

impl PreparedRiderEquipmentWrite {
    pub(crate) fn new(
        admission: ProfileJobAdmission,
        selection: RiderItemSelection,
        catalog: Arc<CatalogInventory>,
        completion: MyRoomCompletionSlot,
    ) -> Self {
        Self {
            admission,
            selection: normalize_selection(selection),
            catalog,
            completion,
            #[cfg(test)]
            test_hook: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_test_hook(mut self, test_hook: RiderEquipmentPersistenceTestHook) -> Self {
        self.test_hook = Some(test_hook);
        self
    }

    pub(crate) fn admitted_nickname(&self) -> &str {
        self.admission.subject().nickname()
    }

    pub(crate) const fn selection(&self) -> RiderItemSelection {
        self.selection
    }

    pub(crate) fn register(self, ticket: MyRoomProfileTicketId) -> RegisteredRiderEquipmentWrite {
        RegisteredRiderEquipmentWrite {
            admission: self.admission,
            selection: self.selection,
            catalog: self.catalog,
            abort: AbortBeforeSubmission::new(ticket, self.completion),
            #[cfg(test)]
            test_hook: self.test_hook,
        }
    }
}

#[must_use = "a registered rider-equipment write must be submitted or explicitly aborted by drop"]
pub(crate) struct RegisteredRiderEquipmentWrite {
    admission: ProfileJobAdmission,
    selection: RiderItemSelection,
    catalog: Arc<CatalogInventory>,
    abort: AbortBeforeSubmission,
    #[cfg(test)]
    test_hook: Option<RiderEquipmentPersistenceTestHook>,
}

impl fmt::Debug for RegisteredRiderEquipmentWrite {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RegisteredRiderEquipmentWrite")
            .field("ticket", &self.abort.ticket)
            .field("admission", &self.admission)
            .field("armed", &self.abort.completion.is_some())
            .finish_non_exhaustive()
    }
}

impl RegisteredRiderEquipmentWrite {
    pub(crate) fn submit(self) {
        let Self {
            admission,
            selection,
            catalog,
            abort,
            #[cfg(test)]
            test_hook,
        } = self;
        let (ticket, completion) = abort.disarm();
        let accepted = AcceptedCompletionGuard {
            ticket,
            completion: Some(completion),
        };
        admission.submit_with_completion(
            RIDER_EQUIPMENT_WRITE_OPERATION,
            move |store, _, subject| {
                #[cfg(test)]
                if let Some(test_hook) = test_hook {
                    test_hook();
                }
                persist_rider_equipment(store, &catalog, subject.nickname(), selection)
            },
            move |result| accepted.finish(result),
        );
    }
}

struct AbortBeforeSubmission {
    ticket: MyRoomProfileTicketId,
    completion: Option<MyRoomCompletionSlot>,
}

impl AbortBeforeSubmission {
    fn new(ticket: MyRoomProfileTicketId, completion: MyRoomCompletionSlot) -> Self {
        Self {
            ticket,
            completion: Some(completion),
        }
    }

    fn disarm(mut self) -> (MyRoomProfileTicketId, MyRoomCompletionSlot) {
        let completion = self
            .completion
            .take()
            .expect("the rider-equipment abort guard is disarmed exactly once");
        (self.ticket, completion)
    }
}

impl Drop for AbortBeforeSubmission {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::RiderEquipment(
                RiderEquipmentProfileCompletion::AbortedBeforeSubmission {
                    ticket: self.ticket,
                },
            ));
        }
    }
}

struct AcceptedCompletionGuard {
    ticket: MyRoomProfileTicketId,
    completion: Option<MyRoomCompletionSlot>,
}

impl AcceptedCompletionGuard {
    fn finish(mut self, result: RiderEquipmentProfileJobResult) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::RiderEquipment(
                RiderEquipmentProfileCompletion::Finished {
                    ticket: self.ticket,
                    result: Box::new(result),
                },
            ));
        }
    }
}

impl Drop for AcceptedCompletionGuard {
    fn drop(&mut self) {
        if let Some(completion) = self.completion.take() {
            completion.send(MyRoomProfileCompletion::RiderEquipment(
                RiderEquipmentProfileCompletion::AcceptedOutcomeLost {
                    ticket: self.ticket,
                },
            ));
        }
    }
}

fn persist_rider_equipment(
    store: &ProfileStore,
    catalog: &CatalogInventory,
    nickname: &str,
    selection: RiderItemSelection,
) -> Result<DurableRiderEquipment, RiderEquipmentPersistError> {
    let selection = normalize_selection(selection);
    let transaction = store.transaction(nickname, |profile| {
        if let Err(error) = validate_rider_item_selection(catalog, profile, selection) {
            return ProfileMutation::Unchanged(Err(error));
        }
        let mut next = profile.clone();
        apply_rider_item_selection(&mut next.rider_item, selection);
        ProfileMutation::changed(Ok(()), next)
    })?;

    let (validation, profile, durability) = ExactProfileTransaction::from(transaction).into_parts();
    validation?;

    let saved = durability
        .confirm_exact::<RiderEquipmentPersistError>(store, nickname)?
        .ok_or(RiderEquipmentPersistError::MissingDurableRevision)?;
    let snapshot = rider_item_snapshot(&profile.rider_item);
    let presentation = myroom_profile_presentation(&profile);
    Ok(DurableRiderEquipment {
        selection,
        snapshot,
        presentation,
        saved,
    })
}

pub(crate) fn catalog_grants(catalog: &CatalogInventory, category: u16, item_id: u16) -> bool {
    catalog.grants_item(category, item_id)
}

pub(crate) fn kart_is_owned(
    catalog: &CatalogInventory,
    profile: &Profile,
    kart_id: u16,
    serial: u16,
) -> bool {
    if serial == 1 {
        return catalog_grants(catalog, 3, kart_id);
    }
    catalog.contains_kart(kart_id)
        && profile
            .granted_karts
            .iter()
            .any(|kart| kart.kart_id == kart_id && kart.serial == serial)
}

const fn normalized_kart_serial(kart_id: u16, serial: u16) -> u16 {
    if kart_id != 0 && serial == 0 {
        1
    } else {
        serial
    }
}

fn normalize_selection(mut selection: RiderItemSelection) -> RiderItemSelection {
    selection.kart_serial = normalized_kart_serial(selection.kart, selection.kart_serial);
    selection
}

pub(crate) fn validate_rider_item_selection(
    catalog: &CatalogInventory,
    profile: &Profile,
    selection: RiderItemSelection,
) -> Result<(), RiderEquipmentValidationError> {
    let current = &profile.rider_item;
    let selected_items = [
        (1, current.character, selection.character),
        (2, current.paint, selection.paint),
        (4, current.plate, selection.plate),
        (8, current.goggle, selection.goggle),
        (9, current.balloon, selection.balloon),
        (11, current.head_band, selection.head_band),
        (12, current.head_phone, selection.head_phone),
        (16, current.hand_gear_left, selection.hand_gear_left),
        (18, current.uniform, selection.uniform),
        (20, current.decal, selection.decal),
        (21, current.pet, selection.pet),
        (52, current.flying_pet, selection.flying_pet),
        (26, current.aura, selection.aura),
        (27, current.skid_mark, selection.skid_mark),
        (30, current.special_kit, selection.special_kit),
        (31, current.rider_color, selection.rider_color),
        (32, current.bonus_card, selection.bonus_card),
        (36, current.boss_mode_card, selection.boss_mode_card),
        (43, current.kart_plant1, selection.kart_plant1),
        (44, current.kart_plant2, selection.kart_plant2),
        (45, current.kart_plant3, selection.kart_plant3),
        (46, current.kart_plant4, selection.kart_plant4),
        (59, current.fishing_pole, selection.fishing_pole),
        (61, current.tachometer, selection.tachometer),
        (70, current.dye, selection.dye),
        (68, current.kart_coating, selection.kart_coating),
        (69, current.kart_tail_lamp, selection.kart_tail_lamp),
    ];
    for (category, previous, item_id) in selected_items {
        if item_id != 0 && item_id != previous && !catalog_grants(catalog, category, item_id) {
            return Err(RiderEquipmentValidationError::RiderItemNotGranted { category, item_id });
        }
    }

    let serial = normalized_kart_serial(selection.kart, selection.kart_serial);
    if selection.kart != 0 && !kart_is_owned(catalog, profile, selection.kart, serial) {
        return Err(RiderEquipmentValidationError::KartNotGranted {
            kart_id: selection.kart,
            serial,
        });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::fmt::Write as _;

    use p5136_core::equipment_protocol::RiderItemSelection;
    use p5136_profile::{CatalogInventory, GrantedKart, Profile, ProfileStore};

    use super::{
        RiderEquipmentPersistError, RiderEquipmentValidationError, persist_rider_equipment,
        validate_rider_item_selection,
    };

    pub(crate) fn selection() -> RiderItemSelection {
        RiderItemSelection {
            character: 1_000,
            paint: 0,
            kart: 1,
            plate: 0,
            goggle: 0,
            balloon: 0,
            unknown1: 0,
            head_band: 0,
            head_phone: 0,
            hand_gear_left: 0,
            unknown2: 0,
            uniform: 0,
            decal: 0,
            pet: 0,
            flying_pet: 0,
            aura: 0,
            skid_mark: 0,
            special_kit: 0,
            rider_color: 0,
            bonus_card: 0,
            boss_mode_card: 0,
            kart_plant1: 0,
            kart_plant2: 0,
            kart_plant3: 0,
            kart_plant4: 0,
            unknown3: 0,
            fishing_pole: 0,
            tachometer: 0,
            dye: 0,
            kart_serial: 0,
            unknown4: 0,
            kart_coating: 0,
            kart_tail_lamp: 0,
        }
    }

    pub(crate) fn catalog() -> CatalogInventory {
        const GRANT_CATEGORIES: &[u16] = &[
            1, 2, 3, 4, 7, 8, 9, 11, 12, 13, 14, 16, 18, 20, 21, 22, 23, 26, 27, 28, 30, 31, 32,
            36, 37, 38, 39, 43, 44, 45, 46, 49, 52, 53, 55, 59, 61, 67, 68, 69, 70,
        ];
        const NON_GRANT_CATEGORIES: &[u16] = &[
            5, 6, 10, 15, 17, 19, 24, 25, 29, 33, 34, 35, 40, 41, 42, 47, 48, 50, 51,
        ];
        let mut items = String::new();
        let mut item_count = 0;
        for &category in GRANT_CATEGORIES {
            let ids: Box<dyn Iterator<Item = u16>> = if category == 3 {
                Box::new((1..=1_198).chain([1_450, 1_453]))
            } else {
                Box::new(1_000..1_110)
            };
            for id in ids {
                writeln!(
                    items,
                    r#"<Item category="{category}" id="{id}" name="test" />"#
                )
                .unwrap();
                item_count += 1;
            }
        }
        for (index, &category) in NON_GRANT_CATEGORIES.iter().enumerate() {
            let count = 63 + usize::from(index < 3);
            for id in 1..=count {
                writeln!(
                    items,
                    r#"<Item category="{category}" id="{id}" name="test" />"#
                )
                .unwrap();
                item_count += 1;
            }
        }
        assert_eq!(item_count, 6_800);
        let xml = format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names>
                    <Kart id="1450" name="testKnownKart" />
                    <Kart id="1453" name="testMissingKartSpec" />
                </Names>
                <Specs>
                    <Spec name="testKnownKart">
                        <BodyParam ForwardAccelForce="147" DragFactor="-0.05" />
                    </Spec>
                </Specs>
                <Inventory total="{item_count}" categories="60">{items}</Inventory>
                <Emblems total="3">
                    <Emblem id="7" />
                    <Emblem id="8" />
                    <Emblem id="9" />
                </Emblems>
                <Abilities total="12" resolved="12">
                    <TransformByKart>
                        <Rule kartId="1410" sourceId="5" targetId="103" probability="100" gitType="no_flag" />
                        <Rule kartId="1410" sourceId="7" targetId="99" probability="100" gitType="no_flag" />
                        <Rule kartId="1410" sourceId="127" targetId="99" probability="100" gitType="no_flag" />
                        <Rule kartId="1411" sourceId="7" targetId="99" probability="50" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="3" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="4" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="5" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="6" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="7" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="9" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="12" targetId="36" probability="25" gitType="no_flag" />
                        <Rule kartId="1395" sourceId="13" targetId="36" probability="25" gitType="no_flag" />
                    </TransformByKart>
                </Abilities>
            </KartCatalog>"#
        );
        CatalogInventory::from_xml(xml.as_bytes()).unwrap()
    }

    #[test]
    fn validation_preserves_existing_legacy_values_and_checks_new_grants() {
        let catalog = catalog();
        let mut profile = Profile::default();
        let selection = selection();
        validate_rider_item_selection(&catalog, &profile, selection).unwrap();

        let mut invalid = selection;
        invalid.character = 999;
        assert_eq!(
            validate_rider_item_selection(&catalog, &profile, invalid),
            Err(RiderEquipmentValidationError::RiderItemNotGranted {
                category: 1,
                item_id: 999,
            })
        );

        profile.rider_item.character = 999;
        validate_rider_item_selection(&catalog, &profile, invalid).unwrap();

        profile.rider_item.kart = 1_453;
        profile.rider_item.kart_serial = 1;
        invalid.kart = 1_453;
        invalid.kart_serial = 1;
        assert_eq!(
            validate_rider_item_selection(&catalog, &profile, invalid),
            Err(RiderEquipmentValidationError::KartNotGranted {
                kart_id: 1_453,
                serial: 1,
            })
        );
        profile.rider_item.kart = 0;
        profile.rider_item.kart_serial = 0;

        profile.granted_karts.push(GrantedKart {
            kart_id: 1_453,
            serial: 2,
        });
        let mut manual_quarantine_override = selection;
        manual_quarantine_override.kart = 1_453;
        manual_quarantine_override.kart_serial = 2;
        validate_rider_item_selection(&catalog, &profile, manual_quarantine_override).unwrap();

        profile.granted_karts.push(GrantedKart {
            kart_id: 1,
            serial: 2,
        });
        let mut duplicate_kart = selection;
        duplicate_kart.kart_serial = 2;
        validate_rider_item_selection(&catalog, &profile, duplicate_kart).unwrap();
        duplicate_kart.kart_serial = 3;
        assert_eq!(
            validate_rider_item_selection(&catalog, &profile, duplicate_kart),
            Err(RiderEquipmentValidationError::KartNotGranted {
                kart_id: 1,
                serial: 3,
            })
        );
    }

    #[test]
    fn durable_write_normalizes_default_kart_serial_and_rejects_without_revision() {
        let root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(root.path());
        store.save("Rider", &Profile::default()).unwrap();
        let durable = persist_rider_equipment(&store, &catalog(), "Rider", selection()).unwrap();
        assert_eq!(durable.selection.kart_serial, 1);
        assert_eq!(durable.snapshot[58..60], 1_u16.to_le_bytes());

        let mut invalid = selection();
        invalid.character = 999;
        assert!(matches!(
            persist_rider_equipment(&store, &catalog(), "Rider", invalid),
            Err(RiderEquipmentPersistError::Validation(
                RiderEquipmentValidationError::RiderItemNotGranted {
                    category: 1,
                    item_id: 999,
                }
            ))
        ));
        assert_eq!(
            store.load_or_create("Rider").unwrap().revision,
            Some(durable.saved.revision)
        );
    }
}
