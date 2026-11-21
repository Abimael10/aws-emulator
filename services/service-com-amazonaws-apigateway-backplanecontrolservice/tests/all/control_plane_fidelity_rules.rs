// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for ten control-plane rules this service holds a request to, each
//! asserted over a TABLE of inputs rather than one recorded call.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service, and for the mapping rule an invoke of
//! the deployed stage — so what is asserted is what a client sees. [`Gateway`] and the
//! `create_api` / `deploy` / `put_method` helpers are this crate's one wire-test harness,
//! reused rather than standing a second emulator up another way.
//!
//! The rules, each with the member or document it comes from:
//!
//! 1. a `responseParameters` VALUE may name any source API Gateway's parameter mapping
//!    publishes — a static value in single quotes, `integration.response.header.{name}`,
//!    `integration.response.body.{JSON-expression}`, `context.<variable>` and
//!    `stageVariables.<name>` — and the method response header it maps carries the value
//!    RESOLVED from that source when the stage serves the call;
//! 2. `PutIntegrationResponse` is a PUT keyed by `{restApiId, resourceId, httpMethod,
//!    statusCode}`: repeating it replaces the stored response whole;
//! 3. `UntagResource` is stated as the RESULT it asks for, so a key the resource does not
//!    carry succeeds and changes nothing else;
//! 4. a tag key and value are drawn from AWS's tag character class — unicode letters,
//!    digits, whitespace and `_ . : / = + - @` — and nothing else;
//! 5. `Stage$variables` distinguishes an EMPTY map from an ABSENT one, and a variable's
//!    name and value are drawn from the sets the member publishes;
//! 6. a control-plane call is served only to a caller IAM allows to perform
//!    `apigateway:<HTTP method>` on the ARN of the path it addresses, and IAM's own
//!    `AccessDeniedException` refuses every other one;
//! 7. a `deploymentId` the request CARRIES that resolves to no deployment of the api is a
//!    bad request, not a missing resource;
//! 8. the SDK-type catalog carries no cursor member, so a `position` a request sends
//!    names nothing (asserted in the `sdk_types` suite, which owns that collection);
//! 9. a `requestModels` / `responseModels` value names a `Model` of the same api;
//! 10. a PRIVATE api is deployable only once a resource policy is attached to it, and
//!     reachable only through an interface VPC endpoint.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_apigateway_backplanecontrolservice::state::invalid_model_reference;

use crate::documentation_versions::{create_version, snapshot_request};
use crate::execute_api_data_plane::{deploy, invoke, put_integration, put_method};
use crate::method_response::response_header;
use crate::rest_api_lifecycle::{
    assert_failure, create_api, create_resource, string_member, Answer, Gateway,
};
use crate::stage_invoke_identity_policy::{given_policy, given_user, Entity, USER_KEY_ID};
use crate::stage_lifecycle::{create_deployment, get_stage};
use crate::stage_request_validation::create_model;

/// The sentence a stage variable outside the sets the member publishes is refused with,
/// written out here rather than imported from the implementation: a test that asserts the
/// handler's own constant passes for whatever wording the handler happens to carry, so the
/// text is pinned literally and a change to it has to be made here too.
///
/// Source: the rule the member's own documentation states — "Variable names can have
/// alphanumeric and underscore characters, and the values must match
/// `[A-Za-z0-9-._~:/?#&=,]+`" (`smithy_query(query="doc", shape="CreateStageRequest",
/// member="variables")`).
const INVALID_STAGE_VARIABLE: &str = "Variable names can have alphanumeric and underscore \
                                     characters, and the values must match \
                                     [A-Za-z0-9-._~:/?#&=,]+";

/// The sentence a PRIVATE api's deployment is refused with while it carries no resource
/// policy, written out here for the reason [`INVALID_STAGE_VARIABLE`] gives.
const PRIVATE_API_WITHOUT_RESOURCE_POLICY: &str =
    "Private REST API doesn't have a resource policy attached to it";

/// The stage the mapping rule's api is deployed to.
const STAGE: &str = "s";

/// The resource path the mapped method sits on.
const RESOURCE_PATH: &str = "mapped";

/// `PutIntegrationResponse` for `status` on the integration of `verb`, as answered.
async fn put_integration_response(
    gateway: &Gateway,
    api: &str,
    resource: &str,
    verb: &str,
    status: &str,
    request: Value,
) -> Answer {
    gateway
        .call(
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/{verb}/integration/responses/{status}"
            ),
            Some(request),
        )
        .await
}

/// `GetIntegrationResponse` for `status` on the integration of `verb`.
async fn get_integration_response(
    gateway: &Gateway,
    api: &str,
    resource: &str,
    verb: &str,
    status: &str,
) -> Answer {
    gateway
        .call(
            "GET",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/{verb}/integration/responses/{status}"
            ),
            None,
        )
        .await
}

/// `CreateStage` for `stage_name` over `deployment`, with `extra` merged into the request.
async fn create_stage(
    gateway: &Gateway,
    api: &str,
    stage_name: &str,
    deployment: &str,
    extra: &Value,
) -> Answer {
    let mut request = json!({ "stageName": stage_name, "deploymentId": deployment });
    if let Some(members) = extra.as_object() {
        for (name, value) in members {
            request[name.as_str()] = value.clone();
        }
    }
    gateway
        .call("POST", &format!("/restapis/{api}/stages"), Some(request))
        .await
}

// ---------------------------------------------------------------------------
// 1. The sources a response-parameter mapping may name, and what each resolves to.
// ---------------------------------------------------------------------------

/// The name of the stage variable the mapping rows read.
const STAGE_VARIABLE: &str = "backend";

/// Its value on the deployed stage.
const STAGE_VARIABLE_VALUE: &str = "beta";

/// The value the `MOCK` integration's REQUEST template puts in the payload an
/// `integration.response.body` expression then reads out of it.
const BODY_VALUE: &str = "from-the-body";

