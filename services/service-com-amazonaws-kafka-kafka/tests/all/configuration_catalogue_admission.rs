// SPDX-License-Identifier: CC0-1.0

//! What `CreateConfiguration` ADMITS in `KafkaVersions`, driven end to end over
//! the same `MultiServiceRouter` the emulator serves from (the shared
//! [`crate::kafka_harness`]).
//!
//! The RULE under test: a configuration declares which Apache Kafka versions a
//! stored `server.properties` body may be APPLIED to, so it may name every
//! version this service publishes a catalogue row for — whatever lifecycle status
//! that row carries, since a configuration is applied to clusters still running
//! an older version — and only a version the catalogue publishes no row for is
//! refused. The valid-value list the refusal advertises is the service's own
//! recorded sentence and a NARROWER fact: a published version it does not name is
//! still one a create is accepted with.
//!
//! Each suite asserts that over a TABLE whose rows differ along what the rule
//! generalises over: how a client built its list out of the catalogue it just
//! read (the whole catalogue sorted, the whole catalogue in published order, the
//! deprecated rows only, the offered rows only, one version at a time, and a
//! single entry), the LIFECYCLE STATUS of the versions named, whether the
//! versions named are ones the refusal advertises, and — for the refusals — the
//! FORM of the unpublished entry and its POSITION in the submitted list.
//!
//! Every expectation is read off the WIRE: the catalogue comes from
//! `ListKafkaVersions` and the advertised list from the refusal itself, so
//! neither side is asserted against the helper that computes it.

use serde_json::json;

use crate::kafka_harness::{pct, ConfigurationRequest, Emulator, Reply, REGION};

/// A generous ceiling on the pages a catalogue walk may take, so a listing that
/// never closes fails here instead of looping forever.
const PAGE_CEILING: usize = 1024;

/// `CreateConfiguration` (`POST /v1/configurations`) naming `versions`.
async fn create(emulator: &Emulator, name: &str, versions: &[String]) -> Reply {
    let request = ConfigurationRequest {
        kafka_versions: versions.to_vec(),
        // The body the recorded flow submits: a single broker property MSK
        // configures.
        server_properties: b"allow.everyone.if.no.acl.found=false\n".to_vec(),
        ..ConfigurationRequest::named(name)
    };
    emulator
        .call("POST", "/v1/configurations", Some(request.to_json()))
        .await
}

/// Every `(version, status)` row `ListKafkaVersions` publishes, walked one page
/// at a time so the answer is the whole catalogue and not one page of it.
async fn published_rows(emulator: &Emulator) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    let mut query = "?maxResults=5".to_string();
    for _ in 0..PAGE_CEILING {
        let page = emulator
            .call("GET", &format!("/v1/kafka-versions{query}"), None)
            .await;
        assert_eq!(
            page.status, 200,
            "case \"the published catalogue\": expected ListKafkaVersions to answer, got {} {}",
            page.status, page.body
        );
        for entry in page
            .at("/kafkaVersions")
            .as_array()
            .unwrap_or_else(|| panic!("expected a kafkaVersions list, got {}", page.body))
        {
            let version = entry["version"]
                .as_str()
                .unwrap_or_else(|| panic!("a catalogue row without a version: {entry}"));
            let status = entry["status"]
                .as_str()
                .unwrap_or_else(|| panic!("a catalogue row without a status: {entry}"));
            rows.push((version.to_string(), status.to_string()));
        }
        match page.body.get("nextToken").and_then(serde_json::Value::as_str) {
            Some(token) => query = format!("?maxResults=5&nextToken={}", pct(token)),
            None => {
                assert!(
                    !rows.is_empty(),
                    "case \"the published catalogue\": it must publish rows, got {}",
                    page.body
                );
                return rows;
            }
        }
    }
    panic!("case \"the published catalogue\": the walk never closed, got {rows:?}");
}

/// Every version the catalogue publishes, each ONCE, in the order its first row
/// appears — what a client enumerating the catalogue holds.
fn distinct(rows: &[(String, String)]) -> Vec<String> {
    let mut versions: Vec<String> = Vec::new();
    for (version, _) in rows {
        if !versions.contains(version) {
            versions.push(version.clone());
        }
    }
    versions
}

/// The versions published under `status`, each once, in published order.
fn with_status(rows: &[(String, String)], status: &str) -> Vec<String> {
    let mut versions: Vec<String> = Vec::new();
    for (version, row_status) in rows {
        if row_status == status && !versions.contains(version) {
            versions.push(version.clone());
        }
    }
    versions
}

