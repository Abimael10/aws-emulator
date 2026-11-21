// SPDX-License-Identifier: CC0-1.0

//! What a deployed stage serves when the api has TWO releases behind one stage — the stage's
//! own deployment and the CANARY beside it — and what a non-proxy `HTTP` integration makes of
//! a real body on its way to a backend and back.
//!
//! Two rules, both read off the members that state them:
//!
//! * `CanarySettings` — `percentTraffic` is "the percent (0-100) of traffic diverted to a
//!   canary deployment", `deploymentId` "the ID of the canary deployment" and
//!   `stageVariableOverrides` "the stage variable overrides used for the canary release
//!   deployment", whose values "override any values defined in the deployment stage". So the
//!   share decides, per request, which of the two deployments answers it and which variable
//!   map its templates render against, while `Stage$deploymentId` keeps naming the base
//!   deployment until a `copy` of `/canarySettings/deploymentId` promotes the canary ("to
//!   promote a canary deployment, you copy the canary deployment ID to the affiliated
//!   deployment ID", `UpdateStage`);
//! * `Integration$type` `HTTP` is "for integrating an API method with an HTTP backend", and
//!   such an integration TRANSFORMS both halves of the call: `requestTemplates` render the
//!   body the backend is called with, and the `IntegrationResponse$responseTemplates` of the
//!   response whose `selectionPattern` matches render the body the caller is served. So a
//!   document the client posted reaches the backend in the shape the request template gave it
//!   and comes back in the shape the response template gives the backend's answer — and where
//!   `passthroughBehavior` is `NEVER`, a content type no request template keys is refused
//!   `415` with the backend never called at all.
//!
//! The templates of the second rule are read as the language defines them, which includes the
//! methods of the VALUE a reference reached: the values a template names are Java objects, so a
//! call written after one — `$input.path('$.name').length()`, `$name.toUpperCase()` — is a
//! method of that value's own type, and the rendering applies it.
//!
//! Every case drives the real wire path — an HTTP request through the multi-service router
//! onto the api's own `{rest-api-id}.execute-api.{region}.amazonaws.com` host — so what is
//! asserted is what a client of the published invoke URL sees. The backend of the second rule
//! is a `MOCK` method of another api of this emulator, which is the one backend whose answer
//! is observable in process.

use bytes::Bytes;
use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    deploy, integration_response, invoke, method_response, put_integration, put_method, Invoked,
};
use crate::rest_api_lifecycle::{create_api, resource_under, string_member, Gateway};
use crate::stage_edge_admission::invoke_bytes;

/// The region every invoke host below is addressed in.
const REGION: &str = "us-east-1";

/// The stage the front api of every case is deployed to.
const STAGE: &str = "live";

/// The share of traffic the two published bounds of `CanarySettings$percentTraffic` name.
const ALL_TRAFFIC: f64 = 100.0;
const NO_TRAFFIC: f64 = 0.0;

/// A share between the two bounds, for the case that asserts the diversion is taken per
/// request rather than once for the stage.
const HALF_THE_TRAFFIC: f64 = 50.0;

/// How many requests that case issues: enough that one release answering all of them is not
/// something a correct split produces, since each request is placed by its own draw.
const SPLIT_REQUESTS: usize = 64;

/// The api's own invoke host.
fn api_host(api: &str) -> String {
    format!("{api}.execute-api.{REGION}.amazonaws.com")
}

/// `CreateDeployment` with no stage of its own, as its deployment id.
async fn deployment_of(gateway: &Gateway, api: &str) -> String {
    let answer = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "description": "the base snapshot" })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "case CreateDeployment({api}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", "CreateDeployment")
}

/// `CreateStage` for [`STAGE`] over `deployment`, carrying `variables`.
async fn create_stage(gateway: &Gateway, api: &str, deployment: &str, variables: Value) {
    let answer = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/stages"),
            Some(json!({
                "stageName": STAGE,
                "deploymentId": deployment,
                "variables": variables,
            })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "case CreateStage({STAGE}): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
}

/// `CreateDeployment` to the EXISTING [`STAGE`] carrying `canarySettings`, which deploys to
/// that stage's canary, as the new deployment's id.
async fn canary_deployment(gateway: &Gateway, api: &str, canary: Value) -> String {
    let answer = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE, "canarySettings": canary })),
        )
        .await;
    assert_eq!(
        answer.status, 201,
        "case CreateDeployment(canary): expected 201, got {} with {:?}",
        answer.status, answer.body
    );
    string_member(&answer.body, "id", "CreateDeployment(canary)")
}