#[tokio::test]
async fn a_response_parameter_mapping_names_a_documented_source_and_carries_what_it_resolves() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "response-parameter-sources").await;
    let resource = create_resource(&gateway, &api, &root, RESOURCE_PATH).await;
    let resource_id = string_member(&resource, "id", RESOURCE_PATH);
    put_method(
        &gateway,
        &api,
        &resource_id,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    // The MOCK's own request template is what its answer carries, so a mapping that reads
    // the integration's response BODY has a document to read.
    put_integration(
        &gateway,
        &api,
        &resource_id,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": {
                "application/json": format!(
                    "{{\"statusCode\":200,\"m\":\"{BODY_VALUE}\"}}"
                ),
            },
        }),
    )
    .await;

    // Rows span the SOURCES the grammar publishes: a static literal, one value of the
    // call's context (two of them, one derived from the request and one from the api), one
    // variable of the invoked stage, a JSON expression over the integration's answer, and
    // a header of that answer — which a `MOCK` carries none of, so that mapping resolves
    // nothing and the header is not sent.
    //
    // `expected` is the value the served response must carry under the header, or `None`
    // for a mapping whose source this call has no value for.
    let stage_variable_source = format!("stageVariables.{STAGE_VARIABLE}");
    let rows: Vec<(&str, &str, Option<String>)> = vec![
        ("X-Static", "'literal-value'", Some("literal-value".into())),
        ("X-Ctx-Method", "context.httpMethod", Some("GET".into())),
        ("X-Ctx-Api", "context.apiId", Some(api.clone())),
        ("X-Ctx-Stage", "context.stage", Some(STAGE.into())),
        (
            // "The request protocol, for example, HTTP/1.1" (`$context.protocol`).
            "X-Ctx-Protocol",
            "context.protocol",
            Some("HTTP/1.1".into()),
        ),
        (
            // "The request path", which carries the STAGE segment — unlike
            // `context.resourcePath`, the path of the matched resource.
            "X-Ctx-Path",
            "context.path",
            Some(format!("/{STAGE}/{RESOURCE_PATH}")),
        ),
        (
            "X-Ctx-Resource-Path",
            "context.resourcePath",
            Some(format!("/{RESOURCE_PATH}")),
        ),
        (
            "X-Ctx-Account",
            "context.accountId",
            Some(::engine::emulator_account_id().to_string()),
        ),
        (
            "X-Stage-Var",
            stage_variable_source.as_str(),
            Some(STAGE_VARIABLE_VALUE.into()),
        ),
        (
            "X-Body",
            "integration.response.body.m",
            Some(BODY_VALUE.into()),
        ),
        ("X-Backend-Header", "integration.response.header.X-Up", None),
    ];

    // Every header is DECLARED by the method response — API Gateway returns only the
    // headers the method response declares — and MAPPED by the integration response.
    let declared: Value = Value::Object(
        rows.iter()
            .map(|(name, ..)| (response_header(name), json!(true)))
            .collect(),
    );
    let mapped: Value = Value::Object(
        rows.iter()
            .map(|(name, source, _)| (response_header(name), json!(source)))
            .collect(),
    );
    crate::execute_api_data_plane::method_response(
        &gateway,
        &api,
        &resource_id,
        "GET",
        "200",
        json!({ "responseParameters": declared }),
    )
    .await;
    let written = put_integration_response(
        &gateway,
        &api,
        &resource_id,
        "GET",
        "200",
        json!({ "responseParameters": mapped.clone() }),
    )
    .await;
    assert_eq!(
        written.status, 201,
        "case PutIntegrationResponse(every documented source): expected 201, got {} with {:?}",
        written.status, written.body
    );
    // The stored record reports every mapping back, whichever source it names.
    assert_eq!(
        written.body["responseParameters"], mapped,
        "case PutIntegrationResponse: expected the mappings to be stored as written, got {:?}",
        written.body["responseParameters"]
    );

    let deployment = create_deployment(
        &gateway,
        &api,
        "CreateDeployment(mapped)",
        json!({ "description": "the snapshot the stage serves" }),
    )
    .await;
    let staged = create_stage(
        &gateway,
        &api,
        STAGE,
        &deployment,
        &json!({ "variables": { STAGE_VARIABLE: STAGE_VARIABLE_VALUE } }),
    )
    .await;
    assert_eq!(
        staged.status, 201,
        "case CreateStage({STAGE}): expected 201, got {} with {:?}",
        staged.status, staged.body
    );

    let answered = invoke(
        &gateway,
        &api,
        "GET",
        &format!("/{STAGE}/{RESOURCE_PATH}"),
        &[],
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case the invoke: expected the mapped 200, got {} with {:?}",
        answered.status, answered.body
    );
    for (name, source, expected) in &rows {
        let header = name.to_ascii_lowercase();
        assert_eq!(
            answered.header(&header).map(str::to_owned),
            *expected,
            "case {name} mapped from {source:?}: expected {expected:?}, got {:?}",
            answered.header(&header)
        );
    }

    // The same call through the addressing that names the api in the PATH rather than in
    // the host: what each source resolves to is a fact about the CALL, so the headers are
    // the same ones — `context.path` included, which is the INVOKE path and never the
    // prefix a client reaching this emulator addressed it by.
    let prefixed = crate::execute_api_data_plane::invoke_on_host(
        &gateway,
        "localhost",
        "GET",
        &format!("/_aws/execute-api/{api}/{STAGE}/{RESOURCE_PATH}"),
        &[],
    )
    .await;
    assert_eq!(
        prefixed.status, 200,
        "case the prefixed invoke: expected the mapped 200, got {} with {:?}",
        prefixed.status, prefixed.body
    );
    for (name, source, expected) in &rows {
        let header = name.to_ascii_lowercase();
        assert_eq!(
            prefixed.header(&header).map(str::to_owned),
            *expected,
            "case {name} mapped from {source:?} on the prefixed address: expected \
             {expected:?}, got {:?}",
            prefixed.header(&header)
        );
    }
}

#[tokio::test]
async fn a_response_parameter_value_outside_the_published_sources_is_refused() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "response-parameter-refusals").await;
    let resource = create_resource(&gateway, &api, &root, RESOURCE_PATH).await;
    let resource_id = string_member(&resource, "id", RESOURCE_PATH);
    put_method(
        &gateway,
        &api,
        &resource_id,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(&gateway, &api, &resource_id, "GET", json!({ "type": "MOCK" })).await;

    // Rows span the ways a value names NO source: a bare word, a quoted value missing one
    // quote, a prefix with nothing after it, a body expression carrying the `$` prefix the
    // member excludes, and a source of the REQUEST, which an integration response has no
    // reading of.
    let refused = [
        "plain-text",
        "'half-quoted",
        "context.",
        "stageVariables.",
        "integration.response.header.",
        "integration.response.body.",
        "integration.response.body.$.m",
        "method.request.header.X-Up",
    ];
    for value in refused {
        let case = format!("PutIntegrationResponse(responseParameters = {value:?})");
        let answer = put_integration_response(
            &gateway,
            &api,
            &resource_id,
            "GET",
            "200",
            json!({ "responseParameters": { response_header("X-Mapped"): value } }),
        )
        .await;
        assert_failure(&answer, 400, "BadRequestException", &case);
        // Nothing was stored: the status code still names no integration response.
        assert_failure(
            &get_integration_response(&gateway, &api, &resource_id, "GET", "200").await,
            404,
            "NotFoundException",
            &format!("{case} / GetIntegrationResponse after the refusal"),
        );
    }
}

