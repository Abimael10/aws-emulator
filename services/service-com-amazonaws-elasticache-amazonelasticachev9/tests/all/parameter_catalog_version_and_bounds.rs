// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the engine version and the node-type-scoped bounds a
//! cache parameter group reports for each of its parameters.
//!
//! The rule under test: `DescribeCacheParameters` reports each parameter of the
//! group's family with the earliest engine version of THAT family the parameter
//! applies to — the floor of the family's version line for a parameter the
//! engine already carried before that line, and the version that introduced it
//! for one the line itself introduced — and reports every parameter whose value
//! the cache node type fixes among the cache-node-type-specific parameters, as a
//! populated member that carries the range the engine bounds that value to and
//! one `CacheNodeType`/`Value` pair per cache node type the service publishes.
//!
//! Every assertion goes over the wire through the same router the production
//! emulator mounts, reusing the harness in [`crate::harness`] and the response
//! scanners in [`crate::describe_cache_parameters`]; only the HTTP status and
//! the XML body are inspected, so an answer that was not computed from the
//! persisted catalog cannot pass. The tables range over the axes the rule
//! generalises over — the FAMILY (both engine catalogs, every published
//! generation), whether the parameter predates its family's line or was
//! introduced inside it, and which of the two lists reports it — so the one
//! family a recorded call happens to name is one row of many. The version and
//! allowed-value columns are LITERALS, so a catalog that drifts fails here
//! rather than being echoed back.

use std::collections::BTreeSet;

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::describe_cache_parameters::{
    create_group, describe_ok, numeric_terms, one, reported, unpaged_names,
};
use crate::harness::{Emulator, Reply};

/// One version row: the family, a parameter its catalog publishes, and the
/// `MinimumEngineVersion` that family must report it under.
///
/// Source: the family lines this service publishes (`state::FAMILY_LINES`,
/// whose floors are the releases each line starts at) and the engine catalogs
/// behind them. The first group of rows are parameters OLDER than the family's
/// line, which a family reports from its own floor; the last rows are
/// parameters a family's own line introduced, which keep the introducing
/// version. Both engines and every published generation are covered, including
/// the families no recorded call names.
const VERSION_ROWS: [(&str, &str, &str); 17] = [
    ("redis2.6", "maxmemory", "2.6.13"),
    ("redis2.8", "maxmemory", "2.8.6"),
    ("redis3.2", "hll-sparse-max-bytes", "3.2.4"),
    ("redis4.0", "timeout", "4.0.9"),
    ("redis5.0", "activerehashing", "5.0.0"),
    ("redis6.x", "databases", "6.0.5"),
    ("redis7", "maxmemory-samples", "7.0.0"),
    ("valkey7", "hash-max-listpack-entries", "7.2.0"),
    ("valkey8", "maxmemory", "8.0.0"),
    ("valkey9", "lfu-log-factor", "9.0.0"),
    ("memcached1.4", "max_cache_memory", "1.4.5"),
    ("memcached1.5", "chunk_size", "1.5.10"),
    ("memcached1.6", "lru_crawler", "1.6.6"),
    // A parameter the family's OWN line introduced: the introducing version is
    // already the earliest version of that family it applies to.
    ("redis6.x", "lazyfree-lazy-user-flush", "6.2.6"),
    ("redis7", "set-max-listpack-entries", "7.2.0"),
    ("redis5.0", "cluster-allow-reads-when-down", "5.0.6"),
    ("memcached1.4", "hash_algorithm", "1.4.24"),
];

/// One allowed-values row: the family, a parameter whose value the cache node
/// type fixes, and the `AllowedValues` the catalog publishes for it.
///
/// Source: the engine bounds the published catalog states for those parameters
/// — a Memcached node's cache memory is bounded at both ends in megabytes, its
/// worker threads at one and above, and the Redis OSS-compatible byte counts at
/// zero and above. A parameter a client cannot SET still publishes its range,
/// because a client sizing a node from the catalog needs the bounds.
const ALLOWED_VALUE_ROWS: [(&str, &str, &str); 6] = [
    ("memcached1.4", "max_cache_memory", "1-100000"),
    ("memcached1.6", "num_threads", "1-"),
    ("redis2.6", "maxmemory", "0-"),
    ("valkey9", "maxmemory", "0-"),
    ("redis7", "client-output-buffer-limit-replica-hard-limit", "0-"),
    ("redis7", "maxmemory-clients", "0-"),
];

