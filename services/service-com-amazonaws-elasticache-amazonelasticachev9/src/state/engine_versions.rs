// SPDX-License-Identifier: CC0-1.0

use super::*;

/// One row of the cache-engine-version catalog this emulator publishes: an
/// engine and one version of it.
///
/// The row carries NO parameter group family: which family a version belongs to
/// is already stated once, by [`parameter_group_family`], and the catalog reads
/// it from there so the family a version reports and the default group a cluster
/// on that version joins cannot disagree.
#[derive(Debug, Clone, Copy)]
pub struct PublishedEngineVersion {
    /// One of [`ENGINES`] — "the name of the cache engine"
    /// (`CacheEngineVersion$Engine`).
    pub engine: &'static str,
    /// "The version number of the cache engine"
    /// (`CacheEngineVersion$EngineVersion`).
    pub engine_version: &'static str,
}

/// One catalog row, so the table below reads as a table.
pub(super) const fn engine_version_row(
    engine: &'static str,
    engine_version: &'static str,
) -> PublishedEngineVersion {
    PublishedEngineVersion {
        engine,
        engine_version,
    }
}

/// The cache engines and versions this emulator publishes.
///
/// AWS publishes this catalog itself — the API has no operation that adds a
/// version — so the store holds it from the moment it comes up and
/// `DescribeCacheEngineVersions` reports it. Every row's version belongs to one
/// of the published [`PARAMETER_GROUP_FAMILIES`], which is where its
/// `CacheParameterGroupFamily` comes from.
///
/// The rows are the versions ElastiCache OFFERS today, per engine: the
/// Memcached releases it still serves, the Redis OSS lines it still serves
/// (4.0.10 and 5.0.6, then the 6 and 7 generations) and the Valkey generations.
/// A version ElastiCache has RETIRED is offered by nothing, so it is absent
/// here even where a parameter group family of that line is still published —
/// the family catalog outlives the engine versions it was published for, which
/// is why [`PARAMETER_GROUP_FAMILIES`] is the longer list.
///
/// What the rest of the service relies on beside the entries is the SHAPE of the
/// catalog — several version lines per engine, several versions per line, and a
/// newest version per engine, which is the one [`default_engine_version`] hands
/// a cluster that names none.
///
/// A Memcached version and a Redis OSS version up to 5 are named by their patch
/// release (`1.6.22`, `5.0.6`), while ElastiCache names a Redis OSS 6 or later
/// version and every Valkey version by its LINE alone (`6.0`, `6.2`, `7.0`,
/// `7.1`, `7.2`, `8.0`) — which is also the spelling a create or an upgrade has
/// to name, so the catalog publishes those lines rather than a patch release
/// behind them.
pub const CACHE_ENGINE_VERSIONS: [PublishedEngineVersion; 23] = [
    // The oldest Memcached version ElastiCache still offers.
    engine_version_row("memcached", "1.4.5"),
    // The version `EngineVersion`'s own documentation uses as its example.
    engine_version_row("memcached", "1.4.14"),
    engine_version_row("memcached", "1.4.24"),
    engine_version_row("memcached", "1.4.33"),
    engine_version_row("memcached", "1.4.34"),
    engine_version_row("memcached", "1.5.10"),
    engine_version_row("memcached", "1.5.16"),
    engine_version_row("memcached", "1.6.6"),
    engine_version_row("memcached", "1.6.12"),
    engine_version_row("memcached", "1.6.17"),
    engine_version_row("memcached", "1.6.22"),
    // The oldest Redis OSS releases ElastiCache still offers; the lines below
    // 4.0 have been retired.
    engine_version_row("redis", "4.0.10"),
    engine_version_row("redis", "5.0.6"),
    engine_version_row("redis", "6.0"),
    engine_version_row("redis", "6.2"),
    engine_version_row("redis", "7.0"),
    engine_version_row("redis", "7.1"),
    engine_version_row("valkey", "7.2"),
    engine_version_row("valkey", "8.0"),
    engine_version_row("valkey", "8.1"),
    engine_version_row("valkey", "8.2"),
    engine_version_row("valkey", "9.0"),
    // The current Valkey generation, whose family is `valkey9` (see
    // `PARAMETER_GROUP_FAMILIES`).
    engine_version_row("valkey", "9.1"),
];

/// How a published version's engine is described
/// (`CacheEngineVersion$CacheEngineDescription`: "the description of the cache
/// engine").
///
/// The catalog describes an engine by the WHOLE label ElastiCache publishes for
/// it, not by the engine token the `Engine` member carries: `memcached` and
/// `Redis` are published under the engine's own product name, spelled the way
/// that project spells it, and Valkey under the longer label ElastiCache gives
/// its Valkey offering. A label is therefore stated here per engine rather than
/// derived from the token by re-casing it.
///
/// `CacheEngineVersion$CacheEngineVersionDescription` is built from the engine's
/// API name instead (see [`published_cache_engine_versions`]), which is why that
/// string is not derived from this one.
pub fn engine_description(engine: &str) -> &'static str {
    match engine {
        "memcached" => "memcached",
        ENGINE_VALKEY => "Valkey Engine for ElastiCache",
        _ => "Redis",
    }
}