/// `UpdateStage` of [`STAGE`] with `patches`, asserting the modelled 200.
async fn patch_stage(gateway: &Gateway, api: &str, case: &str, patches: Value) -> Value {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({ "patchOperations": patches })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from UpdateStage, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// `GetStage` of [`STAGE`].
async fn get_stage(gateway: &Gateway, api: &str) -> Value {
    let answer = gateway
        .call("GET", &format!("/restapis/{api}/stages/{STAGE}"), None)
        .await;
    assert_eq!(
        answer.status, 200,
        "case GetStage({STAGE}): expected 200, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// The response template of the `MOCK` method both releases of the canary case serve: the
/// snapshot it was deployed with, and the stage variable the release is named by — read
/// through `$util.escapeJavaScript`, which "escapes the characters in a string using
/// JavaScript string rules".
fn release_template(snapshot: &str) -> String {
    format!(
        r#"{{"snapshot":"{snapshot}","release":"$util.escapeJavaScript($stageVariables.release)"}}"#
    )
}

/// The body [`release_template`] renders for `snapshot` and `release`.
fn released_body(snapshot: &str, release: &str) -> String {
    format!(r#"{{"snapshot":"{snapshot}","release":"{release}"}}"#)
}

/// `PutIntegrationResponse` for the 200 of `resource`'s `GET`, carrying `template` as its
/// `application/json` response template — called again between deployments, so the two
/// snapshots of the canary case differ in the api's own configuration and not only in the
/// variables.
async fn put_release_template(gateway: &Gateway, api: &str, resource: &str, template: &str) {
    integration_response(
        gateway,
        api,
        resource,
        "GET",
        "200",
        json!({ "responseTemplates": { "application/json": template } }),
    )
    .await;
}

/// A `MOCK` `GET` on a child of the root that answers 200 with [`release_template`], as the
/// resource id.
async fn release_method(gateway: &Gateway, api: &str, root: &str, snapshot: &str) -> String {
    let resource = resource_under(gateway, api, root, "release").await;
    put_method(
        gateway,
        api,
        &resource,
        "GET",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        api,
        &resource,
        "GET",
        json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
        }),
    )
    .await;
    method_response(gateway, api, &resource, "GET", "200", json!({})).await;
    put_release_template(gateway, api, &resource, &release_template(snapshot)).await;
    resource
}

/// One invoke of the deployed release method.
async fn invoke_release(gateway: &Gateway, api: &str) -> Invoked {
    invoke(gateway, api, "GET", &format!("/{STAGE}/release"), &[]).await
}

/// Assert the stage answered `expected`, naming the case.
fn assert_released(case: &str, served: &Invoked, expected: &str) {
    assert_eq!(
        served.status, 200,
        "case {case:?}: expected 200 from the deployed stage, got {} with {:?}",
        served.status, served.body
    );
    assert_eq!(
        served.body, expected,
        "case {case:?}: expected the release {expected:?}, got {:?}",
        served.body
    );
}

#[tokio::test]
async fn a_canarys_share_of_the_traffic_decides_which_release_a_request_is_served_from() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "canary-release").await;
    let resource = release_method(&gateway, &api, &root, "v1").await;
    let base = deployment_of(&gateway, &api).await;
    create_stage(&gateway, &api, &base, json!({ "release": "base" })).await;

    assert_released(
        "a stage with no canary at all",
        &invoke_release(&gateway, &api).await,
        &released_body("v1", "base"),
    );

    // The canary is a SECOND snapshot: the response template is changed before it is
    // deployed, so the two releases differ in the api's own configuration as well as in the
    // variables the canary overrides.
    put_release_template(&gateway, &api, &resource, &release_template("v2")).await;
    let canary = canary_deployment(
        &gateway,
        &api,
        json!({
            "percentTraffic": ALL_TRAFFIC,
            "stageVariableOverrides": { "release": "canary" },
            "useStageCache": false,
        }),
    )
    .await;
    assert_ne!(
        canary, base,
        "case the canary deployment: expected a deployment of its own, got the base one \
         ({base})"
    );
    // "Specifies the deployment this stage points at" (`Stage$deploymentId`): deploying to the
    // canary leaves it naming the BASE deployment, and the canary's own pointer names the new
    // one.
    let stage = get_stage(&gateway, &api).await;
    assert_eq!(
        string_member(&stage, "deploymentId", "GetStage after the canary deployment"),
        base,
        "case the canary deployment: expected the stage to keep serving the base deployment, \
         got {stage:?}"
    );
    assert_eq!(
        stage["canarySettings"]["deploymentId"].as_str(),
        Some(canary.as_str()),
        "case the canary deployment: expected the canary to point at the new deployment, got \
         {stage:?}"
    );

    // Rows differ along the share the canary takes — each published bound of
    // `percentTraffic` — and, after the promotion, along which deployment the STAGE itself
    // serves: the whole rule is which release answers a request.
    assert_released(
        "a canary taking all of the traffic",
        &invoke_release(&gateway, &api).await,
        // The canary's own snapshot, with the variable it overrides.
        &released_body("v2", "canary"),
    );

    patch_stage(
        &gateway,
        &api,
        "the canary's share is set to none",
        json!([{
            "op": "replace",
            "path": "/canarySettings/percentTraffic",
            "value": NO_TRAFFIC.to_string(),
        }]),
    )
    .await;
    assert_released(
        "a canary taking none of the traffic",
        &invoke_release(&gateway, &api).await,
        &released_body("v1", "base"),
    );

    // The promotion: "you copy the canary deployment ID to the affiliated deployment ID".
    let promoted = patch_stage(
        &gateway,
        &api,
        "the canary is promoted",
        json!([{ "op": "copy", "from": "/canarySettings/deploymentId", "path": "/deploymentId" }]),
    )
    .await;
    assert_eq!(
        string_member(&promoted, "deploymentId", "UpdateStage promoting the canary"),
        canary,
        "case the canary is promoted: expected the stage to serve the canary's deployment, got \
         {promoted:?}"
    );
    assert_released(
        "the promoted canary is the stage's own release",
        &invoke_release(&gateway, &api).await,
        // The canary's snapshot, now served as the BASE release — so with the stage's own
        // variables, because the canary takes none of the traffic any more.
        &released_body("v2", "base"),
    );
}

