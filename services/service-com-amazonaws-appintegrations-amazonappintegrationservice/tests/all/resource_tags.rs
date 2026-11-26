// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the AppIntegrations tagging operations
//! (`TagResource` + `ListTagsForResource` + `UntagResource`).
//!
//! The rule under test: a tag set is part of the record of the resource it is on, so
//! `TagResource` merges the request's keys into that resource's tag set,
//! `UntagResource` removes the keys it names from it, and `ListTagsForResource` reports
//! exactly what is stored there — the same copy the resource's own `Get` reports.
//!
//! An ARN this account holds no taggable resource under is refused by all three, but NOT
//! with one refusal: the tag WRITES answer the modelled `ResourceNotFoundException` and
//! the tagging surface's resource-type-agnostic sentence, which carries the `ResourceArn`
//! the request sent, while the tag READ answers the code AWS's tagging front end states,
//! `NotFoundException`, with a CONSTANT message that carries nothing from the request or
//! from state. Neither refusal names a resource type, because a tagging operation is
//! handed an arbitrary ARN and resolves no type before refusing.
//!
//! The suite is a table: rows differ in the resource type the ARN names (every resource
//! of this service whose create takes `Tags`), in tag count, in key and value form and
//! length (both published bounds of `TagKey` and `TagValue`), and in which keys an untag
//! names (a subset, a key the resource does not carry, all of them). The recorded
//! tag/list/untag round trip is one row.
//!
//! The success STATUS of the two tagging mutations is part of that rule: both declare an
//! output with no members and AWS answers them with 204 No Content, so every successful
//! `TagResource` and `UntagResource` answers 204 and nothing to parse, whatever resource
//! type the ARN names and whatever the tag set or the key list is — while the other
//! empty-output operations of this service keep the default success status.
//!
//! Every assertion goes over the wire through the same router the production emulator
//! mounts (see `harness.rs`); nothing reaches into the service's state struct, so a
//! response that is not computed from persisted state cannot pass.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::harness::{encode_label, Emulator, Reply, REGION};

/// The canonical account id every emulated ARN carries (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// `TagKey` is `len 1..128`, `TagValue` is `len 0..256`
/// (`smithy_query(query="shape", shape="TagKey,TagValue")`).
const TAG_KEY_MAX: usize = 128;
const TAG_VALUE_MAX: usize = 256;

/// The most tags one resource carries: the maximum the modelled `TagMap` admits
/// (`smithy_query(query="shape", shape="TagMap")`: `len 1..200`).
const MAX_TAGS_PER_RESOURCE: usize = 200;

/// What a successful tagging mutation of this service answers: 204 No Content. Both
/// tagging operations declare an output with no members
/// (`smithy_query(query="operation", operation="TagResource,UntagResource")`) and AWS
/// answers them with the no-content status rather than the protocol default the model
/// falls back to when it states no explicit success code.
const TAGGING_SUCCESS_STATUS: u16 = 204;

/// The success status of an operation of this service whose output DOES carry members, and
/// of the empty-output operations outside the tagging surface: the protocol default
/// (`smithy_query(query="operation", ...)`: `http.code` 200 on every operation of this
/// model).
const DEFAULT_SUCCESS_STATUS: u16 = 200;

/// The error code the tag READ (`ListTagsForResource`) states for a `resourceArn` that
/// addresses no taggable resource of this account: AWS serves that ARN lookup through a
/// tagging front end with its own taxonomy, so the code is NOT the
/// `ResourceNotFoundException` the operation's model lists (recorded behavior — the model
/// declares no such shape on this operation).
const TAG_READ_NOT_FOUND_CODE: &str = "NotFoundException";

/// The message that refusal carries: entirely fixed text, with nothing interpolated from
/// the request or from state, and no trailing period.
const TAG_READ_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The error code the tag WRITES (`TagResource`, `UntagResource`) state for the same ARN:
/// the one their model lists.
const TAG_WRITE_NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// The sentence the tag writes state for an ARN that addresses no taggable resource:
/// fixed text, then the `ResourceArn` the request sent, verbatim, then a period.
fn tag_write_not_found_message(arn: &str) -> String {
    format!("Could not find resource with Arn: {arn}.")
}

/// The HTTP status of an absent resource, which both tagging refusals carry.
const NOT_FOUND_STATUS: u16 = 404;

/// Assert that one tagging mutation answered exactly as AWS answers a successful one: the
/// no-content status, with no content members to parse.
fn assert_no_content_success(case: &str, operation: &str, reply: &Reply) {
    assert_eq!(
        reply.status, TAGGING_SUCCESS_STATUS,
        "case {case:?}: expected {operation} to answer HTTP {TAGGING_SUCCESS_STATUS}, \
         got {} / {}",
        reply.status, reply.body
    );
    assert!(
        reply
            .body
            .as_object()
            .is_none_or(serde_json::Map::is_empty),
        "case {case:?}: expected {operation} to answer no content members, got {}",
        reply.body
    );
}

