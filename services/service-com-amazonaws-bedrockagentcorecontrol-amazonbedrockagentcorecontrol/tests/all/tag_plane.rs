// SPDX-License-Identifier: CC0-1.0

//! The TAG PLANE of this control plane (`TagResource`, `ListTagsForResource`,
//! `UntagResource`), driven end to end over the wire path (a `restJson1` request through
//! the multi-service router into the mounted service), so what is asserted is what a
//! client sees.
//!
//! THE RULE UNDER TEST: a tag operation is served against the TAGS THE ADDRESSED
//! RESOURCE CARRIES — the map that resource's own record holds — so a write merges into
//! it, a read reports it and an untag removes from it, and the ARN is resolved to a
//! resource this account holds in the Region signed for before anything is read or
//! written. A taggable resource type naming no such resource is the modelled 404, which
//! a resource whose deletion has already been accepted is too: its tags go with it.
//! Another resource type OF THIS SERVICE is not served by this plane at all (the
//! modelled `ValidationException`), and an ARN naming no resource type of this service
//! addresses nothing of this account (the modelled `AccessDeniedException`).
//!
//! The tables span what the rule generalises over: the resource FAMILY (all five the
//! operation documents), the shape of the tag set written (several keys, an empty value,
//! the longest key and value the model admits, the whole character class the pattern
//! allows), the OPERATION asked (all three, so each answer is the plane's and not one
//! handler's), the resource TYPE addressed (the five that carry tags, sixteen of this
//! service's that do not, and types of no family at all) and the SIZE of the tag set a
//! resource ends up holding (at the ceiling and one past it).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::state;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService;

/// The Region every row signs for.
const REGION: &str = "us-east-1";

/// The access key id every row signs with: the account's own published credential.
const KEY_ID: &str = "AKIDEXAMPLE";

/// How every tag operation words a `resourceArn` of a taggable type that names no
/// resource this account holds (`crate::state::tag_target_not_found`). It is not the
/// sentence the resource-policy plane of this service answers for an unresolvable ARN:
/// the two planes word it differently.
const RESOURCE_MISSING: &str = "Resource was not found";

/// The error code this service's tag plane is served with for an ARN it resolves no
/// resource for. It is NOT `ResourceNotFoundException`, the shape the operations' model
/// lists: the code a client branches on is the one the service answers.
const NOT_FOUND_CODE: &str = "NotFoundException";

/// How every tag operation words a `resourceArn` whose resource type this plane does not
/// tag: the member's own refusal, in the shape this service reports one refused member in
/// (`crate::state::tagging_unsupported_type`).
const UNSUPPORTED_TYPE: &str = "resourceArn: tagging is not supported for this resource type";

/// `TagKey`'s published ceiling, `len 1..128`
/// (`smithy_query(query="shape",shape="TagKey")`).
const MAX_TAG_KEY_LEN: usize = 128;

/// `TagValue`'s published ceiling, `len 0..256`
/// (`smithy_query(query="shape",shape="TagValue")`).
const MAX_TAG_VALUE_LEN: usize = 256;

/// How many tags one resource holds at most: `TagsMap`'s own ceiling, `len 0..50`
/// (`smithy_query(query="shape",shape="TagsMap")`).
const MAX_TAGS_PER_RESOURCE: usize = 50;

fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::default();
    router.register_service(EmulatedService::new(engine));
    router
}

fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={KEY_ID}/20260101/{REGION}/bedrock-agentcore/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

async fn send(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Bytes::new, |value| {
        Bytes::from(serde_json::to_vec(value).expect("the request body serializes"))
    });
    let mut request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", authorization())
        .header("host", "bedrock-agentcore-control.amazonaws.com");
    if body.is_some() {
        request = request.header("content-type", "application/json");
    }
    let request = request.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Percent-encode every byte a URI component may not carry literally, leaving the
