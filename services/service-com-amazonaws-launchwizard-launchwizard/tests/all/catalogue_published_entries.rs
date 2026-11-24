// SPDX-License-Identifier: CC0-1.0

//! Cover for the ENTRIES the service-owned workload catalogue publishes, driven over
//! HTTP through the same router the production emulator serves with.
//!
//! RULE: the catalogue publishes one entry per workload Launch Wizard deploys, under the
//! workload and deployment-pattern NAMES AWS publishes them under, and a name it
//! publishes resolves wherever a name is taken: the listing's entries are served by the
//! per-workload pattern listing, the pattern read and a create, and the refusal is
//! reserved for a name the catalogue does not publish at all. Every summary it reports
//! carries EVERY member the projection models — none of them dropped, and none beyond
//! them.
//!
//! The rows span the WHOLE catalogue rather than the entries the report cited: every
//! workload it publishes (differing in how many patterns they offer, whether the pattern
//! launches EC2 instances of the caller's own, and so in the schema each publishes), and
//! every deployment pattern of each. The names the table names by hand are read off the
//! AWS catalogue (see [`AWS_PUBLISHED`]) and include entries the report did not cite.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use service_com_amazonaws_launchwizard_launchwizard::ops::types::{
    WORKLOAD_DEPLOYMENT_PATTERN_STATUS_VALUES, WORKLOAD_STATUS_VALUES,
};

use crate::deployment_lifecycle::{create, get, required_specifications};
use crate::deployment_update::{every_pattern, pattern_data};
use crate::harness::{catalogue, Harness};
use crate::resource_tagging::model;

/// The route the model binds each catalogue read to.
const PATTERNS_PATH: &str = "/listWorkloadDeploymentPatterns";
/// See [`PATTERNS_PATH`].
const WORKLOADS_PATH: &str = "/listWorkloads";

/// The HTTP status and wire error code a lookup of a workload the catalogue does not
/// publish carries (`ResourceNotFoundException`, `errorKind: client`, `httpError: 404`).
const STATUS: u16 = 404;
/// See [`STATUS`].
const CODE: &str = "ResourceNotFoundException";

/// `workloadName`s AWS's own Launch Wizard catalogue publishes, each with the deployment
/// pattern names AWS publishes inside it — the ONE table this crate states that in,
/// `crate::catalogue_entry_identity::PUBLISHED_PATTERNS`, so the two suites that read the
/// published set cannot drift apart over which patterns an entry offers.
///
/// There it is the WHOLE set per entry, which is what that suite asserts; here the rows
/// are read for the half this suite is about — a name AWS publishes is a name this
/// catalogue publishes, so a client that takes one from the real service addresses the
/// same entry — while the display text beside those names is the emulator's own and is
/// only required to be non-empty below.
use crate::catalogue_entry_identity::PUBLISHED_PATTERNS as AWS_PUBLISHED;

/// The image-transformation solution entry of the catalogue, and how many deployment
/// patterns it publishes: one per architecture that solution offers (an Amazon ECS one and
/// an AWS Lambda one), which is what the recorded catalogue attests about it — so an entry
/// that grew a third architecture, or lost one, is observable here beside the two names
/// [`AWS_PUBLISHED`] holds.
const SOLUTION_PATTERNS: (&str, usize) = ("SO0023", 2);

/// `workloadName`s no catalogue entry is published under, each well-formed for
/// `WorkloadName` (@length(min: 1, max: 100), @pattern `^[A-Za-z][a-zA-Z0-9-_]*$` — from
/// smithy_query(query="shape", shape="WorkloadName")), spanning both published length
/// bounds and the character forms the shape admits.
fn unpublished_workloads() -> Vec<(&'static str, String)> {
    vec![
        ("plain unknown", "NotAPublishedWorkload".to_string()),
        ("minimum length 1", "Q".to_string()),
        ("maximum length 100", format!("Z{}", "y".repeat(99))),
        ("underscore and hyphen", "Not_A-Workload9".to_string()),
        // A solution id the catalogue does not publish, so an entry's KIND is not what
        // decides the answer: the name being published is.
        ("another solution id", "SO9999".to_string()),
    ]
}

/// Every member name, as it travels on the wire, that `shape` declares.
///
/// Read off `smithy-model/` rather than written out here, so the "every modelled member
/// is reported" half of the rule cannot drift from what the projection actually models —
/// nor be asserted for a member the model does not declare.
fn modelled_members(model: &Value, shape: &str) -> BTreeSet<String> {
    let declared = model["shapes"][format!("com.amazonaws.launchwizard#{shape}")]["members"]
        .as_object()
        .unwrap_or_else(|| panic!("the model must declare the members of {shape}"))
        .clone();
    declared
        .iter()
        .map(|(name, member)| {
            // `@jsonName` renames a member on the wire; without one it travels as
            // declared.
            member["traits"]["smithy.api#jsonName"]
                .as_str()
                .unwrap_or(name)
                .to_string()
        })
        .collect()
}

