// SPDX-License-Identifier: CC0-1.0

//! Cover for the IDENTITY a service-owned workload catalogue entry publishes, driven over
//! HTTP through the same router the production emulator serves with.
//!
//! RULE: every value a catalogue read reports about an entry — its display text, the
//! version it is published at, the names of its deployment patterns and the text and
//! schema beside each of those names — is data of THAT entry, read off its own catalogue
//! row and served as it stands; nothing is shared between entries or built out of another
//! entry's name, every entry the catalogue publishes resolves wherever a name is taken,
//! and the not-found refusal is reserved for a name no entry carries.
//!
//! That is what a client does with the catalogue: it reads a workload's display text to
//! offer the workload, takes a `deploymentPatternName` and a version straight to
//! `GetWorkloadDeploymentPattern`, `ListDeploymentPatternVersions` and `CreateDeployment`,
//! and tells two patterns of one entry apart by what their names, text and schemas say.
//! A catalogue that published one version string for every entry, a pattern name built
//! from its workload's, or no pattern for a topology it offers would answer 200 and still
//! be unusable for every one of those steps.
//!
//! The rows span the WHOLE catalogue rather than one cited entry: the application entries
//! and the solution entries, whose pattern sets differ in size (two, three, six and eight
//! patterns), in the form their names take (hyphenated for a solution's own pattern names,
//! CamelCase for an application's) and in the dimensions those names state (application
//! crossed with deployment model, directory type crossed with network topology,
//! architecture).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use service_com_amazonaws_launchwizard_launchwizard::state::catalogue_pattern_versions;

use crate::deployment_update::{pattern_data, schema_fields};
use crate::harness::{catalogue, Harness};

/// The routes the model binds the catalogue reads to.
const PATTERNS_PATH: &str = "/listWorkloadDeploymentPatterns";
/// See [`PATTERNS_PATH`].
const WORKLOADS_PATH: &str = "/listWorkloads";
/// See [`PATTERNS_PATH`].
const WORKLOAD_PATH: &str = "/getWorkload";
/// See [`PATTERNS_PATH`].
const VERSIONS_PATH: &str = "/listDeploymentPatternVersions";
/// See [`PATTERNS_PATH`].
const VERSION_PATH: &str = "/getDeploymentPatternVersion";

/// The HTTP status and wire error code a lookup of a name the catalogue publishes nothing
/// under carries (`ResourceNotFoundException`, `errorKind: client`, `httpError: 404`).
const NOT_FOUND: u16 = 404;
/// See [`NOT_FOUND`].
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The whole pattern set each catalogue entry publishes, as the two catalogue reads report
/// it — the ONE table this crate states the published set in, read here for the whole-set
/// rule and by `crate::catalogue_published_entries` for the "a name AWS publishes resolves
/// here" one.
///
/// Source: the catalogue AWS's Launch Wizard publishes through these same reads — its SAP
/// entry's patterns (each SAP application it deploys crossed with the deployment model it
/// deploys it under), its Microsoft Active Directory entry's (each directory type crossed
/// with the network topology it deploys into), its Microsoft SQL Server entry's, and one
/// pattern per architecture of each AWS Solutions Library solution entry, under the
/// pattern names the solution publishes (`ecs-deployment` and `lambda-deployment` for the
/// image-transformation solution, which is why those are hyphenated where an application
/// entry's names are not).
///
/// The rows are the WHOLE set per entry, not a subset, because a dimension an entry offers
/// and the catalogue does not publish a pattern for is a topology no client can select —
/// and they differ in size (8, 6, 2, 2, 3), so a set collapsed to one pattern per
/// dimension fails here whichever dimension was dropped.
pub(crate) const PUBLISHED_PATTERNS: [(&str, &[&str]); 5] = [
    (
        "SAP",
        &[
            "SapHanaHA",
            "SapHanaMulti",
            "SapHanaSingle",
            "SapNWOnAseHA",
            "SapNWOnAseSingle",
            "SapNWOnHanaHA",
            "SapNWOnHanaMulti",
            "SapNWOnHanaSingle",
        ],
    ),
    (
        "MicrosoftActiveDirectory",
        &[
            "ADExtensionExistingVPC",
            "ADExtensionNewVPC",
            "AWSManagedADExistingVPC",
            "AWSManagedADNewVPC",
            "SelfManagedADExistingVPC",
            "SelfManagedADNewVPC",
        ],
    ),
    ("SQLServer", &["SqlServerHA", "SqlServerSingle"]),
    ("SO0023", &["ecs-deployment", "lambda-deployment"]),
    (
        "SO0062",
        &[
            "existing-vpc-deployment",
            "fargate-deployment",
            "regional-deployment",
        ],
    ),
];