/// One taggable resource of this service, as the wire reports it: the ARN the tagging
/// operations address it by, and the URI its own reader answers on.
#[derive(Debug, Clone)]
struct Taggable {
    /// What names this resource in an assertion.
    kind: &'static str,
    arn: String,
    /// `GET <get_uri>` reports this resource, `Tags` included.
    get_uri: String,
}

/// A `@httpQuery` list travels one `tagKeys=` pair per key, and a key can carry `+`,
/// `=`, `:` and `/` (`TagKey` pattern `^(?!aws:)[a-zA-Z+-=._:/]+$`), so every key is
/// percent-encoded rather than left for the query parser to read as a separator.
fn tag_keys_query(keys: &[&str]) -> String {
    keys.iter()
        .map(|key| format!("tagKeys={}", urlencoding::encode(key)))
        .collect::<Vec<_>>()
        .join("&")
}

/// `GET /tags/{resourceArn}`.
async fn list_tags(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .send(
            "GET",
            &format!("/tags/{}", encode_label(arn)),
            REGION,
            None,
        )
        .await
}

/// `POST /tags/{resourceArn}` with the request's `tags`.
async fn tag(emulator: &Emulator, arn: &str, tags: Value) -> Reply {
    emulator
        .send(
            "POST",
            &format!("/tags/{}", encode_label(arn)),
            REGION,
            Some(json!({ "tags": tags })),
        )
        .await
}

/// `DELETE /tags/{resourceArn}?tagKeys=...`.
async fn untag(emulator: &Emulator, arn: &str, keys: &[&str]) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!(
                "/tags/{}?{}",
                encode_label(arn),
                tag_keys_query(keys)
            ),
            REGION,
            None,
        )
        .await
}

/// The `tags`/`Tags` member of `reply`, as a comparable map.
fn tag_map(case: &str, reply: &Reply, member: &str) -> BTreeMap<String, String> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected HTTP 200 reporting {member}, got {} / {}",
        reply.status, reply.body
    );
    let value = reply.field(member);
    assert!(
        value.is_some_and(Value::is_object),
        "case {case:?}: expected a {member} map, got {:?}",
        reply.body
    );
    value
        .and_then(Value::as_object)
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value.as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The tag set `ListTagsForResource` reports for `resource`.
async fn reported_tags(
    emulator: &Emulator,
    case: &str,
    resource: &Taggable,
) -> BTreeMap<String, String> {
    let reply = list_tags(emulator, &resource.arn).await;
    tag_map(case, &reply, "tags")
}

/// The tag set the resource's OWN reader reports — the same stored copy, read through a
/// different operation, so a tag written into a second place beside the record would
/// show up as a difference.
async fn tags_from_own_reader(
    emulator: &Emulator,
    case: &str,
    resource: &Taggable,
) -> BTreeMap<String, String> {
    let reply = emulator.send("GET", &resource.get_uri, REGION, None).await;
    tag_map(case, &reply, "Tags")
}

/// Create one application, optionally with `Tags` on the create.
async fn application(emulator: &Emulator, name: &str, tags: Option<Value>) -> Taggable {
    let mut body = json!({
        "Name": name,
        "Namespace": name,
        "ApplicationSourceConfig": {
            "ExternalUrlConfig": { "AccessUrl": "https://example.com/app" }
        },
    });
    if let Some(tags) = tags {
        body["Tags"] = tags;
    }
    let created = emulator.send("POST", "/applications", REGION, Some(body)).await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: the application under test must be created, got {} / {}",
        created.status, created.body
    );
    let arn = created
        .string("Arn")
        .expect("CreateApplication reports the minted Arn");
    Taggable {
        kind: "application",
        get_uri: format!("/applications/{}", encode_label(&arn)),
        arn,
    }
}

/// Create one event integration, optionally with `Tags` on the create.
async fn event_integration(emulator: &Emulator, name: &str, tags: Option<Value>) -> Taggable {
    let mut body = json!({
        "Name": name,
        "EventBridgeBus": "default",
        "EventFilter": { "Source": "aws.partner/demo" },
    });
    if let Some(tags) = tags {
        body["Tags"] = tags;
    }
    let created = emulator
        .send("POST", "/eventIntegrations", REGION, Some(body))
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: the event integration under test must be created, got {} / {}",
        created.status, created.body
    );
    let arn = created
        .string("EventIntegrationArn")
        .expect("CreateEventIntegration reports the minted ARN");
    Taggable {
        kind: "event integration",
        get_uri: format!("/eventIntegrations/{}", encode_label(name)),
        arn,
    }
}

