// SPDX-License-Identifier: CC0-1.0

//! `GetCompatibleKafkaVersions`, driven end to end over the shared
//! [`crate::kafka_harness`] — the same `MultiServiceRouter` the emulator serves
//! from.
//!
//! The RULE under test: the compatible target versions of a source version are
//! the upgrades the catalog permits FROM that version — a version the service
//! still offers (the catalog publishes an `ACTIVE` row for it in
//! [`KAFKA_VERSION_CATALOG`]), on the SAME variant line as the source and naming a
//! strictly newer release, plus the one cross-line move MSK offers: at its final
//! offered ZooKeeper-mode release, the `.kraft` version of that release (the KRaft
//! migration). MSK upgrades in place, never downgrades, never offers a version it
//! has deprecated, and never moves a cluster between builds of the broker, so a
//! source with nothing newer on its line is TERMINAL and reports an empty list.
//! With a `ClusterArn` the single source version is the one the STORED cluster
//! runs; without one, the answer is the published TABLE — one row per ROW the
//! catalog publishes, in published order, which is the sequence
//! `ListKafkaVersions` pages.
//!
//! Each suite asserts that rule over a TABLE whose rows differ along what the rule
//! generalises over: where the source sits in the catalog (oldest, deprecated, the
//! newest), how many components its version string has, which line it is on (plain
//! ZooKeeper, `.tiered`, `.kraft`), whether it has a KRaft version of its
//! own release, whether it is terminal, and whether the catalog lists it at all.
//! The expected target lists are written out by hand from the catalog and are then
//! cross-checked against the catalog itself, so neither the handler nor its helper
//! is asserted against itself.

use serde_json::Value;
use service_com_amazonaws_kafka_kafka::state::{KAFKA_VERSION_ACTIVE, KAFKA_VERSION_CATALOG};

use crate::kafka_harness::{
    kafka_version_line, kafka_version_release, pct, ClusterRequest, Emulator, Reply, ACCOUNT,
    KRAFT_LINE, OTHER_LINE, REGION,
};

/// `GetCompatibleKafkaVersions` for one cluster, as the model binds it:
/// `GET /v1/compatible-kafka-versions?clusterArn=<arn>`.
async fn compatible_for(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .call(
            "GET",
            &format!("/v1/compatible-kafka-versions?clusterArn={}", pct(arn)),
            None,
        )
        .await
}

/// The same call with no `clusterArn` — the member is optional, and omitting it
/// asks for the whole matrix.
async fn compatible_matrix(emulator: &Emulator) -> Reply {
    emulator
        .call("GET", "/v1/compatible-kafka-versions", None)
        .await
}

/// The `compatibleKafkaVersions` entries of a reply, as
/// `(sourceVersion, targetVersions)` pairs.
fn entries(reply: &Reply) -> Vec<(String, Vec<String>)> {
    reply.body["compatibleKafkaVersions"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a compatibleKafkaVersions list, got {}", reply.body))
        .iter()
        .map(|entry| {
            let source = entry["sourceVersion"]
                .as_str()
                .unwrap_or_else(|| panic!("entry without a sourceVersion: {entry}"))
                .to_string();
            let targets = entry["targetVersions"]
                .as_array()
                .unwrap_or_else(|| panic!("entry without targetVersions: {entry}"))
                .iter()
                .map(|value| {
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("a non-string target version: {entry}"))
                        .to_string()
                })
                .collect();
            (source, targets)
        })
        .collect()
}