/// The field a pattern that launches instances into a network the CALLER already has
/// publishes for that network, and the one a pattern that CREATES the network publishes
/// instead — the pair the Active Directory entry's topology dimension is observable
/// through. Both are `DeploymentSpecifications` keys (len 3..256, `^[a-zA-Z0-9-:]+$`).
const EXISTING_NETWORK_FIELD: &str = "VpcId";
/// See [`EXISTING_NETWORK_FIELD`].
const NEW_NETWORK_FIELD: &str = "VpcCidr";

/// The suffix a pattern name carries for each network topology of a directory type. See
/// [`EXISTING_NETWORK_FIELD`].
const EXISTING_NETWORK_SUFFIX: &str = "ExistingVPC";
/// See [`EXISTING_NETWORK_SUFFIX`].
const NEW_NETWORK_SUFFIX: &str = "NewVPC";

/// The `workloadDeploymentPatterns` the per-workload listing reports for `workload`,
/// requiring the 200 the rule gives a published name.
async fn patterns_of(harness: &Harness, workload: &str) -> Vec<Value> {
    let body = harness
        .ok(workload, PATTERNS_PATH, json!({"workloadName": workload}))
        .await;
    let patterns = body["workloadDeploymentPatterns"]
        .as_array()
        .unwrap_or_else(|| panic!("case {workload:?}: no workloadDeploymentPatterns: {body}"))
        .clone();
    assert!(
        !patterns.is_empty(),
        "case {workload:?}: a published entry must offer at least one deployment pattern, \
         got {body}"
    );
    patterns
}

/// A string member of a reported record, required to be present and non-empty.
fn text(case: &str, record: &Value, member: &str) -> String {
    let value = record[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: {member} must be reported as text, got {record}")
    });
    assert!(
        !value.is_empty(),
        "case {case:?}: {member} must not be empty, got {record}"
    );
    value.to_string()
}

/// The `deploymentPatternName`s of the reported summaries.
fn pattern_names(case: &str, summaries: &[Value]) -> BTreeSet<String> {
    summaries
        .iter()
        .map(|summary| text(case, summary, "deploymentPatternName"))
        .collect()
}

/// The specification names one pattern publishes, and the ones it marks required.
async fn schema_of(
    harness: &Harness,
    case: &str,
    workload: &str,
    pattern: &str,
) -> (BTreeSet<String>, BTreeSet<String>) {
    let data = pattern_data(harness, case, workload, pattern).await;
    let fields = schema_fields(case, &data);
    let published = fields
        .iter()
        .map(|field| text(case, field, "name"))
        .collect();
    let required = fields
        .iter()
        .filter(|field| field["required"].as_str() == Some("Yes"))
        .map(|field| text(case, field, "name"))
        .collect();
    (published, required)
}

