use std::collections::BTreeMap;

use p5136_core::myroom_protocol::{MyRoomInfo, MyRoomProtocolError, validate_myroom_info};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ExtraFields = BTreeMap<String, Value>;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Profile {
    pub server_setting: ServerSettings,
    pub rider: Rider,
    pub rider_item: RiderItems,
    pub granted_karts: Vec<GrantedKart>,
    pub my_room: MyRoom,
    pub game_option: GameOptions,
    #[serde(rename = "P5136RustRiderSchool")]
    pub rider_school: RiderSchoolProgress,
    #[serde(
        default,
        rename = "P5136RustTimeAttackRecords",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub time_attack_records: BTreeMap<u32, u32>,
    #[serde(
        default,
        rename = "P5136RustFavoriteItems",
        skip_serializing_if = "Option::is_none"
    )]
    pub favorite_items: Option<crate::favorite_items::FavoriteItems>,
    #[serde(
        default,
        rename = "P5136RustLockedItems",
        skip_serializing_if = "Option::is_none"
    )]
    pub locked_items: Option<crate::favorite_items::LockedItems>,
    #[serde(
        default,
        rename = "P5136RustRaceRewardReceipt",
        skip_serializing_if = "Option::is_none"
    )]
    pub race_reward_receipt: Option<crate::progression::PersistedRaceRewardReceipt>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

/// Durable P5136 license-school progress.
///
/// The stock client exposes these as two independent fields in
/// `PrRiderSchoolDataPacket`: the current/unlocked license level and the
/// greatest completed school step. Legacy profiles predate this Rust field,
/// so their serde default deliberately preserves the historical all-clear
/// behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct RiderSchoolProgress {
    pub level: u8,
    pub max_completed_step: u8,
}

impl RiderSchoolProgress {
    /// Untouched stock progression: no license level and no completed test.
    pub const NONE_CLEARED: Self = Self {
        level: 0,
        max_completed_step: 0,
    };

    /// Compatibility progression used by all profiles before this field was
    /// persisted.
    pub const ALL_CLEAR: Self = Self {
        level: p5136_core::startup::P5136_RIDER_SCHOOL_LEVEL,
        max_completed_step: p5136_core::startup::P5136_RIDER_SCHOOL_MAX_STEP,
    };

    /// Completed the six Beginner tests.
    pub const BEGINNER: Self = Self {
        level: 1,
        max_completed_step: 6,
    };

    /// Completed the six Rookie tests.
    pub const ROOKIE: Self = Self {
        level: 2,
        max_completed_step: 12,
    };

    /// Completed the six L3 tests.
    pub const L3: Self = Self {
        level: 3,
        max_completed_step: 18,
    };

    /// Completed the six L2 tests.
    pub const L2: Self = Self {
        level: 4,
        max_completed_step: 24,
    };

    /// Completed the six L1 tests and unlocked the PRO challenge set.
    pub const L1: Self = Self {
        level: 5,
        max_completed_step: 30,
    };

    /// Grade-boundary values exposed by the operator account editor.
    pub const GRADE_BOUNDARIES: [Self; 7] = [
        Self::NONE_CLEARED,
        Self::BEGINNER,
        Self::ROOKIE,
        Self::L3,
        Self::L2,
        Self::L1,
        Self::ALL_CLEAR,
    ];

    #[must_use]
    pub const fn is_grade_boundary(self) -> bool {
        matches!(
            (self.level, self.max_completed_step),
            (0, 0) | (1, 6) | (2, 12) | (3, 18) | (4, 24) | (5, 30) | (6, 42)
        )
    }
}

