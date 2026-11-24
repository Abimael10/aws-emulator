// SPDX-License-Identifier: CC0-1.0

//! What a deployment pattern VERSION publishes about its OPTIONAL deployment
//! capabilities — the `featureFlags` block both version views carry
//! (`GetDeploymentPatternVersion` and the page `ListDeploymentPatternVersions` serves),
//! driven over HTTP through the same router the production emulator serves with (the
//! harness in `tests/all/harness.rs`) and, for the branches the seeded catalogue does not
//! itself publish, over the projection the two views report from.
//!
//! The rule under test: a version's capability block is the version's OWN published
//! record. Each capability's status and its configuration are read off the record the
//! catalogue carries for THAT version — never fixed for every version of the catalogue
//! and never read off which operations this emulator happens to serve — and the update
//! path the block publishes follows the modify capability the version publishes: a
//! version a deployment of which is not modified after it was created publishes no
//! version to move a deployment onto, and one that is publishes the versions the single
//! update-target rule allows from it, in catalogue order.
//!
//! Every assertion runs over a table rather than one call: the HTTP rows span every
//! workload of the catalogue, every pattern of each and every version each publishes,
//! and the projection rows span the capability records a version can publish — all
//! unavailable, all available, one capability available, a capability publishing a
//! configuration with fields and one publishing none, and a pattern publishing a single
//! version. That both views report ONE block, and that reporting it changes nothing a
//! later caller sees, are asserted by `crate::list_deployment_pattern_versions`.

use std::collections::BTreeSet;

use serde_json::{json, Map, Value};
use service_com_amazonaws_launchwizard_launchwizard::state::pattern_version_capabilities::{
    feature_flags, published_capabilities, VersionCapabilities, VersionCapability,
    CAPABILITY_DISABLED, CAPABILITY_ENABLED,
};
use service_com_amazonaws_launchwizard_launchwizard::state::{
    DeploymentPattern, PatternVersion, CATALOGUE_PATTERN_VERSIONS, PATTERN_ACTIVE,
};

use crate::deployment_pattern_versions::get_version;
use crate::deployment_update::every_pattern;
use crate::harness::Harness;

/// The member a version's optional-capability block travels on.
const FLAGS: &str = "featureFlags";

/// The member the published update path travels on, named like the
/// `DeploymentPatternVersionFilterKey` that asks the same question of a starting version.
const PATH: &str = "updateFromVersion";

/// One capability as the wire carries it: its status, and its configuration where the
/// record publishes one.
fn capability_json(capability: &VersionCapability) -> Value {
    let mut object = Map::new();
    if let Some(fields) = capability.configuration_fields.as_ref() {
        object.insert("configuration".to_string(), json!({"fields": fields}));
    }
    object.insert(
        "status".to_string(),
        json!(if capability.available {
            CAPABILITY_ENABLED
        } else {
            CAPABILITY_DISABLED
        }),
    );
    Value::Object(object)
}

/// The whole block a version publishing `record` reports, with `path` as its update path.
fn flags_json(record: &VersionCapabilities, path: &[String]) -> Value {
    json!({
        "cloudFormationOutput": capability_json(&record.cloud_formation_output),
        "modifyDeployments": capability_json(&record.modify_deployments),
        "postDeploymentSteps": capability_json(&record.post_deployment_steps),
        PATH: path,
    })
}

/// The update path a version publishing `record` publishes, over a pattern that publishes
/// `versions`: nothing at all where a deployment of it is not modified after it was
/// created, and otherwise every OTHER version the pattern publishes, in the order it
/// publishes them.
fn expected_path(record: &VersionCapabilities, versions: &[&str], version: &str) -> Vec<String> {
    if !record.modify_deployments.available {
        return Vec::new();
    }
    versions
        .iter()
        .filter(|published| **published != version)
        .map(|published| (*published).to_string())
        .collect()
}

#[tokio::test]
async fn the_version_read_reports_the_capability_record_the_catalogue_carries() {
    let harness = Harness::new();
    // The record the catalogue carries for a version of the patterns it publishes: the
    // statuses the block must state, read from the catalogue itself rather than written
    // out here, so a seed that publishes another capability state is asserted as it
    // stands instead of against a copy of today's values. That the PAGE reports a version
    // exactly as this read does, and that a second read reports the same block, are
    // asserted by `crate::list_deployment_pattern_versions`.
    let record = published_capabilities();
    for (workload, pattern) in every_pattern(&harness).await {
        for version in CATALOGUE_PATTERN_VERSIONS {
            let case = format!("{workload}/{pattern}@{version}");
            let expected = flags_json(
                &record,
                &expected_path(&record, CATALOGUE_PATTERN_VERSIONS, version),
            );

            let read = get_version(&harness, &case, &workload, &pattern, version).await;
            let from_read = read.get(FLAGS).cloned().unwrap_or(Value::Null);
            assert_eq!(
                from_read, expected,
                "case {case:?}: expected the capability block {expected}, got {from_read}"
            );
        }
    }
}

/// A capability that is available and publishes no configuration of its own.
fn available() -> VersionCapability {
    VersionCapability::available()
}

/// A capability that is unavailable and publishes no configuration of its own.
fn unavailable() -> VersionCapability {
    VersionCapability::unavailable()
}