/// The target versions the catalog ITSELF implies for `source`, computed here
/// from the published entries rather than from the service's helper, so it is an
/// independent check of the handler's answer: an offered version (the catalog
/// publishes an `ACTIVE` row for it) that is either NEWER on the source's own
/// upgradable line — ZooKeeper mode, which the `.tiered` spelling is on too, or
/// KRaft — or, when the source sits at the final offered ZooKeeper-mode release,
/// the `.kraft` version of that release. A source on any other line is terminal.
/// In catalog order, each version once.
fn targets_from_catalog(source: &str) -> Vec<String> {
    /// The `(release, line)` of a version, read through the shared harness's
    /// version-spelling helpers so this suite places a version exactly as its
    /// neighbours do.
    fn placed(version: &str) -> (Vec<u32>, &str) {
        (kafka_version_release(version), kafka_version_line(version))
    }

    let (source_release, source_line) = placed(source);
    // The release the crossing to KRaft is offered at: the newest release the
    // catalog publishes an offered ZooKeeper-mode version of.
    let crossing = KAFKA_VERSION_CATALOG
        .iter()
        .filter(|entry| published_statuses(entry.version).contains(&KAFKA_VERSION_ACTIVE))
        .filter(|entry| placed(entry.version).1.is_empty())
        .map(|entry| placed(entry.version).0)
        .max()
        .unwrap_or_default();
    let mut targets: Vec<String> = Vec::new();
    for entry in KAFKA_VERSION_CATALOG {
        if !published_statuses(entry.version).contains(&KAFKA_VERSION_ACTIVE) {
            continue;
        }
        let (release, line) = placed(entry.version);
        let permitted = if line == source_line {
            source_line != OTHER_LINE && release > source_release
        } else {
            source_line.is_empty()
                && line == KRAFT_LINE
                && release == source_release
                && source_release == crossing
        };
        if permitted && !targets.iter().any(|target| target == entry.version) {
            targets.push(entry.version.to_string());
        }
    }
    targets
}

/// The statuses the catalog publishes for `version` — one per ROW it publishes
/// for it, which for two of its versions is two rows.
fn published_statuses(version: &str) -> Vec<&'static str> {
    KAFKA_VERSION_CATALOG
        .iter()
        .filter(|entry| entry.version == version)
        .map(|entry| entry.status)
        .collect()
}

/// Whether the catalog still OFFERS `version`: whether it publishes any `ACTIVE`
/// row for it.
fn is_offered(version: &str) -> bool {
    published_statuses(version).contains(&KAFKA_VERSION_ACTIVE)
}

/// Every version the catalog publishes, each once, in published order.
fn catalogued() -> Vec<&'static str> {
    let mut versions: Vec<&'static str> = Vec::new();
    for entry in KAFKA_VERSION_CATALOG {
        if !versions.contains(&entry.version) {
            versions.push(entry.version);
        }
    }
    versions
}