// ---------------------------------------------------------------------------
// 2. PutIntegrationResponse is a keyed PUT.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn put_integration_response_replaces_the_response_its_status_code_names() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "integration-response-upsert").await;
    let resource = create_resource(&gateway, &api, &root, RESOURCE_PATH).await;
    let resource_id = string_member(&resource, "id", RESOURCE_PATH);
    put_method(
        &gateway,
        &api,
        &resource_id,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(&gateway, &api, &resource_id, "GET", json!({ "type": "MOCK" })).await;
    crate::execute_api_data_plane::method_response(
        &gateway,
        &api,
        &resource_id,
        "GET",
        "200",
        json!({ "responseParameters": { response_header("X-Mapped"): true } }),
    )
    .await;

    // Rows are written one after another for the SAME status code: each is a complete
    // description of the response, so the stored record is the LAST one whole — a member
    // an earlier write set and a later one does not is gone, not merged.
    let writes: Vec<(&str, Value, Value)> = vec![
        (
            "the first write",
            json!({ "responseTemplates": { "application/json": "{\"v\":1}" } }),
            json!({
                "statusCode": "200",
                "responseTemplates": { "application/json": "{\"v\":1}" },
            }),
        ),
        (
            "a re-apply of the same configuration",
            json!({ "responseTemplates": { "application/json": "{\"v\":1}" } }),
            json!({
                "statusCode": "200",
                "responseTemplates": { "application/json": "{\"v\":1}" },
            }),
        ),
        (
            "another template and a selection pattern",
            json!({
                "selectionPattern": "2\\d\\d",
                "responseTemplates": { "application/json": "{\"v\":2}" },
            }),
            json!({
                "statusCode": "200",
                "selectionPattern": "2\\d\\d",
                "responseTemplates": { "application/json": "{\"v\":2}" },
            }),
        ),
        (
            "a write carrying a header mapping and no template",
            json!({
                "responseParameters": { response_header("X-Mapped"): "'mapped'" },
                "contentHandling": "CONVERT_TO_TEXT",
            }),
            json!({
                "statusCode": "200",
                "responseParameters": { response_header("X-Mapped"): "'mapped'" },
                "contentHandling": "CONVERT_TO_TEXT",
            }),
        ),
        (
            "an empty write, which carries nothing at all",
            json!({}),
            json!({ "statusCode": "200" }),
        ),
    ];
    for (case, request, expected) in &writes {
        let answer = put_integration_response(
            &gateway,
            &api,
            &resource_id,
            "GET",
            "200",
            request.clone(),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected the modelled 201 from a keyed PUT, got {} with {:?}",
            answer.status, answer.body
        );
        assert_eq!(
            &answer.body, expected,
            "case {case}: expected the written response {expected:?}, got {:?}",
            answer.body
        );
        let read = get_integration_response(&gateway, &api, &resource_id, "GET", "200").await;
        assert_eq!(
            &read.body, expected,
            "case {case}: GetIntegrationResponse must report the last write, got {:?}",
            read.body
        );
    }

    // Another status code is another response: a PUT keyed by one leaves the other alone.
    let other = put_integration_response(
        &gateway,
        &api,
        &resource_id,
        "GET",
        "500",
        json!({ "selectionPattern": "5\\d\\d" }),
    )
    .await;
    assert_eq!(
        other.status, 201,
        "case PutIntegrationResponse(500): expected 201, got {} with {:?}",
        other.status, other.body
    );
    let read = get_integration_response(&gateway, &api, &resource_id, "GET", "200").await;
    assert_eq!(
        read.body,
        json!({ "statusCode": "200" }),
        "case GetIntegrationResponse(200) after writing 500: expected the 200 response to \
         stand, got {:?}",
        read.body
    );
}

// ---------------------------------------------------------------------------
// 3. UntagResource names a RESULT.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn untagging_a_key_the_resource_does_not_carry_succeeds_and_changes_nothing() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "untag-idempotence").await;
    let arn = crate::tag_reads_by_arn::apigateway_arn(&format!("/restapis/{api}"));
    let tagged = gateway
        .call(
            "PUT",
            &format!("/tags/{}", crate::tag_reads_by_arn::encode_label(&arn)),
            Some(json!({ "tags": { "team": "api", "env": "test" } })),
        )
        .await;
    assert_eq!(
        tagged.status, 204,
        "case TagResource: expected 204, got {} with {:?}",
        tagged.status, tagged.body
    );

    // Rows differ along WHY the key names no stored tag — never written, written and
    // already removed, one of several, the empty key, a key AWS reserves and could never
    // have stored — and along how many keys one request names. Each row's `left` is the
    // collection the resource must carry afterwards.
    let rows: Vec<(&str, Vec<&str>, Value)> = vec![
        (
            "a key the resource never carried",
            vec!["absent"],
            json!({ "team": "api", "env": "test" }),
        ),
        (
            "two keys it never carried",
            vec!["absent", "also-absent"],
            json!({ "team": "api", "env": "test" }),
        ),
        ("the empty key", vec![""], json!({ "team": "api", "env": "test" })),
        (
            "a reserved key, which can never be stored",
            vec!["aws:region"],
            json!({ "team": "api", "env": "test" }),
        ),
        (
            "an unknown key beside a stored one",
            vec!["env", "absent"],
            json!({ "team": "api" }),
        ),
        (
            "the same key again, now that it is gone",
            vec!["env"],
            json!({ "team": "api" }),
        ),
    ];
    for (case, keys, expected) in &rows {
        let query = keys
            .iter()
            .map(|key| {
                format!(
                    "tagKeys={}",
                    crate::tag_reads_by_arn::encode_label(key)
                )
            })
            .collect::<Vec<String>>()
            .join("&");
        let uri = format!(
            "/tags/{}?{query}",
            crate::tag_reads_by_arn::encode_label(&arn)
        );
        let answer = gateway.call("DELETE", &uri, None).await;
        assert_eq!(
            answer.status, 204,
            "case {case}: expected the modelled 204 from UntagResource, got {} with {:?}",
            answer.status, answer.body
        );
        let read = crate::tag_reads_by_arn::get_tags(&gateway, &arn).await;
        assert_eq!(
            read.body["tags"], *expected,
            "case {case}: expected the collection {expected:?}, got {:?}",
            read.body["tags"]
        );
    }
}

