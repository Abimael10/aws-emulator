// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DRS tagging route — `ListTagsForResource`
//! (`GET /tags/{resourceArn}`), `TagResource` (`POST`) and `UntagResource` (`DELETE`).
//!
//! Every test drives the service the way the router does — one buffered
//! `http::Request` per call through [`MultiServiceRouter::dispatch`] — so what is
//! asserted is the wire behaviour (status, error code, JSON body), not a handler's Rust
//! return value. The harness (the router, `call_in`, `Answer`) is the one
//! `replication_configuration_template_lifecycle` builds; the tagging route needs a
//! method and a path of its own, which [`list_tags`] builds the way a client builds them.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than one recorded
//! request:
//!
//! 1. The route reports the tag set the resource its `resourceArn` addresses carries —
//!    the set the resource was created with, and the empty set for a resource created
//!    with none — whatever the shape of that set, and for EVERY resource type the route
//!    addresses, because the route addresses a resource by ARN alone and takes no
//!    resource type.
//! 2. It is a READ: the same ARN reports the same tag set however often it is asked, and
//!    the resource's own reader keeps reporting that set too.
//! 3. An ARN of a resource type the route addresses that this service holds no resource
//!    for is refused by EVERY operation of the route alike, with HTTP 404, the error code
//!    `NotFoundException` and the message `Resource was not found`, and nothing it
//!    addresses is written. The ways such an ARN can name nothing span the rows: an id
//!    nothing minted, a stored resource's id under another Region or account, and a
//!    resource type of this service's that holds nothing. An ARN the route does NOT
//!    address is the other refusal class, which `tagging_resource_arn_faults` covers.
//! 4. A write MERGES: `TagResource` sets the keys it names, overwrites a key the resource
//!    already carries and leaves every other key where it is, so two calls naming
//!    disjoint keys leave both sets on the resource; `UntagResource` removes the keys it
//!    names and nothing else. Both are observable through the read AND through the
//!    resource's own reader, because a resource has ONE tag set.

use serde_json::{json, Map, Value};
use service_com_amazonaws_drs_elasticdisasterrecoveryservice::state::{self, SourceServerRecord};
use smithy_server_runtime::MultiServiceRouter;

use crate::replication_configuration_template_lifecycle::{
    call_in, create_body, drs_with_engine, Answer,
};

/// The Region most rows sign for; every ARN this service mints carries it.
const REGION: &str = "eu-west-1";

/// The account id every generated emulator presents (`engine::EMULATOR_ACCOUNT_ID`).
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// The code and the whole message EVERY operation of the tagging route answers an ARN it
/// resolved to no resource with. The route is served ahead of any one resource's
/// operations — it takes no resource type, and one path serves every kind — so it states
/// a resource it could not resolve as its own error class, not as the
/// `ResourceNotFoundException` the id-keyed operations of a resource raise.
const NOT_FOUND_CODE: &str = "NotFoundException";
const NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The code the model declares for a request this service refuses on one of its fields
/// (`smithy_query(query="operation",operation="TagResource")` lists
/// `ValidationException`, HTTP 400).
const VALIDATION_CODE: &str = "ValidationException";

/// How many tags one resource carries at most: "Each resource can have a maximum of 50
/// tags" (`TagResource`'s own documentation,
/// `smithy_query(query="doc",shape="TagResource")`).
const TAGS_PER_RESOURCE_LIMIT: usize = 50;

/// An ARN as a client percent-encodes a non-greedy `@httpLabel`: `resourceArn` is bound
/// to one path segment, so the colons and slashes inside it are escaped.
pub(crate) fn encoded_label(arn: &str) -> String {
    arn.replace(':', "%3A").replace('/', "%2F")
}