/// Owned expectations from a borrowed table, so a row can write them inline.
fn versions(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// A minimal `CreateCluster` row running `kafka_version`.
fn cluster_on(name: &str, kafka_version: &'static str) -> ClusterRequest {
    ClusterRequest {
        kafka_version,
        ..ClusterRequest::named(name)
    }
}

/// The catalog is documented as published in one order, and the matrix answer reads
/// the versions it publishes in that order — so the order itself is pinned here: rows
/// never descend by the release they name, and the versions ascend strictly at their
/// first published row, by the `major.minor` release, then the patch the version
/// spells out, then the build line at that release. An out-of-order insertion would
/// otherwise desync the catalog from the answers computed over it.
#[test]
fn the_catalog_is_published_in_ascending_release_order() {
    /// The published-order key of a version: its `major.minor` release, the rank of
    /// the build line at that release (ZooKeeper 0 — which the `.tiered` storage
    /// spelling is on too — KRaft 1, any other line 2), and the patch components
    /// the version spells out, with `x` above every numbered patch.
    fn key(version: &str) -> ((u32, u32), u8, Vec<u32>) {
        let rank = match version
            .rsplit('.')
            .next()
            .filter(|last| *last != "x" && last.parse::<u32>().is_err())
            .unwrap_or_default()
        {
            "" | "tiered" => 0,
            "kraft" => 1,
            _ => 2,
        };
        let numbers: Vec<u32> = version
            .split('.')
            .filter_map(|part| match part.parse::<u32>() {
                Ok(number) => Some(number),
                Err(_) if part == "x" => Some(u32::MAX),
                Err(_) => None,
            })
            .collect();
        (
            (
                numbers.first().copied().unwrap_or_default(),
                numbers.get(1).copied().unwrap_or_default(),
            ),
            rank,
            numbers.into_iter().skip(2).collect(),
        )
    }

    /// The `major.minor` release a version names: neither the patch it spells out
    /// nor its build line is part of it, so every row of one release shares it.
    fn release_of(version: &str) -> (u32, u32) {
        key(version).0
    }

    for pair in KAFKA_VERSION_CATALOG.windows(2) {
        let (older, newer) = (pair[0].version, pair[1].version);
        assert!(
            release_of(older) <= release_of(newer),
            "case {older:?} -> {newer:?}: the catalog must not descend by release, got {:?} \
             then {:?}",
            release_of(older),
            release_of(newer)
        );
    }
    for pair in catalogued().windows(2) {
        let (older, newer) = (pair[0], pair[1]);
        assert!(
            key(older) < key(newer),
            "case {older:?} -> {newer:?}: the published versions must ascend strictly, got \
             {:?} then {:?}",
            key(older),
            key(newer)
        );
    }
}

/// RULE: with a `ClusterArn`, the answer is one entry whose `sourceVersion` is the
/// version the STORED cluster runs and whose `targetVersions` are the upgrades the
/// catalog permits from it.
///
/// Rows: the oldest catalogued version, a deprecated four-component version, the
/// `.tiered` spelling, the version at the active boundary, a four-component patch
/// of it, the `.x` versions below and at the release the KRaft crossing is offered
/// at, a KRaft source, the newest offered KRaft
/// version and the version the catalog publishes both an offered and a deprecated
/// row for. The expected lists are written out from the catalog documented in
/// `src/state.rs`.
///
/// Every row's source version is one the catalog PUBLISHES, because that is what a
/// create admits: a cluster cannot be stored on a version the service does not
/// name (`create_cluster`'s version gate), so the reader is never asked about one.
#[tokio::test]
async fn compatible_targets_are_the_upgrades_the_catalog_permits() {
    let emulator = Emulator::new();

    let rows: Vec<(&'static str, Vec<String>)> = vec![
        // Older than every offered version, and on the ZooKeeper line: every
        // offered version of that line is reachable, and no KRaft one is.
        ("1.1.1", versions(&["3.6.0", "3.7.x", "3.8.x", "3.9.x"])),
        ("2.4.1.1", versions(&["3.6.0", "3.7.x", "3.8.x", "3.9.x"])),
        ("2.8.1", versions(&["3.6.0", "3.7.x", "3.8.x", "3.9.x"])),
        // The tiered-storage spelling is that same build, so it climbs the same
        // ladder.
        ("2.8.2.tiered", versions(&["3.6.0", "3.7.x", "3.8.x", "3.9.x"])),
        ("3.6.0", versions(&["3.7.x", "3.8.x", "3.9.x"])),
        ("3.6.0.1", versions(&["3.7.x", "3.8.x", "3.9.x"])),
        // A release that HAS a KRaft version but is not the release the crossing
        // is offered at: the ladder stays on the ZooKeeper line.
        ("3.7.x", versions(&["3.8.x", "3.9.x"])),
        ("3.8.x", versions(&["3.9.x"])),
        // The final offered ZooKeeper-mode release: the crossing to KRaft, and
        // nothing else, because there is no newer ZooKeeper-mode version.
        ("3.9.x", versions(&["3.9.x.kraft"])),
        // A KRaft source climbs the KRaft line only — there is no way back to
        // ZooKeeper mode.
        (
            "3.7.x.kraft",
            versions(&[
                "3.8.x.kraft",
                "3.9.x.kraft",
                "4.0.x.kraft",
                "4.1.x.kraft",
                "4.2.x.kraft",
            ]),
        ),
        (
            "3.9.x.kraft",
            versions(&["4.0.x.kraft", "4.1.x.kraft", "4.2.x.kraft"]),
        ),
        ("4.1.x.kraft", versions(&["4.2.x.kraft"])),
        // The newest KRaft version the catalog publishes: nothing to upgrade to.
        ("4.2.x.kraft", Vec::new()),
    ];

    for (index, (version, expected)) in rows.iter().enumerate() {
        let request = cluster_on(&format!("compat-{index}"), version);
        let arn = emulator.create_ok(&request).await;
        let reply = compatible_for(&emulator, &arn).await;
        assert_eq!(
            reply.status, 200,
            "case version={version:?}: expected 200, got {} {}",
            reply.status, reply.body
        );
        let answered = entries(&reply);
        assert_eq!(
            answered.len(),
            1,
            "case version={version:?}: expected exactly one entry, got {answered:?}"
        );
        let (source, targets) = &answered[0];
        assert_eq!(
            source, version,
            "case version={version:?}: expected the stored cluster's version as sourceVersion, got {source:?}"
        );
        assert_eq!(
            targets, expected,
            "case version={version:?}: expected targets {expected:?}, got {targets:?}"
        );
        // The source is never its own target (no self-upgrade), and no target is a
        // version the service no longer offers.
        assert!(
            !targets.contains(source),
            "case version={version:?}: the source version must not be a target, got {targets:?}"
        );
        for target in targets {
            assert!(
                is_offered(target),
                "case version={version:?}: target {target:?} is not an offered catalog version, got {targets:?}"
            );
        }
        // And the answer agrees with what the catalog itself implies.
        assert_eq!(
            targets,
            &targets_from_catalog(version),
            "case version={version:?}: the answer must match the catalog, got {targets:?}"
        );
    }
}

/// RULE: with no `ClusterArn` the answer is the whole published matrix — one entry
/// per ROW the catalog publishes, in the catalog's published order, each carrying
/// the upgrades the catalog permits from that source, so a client reading the
/// matrix is offered a per-source subset rather than one blanket list.
#[tokio::test]
async fn the_matrix_covers_every_catalogued_source_version() {
    let emulator = Emulator::new();
    let reply = compatible_matrix(&emulator).await;
    assert_eq!(
        reply.status, 200,
        "expected 200 for a matrix request, got {} {}",
        reply.status, reply.body
    );
    let answered = entries(&reply);
    let published: Vec<&'static str> = KAFKA_VERSION_CATALOG
        .iter()
        .map(|entry| entry.version)
        .collect();
    assert_eq!(
        answered.len(),
        published.len(),
        "expected one entry per published row ({}), got {}",
        published.len(),
        answered.len()
    );
    for (entry, published) in answered.iter().zip(published.iter()) {
        let (source, targets) = entry;
        assert_eq!(
            source, published,
            "case source={source:?}: expected the catalog order, {published} comes here"
        );
        assert_eq!(
            targets,
            &targets_from_catalog(source),
            "case source={source:?}: expected {:?}, got {targets:?}",
            targets_from_catalog(source)
        );
    }
    // Every version the catalog publishes is a source, whatever number of rows it
    // is published under.
    for version in catalogued() {
        assert!(
            answered.iter().any(|(source, _)| source == version),
            "case version={version:?}: the matrix must publish a row for it, got {answered:?}"
        );
    }
    // The matrix is a per-source graph, not one shared list: a terminal version
    // reports no target at all, and two sources on different lines are offered
    // different things.
    let terminal: Vec<&(String, Vec<String>)> = answered
        .iter()
        .filter(|(_, targets)| targets.is_empty())
        .collect();
    assert!(
        !terminal.is_empty(),
        "expected at least one source with no upgrade target, got {answered:?}"
    );
    let distinct: std::collections::BTreeSet<Vec<String>> = answered
        .iter()
        .map(|(_, targets)| targets.clone())
        .collect();
    assert!(
        distinct.len() > 1,
        "expected the matrix to offer different targets per source, got {answered:?}"
    );
    // And no deprecated version is ever an upgrade target, whichever source names
    // it.
    for (source, targets) in &answered {
        for target in targets {
            assert!(
                is_offered(target),
                "case source={source:?}: target {target:?} is not offered"
            );
        }
    }
}

/// RULE: a reader that models both errors separates a `ClusterArn` it cannot parse
/// (`BadRequestException`, 400) from a well-formed one this endpoint's region holds
/// no cluster for (`NotFoundException`, 404) — including a cluster that exists in
/// another region.
#[tokio::test]
async fn an_unknown_or_malformed_cluster_arn_is_refused() {
    let emulator = Emulator::new();
    emulator.create_ok(&ClusterRequest::named("present")).await;
    // A cluster of the same shape in another region: well formed, and still not
    // this endpoint's.
    let elsewhere = emulator
        .call_in(
            "eu-west-1",
            "POST",
            "/v1/clusters",
            Some(ClusterRequest::named("elsewhere").to_json()),
        )
        .await;
    let elsewhere_arn = elsewhere.body["clusterArn"]
        .as_str()
        .unwrap_or_else(|| panic!("the eu-west-1 create returned no clusterArn: {}", elsewhere.body))
        .to_string();

    // Rows: every way the ARN can fail — absent and another region's (the miss),
    // not an ARN at all (the parameter refusal), and then another service, another
    // resource type, an empty region and a trailing segment: each an ARN that names
    // no cluster, which authorization refuses before anything is resolved.
    let rows: Vec<(String, u16, &str)> = vec![
        (
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/absent/11111111-2222-4333-8444-555555555555-1"),
            404,
            "NotFoundException",
        ),
        (elsewhere_arn, 404, "NotFoundException"),
        ("not-an-arn".to_string(), 400, "BadRequestException"),
        (
            format!("arn:aws:s3:{REGION}:{ACCOUNT}:cluster/present/abc-1"),
            403,
            "AccessDeniedException",
        ),
        (
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:configuration/present/abc-1"),
            403,
            "AccessDeniedException",
        ),
        (
            format!("arn:aws:kafka::{ACCOUNT}:cluster/present/abc-1"),
            403,
            "AccessDeniedException",
        ),
        (
            format!("arn:aws:kafka:{REGION}:{ACCOUNT}:cluster/present/abc-1/extra"),
            403,
            "AccessDeniedException",
        ),
    ];

    for (arn, status, code) in &rows {
        let reply = compatible_for(&emulator, arn).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (*status, *code),
            "case arn={arn:?}: expected {status} {code}, got {} {}",
            reply.status,
            reply.body
        );
    }
}

