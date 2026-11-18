// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for `UpdateDistributionConfiguration`.
//!
//! Every case drives the real wire path — the production `MultiServiceRouter`, restJson1
//! serialization, the modelled-constraint walk and the handlers — so what is asserted is what
//! a signed AWS SDK request observes.
//!
//! The rules under test:
//!
//! * an accepted update REPLACES the stored configuration: the operation "doesn't support
//!   selective updates. The request replaces the stored configuration, so include every
//!   setting that you want to keep", and `distributions` "replaces the configuration's
//!   existing distributions entirely"
//!   (`smithy_query(query="operation",operation="UpdateDistributionConfiguration")`), so the
//!   following read reports every entry of the list the request carried — in its order — and
//!   clears a description the request omitted, while the name the ARN was derived from, the
//!   resource's own tags and the instant it was created stay as they were;
//! * `dateUpdated` is the instant of the last update, so a read reports no such member until
//!   one has run and the last one's instant after that;
//! * an ARN this Region holds no configuration under names nothing, which is reported MISSING
//!   rather than as an invalid request;
//! * `clientToken` is an idempotency token: a retry carrying it answers the ORIGINAL response
//!   and writes nothing again, and the same token carrying different settings is the modelled
//!   mismatch.
//!
//! The rows span what the rules generalise over: how many entries the replacement list holds,
//! whether it adds, drops, reorders or re-settles a target, and which optional member the
//! request carries.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call here is signed for unless a case says otherwise.
const REGION: &str = "us-west-2";

/// A second Region, so a configuration of one Region can be told from another's.
const OTHER_REGION: &str = "eu-west-1";

/// The canonical emulator account (`engine::EMULATOR_ACCOUNT_ID`) every ARN carries.
const ACCOUNT: &str = "000000000000";

/// One emulator with an empty store, mounted exactly as the router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_imagebuilder_imagebuilder::EmulatedService::new(engine),
    );
    router
}

/// A request signed for `imagebuilder` in `region`.
fn request(
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> http::Request<bytes::Bytes> {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("imagebuilder.{region}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDIMAGEBUILDER/20260101/{region}/imagebuilder/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        );
    let payload = match body {
        Some(value) => {
            builder = builder.header("content-type", "application/json");
            bytes::Bytes::from(serde_json::to_vec(value).expect("serialize the request body"))
        }
        None => bytes::Bytes::new(),
    };
    builder.body(payload).expect("a well-formed test request")
}

/// Status and parsed body of one call made in `region`; the error code is folded into the
/// body as `__type`, so the header and the body are read as one.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let response = router.dispatch(request(region, method, uri, body)).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let mut parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    if let (Some(error_type), Value::Object(map)) = (error_type, &mut parsed) {
        map.entry("__type")
            .or_insert_with(|| Value::String(error_type));
    }
    (status, parsed)
}

/// `CreateDistributionConfiguration` in `region`.
async fn create_in(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in(
        router,
        region,
        "PUT",
        "/CreateDistributionConfiguration",
        Some(body),
    )
    .await
}

/// `UpdateDistributionConfiguration` in `region`.
async fn update_in(router: &MultiServiceRouter, region: &str, body: &Value) -> (u16, Value) {
    call_in(
        router,
        region,
        "PUT",
        "/UpdateDistributionConfiguration",
        Some(body),
    )
    .await
}

/// `GetDistributionConfiguration` for `arn`, in `region`.
async fn get_in(router: &MultiServiceRouter, region: &str, arn: &str) -> (u16, Value) {
    call_in(
        router,
        region,
        "GET",
        &format!("/GetDistributionConfiguration?distributionConfigurationArn={arn}"),
        None,
    )
    .await
}

/// The ARN a distribution configuration named `normalized` carries in `region`.
fn arn_of(region: &str, normalized: &str) -> String {
    format!("arn:aws:imagebuilder:{region}:{ACCOUNT}:distribution-configuration/{normalized}")
}

/// One `distributions` entry naming `region` as its target, whose AMI settings name
/// `ami_name` when it names one.
fn entry(region: &str, ami_name: Option<&str>) -> Value {
    let mut settings = Map::new();
    if let Some(name) = ami_name {
        settings.insert("name".to_string(), json!(name));
    }
    json!({ "region": region, "amiDistributionConfiguration": Value::Object(settings) })
}

/// An update body naming `arn`, carrying `client_token`, the replacement `distributions` and
/// whatever else `extra` names.
fn update_body(arn: &str, client_token: &str, distributions: Value, extra: Value) -> Value {
    let mut body = Map::new();
    body.insert("distributionConfigurationArn".to_string(), json!(arn));
    body.insert("clientToken".to_string(), json!(client_token));
    body.insert("distributions".to_string(), distributions);
    if let Value::Object(extra) = extra {
        for (member, value) in extra {
            body.insert(member, value);
        }
    }
    Value::Object(body)
}

