use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::Arc,
};

use p5136_core::{
    bml::{BmlError, BmlLimits, BmlNode},
    packet::PacketReader,
    track::is_random_track_selector,
};
use p5136_rho5::{LegacyRhoArchive, LegacyRhoError, LegacyRhoLimits};
use thiserror::Error;

const RANDOM_TRACK_BML: &str = "randomTrack@kr.bml";
const TRACK_BML: &str = "track@zz.bml";
const TRACK_LOCALE_BML: &str = "trackLocale@kr.bml";
const MAX_POOLS: usize = 32;
const MAX_TRACKS_PER_POOL: usize = 512;
const SUPPORTED_SELECTORS: [u32; 12] = [0, 1, 3, 4, 5, 6, 7, 8, 23, 30, 33, 40];

#[derive(Debug, Default)]
struct RandomPoolSources {
    sets: HashMap<(String, String), Vec<String>>,
    lists: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomTrackDefinition {
    pub id: String,
    pub korean_name: String,
    pub game_type: String,
    pub basic_ai: bool,
    pub hash: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomTrackPool {
    pub game_type: u8,
    pub selector: u32,
    pub korean_name: String,
    pub default_track_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RandomTrackPoolOverride {
    pub game_type: u8,
    pub selector: u32,
    pub track_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RandomTrackConfiguration {
    pub pools: Vec<RandomTrackPoolOverride>,
}

#[derive(Debug, Clone)]
pub struct RandomTrackCatalog {
    source_path: PathBuf,
    tracks: Vec<RandomTrackDefinition>,
    pools: Vec<RandomTrackPool>,
    tracks_by_id: HashMap<String, usize>,
    pools_by_key: HashMap<(u8, u32), usize>,
}

impl RandomTrackCatalog {
    #[must_use]
    pub fn source_path(&self) -> &Path {
        &self.source_path
    }

    #[must_use]
    pub fn tracks(&self) -> &[RandomTrackDefinition] {
        &self.tracks
    }

    #[must_use]
    pub fn pools(&self) -> &[RandomTrackPool] {
        &self.pools
    }

    #[must_use]
    pub fn compatible_tracks(&self, pool: &RandomTrackPool) -> Vec<&RandomTrackDefinition> {
        self.tracks
            .iter()
            .filter(|track| is_compatible_track(pool, track))
            .collect()
    }

    pub fn resolve(
        &self,
        configuration: &RandomTrackConfiguration,
    ) -> Result<ResolvedRandomTracks, RandomTrackError> {
        validate_configuration(configuration)?;
        let overrides = configuration
            .pools
            .iter()
            .map(|pool| ((pool.game_type, pool.selector), pool))
            .collect::<HashMap<_, _>>();
        let mut pools = HashMap::with_capacity(self.pools.len());
        for pool in &self.pools {
            let configured = overrides.get(&(pool.game_type, pool.selector));
            let ids = configured.map_or(pool.default_track_ids.as_slice(), |configured| {
                configured.track_ids.as_slice()
            });
            let mut hashes = Vec::with_capacity(ids.len());
            for id in ids {
                let Some(index) = self.tracks_by_id.get(&id.to_ascii_lowercase()) else {
                    return Err(RandomTrackError::UnknownOverrideTrack { id: id.clone() });
                };
                let track = &self.tracks[*index];
                if configured.is_some() && !is_compatible_track(pool, track) {
                    return Err(RandomTrackError::IncompatibleOverrideTrack {
                        pool: pool.korean_name.clone(),
                        id: id.clone(),
                    });
                }
                if !hashes.contains(&track.hash) {
                    hashes.push(track.hash);
                }
            }
            if hashes.is_empty() {
                return Err(RandomTrackError::EmptyResolvedPool {
                    game_type: pool.game_type,
                    selector: pool.selector,
                });
            }
            let mut basic_ai_hashes = ids
                .iter()
                .filter_map(|id| self.tracks_by_id.get(&id.to_ascii_lowercase()))
                .map(|index| &self.tracks[*index])
                .filter(|track| track.basic_ai)
                .map(|track| track.hash)
                .collect::<Vec<_>>();
            basic_ai_hashes.sort_unstable();
            basic_ai_hashes.dedup();
            pools.insert(
                (pool.game_type, pool.selector),
                ResolvedRandomTrackPool {
                    hashes: hashes.into(),
                    basic_ai_hashes: basic_ai_hashes.into(),
                },
            );
        }
        for configured in &configuration.pools {
            if !self
                .pools_by_key
                .contains_key(&(configured.game_type, configured.selector))
            {
                return Err(RandomTrackError::UnknownOverridePool {
                    game_type: configured.game_type,
                    selector: configured.selector,
                });
            }
        }
        Ok(ResolvedRandomTracks { pools })
    }
}

#[derive(Debug, Clone)]
struct ResolvedRandomTrackPool {
    hashes: Arc<[u32]>,
    basic_ai_hashes: Arc<[u32]>,
}

#[derive(Debug, Clone, Default)]
pub struct ResolvedRandomTracks {
    pools: HashMap<(u8, u32), ResolvedRandomTrackPool>,
}

impl ResolvedRandomTracks {
    #[must_use]
    pub fn candidates(&self, room_game_type: u8, selector: u32, basic_ai_only: bool) -> &[u32] {
        let catalog_game_type = u8::from(matches!(room_game_type, 2 | 4));
        let Some(pool) = self.pools.get(&(catalog_game_type, selector)) else {
            return &[];
        };
        if basic_ai_only && !pool.basic_ai_hashes.is_empty() {
            &pool.basic_ai_hashes
        } else {
            &pool.hashes
        }
    }
}

#[derive(Debug, Error)]
pub enum RandomTrackError {
    #[error(transparent)]
    Archive(#[from] LegacyRhoError),
    #[error(transparent)]
    Bml(#[from] BmlError),
    #[error("random-track BML contains trailing bytes")]
    BmlTrailingBytes,
    #[error("random-track catalog is incomplete: tracks={tracks}, pools={pools}")]
    IncompleteCatalog { tracks: usize, pools: usize },
    #[error("random-track configuration has more than {maximum} pool overrides")]
    TooManyOverrides { maximum: usize },
    #[error(
        "random-track override has an unsupported pool: game_type={game_type}, selector={selector}"
    )]
    InvalidOverridePool { game_type: u8, selector: u32 },
    #[error("random-track override pool is duplicated: game_type={game_type}, selector={selector}")]
    DuplicateOverridePool { game_type: u8, selector: u32 },
    #[error("random-track override pool must contain 1..={maximum} tracks")]
    InvalidOverrideTrackCount { maximum: usize },
    #[error("random-track override contains an invalid or duplicated track ID: {id:?}")]
    InvalidOverrideTrackId { id: String },
    #[error(
        "random-track override references an unavailable pool: game_type={game_type}, selector={selector}"
    )]
    UnknownOverridePool { game_type: u8, selector: u32 },
    #[error("random-track override references an unknown track: {id:?}")]
    UnknownOverrideTrack { id: String },
    #[error("random-track override {pool:?} cannot contain track {id:?}")]
    IncompatibleOverrideTrack { pool: String, id: String },
    #[error("random-track pool resolved empty: game_type={game_type}, selector={selector}")]
    EmptyResolvedPool { game_type: u8, selector: u32 },
}

pub fn load_client_random_track_catalog(
    data_directory: impl AsRef<Path>,
) -> Result<RandomTrackCatalog, RandomTrackError> {
    let source_path = data_directory.as_ref().join("track_common.rho");
    let archive = LegacyRhoArchive::open(&source_path, LegacyRhoLimits::default())?;
    let random_root = decode_bml(&archive.extract_exact(RANDOM_TRACK_BML)?)?;
    let track_root = decode_bml(&archive.extract_exact(TRACK_BML)?)?;
    let locale_root = decode_bml(&archive.extract_exact(TRACK_LOCALE_BML)?)?;
    let tracks = build_tracks(&track_root, &locale_root);
    let pools = build_pools(&random_root, &tracks);
    if tracks.len() < 200 || pools.len() < 15 {
        return Err(RandomTrackError::IncompleteCatalog {
            tracks: tracks.len(),
            pools: pools.len(),
        });
    }
    let tracks_by_id = tracks
        .iter()
        .enumerate()
        .map(|(index, track)| (track.id.to_ascii_lowercase(), index))
        .collect();
    let pools_by_key = pools
        .iter()
        .enumerate()
        .map(|(index, pool)| ((pool.game_type, pool.selector), index))
        .collect();
    Ok(RandomTrackCatalog {
        source_path,
        tracks,
        pools,
        tracks_by_id,
        pools_by_key,
    })
}

fn decode_bml(bytes: &[u8]) -> Result<BmlNode, RandomTrackError> {
    let limits = BmlLimits {
        max_depth: 6,
        max_nodes: 16_384,
        max_attributes_per_node: 32,
        max_children_per_node: 8_192,
        max_string_code_units: 512,
    };
    let mut reader = PacketReader::new(bytes);
    let root = BmlNode::decode_with_limits(&mut reader, limits)?;
    if !reader.remaining().is_empty() {
        return Err(RandomTrackError::BmlTrailingBytes);
    }
    Ok(root)
}

#[derive(Debug, Clone)]
struct MutableTrack {
    id: String,
    name: String,
    game_type: String,
    basic_ai: bool,
    blocked: bool,
}

fn build_tracks(track_root: &BmlNode, locale_root: &BmlNode) -> Vec<RandomTrackDefinition> {
    let mut tracks = HashMap::<String, MutableTrack>::new();
    for child in track_root
        .children
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case("track"))
    {
        let id = attribute(child, "id");
        if id.is_empty() || id.to_ascii_lowercase().contains("_s") {
            continue;
        }
        tracks.insert(
            id.to_ascii_lowercase(),
            MutableTrack {
                id: id.to_owned(),
                name: String::new(),
                game_type: attribute(child, "gameType").to_owned(),
                basic_ai: attribute_is_true(child, "basicAi"),
                blocked: false,
            },
        );
    }
    add_technical_variants(&mut tracks, track_root, "track_crz", "crz");
    add_technical_variants(&mut tracks, track_root, "track_rvs", "rvs");
    for child in locale_root
        .children
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case("track"))
    {
        let key = attribute(child, "id").to_ascii_lowercase();
        if let Some(track) = tracks.get_mut(&key) {
            attribute(child, "name").clone_into(&mut track.name);
            track.blocked = attribute_is_true(child, "blocked");
            if has_attribute(child, "basicAi") {
                track.basic_ai = attribute_is_true(child, "basicAi");
            }
        }
    }
    add_locale_variants(&mut tracks, locale_root, "track_crz", "crz");
    add_locale_variants(&mut tracks, locale_root, "track_rvs", "rvs");
    let mut definitions = tracks
        .into_values()
        .filter(|track| !track.blocked && !track.name.is_empty() && !track.game_type.is_empty())
        .map(|track| RandomTrackDefinition {
            hash: unicode_adler(&track.id),
            id: track.id,
            korean_name: track.name,
            game_type: track.game_type,
            basic_ai: track.basic_ai,
        })
        .collect::<Vec<_>>();
    definitions.sort_by(|left, right| left.korean_name.cmp(&right.korean_name));
    definitions
}