// ---------------------------------------------------------------------------
// 4. The tag character class.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_tag_is_drawn_from_the_published_character_class() {
    let gateway = Gateway::new();
    let (api, _root) = create_api(&gateway, "tag-character-class").await;
    let arn = crate::tag_reads_by_arn::apigateway_arn(&format!("/restapis/{api}"));

    // Rows span the class the `tags` member publishes — "letters, numbers, spaces, and the
    // following: _ . : / = + - @" over unicode letters and digits of any script — and the
    // categories outside it: SYMBOLS (an emoji, a dingbat, a currency sign, a mathematical
    // operator), whichever side of the pair carries them.
    let rows: Vec<(&str, &str, &str, bool)> = vec![
        ("plain ASCII", "team", "api", true),
        ("every listed punctuation mark", "a_b.c:d/e=f+g-h@i", "_.:/=+-@", true),
        ("digits", "tier1", "42", true),
        ("spaces", "Cost Center", "platform team", true),
        ("letters of another script", "クラウド", "東京", true),
        ("letters with diacritics", "équipe", "münchen", true),
        ("an emoji value", "mood", "🎉", false),
        ("an emoji key", "🚀", "launch", false),
        ("a dingbat in a value", "done", "yes ✓", false),
        ("a currency sign in a value", "budget", "100€", false),
        ("a mathematical operator in a key", "a±b", "x", false),
        ("a bracket in a value", "list", "[one]", false),
        ("a comma in a value", "list", "one,two", false),
    ];
    let mut stored: BTreeMap<String, String> = BTreeMap::new();
    for (case, key, value, accepted) in &rows {
        let answer = gateway
            .call(
                "PUT",
                &format!("/tags/{}", crate::tag_reads_by_arn::encode_label(&arn)),
                Some(json!({ "tags": { *key: *value } })),
            )
            .await;
        if *accepted {
            assert_eq!(
                answer.status, 204,
                "case {case}: expected 204 inside the published class, got {} with {:?}",
                answer.status, answer.body
            );
            stored.insert((*key).to_owned(), (*value).to_owned());
        } else {
            assert_failure(&answer, 400, "BadRequestException", case);
        }
        // Whatever the answer, the resource carries exactly the accepted rows: a refused
        // write stores nothing.
        let read = crate::tag_reads_by_arn::get_tags(&gateway, &arn).await;
        let reported: BTreeMap<String, String> = read.body["tags"]
            .as_object()
            .map(|map| {
                map.iter()
                    .map(|(key, value)| {
                        (key.clone(), value.as_str().unwrap_or_default().to_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, stored,
            "case {case}: expected the stored collection {stored:?}, got {reported:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. A stage's `variables`: present-and-empty against absent, and the value set.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stages_variables_member_is_reported_as_the_request_configured_it() {
    let gateway = Gateway::new();
    let (api, _root) = crate::stage_lifecycle::api_with_a_method(&gateway, "stage-variables").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "CreateDeployment(variables)",
        json!({ "description": "the snapshot every stage serves" }),
    )
    .await;

    // Rows differ along what the request SET: no member at all, the empty map, one entry,
    // several. The expectation is what every reader of the stage must report — `Null` for
    // a stage no request ever set the member on, and the map itself for every stage that
    // HAS one, empty included.
    let rows: Vec<(&str, Value, Value)> = vec![
        ("the member omitted", json!({}), Value::Null),
        ("the empty map", json!({ "variables": {} }), json!({})),
        (
            "one variable",
            json!({ "variables": { "backend": "beta" } }),
            json!({ "backend": "beta" }),
        ),
        (
            "two variables",
            json!({ "variables": { "backend": "beta", "table": "pets" } }),
            json!({ "backend": "beta", "table": "pets" }),
        ),
    ];
    for (index, (case, extra, expected)) in rows.iter().enumerate() {
        let stage = format!("s{index}");
        let created = create_stage(&gateway, &api, &stage, &deployment, extra).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected 201 from CreateStage, got {} with {:?}",
            created.status, created.body
        );
        assert_eq!(
            created.body["variables"], *expected,
            "case {case}: CreateStage reported variables {:?}, expected {expected:?}",
            created.body["variables"]
        );
        let read = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            read.body["variables"], *expected,
            "case {case}: GetStage read back variables {:?}, expected {expected:?}",
            read.body["variables"]
        );
        // The listing reports the same record, so the distinction survives every reader.
        let listed = gateway
            .call("GET", &format!("/restapis/{api}/stages"), None)
            .await;
        let entry = listed.body["item"]
            .as_array()
            .and_then(|items| {
                items
                    .iter()
                    .find(|item| item["stageName"].as_str() == Some(stage.as_str()))
            })
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            entry["variables"], *expected,
            "case {case}: GetStages reported variables {:?}, expected {expected:?}",
            entry["variables"]
        );
    }

    // The two stages are therefore DISTINGUISHABLE: one has the member and the other does
    // not, which is what a client reads a stage to decide.
    let empty = get_stage(&gateway, &api, "s1").await;
    let absent = get_stage(&gateway, &api, "s0").await;
    assert!(
        empty.body.get("variables").is_some_and(|value| !value.is_null())
            && absent.body.get("variables").is_none_or(Value::is_null),
        "case the two stages: expected the explicitly-empty stage to report the member and \
         the one that omitted it to report none, got {:?} and {:?}",
        empty.body["variables"],
        absent.body["variables"]
    );
}

#[tokio::test]
async fn a_stage_variable_is_drawn_from_the_sets_the_member_publishes() {
    let gateway = Gateway::new();
    let (api, _root) =
        crate::stage_lifecycle::api_with_a_method(&gateway, "stage-variable-values").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "CreateDeployment(variable values)",
        json!({ "description": "the snapshot every stage serves" }),
    )
    .await;

    // The member's own documentation: "Variable names can have alphanumeric and underscore
    // characters, and the values must match [A-Za-z0-9-._~:/?#&=,]+"
    // (`smithy_query(query="doc", shape="CreateStageRequest", member="variables")`). Each
    // published pattern is a `+`, so neither half may be empty. Rows span every punctuation
    // mark the VALUE set admits, a name spelled with underscores and digits, and the ways
    // either half falls outside its set; `refused` is set where the request must earn the
    // member's own contract sentence, which is the rule the documentation states and
    // interpolates nothing from the request.
    type Row<'a> = (&'a str, &'a str, &'a str, bool);
    let rows: Vec<Row<'_>> = vec![
        ("a plain name and value", "backend", "beta", false),
        (
            "every punctuation mark the value set admits",
            "opaque",
            "v1-._~:/?#&=,9",
            false,
        ),
        ("a name with underscores and digits", "lambda_arn_2", "handler-2", false),
        (
            "an ARN as the value",
            "lambdaArn",
            "arn:aws:lambda:us-east-1:000000000000:function:f",
            false,
        ),
        ("a single-character name and value", "s", "a", false),
        ("an empty value", "emptyVar", "", true),
        ("a space in the value", "greeting", "hello world", true),
        ("a quote in the value", "quoted", "it's", true),
        ("a percent escape in the value", "escaped", "a%20b", true),
        ("a non-ASCII value", "city", "münchen", true),
        ("a hyphen in the name", "stage-name", "prod", true),
        ("a dot in the name", "stage.name", "prod", true),
        ("an empty name", "", "prod", true),
    ];
    let staged = create_stage(&gateway, &api, "patched", &deployment, &json!({})).await;
    assert_eq!(
        staged.status, 201,
        "case CreateStage(patched): expected 201, got {} with {:?}",
        staged.status, staged.body
    );
    for (index, (case, name, value, refused)) in rows.iter().enumerate() {
        let stage = format!("v{index}");
        let answer = create_stage(
            &gateway,
            &api,
            &stage,
            &deployment,
            &json!({ "variables": { *name: *value } }),
        )
        .await;
        if *refused {
            assert_failure(&answer, 400, "BadRequestException", case);
            // The member's own contract sentence, and the same one whichever half broke
            // it: nothing of the request is in it.
            assert_eq!(
                answer.body["message"].as_str(),
                Some(INVALID_STAGE_VARIABLE),
                "case {case}: expected the stage-variable contract sentence, got {:?}",
                answer.body["message"]
            );
            // A refused create stored nothing: the stage it named resolves to nothing.
            assert_failure(
                &get_stage(&gateway, &api, &stage).await,
                404,
                "NotFoundException",
                &format!("{case} / GetStage after the refusal"),
            );
        } else {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected 201 inside the published sets, got {} with {:?}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.body["variables"][*name].as_str(),
                Some(*value),
                "case {case}: expected the stored variable, got {:?}",
                answer.body["variables"]
            );
        }

        // The same value through the PATCH that sets one entry, which is held to the same
        // rule: a value a create refuses cannot be written through an update either. The
        // stage below exists, so what the answer reports is the VALUE's own class.
        let patched = gateway
            .call(
                "PATCH",
                &format!("/restapis/{api}/stages/patched"),
                Some(json!({
                    "patchOperations": [
                        { "op": "replace", "path": format!("/variables/{name}"), "value": value },
                    ],
                })),
            )
            .await;
        if *refused {
            assert_failure(
                &patched,
                400,
                "BadRequestException",
                &format!("{case} / UpdateStage"),
            );
            assert_eq!(
                patched.body["message"].as_str(),
                Some(INVALID_STAGE_VARIABLE),
                "case {case} / UpdateStage: expected the stage-variable contract sentence, \
                 got {:?}",
                patched.body["message"]
            );
        } else {
            assert_eq!(
                patched.status, 200,
                "case {case} / UpdateStage: expected 200, got {} with {:?}",
                patched.status, patched.body
            );
            assert_eq!(
                patched.body["variables"][*name].as_str(),
                Some(*value),
                "case {case} / UpdateStage: expected the patched variable, got {:?}",
                patched.body["variables"]
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 6. The caller's own IAM policies decide a control-plane call.
// ---------------------------------------------------------------------------

/// One control-plane request signed with the credential `access_key_id` names, answered as
/// [`Answer`] reports it.
///
/// `pub(crate)` for the sibling `control_plane_authorization` suite, which drives the SAME
/// gate over the table of documents, verbs and principals this rule's one row stands in for:
/// one signing helper, so the two suites cannot address two different hosts or scopes.
pub(crate) async fn call_as(
    gateway: &Gateway,
    access_key_id: &str,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "apigateway.us-east-1.amazonaws.com")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/us-east-1/apigateway/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let error = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer {
        status,
        error,
        body,
    }
}

/// An identity-based document with one statement of `effect` over `action` and `resource`.
fn statement(effect: &str, action: &str, resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": effect, "Action": action, "Resource": resource }],
    })
}