/// The error code a refusal carries, from `__type`.
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an error shape, got {body}"))
        .rsplit(['#', '/'])
        .next()
        .expect("a non-empty error type")
        .to_string()
}

/// The `message` of a refusal.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a message member in {body}"))
        .to_string()
}

/// Assert `status` and the error code of a refusal.
fn assert_refused(case: &str, status: u16, body: &Value, expect_status: u16, expect_code: &str) {
    assert_eq!(
        status, expect_status,
        "case {case:?}: expected HTTP {expect_status}, got {status} with {body}"
    );
    let code = error_code(case, body);
    assert_eq!(
        code, expect_code,
        "case {case:?}: expected the error code {expect_code:?}, got {code:?}"
    );
}

/// The `distributionConfiguration` of a successful read.
fn configuration(case: &str, body: &Value) -> Value {
    body.get("distributionConfiguration")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected a distributionConfiguration in {body}"))
}

/// The list a stored configuration was created with, which every row below replaces.
fn created_distributions() -> Value {
    json!([entry(REGION, Some("golden-{{ imagebuilder:buildDate }}"))])
}

/// Create the configuration named `name` in [`REGION`], with one target Region, a description
/// and a tag, and answer its ARN.
async fn stored(router: &MultiServiceRouter, name: &str) -> String {
    let body = json!({
        "name": name,
        "clientToken": format!("create-{name}"),
        "distributions": created_distributions(),
        "description": "the configuration as created",
        "tags": {"owner": "platform"},
    });
    let (status, created) = create_in(router, REGION, &body).await;
    assert_eq!(
        status, 200,
        "case {name:?}: expected the create to succeed, got {status} with {created}"
    );
    arn_of(REGION, name)
}

/// RULE: the replacement list is what the following read reports, whole and in its order.
///
/// The rows differ along what the rule generalises over: how many entries the replacement
/// holds, and whether it ADDS a target, drops one, reorders them or only re-settles the one
/// the create named.
#[tokio::test]
async fn an_accepted_update_replaces_the_distributions_list() {
    // (case, the replacement list)
    let rows: Vec<(&str, Value)> = vec![
        (
            "a second target Region added",
            json!([
                entry(REGION, Some("golden-{{ imagebuilder:buildDate }}")),
                entry("eu-central-1", Some("golden-{{ imagebuilder:buildDate }}")),
            ]),
        ),
        (
            "three target Regions, the created one last",
            json!([
                entry("ap-southeast-3", None),
                entry("eu-west-1", Some("{{ imagebuilder:buildDate }}-app")),
                entry(REGION, Some("golden-{{ imagebuilder:buildDate }}")),
            ]),
        ),
        (
            "one target Region, somewhere else entirely",
            json!([entry("me-central-1", None)]),
        ),
        (
            "the created target, with its AMI settings re-settled",
            json!([entry(REGION, Some("renamed-{{ imagebuilder:buildDate }}"))]),
        ),
        (
            "an empty list, which names no target at all",
            json!([]),
        ),
    ];

    for (index, (case, replacement)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("replaced-{index}");
        let arn = stored(&router, &name).await;

        let (status, read) = get_in(&router, REGION, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the created configuration to read back, got {status} with \
             {read}"
        );
        let before = configuration(case, &read);
        assert_eq!(
            before.get("dateUpdated"),
            None,
            "case {case:?}: expected no dateUpdated before any update, got {before}"
        );
        let created_at = before
            .get("dateCreated")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected a dateCreated in {before}"));

        let body = update_body(&arn, &format!("update-{index}"), replacement.clone(), json!({}));
        let (status, updated) = update_in(&router, REGION, &body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from the update, got {status} with {updated}"
        );
        let answered_arn = updated
            .get("distributionConfigurationArn")
            .and_then(Value::as_str);
        assert_eq!(
            answered_arn,
            Some(arn.as_str()),
            "case {case:?}: expected the update to answer the ARN {arn:?}, got {answered_arn:?}"
        );
        let token = updated.get("clientToken").and_then(Value::as_str);
        assert_eq!(
            token,
            Some(format!("update-{index}").as_str()),
            "case {case:?}: expected the update to echo its client token, got {token:?}"
        );
        let request_id = updated.get("requestId").and_then(Value::as_str);
        assert!(
            request_id.is_some_and(|id| !id.is_empty()),
            "case {case:?}: expected the update to name a requestId, got {request_id:?}"
        );

        let (status, read) = get_in(&router, REGION, &arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the updated configuration to read back, got {status} with \
             {read}"
        );
        let after = configuration(case, &read);

        let stored_list = after
            .get("distributions")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected distributions in {after}"));
        assert_eq!(
            stored_list, replacement,
            "case {case:?}: expected the stored distributions {replacement}, got {stored_list}"
        );

        // The members an update cannot carry are the ones the configuration keeps: its name,
        // its own tags and the instant it was created.
        assert_eq!(
            after.get("name").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: expected the stored name {name:?} in {after}"
        );
        assert_eq!(
            after.get("tags"),
            Some(&json!({"owner": "platform"})),
            "case {case:?}: expected the resource's own tags to survive the update in {after}"
        );
        assert_eq!(
            after.get("dateCreated"),
            Some(&created_at),
            "case {case:?}: expected dateCreated to stay {created_at} in {after}"
        );
        // The update carried no description, and an update "replaces the stored
        // configuration", so the one the create named is cleared.
        assert_eq!(
            after.get("description"),
            None,
            "case {case:?}: expected the omitted description to be cleared in {after}"
        );
        // This update IS the configuration's last write, so its instant is reported.
        let updated_at = after.get("dateUpdated").and_then(Value::as_str);
        assert!(
            updated_at.is_some_and(|stamp| {
                stamp.len() >= 20 && stamp.contains('T') && stamp.ends_with('Z')
            }),
            "case {case:?}: expected dateUpdated to be an ISO 8601 UTC instant, got \
             {updated_at:?}"
        );
    }
}