fn add_technical_variants(
    tracks: &mut HashMap<String, MutableTrack>,
    root: &BmlNode,
    element: &str,
    suffix: &str,
) {
    for child in root
        .children
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case(element))
    {
        let reference = attribute(child, "refId").to_ascii_lowercase();
        let Some(base) = tracks.get(&reference).cloned() else {
            continue;
        };
        let id = format!("{}_{}", base.id, suffix);
        tracks.insert(
            id.to_ascii_lowercase(),
            MutableTrack {
                id,
                name: String::new(),
                game_type: base.game_type,
                basic_ai: if has_attribute(child, "basicAi") {
                    attribute_is_true(child, "basicAi")
                } else {
                    base.basic_ai
                },
                blocked: false,
            },
        );
    }
}

fn add_locale_variants(
    tracks: &mut HashMap<String, MutableTrack>,
    root: &BmlNode,
    element: &str,
    suffix: &str,
) {
    for child in root
        .children
        .iter()
        .filter(|node| node.name.eq_ignore_ascii_case(element))
    {
        let reference = attribute(child, "refId").to_ascii_lowercase();
        let key = format!("{reference}_{suffix}");
        let Some(base) = tracks.get(&reference).cloned() else {
            continue;
        };
        let Some(variant) = tracks.get_mut(&key) else {
            continue;
        };
        let explicit_name = attribute(child, "name");
        variant.name = if explicit_name.is_empty() {
            format!(
                "[{}] {}",
                if suffix == "rvs" {
                    "리버스"
                } else {
                    "크레이지"
                },
                base.name
            )
        } else {
            explicit_name.to_owned()
        };
        variant.blocked = base.blocked || attribute_is_true(child, "blocked");
        if has_attribute(child, "basicAi") {
            variant.basic_ai = attribute_is_true(child, "basicAi");
        }
    }
}