/// Create one data integration, optionally with `Tags` on the create.
async fn data_integration(emulator: &Emulator, name: &str, tags: Option<Value>) -> Taggable {
    let kms_key = emulator.create_kms_key(REGION).await;
    let mut body = json!({ "Name": name, "KmsKey": kms_key });
    if let Some(tags) = tags {
        body["Tags"] = tags;
    }
    let created = emulator
        .send("POST", "/dataIntegrations", REGION, Some(body))
        .await;
    assert_eq!(
        created.status, 200,
        "case {name:?}: the data integration under test must be created, got {} / {}",
        created.status, created.body
    );
    let arn = created
        .string("Arn")
        .expect("CreateDataIntegration reports the minted Arn");
    let id = created
        .string("Id")
        .expect("CreateDataIntegration reports the minted Id");
    Taggable {
        kind: "data integration",
        get_uri: format!("/dataIntegrations/{}", encode_label(&id)),
        arn,
    }
}

/// Every resource type of this service whose create takes `Tags`
/// (`smithy_query(query="operation",
/// operation="CreateApplication,CreateEventIntegration,CreateDataIntegration")`), each
/// created untagged under a name derived from `row`.
async fn taggable_resources(emulator: &Emulator, row: usize) -> Vec<Taggable> {
    vec![
        application(emulator, &format!("tagged.application-{row}"), None).await,
        event_integration(emulator, &format!("tagged.event-integration-{row}"), None).await,
        data_integration(emulator, &format!("tagged.data-integration-{row}"), None).await,
    ]
}

/// One tag set a request can carry: the label that names it in an assertion, and the
/// entries themselves.
fn tag_cases() -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        // The shape the recorded round trip carries: one row of many.
        (
            "one tag",
            vec![("owner".to_string(), "team-a".to_string())],
        ),
        (
            "several tags",
            vec![
                ("owner".to_string(), "team-a".to_string()),
                ("env".to_string(), "prod".to_string()),
                ("cost-center".to_string(), "4711".to_string()),
            ],
        ),
        (
            // `TagValue` is len 0..256, so the empty value is a legal value and not an
            // absent tag.
            "an empty value",
            vec![("retired".to_string(), String::new())],
        ),
        (
            // Both published maxima: `TagKey` len 128, `TagValue` len 256.
            "key and value at their maximum length",
            vec![("k".repeat(TAG_KEY_MAX), "v".repeat(TAG_VALUE_MAX))],
        ),
        (
            // Every punctuation class `TagKey`'s pattern admits, plus digits, which the
            // pattern's `+-=` range covers.
            "punctuation and digits in the key",
            vec![(
                "a+b-c=d.e_f:g/h0".to_string(),
                "unicode value: ünïcödé".to_string(),
            )],
        ),
    ]
}

/// The entries as the JSON a request carries.
fn tags_body(entries: &[(String, String)]) -> Value {
    Value::Object(
        entries
            .iter()
            .map(|(key, value)| (key.clone(), Value::String(value.clone())))
            .collect(),
    )
}

/// The entries as the map a response must report.
fn expected_map(entries: &[(String, String)]) -> BTreeMap<String, String> {
    entries.iter().cloned().collect()
}

#[tokio::test]
async fn the_tag_set_tag_resource_wrote_is_what_every_reader_reports() {
    let emulator = Emulator::new();

    for (row, (label, entries)) in tag_cases().into_iter().enumerate() {
        for resource in taggable_resources(&emulator, row).await {
            let case = format!("{label} on a {}", resource.kind);

            // An untagged resource reports the EMPTY tag set, never an absent member.
            assert_eq!(
                reported_tags(&emulator, &case, &resource).await,
                BTreeMap::new(),
                "case {case:?}: an untagged resource must report an empty tag set"
            );

            let tagged = tag(&emulator, &resource.arn, tags_body(&entries)).await;
            assert_no_content_success(&case, "TagResource", &tagged);

            let expected = expected_map(&entries);
            let listed = reported_tags(&emulator, &case, &resource).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected ListTagsForResource to report {expected:?}, got {listed:?}"
            );

            // The same stored copy, through the resource's own reader.
            let own = tags_from_own_reader(&emulator, &case, &resource).await;
            assert_eq!(
                own, expected,
                "case {case:?}: expected the resource's own reader to report {expected:?}, \
                 got {own:?}"
            );
        }
    }
}

