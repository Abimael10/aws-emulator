// SPDX-License-Identifier: CC0-1.0

//! WHICH Apache Kafka versions a configuration writer's per-property refusal says
//! it checked a property name against, driven end to end over the same
//! `MultiServiceRouter` the emulator serves from (the shared `kafka_harness`).
//!
//! RULE under test: a `server.properties` key is admitted or refused by the SERVICE,
//! which knows a broker property from the Apache Kafka versions it publishes — so
//! the version list the refusal quotes ("Key checked against versions: [...]") is
//! the catalog the same service publishes through `ListKafkaVersions`, each version
//! named once in published order, and it is the SAME list whatever the request's
//! `KafkaVersions` carried: a caller that pins one version is not told its key was
//! checked against that version alone.
//!
//! The rows differ along what the rule generalises over: what the request declares
//! (no `KafkaVersions` at all, one pinned version, several, the newest and an
//! older one), which writer stores the body (`CreateConfiguration`'s first revision
//! and `UpdateConfiguration`'s next one, whose configuration declared its own
//! versions at create time), and which key offends (one Apache Kafka does not have
//! and one Amazon MSK reserves).

use serde_json::{json, Value};

use crate::kafka_harness::{pct, Emulator, Reply};

/// A key Apache Kafka does not have, and a real broker property Amazon MSK manages
/// itself: both are keys no MSK configuration may set, so both are refused through
/// the per-property envelope.
const OFFENDING_KEYS: [(&str, &str); 2] = [("nosuchkey.at.all", "1"), ("broker.id", "7")];

/// The per-property envelope Amazon MSK refuses such a key with, for the key, the
/// value the body gave it and the versions the service says it checked.
///
/// Written out here rather than taken from the service's own renderer, so this
/// assertion cannot agree with a renderer that is wrong: the layout — the newlines,
/// the two-space indent, the space on each side of every `:` and inside every
/// bracket pair — is part of the message.
fn unsupported_key_message(key: &str, value: &str, checked: &str) -> String {
    format!(
        "[ {{\n  \"name\" : \"{key}\",\n  \"value\" : \"{value}\",\n  \"errorMessages\" : \
         [ \"Key '{key}' is not supported by at least one Apache Kafka version. Key checked \
         against versions: [{checked}]\" ]\n}} ]"
    )
}

/// The version list a refusal's sentence quotes, read back out of `message`, or
/// `None` when the message is not that sentence at all.
fn quoted_versions(message: &str) -> Option<&str> {
    let (_, tail) = message.split_once("Key checked against versions: [")?;
    let (versions, _) = tail.split_once(']')?;
    Some(versions)
}

/// Every version `ListKafkaVersions` publishes, each ONCE and in published order,
/// read the way a paginating client reads the catalog: page after page at two rows
/// a page, following the token each page hands back.
///
/// This is the list the refusals below must quote, taken from the service's own
/// published catalog rather than written out here, so the assertion is "the refusal
/// quotes what this service publishes" and holds whatever the catalog comes to
/// publish.
async fn published_versions(emulator: &Emulator) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    // Bounded so a token that never clears fails the suite instead of looping: the
    // catalog is a constant of the service, far shorter than this many rows.
    for _ in 0..512 {
        let query = match &token {
            Some(value) => format!("?maxResults=2&nextToken={}", pct(value)),
            None => "?maxResults=2".to_string(),
        };
        let reply: Reply = emulator
            .call("GET", &format!("/v1/kafka-versions{query}"), None)
            .await;
        assert_eq!(
            reply.status, 200,
            "expected ListKafkaVersions to be served, got {} {}",
            reply.status, reply.body
        );
        for entry in reply.at("/kafkaVersions").as_array().cloned().unwrap_or_default() {
            let version = entry["version"]
                .as_str()
                .unwrap_or_else(|| panic!("a catalog row without a version: {entry}"))
                .to_string();
            if !seen.contains(&version) {
                seen.push(version);
            }
        }
        match reply.body.get("nextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => {
                assert!(
                    !seen.is_empty(),
                    "expected the catalog to publish at least one version"
                );
                return seen;
            }
        }
    }
    panic!("the catalog walk did not end");
}