/// The group every published family reports its catalog through: the default
/// group the service provides for that family.
fn default_group(family: &str) -> String {
    format!("default.{family}")
}

/// The line floor of `family` — the oldest engine release of its line
/// (`state::FAMILY_LINES`).
fn line_floor(family: &str) -> &'static str {
    state::FAMILY_LINES
        .iter()
        .find(|(name, _, _, _)| *name == family)
        .map(|(_, _, _, floor)| *floor)
        .unwrap_or_else(|| panic!("case {family}: a published family names its line floor"))
}

/// Read the whole catalog of `group` in one page: the documented maximum
/// `MaxRecords` is 100, which is above every published family's parameter count.
async fn describe_whole(emulator: &Emulator, group: &str) -> Reply {
    describe_ok(emulator, group, &[("MaxRecords", "100")]).await
}

#[tokio::test]
async fn a_parameter_is_reported_under_the_earliest_version_of_its_family_it_applies_to() {
    let emulator = Emulator::new();

    for (family, parameter, expected) in VERSION_ROWS {
        let group = default_group(family);
        let case = format!("{family}/{parameter}");
        let reply = describe_whole(&emulator, &group).await;
        let reported = one(&reply, &case, parameter);
        assert_eq!(
            reported.field("MinimumEngineVersion"),
            Some(expected),
            "case {case}: expected MinimumEngineVersion {expected} — the earliest version of \
             this family the parameter applies to — got {reported:?}"
        );
    }
}

#[tokio::test]
async fn no_family_reports_a_parameter_from_below_its_own_line_floor() {
    let emulator = Emulator::new();

    // The invariant, over BOTH lists of every published family: a family's line
    // starts at its floor, so no version it reports a parameter under can be
    // older than that floor — and the floor itself is reported by the
    // parameters the engine already carried before the line.
    for family in state::PARAMETER_GROUP_FAMILIES {
        let group = default_group(family);
        let reply = describe_whole(&emulator, &group).await;
        let rows = reported(&reply);
        assert!(
            !rows.is_empty(),
            "case {family}: a published family reports its catalog, got {}",
            reply.body
        );
        let floor = line_floor(family);
        let floor_key = state::version_sort_key(floor);
        let mut at_the_floor = 0;
        for row in &rows {
            let version = row
                .field("MinimumEngineVersion")
                .unwrap_or_else(|| panic!("case {family}/{}: no version: {row:?}", row.name));
            assert!(
                state::version_sort_key(version) >= floor_key,
                "case {family}/{}: a parameter cannot apply from below the line floor {floor}, \
                 got {version}",
                row.name
            );
            if version == floor {
                at_the_floor += 1;
            }
        }
        assert!(
            at_the_floor > 0,
            "case {family}: the parameters the engine carried before this line report its \
             floor {floor}, got {:?}",
            rows.iter()
                .map(|row| (row.name.as_str(), row.field("MinimumEngineVersion")))
                .collect::<Vec<_>>()
        );
    }
}