/// Whether `name` is the id an AWS Solutions Library solution entry is published under
/// (`SO` followed by its number), which is the form of a solution's catalogue name.
fn is_solution_id(name: &str) -> bool {
    name.strip_prefix("SO")
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

#[tokio::test]
async fn every_entry_publishes_its_own_version_and_no_version_is_shared_by_the_catalogue() {
    let harness = Harness::new();
    let entries = catalogue(&harness).await;
    assert!(
        entries.len() >= 3,
        "the catalogue must publish enough entries for this table to span it, got {entries:?}"
    );

    // What each entry reports as the version its patterns are published at, collected so
    // the catalogue as a whole can be judged below.
    let mut per_entry: BTreeMap<String, String> = BTreeMap::new();
    for workload in &entries {
        let mut reported: BTreeSet<String> = BTreeSet::new();
        for summary in patterns_of(&harness, workload).await {
            let pattern = text(workload, &summary, "deploymentPatternName");
            let case = format!("{workload}/{pattern}");
            let workload_version = text(&case, &summary, "workloadVersionName");
            let pattern_version = text(&case, &summary, "deploymentPatternVersionName");

            // A deployment pattern is published AT a version of its workload, so one
            // entry's two version members carry one value between them.
            assert_eq!(
                workload_version, pattern_version,
                "case {case:?}: expected one version between the two members, got \
                 {workload_version:?} and {pattern_version:?}"
            );
            reported.insert(workload_version.clone());

            // The pattern read reports the same pair as the listing, and the version
            // listing resolves it as the newest version the pattern publishes — so the
            // version a client takes out of any catalogue view is the same version.
            let data = pattern_data(&harness, &case, workload, &pattern).await;
            assert_eq!(
                text(&case, &data, "workloadVersionName"),
                workload_version,
                "case {case:?}: the pattern read must report the listing's version"
            );
            let versions = harness
                .ok(
                    &case,
                    VERSIONS_PATH,
                    json!({"workloadName": workload, "deploymentPatternName": pattern}),
                )
                .await;
            let listed = versions["deploymentPatternVersions"]
                .as_array()
                .unwrap_or_else(|| panic!("case {case:?}: no version page: {versions}"))
                .clone();
            let newest = text(&case, &listed[0], "deploymentPatternVersionName");
            assert_eq!(
                newest, pattern_version,
                "case {case:?}: the version the pattern reports must be the newest one its \
                 version listing publishes, got {newest:?}"
            );
        }

        // Every pattern of ONE entry is published at that entry's version, so a client
        // that read the version off one pattern can deploy any pattern of the entry.
        assert_eq!(
            reported.len(),
            1,
            "case {workload:?}: every pattern of one entry must be published at the \
             entry's own version, got {reported:?}"
        );
        let version = reported
            .into_iter()
            .next()
            .unwrap_or_else(|| panic!("case {workload:?}: no version was reported"));
        assert_eq!(
            catalogue_pattern_versions(workload).first().copied(),
            Some(version.as_str()),
            "case {workload:?}: the version reported must be the newest of the entry's own \
             published line {:?}, got {version:?}",
            catalogue_pattern_versions(workload)
        );
        per_entry.insert(workload.clone(), version);
    }

    // The catalogue does not publish ONE version for every entry: entries version
    // independently, so a client cannot read one entry's version off another.
    let distinct: BTreeSet<&String> = per_entry.values().collect();
    assert!(
        distinct.len() >= 2,
        "the catalogue must publish a version per entry rather than one shared by all of \
         them, got {per_entry:?}"
    );

    // And a version line belongs to its entry: a version another entry publishes, which
    // this one does not, resolves nowhere in this one.
    for workload in per_entry.keys() {
        let own: BTreeSet<&str> = catalogue_pattern_versions(workload).iter().copied().collect();
        let foreign: Vec<&str> = per_entry
            .keys()
            .filter(|other| *other != workload)
            .flat_map(|other| catalogue_pattern_versions(other).iter().copied())
            .filter(|version| !own.contains(version))
            .collect();
        if foreign.is_empty() {
            continue;
        }
        let pattern = text(
            workload,
            &patterns_of(&harness, workload).await[0],
            "deploymentPatternName",
        );
        for version in foreign {
            let case = format!("{workload}/{pattern}@{version} (another entry's version)");
            harness
                .refused(
                    &case,
                    VERSION_PATH,
                    json!({
                        "workloadName": workload,
                        "deploymentPatternName": pattern,
                        "deploymentPatternVersionName": version,
                    }),
                    NOT_FOUND,
                    NOT_FOUND_CODE,
                )
                .await;
        }
    }
}

#[tokio::test]
async fn every_published_entry_serves_its_whole_pattern_set_and_only_unpublished_names_refused() {
    let harness = Harness::new();
    let entries = catalogue(&harness).await;

    // Every entry of the table is published, and publishes exactly the pattern set beside
    // it — no pattern missing, none the catalogue invented over it.
    for (workload, expected) in PUBLISHED_PATTERNS {
        assert!(
            entries.iter().any(|name| name == workload),
            "case {workload:?}: the catalogue must publish this entry, got {entries:?}"
        );
        let got = pattern_names(workload, &patterns_of(&harness, workload).await);
        let expected: BTreeSet<String> = expected.iter().map(|name| (*name).to_string()).collect();
        assert_eq!(
            got, expected,
            "case {workload:?}: expected the entry's whole published pattern set \
             {expected:?}, got {got:?}"
        );
    }

    // The rule is about the catalogue, not about the rows above: EVERY entry the listing
    // reports is served by the per-workload listing rather than refused, which is what a
    // client that walked the catalogue does next.
    for workload in &entries {
        patterns_of(&harness, workload).await;
    }

    // And the refusal is reserved for a name no entry carries: a well-formed `WorkloadName`
    // (len 1..100, `^[A-Za-z][a-zA-Z0-9-_]*$`) that the catalogue publishes nothing under,
    // including a solution id it does not publish and names one character away from ones
    // it does.
    let mut unpublished: Vec<(String, String)> = vec![
        ("plain unknown".to_string(), "NotAPublishedWorkload".to_string()),
        ("minimum length 1".to_string(), "Q".to_string()),
        (
            "maximum length 100".to_string(),
            format!("Z{}", "y".repeat(99)),
        ),
        (
            "a solution id the catalogue does not publish".to_string(),
            "SO9999".to_string(),
        ),
    ];
    for workload in &entries {
        unpublished.push((
            format!("one character past the published name {workload}"),
            format!("{workload}x"),
        ));
    }
    for (label, absent) in unpublished {
        harness
            .refused(
                &format!("unpublished workload ({label})"),
                PATTERNS_PATH,
                json!({"workloadName": absent}),
                NOT_FOUND,
                NOT_FOUND_CODE,
            )
            .await;
    }
}

#[tokio::test]
async fn a_patterns_text_and_schema_tell_it_apart_from_every_other_pattern_of_its_entry() {
    let harness = Harness::new();
    for workload in catalogue(&harness).await {
        let summaries = patterns_of(&harness, &workload).await;
        let mut display_names: BTreeSet<String> = BTreeSet::new();
        let mut descriptions: BTreeSet<String> = BTreeSet::new();
        for summary in &summaries {
            let pattern = text(&workload, summary, "deploymentPatternName");
            let case = format!("{workload}/{pattern}");
            let display_name = text(&case, summary, "displayName");
            let description = text(&case, summary, "description");

            // Display text is published text, not the identifier again: a client renders
            // it for the operator choosing an architecture, so a pattern whose text is its
            // own name says nothing about what it deploys.
            for (member, value) in [("displayName", &display_name), ("description", &description)] {
                assert_ne!(
                    value.as_str(), pattern,
                    "case {case:?}: {member} must be published text rather than the \
                     pattern's own name, got {value:?}"
                );
            }
            display_names.insert(display_name);
            descriptions.insert(description);
        }

        // Each pattern of one entry carries its OWN text, so a client can tell the
        // entry's patterns apart by what it renders.
        assert_eq!(
            display_names.len(),
            summaries.len(),
            "case {workload:?}: every pattern of one entry must carry its own displayName, \
             got {display_names:?}"
        );
        assert_eq!(
            descriptions.len(),
            summaries.len(),
            "case {workload:?}: every pattern of one entry must carry its own description, \
             got {descriptions:?}"
        );

        // A pattern's SCHEMA states the same dimensions its name does. Where the entry
        // publishes a pattern per network topology, the one that deploys into a network
        // the caller already has asks for that network's id and the one that creates the
        // network asks for the range it is sized with — so the two are different contracts
        // and not one pattern listed twice.
        for summary in &summaries {
            let pattern = text(&workload, summary, "deploymentPatternName");
            let Some(stem) = pattern.strip_suffix(EXISTING_NETWORK_SUFFIX) else {
                continue;
            };
            let sibling = format!("{stem}{NEW_NETWORK_SUFFIX}");
            let case = format!("{workload}/{pattern} vs {sibling}");
            assert!(
                pattern_names(&workload, &summaries).contains(&sibling),
                "case {case:?}: an entry that publishes one network topology of a kind must \
                 publish the other, got {:?}",
                pattern_names(&workload, &summaries)
            );
            let (existing, existing_required) =
                schema_of(&harness, &case, &workload, &pattern).await;
            let (new, new_required) = schema_of(&harness, &case, &workload, &sibling).await;
            assert!(
                existing.contains(EXISTING_NETWORK_FIELD)
                    && existing_required.contains(EXISTING_NETWORK_FIELD),
                "case {case:?}: the existing-network pattern must publish and require \
                 {EXISTING_NETWORK_FIELD}, got {existing:?} required {existing_required:?}"
            );
            assert!(
                new.contains(NEW_NETWORK_FIELD) && new_required.contains(NEW_NETWORK_FIELD),
                "case {case:?}: the new-network pattern must publish and require \
                 {NEW_NETWORK_FIELD}, got {new:?} required {new_required:?}"
            );
            assert!(
                !new.contains(EXISTING_NETWORK_FIELD),
                "case {case:?}: a pattern that creates its network must not ask for the id \
                 of one the caller already has, got {new:?}"
            );
            assert_ne!(
                existing, new,
                "case {case:?}: the two topologies must publish different schemas, got \
                 {existing:?}"
            );
        }
    }
}

#[tokio::test]
async fn an_entry_is_published_under_its_own_display_text_wherever_the_catalogue_reports_it() {
    let harness = Harness::new();
    let listed = harness.ok("listWorkloads", WORKLOADS_PATH, json!({})).await;
    let summaries = listed["workloads"]
        .as_array()
        .unwrap_or_else(|| panic!("ListWorkloads must report a workloads list: {listed}"))
        .clone();

    let mut display_names: BTreeSet<String> = BTreeSet::new();
    for summary in &summaries {
        let workload = text("listWorkloads", summary, "workloadName");
        let display_name = text(&workload, summary, "displayName");
        display_names.insert(display_name.clone());

        // An entry published under an id — which is how a solution entry is named — is
        // published under the SOLUTION's name, not under that id: an id a client already
        // holds tells it nothing about what the entry deploys.
        if is_solution_id(&workload) {
            assert!(
                !display_name.contains(&workload),
                "case {workload:?}: a solution entry's displayName must be the solution's \
                 published name rather than text built from its id, got {display_name:?}"
            );
        }

        // The listing and the entry's own read report ONE record, so the display text a
        // client renders is the same whichever read it came from, and the entry's own read
        // describes it and documents it. `GetWorkload` carries the record on its own
        // `workload` member.
        let reply = harness
            .ok(&workload, WORKLOAD_PATH, json!({"workloadName": workload}))
            .await;
        let data = reply["workload"].clone();
        assert_eq!(
            text(&workload, &data, "displayName"),
            display_name,
            "case {workload:?}: the entry read must report the listing's displayName"
        );
        let description = text(&workload, &data, "description");
        assert_ne!(
            description, workload,
            "case {workload:?}: an entry's description must be published text rather than \
             its own name, got {description:?}"
        );
        let document = text(&workload, &data, "documentationUrl");
        assert!(
            document.starts_with("https://"),
            "case {workload:?}: an entry must document itself with an absolute URL, got \
             {document:?}"
        );
    }

    // Every entry carries its own display text, so no two entries render alike.
    assert_eq!(
        display_names.len(),
        summaries.len(),
        "every catalogue entry must carry its own displayName, got {display_names:?}"
    );
}