#[tokio::test]
async fn a_partial_share_places_each_request_on_its_own_draw() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "canary-split").await;
    let resource = release_method(&gateway, &api, &root, "v1").await;
    let base = deployment_of(&gateway, &api).await;
    create_stage(&gateway, &api, &base, json!({ "release": "base" })).await;
    put_release_template(&gateway, &api, &resource, &release_template("v2")).await;
    canary_deployment(
        &gateway,
        &api,
        json!({
            "percentTraffic": HALF_THE_TRAFFIC,
            "stageVariableOverrides": { "release": "canary" },
            "useStageCache": false,
        }),
    )
    .await;

    let mut served = std::collections::BTreeSet::new();
    for _ in 0..SPLIT_REQUESTS {
        let answered = invoke_release(&gateway, &api).await;
        assert_eq!(
            answered.status, 200,
            "case a half share: expected 200 from the deployed stage, got {} with {:?}",
            answered.status, answered.body
        );
        served.insert(answered.body);
    }
    let expected: std::collections::BTreeSet<String> = [
        released_body("v1", "base"),
        released_body("v2", "canary"),
    ]
    .into_iter()
    .collect();
    assert_eq!(
        served, expected,
        "case a half share: expected {SPLIT_REQUESTS} requests to reach both releases, got \
         {served:?}"
    );
}