#[tokio::test]
async fn tagging_merges_keys_and_overwrites_the_value_of_a_key_already_carried() {
    let emulator = Emulator::new();

    // Rows: the tags the create carried, then the tags one `TagResource` adds, then what
    // the resource must carry — a key the resource already has takes the new value, a key
    // it does not have is added, and a key the request does not name is left alone.
    let created_tags = json!({ "owner": "team-a", "env": "dev" });
    for resource in [
        application(&emulator, "merged.application", Some(created_tags.clone())).await,
        event_integration(
            &emulator,
            "merged.event-integration",
            Some(created_tags.clone()),
        )
        .await,
        data_integration(
            &emulator,
            "merged.data-integration",
            Some(created_tags.clone()),
        )
        .await,
    ] {
        let case = format!("merge on a {}", resource.kind);

        // What the create persisted is readable before any TagResource call.
        let listed = reported_tags(&emulator, &case, &resource).await;
        let expected: BTreeMap<String, String> = [("owner", "team-a"), ("env", "dev")]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the create's tags {expected:?}, got {listed:?}"
        );

        let tagged = tag(
            &emulator,
            &resource.arn,
            json!({ "env": "prod", "tier": "gold" }),
        )
        .await;
        assert_no_content_success(&case, "TagResource", &tagged);

        let expected: BTreeMap<String, String> =
            [("owner", "team-a"), ("env", "prod"), ("tier", "gold")]
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
        let listed = reported_tags(&emulator, &case, &resource).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected {expected:?} after the merge, got {listed:?}"
        );

        // Tagging twice with the same request leaves one entry per key.
        tag(
            &emulator,
            &resource.arn,
            json!({ "env": "prod", "tier": "gold" }),
        )
        .await;
        let listed = reported_tags(&emulator, &case, &resource).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected a repeated TagResource to change nothing, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn untagging_removes_only_the_keys_it_names() {
    let emulator = Emulator::new();

    // Rows differ in which keys the untag names: one of them, several, a key the
    // resource does not carry, a mix of carried and absent, and every key at once.
    let carried = [("owner", "team-a"), ("env", "prod"), ("tier", "gold")];
    let rows: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        ("one carried key", vec!["env"], vec!["owner", "tier"]),
        (
            "several carried keys",
            vec!["env", "tier"],
            vec!["owner"],
        ),
        (
            "a key the resource does not carry",
            vec!["absent"],
            vec!["env", "owner", "tier"],
        ),
        (
            "a carried key beside an absent one",
            vec!["owner", "absent"],
            vec!["env", "tier"],
        ),
        ("every key at once", vec!["owner", "env", "tier"], vec![]),
    ];

    for (row, (label, keys, remaining)) in rows.into_iter().enumerate() {
        let tags = Value::Object(
            carried
                .iter()
                .map(|(key, value)| (key.to_string(), Value::String(value.to_string())))
                .collect(),
        );
        for resource in [
            application(
                &emulator,
                &format!("untagged.application-{row}"),
                Some(tags.clone()),
            )
            .await,
            event_integration(
                &emulator,
                &format!("untagged.event-integration-{row}"),
                Some(tags.clone()),
            )
            .await,
            data_integration(
                &emulator,
                &format!("untagged.data-integration-{row}"),
                Some(tags.clone()),
            )
            .await,
        ] {
            let case = format!("{label} on a {}", resource.kind);
            let expected: BTreeMap<String, String> = remaining
                .iter()
                .map(|key| {
                    let value = carried
                        .iter()
                        .find(|(carried_key, _)| carried_key == key)
                        .map(|(_, value)| value.to_string())
                        .unwrap_or_default();
                    (key.to_string(), value)
                })
                .collect();

            let untagged = untag(&emulator, &resource.arn, &keys).await;
            assert_no_content_success(&case, "UntagResource", &untagged);

            let listed = reported_tags(&emulator, &case, &resource).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected {expected:?} after removing {keys:?}, got {listed:?}"
            );

            // Removing the same keys again removes nothing more.
            untag(&emulator, &resource.arn, &keys).await;
            let listed = reported_tags(&emulator, &case, &resource).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected a repeated UntagResource to change nothing, \
                 got {listed:?}"
            );

            // And the resource's own reader reports the same remainder.
            let own = tags_from_own_reader(&emulator, &case, &resource).await;
            assert_eq!(
                own, expected,
                "case {case:?}: expected the resource's own reader to report {expected:?}, \
                 got {own:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_tag_set_belongs_to_one_resource_only() {
    let emulator = Emulator::new();

    // Three resources of three types, stored side by side: tagging one must leave the
    // others exactly as they were.
    let resources = taggable_resources(&emulator, 0).await;
    let subject = resources[0].clone();
    tag(&emulator, &subject.arn, json!({ "owner": "team-a" })).await;

    for resource in &resources[1..] {
        let case = format!("neighbour {} of a tagged application", resource.kind);
        let listed = reported_tags(&emulator, &case, resource).await;
        assert_eq!(
            listed,
            BTreeMap::new(),
            "case {case:?}: expected an untouched neighbour to report no tags, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn an_arn_no_taggable_resource_carries_is_reported_as_not_found() {
    let emulator = Emulator::new();
    // Stored neighbours of each type, so a 404 is about the ARN the request named and
    // not about an empty store.
    let stored = taggable_resources(&emulator, 0).await;
    assert_eq!(
        stored.len(),
        3,
        "the neighbours of every taggable type must be stored, got {}",
        stored.len()
    );

    // Rows: an ARN of each resource type this service mints one for, carrying an id
    // nothing holds; an ARN of a resource type of this service that is not taggable; an
    // ARN of the right type in another Region and in another account; an ARN of another
    // vendor entirely; an ARN whose resource field names no type at all; and a stored
    // resource's own ARN with one character appended, which addresses nothing. The
    // tagging operations are handed an arbitrary ARN and resolve no resource type before
    // refusing, so every row expects the SAME sentence and differs only in the ARN it
    // carries.
    let absent_id = "00000000-0000-4000-8000-000000000000";
    let rows: Vec<String> = vec![
        format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:application/{absent_id}"),
        format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:event-integration/absent"),
        format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:data-integration/{absent_id}"),
        format!(
            "arn:aws:app-integrations:{REGION}:{ACCOUNT}:data-integration-association/\
             {absent_id}/{absent_id}"
        ),
        format!(
            "arn:aws:app-integrations:{REGION}:{ACCOUNT}:application-association/\
             {absent_id}/{absent_id}"
        ),
        // The right resource type in another Region: an ARN of this service that names
        // nothing this store holds.
        format!("arn:aws:app-integrations:eu-west-1:{ACCOUNT}:application/{absent_id}"),
        // And in another account.
        format!("arn:aws:app-integrations:{REGION}:000000000000:application/{absent_id}"),
        // Another vendor's ARN claims no resource type of this service.
        "arn:aws:s3:::some-bucket".to_string(),
        // A resource field with no type segment at all.
        format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:{absent_id}"),
        // A near miss: a stored resource's ARN with one more character on the end.
        format!("{}x", stored[0].arn),
    ];

    // Each tagging operation states its OWN refusal, over every ARN form: the two writes
    // the modelled code and the sentence that carries the `ResourceArn` they were sent,
    // verbatim; the read the tagging front end's code and its constant message. Neither
    // names a resource type, whatever type the ARN claims.
    for arn in rows {
        let write_message = tag_write_not_found_message(&arn);
        for (operation, reply, expected_code, expected_message) in [
            (
                "ListTagsForResource",
                list_tags(&emulator, &arn).await,
                TAG_READ_NOT_FOUND_CODE,
                TAG_READ_NOT_FOUND_MESSAGE.to_string(),
            ),
            (
                "TagResource",
                tag(&emulator, &arn, json!({ "owner": "team-a" })).await,
                TAG_WRITE_NOT_FOUND_CODE,
                write_message.clone(),
            ),
            (
                "UntagResource",
                untag(&emulator, &arn, &["owner"]).await,
                TAG_WRITE_NOT_FOUND_CODE,
                write_message.clone(),
            ),
        ] {
            let case = format!("{operation} on {arn:?}");
            assert_eq!(
                reply.status, NOT_FOUND_STATUS,
                "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {} / {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error_type, expected_code,
                "case {case:?}: expected {expected_code}, got {:?}",
                reply.error_type
            );
            let stated = reply.message();
            assert_eq!(
                stated.as_deref(),
                Some(expected_message.as_str()),
                "case {case:?}: expected {expected_message:?}, got {stated:?}"
            );
            // The resource-type words this service's OWN not-found family uses (the
            // Smithy shape names of its resources) are what the tagging surface must not
            // assert about an ARN it resolved to nothing; an ARN's own type segment is
            // lower-case and hyphenated, so none of them can appear by way of the echoed
            // value.
            for resource_type in [
                "Application",
                "EventIntegration",
                "DataIntegration",
                "DataIntegrationAssociation",
                "ApplicationAssociation",
            ] {
                let stated = stated.clone().unwrap_or_default();
                assert!(
                    !stated.contains(resource_type),
                    "case {case:?}: the tagging refusal must name no resource type, but \
                     {stated:?} names {resource_type}"
                );
            }
        }
    }

    // A refused tagging call persists nothing: every stored neighbour still reports the
    // empty tag set it was created with.
    for resource in &stored {
        let case = format!("{} after the refused tagging calls", resource.kind);
        let listed = reported_tags(&emulator, &case, resource).await;
        assert_eq!(
            listed,
            BTreeMap::new(),
            "case {case:?}: expected a resource no refused call touched to report no \
             tags, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_tag_read_refuses_a_missing_arn_with_its_own_code_and_a_constant_message() {
    let emulator = Emulator::new();
    // A stored resource of each taggable type, so a refusal below is about the ARN the
    // request named and not about an empty store — and so the positive control at the end
    // reads a real tag set back.
    let stored = taggable_resources(&emulator, 0).await;

    // Rows differ in everything an ARN can differ in — the resource-type segment, the
    // Region, the account, the vendor, the id's form and length, and whether a type
    // segment is there at all — because the rule is about an ARN that resolves to
    // nothing, not about any one ARN.
    let absent_id = "00000000-0000-4000-8000-000000000000";
    let rows: Vec<(&str, String)> = vec![
        (
            "an application id nothing holds",
            format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:application/{absent_id}"),
        ),
        (
            "an event integration name nothing holds",
            format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:event-integration/absent"),
        ),
        (
            "a data integration id nothing holds",
            format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:data-integration/{absent_id}"),
        ),
        (
            "an association ARN nothing holds",
            format!(
                "arn:aws:app-integrations:{REGION}:{ACCOUNT}:data-integration-association/\
                 {absent_id}/{absent_id}"
            ),
        ),
        (
            "the right resource type in another Region",
            format!("arn:aws:app-integrations:eu-west-1:{ACCOUNT}:application/{absent_id}"),
        ),
        (
            "the right resource type in another account",
            format!("arn:aws:app-integrations:{REGION}:111122223333:application/{absent_id}"),
        ),
        (
            "another vendor's ARN entirely",
            "arn:aws:s3:::some-bucket".to_string(),
        ),
        (
            "an ARN with no resource-type segment",
            format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:{absent_id}"),
        ),
        (
            "a stored resource's ARN with one character appended",
            format!("{}x", stored[0].arn),
        ),
    ];

    for (label, arn) in rows {
        let case = format!("ListTagsForResource on {label}");
        let refused = list_tags(&emulator, &arn).await;
        assert_eq!(
            refused.status, NOT_FOUND_STATUS,
            "case {case:?}: expected HTTP {NOT_FOUND_STATUS}, got {} / {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type, TAG_READ_NOT_FOUND_CODE,
            "case {case:?}: expected {TAG_READ_NOT_FOUND_CODE}, got {:?}",
            refused.error_type
        );
        let stated = refused.message();
        assert_eq!(
            stated.as_deref(),
            Some(TAG_READ_NOT_FOUND_MESSAGE),
            "case {case:?}: expected {TAG_READ_NOT_FOUND_MESSAGE:?}, got {stated:?}"
        );
        // That message is a CONSTANT: nothing the request carried and nothing of this
        // run's ARN is interpolated into it, so the same text answers every row.
        let stated = stated.unwrap_or_default();
        for interpolated in [arn.as_str(), REGION, ACCOUNT, absent_id] {
            assert!(
                !stated.contains(interpolated),
                "case {case:?}: the tag read's message must carry nothing from the \
                 request, but {stated:?} carries {interpolated:?}"
            );
        }

        // The tag WRITE on the SAME ARN states a different code and a different sentence:
        // that difference is the signal a caller tells "the tag read found no resource"
        // from "the tag write found no resource" by.
        let written = tag(&emulator, &arn, json!({ "owner": "team-a" })).await;
        let write_case = format!("TagResource on {label}");
        assert_eq!(
            written.error_type, TAG_WRITE_NOT_FOUND_CODE,
            "case {write_case:?}: expected {TAG_WRITE_NOT_FOUND_CODE}, got {:?}",
            written.error_type
        );
        let expected_write = tag_write_not_found_message(&arn);
        assert_eq!(
            written.message().as_deref(),
            Some(expected_write.as_str()),
            "case {write_case:?}: expected {expected_write:?}, got {:?}",
            written.message()
        );
        assert_ne!(
            refused.error_type, written.error_type,
            "case {case:?}: the tag read and the tag write must not state one code, \
             both stated {:?}",
            refused.error_type
        );
    }

    // The positive control: the tag read still answers a REAL resource, reporting exactly
    // the stored map, both after a tag and after an untag.
    for resource in &stored {
        let case = format!("the tag read of a stored {}", resource.kind);
        let tagged = tag(
            &emulator,
            &resource.arn,
            json!({ "owner": "team-a", "env": "prod" }),
        )
        .await;
        assert_no_content_success(&case, "TagResource", &tagged);
        let expected: BTreeMap<String, String> = [("owner", "team-a"), ("env", "prod")]
            .into_iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let listed = reported_tags(&emulator, &case, resource).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the tag read to report {expected:?}, got {listed:?}"
        );

        let untagged = untag(&emulator, &resource.arn, &["env"]).await;
        assert_no_content_success(&case, "UntagResource", &untagged);
        let expected: BTreeMap<String, String> =
            [("owner".to_string(), "team-a".to_string())]
                .into_iter()
                .collect();
        let listed = reported_tags(&emulator, &case, resource).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected the tag read to report {expected:?} after the untag, \
             got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_tagging_refusal_differs_from_the_resource_scoped_not_found_family() {
    let emulator = Emulator::new();

    // One resource of each type, then the ARN of a resource of that type that this
    // account does not hold: the SAME missing resource addressed two ways. Its own
    // reader knows which type it was asked about and names it; the tagging operations
    // know only an ARN, so they name no type at all.
    let absent_id = "00000000-0000-4000-8000-000000000000";
    let rows: Vec<(&str, String, String, String)> = vec![
        (
            "data integration",
            format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:data-integration/{absent_id}"),
            format!("/dataIntegrations/{}", encode_label(absent_id)),
            format!("Could not find DataIntegration with identifier: {absent_id}."),
        ),
        (
            "event integration",
            format!("arn:aws:app-integrations:{REGION}:{ACCOUNT}:event-integration/absent"),
            format!("/eventIntegrations/{}", encode_label("absent")),
            "Could not find EventIntegration with name: absent.".to_string(),
        ),
    ];

    for (kind, arn, get_uri, scoped_message) in rows {
        let case = format!("the resource-scoped reader of an absent {kind}");
        let scoped = emulator.send("GET", &get_uri, REGION, None).await;
        assert_eq!(
            scoped.message().as_deref(),
            Some(scoped_message.as_str()),
            "case {case:?}: expected {scoped_message:?}, got {:?}",
            scoped.message()
        );

        let case = format!("the tagging surface on the ARN of that absent {kind}");
        let expected = tag_write_not_found_message(&arn);
        let tagged = tag(&emulator, &arn, json!({ "owner": "team-a" })).await;
        assert_eq!(
            tagged.message().as_deref(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {:?}",
            tagged.message()
        );
        assert_ne!(
            tagged.message(),
            scoped.message(),
            "case {case:?}: the tagging surface must not state the resource-scoped \
             sentence {scoped_message:?}"
        );
    }
}

#[tokio::test]
async fn a_resource_cannot_be_tagged_past_the_tag_maps_maximum() {
    let emulator = Emulator::new();
    let resource = application(&emulator, "capped.application", None).await;

    // `TagMap` is len 1..200 and is the shape a tag set is both written and reported as
    // (`smithy_query(query="shape", shape="TagMap")`), so a resource carries at most
    // 200 tags. Rows: exactly the maximum, a repeat of keys already carried at the
    // maximum (no growth, so still fine), and one key past it.
    let full: Vec<(String, String)> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("key-{index}"), format!("value-{index}")))
        .collect();
    let at_maximum = tag(&emulator, &resource.arn, tags_body(&full)).await;
    assert_no_content_success("exactly the maximum", "TagResource", &at_maximum);
    let listed = reported_tags(&emulator, "exactly the maximum", &resource).await;
    assert_eq!(
        listed.len(),
        MAX_TAGS_PER_RESOURCE,
        "case \"exactly the maximum\": expected {MAX_TAGS_PER_RESOURCE} tags, got {}",
        listed.len()
    );

    let repeated = tag(
        &emulator,
        &resource.arn,
        tags_body(&[("key-0".to_string(), "replaced".to_string())]),
    )
    .await;
    assert_no_content_success(
        "a key already carried, at the maximum",
        "TagResource",
        &repeated,
    );

    let past = tag(
        &emulator,
        &resource.arn,
        tags_body(&[("one-too-many".to_string(), "value".to_string())]),
    )
    .await;
    assert_eq!(
        past.status, 400,
        "case \"one key past the maximum\": expected HTTP 400, got {} / {}",
        past.status, past.body
    );
    assert_eq!(
        past.error_type, "InvalidRequestException",
        "case \"one key past the maximum\": expected InvalidRequestException, got {:?}",
        past.error_type
    );
    let listed = reported_tags(&emulator, "one key past the maximum", &resource).await;
    assert_eq!(
        listed.len(),
        MAX_TAGS_PER_RESOURCE,
        "case \"one key past the maximum\": expected the refused tag to be stored nowhere, \
         got {} tags",
        listed.len()
    );
}

#[tokio::test]
async fn tags_survive_a_read_and_a_delete_removes_them_with_the_resource() {
    let emulator = Emulator::new();

    // A read must not change what a later caller sees: listing the tags twice, with the
    // resource's own reader in between, reports the same set every time.
    let resource = application(&emulator, "stable.application", None).await;
    tag(&emulator, &resource.arn, json!({ "owner": "team-a" })).await;
    let expected: BTreeMap<String, String> =
        [("owner".to_string(), "team-a".to_string())].into_iter().collect();
    for round in 0..3 {
        let case = format!("read {round} of a tagged application");
        let listed = reported_tags(&emulator, &case, &resource).await;
        assert_eq!(
            listed, expected,
            "case {case:?}: expected a read to report {expected:?} unchanged, got {listed:?}"
        );
        let own = tags_from_own_reader(&emulator, &case, &resource).await;
        assert_eq!(
            own, expected,
            "case {case:?}: expected the resource's own reader to report {expected:?}, got {own:?}"
        );
    }

    // The tag set is part of the resource's record, so the delete that removes the
    // resource takes it with it: the ARN then names nothing to tag.
    let deleted = emulator
        .send(
            "DELETE",
            &format!("/applications/{}", encode_label(&resource.arn)),
            REGION,
            None,
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "the application under test must be deleted, got {} / {}",
        deleted.status, deleted.body
    );
    let listed = list_tags(&emulator, &resource.arn).await;
    assert_eq!(
        listed.status, 404,
        "case \"ListTagsForResource after the delete\": expected HTTP 404, got {} / {}",
        listed.status, listed.body
    );
}

/// One row of the success-status table: the label that names it in an assertion, the tag
/// set one `TagResource` writes, and the keys the `UntagResource` after it names.
#[derive(Debug, Clone)]
struct StatusRow {
    label: &'static str,
    tags: Vec<(String, String)>,
    untag_keys: Vec<String>,
}

/// The rows the success-status rule is asserted over. They differ along what that rule
/// generalises over — how many tags the request carries (one, several, the maximum the
/// modelled `TagMap` admits), the form of a value (a word, the empty string), and which
/// keys the untag names (the only one, a subset, a key the resource does not carry at all,
/// every key). The recorded tag/untag pair is the first row.
fn status_rows() -> Vec<StatusRow> {
    let full: Vec<(String, String)> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("key-{index}"), format!("value-{index}")))
        .collect();
    let every_key: Vec<String> = full.iter().map(|(key, _)| key.clone()).collect();
    vec![
        StatusRow {
            label: "one tag, untagged by its own key",
            tags: vec![("owner".to_string(), "team-a".to_string())],
            untag_keys: vec!["owner".to_string()],
        },
        StatusRow {
            label: "several tags, a subset untagged",
            tags: vec![
                ("owner".to_string(), "team-a".to_string()),
                ("env".to_string(), "dev".to_string()),
                ("tier".to_string(), "gold".to_string()),
            ],
            untag_keys: vec!["env".to_string(), "tier".to_string()],
        },
        StatusRow {
            label: "an empty value, untagged by a key the resource does not carry",
            tags: vec![("empty".to_string(), String::new())],
            untag_keys: vec!["never-written".to_string()],
        },
        StatusRow {
            label: "the maximum tag count the map admits, every key untagged",
            tags: full,
            untag_keys: every_key,
        },
    ]
}

#[tokio::test]
async fn both_tagging_mutations_answer_the_no_content_success_status() {
    let emulator = Emulator::new();

    for (row, StatusRow {
        label,
        tags,
        untag_keys,
    }) in status_rows().into_iter().enumerate()
    {
        for resource in taggable_resources(&emulator, row).await {
            let case = format!("{label} on a {}", resource.kind);

            let tagged = tag(&emulator, &resource.arn, tags_body(&tags)).await;
            assert_no_content_success(&case, "TagResource", &tagged);

            // A no-content answer is still the mutation: what it wrote is readable back,
            // so the status assertion above cannot pass on a call that did nothing.
            let expected = expected_map(&tags);
            let listed = reported_tags(&emulator, &case, &resource).await;
            assert_eq!(
                listed, expected,
                "case {case:?}: expected the no-content TagResource to have stored \
                 {expected:?}, got {listed:?}"
            );

            let keys: Vec<&str> = untag_keys.iter().map(String::as_str).collect();
            let untagged = untag(&emulator, &resource.arn, &keys).await;
            assert_no_content_success(&case, "UntagResource", &untagged);

            let remaining: BTreeMap<String, String> = tags
                .iter()
                .filter(|(key, _)| !untag_keys.contains(key))
                .cloned()
                .collect();
            let listed = reported_tags(&emulator, &case, &resource).await;
            assert_eq!(
                listed, remaining,
                "case {case:?}: expected the no-content UntagResource to leave \
                 {remaining:?}, got {listed:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_no_content_status_is_the_tagging_surfaces_own() {
    let emulator = Emulator::new();

    // Every OTHER operation of this service whose output carries no members
    // (`smithy_query(query="operation", operation="UpdateApplication,
    // UpdateEventIntegration,UpdateDataIntegration,DeleteApplication,
    // DeleteEventIntegration,DeleteDataIntegration")`: each `output` is empty) answers the
    // default success status, so the no-content status belongs to the tagging mutations
    // and is not a blanket rule about an empty output.
    let application = application(&emulator, "statuses.application", None).await;
    let event_integration =
        event_integration(&emulator, "statuses.event-integration", None).await;
    let data_integration = data_integration(&emulator, "statuses.data-integration", None).await;

    let updated = json!({ "Description": "the description the update stored" });
    let rows: Vec<(&str, &str, String, Option<Value>)> = vec![
        (
            "UpdateApplication",
            "PATCH",
            application.get_uri.clone(),
            Some(updated.clone()),
        ),
        (
            "UpdateEventIntegration",
            "PATCH",
            event_integration.get_uri.clone(),
            Some(updated.clone()),
        ),
        (
            "UpdateDataIntegration",
            "PATCH",
            data_integration.get_uri.clone(),
            Some(updated),
        ),
        (
            "DeleteApplication",
            "DELETE",
            application.get_uri.clone(),
            None,
        ),
        (
            "DeleteEventIntegration",
            "DELETE",
            event_integration.get_uri.clone(),
            None,
        ),
        (
            "DeleteDataIntegration",
            "DELETE",
            data_integration.get_uri.clone(),
            None,
        ),
    ];

    for (operation, method, uri, body) in rows {
        let reply = emulator.send(method, &uri, REGION, body).await;
        assert_eq!(
            reply.status, DEFAULT_SUCCESS_STATUS,
            "case {operation:?}: expected HTTP {DEFAULT_SUCCESS_STATUS}, got {} / {}",
            reply.status, reply.body
        );
    }
}