/// `<method> <path>` against this service's endpoint, routed by the SigV4 credential
/// scope exactly as an AWS SDK client configured for `region` routes it.
///
/// The route-level request the suites that drive a PATH rather than an operation name
/// share (`crate::replication_configuration_template_lifecycle::call_in` posts to
/// `/<operation>`, which is how this model binds most of its operations, but not the
/// tagging route and not a path no operation binds at all).
pub(crate) async fn call_path(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: &[u8],
) -> Answer {
    let host = format!("drs.{region}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20240101/{region}/drs/aws4_request, \
                 SignedHeaders=host, Signature=0"
            ),
        )
        .body(bytes::Bytes::copy_from_slice(body))
        .expect("a valid request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(|raw| raw.rsplit('#').next().unwrap_or(raw).to_string());
    let bytes = response.into_body();
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error_code,
        body,
    }
}

/// `GET /tags/{resourceArn}` — `ListTagsForResource` as the model binds it.
pub(crate) async fn list_tags(
    router: &MultiServiceRouter,
    region: &str,
    resource_arn: &str,
) -> Answer {
    call_path(
        router,
        region,
        "GET",
        &format!("/tags/{}", encoded_label(resource_arn)),
        &[],
    )
    .await
}

/// `POST /tags/{resourceArn}` with a `tags` body — `TagResource` as the model binds it.
pub(crate) async fn tag_resource(
    router: &MultiServiceRouter,
    region: &str,
    resource_arn: &str,
    tags: &Value,
) -> Answer {
    let body = json!({"tags": tags.clone()});
    call_path(
        router,
        region,
        "POST",
        &format!("/tags/{}", encoded_label(resource_arn)),
        &serde_json::to_vec(&body).expect("the request body serializes"),
    )
    .await
}

/// `DELETE /tags/{resourceArn}?tagKeys=…` — `UntagResource` as the model binds it:
/// `tagKeys` is an `@httpQuery` list, so a client repeats the parameter per key.
pub(crate) async fn untag_resource(
    router: &MultiServiceRouter,
    region: &str,
    resource_arn: &str,
    tag_keys: &[&str],
) -> Answer {
    let query = tag_keys
        .iter()
        .map(|key| format!("tagKeys={}", query_value(key)))
        .collect::<Vec<_>>()
        .join("&");
    call_path(
        router,
        region,
        "DELETE",
        &format!("/tags/{}?{query}", encoded_label(resource_arn)),
        &[],
    )
    .await
}

/// A query-string value as a client sends one. Every `tagKeys` value this suite sends is
/// drawn from the RFC 3986 unreserved set, so none needs percent-encoding; a row that
/// adds one which does is caught here rather than silently addressing another parameter,
/// and such a row encodes it with the `percent-encoding` crate rather than by hand.
fn query_value(key: &str) -> &str {
    assert!(
        key.bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')),
        "the tag key {key:?} is not unreserved, so it must be percent-encoded before it is \
         sent as a query value"
    );
    key
}

/// The `tags` member of an answer, for an assertion that names the case.
pub(crate) fn reported_tags(answer: &Answer) -> Option<&Value> {
    answer.member("tags")
}