/// The `MOCK` backend api of the transformation case: its request template carries the
/// document the front api sent it into the integration's own answer — which is what a `MOCK`
/// answers ("the integration request a MOCK answers with is its request template rendered
/// against this call") — and its response template reads that document back out, so what the
/// backend SAW is in the body it answers. As the api id.
async fn echoing_backend(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "vtl-backend").await;
    let resource = resource_under(gateway, &api, &root, "back").await;
    put_method(
        gateway,
        &api,
        &resource,
        "POST",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "POST",
        json!({
            "type": "MOCK",
            "requestTemplates": {
                "application/json": r#"{"statusCode": 200, "echo": $input.json('$.payload')}"#,
            },
        }),
    )
    .await;
    method_response(gateway, &api, &resource, "POST", "200", json!({})).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "POST",
        "200",
        json!({
            "responseTemplates": {
                "application/json": r#"{"backend":"seen","echo":$input.json('$.echo')}"#,
            },
        }),
    )
    .await;
    deploy(gateway, &api, "back").await;
    api
}

/// The front api of the transformation case: one `POST` whose non-proxy `HTTP` integration
/// calls `backend`'s deployed stage with the body its request template renders, and answers
/// the body its response template makes of what the backend sent back. As the api id.
async fn transforming_front(gateway: &Gateway, backend: &str, passthrough: &str) -> String {
    let (api, root) = create_api(gateway, &format!("vtl-front-{passthrough}")).await;
    let resource = resource_under(gateway, &api, &root, "front").await;
    put_method(
        gateway,
        &api,
        &resource,
        "POST",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "POST",
        json!({
            "type": "HTTP",
            "httpMethod": "POST",
            "uri": format!("https://{}/back/back", api_host(backend)),
            "requestTemplates": {
                "application/json": r#"{"payload": $input.json('$.order')}"#,
            },
            "requestParameters": {
                "integration.request.header.Content-Type": "'application/json'",
            },
            "passthroughBehavior": passthrough,
        }),
    )
    .await;
    method_response(gateway, &api, &resource, "POST", "200", json!({})).await;
    integration_response(
        gateway,
        &api,
        &resource,
        "POST",
        "200",
        json!({
            "selectionPattern": "",
            "responseTemplates": {
                "application/json": r#"{"front":"mapped","echo":$input.json('$.echo')}"#,
            },
        }),
    )
    .await;
    deploy(gateway, &api, STAGE).await;
    api
}