#[tokio::test]
async fn every_family_reports_its_node_type_scoped_parameters_as_a_populated_list() {
    let emulator = Emulator::new();

    let published: BTreeSet<&str> = state::OFFERING_NODE_TYPES
        .iter()
        .map(|(node_type, _)| *node_type)
        .collect();
    for family in state::PARAMETER_GROUP_FAMILIES {
        let group = default_group(family);
        let reply = describe_whole(&emulator, &group).await;
        let scoped: Vec<_> = reported(&reply)
            .into_iter()
            .filter(|row| row.node_type_specific)
            .collect();
        assert!(
            !scoped.is_empty(),
            "case {family}: a family publishes the parameters its node type fixes, got {}",
            reply.body
        );
        assert_eq!(
            unpaged_names(&reply).len(),
            scoped.len(),
            "case {family}: every node-type-scoped parameter is named once, got {}",
            reply.body
        );
        let floor_key = state::version_sort_key(line_floor(family));
        for row in &scoped {
            let case = format!("{family}/{}", row.name);
            for member in ["Description", "DataType", "AllowedValues", "MinimumEngineVersion"] {
                assert!(
                    row.field(member).is_some_and(|text| !text.is_empty()),
                    "case {case}: {member} must be published, got {row:?}"
                );
            }
            assert!(
                row.field("ParameterValue").is_none(),
                "case {case}: a value the node type fixes is not a single value, got {row:?}"
            );
            assert!(
                state::version_sort_key(row.field("MinimumEngineVersion").unwrap_or_default())
                    >= floor_key,
                "case {case}: the version cannot predate the line floor, got {row:?}"
            );
        }

        // One `CacheNodeType`/`Value` pair per published node type for each of
        // them, and every value inside the range the member publishes.
        let node_types: Vec<&str> = reply.all("CacheNodeTypeSpecificValue/CacheNodeType");
        let values: Vec<&str> = reply.all("CacheNodeTypeSpecificValue/Value");
        assert_eq!(
            node_types.len(),
            published.len() * scoped.len(),
            "case {family}: expected one pair per published node type for each of {:?}, got \
             {node_types:?}",
            scoped.iter().map(|row| &row.name).collect::<Vec<_>>()
        );
        assert_eq!(
            values.len(),
            node_types.len(),
            "case {family}: every node type reports a value, got {values:?}"
        );
        assert_eq!(
            node_types.iter().copied().collect::<BTreeSet<&str>>(),
            published,
            "case {family}: the node types are the ones this service publishes, got {}",
            reply.body
        );
        for (index, value) in values.iter().enumerate() {
            let row = &scoped[index / published.len()];
            let case = format!("{family}/{}", row.name);
            let allowed = row.field("AllowedValues").unwrap_or_default();
            let terms = numeric_terms(allowed).unwrap_or_else(|| {
                panic!("case {case}: expected a published numeric range, got {allowed:?}")
            });
            let number: f64 = value
                .parse()
                .unwrap_or_else(|_| panic!("case {case}: expected a number, got {value}"));
            assert!(
                terms.iter().any(|term| term.covers(number)),
                "case {case}: a node-type value is inside the published range {allowed}, got \
                 {value}"
            );
        }
    }
}

#[tokio::test]
async fn a_node_type_scoped_parameter_publishes_the_range_the_engine_bounds_it_to() {
    let emulator = Emulator::new();

    for (family, parameter, expected) in ALLOWED_VALUE_ROWS {
        let group = default_group(family);
        let case = format!("{family}/{parameter}");
        let reply = describe_whole(&emulator, &group).await;
        let reported = one(&reply, &case, parameter);
        assert!(
            reported.node_type_specific,
            "case {case}: this parameter's value is fixed by the node type, got {reported:?}"
        );
        assert_eq!(
            reported.field("AllowedValues"),
            Some(expected),
            "case {case}: expected the published range {expected}, got {reported:?}"
        );
    }
}

#[tokio::test]
async fn a_created_group_reports_its_familys_versions_and_bounds() {
    let emulator = Emulator::new();

    // A group of one's own is the same catalog read through another group, so
    // the rule holds for a created group too — the round trip through
    // `CreateCacheParameterGroup` and back out of `DescribeCacheParameters`.
    for (index, family) in ["valkey9", "memcached1.4", "redis2.8"].into_iter().enumerate() {
        let group = format!("version-rule-{index}");
        create_group(&emulator, &group, family).await;
        let created = describe_whole(&emulator, &group).await;
        let default = describe_whole(&emulator, &default_group(family)).await;
        let shape = |reply: &Reply| -> Vec<(String, Option<String>, Option<String>)> {
            reported(reply)
                .into_iter()
                .map(|row| {
                    (
                        row.name.clone(),
                        row.field("MinimumEngineVersion").map(str::to_string),
                        row.field("AllowedValues").map(str::to_string),
                    )
                })
                .collect()
        };
        assert_eq!(
            shape(&created),
            shape(&default),
            "case {family}: a created group reports its family's whole catalog, with the same \
             versions and bounds, got {}",
            created.body
        );
        assert!(
            shape(&created)
                .iter()
                .any(|(_, version, _)| version.as_deref() == Some(line_floor(family))),
            "case {family}: the parameters older than the line report its floor {}, got {}",
            line_floor(family),
            created.body
        );
    }
}
