// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the METADATA a cache parameter read publishes for a
//! parameter — `Parameter$Description` and `Parameter$MinimumEngineVersion`.
//!
//! The rule under test: a parameter's description and minimum engine version are
//! CATALOG FACTS about that parameter on the group's family, served from the
//! family's published catalog — the description is the published catalog's own
//! one-line wording for the parameter, identical for every family that publishes
//! it and through every read that reports it, and the minimum engine version is
//! the earliest release of the family's own line the parameter applies to.
//! Neither is composed from the request or from the group: a write that sets or
//! changes the parameter's VALUE moves only `ParameterValue` and `Source`, and a
//! read leaves both exactly as the next caller finds them.
//!
//! Every assertion goes over the wire through the same router the production
//! emulator mounts, reusing the harness in [`crate::harness`], the parameter
//! scanners in [`crate::describe_cache_parameters`] and the writer in
//! [`crate::modify_cache_parameter_group`]; only the HTTP status and the XML
//! body are inspected, so metadata that was not read out of the persisted
//! catalog cannot pass. The tables range over what the rule generalises over —
//! the PARAMETER (two of them, one a flag and one an enumeration), the FAMILY
//! that publishes it (every published generation of the Redis OSS-compatible
//! line, so the one family a recorded call happens to name is one row of many),
//! the GROUP it is read through (a family default, the cluster-mode-on default
//! whose value the service itself set, and a group of one's own), the READ that
//! reports it (per-group and engine-default) and whether the parameter's value
//! has been written. The description and version columns are LITERALS, so a
//! catalog re-authored into prose of this crate's own fails here rather than
//! being echoed back.

use std::collections::{BTreeMap, BTreeSet};

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::describe_cache_parameters::{create_group, describe_ok, one, reported};
use crate::harness::{params, Emulator, Reply};
use crate::modify_cache_parameter_group::modify_ok;

/// One description row: a parameter and the published catalog's own one-line
/// description of it, and a value the parameter takes beside its default.
///
/// Source: the published parameter table of the Redis OSS-compatible engines,
/// which words the cluster-mode flag and the ACL pub/sub default exactly as
/// below — the published wording, with its own capitalisation and its own stop
/// or lack of one. The second value column is a value the row's published
/// `AllowedValues` covers, used to prove a WRITE does not re-author the
/// metadata.
const DESCRIPTION_ROWS: [(&str, &str, &str); 2] = [
    ("cluster-enabled", "Enable cluster mode", "yes"),
    (
        "acl-pubsub-default",
        "Default pubsub channel permissions for ACL users deployed to this cluster",
        "allchannels",
    ),
];

/// The `MinimumEngineVersion` each family publishing the cluster-mode flag
/// reports it under, as `(family, version)`.
///
/// Source: the oldest engine release of each family's own version line (the
/// floors of `state::FAMILY_LINES`) — the cluster-mode flag arrived with Redis
/// OSS 3.2.4, so the family whose line starts there reports that release and
/// every later line reports its own floor, since no version below it exists
/// inside that family. Every published family that carries the parameter is a
/// row, including the ones no recorded call names.
const CLUSTER_MODE_VERSION_ROWS: [(&str, &str); 8] = [
    ("redis3.2", "3.2.4"),
    ("redis4.0", "4.0.9"),
    ("redis5.0", "5.0.0"),
    ("redis6.x", "6.0.5"),
    ("redis7", "7.0.0"),
    ("valkey7", "7.2.0"),
    ("valkey8", "8.0.0"),
    ("valkey9", "9.0.0"),
];

/// The default group of `family`: the group every published family's catalog can
/// be read through.
fn default_group(family: &str) -> String {
    format!("default.{family}")
}

/// Read the whole catalog of `group` in one page: the documented maximum
/// `MaxRecords` is 100, which is above every published family's parameter count.
async fn describe_whole(emulator: &Emulator, group: &str) -> Reply {
    describe_ok(emulator, group, &[("MaxRecords", "100")]).await
}

/// Read a family's engine defaults — the OTHER read that reports the same
/// catalog rows.
async fn engine_defaults(emulator: &Emulator, family: &str) -> Reply {
    let reply = emulator
        .call(
            "DescribeEngineDefaultParameters",
            &params(&[
                ("CacheParameterGroupFamily", family),
                ("MaxRecords", "100"),
            ]),
        )
        .await;
    assert_eq!(
        reply.status, 200,
        "case {family}: DescribeEngineDefaultParameters must answer 200, got {} {}",
        reply.status, reply.body
    );
    reply
}

