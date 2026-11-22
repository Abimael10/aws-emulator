// SPDX-License-Identifier: CC0-1.0

//! What a PAGINATING client reads out of the Apache Kafka version catalog, and
//! whether the CREATE surfaces agree with it.
//!
//! RULE: the status a client folds out of the published version catalog for a
//! version — enumerate every page of `ListKafkaVersions` and map each row's
//! version to its status, so a version published on more than one row is read
//! from its last row — is the status the service HONOURS for that version,
//! whatever page size the client walked the catalog at. A version the fold reads
//! as `ACTIVE` is one the service still offers, so the versions each broker
//! category's create surface names as the ones a new cluster may run are drawn
//! from that set and a create naming one of them is accepted; a version the fold
//! reads as `DEPRECATED` is offered to no create, on either surface and in either
//! broker category.
//!
//! The suites beside this one read the catalog in ONE page and drive the v1
//! `CreateCluster` (`kafka_version_catalog_fold.rs`), and compare a paged walk to
//! the published rows without folding it (`kafka_version_lifecycle_rows.rs`).
//! What is pinned here is the other half of the same rule: that the fold a client
//! computes is page-size independent, and that the `CreateClusterV2` gate — the
//! surface a current client creates a provisioned cluster through — agrees with
//! it for each broker category.
//!
//! Every reading comes off the WIRE: the fold is computed from the published
//! pages and the offered set from the sentence the service renders when it
//! refuses a version, so neither side is read from the service's own constants.
//! Driven end to end over the same `MultiServiceRouter` the emulator serves from
//! (the shared harness in `tests/all/kafka_harness.rs`).

use serde_json::Value;

use crate::kafka_harness::{
    pct, unsupported_broker_version_message, v2_provisioned_body, Emulator, Reply,
};

/// The two `KafkaVersionStatus` values the model declares (`KafkaVersionStatus` =
/// `ACTIVE | DEPRECATED`).
const ACTIVE: &str = "ACTIVE";
const DEPRECATED: &str = "DEPRECATED";

/// A version string the catalog publishes no row for, which is what makes a create
/// report the versions it DOES take.
const UNCATALOGUED: &str = "9.9.9";

/// One broker instance type per CATEGORY a provisioned cluster's brokers come in:
/// a standard family member and an Express one, each with an offering of its own,
/// so the rule is asserted across both gates rather than one.
const STANDARD_INSTANCE_TYPE: &str = "kafka.m5.large";
const EXPRESS_INSTANCE_TYPE: &str = "express.m7g.large";

/// The category names the service's version refusal words itself with, one per
/// instance type above.
const STANDARD_CATEGORY: &str = "Standard";
const EXPRESS_CATEGORY: &str = "Express";

/// The most rows the model lets one page carry (`MaxResults` range 1..100), which
/// is also the page size a request that names none is served at.
const MAX_RESULTS: usize = 100;