#[tokio::test]
async fn a_non_proxy_http_integration_transforms_the_body_in_both_directions() {
    let gateway = Gateway::new();
    let backend = echoing_backend(&gateway).await;
    let front = transforming_front(&gateway, &backend, "WHEN_NO_MATCH").await;

    // Rows differ along the DOCUMENT posted, since the rule is that the request template's
    // rendering is what the backend is called with and the response template's rendering is
    // what the caller is served: an object, a scalar, an array, and a member the template's
    // path does not select at all.
    let cases: [(&str, Value, &str); 5] = [
        ("an object", json!({ "order": { "id": 7 } }), r#"{"id":7}"#),
        ("a number", json!({ "order": 7 }), "7"),
        ("a string", json!({ "order": "seven" }), r#""seven""#),
        (
            "an array",
            json!({ "order": [1, "two"] }),
            r#"[1,"two"]"#,
        ),
        // The path selects nothing, so the template renders nothing in its place: the
        // document the backend is called with carries an empty value there, and the answer
        // reports exactly that.
        ("no order member at all", json!({ "other": 1 }), ""),
    ];
    for (case, posted, echoed) in cases {
        let answered = invoke_bytes(
            &gateway,
            &front,
            "POST",
            &format!("/{STAGE}/front"),
            &[("content-type", "application/json")],
            Bytes::from(posted.to_string()),
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the backend's mapped 200, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.text(),
            format!(r#"{{"front":"mapped","echo":{echoed}}}"#),
            "case {case:?}: expected the response template's rendering of what the backend \
             saw, got {:?}",
            answered.text()
        );
    }
}

#[tokio::test]
async fn a_content_type_no_request_template_keys_is_refused_where_passthrough_is_never() {
    let gateway = Gateway::new();
    let backend = echoing_backend(&gateway).await;
    // `Integration$passthroughBehavior`: `NEVER` "rejects the method request with an HTTP 415
    // Unsupported Media Type response" when the content type matches none of the
    // `requestTemplates` keys, while `WHEN_NO_MATCH` "passes the method request body through
    // the integration request without transformation".
    let strict = transforming_front(&gateway, &backend, "NEVER").await;
    let lenient = transforming_front(&gateway, &backend, "WHEN_NO_MATCH").await;

    // Rows differ along the content type sent and the behaviour declared: the mapped type
    // under both, and an unmapped one under each.
    let cases: [(&str, &str, &str, u16); 4] = [
        ("the mapped type under NEVER", &strict, "application/json", 200),
        ("an unmapped type under NEVER", &strict, "text/plain", 415),
        (
            "the mapped type under WHEN_NO_MATCH",
            &lenient,
            "application/json",
            200,
        ),
        (
            "an unmapped type under WHEN_NO_MATCH",
            &lenient,
            "text/plain",
            200,
        ),
    ];
    for (case, api, content_type, expected) in cases {
        let answered = invoke_bytes(
            &gateway,
            api,
            "POST",
            &format!("/{STAGE}/front"),
            &[("content-type", content_type)],
            Bytes::from(json!({ "order": { "id": 7 } }).to_string()),
        )
        .await;
        assert_eq!(
            answered.status, expected,
            "case {case:?}: expected {expected}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        if expected == 415 {
            // The refusal API Gateway answers an unmapped media type with, through the
            // `UNSUPPORTED_MEDIA_TYPE` gateway response, and nothing of the backend in it.
            assert_eq!(
                answered.text(),
                r#"{"message":"Unsupported Media Type"}"#,
                "case {case:?}: expected the UNSUPPORTED_MEDIA_TYPE body with the backend \
                 never called, got {:?}",
                answered.text()
            );
        }
    }
}

/// The path the branching backend of the case below serves, and the one its front api serves.
const BRANCH_PATH: &str = "branch";
const CHOOSE_PATH: &str = "choose";

/// The name the backend's `#if` admits, and one it does not: the condition is over the LENGTH
/// and the text the front api rewrote the client's document into, so the two rows differ in
/// what the client posted and nothing else.
const ADMITTED_NAME: &str = "ok";
const REFUSED_NAME: &str = "other";

/// A `MOCK` backend whose REQUEST template chooses the integration status with `#set`, `#if`
/// and `#else` over what the caller sent — the form a mapping template branches in — and whose
/// two integration responses answer each branch. As its api id.
///
/// The `#if` is over `$seen` and `$len`, the two members the front api's request template
/// rewrites the client's document into, so the branch the backend takes is decided by the
/// rewriting and not by the client's own body.
async fn branching_backend(gateway: &Gateway) -> String {
    let (api, root) = create_api(gateway, "vtl-branch-backend").await;
    let resource = resource_under(gateway, &api, &root, BRANCH_PATH).await;
    put_method(
        gateway,
        &api,
        &resource,
        "POST",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "POST",
        json!({
            "type": "MOCK",
            "requestTemplates": {
                "application/json": format!(
                    "#set($seen = $input.path('$.seen'))\n\
                     #set($len = $input.path('$.len'))\n\
                     #if($seen == \"{ADMITTED_NAME}\" && $len == {admitted_len})\n\
                     {{\"statusCode\":200,\"seen\":\"$seen\"}}\n\
                     #else\n\
                     {{\"statusCode\":404}}\n\
                     #end",
                    admitted_len = ADMITTED_NAME.len(),
                ),
            },
        }),
    )
    .await;
    for (status, pattern, template) in [
        (
            "200",
            "",
            r#"{"from":"backend","seen":"$input.path('$.seen')"}"#,
        ),
        ("404", "404", r#"{"from":"backend","rejected":true}"#),
    ] {
        method_response(gateway, &api, &resource, "POST", status, json!({})).await;
        integration_response(
            gateway,
            &api,
            &resource,
            "POST",
            status,
            json!({
                "selectionPattern": pattern,
                "responseTemplates": { "application/json": template },
            }),
        )
        .await;
    }
    deploy(gateway, &api, "back").await;
    api
}

/// The front api of the branching case: its non-proxy `HTTP` integration REWRITES the client's
/// document into the two members the backend branches on — through
/// `$util.escapeJavaScript($input.path(…))` and the `length()` of the value that path selected
/// — and its two integration responses shape each of the backend's statuses. As its api id.
async fn rewriting_front(gateway: &Gateway, backend: &str) -> String {
    let (api, root) = create_api(gateway, "vtl-branch-front").await;
    let resource = resource_under(gateway, &api, &root, CHOOSE_PATH).await;
    put_method(
        gateway,
        &api,
        &resource,
        "POST",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    put_integration(
        gateway,
        &api,
        &resource,
        "POST",
        json!({
            "type": "HTTP",
            "httpMethod": "POST",
            "uri": format!("https://{}/back/{BRANCH_PATH}", api_host(backend)),
            "requestTemplates": {
                "application/json":
                    "{\"seen\":\"$util.escapeJavaScript($input.path('$.name'))\",\
                     \"len\":$input.path('$.name').length()}",
            },
            "requestParameters": {
                "integration.request.header.Content-Type": "'application/json'",
            },
            "passthroughBehavior": "NEVER",
        }),
    )
    .await;
    for (status, pattern, template) in [
        (
            "200",
            "",
            // A `#set` binds the member the backend reported, and the response is shaped from
            // the methods of the value it bound plus the stage the call arrived at.
            "#set($v = $input.path('$.seen'))\n\
             {\"upper\":\"$v.toUpperCase()\",\"len\":$v.length(),\"stage\":\"$context.stage\"}",
        ),
        ("404", "404", r#"{"rejected_by_backend":true}"#),
    ] {
        method_response(gateway, &api, &resource, "POST", status, json!({})).await;
        integration_response(
            gateway,
            &api,
            &resource,
            "POST",
            status,
            json!({
                "selectionPattern": pattern,
                "responseTemplates": { "application/json": template },
            }),
        )
        .await;
    }
    deploy(gateway, &api, STAGE).await;
    api
}

#[tokio::test]
async fn a_branching_template_serves_the_status_and_body_its_condition_chooses() {
    let gateway = Gateway::new();
    let backend = branching_backend(&gateway).await;
    let front = rewriting_front(&gateway, &backend).await;

    // Rows differ along what the rule generalises over: which api is addressed (the backend
    // on its own, and the front that rewrites the document before calling it) and which
    // branch the condition then takes — so the same document answers differently through the
    // front, and the front's own response templates shape each of the backend's statuses.
    let cases: [(&str, &str, &str, &str, Value, u16, String); 4] = [
        (
            "the backend reached directly with an un-rewritten document",
            &backend,
            "back",
            BRANCH_PATH,
            json!({ "name": ADMITTED_NAME }),
            404,
            r#"{"from":"backend","rejected":true}"#.to_string(),
        ),
        (
            "the backend reached directly with the document the front would send",
            &backend,
            "back",
            BRANCH_PATH,
            json!({ "seen": ADMITTED_NAME, "len": ADMITTED_NAME.len() }),
            200,
            format!(r#"{{"from":"backend","seen":"{ADMITTED_NAME}"}}"#),
        ),
        (
            "the front rewrites the document into the branch the backend admits",
            &front,
            STAGE,
            CHOOSE_PATH,
            json!({ "name": ADMITTED_NAME }),
            200,
            format!(
                r#"{{"upper":"{upper}","len":{len},"stage":"{STAGE}"}}"#,
                upper = ADMITTED_NAME.to_uppercase(),
                len = ADMITTED_NAME.len(),
            ),
        ),
        (
            "the front rewrites a document the backend refuses",
            &front,
            STAGE,
            CHOOSE_PATH,
            json!({ "name": REFUSED_NAME }),
            404,
            r#"{"rejected_by_backend":true}"#.to_string(),
        ),
    ];
    for (case, api, stage, path, posted, status, expected) in cases {
        let answered = invoke_bytes(
            &gateway,
            api,
            "POST",
            &format!("/{stage}/{path}"),
            &[("content-type", "application/json")],
            Bytes::from(posted.to_string()),
        )
        .await;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected {status}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.text(),
            expected,
            "case {case:?}: expected the branch's own body {expected:?}, got {:?}",
            answered.text()
        );
    }
}
