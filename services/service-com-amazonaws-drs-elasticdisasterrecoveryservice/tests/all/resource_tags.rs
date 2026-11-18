// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the DRS tagging route's read, `ListTagsForResource`
//! (`GET /tags/{resourceArn}`).
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
//!    with none — whatever the shape of that set, and for EVERY kind of resource this
//!    service holds, because the route addresses a resource by ARN alone and takes no
//!    resource type.
//! 2. It is a READ: the same ARN reports the same tag set however often it is asked, and
//!    the resource's own reader keeps reporting that set too.
//! 3. An ARN this service holds no resource for is 404 `ResourceNotFoundException`,
//!    reporting the ARN the request named and this service's name for the kind of
//!    resource that ARN addresses. The ways an ARN can name nothing span the rows: an id
//!    nothing minted, a stored resource's id under another Region or account, a kind of
//!    this service's that holds nothing, and another service's resource.

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

/// The code the model declares for a resource this operation found nothing for
/// (`smithy_query(query="operation",operation="ListTagsForResource")` lists
/// `ResourceNotFoundException`, HTTP 404).
const NOT_FOUND_CODE: &str = "ResourceNotFoundException";

/// This service's names for the kinds of resource an ARN can address, written out here
/// as the literals a client reads rather than taken from the implementation's constants,
/// so a name that drifts fails this suite.
const RECOVERY_PLAN_TYPE: &str = "Recovery Plan";
const TEMPLATE_TYPE: &str = "Replication Configuration Template";
const SOURCE_SERVER_TYPE: &str = "Source Server";

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
async fn list_tags(router: &MultiServiceRouter, region: &str, resource_arn: &str) -> Answer {
    call_path(
        router,
        region,
        "GET",
        &format!("/tags/{}", encoded_label(resource_arn)),
        &[],
    )
    .await
}

/// The `tags` member of an answer, for an assertion that names the case.
fn reported_tags(answer: &Answer) -> Option<&Value> {
    answer.member("tags")
}

/// A string member of an answer's body.
fn text(answer: &Answer, name: &str) -> String {
    answer
        .member(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Create a Recovery Plan, optionally tagged, and answer its ARN.
async fn plan_with_tags(router: &MultiServiceRouter, region: &str, tags: Option<&Value>) -> String {
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

/// ONE route, every kind of resource: the tag set reported for an ARN is the set the
/// resource THAT ARN addresses carries, whether the resource was created through an
/// operation of this service or registered by the replication agent — the route takes no
/// resource type, so the kind can only come from the ARN.
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

/// An ARN this service holds no resource for is 404 `ResourceNotFoundException` naming
/// the ARN the request asked for, and nothing it addresses changes. The rows are ARNs
/// absent for different reasons: ids nothing minted, a STORED resource's id under
/// another Region or another account, a resource kind of this service that holds
/// nothing, and another service's resource — each reported with this service's name for
/// the kind the ARN addresses, or with none when the ARN addresses no kind of this
/// service.
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
    let absent: Vec<(&str, String, Option<&str>)> = vec![
        (
            "a recovery plan id nothing minted",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-plan/plan-00000000000000000"),
            Some(RECOVERY_PLAN_TYPE),
        ),
        (
            "a stored plan's id under another Region",
            format!("arn:aws:drs:eu-central-1:{ACCOUNT}:recovery-plan/{plan_id}"),
            Some(RECOVERY_PLAN_TYPE),
        ),
        (
            "a stored plan's id under another account",
            format!("arn:aws:drs:{REGION}:123456789012:recovery-plan/{plan_id}"),
            Some(RECOVERY_PLAN_TYPE),
        ),
        (
            "a template id nothing minted",
            format!(
                "arn:aws:drs:{REGION}:{ACCOUNT}:replication-configuration-template/\
                 rct-00000000000000000"
            ),
            Some(TEMPLATE_TYPE),
        ),
        (
            "a source server nothing registered",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:source-server/s-00000000000000009"),
            Some(SOURCE_SERVER_TYPE),
        ),
        (
            "a resource kind of this service that holds nothing",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:job/drsjob-00000000000000000"),
            None,
        ),
        (
            "a recovery instance, another kind this service holds nothing of",
            format!("arn:aws:drs:{REGION}:{ACCOUNT}:recovery-instance/i-0123456789abcdef0"),
            None,
        ),
        (
            "another service's resource",
            format!("arn:aws:ec2:{REGION}:{ACCOUNT}:subnet/subnet-0123456789abcdef0"),
            None,
        ),
    ];

    for (case, arn, resource_type) in &absent {
        let answer = list_tags(&router, REGION, arn).await;
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
            text(&answer, "resourceId"),
            *arn,
            "case {case:?}: resourceId must be the ARN the request named, got {answer:?}"
        );
        assert_eq!(
            answer.member("resourceType").and_then(Value::as_str),
            *resource_type,
            "case {case:?}: expected resourceType {resource_type:?}, got {answer:?}"
        );
        assert_eq!(
            text(&answer, "message"),
            format!(
                "{} with Arn: '{arn}' was not found",
                resource_type.unwrap_or("Resource")
            ),
            "case {case:?}: wrong message in {answer:?}"
        );
        assert!(
            reported_tags(&answer).is_none(),
            "case {case:?}: a refusal must report no tags, got {answer:?}"
        );
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