/// unreserved set (RFC 3986 `ALPHA / DIGIT / "-" / "." / "_" / "~"`) alone.
fn percent_encoded(value: &str) -> String {
    /// Everything outside RFC 3986's unreserved set is encoded.
    const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    utf8_percent_encode(value, UNRESERVED).to_string()
}

/// The `/tags/{resourceArn}` path of `arn`, with the label encoded as a client encodes a
/// non-greedy `@httpLabel` (`smithy_query(query="operation",operation="TagResource")`).
fn tags_uri(arn: &str) -> String {
    format!("/tags/{}", percent_encoded(arn))
}

/// `TagResource`: `POST /tags/{resourceArn}` carrying the tag map.
async fn tag(router: &MultiServiceRouter, arn: &str, tags: &BTreeMap<String, String>) -> (u16, Value) {
    let mut body = Map::new();
    body.insert(
        "tags".to_string(),
        Value::Object(
            tags.iter()
                .map(|(key, value)| (key.clone(), Value::String(value.clone())))
                .collect(),
        ),
    );
    send(router, "POST", &tags_uri(arn), Some(&Value::Object(body))).await
}

/// `ListTagsForResource`: `GET /tags/{resourceArn}`.
async fn list_tags(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    send(router, "GET", &tags_uri(arn), None).await
}

/// `UntagResource`: `DELETE /tags/{resourceArn}?tagKeys=…`, the keys on the query string
/// (`smithy_query(query="operation",operation="UntagResource")`).
async fn untag(router: &MultiServiceRouter, arn: &str, keys: &[&str]) -> (u16, Value) {
    let query = keys
        .iter()
        .map(|key| format!("tagKeys={}", percent_encoded(key)))
        .collect::<Vec<_>>()
        .join("&");
    send(
        router,
        "DELETE",
        &format!("{}?{query}", tags_uri(arn)),
        None,
    )
    .await
}