impl Default for RiderSchoolProgress {
    fn default() -> Self {
        Self::ALL_CLEAR
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerSettings {
    #[serde(rename = "PreventItem_Use")]
    pub prevent_item_use: u8,
    #[serde(rename = "SpeedPatch_Use")]
    pub speed_patch_use: u8,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Rider {
    pub ban_type: u16,
    pub club_code: i32,
    #[serde(rename = "ClubMark_LOGO")]
    pub club_mark_logo: i32,
    #[serde(rename = "ClubMark_LINE")]
    pub club_mark_line: i32,
    pub club_name: String,
    pub club_intro: String,
    pub rider_intro: String,
    pub card: String,
    pub emblem1: i16,
    pub emblem2: i16,
    pub emblem3: i16,
    pub lucci: u32,
    #[serde(rename = "RP")]
    pub rp: u32,
    pub koin: u32,
    pub cash: u32,
    pub tc_cash: u32,
    pub premium: i32,
    pub ranker: u8,
    pub slot_changer: u16,
    #[serde(rename = "pmap")]
    pub pmap: u32,
    pub identification_type: u8,
    pub scenario_type: i32,
    pub speed_type: u8,
    pub game_type: u8,
    pub attack_type: u8,
    pub time: u32,
    pub track: u32,
    pub client_id: String,
    pub p2p_port: i32,
    pub udp_port: i32,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for Rider {
    fn default() -> Self {
        Self {
            ban_type: 0,
            club_code: 10_000,
            club_mark_logo: 0,
            club_mark_line: 0,
            club_name: "TCCstar".to_owned(),
            club_intro:
                "跑跑卡丁车交流群：84338611\n单机启动器下载地址：https://yanygm.github.io/Launcher_V2/"
                    .to_owned(),
            rider_intro: String::new(),
            card: String::new(),
            emblem1: 0,
            emblem2: 0,
            emblem3: 0,
            lucci: 1_000_000,
            rp: 20_000_000,
            koin: 10_000,
            cash: 10_000,
            tc_cash: 10_000,
            premium: 5,
            ranker: 0,
            slot_changer: i16::MAX as u16,
            pmap: 0,
            identification_type: 1,
            scenario_type: 0,
            speed_type: 0,
            game_type: 0,
            attack_type: 0,
            time: 0,
            track: 0,
            client_id: String::new(),
            p2p_port: 0,
            udp_port: 0,
            extra: ExtraFields::new(),
        }
    }
}

impl Rider {
    /// Replaces the three persisted main-emblem selections without disturbing
    /// forward-compatible rider fields.
    pub fn set_main_emblems(&mut self, emblem_1: i16, emblem_2: i16, emblem_3: i16) {
        self.emblem1 = emblem_1;
        self.emblem2 = emblem_2;
        self.emblem3 = emblem_3;
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct GrantedKart {
    pub kart_id: u16,
    pub serial: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RiderItems {
    #[serde(rename = "Set_Character")]
    pub character: u16,
    #[serde(rename = "Set_Paint")]
    pub paint: u16,
    #[serde(rename = "Set_Kart")]
    pub kart: u16,
    #[serde(rename = "Set_Plate")]
    pub plate: u16,
    #[serde(rename = "Set_Goggle")]
    pub goggle: u16,
    #[serde(rename = "Set_Balloon")]
    pub balloon: u16,
    #[serde(rename = "Set_Unknown1")]
    pub unknown1: u16,
    #[serde(rename = "Set_HeadBand")]
    pub head_band: u16,
    /// Legacy profile/wire name for the KR P5136 category-12 equipment slot.
    ///
    /// `KartCatalog.xml` identifies this category as replay-recording cameras,
    /// and the client race-state builder tests category 12 before retaining its
    /// `KartRecorder`. Keep the serialized C# field name for profile
    /// compatibility and use [`Self::replay_recording_camera_id`] in semantic
    /// code.
    #[serde(rename = "Set_HeadPhone")]
    pub head_phone: u16,
    #[serde(rename = "Set_HandGearL")]
    pub hand_gear_left: u16,
    #[serde(rename = "Set_Unknown2")]
    pub unknown2: u16,
    #[serde(rename = "Set_Uniform")]
    pub uniform: u16,
    #[serde(rename = "Set_Decal")]
    pub decal: u16,
    #[serde(rename = "Set_Pet")]
    pub pet: u16,
    #[serde(rename = "Set_FlyingPet")]
    pub flying_pet: u16,
    #[serde(rename = "Set_Aura")]
    pub aura: u16,
    #[serde(rename = "Set_SkidMark")]
    pub skid_mark: u16,
    #[serde(rename = "Set_SpecialKit")]
    pub special_kit: u16,
    #[serde(rename = "Set_RidColor")]
    pub rider_color: u16,
    #[serde(rename = "Set_BonusCard")]
    pub bonus_card: u16,
    #[serde(rename = "Set_BossModeCard")]
    pub boss_mode_card: u16,
    #[serde(rename = "Set_KartPlant1")]
    pub kart_plant1: u16,
    #[serde(rename = "Set_KartPlant2")]
    pub kart_plant2: u16,
    #[serde(rename = "Set_KartPlant3")]
    pub kart_plant3: u16,
    #[serde(rename = "Set_KartPlant4")]
    pub kart_plant4: u16,
    #[serde(rename = "Set_Unknown3")]
    pub unknown3: u16,
    #[serde(rename = "Set_FishingPole")]
    pub fishing_pole: u16,
    #[serde(rename = "Set_Tachometer")]
    pub tachometer: u16,
    #[serde(rename = "Set_Dye")]
    pub dye: u16,
    #[serde(rename = "Set_KartSN")]
    pub kart_serial: u16,
    #[serde(rename = "Set_Unknown4")]
    pub unknown4: u8,
    #[serde(rename = "Set_KartCoating")]
    pub kart_coating: u16,
    #[serde(rename = "Set_KartTailLamp")]
    pub kart_tail_lamp: u16,
    #[serde(rename = "Set_slotBg")]
    pub slot_background: u16,
    #[serde(rename = "Set_KartCoating12")]
    pub kart_coating12: u16,
    #[serde(rename = "Set_KartTailLamp12")]
    pub kart_tail_lamp12: u16,
    #[serde(rename = "Set_KartBoosterEffect12")]
    pub kart_booster_effect12: u16,
    #[serde(rename = "Set_Unknown5")]
    pub unknown5: u16,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl RiderItems {
    /// Returns the equipped KR P5136 category-12 replay-recording camera ID.
    #[must_use]
    pub const fn replay_recording_camera_id(&self) -> u16 {
        self.head_phone
    }

    /// Mirrors the client's `sub_8E0970(12) != 0` equipment gate.
    #[must_use]
    pub const fn has_replay_recording_camera(&self) -> bool {
        self.replay_recording_camera_id() != 0
    }
}

impl Default for RiderItems {
    fn default() -> Self {
        Self {
            character: 3,
            paint: 1,
            kart: 0,
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
            dye: 1,
            kart_serial: 0,
            unknown4: 0,
            kart_coating: 0,
            kart_tail_lamp: 0,
            slot_background: 0,
            kart_coating12: 0,
            kart_tail_lamp12: 0,
            kart_booster_effect12: 0,
            unknown5: 0,
            extra: ExtraFields::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct MyRoom {
    pub my_room: i16,
    #[serde(rename = "MyRoomBGM")]
    pub my_room_bgm: u8,
    pub use_room_pwd: u8,
    pub use_item_pwd: u8,
    pub talk_lock: u8,
    pub room_pwd: String,
    pub item_pwd: String,
    pub my_room_kart1: i16,
    pub my_room_kart2: i16,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for MyRoom {
    fn default() -> Self {
        Self {
            my_room: 0,
            my_room_bgm: 0,
            use_room_pwd: 0,
            use_item_pwd: 0,
            talk_lock: 1,
            room_pwd: String::new(),
            item_pwd: String::new(),
            my_room_kart1: 0,
            my_room_kart2: 0,
            extra: ExtraFields::new(),
        }
    }
}

impl MyRoom {
    /// Builds a wire-facing snapshot after applying the core protocol bounds.
    ///
    /// Persisted profiles can predate the Rust server and are therefore not
    /// assumed to contain wire-safe password lengths.
    pub fn try_to_protocol_info(&self) -> Result<MyRoomInfo, MyRoomProtocolError> {
        let info = MyRoomInfo {
            room_id: self.my_room,
            bgm: self.my_room_bgm,
            use_room_password: self.use_room_pwd,
            use_item_password: self.use_item_pwd,
            talk_lock: self.talk_lock,
            room_password: self.room_pwd.clone(),
            item_password: self.item_pwd.clone(),
            kart_1: self.my_room_kart1,
            kart_2: self.my_room_kart2,
        };
        validate_myroom_info(&info)?;
        Ok(info)
    }

    /// Applies a wire-facing snapshot only when all core protocol bounds pass.
    ///
    /// The validation happens before any assignment, so an error leaves this
    /// value unchanged. Flattened unknown profile fields are intentionally
    /// retained.
    pub fn try_apply_protocol_info(
        &mut self,
        info: &MyRoomInfo,
    ) -> Result<(), MyRoomProtocolError> {
        validate_myroom_info(info)?;
        self.my_room = info.room_id;
        self.my_room_bgm = info.bgm;
        self.use_room_pwd = info.use_room_password;
        self.use_item_pwd = info.use_item_password;
        self.talk_lock = info.talk_lock;
        self.room_pwd.clone_from(&info.room_password);
        self.item_pwd.clone_from(&info.item_password);
        self.my_room_kart1 = info.kart_1;
        self.my_room_kart2 = info.kart_2;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameOptions {
    #[serde(rename = "Set_BGM")]
    pub bgm_volume: f32,
    #[serde(rename = "Set_Sound")]
    pub sound_volume: f32,
    #[serde(rename = "Main_BGM")]
    pub main_bgm: u8,
    #[serde(rename = "Sound_effect")]
    pub sound_effect: u8,
    #[serde(rename = "Full_screen")]
    pub full_screen: u8,
    #[serde(rename = "ShowMirror")]
    pub show_mirror: u8,
    #[serde(rename = "ShowOtherPlayerNames")]
    pub show_other_player_names: u8,
    #[serde(rename = "ShowOutlines")]
    pub show_outlines: u8,
    #[serde(rename = "ShowShadows")]
    pub show_shadows: u8,
    #[serde(rename = "HighLevelEffect")]
    pub high_level_effect: u8,
    #[serde(rename = "MotionBlurEffect")]
    pub motion_blur_effect: u8,
    #[serde(rename = "MotionDistortionEffect")]
    pub motion_distortion_effect: u8,
    #[serde(rename = "HighEndOptimization")]
    pub high_end_optimization: u8,
    #[serde(rename = "AutoReady")]
    pub auto_ready: u8,
    #[serde(rename = "PropDescription")]
    pub prop_description: u8,
    #[serde(rename = "VideoQuality")]
    pub video_quality: u8,
    #[serde(rename = "BGM_Check")]
    pub bgm_check: u8,
    #[serde(rename = "Sound_Check")]
    pub sound_check: u8,
    #[serde(rename = "ShowHitInfo")]
    pub show_hit_info: u8,
    #[serde(rename = "AutoBoost")]
    pub auto_boost: u8,
    #[serde(rename = "GameType")]
    pub game_type: u8,
    #[serde(rename = "SetGhost")]
    pub set_ghost: u8,
    #[serde(rename = "SpeedType")]
    pub speed_type: u8,
    #[serde(rename = "RoomChat")]
    pub room_chat: u8,
    #[serde(rename = "DrivingChat")]
    pub driving_chat: u8,
    #[serde(rename = "ShowAllPlayerHitInfo")]
    pub show_all_player_hit_info: u8,
    #[serde(rename = "ShowTeamColor")]
    pub show_team_color: u8,
    #[serde(rename = "Set_screen")]
    pub screen: u8,
    #[serde(rename = "HideCompetitiveRank")]
    pub hide_competitive_rank: u8,
    #[serde(rename = "QuickMsg")]
    pub quick_messages: BTreeMap<i32, String>,
    #[serde(rename = "TeamQuickMsg")]
    pub team_quick_messages: BTreeMap<i32, String>,
    #[serde(flatten)]
    pub extra: ExtraFields,
}

impl Default for GameOptions {
    fn default() -> Self {
        Self {
            bgm_volume: 1.0,
            sound_volume: 1.0,
            main_bgm: 0,
            sound_effect: 1,
            full_screen: 1,
            show_mirror: 1,
            show_other_player_names: 1,
            show_outlines: 1,
            show_shadows: 1,
            high_level_effect: 0,
            motion_blur_effect: 0,
            motion_distortion_effect: 0,
            high_end_optimization: 1,
            auto_ready: 1,
            prop_description: 1,
            video_quality: 14,
            bgm_check: 1,
            sound_check: 1,
            show_hit_info: 1,
            auto_boost: 1,
            game_type: 0,
            set_ghost: 1,
            speed_type: 7,
            room_chat: 1,
            driving_chat: 1,
            show_all_player_hit_info: 1,
            show_team_color: 1,
            screen: 0,
            hide_competitive_rank: 0,
            quick_messages: BTreeMap::new(),
            team_quick_messages: BTreeMap::new(),
            extra: ExtraFields::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use p5136_core::myroom_protocol::{
        MAX_MYROOM_PASSWORD_UTF16_UNITS, MyRoomInfo, MyRoomProtocolError,
    };
    use serde_json::json;

    use super::{MyRoom, Profile, Rider, RiderSchoolProgress};

    #[test]
    fn defaults_match_the_p5136_csharp_profile() {
        let profile = Profile::default();
        assert_eq!(profile.rider.club_code, 10_000);
        assert_eq!(profile.rider.lucci, 1_000_000);
        assert_eq!(profile.rider.rp, 20_000_000);
        assert_eq!(profile.rider.premium, 5);
        assert_eq!(profile.rider_item.character, 3);
        assert_eq!(profile.rider_item.paint, 1);
        assert_eq!(profile.rider_item.dye, 1);
        assert_eq!(profile.game_option.video_quality, 14);
        assert_eq!(profile.game_option.speed_type, 7);
        assert_eq!(profile.rider_school, RiderSchoolProgress::ALL_CLEAR);
        assert!(profile.time_attack_records.is_empty());
        assert!(profile.favorite_items.is_none());
        assert_eq!(profile.race_reward_receipt, None);
    }

    #[test]
    fn legacy_profiles_default_to_all_clear_but_none_cleared_roundtrips() {
        let legacy: Profile = serde_json::from_value(json!({})).unwrap();
        assert_eq!(legacy.rider_school, RiderSchoolProgress::ALL_CLEAR);
        assert!(legacy.time_attack_records.is_empty());

        let fresh = Profile {
            rider_school: RiderSchoolProgress::NONE_CLEARED,
            ..Profile::default()
        };
        let encoded = serde_json::to_value(&fresh).unwrap();
        assert_eq!(encoded["P5136RustRiderSchool"]["Level"], 0);
        assert_eq!(encoded["P5136RustRiderSchool"]["MaxCompletedStep"], 0);
        assert_eq!(
            serde_json::from_value::<Profile>(encoded)
                .unwrap()
                .rider_school,
            RiderSchoolProgress::NONE_CLEARED
        );
    }

    #[test]
    fn time_attack_records_roundtrip_as_track_specific_personal_bests() {
        let mut profile = Profile::default();
        profile.time_attack_records.insert(0x1BAE_02BB, 71_234);
        profile.time_attack_records.insert(0x2C60_03A6, 130_987);

        let encoded = serde_json::to_value(&profile).unwrap();
        let decoded: Profile = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.time_attack_records, profile.time_attack_records);
    }

    #[test]
    fn rider_school_grade_boundaries_match_the_kr_client_catalog() {
        assert_eq!(RiderSchoolProgress::GRADE_BOUNDARIES.len(), 7);
        assert!(
            RiderSchoolProgress::GRADE_BOUNDARIES
                .into_iter()
                .all(RiderSchoolProgress::is_grade_boundary)
        );
        assert!(
            !RiderSchoolProgress {
                level: 5,
                max_completed_step: 29,
            }
            .is_grade_boundary()
        );
    }

    #[test]
    fn legacy_property_names_and_unknown_fields_survive_roundtrip() {
        let source = json!({
            "Rider": {
                "Lucci": 123,
                "RP": 456,
                "ClubMark_LOGO": 77,
                "futureRiderField": {"value": true}
            },
            "RiderItem": {
                "Set_Character": 42,
                "Set_HeadPhone": 5,
                "Set_slotBg": 8
            },
            "futureTopLevel": [1, 2, 3]
        });
        let profile: Profile = serde_json::from_value(source).unwrap();
        assert_eq!(profile.rider.lucci, 123);
        assert_eq!(profile.rider.rp, 456);
        assert_eq!(profile.rider.club_mark_logo, 77);
        assert_eq!(profile.rider_item.character, 42);
        assert_eq!(profile.rider_item.replay_recording_camera_id(), 5);
        assert!(profile.rider_item.has_replay_recording_camera());
        assert_eq!(profile.rider_item.slot_background, 8);
        assert_eq!(profile.race_reward_receipt, None);

        let encoded = serde_json::to_value(profile).unwrap();
        assert_eq!(encoded["Rider"]["futureRiderField"]["value"], true);
        assert_eq!(encoded["futureTopLevel"], json!([1, 2, 3]));
        assert_eq!(encoded["Rider"]["RP"], 456);
        assert_eq!(encoded["RiderItem"]["Set_Character"], 42);
        assert_eq!(encoded["RiderItem"]["Set_HeadPhone"], 5);
        assert!(encoded.get("P5136RustFavoriteItems").is_none());
        assert!(encoded.get("P5136RustRaceRewardReceipt").is_none());
    }

    #[test]
    fn favorite_items_roundtrip_without_disturbing_unknown_profile_fields() {
        let source = json!({
            "P5136RustFavoriteItems": [
                {"ItemCatID": 3, "ItemID": 1450, "ItemSN": 2},
                {"ItemCatID": 4, "ItemID": 300, "ItemSN": 7}
            ],
            "futureTopLevel": {"keep": true},
            "Rider": {
                "futureRiderField": [1, 2, 3]
            }
        });

        let profile: Profile = serde_json::from_value(source).unwrap();
        assert_eq!(profile.favorite_items.as_ref().unwrap().len(), 2);
        let encoded = serde_json::to_value(profile).unwrap();
        assert_eq!(encoded["futureTopLevel"]["keep"], true);
        assert_eq!(
            encoded["Rider"]["futureRiderField"],
            serde_json::json!([1, 2, 3])
        );
        assert_eq!(
            encoded["P5136RustFavoriteItems"],
            json!([
                {"ItemCatID": 3, "ItemID": 1450, "ItemSN": 2},
                {"ItemCatID": 4, "ItemID": 300, "ItemSN": 7}
            ])
        );
    }

    #[test]
    fn explicit_empty_favorite_items_are_not_conflated_with_a_missing_field() {
        let missing: Profile = serde_json::from_value(json!({})).unwrap();
        assert!(missing.favorite_items.is_none());
        assert!(
            serde_json::to_value(missing)
                .unwrap()
                .get("P5136RustFavoriteItems")
                .is_none()
        );

        let explicit_empty: Profile = serde_json::from_value(json!({
            "P5136RustFavoriteItems": []
        }))
        .unwrap();
        assert_eq!(
            explicit_empty
                .favorite_items
                .as_ref()
                .map(crate::favorite_items::FavoriteItems::is_empty),
            Some(true)
        );
        assert_eq!(
            serde_json::to_value(explicit_empty).unwrap()["P5136RustFavoriteItems"],
            json!([])
        );
    }

    #[test]
    fn first_generation_race_receipt_schema_remains_readable() {
        let source = json!({
            "P5136RustRaceRewardReceipt": {
                "Key": {
                    "RunId": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                    "RoomId": 7,
                    "RaceEpoch": 11,
                    "UserNo": 42
                },
                "Applied": {
                    "CurrentRp": 20_000_000,
                    "EarnedRp": 37,
                    "EarnedLucci": 25,
                    "CurrentLucci": 1_000_025
                }
            }
        });
        let profile: Profile = serde_json::from_value(source).unwrap();
        let receipt = profile.race_reward_receipt.as_ref().unwrap();
        assert_eq!(receipt.key.run_generation(), None);
        assert!(receipt.key.legacy_run_id().is_some());
        assert_eq!(receipt.key.canonical_nickname(), None);
        assert_eq!(receipt.applied.current_lucci, 1_000_025);

        let encoded = serde_json::to_value(profile).unwrap();
        let key = &encoded["P5136RustRaceRewardReceipt"]["Key"];
        assert!(key.get("RunId").is_some());
        assert!(key.get("RunGeneration").is_none());
    }

    #[test]
    fn myroom_protocol_conversion_validates_persisted_values() {
        let mut my_room = MyRoom {
            my_room: 17,
            my_room_bgm: 3,
            use_room_pwd: 1,
            use_item_pwd: 1,
            talk_lock: 0,
            room_pwd: "room".to_owned(),
            item_pwd: "item".to_owned(),
            my_room_kart1: 41,
            my_room_kart2: 42,
            ..MyRoom::default()
        };
        my_room.extra.insert("Future".to_owned(), json!({"x": 1}));

        assert_eq!(
            my_room.try_to_protocol_info().unwrap(),
            MyRoomInfo {
                room_id: 17,
                bgm: 3,
                use_room_password: 1,
                use_item_password: 1,
                talk_lock: 0,
                room_password: "room".to_owned(),
                item_password: "item".to_owned(),
                kart_1: 41,
                kart_2: 42,
            }
        );

        my_room.room_pwd = "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1);
        assert!(matches!(
            my_room.try_to_protocol_info(),
            Err(MyRoomProtocolError::StringTooLong {
                field: "MyRoom room password",
                ..
            })
        ));
    }

    #[test]
    fn myroom_protocol_apply_is_atomic_and_preserves_unknown_fields() {
        let mut my_room = MyRoom::default();
        my_room
            .extra
            .insert("FutureMyRoomField".to_owned(), json!([1, 2, 3]));
        let original = my_room.clone();
        let invalid = MyRoomInfo {
            room_id: 99,
            room_password: "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1),
            ..MyRoomInfo::default()
        };
        assert!(my_room.try_apply_protocol_info(&invalid).is_err());
        assert_eq!(my_room, original);

        let valid = MyRoomInfo {
            room_id: 99,
            bgm: 4,
            use_room_password: 1,
            use_item_password: 1,
            talk_lock: 0,
            room_password: "new-room".to_owned(),
            item_password: "new-item".to_owned(),
            kart_1: 7,
            kart_2: 8,
        };
        my_room.try_apply_protocol_info(&valid).unwrap();
        assert_eq!(my_room.try_to_protocol_info().unwrap(), valid);
        assert_eq!(my_room.extra["FutureMyRoomField"], json!([1, 2, 3]));
    }

    #[test]
    fn setting_main_emblems_preserves_unknown_rider_fields() {
        let mut rider = Rider::default();
        rider
            .extra
            .insert("FutureRiderField".to_owned(), json!({"keep": true}));

        rider.set_main_emblems(11, 12, 13);

        assert_eq!((rider.emblem1, rider.emblem2, rider.emblem3), (11, 12, 13));
        assert_eq!(rider.extra["FutureRiderField"]["keep"], true);
    }

    #[test]
    fn legacy_rider_without_third_emblem_defaults_only_that_slot() {
        let rider: Rider = serde_json::from_value(json!({
            "Emblem1": 7,
            "Emblem2": 8,
            "FutureRiderField": {"keep": true}
        }))
        .unwrap();

        assert_eq!((rider.emblem1, rider.emblem2, rider.emblem3), (7, 8, 0));
        assert_eq!(rider.extra["FutureRiderField"]["keep"], true);
    }
}
