// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of this service's three tagging operations — `TagResource`,
//! `UntagResource` and `ListTagsForResource`.
//!
//! RULE. A tagging call of this service addresses a resource by an ARN of this model's own
//! `TaggableResourceArn` (`smithy_query(query="shape",shape="TaggableResourceArn")`:
//! `@length(20..1011)` over
//! `^arn:aws(|-cn|-iso|-iso-[a-z]|-us-gov):bedrock:[a-z0-9-]*:[0-9]{12}:(blueprint|data-automation-project|blueprint-optimization-invocation|data-automation-library|data-automation-library-ingestion-job)/[a-zA-Z0-9-]{12,36}$`),
//! reads or writes the tag list the resource that ARN names HOLDS, and answers the modelled
//! `ResourceNotFoundException` (HTTP 404) for an ARN this store holds no such resource
//! under. The constraint a value of that member is judged by is THIS model's, never a
//! neighbouring Bedrock model's taggable-resource syntax, whose alternation names none of
//! this service's resource types.
//!
//! WHY BOTH SERVICES ARE MOUNTED. `POST /tagResource`, `POST /untagResource` and
//! `POST /listTagsForResource` are bound identically by this model and by the Bedrock
//! control plane, both sign as `bedrock`, and the family's claim rule
//! (`service_common::bedrock`) gives that whole segment to the control plane — so these
//! three operations are reached exactly as the assembled emulator reaches them: both
//! services on ONE engine, the control plane claiming the path and handing a request whose
//! `resourceARN` names one of THIS service's resources to this model.
//!
//! The suites are TABLE-driven. The rows differ along what the rule generalises over: the
//! resource FAMILY the ARN names (every family of this service a create here can mint), the
//! OPERATION, the tag set (tags from the create, a first set, a merge, a removal, a key the
//! resource never carried), the resource kind an absent ARN spells, and the way a value can
//! fall outside the modelled ARN syntax (identifier too short, too long, out of its
//! character class, a region or an account field outside the pattern's).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService as ControlPlane;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code};

/// The region every case signs for; the creates mint their ARNs in it.
const REGION: &str = "us-east-1";

/// The modelled `TaggableResourceArn` pattern as a refusal quotes it: the expression this
/// service advertises, without the `^`/`$` a whole-value pattern wraps it in
/// (`crate::support` has no copy of it; `src/service/validation.rs` states the rule).
const TAGGABLE_RESOURCE_ARN_PATTERN: &str = "arn:aws(|-cn|-iso|-iso-[a-z]|-us-gov):bedrock:\
     [a-z0-9-]*:[0-9]{12}:(blueprint|data-automation-project|\
     blueprint-optimization-invocation|data-automation-library|\
     data-automation-library-ingestion-job)/[a-zA-Z0-9-]{12,36}";

/// A SigV4 `Authorization` header for the signing name the whole Bedrock family shares.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{region}/bedrock/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

/// One answered request: the status, the decoded body, and the error type the wire carries
/// (the `x-amzn-errortype` header, else the body's `__type`).
struct Reply {
    status: u16,
    body: Value,
    error: Option<String>,
}

/// This service beside the Bedrock control plane on ONE engine, which is how the assembled
/// emulator mounts them — and the only way a request on the three shared tagging paths
/// reaches this model's handlers. Request authentication is off, so the suite signs nothing
/// for real.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ControlPlane::new(Arc::clone(&engine)));
    router
}

/// Dispatch one request and read its status, body and error type.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Reply {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-data-automation.amazonaws.com")
        .header("content-type", "application/json")
        .header("authorization", authorization(REGION))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        // The header's value may carry a trailing shape-id suffix after a `:`; the error
        // type is the part before it.
        .and_then(|value| value.split(':').next())
        .map(str::to_string);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    let error = header_error.or_else(|| body.get("__type").map(|_| error_code(&body)));
    Reply {
        status,
        body,
        error,
    }
}

/// A `TagList` out of `(key, value)` pairs.
fn tag_list(pairs: &[(&str, &str)]) -> Value {
    Value::Array(
        pairs
            .iter()
            .map(|(key, value)| json!({ "key": key, "value": value }))
            .collect(),
    )
}

async fn tag(router: &MultiServiceRouter, resource_arn: &str, tags: Value) -> Reply {
    call(
        router,
        "POST",
        "/tagResource",
        &json!({ "resourceARN": resource_arn, "tags": tags }),
    )
    .await
}

async fn untag(router: &MultiServiceRouter, resource_arn: &str, keys: &[&str]) -> Reply {
    call(
        router,
        "POST",
        "/untagResource",
        &json!({ "resourceARN": resource_arn, "tagKeys": keys }),
    )
    .await
}

