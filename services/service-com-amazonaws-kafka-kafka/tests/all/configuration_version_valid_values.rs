// SPDX-License-Identifier: CC0-1.0

//! The valid-value list `CreateConfiguration`'s unsupported-version refusal
//! carries, driven end to end over the same `MultiServiceRouter` the emulator
//! serves from (the shared [`crate::kafka_harness`]).
//!
//! The RULE under test: when a request names an Apache Kafka version the service
//! publishes no catalogue row for, the refusal reports which versions the service
//! names as valid — its own recorded list, each version named ONCE, every one of
//! them a version the same create is then accepted with — rendered in the one
//! fixed order the service names that list in, which is neither sorted by release
//! nor the order the catalogue pages its rows in. That list is a PROPER SUBSET of
//! the catalogue `ListKafkaVersions` publishes, and so of the gate this create is
//! admitted on: a published row the sentence does not name is still a version a
//! configuration may name, because a configuration declares which versions it may
//! be applied to.
//!
//! Each suite asserts that over a TABLE whose rows differ along what the rule
//! generalises over: the FORM of the rejected value (a release newer than every
//! published one, a name that never existed, a truncated and an extended release,
//! a padded and an empty entry, a variant spelling of a published release that
//! the service does not publish, a case-folded one), its POSITION in the
//! submitted list, and how many entries the list carries. The second suite walks
//! the advertised list EXHAUSTIVELY — every version it names — because the rule
//! there is an invariant: a version the refusal advertises as retryable is one
//! the same request is then accepted with.
//!
//! The ORDER is pinned by this suite's own hand-written sequence, the MEMBERSHIP
//! against the list the refusal itself advertises on the wire, and the
//! containment against the catalogue the service publishes, so no half of the
//! list is asserted against the helper that builds it.

use serde_json::json;

use crate::kafka_harness::{pct, ConfigurationRequest, Emulator, Reply};

/// The order the service names the Apache Kafka versions in when this refusal
/// reports which versions are valid: its own fixed sequence — the newest KRaft
/// releases and the variant lines first, the oldest release last — which is
/// NEITHER ascending by release NOR the order `ListKafkaVersions` pages its rows
/// in.
///
/// Source: the list the service itself renders in this refusal, which is both the
/// versions it names as valid and the order it names them in. Written out here
/// rather than read from the service's own table, so this suite pins the list
/// instead of the code under test pinning it; it is cross-checked below against
/// the list the refusal advertises and against the catalogue `ListKafkaVersions`
/// publishes, which CONTAINS it.
const VALID_VALUE_ORDER: &[&str] = &[
    "4.2.x.kraft",
    "4.1.x.kraft",
    "2.8.2.tiered",
    "4.0.x.kraft",
    "2.4.1.1",
    "3.3.2",
    "3.5.1",
    "3.6.0",
    "3.1.1",
    "3.2.0",
    "3.3.1",
    "3.4.0",
    "2.1.0",
    "3.7.x",
    "3.8.x",
    "2.6.2",
    "2.7.1",
    "2.8.0",
    "2.6.3",
    "2.7.2",
    "2.8.1",
    "2.5.1",
    "2.6.0",
    "2.6.1",
    "2.7.0",
    "3.6.0.1",
    "3.9.x",
    "2.3.1",
    "2.4.1",
    "3.7.x.kraft",
    "2.2.1",
    "3.8.x.kraft",
    "3.9.x.kraft",
    "1.1.1",
];

/// `CreateConfiguration` (`POST /v1/configurations`) naming `versions`.
async fn create(emulator: &Emulator, name: &str, versions: &[&str]) -> Reply {
    let request = ConfigurationRequest {
        kafka_versions: versions.iter().map(|entry| (*entry).to_string()).collect(),
        ..ConfigurationRequest::named(name)
    };
    emulator
        .call("POST", "/v1/configurations", Some(request.to_json()))
        .await
}

/// `DescribeConfiguration` (`GET /v1/configurations/{Arn}`).
async fn describe(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .call("GET", &format!("/v1/configurations/{}", pct(arn)), None)
        .await
}