/// The patch-level release behind each version ElastiCache names by its LINE
/// alone, as `(engine, published version, release)`.
///
/// Source: the release AWS lists for each of those engine lines among the
/// ElastiCache engine versions it supports — Redis OSS 6.0 is 6.0.5, 6.2 is
/// 6.2.6, 7.0 is 7.0.7 and 7.1 is 7.1.0; Valkey 7.2 is 7.2.6 and 8.0 is 8.0.1.
/// The Valkey lines above 8.0 are each published by their FIRST release, the
/// `.0` an engine line's first release is numbered with (8.1.0, 8.2.0, 9.0.0,
/// 9.1.0), which is the release the published version description names them
/// by.
///
/// Every other published version already NAMES a patch release (`1.6.22`,
/// `5.0.6`), so it is its own release and is absent from this table — see
/// [`patch_release`].
const LINE_PATCH_RELEASES: [(&str, &str, &str); 10] = [
    ("redis", "6.0", "6.0.5"),
    ("redis", "6.2", "6.2.6"),
    ("redis", "7.0", "7.0.7"),
    ("redis", "7.1", "7.1.0"),
    (ENGINE_VALKEY, "7.2", "7.2.6"),
    (ENGINE_VALKEY, "8.0", "8.0.1"),
    (ENGINE_VALKEY, "8.1", "8.1.0"),
    (ENGINE_VALKEY, "8.2", "8.2.0"),
    (ENGINE_VALKEY, "9.0", "9.0.0"),
    (ENGINE_VALKEY, "9.1", "9.1.0"),
];

/// The full release `engine_version` of `engine` stands for: the patch release
/// behind a version ElastiCache names by its version LINE
/// ([`LINE_PATCH_RELEASES`]), and the version itself where it already names a
/// patch release.
///
/// This is what the version DESCRIPTION publishes, because the description is
/// the only place an entry whose `EngineVersion` is a line states the release
/// behind it.
pub fn patch_release<'a>(engine: &str, engine_version: &'a str) -> &'a str {
    LINE_PATCH_RELEASES
        .iter()
        .find(|(line_engine, line, _)| *line_engine == engine && *line == engine_version)
        .map_or(engine_version, |(_, _, release)| *release)
}