/// RULE: an update carries every setting it wants kept, so a description it names is stored
/// and a following update that omits it clears it again.
#[tokio::test]
async fn an_update_carries_every_setting_it_keeps() {
    let router = emulator();
    let arn = stored(&router, "described").await;

    let described = update_body(
        &arn,
        "update-described",
        json!([entry("eu-west-1", None)]),
        json!({ "description": "the configuration as updated" }),
    );
    let (status, answer) = update_in(&router, REGION, &described).await;
    assert_eq!(
        status, 200,
        "case \"an update naming a description\": got {status} with {answer}"
    );
    let (_, read) = get_in(&router, REGION, &arn).await;
    let after = configuration("an update naming a description", &read);
    assert_eq!(
        after.get("description").and_then(Value::as_str),
        Some("the configuration as updated"),
        "case \"an update naming a description\": expected the description the update named \
         in {after}"
    );

    let cleared = update_body(
        &arn,
        "update-cleared",
        json!([entry("eu-west-1", None)]),
        json!({}),
    );
    let (status, answer) = update_in(&router, REGION, &cleared).await;
    assert_eq!(
        status, 200,
        "case \"an update omitting the description\": got {status} with {answer}"
    );
    let (_, read) = get_in(&router, REGION, &arn).await;
    let after = configuration("an update omitting the description", &read);
    assert_eq!(
        after.get("description"),
        None,
        "case \"an update omitting the description\": expected it cleared in {after}"
    );
}

/// RULE: one `clientToken` answers for ONE update: a retry of the same request answers the
/// original response and writes nothing again, and the same token carrying different settings
/// is refused as the modelled mismatch.
#[tokio::test]
async fn one_client_token_updates_at_most_once() {
    let router = emulator();
    let arn = stored(&router, "idempotent").await;

    let first_list = json!([
        entry(REGION, Some("golden-{{ imagebuilder:buildDate }}")),
        entry("eu-central-1", None),
    ]);
    let body = update_body(&arn, "token-reused", first_list.clone(), json!({}));
    let (status, first) = update_in(&router, REGION, &body).await;
    assert_eq!(status, 200, "case the first update: got {status} with {first}");

    let (status, retry) = update_in(&router, REGION, &body).await;
    assert_eq!(status, 200, "case the retry: got {status} with {retry}");
    assert_eq!(
        retry, first,
        "case the retry: expected the original response, got {retry} after {first}"
    );

    // Each row changes ONE parameter of the request above while keeping its token.
    let rows: Vec<(&str, Value)> = vec![
        (
            "one target Region fewer",
            update_body(
                &arn,
                "token-reused",
                json!([entry(REGION, Some("golden-{{ imagebuilder:buildDate }}"))]),
                json!({}),
            ),
        ),
        (
            "the target Regions in another order",
            update_body(
                &arn,
                "token-reused",
                json!([
                    entry("eu-central-1", None),
                    entry(REGION, Some("golden-{{ imagebuilder:buildDate }}")),
                ]),
                json!({}),
            ),
        ),
        (
            "a description the first request did not carry",
            update_body(
                &arn,
                "token-reused",
                first_list.clone(),
                json!({ "description": "something else" }),
            ),
        ),
        (
            "another configuration entirely",
            update_body(
                &arn.replace("idempotent", "other"),
                "token-reused",
                first_list.clone(),
                json!({}),
            ),
        ),
    ];

    for (case, body) in rows {
        let (status, answer) = update_in(&router, REGION, &body).await;
        assert_refused(
            case,
            status,
            &answer,
            400,
            "IdempotentParameterMismatchException",
        );
        let expected = "The request parameters do not match those in the original idempotent \
                        request.";
        let actual = error_message(case, &answer);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );
    }

    // Nothing above wrote again: the list the first request stored is the one a read reports.
    let (_, read) = get_in(&router, REGION, &arn).await;
    let after = configuration("a read after the retries", &read);
    assert_eq!(
        after.get("distributions"),
        Some(&first_list),
        "case \"a read after the retries\": expected the first update's list in {after}"
    );
}