/// A `CreateConfiguration` body for `name` declaring `versions` (omitting the
/// member where the row names none) and carrying `body` as its `server.properties`.
fn create_body(name: &str, versions: Option<&[&str]>, body: &[u8]) -> Value {
    let mut request = json!({
        "name": name,
        "serverProperties": ::aws_smithy_types::base64::encode(body),
    });
    if let Some(versions) = versions {
        request["kafkaVersions"] = json!(versions);
    }
    request
}

/// RULE: the version list the refusal quotes is the service's published catalog,
/// each version once in published order — whatever the request declared.
#[tokio::test]
async fn the_refusal_quotes_the_published_catalog_whatever_the_request_declared() {
    let emulator = Emulator::new();
    let published = published_versions(&emulator).await.join(", ");

    // `(case, the KafkaVersions the request declares)`: the member omitted, one
    // version pinned, two pinned, and the newest the catalog publishes.
    let declarations: Vec<(&str, Option<Vec<&str>>)> = vec![
        ("no kafkaVersions at all", None),
        ("one pinned version", Some(vec!["3.6.0"])),
        ("another pinned version", Some(vec!["3.9.x"])),
        ("two pinned versions", Some(vec!["3.7.x", "3.9.x"])),
    ];

    for (case, versions) in &declarations {
        for (key, value) in OFFENDING_KEYS {
            let name = format!(
                "checked-{}-{}",
                key.replace('.', "-"),
                case.replace(' ', "-")
            );
            let reply = emulator
                .call(
                    "POST",
                    "/v1/configurations",
                    Some(create_body(
                        &name,
                        versions.as_deref(),
                        format!("{key}={value}\n").as_bytes(),
                    )),
                )
                .await;
            assert_eq!(
                (reply.status, reply.error_code()),
                (400, "BadRequestException"),
                "case {case:?}/{key}: expected 400 BadRequestException, got {} {}",
                reply.status,
                reply.body
            );
            assert_eq!(
                reply.error_message(),
                unsupported_key_message(key, value, &published),
                "case {case:?}/{key}: expected the per-property envelope naming the published \
                 catalog, got {:?}",
                reply.error_message()
            );
            // The same list for every row: the set is the service's, so a request
            // cannot narrow or widen it.
            assert_eq!(
                quoted_versions(reply.error_message()),
                Some(published.as_str()),
                "case {case:?}/{key}: expected the quoted versions to be request-independent, \
                 got {:?}",
                reply.error_message()
            );
        }
    }
}

/// RULE: the same list, through the other writer that stores a body — a
/// configuration created FOR one pinned version is still told which versions the
/// service checked its update's keys against.
#[tokio::test]
async fn an_updates_refusal_quotes_the_same_published_catalog() {
    let emulator = Emulator::new();
    let published = published_versions(&emulator).await.join(", ");

    // The configuration itself declares one version, so a list read off the record
    // rather than off the catalog would show up here.
    let created = emulator
        .call(
            "POST",
            "/v1/configurations",
            Some(create_body(
                "checked-versions-updated",
                Some(&["3.6.0"]),
                b"num.partitions=1\n",
            )),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "expected the control configuration to be created, got {} {}",
        created.status, created.body
    );
    let arn = created
        .at("/arn")
        .as_str()
        .expect("the create reported no ARN")
        .to_string();

    for (key, value) in OFFENDING_KEYS {
        let reply = emulator
            .call(
                "PUT",
                &format!("/v1/configurations/{}", pct(&arn)),
                Some(json!({
                    "serverProperties": ::aws_smithy_types::base64::encode(
                        format!("{key}={value}\n").as_bytes(),
                    ),
                })),
            )
            .await;
        assert_eq!(
            (reply.status, reply.error_code()),
            (400, "BadRequestException"),
            "case {key}: expected 400 BadRequestException, got {} {}",
            reply.status,
            reply.body
        );
        assert_eq!(
            reply.error_message(),
            unsupported_key_message(key, value, &published),
            "case {key}: expected the per-property envelope naming the published catalog, got {:?}",
            reply.error_message()
        );
    }

    // And the refused updates appended nothing: the configuration still reports the
    // revision its create stored.
    let described = emulator.describe_configuration(&arn).await;
    assert_eq!(
        described.at("/latestRevision/revision").as_i64(),
        Some(1),
        "a refused update must append nothing, got {}",
        described.body
    );
}