/// A string member of an answer's body.
pub(crate) fn text(answer: &Answer, name: &str) -> String {
    answer
        .member(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Create a Recovery Plan, optionally tagged, and answer its ARN.
pub(crate) async fn plan_with_tags(
    router: &MultiServiceRouter,
    region: &str,
    tags: Option<&Value>,
) -> String {
    let mut body = json!({"name": "Tagged plan"});
    if let Some(tags) = tags {
        body["tags"] = tags.clone();
    }
    let answer = call_in(router, region, "CreateRecoveryPlan", &body).await;
    assert_eq!(
        answer.status, 201,
        "creating a plan with {tags:?}: expected 201, got {answer:?}"
    );
    answer
        .member("recoveryPlan")
        .and_then(|plan| plan.get("recoveryPlanArn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the created plan's ARN, got {answer:?}"))
        .to_string()
}

/// Create a replication configuration template carrying `tags`, and answer its ARN.
async fn template_with_tags(router: &MultiServiceRouter, tags: &Value) -> String {
    let body = create_body("subnet-0123456789abcdef0", json!({"tags": tags.clone()}));
    let answer = call_in(
        router,
        REGION,
        "CreateReplicationConfigurationTemplate",
        &body,
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "creating a template with {tags}: expected 201, got {answer:?}"
    );
    answer
        .member("arn")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the created template's ARN, got {answer:?}"))
        .to_string()
}

/// Create a launch configuration template carrying `tags`, and answer its ARN.
///
/// The tagging route does not address this resource type (see
/// `crate::tagging_resource_arn_faults`), so what it is here for is the rows that show the
/// route refusing its ARN while the resource itself carries its tags.
pub(crate) async fn launch_template_with_tags(
    router: &MultiServiceRouter,
    tags: &Value,
) -> String {
    let answer = call_in(
        router,
        REGION,
        "CreateLaunchConfigurationTemplate",
        &json!({"tags": tags.clone()}),
    )
    .await;
    assert_eq!(
        answer.status, 201,
        "creating a launch configuration template with {tags}: expected 201, got {answer:?}"
    );
    answer
        .member("launchConfigurationTemplate")
        .and_then(|template| template.get("arn"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected the created template's ARN, got {answer:?}"))
        .to_string()
}

/// A one-entry tag map built from values a test holds, which `json!` cannot take as a
/// key.
fn one_tag(key: &str, value: &str) -> Value {
    let mut map = Map::new();
    map.insert(key.to_string(), Value::String(value.to_string()));
    Value::Object(map)
}

// ---------------------------------------------------------------------------
// Rules 1 and 2 — the route reports the set the resource carries, and reports
// it unchanged
// ---------------------------------------------------------------------------

/// The route reports exactly the tag set the resource carries, for every shape of set
/// the model admits, and reports it again unchanged — as does the resource's own reader.
/// The rows differ along what the rule generalises over: whether the resource was created
/// with no tag member at all, with the empty set, with one entry, with several, with an
/// empty value, and with a key and a value at the greatest length `TagsMap` admits
/// (`TagKey` / `TagValue`, length 0..256).
#[tokio::test]
async fn a_resource_reports_the_tag_set_it_carries() {
    let longest = one_tag(&"k".repeat(256), &"v".repeat(256));
    let rows: Vec<(&str, Option<Value>)> = vec![
        ("no tag member at all", None),
        ("the empty tag set", Some(json!({}))),
        ("one tag", Some(json!({"env": "prod"}))),
        (
            "several tags",
            Some(json!({"env": "prod", "team": "dr", "tier": "1"})),
        ),
        ("a tag whose value is empty", Some(json!({"env": ""}))),
        ("a tag whose key is empty", Some(one_tag("", "unkeyed"))),
        ("a key and a value at the greatest modelled length", Some(longest)),
    ];

    let (router, _engine) = drs_with_engine();
    for (case, tags) in &rows {
        let arn = plan_with_tags(&router, REGION, tags.as_ref()).await;
        // A resource always carries a tag set: one created without the member carries
        // the empty one, which is what the read reports.
        let expected = tags.clone().unwrap_or_else(|| json!({}));

        let read = list_tags(&router, REGION, &arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the tags of {arn} to be served, got {read:?}"
        );
        assert_eq!(
            reported_tags(&read),
            Some(&expected),
            "case {case:?}: expected {expected}, got {read:?}"
        );

        // Rule 2: asking again answers the same set, and the resource's own reader
        // still reports it — so the read changed nothing a later caller sees.
        let again = list_tags(&router, REGION, &arn).await;
        assert_eq!(
            reported_tags(&again),
            Some(&expected),
            "case {case:?}: a second read must report {expected}, got {again:?}"
        );
        let plan = call_in(
            &router,
            REGION,
            "GetRecoveryPlan",
            &json!({"recoveryPlanArn": arn}),
        )
        .await;
        assert_eq!(
            plan.member("recoveryPlan").and_then(|plan| plan.get("tags")),
            Some(&expected),
            "case {case:?}: the resource must still carry {expected}, got {plan:?}"
        );
    }
}

/// ONE route, every resource type it addresses: the tag set reported for an ARN is the set
/// the resource THAT ARN addresses carries, whether the resource was created through an
/// operation of this service or registered by the replication agent — the route takes no
/// resource type, so the type can only come from the ARN.
#[tokio::test]
async fn every_kind_of_resource_reports_its_own_tag_set_through_the_one_route() {
    let (router, engine) = drs_with_engine();

    let plan_tags = json!({"kind": "recovery-plan", "env": "prod"});
    let plan = plan_with_tags(&router, REGION, Some(&plan_tags)).await;

    let template_tags = json!({"kind": "replication-configuration-template"});
    let template = template_with_tags(&router, &template_tags).await;

    // A source server is registered by the replication agent, which this emulator does
    // not model, so the one way to hold one is to write the store directly — the same
    // seam `describe_source_servers_applies_every_modelled_filter` uses.
    let server_id = "s-00000000000000001";
    let server = engine::emulator_arn("drs", REGION, &format!("source-server/{server_id}"));
    let server_tags = json!({"kind": "source-server"});
    {
        let store = state::service_state(&*engine).expect("the DRS store");
        let mut store = store.lock();
        store.source_servers.insert(
            server_id.to_string(),
            SourceServerRecord {
                source_server_id: server_id.to_string(),
                arn: server.clone(),
                tags: [("kind".to_string(), "source-server".to_string())]
                    .into_iter()
                    .collect(),
                ..SourceServerRecord::default()
            },
        );
    }

    for (case, arn, expected) in [
        ("a recovery plan", &plan, &plan_tags),
        ("a replication configuration template", &template, &template_tags),
        ("a source server", &server, &server_tags),
    ] {
        let read = list_tags(&router, REGION, arn).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected the tags of {arn} to be served, got {read:?}"
        );
        assert_eq!(
            reported_tags(&read),
            Some(expected),
            "case {case:?}: expected {expected}, got {read:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 3 — an ARN this service holds no resource for
// ---------------------------------------------------------------------------

/// An ARN of a resource type the route addresses that this service holds no resource for
/// is refused by every operation of the route alike — 404 `NotFoundException` with the
/// message `Resource was not found` — and nothing it addresses changes. The rows are such
/// ARNs, absent for different reasons: ids nothing minted, a STORED resource's id under
/// another Region or another account, and resource types of this service that hold
/// nothing.
#[tokio::test]
async fn an_arn_this_service_holds_no_resource_for_is_not_found() {
    let (router, _engine) = drs_with_engine();
    let tags = json!({"env": "prod"});
    let present = plan_with_tags(&router, REGION, Some(&tags)).await;
    let plan_id = present
        .rsplit_once('/')
        .expect("a recovery-plan ARN")
        .1
        .to_string();

    // Every ARN below satisfies the modelled `ARN` (length 20..2048, pattern
    // `^arn:.{16,2044}$`), so each reaches the handler rather than the boundary
    // validator.
    let absent: Vec<(&str, String)> = vec![
        (
            "a recovery plan id nothing minted",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-plan/plan-00000000000000000"),
        ),
        (
            "a stored plan's id under another Region",
            format!("arn:aws:drs:eu-central-1:{ACCOUNT}:recovery-plan/{plan_id}"),
        ),
        (
            "a stored plan's id under another account",
            format!("arn:aws:drs:{REGION}:123456789012:recovery-plan/{plan_id}"),
        ),
        (
            "a replication configuration template id nothing minted",
            format!(
                "arn:aws:drs:{REGION}:{ACCOUNT}:replication-configuration-template/\
                 rct-00000000000000000"
            ),
        ),
        (
            "a source server nothing registered",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-server/s-00000000000000009"),
        ),
        (
            "a resource type of this service that holds nothing",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:job/drsjob-00000000000000000"),
        ),
        (
            "a recovery instance, another type this service holds nothing of",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-instance/i-0123456789abcdef0"),
        ),
        (
            "a source network, another type this service holds nothing of",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-network/sn-00000000000000000"),
        ),
    ];

    for (case, arn) in &absent {
        // The read, the write and the removal of the one route answer the same way: the
        // ARN resolves to no resource, and that is all the route knows about it.
        let answers = [
            ("ListTagsForResource", list_tags(&router, REGION, arn).await),
            (
                "TagResource",
                tag_resource(&router, REGION, arn, &json!({"env": "prod"})).await,
            ),
            (
                "UntagResource",
                untag_resource(&router, REGION, arn, &["env"]).await,
            ),
        ];
        for (operation, answer) in &answers {
            let case = format!("{operation} on {case}");
            assert_eq!(
                answer.status, 404,
                "case {case:?}: expected 404 for {arn}, got {answer:?}"
            );
            assert_eq!(
                answer.error_code.as_deref(),
                Some(NOT_FOUND_CODE),
                "case {case:?}: expected {NOT_FOUND_CODE}, got {answer:?}"
            );
            assert_eq!(
                text(answer, "message"),
                NOT_FOUND_MESSAGE,
                "case {case:?}: wrong message in {answer:?}"
            );
            assert!(
                reported_tags(answer).is_none(),
                "case {case:?}: a refusal must report no tags, got {answer:?}"
            );
        }
    }

    // None of those refusals touched anything: the resource that IS held still reports
    // its own tag set.
    let read = list_tags(&router, REGION, &present).await;
    assert_eq!(
        (read.status, reported_tags(&read)),
        (200, Some(&tags)),
        "the stored plan's tags must survive every refusal, got {read:?}"
    );
}

// ---------------------------------------------------------------------------
// Rule 4 — the writes of the route merge into, and remove from, the one set
// ---------------------------------------------------------------------------

/// A write sets the keys it names and leaves every other key where it is, so the set the
/// resource carries afterwards is the one it carried with the written keys overlaid. The
/// rows differ along what the rule generalises over: writing onto an empty set, adding
/// keys disjoint from the stored ones, overwriting a stored key, overwriting it with an
/// empty value, writing a key and a value at the greatest modelled length, writing the
/// same set twice, and writing nothing at all.
#[tokio::test]
async fn a_write_overlays_the_keys_it_names_on_the_set_the_resource_carries() {
    let longest_key = "k".repeat(256);
    let rows: Vec<(&str, Value, Value, Value)> = vec![
        (
            "onto a resource created with no tags",
            json!({}),
            json!({"env": "prod"}),
            json!({"env": "prod"}),
        ),
        (
            "keys disjoint from the stored ones",
            json!({"env": "prod"}),
            json!({"team": "dr", "tier": "1"}),
            json!({"env": "prod", "team": "dr", "tier": "1"}),
        ),
        (
            "a stored key overwritten",
            json!({"env": "staging", "team": "dr"}),
            json!({"env": "prod"}),
            json!({"env": "prod", "team": "dr"}),
        ),
        (
            "a stored key overwritten with an empty value",
            json!({"env": "prod"}),
            json!({"env": ""}),
            json!({"env": ""}),
        ),
        (
            "a key and a value at the greatest modelled length",
            json!({"env": "prod"}),
            one_tag(&longest_key, &"v".repeat(256)),
            {
                let mut expected = json!({"env": "prod"});
                expected[&longest_key] = Value::String("v".repeat(256));
                expected
            },
        ),
        (
            "the empty key",
            json!({"env": "prod"}),
            one_tag("", "unkeyed"),
            {
                let mut expected = json!({"env": "prod"});
                expected[""] = Value::String("unkeyed".to_string());
                expected
            },
        ),
        (
            "no key at all",
            json!({"env": "prod"}),
            json!({}),
            json!({"env": "prod"}),
        ),
    ];

    let (router, _engine) = drs_with_engine();
    for (case, created_with, written, expected) in &rows {
        let arn = plan_with_tags(&router, REGION, Some(created_with)).await;

        let answer = tag_resource(&router, REGION, &arn, written).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the write to be served, got {answer:?}"
        );

        let read = list_tags(&router, REGION, &arn).await;
        assert_eq!(
            reported_tags(&read),
            Some(expected),
            "case {case:?}: expected {expected}, got {read:?}"
        );

        // A resource has ONE tag set: what the route wrote is what the resource's own
        // reader reports.
        let plan = call_in(
            &router,
            REGION,
            "GetRecoveryPlan",
            &json!({"recoveryPlanArn": arn}),
        )
        .await;
        assert_eq!(
            plan.member("recoveryPlan").and_then(|plan| plan.get("tags")),
            Some(expected),
            "case {case:?}: the resource must carry {expected}, got {plan:?}"
        );

        // `TagResource` is modelled `@idempotent`: writing the same set again leaves the
        // resource carrying the same tags.
        let again = tag_resource(&router, REGION, &arn, written).await;
        assert_eq!(
            again.status, 200,
            "case {case:?}: expected the repeated write to be served, got {again:?}"
        );
        let read = list_tags(&router, REGION, &arn).await;
        assert_eq!(
            reported_tags(&read),
            Some(expected),
            "case {case:?}: a repeated write must leave {expected}, got {read:?}"
        );
    }
}

/// Writes naming disjoint keys MERGE: each call adds its own keys without dropping the
/// keys another call added, so after a series of such writes the resource carries the
/// union of them. The rows span how many writers there are and how many keys each one
/// names.
#[tokio::test]
async fn writes_naming_disjoint_keys_merge() {
    let (router, _engine) = drs_with_engine();
    for (case, writes) in [
        ("two writers, one key each", vec![json!({"a": "1"}), json!({"b": "2"})]),
        (
            "three writers, several keys each",
            vec![
                json!({"a": "1", "b": "2"}),
                json!({"c": "3", "d": "4"}),
                json!({"e": "5"}),
            ],
        ),
    ] {
        let arn = plan_with_tags(&router, REGION, None).await;
        let mut expected = Map::new();
        for write in &writes {
            let answer = tag_resource(&router, REGION, &arn, write).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the write {write} to be served, got {answer:?}"
            );
            for (key, value) in write.as_object().expect("a JSON object of tags") {
                expected.insert(key.clone(), value.clone());
            }
        }
        let read = list_tags(&router, REGION, &arn).await;
        assert_eq!(
            reported_tags(&read),
            Some(&Value::Object(expected.clone())),
            "case {case:?}: expected the union of every write, got {read:?}"
        );
    }
}

/// A removal takes out the keys it names and nothing else, and a key the resource does
/// not carry removes nothing — so repeating a removal answers the same way. The rows
/// differ along how many of the stored keys are named, and whether a named key is stored
/// at all.
#[tokio::test]
async fn a_removal_takes_out_only_the_keys_it_names() {
    let stored = json!({"env": "prod", "team": "dr", "tier": "1"});
    let rows: Vec<(&str, Vec<&str>, Value)> = vec![
        (
            "one of the stored keys",
            vec!["team"],
            json!({"env": "prod", "tier": "1"}),
        ),
        (
            "several of the stored keys",
            vec!["env", "tier"],
            json!({"team": "dr"}),
        ),
        ("every stored key", vec!["env", "team", "tier"], json!({})),
        ("a key the resource does not carry", vec!["absent"], stored.clone()),
        (
            "a stored key and one the resource does not carry",
            vec!["env", "absent"],
            json!({"team": "dr", "tier": "1"}),
        ),
    ];

    let (router, _engine) = drs_with_engine();
    for (case, keys, expected) in &rows {
        let arn = plan_with_tags(&router, REGION, Some(&stored)).await;

        for attempt in ["the removal", "the repeated removal"] {
            let answer = untag_resource(&router, REGION, &arn, keys).await;
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected {attempt} to be served, got {answer:?}"
            );
            let read = list_tags(&router, REGION, &arn).await;
            assert_eq!(
                reported_tags(&read),
                Some(expected),
                "case {case:?}: after {attempt}, expected {expected}, got {read:?}"
            );
        }
    }
}

/// ONE route, every resource type it addresses: a write addresses the resource its ARN
/// names, and the tags it sets and removes are the ones that resource's own reader reports
/// — the route takes no resource type, so the type can only come from the ARN.
#[tokio::test]
async fn a_write_reaches_every_kind_of_resource_through_the_one_route() {
    let (router, engine) = drs_with_engine();

    let plan = plan_with_tags(&router, REGION, Some(&json!({"kind": "recovery-plan"}))).await;
    let template = template_with_tags(&router, &json!({"kind": "template"})).await;

    // A source server is registered by the replication agent, which this emulator does
    // not model, so the one way to hold one is to write the store directly.
    let server_id = "s-00000000000000002";
    let server = engine::emulator_arn("drs", REGION, &format!("source-server/{server_id}"));
    {
        let store = state::service_state(&*engine).expect("the DRS store");
        let mut store = store.lock();
        store.source_servers.insert(
            server_id.to_string(),
            SourceServerRecord {
                source_server_id: server_id.to_string(),
                arn: server.clone(),
                tags: [("kind".to_string(), "source-server".to_string())]
                    .into_iter()
                    .collect(),
                ..SourceServerRecord::default()
            },
        );
    }

    for (case, arn, kind) in [
        ("a recovery plan", &plan, "recovery-plan"),
        ("a replication configuration template", &template, "template"),
        ("a source server", &server, "source-server"),
    ] {
        let answer = tag_resource(&router, REGION, arn, &json!({"added": "yes"})).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the write to be served, got {answer:?}"
        );
        let read = list_tags(&router, REGION, arn).await;
        assert_eq!(
            reported_tags(&read),
            Some(&json!({"kind": kind, "added": "yes"})),
            "case {case:?}: expected the written tag beside the created one, got {read:?}"
        );

        let answer = untag_resource(&router, REGION, arn, &["kind"]).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected the removal to be served, got {answer:?}"
        );
        let read = list_tags(&router, REGION, arn).await;
        assert_eq!(
            reported_tags(&read),
            Some(&json!({"added": "yes"})),
            "case {case:?}: expected only the written tag to remain, got {read:?}"
        );
    }
}

/// A resource carries at most the documented maximum of tags: the write that would take
/// it past that is refused with the modelled `ValidationException` and the set is left
/// exactly as it was, while a write that only overwrites keys the resource already
/// carries is served at the limit.
#[tokio::test]
async fn a_write_past_the_documented_tag_maximum_is_refused() {
    let (router, _engine) = drs_with_engine();
    let arn = plan_with_tags(&router, REGION, None).await;

    // Fill the set to the limit in one write, then read back what the resource carries.
    let mut at_limit = Map::new();
    for index in 0..TAGS_PER_RESOURCE_LIMIT {
        at_limit.insert(format!("key-{index}"), Value::String(index.to_string()));
    }
    let at_limit = Value::Object(at_limit);
    let answer = tag_resource(&router, REGION, &arn, &at_limit).await;
    assert_eq!(
        answer.status, 200,
        "filling the set to the limit: expected the write to be served, got {answer:?}"
    );

    // A write that only overwrites stored keys adds nothing, so it is served at the
    // limit; one that names a key the resource does not carry is refused.
    for (case, written, expect_served) in [
        ("overwriting a stored key", json!({"key-0": "overwritten"}), true),
        ("one key past the limit", json!({"one-too-many": "x"}), false),
        (
            "several keys past the limit",
            json!({"a": "1", "b": "2", "c": "3"}),
            false,
        ),
    ] {
        let before = list_tags(&router, REGION, &arn).await;
        let before = reported_tags(&before).cloned();
        let answer = tag_resource(&router, REGION, &arn, &written).await;
        let read = list_tags(&router, REGION, &arn).await;
        if expect_served {
            assert_eq!(
                answer.status, 200,
                "case {case:?}: expected the write to be served, got {answer:?}"
            );
            assert_eq!(
                reported_tags(&read).and_then(|tags| tags.get("key-0")),
                Some(&Value::String("overwritten".to_string())),
                "case {case:?}: expected the overwrite to be stored, got {read:?}"
            );
        } else {
            assert_eq!(
                answer.status, 400,
                "case {case:?}: expected 400, got {answer:?}"
            );
            assert_eq!(
                answer.error_code.as_deref(),
                Some(VALIDATION_CODE),
                "case {case:?}: expected {VALIDATION_CODE}, got {answer:?}"
            );
            assert_eq!(
                text(&answer, "message"),
                format!("Each resource can have a maximum of {TAGS_PER_RESOURCE_LIMIT} tags"),
                "case {case:?}: wrong message in {answer:?}"
            );
            assert_eq!(
                reported_tags(&read).cloned(),
                before,
                "case {case:?}: a refused write must leave the set as it was, got {read:?}"
            );
        }
    }
}