/// RULE: an ARN this Region holds no configuration under names nothing, so the update is
/// reported MISSING and nothing is written — including an ARN of another Region, which no
/// endpoint of this one serves.
#[tokio::test]
async fn an_arn_naming_no_configuration_is_reported_missing() {
    let router = emulator();
    let present = stored(&router, "present").await;

    // (case, the ARN the update named)
    let rows: Vec<(&str, String)> = vec![
        ("a name nothing created", arn_of(REGION, "absent")),
        (
            "a name that differs from a stored one",
            arn_of(REGION, "present-2"),
        ),
        (
            "a configuration of another Region",
            arn_of(OTHER_REGION, "present"),
        ),
    ];

    for (index, (case, arn)) in rows.into_iter().enumerate() {
        let body = update_body(
            &arn,
            &format!("token-missing-{index}"),
            json!([entry(REGION, None)]),
            json!({}),
        );
        let (status, answer) = update_in(&router, REGION, &body).await;
        assert_refused(case, status, &answer, 404, "ResourceNotFoundException");
        let expected =
            format!("The following required resource 'DistributionConfiguration' cannot be found: '{arn}'.");
        let actual = error_message(case, &answer);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );
        let (status, read) = get_in(&router, REGION, &arn).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the refused update to have stored nothing, got {status} \
             with {read}"
        );
    }

    // The configuration that DOES exist was not touched by any of them.
    let (_, read) = get_in(&router, REGION, &present).await;
    let after = configuration("the stored configuration", &read);
    assert_eq!(
        after.get("distributions"),
        Some(&created_distributions()),
        "case \"the stored configuration\": expected the created list in {after}"
    );
    assert_eq!(
        after.get("dateUpdated"),
        None,
        "case \"the stored configuration\": expected no dateUpdated in {after}"
    );
}

/// RULE: the replacement list names each target Region at most once — "a Region can appear at
/// most once in the list" — and a list that names one twice is refused, leaving the stored
/// list as it was.
#[tokio::test]
async fn a_replacement_names_each_target_region_at_most_once() {
    // (case, the replacement list, the Region a refusal names)
    let rows: Vec<(&str, Value, &str)> = vec![
        (
            "the same Region twice, adjacent",
            json!([entry(REGION, None), entry(REGION, None)]),
            REGION,
        ),
        (
            "the same Region twice, with another between them",
            json!([
                entry("eu-west-1", None),
                entry("us-east-1", None),
                entry("eu-west-1", None),
            ]),
            "eu-west-1",
        ),
        (
            "the same Region twice, carrying different settings",
            json!([
                entry("ap-south-1", Some("one-{{ imagebuilder:buildDate }}")),
                entry("ap-south-1", Some("two-{{ imagebuilder:buildDate }}")),
            ]),
            "ap-south-1",
        ),
    ];

    for (index, (case, replacement, duplicate)) in rows.into_iter().enumerate() {
        let router = emulator();
        let name = format!("duplicate-target-{index}");
        let arn = stored(&router, &name).await;

        let body = update_body(&arn, &format!("token-dup-{index}"), replacement, json!({}));
        let (status, answer) = update_in(&router, REGION, &body).await;
        assert_refused(
            case,
            status,
            &answer,
            400,
            "InvalidParameterCombinationException",
        );
        let expected = format!(
            "The distributions list can name each target Region at most once, and it names \
             {duplicate} more than once."
        );
        let actual = error_message(case, &answer);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the message {expected:?}, got {actual:?}"
        );

        let (_, read) = get_in(&router, REGION, &arn).await;
        let after = configuration(case, &read);
        assert_eq!(
            after.get("distributions"),
            Some(&created_distributions()),
            "case {case:?}: expected the refused update to have changed nothing, got {after}"
        );
    }
}