fn build_pools(random_root: &BmlNode, tracks: &[RandomTrackDefinition]) -> Vec<RandomTrackPool> {
    let track_map = tracks
        .iter()
        .map(|track| (track.id.to_ascii_lowercase(), track))
        .collect::<HashMap<_, _>>();
    let sources = random_pool_sources(random_root);
    let active = |ids: Vec<String>| {
        let mut seen = HashSet::new();
        ids.into_iter()
            .filter(|id| {
                track_map.contains_key(&id.to_ascii_lowercase())
                    && seen.insert(id.to_ascii_lowercase())
            })
            .collect::<Vec<_>>()
    };
    let base_tracks = |types: &[&str]| {
        tracks
            .iter()
            .filter(|track| {
                types
                    .iter()
                    .any(|game_type| track.game_type.eq_ignore_ascii_case(game_type))
                    && !track.id.to_ascii_lowercase().ends_with("_rvs")
                    && !track.id.to_ascii_lowercase().ends_with("_crz")
            })
            .map(|track| track.id.clone())
            .collect::<Vec<_>>()
    };
    let direct = |game_type: &str, random_type: &str| {
        active(
            sources
                .sets
                .get(&(
                    game_type.to_ascii_lowercase(),
                    random_type.to_ascii_lowercase(),
                ))
                .cloned()
                .unwrap_or_default(),
        )
    };
    let listed = |random_type: &str| {
        active(
            sources
                .lists
                .get(&random_type.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default(),
        )
    };
    let item_only = |ids: Vec<String>| {
        ids.into_iter()
            .filter(|id| {
                track_map[&id.to_ascii_lowercase()]
                    .game_type
                    .eq_ignore_ascii_case("item")
            })
            .collect::<Vec<_>>()
    };
    let mut pools = Vec::new();
    let mut add = |game_type: u8, selector: u32, ids: Vec<String>| {
        let ids = active(ids);
        if !ids.is_empty() {
            pools.push(RandomTrackPool {
                game_type,
                selector,
                korean_name: pool_name(game_type, selector),
                default_track_ids: ids,
            });
        }
    };
    add(0, 0, base_tracks(&["speed", "item"]));
    add(1, 0, base_tracks(&["item"]));
    add(0, 1, direct("speed", "clubSpeed"));
    add(1, 1, direct("item", "clubItem"));
    for selector in 3..=7 {
        let random_type = format!("hot{}", selector - 2);
        add(0, selector, direct("speed", &random_type));
        add(1, selector, direct("item", &random_type));
    }
    let new_tracks = listed("new");
    add(0, 8, new_tracks.clone());
    add(1, 8, item_only(new_tracks));
    add(1, 23, direct("item", "crazy"));
    let reverse = listed("reverse");
    add(0, 30, reverse.clone());
    add(1, 30, item_only(reverse));
    add(0, 33, direct("speed", "newLeagueRandom"));
    add(1, 33, direct("item", "newLeagueRandom"));
    add(0, 40, base_tracks(&["speed"]));
    pools
}

fn random_pool_sources(random_root: &BmlNode) -> RandomPoolSources {
    let mut sources = RandomPoolSources::default();
    for child in &random_root.children {
        if child.name.eq_ignore_ascii_case("RandomTrackSet") {
            sources
                .sets
                .entry((
                    attribute(child, "gameType").to_ascii_lowercase(),
                    attribute(child, "randomType").to_ascii_lowercase(),
                ))
                .or_default()
                .extend(track_ids(child));
        } else if child.name.eq_ignore_ascii_case("RandomTrackList") {
            sources.lists.insert(
                attribute(child, "randomType").to_ascii_lowercase(),
                track_ids(child),
            );
        }
    }
    sources
}

fn validate_configuration(
    configuration: &RandomTrackConfiguration,
) -> Result<(), RandomTrackError> {
    if configuration.pools.len() > MAX_POOLS {
        return Err(RandomTrackError::TooManyOverrides { maximum: MAX_POOLS });
    }
    let mut keys = HashSet::new();
    for pool in &configuration.pools {
        if pool.game_type > 1
            || !is_random_track_selector(pool.selector)
            || !SUPPORTED_SELECTORS.contains(&pool.selector)
        {
            return Err(RandomTrackError::InvalidOverridePool {
                game_type: pool.game_type,
                selector: pool.selector,
            });
        }
        if !keys.insert((pool.game_type, pool.selector)) {
            return Err(RandomTrackError::DuplicateOverridePool {
                game_type: pool.game_type,
                selector: pool.selector,
            });
        }
        if pool.track_ids.is_empty() || pool.track_ids.len() > MAX_TRACKS_PER_POOL {
            return Err(RandomTrackError::InvalidOverrideTrackCount {
                maximum: MAX_TRACKS_PER_POOL,
            });
        }
        let mut ids = HashSet::new();
        for id in &pool.track_ids {
            if id.is_empty()
                || id.len() > 80
                || id.chars().any(char::is_control)
                || !ids.insert(id.to_ascii_lowercase())
            {
                return Err(RandomTrackError::InvalidOverrideTrackId { id: id.clone() });
            }
        }
    }
    Ok(())
}

fn is_compatible_track(pool: &RandomTrackPool, track: &RandomTrackDefinition) -> bool {
    let id = track.id.to_ascii_lowercase();
    let reverse = id.ends_with("_rvs");
    let crazy = id.ends_with("_crz");
    if (pool.selector == 30 && !reverse)
        || (pool.selector == 23 && !crazy)
        || (!matches!(pool.selector, 23 | 30) && (reverse || crazy))
    {
        return false;
    }
    if pool.game_type == 1 {
        track.game_type.eq_ignore_ascii_case("item")
    } else if pool.selector == 40 {
        track.game_type.eq_ignore_ascii_case("speed")
    } else {
        track.game_type.eq_ignore_ascii_case("speed")
            || track.game_type.eq_ignore_ascii_case("item")
    }
}

fn attribute<'a>(node: &'a BmlNode, name: &str) -> &'a str {
    node.attributes
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map_or("", |(_, value)| value)
}