#[tokio::test]
async fn a_control_plane_call_is_served_only_where_the_callers_policies_allow_the_action() {
    let gateway = crate::rest_api_lifecycle::gateway_with_iam();
    given_user(&gateway);
    let (api, root) = create_api(&gateway, "control-plane-authorization").await;
    let resource = create_resource(&gateway, &api, &root, "pets").await;
    let resource_id = string_member(&resource, "id", "pets");
    put_method(
        &gateway,
        &api,
        &resource_id,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    let version = create_version(&gateway, &api, snapshot_request("v1", None)).await;
    assert_eq!(
        version.status, 201,
        "case CreateDocumentationVersion: expected 201, got {} with {:?}",
        version.status, version.body
    );

    // The reads the issue cites, plus one WRITE: the gate is the service's and not one
    // operation's, so the action it decides is the HTTP METHOD of the call.
    let method_uri = format!("/restapis/{api}/resources/{resource_id}/methods/GET");
    let version_uri = format!("/restapis/{api}/documentation/versions/v1");
    let calls: Vec<(&str, &str, String, Option<Value>)> = vec![
        ("GetMethod", "GET", method_uri.clone(), None),
        ("GetDocumentationVersion", "GET", version_uri.clone(), None),
        ("GetRestApi", "GET", format!("/restapis/{api}"), None),
        (
            "UpdateRestApi",
            "PATCH",
            format!("/restapis/{api}"),
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/description", "value": "patched" },
                ],
            })),
        ),
    ];

    // Rows span the document in force on the caller: none at all, every action of this
    // service, the one HTTP-method action the reads perform, that action scoped to this
    // api's own ARN, the same action scoped to ANOTHER api, an explicit deny beside a
    // grant of everything, and another service's actions only.
    let other_api = "arn:aws:apigateway:us-east-1::/restapis/zzzzzzzzzz/*".to_string();
    type Row<'a> = (&'a str, Option<Value>, bool, bool);
    let rows: Vec<Row<'_>> = vec![
        ("no policy at all", None, false, false),
        (
            "every action of this service",
            Some(statement("Allow", "apigateway:*", "*")),
            true,
            true,
        ),
        (
            "the read action alone",
            Some(statement("Allow", "apigateway:GET", "*")),
            true,
            false,
        ),
        (
            "the read action scoped to this api",
            Some(statement(
                "Allow",
                "apigateway:GET",
                // The api's own path and every path under it both BEGIN with it, which is
                // how a statement names one api's whole surface.
                &format!("arn:aws:apigateway:us-east-1::/restapis/{api}*"),
            )),
            true,
            false,
        ),
        (
            "the read action scoped to another api",
            Some(statement("Allow", "apigateway:GET", &other_api)),
            false,
            false,
        ),
        (
            "another service's actions only",
            Some(statement("Allow", "s3:GetObject", "*")),
            false,
            false,
        ),
        (
            "an explicit deny of this service",
            Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "*", "Resource": "*" },
                    { "Effect": "Deny", "Action": "apigateway:*", "Resource": "*" },
                ],
            })),
            false,
            false,
        ),
    ];

    for (case, document, reads_allowed, writes_allowed) in &rows {
        given_policy(&gateway, Entity::User, document.as_ref());
        for (operation, method, uri, body) in &calls {
            let case = format!("{case} / {operation}");
            let answer = call_as(&gateway, USER_KEY_ID, method, uri, body.clone()).await;
            let allowed = if *method == "GET" {
                *reads_allowed
            } else {
                *writes_allowed
            };
            if allowed {
                assert!(
                    answer.status < 300,
                    "case {case}: expected the call to be served, got {} with {:?}",
                    answer.status,
                    answer.body
                );
            } else {
                assert_failure(&answer, 403, "AccessDeniedException", &case);
                // The refusal says nothing about the records: it names the principal, the
                // action and the resource the request addressed.
                let message = answer.body["message"].as_str().unwrap_or_default();
                assert!(
                    message.contains("is not authorized to perform:")
                        && message.contains(&format!("apigateway:{method}")),
                    "case {case}: expected the IAM authorization sentence, got {message:?}"
                );
            }
        }
    }

    // A caller IAM holds no record for at all — the account's own published credential,
    // which every other suite signs with — is served, and so is one whose policies allow
    // the action.
    given_policy(&gateway, Entity::User, None);
    let served = gateway.call("GET", &method_uri, None).await;
    assert_eq!(
        served.status, 200,
        "case the account's own credential: expected 200 from GetMethod, got {} with {:?}",
        served.status, served.body
    );

    // An UNAUTHORIZED caller is refused whether or not the resource it named exists: the
    // decision is taken before anything is resolved, so a missing api is still the
    // authorization refusal and the answer leaks nothing about what this account holds.
    for (case, uri) in [
        ("an api this account does not hold", "/restapis/zzzzzzzzzz"),
        (
            "a method of an api this account does not hold",
            "/restapis/zzzzzzzzzz/resources/yyyyyyyyyy/methods/GET",
        ),
    ] {
        let answer = call_as(&gateway, USER_KEY_ID, "GET", uri, None).await;
        assert_failure(&answer, 403, "AccessDeniedException", case);
    }
}