/// RULE: reporting compatible versions is a READ — it changes nothing a later
/// caller sees, and it answers the same thing every time it is asked.
#[tokio::test]
async fn reporting_compatible_versions_changes_nothing() {
    let emulator = Emulator::new();
    // Rows: a source in the middle of the ladder and one at its top, so the
    // invariant is checked both with and without targets to report.
    for (index, version) in ["3.4.0", "3.9.x"].iter().enumerate() {
        let request = cluster_on(&format!("stable-{index}"), version);
        let arn = emulator.create_ok(&request).await;
        let before = emulator.describe(&arn).await;
        let first = entries(&compatible_for(&emulator, &arn).await);
        let second = entries(&compatible_for(&emulator, &arn).await);
        let after = emulator.describe(&arn).await;
        assert_eq!(
            first, second,
            "case version={version:?}: two reads must answer the same, got {first:?} then {second:?}"
        );
        for member in [
            "/clusterInfo/currentBrokerSoftwareInfo/kafkaVersion",
            "/clusterInfo/currentVersion",
        ] {
            assert_eq!(
                before.at(member),
                after.at(member),
                "case version={version:?}: the read must not change {member}, got {} then {}",
                before.at(member),
                after.at(member)
            );
            assert_ne!(
                before.at(member),
                &Value::Null,
                "case version={version:?}: DescribeCluster must report {member}, got {}",
                before.body
            );
        }
    }
}