/// Every version `ListKafkaVersions` publishes, each ONCE, in the order the
/// catalogue publishes its rows — the service's own answer to "which versions
/// exist", read from the wire rather than from the code under test.
async fn published_versions(emulator: &Emulator) -> Vec<String> {
    let reply = emulator.call("GET", "/v1/kafka-versions", None).await;
    assert_eq!(
        reply.status, 200,
        "case \"the published catalogue\": expected ListKafkaVersions to answer, got {} {}",
        reply.status, reply.body
    );
    let entries = reply
        .at("/kafkaVersions")
        .as_array()
        .unwrap_or_else(|| panic!("expected a kafkaVersions list, got {}", reply.body))
        .clone();
    let mut versions: Vec<String> = Vec::new();
    for entry in &entries {
        let version = entry["version"]
            .as_str()
            .unwrap_or_else(|| panic!("a catalogue row without a version: {entry}"))
            .to_string();
        if !versions.contains(&version) {
            versions.push(version);
        }
    }
    assert!(
        !versions.is_empty(),
        "case \"the published catalogue\": the catalogue must publish versions, got {}",
        reply.body
    );
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

/// `values` sorted, so two renderings can be compared as SETS.
fn as_set(values: &[String]) -> Vec<String> {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    sorted
}

/// RULE: a `KafkaVersions` entry the service publishes no catalogue row for is
/// refused HTTP 400 `BadRequestException` naming `kafkaVersions`, and the refusal
/// reports the FIRST such entry verbatim together with the versions the service
/// names as valid: its own recorded list, each version once, in the one fixed
/// order the service names that list in.
#[tokio::test]
async fn the_refusal_lists_the_published_versions_in_the_services_own_order() {
    let emulator = Emulator::new();
    let published = published_versions(&emulator).await;

    // The hand-written order above must name exactly the versions the refusal
    // advertises, and every one of them must be a version the catalogue
    // publishes: the expectation below then pins the ORDER against this suite's
    // sequence, the MEMBERSHIP against the list on the wire, and the containment
    // against the catalogue on the wire.
    let ordered: Vec<String> = VALID_VALUE_ORDER.iter().map(|v| v.to_string()).collect();
    let probe = create(&emulator, "badver-list-probe", &["9.9.9"]).await;
    let probe_message = probe.error_message();
    assert_eq!(
        as_set(&ordered),
        as_set(&advertised(probe_message, "list-probe")),
        "case \"the suite's sequence\": it must name exactly the advertised versions"
    );
    for version in &ordered {
        assert!(
            published.contains(version),
            "case version={version:?}: an advertised version must be one the catalogue \
             publishes, got {published:?}"
        );
    }
    // And the containment is strict: the catalogue publishes at least one version
    // this sentence does not name, so the two lists are not the same fact — and
    // such a version is still one a create is admitted with, which the second
    // suite walks.
    assert!(
        published.iter().any(|version| !ordered.contains(version)),
        "case \"the published catalogue\": it must publish a version the advertised list omits, \
         got {published:?}"
    );

    let expected_list = VALID_VALUE_ORDER.join(", ");

    // The rows differ along the FORM of the uncatalogued entry and its POSITION
    // in the submitted list, with the entry the refusal must name beside each.
    let rows: Vec<(&str, Vec<&str>, &str)> = vec![
        ("newer-than-the-catalogue", vec!["9.9.9"], "9.9.9"),
        (
            "never-existed",
            vec!["0.0.1-does-not-exist"],
            "0.0.1-does-not-exist",
        ),
        ("truncated-release", vec!["3.9"], "3.9"),
        ("extended-release", vec!["3.6.0.9"], "3.6.0.9"),
        ("padded-release", vec![" 3.6.0"], " 3.6.0"),
        ("empty-entry", vec![""], ""),
        ("case-folded-variant", vec!["2.8.2.TIERED"], "2.8.2.TIERED"),
        // Variant spellings of releases the catalogue DOES publish, in variants
        // it does not: a spelling is a version of its own, so none of these is a
        // version the service knows.
        ("unpublished-variant", vec!["3.8.kraft"], "3.8.kraft"),
        ("unpublished-tiered", vec!["3.8.x.tiered"], "3.8.x.tiered"),
        ("unpublished-kraft", vec!["4.3.x.kraft"], "4.3.x.kraft"),
        ("unpublished-link", vec!["3.8.x.link"], "3.8.x.link"),
        // Position among entries that ARE published: first, middle, last.
        ("first-of-three", vec!["9.9.9", "2.8.1", "3.6.0"], "9.9.9"),
        (
            "middle-of-three",
            vec!["2.8.1", "3.9.link", "3.6.0"],
            "3.9.link",
        ),
        ("last-of-three", vec!["2.8.1", "3.6.0", "nope"], "nope"),
        // Two uncatalogued entries: the FIRST of them is the one named.
        (
            "two-of-four",
            vec!["2.8.1", "nope", "3.6.0", "9.9.9"],
            "nope",
        ),
    ];

    for (case, versions, named) in &rows {
        let reply = create(&emulator, &format!("badver-{case}"), versions).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "BadRequestException"),
            "case {case} ({versions:?}): expected the create to be refused, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.at("/invalidParameter").as_str(),
            Some("kafkaVersions"),
            "case {case}: the refusal must name the offending member, got {}",
            reply.body
        );
        let expected =
            format!("Unsupported KafkaVersion [{named}]. Valid values: [{expected_list}]");
        let message = reply.error_message();
        assert_eq!(
            message, expected,
            "case {case} ({versions:?}): expected {expected:?}, got {message:?}"
        );
    }

    // Not vacuous: the order the refusal renders is the service's OWN, so it is
    // neither sorted by release nor the order the catalogue publishes its rows
    // in — a list rendered in either of those orders would pass neither check.
    let probe = create(&emulator, "badver-order-probe", &["9.9.9"]).await;
    let listed = advertised(probe.error_message(), "order-probe");
    assert_ne!(
        listed, published,
        "case \"order-probe\": the refusal's order must not be the catalogue's publication order"
    );
    assert_ne!(
        listed,
        as_set(&listed),
        "case \"order-probe\": the refusal's order must not be a sort of its own entries"
    );
    // And the list is not a per-run value: the same question asked again, and a
    // different rejected version, get the same sequence.
    let again = create(&emulator, "badver-order-probe-2", &["8.8.8"]).await;
    assert_eq!(
        advertised(again.error_message(), "order-probe-2"),
        listed,
        "case \"order-probe-2\": the valid-value list must not change between calls"
    );
}

