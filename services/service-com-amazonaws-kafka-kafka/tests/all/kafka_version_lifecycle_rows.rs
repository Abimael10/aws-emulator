// SPDX-License-Identifier: CC0-1.0

//! A version's LIFECYCLE ROWS: the Apache Kafka version catalog is a sequence of
//! published `{version, status}` rows rather than a version -> status map, so a
//! version the service publishes under both lifecycle statuses is served on BOTH
//! rows, in the order it publishes them — while every answer about a VERSION
//! (which versions the service offers, which it accepts, which it lists as an
//! upgrade source) is computed over the de-duplicated version set, so a second row
//! changes none of them.
//!
//! Driven end to end over the same `MultiServiceRouter` the emulator serves from
//! (the shared harness in `tests/all/kafka_harness.rs`).

use serde_json::Value;

use crate::kafka_harness::{pct, ClusterRequest, Emulator, Reply};

/// The `KafkaVersionStatus` value a version the service still offers is published
/// under (`KafkaVersionStatus` = `ACTIVE | DEPRECATED` in the Smithy model).
const ACTIVE: &str = "ACTIVE";

/// One `ListKafkaVersions` call. `query` is the raw query string, `""` for none.
async fn list_versions(emulator: &Emulator, query: &str) -> Reply {
    emulator
        .call("GET", &format!("/v1/kafka-versions{query}"), None)
        .await
}

/// The published rows a reply reports, as `(version, status)` in reported order.
fn rows(reply: &Reply) -> Vec<(String, String)> {
    assert_eq!(
        reply.status, 200,
        "a version listing must succeed, got {} {}",
        reply.status, reply.body
    );
    reply
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
        .collect()
}

/// The whole published catalog, read in one page.
async fn published_rows(emulator: &Emulator) -> Vec<(String, String)> {
    rows(&list_versions(emulator, "").await)
}

/// The statuses the catalog publishes `version` under, in published order.
fn published_statuses(rows: &[(String, String)], version: &str) -> Vec<String> {
    rows.iter()
        .filter(|(name, _)| name == version)
        .map(|(_, status)| status.clone())
        .collect()
}

/// The versions the catalog publishes, each ONCE, in published order.
fn distinct_versions(rows: &[(String, String)]) -> Vec<String> {
    let mut versions: Vec<String> = Vec::new();
    for (version, _) in rows {
        if !versions.contains(version) {
            versions.push(version.clone());
        }
    }
    versions
}

/// Walk every page of the listing at `page_size` and hand back the rows in the
/// order the pages reported them.
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
        let reply = list_versions(emulator, &query).await;
        seen.extend(rows(&reply));
        match reply.body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => return seen,
        }
    }
}

/// RULE: the listing publishes one entry per catalog ROW, so a version the
/// catalog publishes under both lifecycle statuses is served TWICE — once per row,
/// in the order the catalog publishes them — and a walk of the pages at any page
/// size reproduces that row sequence exactly, duplicates included.
#[tokio::test]
async fn a_version_published_under_two_statuses_is_served_on_both_rows() {
    let emulator = Emulator::new();
    let catalog = published_rows(&emulator).await;

    // Hand-written from the catalog Amazon MSK publishes (the Developer Guide's
    // "Supported Apache Kafka versions" list, and the rows the service itself
    // publishes). The table spans the multiplicities a published version comes in
    // — one offered row, one deprecated row, two rows (the offering one last) —
    // across the
    // version spellings the catalog uses, and includes versions neither this rule
    // nor its report was derived from (`2.3.1`, `3.7.x.kraft`, `2.8.2.tiered`).
    let expected: &[(&str, &[&str])] = &[
        ("2.3.1", &["DEPRECATED"]),
        ("2.8.2.tiered", &["DEPRECATED"]),
        ("3.6.0", &["ACTIVE"]),
        ("3.7.x.kraft", &["ACTIVE"]),
        ("3.9.x", &["DEPRECATED", "ACTIVE"]),
        ("4.2.x.kraft", &["DEPRECATED", "ACTIVE"]),
    ];
    for (version, statuses) in expected {
        let served = published_statuses(&catalog, version);
        assert_eq!(
            served, *statuses,
            "case version={version:?}: expected the published rows {statuses:?}, got {served:?}"
        );
    }

    // At least one version really is published on two rows, so the rule above is
    // not vacuous, and the catalog carries more rows than versions.
    let versions = distinct_versions(&catalog);
    let multi_row: Vec<&String> = versions
        .iter()
        .filter(|version| published_statuses(&catalog, version).len() > 1)
        .collect();
    assert!(
        !multi_row.is_empty(),
        "the catalog must publish a version under both lifecycle statuses, got {catalog:?}"
    );
    assert!(
        catalog.len() > versions.len(),
        "a version published twice means more rows than versions, got {catalog:?}"
    );

    // Paging is over ROWS, so every walk reproduces the row sequence including the
    // second rows: page sizes smaller than, dividing, equal to and larger than the
    // row count, plus a size that splits a multi-row version's rows across pages.
    let sizes = [1usize, 2, 3, catalog.len() - 1, catalog.len(), 100];
    for page_size in sizes {
        let seen = walk(&emulator, page_size, catalog.len() + 1).await;
        assert_eq!(
            seen, catalog,
            "case maxResults={page_size}: the pages must reproduce the catalog row for row"
        );
    }
}