#[test]
fn the_seeded_catalogue_publishes_no_optional_capability() {
    let record = published_capabilities();
    // What a client gates on: none of the three optional capabilities is available for a
    // version of the patterns this catalogue publishes, so none of them is offered to an
    // operator and the update path that follows the modify capability is empty.
    for (capability, published) in [
        ("cloudFormationOutput", &record.cloud_formation_output),
        ("modifyDeployments", &record.modify_deployments),
        ("postDeploymentSteps", &record.post_deployment_steps),
    ] {
        assert!(
            !published.available,
            "case {capability:?}: the catalogue publishes this capability as available, \
             where its versions publish none"
        );
    }
    // The capability that is configurable at all publishes its configuration block with
    // nothing to configure through; the other two publish no block.
    assert_eq!(
        record.cloud_formation_output.configuration_fields,
        Some(Vec::new()),
        "case \"cloudFormationOutput\": expected a published configuration naming no \
         field, got {:?}",
        record.cloud_formation_output.configuration_fields
    );
    for (capability, published) in [
        ("modifyDeployments", &record.modify_deployments),
        ("postDeploymentSteps", &record.post_deployment_steps),
    ] {
        assert_eq!(
            published.configuration_fields, None,
            "case {capability:?}: expected no published configuration block, got {:?}",
            published.configuration_fields
        );
    }
}

/// One deployment pattern publishing `versions`, every one of them carrying `record`.
fn pattern_publishing(versions: &[&str], record: &VersionCapabilities) -> DeploymentPattern {
    DeploymentPattern {
        workload_name: "TestWorkload".to_string(),
        pattern_name: "TestPattern".to_string(),
        display_name: "A pattern under test".to_string(),
        description: "A pattern whose versions publish the capability record under test."
            .to_string(),
        status: PATTERN_ACTIVE.to_string(),
        status_message: None,
        workload_version_name: versions[0].to_string(),
        versions: versions
            .iter()
            .map(|version| PatternVersion {
                version_name: (*version).to_string(),
                description: format!("Version {version} of the pattern under test."),
                capabilities: record.clone(),
            })
            .collect(),
        specifications: Vec::new(),
    }
}

#[test]
fn the_block_states_the_record_the_version_carries_and_a_path_that_follows_it() {
    // Rows differ along what the rule generalises over: which capabilities a version
    // publishes, whether a capability publishes a configuration and whether that
    // configuration names fields, and how many versions the pattern publishes.
    let rows: Vec<(&str, Vec<&str>, VersionCapabilities)> = vec![
        (
            "the record the seeded catalogue carries",
            CATALOGUE_PATTERN_VERSIONS.to_vec(),
            published_capabilities(),
        ),
        (
            "every capability available, none configurable",
            CATALOGUE_PATTERN_VERSIONS.to_vec(),
            VersionCapabilities {
                cloud_formation_output: available(),
                modify_deployments: available(),
                post_deployment_steps: available(),
            },
        ),
        (
            "only the modify capability available",
            CATALOGUE_PATTERN_VERSIONS.to_vec(),
            VersionCapabilities {
                cloud_formation_output: unavailable(),
                modify_deployments: available(),
                post_deployment_steps: unavailable(),
            },
        ),
        (
            "an available capability configured through named fields",
            CATALOGUE_PATTERN_VERSIONS.to_vec(),
            VersionCapabilities {
                cloud_formation_output: available()
                    .configured_through(vec!["StackName".to_string(), "OutputKey".to_string()]),
                modify_deployments: available(),
                post_deployment_steps: unavailable().configured_through(Vec::new()),
            },
        ),
        (
            "a pattern publishing a single version, modifiable",
            vec![CATALOGUE_PATTERN_VERSIONS[0]],
            VersionCapabilities {
                cloud_formation_output: unavailable(),
                modify_deployments: available(),
                post_deployment_steps: unavailable(),
            },
        ),
        (
            "a pattern publishing a single version, not modifiable",
            vec![CATALOGUE_PATTERN_VERSIONS[0]],
            published_capabilities(),
        ),
    ];

    for (label, versions, record) in rows {
        let pattern = pattern_publishing(&versions, &record);
        for version in &pattern.versions {
            let case = format!("{label}: {}", version.version_name);
            let flags = feature_flags(&pattern, version);

            // Each capability states the status its own record publishes, and its
            // configuration exactly where the record publishes one.
            for (capability, published, got) in [
                (
                    "cloudFormationOutput",
                    &record.cloud_formation_output,
                    &flags.cloud_formation_output,
                ),
                (
                    "modifyDeployments",
                    &record.modify_deployments,
                    &flags.modify_deployments,
                ),
                (
                    "postDeploymentSteps",
                    &record.post_deployment_steps,
                    &flags.post_deployment_steps,
                ),
            ] {
                let expected_status = if published.available {
                    CAPABILITY_ENABLED
                } else {
                    CAPABILITY_DISABLED
                };
                assert_eq!(
                    got.status, expected_status,
                    "case {case:?}: capability {capability:?} must state the status its \
                     record publishes ({expected_status}), got {:?}",
                    got.status
                );
                let expected_fields = published.configuration_fields.clone();
                let got_fields = got
                    .configuration
                    .as_ref()
                    .map(|configuration| configuration.fields.clone());
                assert_eq!(
                    got_fields, expected_fields,
                    "case {case:?}: capability {capability:?} must publish the \
                     configuration its record carries {expected_fields:?}, got \
                     {got_fields:?}"
                );
            }

            // The update path follows the modify capability: nothing where a deployment
            // of the version is not modified after it was created, and otherwise the
            // versions an update moves a deployment on this one onto, in the order the
            // pattern publishes them.
            let expected = expected_path(&record, &versions, &version.version_name);
            assert_eq!(
                flags.update_from_version, expected,
                "case {case:?}: expected the published update path {expected:?}, got {:?}",
                flags.update_from_version
            );
            // Whatever the path names is a version this pattern publishes, and never the
            // version itself.
            let published: BTreeSet<&str> = versions.iter().copied().collect();
            for named in &flags.update_from_version {
                assert!(
                    published.contains(named.as_str()) && *named != version.version_name,
                    "case {case:?}: the path must name another version this pattern \
                     publishes, got {named:?}"
                );
            }
        }
    }
}