/// The error code `restJson1` puts in `__type` / `x-amzn-errortype`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `tags` member of a `ListTagsForResource` answer, as a map a test compares.
fn reported_tags(body: &Value) -> BTreeMap<String, String> {
    body.get("tags")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
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

fn tags_of(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The sentence this endpoint denies a call in: the principal that signed — the account's
/// own published credential here — and the action the operation is performed under.
fn not_authorized(operation: &str) -> String {
    format!(
        "User: {} is not authorized to perform: bedrock-agentcore:{operation}",
        ::service_common::caller_principal::ambient_user_arn()
    )
}

async fn create_runtime(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/runtimes/",
        Some(&json!({
            "agentRuntimeName": name,
            "agentRuntimeArtifact": {"containerConfiguration": {
                "containerUri": "public.ecr.aws/agentcore/demo:v1"
            }},
            "roleArn": "arn:aws:iam::000000000000:role/AgentRole",
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the agent runtime a tag case addresses is created, got {status} ({created})"
    );
    text(&created, "agentRuntimeArn")
}

async fn create_gateway(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "POST",
        "/gateways/",
        Some(&json!({
            "name": name,
            "roleArn": "arn:aws:iam::000000000000:role/GatewayRole",
            "authorizerType": "AWS_IAM",
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the gateway a tag case addresses is created, got {status} ({created})"
    );
    text(&created, "gatewayArn")
}

async fn create_browser(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/browsers",
        Some(&json!({
            "name": name,
            "networkConfiguration": {"networkMode": "PUBLIC"},
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the browser a tag case addresses is created, got {status} ({created})"
    );
    text(&created, "browserArn")
}

async fn create_browser_profile(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(router, "PUT", "/browser-profiles", Some(&json!({"name": name}))).await;
    assert_eq!(
        status, 200,
        "setup: the browser profile a tag case addresses is created, got {status} ({created})"
    );
    text(&created, "profileArn")
}

async fn create_code_interpreter(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = send(
        router,
        "PUT",
        "/code-interpreters",
        Some(&json!({
            "name": name,
            "networkConfiguration": {"networkMode": "SANDBOX"},
        })),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: the code interpreter a tag case addresses is created, got {status} ({created})"
    );
    text(&created, "codeInterpreterArn")
}

/// Every taggable family, each created in this account, with the tag set its row writes.
///
/// The families are the ones the operation documents — "AgentCore Runtime, Browser,
/// Browser Profile, Code Interpreter tool, and Gateway"
/// (`smithy_query(query="doc",shape="TagResource")`) — and the tag sets differ along what
/// the rule generalises over: the number of pairs, an empty value, the longest key and
/// value `TagKey` / `TagValue` admit, and the whole character class their pattern allows.
async fn taggable_rows(router: &MultiServiceRouter) -> Vec<(&'static str, String, BTreeMap<String, String>)> {
    let longest_key = format!("k{}", "K".repeat(MAX_TAG_KEY_LEN - 1));
    let longest_value = "v".repeat(MAX_TAG_VALUE_LEN);
    vec![
        (
            "an agent runtime, two plain pairs",
            create_runtime(router, "tagged_runtime").await,
            tags_of(&[("team", "agents"), ("environment", "prod")]),
        ),
        (
            "a gateway, a tag whose value is empty",
            create_gateway(router, "taggedgateway").await,
            tags_of(&[("placeholder", "")]),
        ),
        (
            "a browser, the longest key and value the model admits",
            create_browser(router, "taggedBrowser").await,
            BTreeMap::from([(longest_key, longest_value)]),
        ),
        (
            "a browser profile, the character class the pattern allows",
            create_browser_profile(router, "taggedProfile").await,
            tags_of(&[("path/like:key=1+2@x-y.z", "a value with spaces . _ : / = + @ -")]),
        ),
        (
            "a code interpreter, three plain pairs",
            create_code_interpreter(router, "taggedInterpreter").await,
            tags_of(&[("a", "1"), ("b", "2"), ("c", "3")]),
        ),
    ]
}

/// RULE: a tag write is served against the tag set the addressed resource carries — the
/// keys the request names are merged in, a key already held takes the submitted value and
/// every key the request does not name is left alone — a read reports exactly that set,
/// and an untag removes exactly the keys it names, whichever taggable family the ARN
/// names and whatever the tags look like.
#[tokio::test]
async fn tags_round_trip_on_every_taggable_resource_family() {
    let router = router();

    for (case, arn, written) in taggable_rows(&router).await {
        let (status, answer) = tag(&router, &arn, &written).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected HTTP 204 from TagResource, got {status} ({answer})"
        );

        let (status, read_back) = list_tags(&router, &arn).await;
        assert_eq!(
            (status, reported_tags(&read_back)),
            (200, written.clone()),
            "case {case:?}: expected the written tag set back, got {status} ({read_back})"
        );

        // A second write MERGES: the key it names again takes the new value, the key it
        // adds appears, and the keys it does not name are unchanged.
        let first_key = written
            .keys()
            .next()
            .expect("every row writes at least one tag")
            .clone();
        let mut second = BTreeMap::new();
        second.insert(first_key.clone(), "changed".to_string());
        second.insert("addedLater".to_string(), "second write".to_string());
        let (status, answer) = tag(&router, &arn, &second).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected HTTP 204 from the second TagResource, got {status} ({answer})"
        );
        let mut expected = written.clone();
        expected.extend(second.clone());
        let (status, merged) = list_tags(&router, &arn).await;
        assert_eq!(
            (status, reported_tags(&merged)),
            (200, expected.clone()),
            "case {case:?}: expected the merged tag set, got {status} ({merged})"
        );

        // An untag removes the keys it names and nothing else; a key the resource does
        // not carry removes nothing, and the operation is idempotent.
        let (status, answer) = untag(&router, &arn, &[&first_key, "neverWritten"]).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected HTTP 204 from UntagResource, got {status} ({answer})"
        );
        expected.remove(&first_key);
        let (status, remaining) = list_tags(&router, &arn).await;
        assert_eq!(
            (status, reported_tags(&remaining)),
            (200, expected.clone()),
            "case {case:?}: expected only the named key removed, got {status} ({remaining})"
        );

        let (status, answer) = untag(&router, &arn, &[&first_key]).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected a repeated UntagResource to be accepted, got {status} \
             ({answer})"
        );
        let (status, unchanged) = list_tags(&router, &arn).await;
        assert_eq!(
            (status, reported_tags(&unchanged)),
            (200, expected),
            "case {case:?}: expected the repeat to change nothing, got {status} ({unchanged})"
        );
    }
}

/// RULE: a resource that carries no tag is reported as holding an empty tag set — it
/// exists, so the read is served — and a read changes nothing a later caller sees.
#[tokio::test]
async fn an_untagged_resource_reports_an_empty_tag_set() {
    let router = router();

    for (case, arn, _) in taggable_rows(&router).await {
        for attempt in ["the first read", "the second read"] {
            let (status, answer) = list_tags(&router, &arn).await;
            assert_eq!(
                (status, reported_tags(&answer)),
                (200, BTreeMap::new()),
                "case {case:?} ({attempt}): expected HTTP 200 with an empty tag set, got {status} \
                 ({answer})"
            );
        }
    }
}

/// RULE: a taggable resource type naming no resource this account holds in the Region
/// signed for is the modelled 404, whichever of the three operations asked.
#[tokio::test]
async fn a_taggable_type_naming_no_held_resource_is_not_found() {
    let router = router();
    let browser = create_browser(&router, "heldBrowser").await;
    let browser_id = browser.rsplit('/').next().unwrap_or_default().to_string();
    let runtime = create_runtime(&router, "held_runtime").await;
    let runtime_id = runtime.rsplit('/').next().unwrap_or_default().to_string();

    // (case, the ARN addressed) — each carries a taggable resource type, and each names no
    // resource of this account in the Region the request is signed for.
    let absent: Vec<(&str, String)> = vec![
        (
            "a runtime ARN naming no stored runtime",
            state::runtime_arn(REGION, "absent_runtime-0123456789"),
        ),
        (
            "a gateway ARN naming no stored gateway",
            state::gateway_arn(REGION, "absentgateway-0123456789"),
        ),
        (
            "a browser ARN naming no stored browser",
            state::browser_arn(REGION, "absentBrowser-0123456789"),
        ),
        (
            "a browser profile ARN naming no stored profile",
            state::browser_profile_arn(REGION, "absentProfile-0123456789"),
        ),
        (
            "a code-interpreter ARN naming no stored interpreter",
            state::code_interpreter_arn(REGION, "absentInterpreter-0123456789"),
        ),
        (
            "a stored browser's id in another Region than the request",
            state::browser_arn("eu-west-2", &browser_id),
        ),
        (
            "a stored runtime's id under another account",
            state::runtime_arn(REGION, &runtime_id)
                .replace(::engine::emulator_account_id(), "210987654321"),
        ),
    ];

    for (case, arn) in absent {
        for (operation, (status, answer)) in [
            (
                "TagResource",
                tag(&router, &arn, &tags_of(&[("probe", "absent")])).await,
            ),
            ("ListTagsForResource", list_tags(&router, &arn).await),
            ("UntagResource", untag(&router, &arn, &["probe"]).await),
        ] {
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (
                    404,
                    NOT_FOUND_CODE,
                    RESOURCE_MISSING.to_string()
                ),
                "case {case:?} ({operation}): expected HTTP 404 {NOT_FOUND_CODE} \
                 {RESOURCE_MISSING:?}, got {status} ({answer})"
            );
        }
    }
}

/// Wait until the resource `uri` addresses reports `READY`, looking again until it does
/// or a generous deadline passes.
///
/// A create of some of these families settles on the clock, and a delete is refused while
/// the resource is still coming up, so the test waits for the CONDITION rather than for a
/// span of its own choosing.
async fn await_ready(router: &MultiServiceRouter, case: &str, uri: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let (status, body) = send(router, "GET", uri, None).await;
        if status == 200 && text(&body, "status") == "READY" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the resource never reported READY, last answer {status} ({body})"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// RULE: a resource whose deletion this service has already accepted carries no tag plane
/// — the tags go with the resource — so every tag operation on it is the modelled 404,
/// and the tags another resource carries are untouched by it.
#[tokio::test]
async fn a_deleted_resource_is_no_longer_tagged_or_reported() {
    let router = router();

    let kept = create_runtime(&router, "kept_runtime").await;
    let kept_tags = tags_of(&[("keep", "me")]);
    let (status, answer) = tag(&router, &kept, &kept_tags).await;
    assert_eq!(status, 204, "setup: the kept runtime is tagged ({answer})");

    // (case, the ARN addressed, the request that deletes it) — one per taggable family,
    // whether its removal completes inside the delete call or is a span the clock owes.
    let runtime = create_runtime(&router, "doomed_runtime").await;
    let gateway = create_gateway(&router, "doomedgateway").await;
    let browser = create_browser(&router, "doomedBrowser").await;
    let profile = create_browser_profile(&router, "doomedProfile").await;
    let interpreter = create_code_interpreter(&router, "doomedInterpreter").await;
    let id_of = |arn: &str| arn.rsplit('/').next().unwrap_or_default().to_string();
    let deletions: Vec<(&str, String, String)> = vec![
        (
            "an agent runtime",
            runtime.clone(),
            format!("/runtimes/{}/", id_of(&runtime)),
        ),
        (
            "a gateway",
            gateway.clone(),
            format!("/gateways/{}/", id_of(&gateway)),
        ),
        (
            "a browser",
            browser.clone(),
            format!("/browsers/{}", id_of(&browser)),
        ),
        (
            "a browser profile",
            profile.clone(),
            format!("/browser-profiles/{}", id_of(&profile)),
        ),
        (
            "a code interpreter",
            interpreter.clone(),
            format!("/code-interpreters/{}", id_of(&interpreter)),
        ),
    ];

    for (case, arn, resource_uri) in deletions {
        let delete_uri = resource_uri.clone();
        // Tagged while it was there, so what the delete takes away is a tag set that
        // really was readable.
        let (status, answer) = tag(&router, &arn, &tags_of(&[("owner", "doomed")])).await;
        assert_eq!(
            status, 204,
            "case {case:?}: expected the live resource to accept tags, got {status} ({answer})"
        );

        // A resource still coming up refuses its own delete, so the row waits for it to
        // be READY before deleting it.
        await_ready(&router, case, &resource_uri).await;
        let (status, deleted) = send(&router, "DELETE", &delete_uri, None).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: expected the delete to be accepted, got {status} ({deleted})"
        );

        for (operation, (status, answer)) in [
            (
                "TagResource",
                tag(&router, &arn, &tags_of(&[("after", "delete")])).await,
            ),
            ("ListTagsForResource", list_tags(&router, &arn).await),
            ("UntagResource", untag(&router, &arn, &["owner"]).await),
        ] {
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (
                    404,
                    NOT_FOUND_CODE,
                    RESOURCE_MISSING.to_string()
                ),
                "case {case:?} ({operation} after the delete): expected HTTP 404 \
                 {NOT_FOUND_CODE} {RESOURCE_MISSING:?}, got {status} ({answer})"
            );
        }
    }

    // Nothing the refusals above did touched the resource that was not deleted.
    let (status, still_there) = list_tags(&router, &kept).await;
    assert_eq!(
        (status, reported_tags(&still_there)),
        (200, kept_tags),
        "case \"the kept runtime after every refusal\": expected its tags untouched, got {status} \
         ({still_there})"
    );
}

/// RULE: a resource type OF THIS SERVICE that this plane does not tag is refused with the
/// modelled `ValidationException`, and nothing is written for it.
///
/// The rows are the service's other ARN resource types, each built by this crate's own
/// reading of that family's `*Arn` pattern, so the table is the catalog rather than one
/// cited ARN. Whether the named resource exists does not enter into it: no resource of
/// these types is tagged here.
#[tokio::test]
async fn a_resource_type_this_plane_does_not_tag_is_refused_as_a_validation_error() {
    let router = router();
    let runtime = create_runtime(&router, "untouched_runtime").await;
    let kept = tags_of(&[("kept", "yes")]);
    let (status, answer) = tag(&router, &runtime, &kept).await;
    assert_eq!(status, 204, "setup: the runtime's own tags are written ({answer})");

    let vault = "default";
    // (case, the ARN addressed). Source: each family's own `*Arn` pattern as this crate's
    // `crate::state` builders render it — the credential providers and the workload
    // identity are nested under the token vault and the workload identity directory, so
    // the type their ARN carries is the parent's.
    let out_of_scope: Vec<(&str, String)> = vec![
        (
            "a workload identity ARN",
            state::workload_identity_arn(REGION, "probe-identity"),
        ),
        (
            "an API key credential provider ARN",
            state::api_key_credential_provider_arn(REGION, vault, "probe-provider"),
        ),
        // The OAuth2 credential provider's ARN type carries a digit
        // (`oauth2credentialprovider`), which `TaggableResourcesArn` does not admit in a
        // resource-type segment (`[a-z-]+`), so no tag request can carry that ARN at all.
        (
            "a payment credential provider ARN",
            state::payment_credential_provider_arn(REGION, vault, "probe-provider"),
        ),
        ("a memory ARN", state::memory_arn(REGION, "probe-0123456789")),
        ("a registry ARN", state::registry_arn(REGION, "aaaaaaaaaaaa")),
        ("a dataset ARN", state::dataset_arn(REGION, "probe-dataset")),
        (
            "a harness ARN",
            state::harness_arn(REGION, "probe-0123456789"),
        ),
        (
            "a configuration bundle ARN",
            state::configuration_bundle_arn(REGION, "probe-0123456789"),
        ),
        (
            "a capacity provider ARN",
            state::capacity_provider_arn(REGION, "probe-0123456789"),
        ),
        (
            "a payment manager ARN",
            state::payment_manager_arn(REGION, "probe-0123456789"),
        ),
        (
            "a policy engine ARN",
            state::policy_engine_arn(REGION, "probe-0123456789"),
        ),
        (
            "a consent portal ARN",
            state::consent_portal_arn(REGION, "probe-0123456789"),
        ),
        (
            "an online evaluation config ARN",
            state::online_evaluation_config_arn(REGION, "probe-0123456789"),
        ),
        (
            // `EvaluatorArn`'s own pattern, which this crate publishes no builder for.
            "an evaluator ARN",
            format!(
                "arn:aws:bedrock-agentcore:{REGION}:{}:evaluator/probe-0123456789",
                ::engine::emulator_account_id()
            ),
        ),
    ];

    for (case, arn) in out_of_scope {
        for (operation, (status, answer)) in [
            (
                "TagResource",
                tag(&router, &arn, &tags_of(&[("probe", "scope")])).await,
            ),
            ("ListTagsForResource", list_tags(&router, &arn).await),
            ("UntagResource", untag(&router, &arn, &["probe"]).await),
        ] {
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (400, "ValidationException", UNSUPPORTED_TYPE.to_string()),
                "case {case:?} ({operation}): expected HTTP 400 ValidationException \
                 {UNSUPPORTED_TYPE:?}, got {status} ({answer})"
            );
        }
    }

    // None of the refusals wrote, replaced or removed anything.
    let (status, still_there) = list_tags(&router, &runtime).await;
    assert_eq!(
        (status, reported_tags(&still_there)),
        (200, kept),
        "case \"the runtime's tags after every refusal\": expected them untouched, got {status} \
         ({still_there})"
    );
}

/// RULE: an ARN naming no resource type of this service addresses nothing of this
/// account, so it is denied — the modelled `AccessDeniedException` (403) in the sentence
/// this endpoint words a denial in, naming the principal that signed and the action asked
/// for.
#[tokio::test]
async fn an_arn_naming_no_resource_type_of_this_service_is_denied() {
    let router = router();
    let account = ::engine::emulator_account_id();

    // (case, the ARN addressed) — each satisfies `TaggableResourcesArn`'s pattern
    // (`arn:…:bedrock-agentcore:{region}:{12 digits}:{[a-z-]+}/{id}`), so every answer
    // below is the handler's and not the boundary's.
    let unrecognized: Vec<(&str, String)> = vec![
        (
            "an invented AgentCore resource type",
            format!("arn:aws:bedrock-agentcore:{REGION}:{account}:totally-bogus-type/nothing-here"),
        ),
        (
            "a resource type of another service's shape",
            format!("arn:aws:bedrock-agentcore:{REGION}:{account}:bucket/a-bucket-name"),
        ),
        (
            "a plural spelling of a type this service publishes",
            format!("arn:aws:bedrock-agentcore:{REGION}:{account}:runtimes/some_runtime-0123456789"),
        ),
    ];

    for (case, arn) in unrecognized {
        for (operation, (status, answer)) in [
            (
                "TagResource",
                tag(&router, &arn, &tags_of(&[("probe", "denied")])).await,
            ),
            ("ListTagsForResource", list_tags(&router, &arn).await),
            ("UntagResource", untag(&router, &arn, &["probe"]).await),
        ] {
            let expected = not_authorized(operation);
            assert_eq!(
                (status, error_code(&answer).as_str(), text(&answer, "message")),
                (403, "AccessDeniedException", expected.clone()),
                "case {case:?} ({operation}): expected HTTP 403 AccessDeniedException \
                 {expected:?}, got {status} ({answer})"
            );
        }
    }
}

/// RULE: the tag set a resource carries is bounded by `TagsMap`'s ceiling, so a write
/// that would take it past that is the modelled quota error and changes nothing, while a
/// write that lands exactly at the ceiling — or replaces a key already held while the set
/// is full — is served.
#[tokio::test]
async fn a_tag_set_is_bounded_by_the_modelled_ceiling() {
    let router = router();
    let arn = create_runtime(&router, "bounded_runtime").await;

    let at_ceiling: BTreeMap<String, String> = (0..MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("key{index:02}"), format!("value{index:02}")))
        .collect();
    let (status, answer) = tag(&router, &arn, &at_ceiling).await;
    assert_eq!(
        status, 204,
        "case \"a tag set exactly at the ceiling\": expected HTTP 204, got {status} ({answer})"
    );
    let (status, read_back) = list_tags(&router, &arn).await;
    assert_eq!(
        (status, reported_tags(&read_back)),
        (200, at_ceiling.clone()),
        "case \"a tag set exactly at the ceiling\": expected all {MAX_TAGS_PER_RESOURCE} tags \
         back, got {status} ({read_back})"
    );

    // A key already held is replaced rather than added, so a full set still takes a write.
    let replacement = tags_of(&[("key00", "replaced")]);
    let (status, answer) = tag(&router, &arn, &replacement).await;
    assert_eq!(
        status, 204,
        "case \"replacing a key while the set is full\": expected HTTP 204, got {status} ({answer})"
    );

    // One key past the ceiling is the modelled quota error, and nothing is written.
    let (status, refused) = tag(&router, &arn, &tags_of(&[("oneTooMany", "51")])).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (402, "ServiceQuotaExceededException"),
        "case \"one tag past the ceiling\": expected HTTP 402 ServiceQuotaExceededException, got \
         {status} ({refused})"
    );
    let mut expected = at_ceiling;
    expected.extend(replacement);
    let (status, after) = list_tags(&router, &arn).await;
    assert_eq!(
        (status, reported_tags(&after)),
        (200, expected),
        "case \"one tag past the ceiling\": expected the stored set unchanged, got {status} \
         ({after})"
    );
}
