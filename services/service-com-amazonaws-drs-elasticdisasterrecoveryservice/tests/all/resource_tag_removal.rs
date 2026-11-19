// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UntagResource` (`DELETE /tags/{resourceArn}`), the keys it
//! takes off a resource and the request shapes a client sends it in.
//!
//! The tagging route's merge rules are pinned in `tests/all/resource_tags.rs`; what this
//! suite pins is the part of `UntagResource` that is about its REQUEST rather than about
//! the merge: the operation binds `tagKeys` to the query string
//! (`smithy_query(query="operation",operation="UntagResource")` — `tagKeys`,
//! `@httpQuery("tagKeys")`, `@required`), so the key a client means reaches the service
//! percent-encoded and repeated once per key, and the removal has to act on the DECODED
//! key.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! 1. A removal takes the keys the request names off the addressed resource's own tag
//!    set, whatever characters those keys carry: a key is matched by the value the query
//!    binding decodes, not by the text the client sent, so a key holding a reserved
//!    character, a non-ASCII one, or a key at either end of the modelled length range
//!    (`TagKey`, length 0..256) is removed exactly like an unreserved one.
//! 2. The removal is observable through BOTH readers of one tag set — the route's own
//!    read and the resource's own reader — because a resource carries ONE tag set.
//! 3. A removal reaches ONE resource: the keys it names come off the resource its ARN
//!    addresses and stay on every other resource, whatever kind either is.
//! 4. A request that names no `tagKeys` at all leaves the resource exactly as it was: the
//!    member is `@required` and bound to the query string, so a request without it does
//!    not name this route — it carries no action for the layer ahead of the service to
//!    authorize, which is the answer `tests/all/unroutable_request_refusal.rs` pins for
//!    every such request — and nothing is removed for it.

use std::collections::BTreeMap;

use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json, Map, Value};
use service_com_amazonaws_drs_elasticdisasterrecoveryservice::state::{self, SourceServerRecord};
use smithy_server_runtime::MultiServiceRouter;

use crate::replication_configuration_template_lifecycle::{call_in, drs_with_engine, Answer};
use crate::resource_tags::{call_path, encoded_label};

/// The Region every request in this suite signs for; every ARN this service mints carries
/// it.
const REGION: &str = "eu-west-1";

/// The status, the error class and the whole sentence this endpoint answers a request it
/// cannot place on one of its operations with — fixed text, naming nothing of the request
/// (`tests/all/unroutable_request_refusal.rs` pins the rule over every such request).
const NOT_AUTHORIZED_STATUS: u16 = 403;
const NOT_AUTHORIZED_CODE: &str = "AccessDeniedException";
const NOT_AUTHORIZED: &str = "Unable to determine service/operation name to be authorized";

/// A tag key as a client puts it in a query VALUE: everything outside the alphanumerics
/// is percent-encoded, which is always correct and never ambiguous — a key carrying `&`,
/// `=`, `+` or a space would otherwise address another parameter, or another key.
fn encoded_query_value(key: &str) -> String {
    utf8_percent_encode(key, NON_ALPHANUMERIC).to_string()
}

/// `DELETE /tags/{resourceArn}?tagKeys=…` — `UntagResource` as the model binds it:
/// `tagKeys` is an `@httpQuery` list, so a client repeats the parameter once per key, and
/// a slice of no keys sends the route with no query string at all.
async fn untag(router: &MultiServiceRouter, resource_arn: &str, tag_keys: &[&str]) -> Answer {
    let path = format!("/tags/{}", encoded_label(resource_arn));
    let path = if tag_keys.is_empty() {
        path
    } else {
        let query = tag_keys
            .iter()
            .map(|key| format!("tagKeys={}", encoded_query_value(key)))
            .collect::<Vec<_>>()
            .join("&");
        format!("{path}?{query}")
    };
    call_path(router, REGION, "DELETE", &path, &[]).await
}

/// `GET /tags/{resourceArn}` — `ListTagsForResource`, the route's own read.
async fn list_tags(router: &MultiServiceRouter, resource_arn: &str) -> Answer {
    call_path(
        router,
        REGION,
        "GET",
        &format!("/tags/{}", encoded_label(resource_arn)),
        &[],
    )
    .await
}