/// `ListKafkaVersions`, which the model binds to `GET /v1/kafka-versions`;
/// `query` is the whole query string including its `?`, or empty.
async fn list_versions(emulator: &Emulator, query: &str) -> Reply {
    let reply = emulator
        .call("GET", &format!("/v1/kafka-versions{query}"), None)
        .await;
    assert_eq!(
        reply.status, 200,
        "expected 200 from ListKafkaVersions, got {} {}",
        reply.status, reply.body
    );
    reply
}

/// The version of every ROW a page reports, in reported order.
fn page_versions(reply: &Reply) -> Vec<String> {
    reply
        .at("/kafkaVersions")
        .as_array()
        .unwrap_or_else(|| panic!("expected a kafkaVersions list, got {}", reply.body))
        .iter()
        .map(|entry| {
            entry["version"]
                .as_str()
                .unwrap_or_else(|| panic!("an entry without a version: {entry}"))
                .to_string()
        })
        .collect()
}

/// RULE: the table this operation publishes without a `ClusterArn` IS the
/// published catalog, row for row and in published order — so its source sequence
/// is exactly the version sequence `ListKafkaVersions` publishes, and a version
/// the catalog publishes on N rows is a source on N rows. Asserted from the WIRE
/// alone, so a table derived from a second, drifting list would fail here.
#[tokio::test]
async fn the_published_table_is_the_published_catalog_row_for_row() {
    let emulator = Emulator::new();
    let published = page_versions(&list_versions(&emulator, "").await);
    let answered = entries(&compatible_matrix(&emulator).await);
    let sources: Vec<String> = answered.iter().map(|(source, _)| source.clone()).collect();
    assert_eq!(
        sources, published,
        "the matrix must publish one source per published catalog row, in published order"
    );

    // Spelled out per version as well as a sequence, so a row the table drops or
    // doubles names the version it happened to.
    let mut rows_per_version: std::collections::BTreeMap<&String, usize> = Default::default();
    for version in &published {
        *rows_per_version.entry(version).or_default() += 1;
    }
    for (version, rows) in &rows_per_version {
        let answered = sources.iter().filter(|source| source == version).count();
        assert_eq!(
            answered, *rows,
            "case version={version:?}: the catalog publishes {rows} row(s) for it, so the \
             matrix must publish {rows}, got {answered}"
        );
    }
    // Both classes of row the catalog has are covered: a version published once,
    // and a version published more than once.
    assert!(
        rows_per_version.values().any(|rows| *rows == 1),
        "the catalog must publish some version on one row, got {rows_per_version:?}"
    );
    assert!(
        rows_per_version.values().any(|rows| *rows > 1),
        "the catalog must publish some version on more than one row, got {rows_per_version:?}"
    );

    // Every row of one version carries the same upgrade set: the targets are an
    // answer ABOUT the version, so publishing the version twice cannot offer two
    // different ladders from it.
    let mut targets_per_version: std::collections::BTreeMap<&String, &Vec<String>> =
        Default::default();
    for (source, targets) in &answered {
        let first = targets_per_version.entry(source).or_insert(targets);
        assert_eq!(
            *first, targets,
            "case version={source:?}: every published row of a version must carry the same \
             upgrade set, got {first:?} and {targets:?}"
        );
    }
}

