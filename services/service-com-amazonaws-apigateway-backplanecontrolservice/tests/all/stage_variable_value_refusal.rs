// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal a stage variable whose VALUE lies outside the set a
//! value is spelled from earns, and for the KEY that refusal names.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `GetStage` / PATCH helpers are this crate's
//! existing harness, reused from `rest_api_lifecycle`, `stage_lifecycle` and
//! `patch_updates`.
//!
//! The RULE under test, asserted over a TABLE of inputs rather than one recorded call:
//! API Gateway refuses a stage variable whose value lies outside the set it stores a value
//! from — the empty string included — with `BadRequestException` (HTTP 400) whose message
//! names the MAP KEY the rejected value was set under and then the characters a value may
//! be spelled from, and stores nothing of the refused call. The key is the only part of the
//! message that comes from the request; a refusal of the variable's NAME is a different
//! class with its own sentence, which the sibling `control_plane_fidelity_rules` suite
//! tables beside this one.
//!
//! Rows differ along what the rule generalises over: the FORM of the rejected value (empty,
//! a space, a quote, a percent escape, non-ASCII), the spelling and length of the key it
//! was set under, how many well-formed variables ride beside it, and WHICH call carried it
//! — `CreateStage`, the stage `CreateDeployment` creates, a `/variables/{name}` patch, and a
//! canary's `stageVariableOverrides` on either create.

use serde_json::{json, Value};

use crate::patch_updates::patch;
use crate::rest_api_lifecycle::{assert_failure, create_api, Answer, Gateway};
use crate::stage_lifecycle::{create_deployment, get_stage};

/// The sentence a stage variable whose VALUE is outside the set the service stores a value
/// from is refused with: API Gateway reports the VALUE separately from the name, naming the
/// map KEY the rejected value was set under and then the characters a value may be spelled
/// from.
///
/// Written out here rather than imported from the implementation — a test that asserts the
/// handler's own constant passes for whatever wording the handler happens to carry, so the
/// FIXED text is pinned literally (the double space after the period and the unbalanced
/// `-'` are API Gateway's own and are served as written) and only the variable part, the
/// key, is taken from the request the row sent.
///
/// `pub(crate)` so the sibling `control_plane_fidelity_rules` suite, which tables the two
/// halves of the member against each other, asserts the same one template.
pub(crate) fn invalid_stage_variable_value(key: &str) -> String {
    format!(
        "Invalid stage variable value of key {key}.  Please use values with alphanumeric \
         characters and the symbols ' ', -', '.', '_', ':', '/', '?', '&', '=', and ','."
    )
}

/// The sentence `GetStage` answers for a stage name that resolves to no stage, written out
/// here for the reason [`invalid_stage_variable_value`] gives: it is what proves a refused
/// create left no partial stage behind.
const NO_SUCH_STAGE: &str = "Invalid stage identifier specified";

/// One rejected value and the key it rides under: the case name, the whole `variables` map
/// the call carries, and the key whose value the refusal must name.
type Row<'a> = (&'a str, Value, &'a str);

/// The values outside the set, each under a key of its own, with well-formed variables
/// riding beside some of them — so the key in the message can only have come from the
/// request's own map.
fn rejected_rows() -> Vec<Row<'static>> {
    vec![
        (
            "an empty value, the only variable",
            json!({ "emptyVar": "" }),
            "emptyVar",
        ),
        (
            "an empty value under a key spelled with underscores and digits",
            json!({ "lambda_arn_2": "" }),
            "lambda_arn_2",
        ),
        (
            "an empty value under a single-character key",
            json!({ "s": "" }),
            "s",
        ),
        (
            "an empty value among three well-formed variables",
            json!({ "table": "pets", "region": "us-west-2", "blank": "", "stage": "prod" }),
            "blank",
        ),
        (
            "a space in the value",
            json!({ "greeting": "hello world" }),
            "greeting",
        ),
        (
            "a quote in the value",
            json!({ "table": "pets", "quoted": "it's" }),
            "quoted",
        ),
        (
            "a percent escape in the value",
            json!({ "escaped": "a%20b" }),
            "escaped",
        ),
        (
            "a non-ASCII value",
            json!({ "city": "münchen" }),
            "city",
        ),
    ]
}

/// `CreateStage` for `stage_name` over `deployment`, with `extra` merged into the request.
async fn create_stage(
    gateway: &Gateway,
    api: &str,
    stage_name: &str,
    deployment: &str,
    extra: Value,
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

/// A fresh API carrying one deployment every row stages over.
async fn api_with_a_deployment(gateway: &Gateway, name: &str) -> (String, String) {
    let (api, _root) = create_api(gateway, name).await;
    let deployment = create_deployment(
        gateway,
        &api,
        &format!("CreateDeployment({name})"),
        json!({ "description": "the snapshot every stage serves" }),
    )
    .await;
    (api, deployment)
}

#[tokio::test]
async fn a_rejected_stage_variable_value_is_refused_by_the_key_it_was_set_under() {
    let gateway = Gateway::new();
    let (api, deployment) = api_with_a_deployment(&gateway, "variable-value-refusal").await;

    for (index, (case, variables, key)) in rejected_rows().into_iter().enumerate() {
        let stage = format!("v{index}");
        let answer = create_stage(
            &gateway,
            &api,
            &stage,
            &deployment,
            json!({ "variables": variables }),
        )
        .await;
        assert_failure(&answer, 400, "BadRequestException", case);
        let expected = invalid_stage_variable_value(key);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.body["message"]
        );

        // The refused create stored nothing: the stage it named resolves to nothing, with
        // the sentence a stage identifier that names no stage earns.
        let read = get_stage(&gateway, &api, &stage).await;
        assert_failure(
            &read,
            404,
            "NotFoundException",
            &format!("{case} / GetStage after the refusal"),
        );
        assert_eq!(
            read.body["message"].as_str(),
            Some(NO_SUCH_STAGE),
            "case {case:?} / GetStage after the refusal: expected {NO_SUCH_STAGE:?}, got {:?}",
            read.body["message"]
        );
    }
}