/// RULE: a second row for a version changes no answer ABOUT that version: the
/// version set is de-duplicated, so a version the catalog publishes an `ACTIVE`
/// row for is offered exactly once wherever the service answers about versions —
/// the upgrade matrix's target lists, the valid-value list a refusal carries — and
/// the create gate judges it by the version too, admitting exactly the versions
/// that refusal lists. The matrix's SOURCES are the catalog's rows instead: that
/// answer publishes the table rather than answering about one version, so a
/// version published twice is a source twice.
#[tokio::test]
async fn a_second_row_changes_no_answer_about_the_version() {
    let emulator = Emulator::new();
    let catalog = published_rows(&emulator).await;
    let versions = distinct_versions(&catalog);

    // The upgrade matrix publishes one entry per published ROW, in published
    // order.
    let matrix = emulator
        .call("GET", "/v1/compatible-kafka-versions", None)
        .await;
    assert_eq!(
        matrix.status, 200,
        "the upgrade matrix must succeed, got {} {}",
        matrix.status, matrix.body
    );
    let sources: Vec<String> = matrix
        .at("/compatibleKafkaVersions")
        .as_array()
        .unwrap_or_else(|| panic!("expected a compatibleKafkaVersions list, got {}", matrix.body))
        .iter()
        .map(|entry| {
            entry["sourceVersion"]
                .as_str()
                .unwrap_or_else(|| panic!("an entry without a sourceVersion: {entry}"))
                .to_string()
        })
        .collect();
    let published: Vec<String> = catalog.iter().map(|(version, _)| version.clone()).collect();
    assert_eq!(
        sources, published,
        "the matrix must publish one entry per published row, in published order"
    );
    for version in &versions {
        let rows = published.iter().filter(|row| *row == version).count();
        let occurrences = sources.iter().filter(|source| *source == version).count();
        assert_eq!(
            occurrences, rows,
            "case version={version:?}: expected one matrix entry per published row ({rows}), \
             got {occurrences}"
        );
    }

    // The valid-value list a refusal carries is drawn from that same de-duplicated
    // set, filtered to the versions the service offers, each named once — a second
    // row for a version never puts it in the list twice.
    let refusal = emulator
        .create(&ClusterRequest {
            kafka_version: "9.9.9",
            ..ClusterRequest::named("second-row-refusal")
        })
        .await;
    assert_eq!(
        (refusal.status, refusal.error_code()),
        (400, "BadRequestException"),
        "a create naming an uncatalogued version must be refused, got {} {}",
        refusal.status,
        refusal.body
    );
    let message = refusal.error_message().to_string();
    let listed = message
        .rsplit_once("Valid values: [")
        .and_then(|(_, tail)| tail.strip_suffix(']'))
        .unwrap_or_else(|| panic!("the refusal must carry a valid-value list, got {message:?}"));
    let listed: Vec<&str> = listed.split(", ").collect();
    let offered: Vec<&String> = versions
        .iter()
        .filter(|version| published_statuses(&catalog, version).iter().any(|s| s == ACTIVE))
        .collect();
    // Without this, a refusal that named no version at all would satisfy the loop below by
    // holding nothing to break it.
    assert!(
        !listed.is_empty(),
        "expected the refusal to name the versions a create may use, got {message:?}"
    );
    for (index, version) in listed.iter().enumerate() {
        assert!(
            offered.iter().any(|offered| offered.as_str() == *version),
            "case version={version:?}: the refusal may only name a version the catalog \
             publishes an {ACTIVE} row for, got {message:?}"
        );
        assert!(
            !listed[..index].contains(version),
            "case version={version:?}: the refusal must name each version once, got {message:?}"
        );
    }

    // And a version the catalog publishes on two rows, one of them `ACTIVE`, is
    // answered about by the VERSION rather than by its last row: the one the
    // refusal lists is admitted by the create gate, and the one it does not list is
    // still offered as an upgrade target, which a version read as deprecated never
    // is.
    for version in ["3.9.x", "4.2.x.kraft"] {
        let statuses = published_statuses(&catalog, version);
        assert!(
            statuses.len() > 1 && statuses.iter().any(|status| status == ACTIVE),
            "case version={version:?}: this case needs a version with an ACTIVE row among \
             several, got {statuses:?}"
        );
        let created = emulator
            .create(&ClusterRequest {
                kafka_version: version,
                ..ClusterRequest::named(&format!("two-row-{}", version.replace('.', "-")))
            })
            .await;
        match listed.contains(&version) {
            true => assert_eq!(
                created.status, 200,
                "case version={version:?}: a create naming a version the refusal lists must \
                 succeed, got {} {}",
                created.status, created.body
            ),
            false => assert_eq!(
                (created.status, created.error_code()),
                (400, "BadRequestException"),
                "case version={version:?}: a create naming a version the refusal does not list \
                 must be refused, got {} {}",
                created.status,
                created.body
            ),
        }
        // Whichever of the two it is, the service still OFFERS the version: the
        // matrix names it as a target reachable from some source it publishes.
        let offered_as_target = matrix
            .at("/compatibleKafkaVersions")
            .as_array()
            .unwrap_or_else(|| panic!("expected a compatibleKafkaVersions list, got {}", matrix.body))
            .iter()
            .any(|entry| {
                entry["targetVersions"]
                    .as_array()
                    .is_some_and(|targets| targets.iter().any(|target| target == version))
            });
        assert!(
            offered_as_target,
            "case version={version:?}: a version with an {ACTIVE} row must be offered as an \
             upgrade target, got {}",
            matrix.body
        );
    }

    // A read changes nothing: the catalog a later caller sees is the same one.
    let again = published_rows(&emulator).await;
    assert_eq!(
        again, catalog,
        "the catalog must be unchanged by reads and creates, got {again:?}"
    );
}