fn has_attribute(node: &BmlNode, name: &str) -> bool {
    node.attributes
        .iter()
        .any(|(key, _)| key.eq_ignore_ascii_case(name))
}

fn attribute_is_true(node: &BmlNode, name: &str) -> bool {
    attribute(node, name).eq_ignore_ascii_case("true")
}

fn track_ids(node: &BmlNode) -> Vec<String> {
    node.children
        .iter()
        .filter(|child| child.name.eq_ignore_ascii_case("track"))
        .map(|child| attribute(child, "id").to_owned())
        .filter(|id| !id.is_empty())
        .collect()
}

fn pool_name(game_type: u8, selector: u32) -> String {
    let mode = if game_type == 0 {
        "스피드전"
    } else {
        "아이템전"
    };
    let name = match selector {
        0 => "전체 랜덤",
        1 => "클럽 랜덤",
        3 => "인기 매우 쉬움",
        4 => "인기 쉬움",
        5 => "인기 보통",
        6 => "인기 어려움",
        7 => "인기 매우 어려움",
        8 => "신규 트랙",
        23 => "크레이지",
        30 => "리버스",
        33 => "뉴 리그",
        40 => "스피드 전용",
        _ => "랜덤",
    };
    format!("{mode} · {name}")
}

fn unicode_adler(value: &str) -> u32 {
    const MODULUS: u32 = 65_521;
    let mut a = 0_u32;
    let mut b = 0_u32;
    for byte in value.encode_utf16().flat_map(u16::to_le_bytes) {
        a = (a + u32::from(byte)) % MODULUS;
        b = (b + a) % MODULUS;
    }
    a | (b << 16)
}