#[tokio::test]
async fn every_call_that_sets_a_stage_variable_names_the_rejected_key_the_same_way() {
    let gateway = Gateway::new();
    // One rejected value, carried by each call that can set a stage variable: the message
    // is the key's, not one operation's.
    let key = "emptyVar";
    let expected = invalid_stage_variable_value(key);

    let (api, deployment) = api_with_a_deployment(&gateway, "variable-value-paths").await;
    // A stage that already exists, for the patch path to address.
    let live = create_stage(&gateway, &api, "live", &deployment, json!({})).await;
    assert_eq!(
        live.status, 201,
        "case CreateStage(live): expected 201, got {} with {:?}",
        live.status, live.body
    );

    let staged = create_stage(
        &gateway,
        &api,
        "by_create",
        &deployment,
        json!({ "variables": { key: "" } }),
    )
    .await;
    let canaried = create_stage(
        &gateway,
        &api,
        "by_canary",
        &deployment,
        json!({
            "canarySettings": {
                "percentTraffic": 10.0,
                "useStageCache": false,
                "stageVariableOverrides": { key: "" },
            },
        }),
    )
    .await;
    let deployed = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": "by_deployment", "variables": { key: "" } })),
        )
        .await;
    let deployed_canary = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({
                "stageName": "by_deployment_canary",
                "canarySettings": {
                    "percentTraffic": 10.0,
                    "useStageCache": false,
                    "stageVariableOverrides": { key: "" },
                },
            })),
        )
        .await;
    let patched = patch(
        &gateway,
        &format!("/restapis/{api}/stages/live"),
        vec![json!({ "op": "replace", "path": format!("/variables/{key}"), "value": "" })],
    )
    .await;

    for (case, answer) in [
        ("CreateStage variables", &staged),
        ("CreateStage canarySettings.stageVariableOverrides", &canaried),
        ("CreateDeployment variables", &deployed),
        (
            "CreateDeployment canarySettings.stageVariableOverrides",
            &deployed_canary,
        ),
        ("UpdateStage /variables/{name}", &patched),
    ] {
        assert_failure(answer, 400, "BadRequestException", case);
        assert_eq!(
            answer.body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {:?}",
            answer.body["message"]
        );
    }

    // None of the refused calls wrote anything: neither the stages they named nor a
    // variable on the stage the patch addressed.
    for stage in ["by_create", "by_canary", "by_deployment", "by_deployment_canary"] {
        assert_failure(
            &get_stage(&gateway, &api, stage).await,
            404,
            "NotFoundException",
            &format!("GetStage({stage}) after the refusal"),
        );
    }
    let read = get_stage(&gateway, &api, "live").await;
    assert_eq!(
        read.status, 200,
        "case GetStage(live): expected 200, got {} with {:?}",
        read.status, read.body
    );
    assert_eq!(
        read.body["variables"][key],
        Value::Null,
        "case GetStage(live): a refused patch wrote {:?}",
        read.body["variables"]
    );
}

#[tokio::test]
async fn a_value_inside_the_set_is_stored_and_a_never_set_key_is_absent() {
    let gateway = Gateway::new();
    let (api, deployment) = api_with_a_deployment(&gateway, "variable-value-control").await;

    // The positive control of the same rule: a value inside the set the member publishes —
    // "the values must match [A-Za-z0-9-._~:/?#&=,]+" (`smithy_query(query="doc",
    // shape="CreateStageRequest", member="variables")`) — is stored under its key and read
    // back, and a key no request ever set has no entry at all.
    let rows: Vec<(&str, &str, &str)> = vec![
        ("a plain value", "presentVar", "hello"),
        ("every punctuation mark the value set admits", "opaque", "v1-._~:/?#&=,9"),
        (
            "an ARN as the value",
            "lambdaArn",
            "arn:aws:lambda:us-east-1:000000000000:function:f",
        ),
        ("a single-character value", "s", "a"),
    ];

    for (index, (case, key, value)) in rows.iter().enumerate() {
        let stage = format!("ok{index}");
        let answer = create_stage(
            &gateway,
            &api,
            &stage,
            &deployment,
            json!({ "variables": { *key: *value } }),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {case:?}: expected 201 inside the published set, got {} with {:?}",
            answer.status, answer.body
        );
        let read = get_stage(&gateway, &api, &stage).await;
        assert_eq!(
            read.body["variables"][*key].as_str(),
            Some(*value),
            "case {case:?}: GetStage read back variables {:?}",
            read.body["variables"]
        );
        assert_eq!(
            read.body["variables"]["neverSet"],
            Value::Null,
            "case {case:?}: a key no request set has an entry in {:?}",
            read.body["variables"]
        );
    }
}