/// The versions a refusal advertises as valid, in the order it named them.
fn advertised(message: &str, case: &str) -> Vec<String> {
    let (_, tail) = message.split_once("Valid values: [").unwrap_or_else(|| {
        panic!("case {case}: the refusal must carry a valid-value list, got {message:?}")
    });
    tail.strip_suffix(']')
        .unwrap_or_else(|| {
            panic!("case {case}: the valid-value list must be closed, got {message:?}")
        })
        .split(", ")
        .map(str::to_string)
        .collect()
}

/// The advertised valid-value list, read off the wire from a refusal.
async fn advertised_list(emulator: &Emulator) -> Vec<String> {
    let probe = create(
        emulator,
        "catalogue-advert-probe",
        &["0.0.1-does-not-exist".to_string()],
    )
    .await;
    assert_eq!(
        (probe.status, probe.error_code()),
        (400, "BadRequestException"),
        "case \"the advertised list\": expected a refusal to read it from, got {} {}",
        probe.status,
        probe.body
    );
    advertised(probe.error_message(), "the advertised list")
}

/// A configuration name built from a case label: the service admits ASCII
/// alphanumerics, `.`, `_` and `-` only, so a version string's dots become
/// dashes.
fn configuration_name(label: &str) -> String {
    format!("cfg-{}", label.replace(['.', ' '], "-"))
}

/// RULE: a `KafkaVersions` list built out of the catalogue the same service just
/// published is accepted, however the client assembled it — sorted, in published
/// order, filtered by lifecycle status, or one version at a time — and the stored
/// configuration reports the list back verbatim.
#[tokio::test]
async fn a_configuration_may_name_every_version_the_catalogue_publishes() {
    let emulator = Emulator::new();
    let rows = published_rows(&emulator).await;
    let published = distinct(&rows);
    let deprecated = with_status(&rows, "DEPRECATED");
    let offered = with_status(&rows, "ACTIVE");
    assert!(
        !deprecated.is_empty() && !offered.is_empty(),
        "the catalogue must publish both lifecycle statuses, got {rows:?}"
    );

    // The whole catalogue, SORTED, is how a client that reduced the published
    // rows to a version set holds it — the way the catalogue is enumerated in
    // practice, and an order that is not the catalogue's own.
    let mut sorted = published.clone();
    sorted.sort_unstable();

    let mut table: Vec<(String, Vec<String>)> = vec![
        ("whole-catalogue-sorted".to_string(), sorted.clone()),
        ("whole-catalogue-published-order".to_string(), published.clone()),
        ("deprecated-rows-only".to_string(), deprecated.clone()),
        ("offered-rows-only".to_string(), offered.clone()),
        (
            "two-versions".to_string(),
            vec![
                published.first().expect("a published version").clone(),
                published.last().expect("a published version").clone(),
            ],
        ),
    ];
    // And one row per published version on its own, so every row of the
    // catalogue — the variant lines and the deprecated ones included — is
    // exercised as a list of its own.
    for version in &published {
        table.push((format!("single-{version}"), vec![version.clone()]));
    }

    for (case, versions) in &table {
        let name = configuration_name(case);
        let reply = create(&emulator, &name, versions).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (200, ""),
            "case {case} ({versions:?}): a list of published versions must be accepted, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/state").as_str(),
            Some("ACTIVE"),
            "case {case}: a created configuration is ACTIVE, got {}",
            reply.body
        );
        assert_eq!(
            reply.at("/latestRevision/revision").as_i64(),
            Some(1),
            "case {case}: the submitted body is revision 1, got {}",
            reply.body
        );
        let arn = reply
            .at("/arn")
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the create returned no arn: {}", reply.body))
            .to_string();
        assert!(
            arn.contains(REGION),
            "case {case}: the ARN must be minted for the signing region, got {arn}"
        );
        // Read the record back through an implemented reader: the versions are
        // stored as submitted, in the order submitted.
        let described = emulator.describe_configuration(&arn).await;
        assert_eq!(
            described.at("/kafkaVersions"),
            &json!(versions),
            "case {case}: DescribeConfiguration must report the versions submitted, got {}",
            described.body
        );
    }
}