// ---------------------------------------------------------------------------
// 7. A deploymentId the request CARRIES is a value, not an identifier.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_deployment_id_that_names_no_deployment_of_the_api_is_a_bad_request() {
    let gateway = Gateway::new();
    let (api, _root) = crate::stage_lifecycle::api_with_a_method(&gateway, "stage-deployment").await;
    let deployment = create_deployment(
        &gateway,
        &api,
        "CreateDeployment(value class)",
        json!({ "description": "the one deployment this api holds" }),
    )
    .await;
    // Another api's deployment: a record that EXISTS and is still not this api's.
    let (other, _other_root) =
        crate::stage_lifecycle::api_with_a_method(&gateway, "stage-deployment-other").await;
    let foreign = create_deployment(
        &gateway,
        &other,
        "CreateDeployment(another api)",
        json!({ "description": "another api's deployment" }),
    )
    .await;

    // Rows differ along WHY the id names no deployment of this api — nothing ever minted
    // it, another api holds it, it is the empty string — and along which member carried
    // it, the stage's own pointer or its canary's.
    let values: Vec<(&str, String)> = vec![
        ("an id nothing minted", "zzzzzzzzzz".to_string()),
        ("another api's deployment", foreign.clone()),
        ("the empty string", String::new()),
    ];
    for (index, (case, value)) in values.iter().enumerate() {
        for member in ["the stage's own pointer", "the canary's"] {
            let stage = format!("d{index}");
            let request = if member == "the canary's" {
                json!({
                    "stageName": stage,
                    "deploymentId": deployment,
                    "canarySettings": { "percentTraffic": 5.0, "deploymentId": value },
                })
            } else {
                json!({ "stageName": stage, "deploymentId": value })
            };
            let case = format!("CreateStage({member} = {case})");
            let answer = gateway
                .call("POST", &format!("/restapis/{api}/stages"), Some(request))
                .await;
            assert_failure(&answer, 400, "BadRequestException", &case);
            assert_failure(
                &get_stage(&gateway, &api, &stage).await,
                404,
                "NotFoundException",
                &format!("{case} / GetStage after the refusal"),
            );
        }
    }

    // The identifiers in the URI keep the not-found class, which is the class the value
    // class is distinct from.
    let answer = gateway
        .call(
            "POST",
            "/restapis/zzzzzzzzzz/stages",
            Some(json!({ "stageName": "prod", "deploymentId": deployment })),
        )
        .await;
    assert_failure(
        &answer,
        404,
        "NotFoundException",
        "CreateStage(an api this account does not hold)",
    );
}