/// RULE: that agreement holds however the catalog is READ — a walk of
/// `ListKafkaVersions` at any page size reports the same row sequence the table
/// publishes, so neither read path can drift from the other.
#[tokio::test]
async fn the_published_table_agrees_with_a_paged_walk_of_the_catalog() {
    let emulator = Emulator::new();
    let sources: Vec<String> = entries(&compatible_matrix(&emulator).await)
        .into_iter()
        .map(|(source, _)| source)
        .collect();

    // Rows: the page sizes a client reads the catalog at — one row at a time, a
    // size that divides the catalog unevenly, and one page for the lot.
    for page_size in [1, 7, sources.len()] {
        let mut walked: Vec<String> = Vec::new();
        let mut query = format!("?maxResults={page_size}");
        loop {
            let page = list_versions(&emulator, &query).await;
            let versions = page_versions(&page);
            assert!(
                versions.len() <= page_size,
                "case maxResults={page_size}: a page must not exceed the size asked for, got \
                 {} rows",
                versions.len()
            );
            walked.extend(versions);
            match page.body.get("nextToken").and_then(Value::as_str) {
                Some(token) => {
                    query = format!("?maxResults={page_size}&nextToken={}", pct(token));
                }
                None => break,
            }
        }
        assert_eq!(
            walked, sources,
            "case maxResults={page_size}: the walked catalog must be the table's source sequence"
        );
    }
}