/// `version` with every numeric component zero-padded to a fixed width, so
/// comparing two of these byte-wise orders them the way version numbers compare
/// — `1.6.22` AFTER `1.6.6`, which the raw strings get backwards.
///
/// Four digits is the width: no cache engine has ever carried a version
/// component anywhere near 10000, and a component that wide would be the only
/// thing this ordering got wrong.
pub fn version_sort_key(version: &str) -> String {
    version
        .split('.')
        .map(|part| match part.parse::<u32>() {
            Ok(number) => format!("{number:04}"),
            // A component that is not a number (the `x` of an open `6.x` line)
            // sorts after every numbered one: `~` is above the digits in ASCII.
            Err(_) => format!("~{part}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

/// The catalog key of `engine_version` of `engine`.
///
/// The engine name and then the version's sort key, so the map is ordered by
/// engine and, within an engine, by version from oldest to newest. That total
/// order is what the `Marker` indexes into, and its last row per engine is the
/// newest version of that engine.
pub fn engine_version_key(engine: &str, engine_version: &str) -> String {
    format!("{engine}|{}", version_sort_key(engine_version))
}

/// True when `version` is a version the filter value `filter` asks for: the same
/// version, or one in the version line it names (`5.0` names `5.0.5` and
/// `5.0.6`).
///
/// `EngineVersion` is "the cache engine version to return" with the example
/// `1.4.14`, and `DefaultOnly` speaks of "the specified engine or engine and
/// major version combination" — so a value that names only the leading
/// components of a version names that whole line.
pub fn version_matches(filter: &str, version: &str) -> bool {
    version == filter
        || version
            .strip_prefix(filter)
            .is_some_and(|rest| rest.starts_with('.'))
}

/// The highest version [`CACHE_ENGINE_VERSIONS`] publishes for `engine`, or
/// `None` when it publishes none for it.
pub(super) fn latest_published_version(engine: &str) -> Option<&'static str> {
    CACHE_ENGINE_VERSIONS
        .iter()
        .filter(|row| row.engine == engine)
        .max_by_key(|row| version_sort_key(row.engine_version))
        .map(|row| row.engine_version)
}

/// True when the published catalog OFFERS `engine_version` for `engine`.
///
/// A request may name a published version itself, or the version LINE one belongs to (`5.0`
/// names `5.0.5` and `5.0.6`) — the same reading [`version_matches`] gives the describe's
/// own filter, so what a client may create on is exactly what
/// `DescribeCacheEngineVersions` offers it. A version of ANOTHER engine, or one past what
/// this engine publishes, is offered by nothing.
pub fn version_is_offered(engine: &str, engine_version: &str) -> bool {
    CACHE_ENGINE_VERSIONS
        .iter()
        .any(|row| row.engine == engine && version_matches(engine_version, row.engine_version))
}

/// How ElastiCache words a request naming an engine version the engine does not offer:
/// `Cannot find version <engine-version> for <engine>`.
///
/// The variable parts are the version and the engine the request named; the rest is fixed
/// text, and it carries no trailing stop.
pub fn version_not_offered_message(engine: &str, engine_version: &str) -> String {
    format!("Cannot find version {engine_version} for {engine}")
}

/// The FIRST engine version of each engine that supports ElastiCache's encryption
/// features, as `(engine, version)`.
///
/// Source: the oldest engine version AWS states those features are available from —
/// Memcached supports in-transit encryption from 1.6.12 onwards, and Redis OSS from 3.2.6
/// onwards (AWS lists 3.2.6 and 4.0.10 or later, and 4.0.10 is the oldest Redis OSS version
/// [`CACHE_ENGINE_VERSIONS`] publishes, so no published Redis version is below this entry).
/// An engine ABSENT from this table supports encryption in every version ElastiCache
/// publishes for it, which is the case for Valkey (published from 7.2 up).
///
/// A version below its engine's entry can run a cache, but not an encrypted one: that
/// engine build has no encryption to turn on, so asking for it is a combination the
/// service cannot serve ([`encryption_unsupported_message`]).
const ENCRYPTION_MINIMUM_VERSIONS: [(&str, &str); 2] =
    [("memcached", "1.6.12"), ("redis", "3.2.6")];

/// How ElastiCache words a request that asks for an encryption feature on an engine
/// version that has none: `Encryption features are not supported for engine version
/// <engine-version>. Please use engine version <minimum>`.
///
/// The variable parts are the version the request named and the first version of that
/// engine which supports encryption; the rest is fixed text, and it carries no trailing
/// stop.
pub fn encryption_unsupported_message(engine_version: &str, minimum: &str) -> String {
    format!(
        "Encryption features are not supported for engine version {engine_version}. Please use \
         engine version {minimum}"
    )
}

/// Why `engine_version` of `engine` cannot carry an encryption feature, or `None` when it
/// can.
///
/// The ONE statement of that rule, so a cache cluster create and a replication group
/// create refuse the same pair the same way. A version at or above its engine's first
/// encryption-capable release ([`ENCRYPTION_MINIMUM_VERSIONS`]) can carry it, and so can
/// every version of an engine the table names no minimum for.
pub fn encryption_support_refusal(engine: &str, engine_version: &str) -> Option<String> {
    let (_, minimum) = ENCRYPTION_MINIMUM_VERSIONS
        .iter()
        .find(|(named, _)| *named == engine)?;
    // Compared as version NUMBERS rather than as strings, so 1.6.6 counts as older than
    // 1.6.12 ([`version_sort_key`]).
    (version_sort_key(engine_version) < version_sort_key(minimum))
        .then(|| encryption_unsupported_message(engine_version, minimum))
}

/// One published cache engine version, as this emulator holds it.
#[derive(Debug, Clone)]
pub struct CacheEngineVersionRecord {
    /// See [`PublishedEngineVersion::engine`].
    pub engine: String,
    /// See [`PublishedEngineVersion::engine_version`].
    pub engine_version: String,
    /// The family the version belongs to, from [`parameter_group_family`] —
    /// absent for a version no published family covers, which is how
    /// `CacheEngineVersion$CacheParameterGroupFamily` reports "none" rather than
    /// inventing a family name.
    pub cache_parameter_group_family: Option<String>,
    /// See [`engine_description`].
    pub cache_engine_description: String,
    /// "The description of the cache engine version"
    /// (`CacheEngineVersion$CacheEngineVersionDescription`).
    pub cache_engine_version_description: String,
}

impl CacheEngineVersionRecord {
    /// This record as the modelled `CacheEngineVersion`.
    pub fn to_modeled(&self) -> CacheEngineVersion {
        CacheEngineVersion {
            engine: Some(self.engine.clone()),
            engine_version: Some(self.engine_version.clone()),
            cache_parameter_group_family: self.cache_parameter_group_family.clone(),
            cache_engine_description: Some(self.cache_engine_description.clone()),
            cache_engine_version_description: Some(self.cache_engine_version_description.clone()),
        }
    }
}