// ---------------------------------------------------------------------------
// 9. A model reference names a model of the same api.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_model_reference_names_a_model_of_the_same_api() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "model-references").await;
    create_model(&gateway, &api, "Pet", json!({ "type": "object" })).await;
    let (other, other_root) = create_api(&gateway, "model-references-other").await;
    create_model(&gateway, &other, "Owner", json!({ "type": "object" })).await;
    let resource = create_resource(&gateway, &api, &root, "pets").await;
    let resource_id = string_member(&resource, "id", "pets");
    let other_resource = create_resource(&gateway, &other, &other_root, "owners").await;
    let other_resource_id = string_member(&other_resource, "id", "owners");

    // Rows span the model names a reference can carry: one this api holds, one the service
    // creates every api with (`Empty`, `Error`), one ANOTHER api holds, one nothing holds,
    // and a published name in the wrong case.
    let rows: Vec<(&str, &str, bool)> = vec![
        ("a model this api holds", "Pet", true),
        ("the api's own default model", "Empty", true),
        ("the api's own error model", "Error", true),
        ("a model only another api holds", "Owner", false),
        ("a name nothing holds", "NoSuchModelXYZ123", false),
        ("a published name in the wrong case", "pet", false),
    ];
    for (index, (case, model, accepted)) in rows.iter().enumerate() {
        let verb = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD"][index];
        put_method(
            &gateway,
            &api,
            &resource_id,
            verb,
            json!({ "authorizationType": "NONE" }),
        )
        .await;

        // The reference rides on a method response's `responseModels`…
        let response = gateway
            .call(
                "PUT",
                &format!("/restapis/{api}/resources/{resource_id}/methods/{verb}/responses/200"),
                Some(json!({ "responseModels": { "application/json": model } })),
            )
            .await;
        let case_response = format!("PutMethodResponse({case})");
        if *accepted {
            assert_eq!(
                response.status, 201,
                "case {case_response}: expected 201, got {} with {:?}",
                response.status, response.body
            );
            assert_eq!(
                response.body["responseModels"]["application/json"].as_str(),
                Some(*model),
                "case {case_response}: expected the stored reference, got {:?}",
                response.body["responseModels"]
            );
        } else {
            assert_failure(&response, 400, "BadRequestException", &case_response);
            assert_eq!(
                response.body["message"].as_str(),
                Some(invalid_model_reference(model).as_str()),
                "case {case_response}: expected the unresolved-model sentence, got {:?}",
                response.body["message"]
            );
            // Nothing was stored: the status code names no method response.
            assert_failure(
                &gateway
                    .call(
                        "GET",
                        &format!(
                            "/restapis/{api}/resources/{resource_id}/methods/{verb}/responses/200"
                        ),
                        None,
                    )
                    .await,
                404,
                "NotFoundException",
                &format!("{case_response} / GetMethodResponse after the refusal"),
            );
        }

        // …and on a method's own `requestModels`, under the same rule.
        let other_verb = ["OPTIONS", "ANY"][index % 2];
        let method = gateway
            .call(
                "PUT",
                &format!(
                    "/restapis/{api}/resources/{resource_id}/methods/{other_verb}"
                ),
                Some(json!({
                    "authorizationType": "NONE",
                    "requestModels": { "application/json": model },
                })),
            )
            .await;
        let case_method = format!("PutMethod({case})");
        if *accepted {
            assert_eq!(
                method.status, 201,
                "case {case_method}: expected 201, got {} with {:?}",
                method.status, method.body
            );
            // Removed again, so the next row's verb is free.
            let deleted = gateway
                .call(
                    "DELETE",
                    &format!("/restapis/{api}/resources/{resource_id}/methods/{other_verb}"),
                    None,
                )
                .await;
            assert_eq!(
                deleted.status, 204,
                "case {case_method}: expected 204 from DeleteMethod, got {} with {:?}",
                deleted.status, deleted.body
            );
        } else {
            assert_failure(&method, 400, "BadRequestException", &case_method);
        }
    }

    // The patch that sets one entry is held to the same rule, so a reference a create
    // refuses cannot be written through an update either.
    put_method(
        &gateway,
        &other,
        &other_resource_id,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    let seeded = gateway
        .call(
            "PUT",
            &format!(
                "/restapis/{other}/resources/{other_resource_id}/methods/GET/responses/200"
            ),
            Some(json!({ "responseModels": { "application/json": "Owner" } })),
        )
        .await;
    assert_eq!(
        seeded.status, 201,
        "case PutMethodResponse(the other api): expected 201, got {} with {:?}",
        seeded.status, seeded.body
    );
    for (case, model, expected) in [
        ("a model that api holds", "Empty", 201),
        ("a model only the first api holds", "Pet", 400),
        ("a name nothing holds", "NoSuchModelXYZ123", 400),
    ] {
        let patched = gateway
            .call(
                "PATCH",
                &format!(
                    "/restapis/{other}/resources/{other_resource_id}/methods/GET/responses/200"
                ),
                Some(json!({
                    "patchOperations": [{
                        "op": "replace",
                        "path": "/responseModels/application~1json",
                        "value": model,
                    }],
                })),
            )
            .await;
        assert_eq!(
            patched.status, expected,
            "case UpdateMethodResponse({case}): expected {expected}, got {} with {:?}",
            patched.status, patched.body
        );
    }
}

// ---------------------------------------------------------------------------
// 10. A PRIVATE api is deployable only with a resource policy.
// ---------------------------------------------------------------------------

/// A document admitting `execute-api:Invoke` from anywhere.
fn invoke_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": "*",
            "Action": "execute-api:Invoke",
            "Resource": "execute-api:/*",
        }],
    })
    .to_string()
}

/// `CreateRestApi` with `endpointConfiguration.types` = `types`, plus `policy` when one is
/// given; answers `(api, root)`.
async fn api_of_type(
    gateway: &Gateway,
    name: &str,
    types: &[&str],
    policy: Option<String>,
) -> (String, String) {
    let mut request = json!({ "name": name, "endpointConfiguration": { "types": types } });
    if let Some(policy) = policy {
        request["policy"] = json!(policy);
    }
    let created = gateway.call("POST", "/restapis", Some(request)).await;
    assert_eq!(
        created.status, 201,
        "case CreateRestApi({name}): expected 201, got {} with {:?}",
        created.status, created.body
    );
    (
        string_member(&created.body, "id", name),
        string_member(&created.body, "rootResourceId", name),
    )
}

