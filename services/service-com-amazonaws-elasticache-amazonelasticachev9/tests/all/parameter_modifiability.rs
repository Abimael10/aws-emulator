// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what the published parameter catalog's
//! `Parameter$IsModifiable` column DECIDES.
//!
//! The rule under test: a parameter the published catalog of a group's family
//! marks modifiable is one a customer may set on a group of their own, so
//! `ModifyCacheParameterGroup` accepts a value the row's `AllowedValues` cover
//! and answers 200 with the group's name, `DescribeCacheParameters` reports that
//! value verbatim under `Source` `user`, `ResetCacheParameterGroup` accepts the
//! same parameter and puts it back to the engine's own value under `Source`
//! `system`, and a value the row's published set does NOT cover is refused 400
//! `InvalidParameterValue` naming that set — never the parameter's
//! modifiability. Modifiability is a fact of the catalog row, not a consequence
//! of what the service itself reads the parameter for, so a value the customer
//! sets takes effect on the group: a group whose cluster-mode flag the customer
//! turned on is one no cache cluster may be created with, and resetting it makes
//! the group usable again.
//!
//! Every assertion goes over the wire through the same router the production
//! emulator mounts, reusing the harness in [`crate::harness`], the parameter
//! scanners in [`crate::describe_cache_parameters`] and the writer drivers in
//! [`crate::modify_cache_parameter_group`], so nothing that was not persisted
//! can pass. The rows are taken FROM the published catalog
//! (`state::family_default_parameters`) rather than written out by hand, over
//! families spanning both engine catalogs and four version lines, so every
//! parameter the catalog marks modifiable is a row and the one a recording
//! happens to name is not special.

use service_com_amazonaws_elasticache_amazonelasticachev9::state;

use crate::describe_cache_parameters::{create_group, describe_ok, names, one};
use crate::harness::{params, Emulator, Reply};
use crate::modify_cache_parameter_group::{effective, modify, modify_ok, refused};

/// The families every sweep below is driven over: both engine catalogs and four
/// version lines, from the earliest line that publishes a cluster-mode flag to
/// the latest Valkey one.
const FAMILIES: [&str; 4] = ["redis3.2", "redis7", "valkey9", "memcached1.6"];

/// One published row a customer may set a value for: the parameter's name, the
/// published `AllowedValues` set, its `DataType`, the engine's own default and a
/// value from the set that is NOT that default.
#[derive(Debug)]
struct Settable {
    name: String,
    allowed: String,
    data_type: String,
    default: String,
    value: String,
}

/// Whether `term` of a published `AllowedValues` list is a numeric RANGE
/// (`1-75`, `16384-`) rather than a literal value — the same reading the
/// catalog's own column is written in, where what precedes the `-` being a
/// number is what tells a range from a literal that carries a hyphen
/// (`volatile-lru`).
fn is_range(term: &str) -> bool {
    term.split_once('-')
        .is_some_and(|(low, _)| low.parse::<f64>().is_ok())
}

/// Every parameter of `family` the published catalog marks modifiable and
/// publishes an ENUMERATION of literal values for, each with a value of that
/// enumeration to set.
///
/// Source: the catalog this service publishes
/// (`state::family_default_parameters`), so the sweep covers whatever the
/// catalog carries rather than a list repeated here. The enumerated rows are the
/// ones whose refusal names the published SET, which is the wording asserted
/// below; a row whose set bounds a number is worded differently and is covered
/// by the suite for the writer itself.
fn settable_enumerations(family: &str) -> Vec<Settable> {
    let catalog = state::family_default_parameters(family)
        .unwrap_or_else(|| panic!("case {family}: a published family has a catalog"));
    let mut rows = Vec::new();
    for record in catalog.values() {
        if !record.is_modifiable || record.is_node_type_specific() {
            continue;
        }
        let Some(allowed) = record.allowed_values.as_deref() else {
            continue;
        };
        if allowed.split(',').any(is_range) {
            continue;
        }
        let default = record.value.clone().unwrap_or_default();
        // A row has to move the value, or setting it would prove nothing.
        let Some(value) = allowed.split(',').find(|term| *term != default) else {
            continue;
        };
        rows.push(Settable {
            name: record.name.clone(),
            allowed: allowed.to_string(),
            data_type: record.data_type.clone(),
            default,
            value: value.to_string(),
        });
    }
    assert!(
        !rows.is_empty(),
        "case {family}: the published catalog must carry parameters a customer may set"
    );
    rows
}