async fn list_tags(router: &MultiServiceRouter, resource_arn: &str) -> Reply {
    call(
        router,
        "POST",
        "/listTagsForResource",
        &json!({ "resourceARN": resource_arn }),
    )
    .await
}

/// The tags a listing reported, as `(key, value)` pairs in the order served.
fn listed(case: &str, reply: &Reply) -> Vec<(String, String)> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected ListTagsForResource 200, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("tags")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("case {case:?}: no tags member in {:?}", reply.body))
        .iter()
        .map(|tag| {
            (
                tag.get("key")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                tag.get("value")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// Assert that a `TagResource`/`UntagResource` answered its modelled success: 200 with a
/// body carrying no member, which is what an empty output structure serializes to.
fn assert_empty_success(case: &str, operation: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: expected {operation} 200, got {} with {:?}",
        reply.status, reply.body
    );
    let empty = match &reply.body {
        Value::Null => true,
        Value::Object(members) => members.is_empty(),
        _ => false,
    };
    assert!(
        empty,
        "case {case:?}: expected {operation} to answer a body with no member, got {:?}",
        reply.body
    );
}

/// A blueprint of this service, created through its own `PUT /blueprints/`, carrying the
/// tags its create named. Returns its ARN.
async fn given_blueprint(router: &MultiServiceRouter, name: &str, tags: Value) -> String {
    let reply = call(
        router,
        "PUT",
        "/blueprints/",
        &json!({
            "blueprintName": name,
            "type": "DOCUMENT",
            "schema": r#"{"class":"invoice"}"#,
            "tags": tags,
        }),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "the fixture blueprint must be created, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("blueprint")
        .and_then(|blueprint| blueprint.get("blueprintArn"))
        .and_then(Value::as_str)
        .expect("the create answers the blueprint's ARN")
        .to_string()
}

/// A data-automation project, created through `PUT /data-automation-projects/`.
async fn given_project(router: &MultiServiceRouter, name: &str, tags: Value) -> String {
    let reply = call(
        router,
        "PUT",
        "/data-automation-projects/",
        &json!({
            "projectName": name,
            "standardOutputConfiguration": {},
            "tags": tags,
        }),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "the fixture project must be created, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("projectArn")
        .and_then(Value::as_str)
        .expect("the create answers the project's ARN")
        .to_string()
}

/// A data-automation library, created through `PUT /data-automation-libraries/`.
async fn given_library(router: &MultiServiceRouter, name: &str, tags: Value) -> String {
    let reply = call(
        router,
        "PUT",
        "/data-automation-libraries/",
        &json!({ "libraryName": name, "tags": tags }),
    )
    .await;
    assert_eq!(
        reply.status, 201,
        "the fixture library must be created, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("libraryArn")
        .and_then(Value::as_str)
        .expect("the create answers the library's ARN")
        .to_string()
}

/// A blueprint-optimization invocation, submitted through
/// `POST /invokeBlueprintOptimizationAsync` over a blueprint created for it.
async fn given_optimization_invocation(router: &MultiServiceRouter, tags: Value) -> String {
    let blueprint = given_blueprint(router, "optimized-blueprint", tag_list(&[])).await;
    let reply = call(
        router,
        "POST",
        "/invokeBlueprintOptimizationAsync",
        &json!({
            "blueprint": { "blueprintArn": blueprint },
            "samples": [{
                "assetS3Object": { "s3Uri": "s3://bda-samples/assets/invoice-0.pdf" },
                "groundTruthS3Object": { "s3Uri": "s3://bda-samples/truth/invoice-0.json" },
            }],
            "outputConfiguration": { "s3Object": { "s3Uri": "s3://bda-output/run.json" } },
            "dataAutomationProfileArn": format!(
                "arn:aws:bedrock:{REGION}:{}:data-automation-profile/us.data-automation-v1",
                engine::emulator_account_id()
            ),
            "tags": tags,
        }),
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "the fixture optimization invocation must be submitted, got {} with {:?}",
        reply.status, reply.body
    );
    reply
        .body
        .get("invocationArn")
        .and_then(Value::as_str)
        .expect("the submission answers the invocation's ARN")
        .to_string()
}

/// One resource a tagging call can address: the case it stands for, its ARN, and the tags
/// its create carried.
struct Target {
    case: &'static str,
    arn: String,
    created_with: Vec<(String, String)>,
}

/// The pairs `tag_list` was built from, as a listing reports them.
fn held(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// RULE: for every family of this service a tagging call can address, the tag list the
/// resource HOLDS is what `ListTagsForResource` reports, `TagResource` MERGES into it (a key
/// already held takes the new value in place, a new key is appended) and `UntagResource`
/// drops exactly the keys it names — each answering its modelled 200.
#[tokio::test]
async fn every_taggable_family_round_trips_the_tags_its_resource_holds() {
    let router = emulator();

    let targets: Vec<Target> = vec![
        Target {
            case: "a blueprint created with one tag",
            arn: given_blueprint(&router, "tagged-blueprint", tag_list(&[("team", "data-eng")]))
                .await,
            created_with: held(&[("team", "data-eng")]),
        },
        Target {
            case: "a blueprint created with none",
            arn: given_blueprint(&router, "untagged-blueprint", tag_list(&[])).await,
            created_with: Vec::new(),
        },
        Target {
            case: "a data-automation project created with two tags",
            arn: given_project(
                &router,
                "tagged-project",
                tag_list(&[("team", "data-eng"), ("cost-center", "4711")]),
            )
            .await,
            created_with: held(&[("team", "data-eng"), ("cost-center", "4711")]),
        },
        Target {
            case: "a data-automation library created with one tag",
            arn: given_library(&router, "tagged-library", tag_list(&[("tier", "gold")])).await,
            created_with: held(&[("tier", "gold")]),
        },
        Target {
            case: "a blueprint-optimization invocation created with one tag",
            arn: given_optimization_invocation(&router, tag_list(&[("run", "nightly")])).await,
            created_with: held(&[("run", "nightly")]),
        },
    ];

    for Target {
        case,
        arn,
        created_with,
    } in targets
    {
        // What the resource HOLDS from its create is what the listing reports — and a
        // resource carrying none reports an empty list rather than a refusal.
        assert_eq!(
            listed(case, &list_tags(&router, &arn).await),
            created_with,
            "case {case:?}: the listing must report the tags the create carried"
        );

        // A first set, then a merge over it.
        let set = tag(&router, &arn, tag_list(&[("env", "test"), ("owner", "ada")])).await;
        assert_empty_success(case, "TagResource", &set);
        let merged = tag(&router, &arn, tag_list(&[("env", "prod")])).await;
        assert_empty_success(case, "a merging TagResource", &merged);

        let mut expected = created_with.clone();
        expected.push(("env".to_string(), "prod".to_string()));
        expected.push(("owner".to_string(), "ada".to_string()));
        assert_eq!(
            listed(case, &list_tags(&router, &arn).await),
            expected,
            "case {case:?}: the listing must report one value per key, the last set"
        );

        // A listing is a REPORT: reading twice reports the same list.
        assert_eq!(
            listed(case, &list_tags(&router, &arn).await),
            expected,
            "case {case:?}: a second listing must report the same list"
        );

        // Untagging drops exactly the keys it names; a key the resource never carried
        // removes nothing.
        let dropped = untag(&router, &arn, &["env", "never-set"]).await;
        assert_empty_success(case, "UntagResource", &dropped);
        expected.retain(|(key, _)| key != "env");
        assert_eq!(
            listed(case, &list_tags(&router, &arn).await),
            expected,
            "case {case:?}: the listing must report the remaining tags"
        );
    }
}

/// RULE: an ARN of a resource type this model's taggable-resource syntax admits, naming no
/// resource this store holds, is refused by all three operations with the modelled
/// `ResourceNotFoundException` — HTTP 404, that error type on the wire, and the sentence
/// `<Kind> <resourceARN> does not exist` with the kind word this service names that resource
/// kind by around the ARN the request carried.
///
/// The rows span every resource type of the modelled alternation
/// (`smithy_query(query="shape",shape="TaggableResourceArn")`), including the two no create
/// in this suite mints.
#[tokio::test]
async fn an_arn_this_store_holds_no_resource_under_is_the_modelled_404() {
    let router = emulator();
    let account = engine::emulator_account_id();

    // (case, the resource type the ARN spells, the kind word the refusal names it by)
    let rows: [(&str, &str, &str); 5] = [
        ("a blueprint", "blueprint", "Blueprint"),
        ("a project", "data-automation-project", "Project"),
        ("a library", "data-automation-library", "Library"),
        (
            "an optimization invocation",
            "blueprint-optimization-invocation",
            "Invocation",
        ),
        (
            "a library ingestion job",
            "data-automation-library-ingestion-job",
            "Ingestion Job",
        ),
    ];

    for (case, resource_type, kind) in rows {
        let arn = format!("arn:aws:bedrock:{REGION}:{account}:{resource_type}/0123456789ab-0000");
        let expected = format!("{kind} {arn} does not exist");
        let calls: Vec<(&str, Reply)> = vec![
            (
                "TagResource",
                tag(&router, &arn, tag_list(&[("team", "data-eng")])).await,
            ),
            ("UntagResource", untag(&router, &arn, &["team"]).await),
            ("ListTagsForResource", list_tags(&router, &arn).await),
        ];
        for (operation, reply) in calls {
            let case = format!("{case} / {operation}");
            assert_eq!(
                reply.status, 404,
                "case {case:?}: expected 404, got {} with {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case:?}: expected ResourceNotFoundException, got {:?}",
                reply.error
            );
            let message = reply
                .body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}

/// RULE: existence is decided against what the store HOLDS at the time of the call, so a
/// resource released by its delete takes its tags with it: the very ARN that was tagged
/// answers the modelled 404 once the resource is gone.
#[tokio::test]
async fn a_released_resource_takes_its_tags_with_it() {
    let router = emulator();
    let arn = given_blueprint(&router, "deleted-blueprint", tag_list(&[("team", "data-eng")])).await;
    assert_eq!(
        listed("before the delete", &list_tags(&router, &arn).await),
        held(&[("team", "data-eng")]),
        "case \"before the delete\": the listing must report the create's tag"
    );

    let deleted = call(
        &router,
        "DELETE",
        &format!("/blueprints/{}/", encode_label(&arn)),
        &json!({}),
    )
    .await;
    assert_eq!(
        deleted.status, 200,
        "the fixture blueprint must be deleted, got {} with {:?}",
        deleted.status, deleted.body
    );

    let expected = format!("Blueprint {arn} does not exist");
    for (operation, reply) in [
        (
            "TagResource",
            tag(&router, &arn, tag_list(&[("team", "data-eng")])).await,
        ),
        ("UntagResource", untag(&router, &arn, &["team"]).await),
        ("ListTagsForResource", list_tags(&router, &arn).await),
    ] {
        assert_eq!(
            reply.status, 404,
            "case {operation:?} after the delete: expected 404, got {} with {:?}",
            reply.status, reply.body
        );
        let message = reply
            .body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            message, expected,
            "case {operation:?} after the delete: expected {expected:?}, got {message:?}"
        );
    }
}

/// RULE: a `resourceARN` outside the modelled `TaggableResourceArn` is refused 400
/// `ValidationException` against THIS model's pattern — the one clause names the member and
/// quotes this service's own expression, and names none of a neighbouring Bedrock model's
/// resource types.
///
/// The rows differ along the ways a value can miss that pattern: an identifier under its
/// 12-character lower bound, one over its 36-character upper bound, one outside its
/// character class, a region field outside `[a-z0-9-]*` and an account field that is not 12
/// digits. Every row is well over the shape's 20-character minimum length, so the pattern
/// is the only requirement it misses.
#[tokio::test]
async fn an_arn_outside_this_models_taggable_syntax_is_judged_by_this_models_pattern() {
    let router = emulator();
    let account = engine::emulator_account_id();
    let expected = format!(
        "1 validation error detected: Value at 'resourceARN' failed to satisfy constraint: \
         Member must satisfy regular expression pattern: {TAGGABLE_RESOURCE_ARN_PATTERN}"
    );

    // (case, the resourceARN the call carries)
    let rows: Vec<(&str, String)> = vec![
        (
            "an identifier one character under the 12-character lower bound",
            format!("arn:aws:bedrock:{REGION}:{account}:blueprint/0123456789a"),
        ),
        (
            "an identifier one character over the 36-character upper bound",
            format!(
                "arn:aws:bedrock:{REGION}:{account}:data-automation-project/{}",
                "a".repeat(37)
            ),
        ),
        (
            "an identifier carrying a character outside its class",
            format!("arn:aws:bedrock:{REGION}:{account}:data-automation-library/under_score1"),
        ),
        (
            "a region field outside the pattern's character class",
            format!("arn:aws:bedrock:US-EAST-1:{account}:blueprint/0123456789ab"),
        ),
        (
            "an account field that is not twelve digits",
            format!("arn:aws:bedrock:{REGION}:12345:blueprint/0123456789ab"),
        ),
    ];

    for (case, arn) in rows {
        let calls: Vec<(&str, Reply)> = vec![
            (
                "TagResource",
                tag(&router, &arn, tag_list(&[("team", "data-eng")])).await,
            ),
            ("UntagResource", untag(&router, &arn, &["team"]).await),
            ("ListTagsForResource", list_tags(&router, &arn).await),
        ];
        for (operation, reply) in calls {
            let case = format!("{case} / {operation}");
            assert_eq!(
                reply.status, 400,
                "case {case:?}: expected 400, got {} with {:?}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ValidationException"),
                "case {case:?}: expected ValidationException, got {:?}",
                reply.error
            );
            let message = reply
                .body
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                message, expected,
                "case {case:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