#[tokio::test]
async fn a_private_api_is_reachable_only_through_an_interface_vpc_endpoint() {
    let gateway = Gateway::new();
    // The endpoint a request arrives through, in EC2's own id form, and the DNS names it
    // publishes: the regional one and the zonal one EC2 reports in an interface endpoint's
    // `DnsEntries`.
    const ENDPOINT: &str = "vpce-0123456789abcdef0";
    let region = "us-east-1";
    let body = "{\"m\":\"served\"}";

    let (private, private_root) = api_of_type(
        &gateway,
        "private-reachability",
        &["PRIVATE"],
        Some(invoke_policy()),
    )
    .await;
    let (public, public_root) =
        api_of_type(&gateway, "public-reachability", &["REGIONAL"], None).await;
    for (api, root) in [(&private, &private_root), (&public, &public_root)] {
        crate::execute_api_data_plane::mock_resource(
            &gateway,
            api,
            root,
            "whoami",
            "GET",
            "200",
            crate::execute_api_data_plane::json_template(body),
        )
        .await;
        deploy(&gateway, api, STAGE).await;
    }

    // Rows differ along the ADDRESS the request arrives at and the endpoint type of the
    // api it names: a PRIVATE api has no public endpoint at all, so only an address that
    // names an interface VPC endpoint reaches it; an api that is not private is reached on
    // its own `execute-api` host as before.
    type Row<'a> = (&'a str, String, &'a str, Vec<(&'a str, &'a str)>, u16);
    let rows: Vec<Row<'_>> = vec![
        (
            "a PRIVATE api on its own public execute-api host",
            format!("{private}.execute-api.{region}.amazonaws.com"),
            &private,
            Vec::new(),
            403,
        ),
        (
            "a PRIVATE api on the regional name of an interface endpoint",
            format!("{ENDPOINT}.execute-api.{region}.vpce.amazonaws.com"),
            &private,
            vec![("x-apigw-api-id", private.as_str())],
            200,
        ),
        (
            "a PRIVATE api on the zonal name of the same endpoint",
            format!("{ENDPOINT}-{region}a.execute-api.{region}.vpce.amazonaws.com"),
            &private,
            vec![("x-apigw-api-id", private.as_str())],
            200,
        ),
        (
            "an api that is not private, on its own public host",
            format!("{public}.execute-api.{region}.amazonaws.com"),
            &public,
            Vec::new(),
            200,
        ),
    ];
    for (case, host, api, headers, expected) in &rows {
        let answered = crate::execute_api_data_plane::invoke_on_host(
            &gateway,
            host,
            "GET",
            &format!("/{STAGE}/whoami"),
            headers,
        )
        .await;
        assert_eq!(
            answered.status, *expected,
            "case {case}: expected {expected}, got {} with {:?}",
            answered.status, answered.body
        );
        if *expected == 200 {
            assert_eq!(
                answered.body, body,
                "case {case}: expected the integration's own answer, got {:?}",
                answered.body
            );
        } else {
            // A refused request carries nothing of the api: the endpoint's own refusal.
            assert!(
                !answered.body.contains("served"),
                "case {case}: a refused request must carry nothing the integration \
                 answers, got {:?}",
                answered.body
            );
            let _ = api;
        }
    }
}

#[tokio::test]
async fn a_private_api_is_deployable_only_once_a_resource_policy_is_attached() {
    let gateway = Gateway::new();

    // Rows differ along the api's endpoint types and whether a document is attached at
    // create time: only a PRIVATE api with no document is refused, and the endpoint type
    // is read off the stored configuration, so a list naming PRIVATE beside another type
    // is private too.
    type Row<'a> = (&'a str, Vec<&'a str>, bool, bool);
    let rows: Vec<Row<'_>> = vec![
        ("a PRIVATE api with no policy", vec!["PRIVATE"], false, false),
        ("a PRIVATE api carrying a policy", vec!["PRIVATE"], true, true),
        (
            "an api whose types name PRIVATE beside REGIONAL, with no policy",
            vec!["REGIONAL", "PRIVATE"],
            false,
            false,
        ),
        ("a REGIONAL api with no policy", vec!["REGIONAL"], false, true),
        ("an EDGE api with no policy", vec!["EDGE"], false, true),
        ("an api created with no endpoint types", Vec::new(), false, true),
    ];
    for (index, (case, types, with_policy, deployable)) in rows.iter().enumerate() {
        let name = format!("private-deploy-{index}");
        let (api, root) = if types.is_empty() {
            let (api, root) = create_api(&gateway, &name).await;
            (api, root)
        } else {
            api_of_type(
                &gateway,
                &name,
                types,
                with_policy.then(invoke_policy),
            )
            .await
        };
        // A deployment is the snapshot of the api's methods, so the api carries one.
        crate::execute_api_data_plane::mock_method(
            &gateway,
            &api,
            &root,
            "GET",
            "200",
            crate::execute_api_data_plane::json_template("{\"m\":\"served\"}"),
        )
        .await;

        let answer = gateway
            .call(
                "POST",
                &format!("/restapis/{api}/deployments"),
                Some(json!({ "stageName": "s" })),
            )
            .await;
        if *deployable {
            assert_eq!(
                answer.status, 201,
                "case {case}: expected 201 from CreateDeployment, got {} with {:?}",
                answer.status, answer.body
            );
            continue;
        }
        assert_failure(&answer, 400, "BadRequestException", case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(PRIVATE_API_WITHOUT_RESOURCE_POLICY),
            "case {case}: expected the private-deployment sentence, got {:?}",
            answer.body["message"]
        );
        // Nothing was deployed: the api holds no deployment and no stage. A listing reports
        // its page member whatever the page holds, so what says nothing was deployed is a
        // page of no elements (`crate::deployment_collection`).
        let deployments = gateway
            .call("GET", &format!("/restapis/{api}/deployments"), None)
            .await;
        assert_eq!(
            deployments.body.get("item"),
            Some(&json!([])),
            "case {case}: expected no deployment to have been created, got {:?}",
            deployments.body
        );
        assert_failure(
            &get_stage(&gateway, &api, "s").await,
            404,
            "NotFoundException",
            &format!("{case} / GetStage after the refusal"),
        );

        // Attaching a document makes the SAME call succeed, which is the other half of
        // the rule.
        let patched = gateway
            .call(
                "PATCH",
                &format!("/restapis/{api}"),
                Some(json!({
                    "patchOperations": [
                        { "op": "replace", "path": "/policy", "value": invoke_policy() },
                    ],
                })),
            )
            .await;
        assert_eq!(
            patched.status, 200,
            "case {case}: expected 200 from UpdateRestApi, got {} with {:?}",
            patched.status, patched.body
        );
        let deployed = gateway
            .call(
                "POST",
                &format!("/restapis/{api}/deployments"),
                Some(json!({ "stageName": "s" })),
            )
            .await;
        assert_eq!(
            deployed.status, 201,
            "case {case} / once a policy is attached: expected 201, got {} with {:?}",
            deployed.status, deployed.body
        );

        // And taking the document away closes the gate again: the next deployment of the
        // same api is refused.
        let removed = gateway
            .call(
                "PATCH",
                &format!("/restapis/{api}"),
                Some(json!({
                    "patchOperations": [{ "op": "remove", "path": "/policy" }],
                })),
            )
            .await;
        assert_eq!(
            removed.status, 200,
            "case {case}: expected 200 from UpdateRestApi(remove), got {} with {:?}",
            removed.status, removed.body
        );
        assert_failure(
            &gateway
                .call(
                    "POST",
                    &format!("/restapis/{api}/deployments"),
                    Some(json!({ "stageName": "s2" })),
                )
                .await,
            400,
            "BadRequestException",
            &format!("{case} / once the policy is removed again"),
        );
    }
}