/// Ask `ResetCacheParameterGroup` to put `parameters` of `group` back to the
/// engine's own values.
///
/// `ParameterNameValueList`'s member carries the `xmlName` `ParameterNameValue`
/// (`smithy_query(query="shape", shape="ParameterNameValueList")`), which is
/// what an `awsQuery` client indexes the names under; the `ParameterValue` a
/// pair carries is not read by a reset.
async fn reset(emulator: &Emulator, group: &str, parameters: &[&str]) -> Reply {
    let mut form = params(&[("CacheParameterGroupName", group)]);
    for (index, name) in parameters.iter().enumerate() {
        let member = index + 1;
        form.push((
            format!("ParameterNameValues.ParameterNameValue.{member}.ParameterName"),
            (*name).to_string(),
        ));
        form.push((
            format!("ParameterNameValues.ParameterNameValue.{member}.ParameterValue"),
            "ignored-by-a-reset".to_string(),
        ));
    }
    emulator.call("ResetCacheParameterGroup", &form).await
}

/// [`reset`], asserting it was accepted and answered with the name the group is
/// STORED under (`CacheParameterGroupNameMessage` carries that alone).
async fn reset_ok(emulator: &Emulator, case: &str, group: &str, parameters: &[&str]) {
    let reply = reset(emulator, group, parameters).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: ResetCacheParameterGroup must answer 200, got {} {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.text("CacheParameterGroupName"),
        Some(group),
        "case {case}: expected the stored group name {group}, got {}",
        reply.body
    );
}

/// Ask `CreateCacheCluster` for a single-node cluster of `engine` on the
/// parameter group `group`.
async fn create_cluster(
    emulator: &Emulator,
    id: &str,
    engine: &str,
    group: &str,
) -> Reply {
    emulator
        .call(
            "CreateCacheCluster",
            &params(&[
                ("CacheClusterId", id),
                ("Engine", engine),
                ("CacheNodeType", "cache.m5.large"),
                ("NumCacheNodes", "1"),
                ("CacheParameterGroupName", group),
            ]),
        )
        .await
}

/// The names `DescribeCacheParameters` reports for `group` under `source`.
async fn under_source(emulator: &Emulator, group: &str, source: &str) -> Vec<String> {
    names(&describe_ok(emulator, group, &[("Source", source), ("MaxRecords", "100")]).await)
}