/// RULE: every version the refusal advertises is one the very same create is
/// accepted with — and stores verbatim — and so is every OTHER version the
/// catalogue publishes, including the published rows the sentence does not name,
/// because a configuration declares the versions it may be applied to. A version
/// the catalogue publishes no row for is advertised by no refusal and accepted by
/// no create.
///
/// Walked EXHAUSTIVELY over the published catalogue, which is the whole input
/// space of the invariant.
#[tokio::test]
async fn every_advertised_version_is_one_a_create_accepts() {
    let emulator = Emulator::new();

    let refusal = create(&emulator, "advertised-probe", &["9.9.9"]).await;
    assert_eq!(
        (refusal.status, refusal.error_code()),
        (400, "BadRequestException"),
        "case \"advertised-probe\": expected a refusal to read the list from, got {} {}",
        refusal.status,
        refusal.body
    );
    let listed = advertised(refusal.error_message(), "advertised-probe");
    assert!(
        listed.len() > 1,
        "case \"advertised-probe\": expected the published versions, got {listed:?}"
    );

    // The advertised set is a subset of the catalogue's, each version named once,
    // and a proper one: the catalogue publishes a row the sentence does not name.
    let published = published_versions(&emulator).await;
    for version in &listed {
        assert!(
            published.contains(version),
            "case version={version:?}: an advertised version must be published, got {published:?}"
        );
    }
    assert!(
        published.iter().any(|version| !listed.contains(version)),
        "case \"advertised-probe\": the catalogue must publish a version the advertised list \
         omits, got {published:?}"
    );
    let mut distinct = as_set(&listed);
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        listed.len(),
        "case \"advertised-probe\": each version is named once, got {listed:?}"
    );

    // Every version the catalogue publishes is retryable: the same create naming
    // it succeeds and the stored configuration reports it back. Walked over the
    // whole published catalogue, which is the gate — so it covers every version
    // the refusal advertises AND the published rows it does not name.
    for version in &published {
        let advertised_here = listed.contains(version);
        let name = format!("published-{}", version.replace('.', "-"));
        let reply = create(&emulator, &name, &[version.as_str()]).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (200, ""),
            "case version={version:?} (advertised={advertised_here}): a published version must \
             be accepted, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.body["state"].as_str(),
            Some("ACTIVE"),
            "case version={version:?}: a created configuration is ACTIVE, got {}",
            reply.body
        );
        let arn = reply.body["arn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case version={version:?}: CreateConfiguration returned no arn: {}", reply.body)
            })
            .to_string();
        let described = describe(&emulator, &arn).await;
        assert_eq!(
            described.body["kafkaVersions"],
            json!([version]),
            "case version={version:?}: the stored configuration must report it, got {}",
            described.body
        );
    }

    // And a version the catalogue publishes no row for is advertised by nothing
    // and accepted by nothing — variant spellings of releases the catalogue does
    // publish, each naming a build of its own, a truncated patch name and a
    // release newer than everything published.
    for version in ["3.8.x.link", "3.8.x.tiered", "3.9.x.kraft.link", "9.9.9", "3.9"] {
        assert!(
            !published.iter().any(|entry| entry == version),
            "case version={version:?}: it must not be published, got {published:?}"
        );
        assert!(
            !listed.iter().any(|entry| entry == version),
            "case version={version:?}: an unpublished version must not be advertised, \
             got {listed:?}"
        );
        let name = format!("unpublished-{}", version.replace('.', "-"));
        let reply = create(&emulator, &name, &[version]).await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "BadRequestException"),
            "case version={version:?}: an unpublished version must be refused, got {} {}",
            reply.status,
            reply.body
        );
        // The refusal stored nothing: the name is still free.
        let listed_again = emulator
            .call("GET", "/v1/configurations?maxResults=100", None)
            .await;
        assert!(
            !listed_again.at("/configurations")
                .as_array()
                .map(|entries| entries
                    .iter()
                    .any(|entry| entry["name"].as_str() == Some(name.as_str())))
                .unwrap_or_default(),
            "case version={version:?}: a refused create must store nothing, got {}",
            listed_again.body
        );
    }
}