/// The tag set the route reports for `resource_arn`, as a map an assertion can compare
/// whatever order the service reports its members in.
async fn reported_tags(router: &MultiServiceRouter, resource_arn: &str) -> BTreeMap<String, String> {
    let read = list_tags(router, resource_arn).await;
    assert_eq!(
        read.status, 200,
        "reading the tags of {resource_arn}: expected 200, got {read:?}"
    );
    as_map(
        read.member("tags")
            .unwrap_or_else(|| panic!("expected a reported tag set, got {read:?}")),
    )
}

/// The tag set the RESOURCE's own reader reports for a recovery plan.
async fn plan_tags(router: &MultiServiceRouter, resource_arn: &str) -> BTreeMap<String, String> {
    let answer = call_in(
        router,
        REGION,
        "GetRecoveryPlan",
        &json!({"recoveryPlanArn": resource_arn}),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "reading the plan {resource_arn}: expected 200, got {answer:?}"
    );
    as_map(
        answer
            .member("recoveryPlan")
            .and_then(|plan| plan.get("tags"))
            .unwrap_or_else(|| panic!("expected the plan's tag set, got {answer:?}")),
    )
}

/// A reported JSON tag set as a comparable map.
fn as_map(tags: &Value) -> BTreeMap<String, String> {
    tags.as_object()
        .unwrap_or_else(|| panic!("expected a JSON object of tags, got {tags}"))
        .iter()
        .map(|(key, value)| {
            (
                key.clone(),
                value.as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// A tag map built from keys a test holds, which `json!` cannot take as keys.
fn tag_map(entries: &[(&str, &str)]) -> Value {
    let mut map = Map::new();
    for (key, value) in entries {
        map.insert(key.to_string(), Value::String(value.to_string()));
    }
    Value::Object(map)
}

/// The same entries as a comparable map.
fn expected_map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Create a recovery plan carrying `tags`, and answer its ARN.
async fn plan_with_tags(router: &MultiServiceRouter, tags: &Value) -> String {
    let answer = call_in(
        router,
        REGION,
        "CreateRecoveryPlan",
        &json!({"name": "Tagged plan", "tags": tags.clone()}),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "creating a tagged plan: expected 201, got {answer:?}"
    );
    answer
        .member("recoveryPlan")
        .and_then(|plan| plan.get("recoveryPlanArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the created plan's ARN, got {answer:?}"))
        .to_string()
}

/// A removal takes off the key the query binding DECODES, not the text the client sent:
/// the rows differ along what characters the key carries — unreserved, a space, the
/// delimiters of the query string itself, a colon and a slash, non-ASCII — and along the
/// two ends of the modelled length range (`TagKey`, length 0..256, from
/// `smithy_query(query="shape",shape="TagKey")`). Every row keeps a second key on the
/// resource, so a row that removed the whole set rather than the one key it named fails.
#[tokio::test]
async fn a_removal_takes_off_the_decoded_key_whatever_characters_it_carries() {
    let longest = "k".repeat(256);
    let rows: Vec<(&str, &str)> = vec![
        ("an unreserved key", "env"),
        ("a key holding a space", "cost center"),
        ("a key holding the query delimiters", "a&b=c+d"),
        ("a key holding a colon and a slash", "aws:dr/team"),
        ("a key holding a percent sign", "100%"),
        ("a non-ASCII key", "région-créée"),
        ("the shortest modelled key", ""),
        ("the longest modelled key", longest.as_str()),
    ];

    let (router, _engine) = drs_with_engine();
    for (case, key) in &rows {
        let arn = plan_with_tags(&router, &tag_map(&[(key, "removed"), ("keep", "me")])).await;
        assert_eq!(
            reported_tags(&router, &arn).await,
            expected_map(&[(key, "removed"), ("keep", "me")]),
            "case {case:?}: the resource must start out carrying the key to remove"
        );

        let answer = untag(&router, &arn, &[key]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the removal to be served, got {answer:?}"
        );

        let expected = expected_map(&[("keep", "me")]);
        let after = reported_tags(&router, &arn).await;
        assert_eq!(
            after, expected,
            "case {case:?}: expected {expected:?} through the route's read, got {after:?}"
        );
        // ONE tag set: the resource's own reader reports the same removal.
        let carried = plan_tags(&router, &arn).await;
        assert_eq!(
            carried, expected,
            "case {case:?}: expected {expected:?} through the resource's reader, got {carried:?}"
        );
    }
}

/// A removal reaches ONE resource: the keys it names come off the resource its ARN
/// addresses and stay on every other resource, whatever kind either resource is. The rows
/// pair the resource the removal addresses with a bystander of each kind this service
/// holds a tag set for, both carrying the same key.
#[tokio::test]
async fn a_removal_leaves_every_other_resource_carrying_the_same_key() {
    let (router, engine) = drs_with_engine();

    // A source server is registered by the replication agent, which this emulator does
    // not model, so the one way to hold one is to write the store directly.
    let server_id = "s-00000000000000003";
    let server_arn = engine::emulator_arn("drs", REGION, &format!("source-server/{server_id}"));
    {
        let store = state::service_state(&*engine).expect("the DRS store");
        let mut store = store.lock();
        store.source_servers.insert(
            server_id.to_string(),
            SourceServerRecord {
                source_server_id: server_id.to_string(),
                arn: server_arn.clone(),
                tags: [("env".to_string(), "prod".to_string())]
                    .into_iter()
                    .collect(),
                ..SourceServerRecord::default()
            },
        );
    }

    let addressed = plan_with_tags(&router, &json!({"env": "prod", "team": "dr"})).await;
    let other_plan = plan_with_tags(&router, &json!({"env": "prod"})).await;

    let answer = untag(&router, &addressed, &["env"]).await;
    assert_eq!(
        answer.status, 200,
        "expected the removal to be served, got {answer:?}"
    );
    let after = reported_tags(&router, &addressed).await;
    assert_eq!(
        after,
        expected_map(&[("team", "dr")]),
        "the addressed resource must lose only the named key, got {after:?}"
    );

    for (case, arn) in [
        ("another recovery plan", &other_plan),
        ("a source server", &server_arn),
    ] {
        let carried = reported_tags(&router, arn).await;
        assert_eq!(
            carried,
            expected_map(&[("env", "prod")]),
            "case {case:?}: a removal addressed elsewhere must leave this set, got {carried:?}"
        );
    }
}

/// `tagKeys` is `@required` and bound to the query string, so a request that names none
/// does not name this route at all: it carries no action to authorize and is refused as
/// the request the layer ahead of the service could not place, and the resource keeps
/// every key it had. The rows differ along the sets such a request is sent against.
#[tokio::test]
async fn a_removal_naming_no_tag_key_is_refused_and_removes_nothing() {
    /// One row: what the case is called, the set the plan is created with, and the set it
    /// must still carry after the refused request.
    type Row<'a> = (&'a str, Value, Vec<(&'a str, &'a str)>);

    let rows: Vec<Row<'_>> = vec![
        (
            "a resource carrying several keys",
            json!({"env": "prod", "team": "dr"}),
            vec![("env", "prod"), ("team", "dr")],
        ),
        (
            "a resource carrying one key",
            json!({"env": "prod"}),
            vec![("env", "prod")],
        ),
        ("a resource carrying none", json!({}), vec![]),
    ];

    let (router, _engine) = drs_with_engine();
    for (case, created_with, expected) in &rows {
        let arn = plan_with_tags(&router, created_with).await;

        let answer = untag(&router, &arn, &[]).await;
        assert_eq!(
            answer.status, NOT_AUTHORIZED_STATUS,
            "case {case:?}: expected {NOT_AUTHORIZED_STATUS}, got {answer:?}"
        );
        assert_eq!(
            answer.error_code.as_deref(),
            Some(NOT_AUTHORIZED_CODE),
            "case {case:?}: expected {NOT_AUTHORIZED_CODE}, got {answer:?}"
        );
        assert_eq!(
            answer.member("message").and_then(Value::as_str),
            Some(NOT_AUTHORIZED),
            "case {case:?}: expected the unplaceable-request sentence, got {answer:?}"
        );

        let expected = expected_map(expected);
        let after = reported_tags(&router, &arn).await;
        assert_eq!(
            after, expected,
            "case {case:?}: a refused removal must leave {expected:?}, got {after:?}"
        );
    }
}