/// The families whose published catalog carries `parameter`, in the order this
/// service publishes them.
fn families_publishing(parameter: &str) -> Vec<&'static str> {
    let families: Vec<&'static str> = state::PARAMETER_GROUP_FAMILIES
        .iter()
        .copied()
        .filter(|family| {
            state::family_default_parameters(family).is_some_and(|catalog| {
                catalog.values().any(|published| published.name == parameter)
            })
        })
        .collect();
    assert!(
        !families.is_empty(),
        "case {parameter}: some published family must carry the parameter"
    );
    families
}

/// The metadata of a reported parameter that is a CATALOG fact: everything the
/// row publishes about the parameter itself, as opposed to the value in effect
/// (`ParameterValue`) and the source it is reported under (`Source`).
fn catalog_metadata(row: &crate::describe_cache_parameters::Reported) -> Vec<(String, String)> {
    [
        "Description",
        "DataType",
        "AllowedValues",
        "IsModifiable",
        "MinimumEngineVersion",
        "ChangeType",
    ]
    .into_iter()
    .map(|member| {
        (
            member.to_string(),
            row.field(member).unwrap_or_default().to_string(),
        )
    })
    .collect()
}

#[tokio::test]
async fn a_parameter_is_published_with_its_catalogs_own_description_on_every_family() {
    let emulator = Emulator::new();

    for (parameter, published, _) in DESCRIPTION_ROWS {
        for family in families_publishing(parameter) {
            // The axis: the GROUP the family's catalog is read through, and the
            // READ that reports it. The cluster-mode-on default is the group
            // whose value for the cluster-mode flag the service itself set, so
            // a group holding a value reports the same metadata as one that
            // does not.
            let mut groups = vec![default_group(family)];
            if state::family_publishes_cluster_mode(family) {
                groups.push(format!(
                    "{}{}",
                    default_group(family),
                    state::CLUSTER_MODE_ON_SUFFIX
                ));
            }
            for group in &groups {
                let case = format!("{group}/{parameter}");
                let reply = describe_whole(&emulator, group).await;
                let row = one(&reply, &case, parameter);
                assert_eq!(
                    row.field("Description"),
                    Some(published),
                    "case {case}: expected the published description {published:?}, got {row:?}"
                );
            }
            let case = format!("{family}/{parameter}/engine-default");
            let reply = engine_defaults(&emulator, family).await;
            let row = one(&reply, &case, parameter);
            assert_eq!(
                row.field("Description"),
                Some(published),
                "case {case}: the engine-default read reports the same published description \
                 {published:?}, got {row:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_cluster_mode_flag_applies_from_the_earliest_release_of_each_familys_line() {
    let emulator = Emulator::new();

    let parameter = state::CLUSTER_ENABLED_PARAMETER;
    // Every family that publishes the flag is a row, so the table and the
    // catalog cannot disagree about which families carry it.
    let published_by: BTreeSet<&str> = families_publishing(parameter).into_iter().collect();
    let tabled: BTreeSet<&str> = CLUSTER_MODE_VERSION_ROWS
        .iter()
        .map(|(family, _)| *family)
        .collect();
    assert_eq!(
        published_by, tabled,
        "case {parameter}: every family publishing the flag must be a row of this table"
    );

    for (family, expected) in CLUSTER_MODE_VERSION_ROWS {
        let group = default_group(family);
        let case = format!("{family}/{parameter}");
        let reply = describe_whole(&emulator, &group).await;
        let row = one(&reply, &case, parameter);
        assert_eq!(
            row.field("MinimumEngineVersion"),
            Some(expected),
            "case {case}: expected MinimumEngineVersion {expected} — the earliest release of \
             this family's line the parameter applies to — got {row:?}"
        );
        // The same fact through the other read of the same catalog.
        let defaults = engine_defaults(&emulator, family).await;
        let default_row = one(&defaults, &case, parameter);
        assert_eq!(
            default_row.field("MinimumEngineVersion"),
            Some(expected),
            "case {case}: the engine-default read reports the same version {expected}, got \
             {default_row:?}"
        );
    }
}

#[tokio::test]
async fn writing_a_parameters_value_leaves_its_published_metadata_alone() {
    let emulator = Emulator::new();

    // The axis: the family, and the parameter written on a group of one's own —
    // the read before the write and the read after it, which is what a client
    // confirming a modify compares.
    for (index, family) in ["redis7", "valkey9"].into_iter().enumerate() {
        let group = format!("catalog-metadata-{index}");
        create_group(&emulator, &group, family).await;
        for (parameter, published, value) in DESCRIPTION_ROWS {
            let case = format!("{family}/{parameter}");
            let before = describe_whole(&emulator, &group).await;
            let before_row = one(&before, &case, parameter);
            let metadata = catalog_metadata(&before_row);
            assert_eq!(
                before_row.field("Description"),
                Some(published),
                "case {case}: expected the published description {published:?}, got {before_row:?}"
            );
            assert_eq!(
                before_row.field("Source"),
                Some(state::PARAMETER_SOURCE_SYSTEM),
                "case {case}: an unwritten parameter reports the engine's own value, got \
                 {before_row:?}"
            );

            modify_ok(&emulator, &case, &group, &[(parameter, value)]).await;

            let after = describe_whole(&emulator, &group).await;
            let after_row = one(&after, &case, parameter);
            assert_eq!(
                catalog_metadata(&after_row),
                metadata,
                "case {case}: a write to the value must leave the published metadata alone, got \
                 {after_row:?}"
            );
            assert_eq!(
                after_row.field("ParameterValue"),
                Some(value),
                "case {case}: the written value is what the group reports, got {after_row:?}"
            );
            assert_eq!(
                after_row.field("Source"),
                Some(state::PARAMETER_SOURCE_USER),
                "case {case}: a written parameter is the customer's value, got {after_row:?}"
            );
        }
    }
}

#[tokio::test]
async fn every_published_parameters_metadata_is_one_catalog_fact_per_parameter() {
    let emulator = Emulator::new();

    // The invariant, over the WHOLE catalog of every published family rather
    // than the two parameters a table can name: the description a parameter is
    // published with is the parameter's own, so the per-group read and the
    // engine-default read of one family report the same string for every row,
    // and every family that publishes a parameter publishes the same
    // description for it. A description composed per family or per read breaks
    // one of the two.
    let mut by_parameter: BTreeMap<String, BTreeMap<String, Vec<&'static str>>> = BTreeMap::new();
    for family in state::PARAMETER_GROUP_FAMILIES {
        let group = default_group(family);
        let grouped = describe_whole(&emulator, &group).await;
        let defaults = engine_defaults(&emulator, family).await;
        let rows = reported(&grouped);
        assert!(
            !rows.is_empty(),
            "case {family}: a published family reports its catalog, got {}",
            grouped.body
        );
        let described: BTreeMap<String, String> = reported(&defaults)
            .into_iter()
            .map(|row| {
                (
                    row.name.clone(),
                    row.field("Description").unwrap_or_default().to_string(),
                )
            })
            .collect();
        for row in &rows {
            let case = format!("{family}/{}", row.name);
            let description = row.field("Description").unwrap_or_default();
            assert!(
                !description.is_empty(),
                "case {case}: every published parameter carries a description, got {row:?}"
            );
            assert_eq!(
                described.get(&row.name).map(String::as_str),
                Some(description),
                "case {case}: both reads of one catalog report the same description, got \
                 {described:?}"
            );
            by_parameter
                .entry(row.name.clone())
                .or_default()
                .entry(description.to_string())
                .or_default()
                .push(family);
        }
    }
    for (parameter, descriptions) in &by_parameter {
        assert_eq!(
            descriptions.len(),
            1,
            "case {parameter}: a parameter's description is the parameter's own, so every family \
             publishing it reports one description, got {descriptions:?}"
        );
    }

    // A read does not change what the next caller sees: the second read of a
    // catalog reports exactly the first one's rows.
    let family = "redis7";
    let group = default_group(family);
    let shape = |reply: &Reply| -> Vec<(String, Vec<(String, String)>)> {
        reported(reply)
            .into_iter()
            .map(|row| (row.name.clone(), catalog_metadata(&row)))
            .collect()
    };
    let once = describe_whole(&emulator, &group).await;
    let twice = describe_whole(&emulator, &group).await;
    assert_eq!(
        shape(&once),
        shape(&twice),
        "case {group}: two reads of a published catalog report the same metadata"
    );
}