/// One `ListKafkaVersions` call, `query` being the raw query string (`""` for
/// none), with the rows it reported as `(version, status)` in reported order.
async fn page(emulator: &Emulator, query: &str) -> (Vec<(String, String)>, Option<String>) {
    let reply: Reply = emulator
        .call("GET", &format!("/v1/kafka-versions{query}"), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "ListKafkaVersions must succeed, got {} {}",
        reply.status, reply.body
    );
    let rows = reply
        .at("/kafkaVersions")
        .as_array()
        .unwrap_or_else(|| panic!("expected a kafkaVersions list, got {}", reply.body))
        .iter()
        .map(|entry| {
            (
                entry["version"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a row without a version: {entry}"))
                    .to_string(),
                entry["status"]
                    .as_str()
                    .unwrap_or_else(|| panic!("a row without a status: {entry}"))
                    .to_string(),
            )
        })
        .collect();
    let token = reply
        .body
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (rows, token)
}

/// Every row of the catalog, read the way a paginating client reads it: page after
/// page at `page_size` rows, following the token each page hands back until a page
/// hands back none.
async fn walk(emulator: &Emulator, page_size: usize, pages_allowed: usize) -> Vec<(String, String)> {
    let mut seen: Vec<(String, String)> = Vec::new();
    let mut token: Option<String> = None;
    let mut pages = 0usize;
    loop {
        pages += 1;
        assert!(
            pages <= pages_allowed,
            "case maxResults={page_size}: the walk did not end after {pages} pages"
        );
        let query = match &token {
            Some(value) => format!("?maxResults={page_size}&nextToken={}", pct(value)),
            None => format!("?maxResults={page_size}"),
        };
        let (rows, next) = page(emulator, &query).await;
        seen.extend(rows);
        match next {
            Some(next) => token = Some(next),
            None => return seen,
        }
    }
}

/// The rows folded the way a client folds them: one status per version, the LAST
/// row published for it, in the order the versions are first published.
fn folded(rows: &[(String, String)]) -> Vec<(String, String)> {
    let mut folded: Vec<(String, String)> = Vec::new();
    for (version, status) in rows {
        match folded.iter_mut().find(|(name, _)| name == version) {
            Some(entry) => entry.1 = status.clone(),
            None => folded.push((version.clone(), status.clone())),
        }
    }
    folded
}

/// The status the fold reports for `version`, or `None` when the catalog publishes
/// no row for it.
fn folded_status(folded: &[(String, String)], version: &str) -> Option<String> {
    folded
        .iter()
        .find(|(name, _)| name == version)
        .map(|(_, status)| status.clone())
}

/// The versions the SERVICE names as the ones a new cluster whose brokers are of
/// `instance_type`'s category may run: the valid-value list its `CreateClusterV2`
/// refusal of a version it does not take carries, in the order that sentence names
/// them. Read off the wire, and the whole sentence is checked against the one MSK
/// words that refusal with.
async fn offered_for(emulator: &Emulator, case: &str, instance_type: &str, category: &str) -> Vec<String> {
    let refusal = emulator
        .create_cluster_v2(v2_provisioned_body(
            // The name pattern admits no dot, and a row's case can carry one.
            &format!("offered-{}", case.replace('.', "-")),
            UNCATALOGUED,
            instance_type,
        ))
        .await;
    assert_eq!(
        (refusal.status, refusal.error_code()),
        (400, "BadRequestException"),
        "case {case}: a create naming a version the catalog does not publish must be refused, \
         got {} {}",
        refusal.status,
        refusal.body
    );
    let message = refusal.error_message().to_string();
    let listed = message
        .rsplit_once("Valid values: [")
        .and_then(|(_, tail)| tail.strip_suffix(']'))
        .unwrap_or_else(|| {
            panic!("case {case}: the refusal must carry a valid-value list, got {message:?}")
        });
    let listed: Vec<String> = listed.split(", ").map(str::to_string).collect();
    assert_eq!(
        message,
        unsupported_broker_version_message(category, UNCATALOGUED, &listed),
        "case {case}: expected the service's version refusal for that broker category, got \
         {message:?}"
    );
    listed
}

/// A cluster name out of a version string: the name pattern admits no dot.
fn cluster_name(prefix: &str, version: &str) -> String {
    format!("{prefix}-{}", version.replace('.', "-"))
}

/// RULE: the fold is page-size independent — a client that walks the catalog at
/// any page size the model allows reads the same status for every version as one
/// that reads it in a single page — and that status is the one the service honours.
///
/// The page sizes span the model's `MaxResults` range: the smallest page (one row,
/// so a multi-row version's rows land on different pages), small sizes that do and
/// do not divide the row count, the sizes either side of the whole catalog, and the
/// largest page the model allows. The folds are checked against a table written by
/// hand from the catalog Amazon MSK publishes (the Developer Guide's "Supported
/// Apache Kafka versions" list, and the rows the service itself publishes),
/// spanning the version spellings the catalog uses — a three-component release, a
/// four-component one, the `.tiered` storage line, a `.x` release and a `.x.kraft`
/// one — and naming versions the report this rule came from does not cite
/// (`1.1.1`, `2.4.1.1`, `2.8.2.tiered`, `3.6.0`, `3.8.x`).
#[tokio::test]
async fn a_paged_walk_of_the_catalog_folds_to_the_status_the_service_honours() {
    let emulator = Emulator::new();
    let (whole, token) = page(&emulator, "").await;
    assert_eq!(
        token, None,
        "a request naming no maxResults must serve the whole catalog in one page, got {token:?}"
    );
    let single_page = folded(&whole);

    // Not vacuous: the catalog really does publish a version on more than one row,
    // so the LAST row for a version is what decides the fold.
    assert!(
        whole.len() > single_page.len(),
        "a version published on more than one row means more rows than folded versions, got \
         {whole:?}"
    );

    let sizes = [1usize, 2, 7, whole.len() - 1, whole.len(), MAX_RESULTS];
    for page_size in sizes {
        let walked = walk(&emulator, page_size, whole.len() + 1).await;
        let paged = folded(&walked);
        assert_eq!(
            paged, single_page,
            "case maxResults={page_size}: a paged walk must fold to the same version -> status \
             map as a single-page read, got {paged:?}"
        );
    }

    let expected: &[(&str, &str)] = &[
        ("1.1.1", DEPRECATED),
        ("2.4.1.1", DEPRECATED),
        ("2.8.2.tiered", DEPRECATED),
        ("3.6.0", ACTIVE),
        ("3.8.x", ACTIVE),
        ("3.9.x", ACTIVE),
        ("3.9.x.kraft", ACTIVE),
        ("4.2.x.kraft", ACTIVE),
    ];
    // Read at the harshest page size — one row per page, which is where a version's
    // two rows are served on two different pages.
    let paged = folded(&walk(&emulator, 1, whole.len() + 1).await);
    for (version, status) in expected {
        let actual = folded_status(&paged, version);
        assert_eq!(
            actual.as_deref(),
            Some(*status),
            "case version={version:?}: expected a paged client to fold {status:?} out of the \
             catalog, got {actual:?} out of the rows {whole:?}"
        );
    }
}

/// RULE, the gate side of the same read: the versions each broker category's
/// `CreateClusterV2` gate takes for a NEW cluster are drawn from the versions the
/// fold reads as `ACTIVE`, and a create naming one of them is accepted and reports
/// that version back — while a version the fold reads as `DEPRECATED` is taken by
/// neither category, refused in the service's own sentence.
///
/// Both broker categories are read, so the rule is asserted over the categories it
/// generalises across rather than over one of them, and the deprecated rows span
/// the version spellings the catalog publishes deprecated versions under.
#[tokio::test]
async fn the_v2_create_gate_agrees_with_the_folded_catalog() {
    let emulator = Emulator::new();
    let (whole, _) = page(&emulator, "").await;
    // The fold a paginating client computes, which is what the gate must agree with.
    let paged = folded(&walk(&emulator, 1, whole.len() + 1).await);

    let categories = [
        ("standard", STANDARD_INSTANCE_TYPE, STANDARD_CATEGORY),
        ("express", EXPRESS_INSTANCE_TYPE, EXPRESS_CATEGORY),
    ];
    for (case, instance_type, category) in categories {
        let offered = offered_for(&emulator, case, instance_type, category).await;
        assert!(
            !offered.is_empty(),
            "case {case}: the refusal must name the versions a new cluster may run, got \
             {offered:?}"
        );
        for (index, version) in offered.iter().enumerate() {
            let status = folded_status(&paged, version);
            assert_eq!(
                status.as_deref(),
                Some(ACTIVE),
                "case {case} version={version:?}: a version a create may name must fold to \
                 {ACTIVE:?}, got {status:?}"
            );
            assert!(
                !offered[..index].contains(version),
                "case {case} version={version:?}: the refusal must name each version once, got \
                 {offered:?}"
            );
        }
    }

    // A create naming a version the standard category is offered is accepted, and
    // the cluster it mints reports that version back: the fold is not just agreed
    // with on paper.
    let offered = offered_for(&emulator, "standard", STANDARD_INSTANCE_TYPE, STANDARD_CATEGORY).await;
    for version in &offered {
        let created = emulator
            .create_cluster_v2(v2_provisioned_body(
                &cluster_name("paged-fold", version),
                version,
                STANDARD_INSTANCE_TYPE,
            ))
            .await;
        assert_eq!(
            created.status, 200,
            "case version={version:?}: a create naming an offered version must be accepted, got \
             {} {}",
            created.status, created.body
        );
        let arn = created
            .at("/clusterArn")
            .as_str()
            .unwrap_or_else(|| {
                panic!("case version={version:?}: expected a clusterArn, got {}", created.body)
            })
            .to_string();
        let described = emulator.describe_v2(&arn).await;
        assert_eq!(
            described.status, 200,
            "case version={version:?}: DescribeClusterV2 must answer, got {} {}",
            described.status, described.body
        );
        let reported = described
            .at("/clusterInfo/provisioned/currentBrokerSoftwareInfo/kafkaVersion")
            .as_str()
            .map(str::to_string);
        assert_eq!(
            reported.as_deref(),
            Some(version.as_str()),
            "case version={version:?}: the cluster must report the version it was created on, \
             got {reported:?}"
        );
    }

    // And the other half: a version the fold reads as DEPRECATED is taken by
    // neither category's gate. The rows are the spellings a deprecated version
    // comes in.
    for version in ["1.1.1", "2.4.1.1", "2.8.2.tiered", "3.6.0.1"] {
        let status = folded_status(&paged, version);
        assert_eq!(
            status.as_deref(),
            Some(DEPRECATED),
            "case version={version:?}: this row needs a version the fold reads as \
             {DEPRECATED:?}, got {status:?}"
        );
        for (case, instance_type, category) in categories {
            let offered = offered_for(&emulator, &format!("{case}-{version}"), instance_type, category).await;
            let refused = emulator
                .create_cluster_v2(v2_provisioned_body(
                    &cluster_name(&format!("paged-fold-{case}"), version),
                    version,
                    instance_type,
                ))
                .await;
            assert_eq!(
                (refused.status, refused.error_code()),
                (400, "BadRequestException"),
                "case {case} version={version:?}: a create naming a version the fold reads as \
                 {DEPRECATED:?} must be refused, got {} {}",
                refused.status,
                refused.body
            );
            assert_eq!(
                refused.error_message(),
                unsupported_broker_version_message(category, version, &offered),
                "case {case} version={version:?}: expected the service's version refusal naming \
                 the versions that category takes, got {}",
                refused.body
            );
        }
    }

    // Reading the catalog and creating clusters changed nothing a later caller
    // sees: the rows are published metadata.
    let (again, _) = page(&emulator, "").await;
    assert_eq!(
        again, whole,
        "the published catalog must be unchanged by reads and creates, got {again:?}"
    );
}