/// The members one reported summary carries on the wire.
fn reported_members(case: &str, summary: &Value) -> BTreeSet<String> {
    summary
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: a reported summary must be an object: {summary}"))
        .iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(name, _)| name.clone())
        .collect()
}

/// The `workloadDeploymentPatterns` the per-workload listing reports for `workload`,
/// requiring the served answer the rule gives a published name.
async fn published_patterns(harness: &Harness, workload: &str) -> Vec<Value> {
    let body = harness
        .ok(workload, PATTERNS_PATH, json!({"workloadName": workload}))
        .await;
    let patterns = body["workloadDeploymentPatterns"]
        .as_array()
        .unwrap_or_else(|| panic!("case {workload:?}: no workloadDeploymentPatterns: {body}"))
        .clone();
    assert!(
        !patterns.is_empty(),
        "case {workload:?}: a published workload must offer at least one deployment \
         pattern, got {body}"
    );
    patterns
}

#[tokio::test]
async fn every_name_the_catalogue_publishes_is_served_and_only_an_unpublished_one_refused() {
    let harness = Harness::new();
    let names = catalogue(&harness).await;
    assert!(
        names.len() >= 2,
        "the catalogue must hold more than one workload for this table to span it, got {names:?}"
    );

    // The names AWS publishes are the names this catalogue publishes, so a client that
    // takes one from the real service addresses the same entry here.
    for (workload, patterns) in AWS_PUBLISHED {
        assert!(
            names.iter().any(|name| name == workload),
            "case {workload:?}: the catalogue must publish this AWS workload name, got {names:?}"
        );
        let published: BTreeSet<String> = published_patterns(&harness, workload)
            .await
            .iter()
            .map(|summary| {
                summary["deploymentPatternName"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {workload:?}: a pattern carries no name"))
                    .to_string()
            })
            .collect();
        for pattern in patterns {
            assert!(
                published.contains(*pattern),
                "case {workload:?}/{pattern:?}: the catalogue must publish this AWS \
                 deployment pattern name, got {published:?}"
            );
        }
    }

    // The solution entry publishes one pattern per architecture the solution offers,
    // which is what is attested about it — the names of those two are the emulator's.
    let (solution, architectures) = SOLUTION_PATTERNS;
    let published = published_patterns(&harness, solution).await;
    assert_eq!(
        published.len(),
        architectures,
        "case {solution:?}: the solution entry must publish one pattern per architecture \
         it offers, got {published:?}"
    );

    // Every entry of the catalogue — not only the ones named above — is served by the
    // per-workload listing, and every summary it reports is a pattern OF that workload
    // which the pattern read resolves and reports the same values for.
    for workload in &names {
        for summary in published_patterns(&harness, workload).await {
            let pattern = summary["deploymentPatternName"]
                .as_str()
                .unwrap_or_else(|| panic!("case {workload:?}: a pattern carries no name"))
                .to_string();
            let case = format!("{workload}/{pattern}");
            assert_eq!(
                summary["workloadName"], json!(workload),
                "case {case:?}: a reported pattern must name the workload it was listed \
                 for, got {}",
                summary["workloadName"]
            );

            // `DeploymentPatternName`: len 1..256, `^[A-Za-z0-9][a-zA-Z0-9-]*$`.
            assert!(
                (1..=256).contains(&pattern.len())
                    && pattern.starts_with(|c: char| c.is_ascii_alphanumeric())
                    && pattern
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "case {case:?}: a published pattern name must match \
                 ^[A-Za-z0-9][a-zA-Z0-9-]*$ with len 1..256, got {pattern:?}"
            );

            // The pattern read resolves the listed name and reports the same record, so a
            // name taken out of the listing is usable in the next call of the documented
            // discovery flow.
            let data = pattern_data(&harness, &case, workload, &pattern).await;
            for member in [
                "workloadName",
                "deploymentPatternName",
                "workloadVersionName",
                "deploymentPatternVersionName",
                "displayName",
                "description",
                "status",
                "accountConstraints",
            ] {
                assert_eq!(
                    data[member], summary[member],
                    "case {case:?}: the pattern read's {member} must match the listing's, \
                     got {} vs {}",
                    data[member], summary[member]
                );
            }
        }
    }

    // And the refusal is reserved for a name the catalogue does not publish, so serving
    // every published entry did not turn the lookup into one that serves anything.
    for (label, absent) in unpublished_workloads() {
        harness
            .refused(
                &format!("unpublished workload ({label})"),
                PATTERNS_PATH,
                json!({"workloadName": absent}),
                STATUS,
                CODE,
            )
            .await;
    }
}

#[tokio::test]
async fn every_catalogue_summary_carries_the_members_its_projection_models() {
    let harness = Harness::new();
    let model = model();

    // `statusMessage` is the one modelled member a catalogue entry in a steady status has
    // nothing to put in, so it is reported by being left out; every other modelled member
    // is carried. The sets themselves are read off the model, so a member added to a
    // projection is covered here without this suite being edited.
    let workload_members = modelled_members(&model, "WorkloadDataSummary");
    let pattern_members = modelled_members(&model, "WorkloadDeploymentPatternDataSummary");
    let status_message: BTreeSet<String> = ["statusMessage".to_string()].into_iter().collect();

    let listed = harness.ok("listWorkloads", WORKLOADS_PATH, json!({})).await;
    let workloads = listed["workloads"]
        .as_array()
        .unwrap_or_else(|| panic!("ListWorkloads must report a workloads list: {listed}"))
        .clone();
    assert!(
        workloads.len() >= 2,
        "the catalogue must report more than one workload summary, got {listed}"
    );

    for summary in &workloads {
        let case = summary["workloadName"].as_str().unwrap_or_default().to_string();
        assert_eq!(
            reported_members(&case, summary),
            workload_members
                .difference(&status_message)
                .cloned()
                .collect::<BTreeSet<String>>(),
            "case {case:?}: a workload summary must carry every member \
             WorkloadDataSummary models and nothing beyond them, got {summary}"
        );
        let status = summary["status"].as_str().unwrap_or_default();
        assert!(
            WORKLOAD_STATUS_VALUES.contains(&status),
            "case {case:?}: status {status:?} is not a WorkloadStatus value"
        );
        assert!(
            summary["displayName"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "case {case:?}: every entry must carry a non-empty displayName, got {}",
            summary["displayName"]
        );
    }

    for workload in catalogue(&harness).await {
        for summary in published_patterns(&harness, &workload).await {
            let pattern = summary["deploymentPatternName"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            let case = format!("{workload}/{pattern}");
            assert_eq!(
                reported_members(&case, &summary),
                pattern_members
                    .difference(&status_message)
                    .cloned()
                    .collect::<BTreeSet<String>>(),
                "case {case:?}: a pattern summary must carry every member \
                 WorkloadDeploymentPatternDataSummary models and nothing beyond them, got \
                 {summary}"
            );

            let status = summary["status"].as_str().unwrap_or_default();
            assert!(
                WORKLOAD_DEPLOYMENT_PATTERN_STATUS_VALUES.contains(&status),
                "case {case:?}: status {status:?} is not a WorkloadDeploymentPatternStatus \
                 value"
            );

            // A deployment pattern is published AT a version of its workload, so the two
            // version members of one entry carry one value between them — and each is
            // inside `WorkloadVersionName` / `DeploymentPatternVersionName` (len 5..30).
            let workload_version = summary["workloadVersionName"].as_str().unwrap_or_default();
            let pattern_version = summary["deploymentPatternVersionName"]
                .as_str()
                .unwrap_or_default();
            assert_eq!(
                workload_version, pattern_version,
                "case {case:?}: the entry's workload version and deployment pattern \
                 version must agree, got {workload_version:?} vs {pattern_version:?}"
            );
            for version in [workload_version, pattern_version] {
                assert!(
                    (5..=30).contains(&version.len()),
                    "case {case:?}: a published version name must fit len 5..30, got \
                     {version:?}"
                );
            }

            // `accountConstraints`' list admits zero elements, so an entry no constraint
            // applies to reports the EMPTY list rather than leaving the member out.
            assert_eq!(
                summary["accountConstraints"], json!([]),
                "case {case:?}: accountConstraints must be the empty list for an entry no \
                 account constraint applies to, got {}",
                summary["accountConstraints"]
            );
        }
    }
}

#[tokio::test]
async fn every_published_pattern_backs_a_deployment_created_through_the_discovery_flow() {
    let harness = Harness::new();

    // One row per (workload, pattern) the catalogue publishes — the pairs the discovery
    // flow hands a client — so an entry that lists but cannot be deployed against is
    // observable. The rows differ in the schema the pattern publishes, which is what the
    // create fills in.
    let pairs = every_pattern(&harness).await;
    for (index, (workload, pattern)) in pairs.iter().enumerate() {
        let case = format!("{workload}/{pattern}");
        let specifications = required_specifications(&harness, workload, pattern).await;
        assert!(
            !specifications.is_empty(),
            "case {case:?}: the published schema must ask for at least one setting"
        );
        let id = create(
            &harness,
            &case,
            workload,
            pattern,
            &format!("published-entry-{index}"),
        )
        .await;

        // Read back: the deployment is stored against the catalogue pair the listing
        // handed out, so the whole flow (ListWorkloads ->
        // ListWorkloadDeploymentPatterns -> GetWorkloadDeploymentPattern ->
        // CreateDeployment -> GetDeployment) closes on the same two names.
        let deployment = get(&harness, &case, &id).await;
        assert_eq!(
            deployment["workloadName"], json!(workload),
            "case {case:?}: the stored workloadName must be the listed one, got {}",
            deployment["workloadName"]
        );
        assert_eq!(
            deployment["patternName"], json!(pattern),
            "case {case:?}: the stored patternName must be the listed one, got {}",
            deployment["patternName"]
        );
        assert_eq!(
            deployment["specifications"],
            Value::Object(specifications),
            "case {case:?}: the stored settings must be the submitted map, got {}",
            deployment["specifications"]
        );
    }
}