#[tokio::test]
async fn every_parameter_the_catalog_marks_modifiable_is_set_read_back_and_reset() {
    let emulator = Emulator::new();

    for (index, family) in FAMILIES.into_iter().enumerate() {
        let group = format!("modifiable-{index}");
        create_group(&emulator, &group, family).await;
        let rows = settable_enumerations(family);

        // A family that can run cluster mode publishes the cluster-mode flag,
        // so that flag is one of the rows below rather than a parameter of its
        // own kind.
        if state::family_publishes_cluster_mode(family) {
            assert!(
                rows.iter()
                    .any(|row| row.name == state::CLUSTER_ENABLED_PARAMETER),
                "case {family}: the cluster-mode flag must be a parameter a customer may set, \
                 got {:?}",
                rows.iter().map(|row| &row.name).collect::<Vec<_>>()
            );
        }

        for row in &rows {
            let case = format!("{family}/{}={}", row.name, row.value);

            // Nothing is the customer's before the write.
            let before = under_source(&emulator, &group, state::PARAMETER_SOURCE_USER).await;
            assert!(
                !before.contains(&row.name),
                "case {case}: nothing has been set yet, so the user source reports nothing, got \
                 {before:?}"
            );

            modify_ok(&emulator, &case, &group, &[(&row.name, &row.value)]).await;

            // The user-source read is how a client confirms its own write: the
            // parameter is reported there, with the value sent verbatim and the
            // catalog metadata of its row.
            let selected = under_source(&emulator, &group, state::PARAMETER_SOURCE_USER).await;
            assert!(
                selected.contains(&row.name),
                "case {case}: a parameter the customer set is reported under the user source, \
                 got {selected:?}"
            );
            let reply = describe_ok(
                &emulator,
                &group,
                &[
                    ("Source", state::PARAMETER_SOURCE_USER),
                    ("MaxRecords", "100"),
                ],
            )
            .await;
            let reported_one = one(&reply, &case, &row.name);
            assert_eq!(
                reported_one.field("ParameterValue"),
                Some(row.value.as_str()),
                "case {case}: expected the value that was set verbatim, got {:?}",
                reported_one.field("ParameterValue")
            );
            assert_eq!(
                reported_one.field("Source"),
                Some(state::PARAMETER_SOURCE_USER),
                "case {case}: a value the customer set is theirs, got {:?}",
                reported_one.field("Source")
            );
            assert_eq!(
                reported_one.field("IsModifiable"),
                Some("true"),
                "case {case}: the published row marks it modifiable, got {:?}",
                reported_one.field("IsModifiable")
            );
            assert_eq!(
                reported_one.field("AllowedValues"),
                Some(row.allowed.as_str()),
                "case {case}: expected the published set {}, got {:?}",
                row.allowed,
                reported_one.field("AllowedValues")
            );
            assert_eq!(
                reported_one.field("DataType"),
                Some(row.data_type.as_str()),
                "case {case}: expected the published data type {}, got {:?}",
                row.data_type,
                reported_one.field("DataType")
            );

            // And back to the engine's own value: the same parameter is reset,
            // after which the group reports the catalog's default as the
            // service's and the user source reports nothing again.
            reset_ok(&emulator, &case, &group, &[row.name.as_str()]).await;
            let (value, source) = effective(&emulator, &case, &group, &row.name).await;
            assert_eq!(
                (value.as_str(), source.as_str()),
                (row.default.as_str(), state::PARAMETER_SOURCE_SYSTEM),
                "case {case}: a reset parameter reports the engine's own value, got \
                 {value}/{source}"
            );
            let after = under_source(&emulator, &group, state::PARAMETER_SOURCE_USER).await;
            assert!(
                !after.contains(&row.name),
                "case {case}: a reset parameter is no longer the customer's, got {after:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_value_outside_a_published_enumeration_is_refused_with_that_set() {
    let emulator = Emulator::new();

    for (index, family) in FAMILIES.into_iter().enumerate() {
        let group = format!("outside-{index}");
        create_group(&emulator, &group, family).await;

        for row in settable_enumerations(family) {
            // A boolean parameter's values ARE its published set, so no value
            // outside the set is a value of the type at all and the refusal
            // names the TYPE instead; those rows are covered by the suite for
            // the writer itself.
            if row.data_type == state::PARAMETER_DATA_TYPE_BOOLEAN {
                continue;
            }
            // A value the set does not carry, of the parameter's own data type:
            // a word for a string parameter, and a number the enumeration omits
            // for a numeric one.
            let outside = if row.data_type == state::PARAMETER_DATA_TYPE_STRING {
                "not-a-published-value".to_string()
            } else {
                (0..)
                    .map(|candidate: i64| candidate.to_string())
                    .find(|candidate| !row.allowed.split(',').any(|term| term == candidate))
                    .expect("a finite enumeration omits some number")
            };
            let case = format!("{family}/{}={outside}", row.name);

            let reply = modify(&emulator, &group, &[(&row.name, &outside)]).await;
            let message = refused(&case, &reply, 400, "InvalidParameterValue");
            assert_eq!(
                message,
                format!("invalid parameter value, allowed values are:{}", row.allowed),
                "case {case}: the refusal must name the published set, got {message:?}"
            );

            // And the refused write changed nothing: the group still reports the
            // engine's own value.
            let (value, source) = effective(&emulator, &case, &group, &row.name).await;
            assert_eq!(
                (value.as_str(), source.as_str()),
                (row.default.as_str(), state::PARAMETER_SOURCE_SYSTEM),
                "case {case}: a refused write must change nothing, got {value}/{source}"
            );
        }
    }
}

#[tokio::test]
async fn turning_cluster_mode_on_is_a_change_the_service_acts_on() {
    let emulator = Emulator::new();

    // The axis: the family whose cluster-mode flag is turned on, over three
    // version lines of both Redis OSS-compatible naming conventions. A group's
    // parameters are its family's ENGINE's, so each row creates its clusters on
    // that engine.
    for (index, family) in ["redis3.2", "redis7", "valkey9"].into_iter().enumerate() {
        let group = format!("cluster-mode-{index}");
        let engine = state::family_engine(family).to_string();
        create_group(&emulator, &group, family).await;

        // With cluster mode off — the engine's own default — the group is one a
        // cache cluster may be created with.
        let before =
            create_cluster(&emulator, &format!("mode-off-{index}"), &engine, &group).await;
        assert_eq!(
            before.status, 200,
            "case {family}: a group with cluster mode off may create a cluster, got {} {}",
            before.status, before.body
        );

        // Turning it on is a change the customer makes through the catalog.
        let case = format!("{family}/cluster mode on");
        modify_ok(
            &emulator,
            &case,
            &group,
            &[(
                state::CLUSTER_ENABLED_PARAMETER,
                state::CLUSTER_ENABLED_ON,
            )],
        )
        .await;
        let (value, source) =
            effective(&emulator, &case, &group, state::CLUSTER_ENABLED_PARAMETER).await;
        assert_eq!(
            (value.as_str(), source.as_str()),
            (state::CLUSTER_ENABLED_ON, state::PARAMETER_SOURCE_USER),
            "case {case}: the group must report the flag the customer set, got {value}/{source}"
        );

        // And the service acts on it: "You cannot use any parameter group which
        // has cluster-enabled='yes' when creating a cluster"
        // (`CreateCacheCluster$CacheParameterGroupName`).
        let refused_create =
            create_cluster(&emulator, &format!("mode-on-{index}"), &engine, &group).await;
        assert_eq!(
            (refused_create.status, refused_create.error_code()),
            (400, "InvalidParameterValue"),
            "case {case}: a cluster-mode group may create no cluster, got {} {}",
            refused_create.status,
            refused_create.body
        );
        assert_eq!(
            refused_create.error_message(),
            format!(
                "Invalid value {group} for CacheParameterGroupName: a parameter group with {}='{}' \
                 cannot be used to create a cluster.",
                state::CLUSTER_ENABLED_PARAMETER,
                state::CLUSTER_ENABLED_ON
            ),
            "case {case}: unexpected refusal, got {}",
            refused_create.body
        );

        // Putting the flag back to the engine's own value makes the group one a
        // cluster may be created with again.
        reset_ok(
            &emulator,
            &case,
            &group,
            &[state::CLUSTER_ENABLED_PARAMETER],
        )
        .await;
        let (value, source) =
            effective(&emulator, &case, &group, state::CLUSTER_ENABLED_PARAMETER).await;
        assert_eq!(
            (value.as_str(), source.as_str()),
            ("no", state::PARAMETER_SOURCE_SYSTEM),
            "case {case}: the reset flag is the engine's own value again, got {value}/{source}"
        );
        let after =
            create_cluster(&emulator, &format!("mode-back-off-{index}"), &engine, &group).await;
        assert_eq!(
            after.status, 200,
            "case {case}: the group may create a cluster again, got {} {}",
            after.status, after.body
        );
    }
}