#[cfg(test)]
mod tests {
    use super::{
        RandomTrackConfiguration, RandomTrackPoolOverride, load_client_random_track_catalog,
        validate_configuration,
    };

    #[test]
    fn override_validation_rejects_duplicates_and_unknown_selectors() {
        let pool = RandomTrackPoolOverride {
            game_type: 0,
            selector: 3,
            track_ids: vec!["village_R01".to_owned()],
        };
        assert!(
            validate_configuration(&RandomTrackConfiguration {
                pools: vec![pool.clone()]
            })
            .is_ok()
        );
        assert!(
            validate_configuration(&RandomTrackConfiguration {
                pools: vec![pool.clone(), pool]
            })
            .is_err()
        );
        assert!(
            validate_configuration(&RandomTrackConfiguration {
                pools: vec![RandomTrackPoolOverride {
                    game_type: 0,
                    selector: 2,
                    track_ids: vec!["village_R01".to_owned()]
                }]
            })
            .is_err()
        );
    }

    #[test]
    #[ignore = "requires P5136_CLIENT_DATA to point at the stock Data directory"]
    fn stock_client_catalog_smoke() {
        let data = std::env::var_os("P5136_CLIENT_DATA").expect("P5136_CLIENT_DATA");
        let catalog = load_client_random_track_catalog(data).unwrap();
        assert!(catalog.tracks().len() >= 200);
        assert!(catalog.pools().len() >= 15);
        catalog
            .resolve(&RandomTrackConfiguration::default())
            .unwrap();
    }
}