/// RULE: the gate is the published catalogue, which is WIDER than the valid-value
/// list the refusal advertises — so every published version that list does not
/// name is accepted all the same, while a version the catalogue publishes no row
/// for is refused HTTP 400 `BadRequestException` naming `kafkaVersions`, with the
/// service's sentence over the FIRST such entry and the versions it advertises.
///
/// Walked EXHAUSTIVELY over the published versions the advertised list omits,
/// which is the whole input space of the first half.
#[tokio::test]
async fn the_gate_is_wider_than_the_list_the_refusal_advertises() {
    let emulator = Emulator::new();
    let published = distinct(&published_rows(&emulator).await);
    let listed = advertised_list(&emulator).await;

    // The advertised list is contained in the catalogue, and strictly: there is
    // at least one published version it does not name, which is what makes the
    // two separate facts.
    for version in &listed {
        assert!(
            published.contains(version),
            "case version={version:?}: an advertised version must be published, got {published:?}"
        );
    }
    let omitted: Vec<String> = published
        .iter()
        .filter(|version| !listed.contains(version))
        .cloned()
        .collect();
    assert!(
        !omitted.is_empty(),
        "the catalogue must publish a version the advertised list omits, got {published:?} and \
         {listed:?}"
    );

    // Every one of them is accepted, alone and beside an advertised version: a
    // version a client can only have learnt from this service's own catalogue is
    // never refused by the next call.
    let advertised_version = listed.first().expect("the advertised list is not empty");
    for version in &omitted {
        for (shape, versions) in [
            ("alone", vec![version.clone()]),
            (
                "beside-an-advertised-version",
                vec![advertised_version.clone(), version.clone()],
            ),
        ] {
            let case = format!("{version}-{shape}");
            let name = configuration_name(&case);
            let reply = create(&emulator, &name, &versions).await;
            assert_eq!(
                (reply.status, reply.error_code()),
                (200, ""),
                "case {case} ({versions:?}): a published version the advertised list omits must \
                 still be accepted, got {} {}",
                reply.status,
                reply.body
            );
            let arn = reply
                .at("/arn")
                .as_str()
                .unwrap_or_else(|| panic!("case {case}: the create returned no arn: {}", reply.body))
                .to_string();
            let described = emulator.describe_configuration(&arn).await;
            assert_eq!(
                described.at("/kafkaVersions"),
                &json!(versions),
                "case {case}: the stored configuration must report the versions submitted, got {}",
                described.body
            );
        }
    }

    // A version the catalogue publishes no row for names no broker software, so
    // it is refused — wherever it sits among published entries. The rows differ
    // along the FORM of the entry (a release newer than everything published, a
    // name that never existed, a variant spelling the catalogue does not publish,
    // a truncated release, a padded and an empty entry) and its POSITION.
    let first_published = published.first().expect("a published version").clone();
    let refused: Vec<(&str, Vec<String>, String)> = vec![
        ("newer-than-everything", vec!["9.9.9".to_string()], "9.9.9".to_string()),
        (
            "never-existed",
            vec!["0.0.1-does-not-exist".to_string()],
            "0.0.1-does-not-exist".to_string(),
        ),
        (
            "unpublished-variant",
            vec!["3.8.x.link".to_string()],
            "3.8.x.link".to_string(),
        ),
        ("truncated-release", vec!["3.9".to_string()], "3.9".to_string()),
        (
            "padded-published-name",
            vec![format!(" {first_published}")],
            format!(" {first_published}"),
        ),
        ("empty-entry", vec![String::new()], String::new()),
        (
            "last-of-three",
            vec![
                first_published.clone(),
                advertised_version.clone(),
                "8.8.8".to_string(),
            ],
            "8.8.8".to_string(),
        ),
        (
            "first-of-two-unpublished",
            vec!["7.7.7".to_string(), first_published.clone(), "6.6.6".to_string()],
            "7.7.7".to_string(),
        ),
    ];

    let expected_list = listed.join(", ");
    for (case, versions, named) in &refused {
        assert!(
            !published.contains(named),
            "case {case}: {named:?} must not be a published version, got {published:?}"
        );
        let name = configuration_name(case);
        let reply = create(&emulator, &name, versions).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "BadRequestException"),
            "case {case} ({versions:?}): an unpublished version must be refused, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/invalidParameter").as_str(),
            Some("kafkaVersions"),
            "case {case}: the refusal must name the offending member, got {}",
            reply.body
        );
        let expected = format!("Unsupported KafkaVersion [{named}]. Valid values: [{expected_list}]");
        assert_eq!(
            reply.error_message(),
            expected,
            "case {case} ({versions:?}): expected {expected:?}, got {:?}",
            reply.error_message()
        );
        assert!(
            !emulator.listed_configuration_names().await.contains(&name),
            "case {case}: a refused create must store nothing"
        );
    }
}
