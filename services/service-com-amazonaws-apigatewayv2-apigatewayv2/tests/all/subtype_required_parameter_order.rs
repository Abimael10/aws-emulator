// SPDX-License-Identifier: CC0-1.0

//! The ORDER the refusal of an under-configured first-class subtype integration lists the
//! subtype's required parameters in.
//!
//! RULE: when a write (`CreateIntegration`, `UpdateIntegration`) would leave an `AWS_PROXY`
//! integration naming a first-class `IntegrationSubtype` without every parameter that
//! subtype's action requires, the `BadRequestException` (httpError 400,
//! `smithy_query(query="operation",operation="CreateIntegration")`) names the subtype's WHOLE
//! required set in the order AWS publishes FOR THAT SUBTYPE — a per-subtype order that is
//! neither alphabetical nor derivable from the request, so the list is never sorted, never
//! reversed and never follows which parameters the mapping happened to define.
//!
//! The rows span what that order is a fact about — the SUBTYPE (one, two, three and four
//! required parameters; two actions of one service published the opposite way round; four
//! subtypes whose published order is not the alphabetical one) — and, for each of them, WHICH
//! parameters the mapping leaves out and in which order it carries the ones it keeps, because
//! the listed order may not depend on either. The case the issue was filed about,
//! `AppConfig-GetConfiguration` with an empty mapping, is ONE row.

use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, error_code};

/// Every first-class subtype's required parameters, in the order API Gateway publishes for
/// that subtype in its AWS-service integration reference for HTTP APIs — the order its own
/// refusal reproduces.
///
/// Written out here rather than read off the service's catalog, so what is asserted is AWS's
/// order and not merely the order this emulator happens to hold. Note `SQS-SendMessage` and
/// `SQS-DeleteMessage`, two actions of ONE service whose two parameters are published the
/// opposite way round, and `AppConfig-GetConfiguration`, `Kinesis-PutRecord` and
/// `SQS-DeleteMessage`, none of which is published alphabetically.
const PUBLISHED_ORDER: &[(&str, &[&str])] = &[
    (
        "AppConfig-GetConfiguration",
        &["Application", "Environment", "Configuration", "ClientId"],
    ),
    ("EventBridge-PutEvents", &["Detail", "DetailType", "Source"]),
    ("Kinesis-PutRecord", &["StreamName", "Data", "PartitionKey"]),
    ("SQS-DeleteMessage", &["ReceiptHandle", "QueueUrl"]),
    ("SQS-SendMessage", &["QueueUrl", "MessageBody"]),
    ("SQS-PurgeQueue", &["QueueUrl"]),
    ("SQS-ReceiveMessage", &["QueueUrl"]),
    ("StepFunctions-StartExecution", &["StateMachineArn"]),
    ("StepFunctions-StartSyncExecution", &["StateMachineArn"]),
    ("StepFunctions-StopExecution", &["ExecutionArn"]),
];

/// A parameter no catalogued action requires, carried by the incomplete mappings below so a
/// refusal is about the missing required parameter and not about a mapping that defines
/// nothing at all.
const OPTIONAL_PARAMETER: (&str, &str) = ("Region", "eu-west-1");

/// The value a mapping defines a parameter with: any non-blank value defines it.
const DEFINED: &str = "$request.body.value";

/// A `RequestParameters` map built from `(parameter, value)` pairs.
fn mapping(entries: &[(&str, &str)]) -> Value {
    let object: Map<String, Value> = entries
        .iter()
        .map(|(parameter, value)| ((*parameter).to_string(), json!(*value)))
        .collect();
    Value::Object(object)
}

/// The sentence API Gateway refuses such a write with: fixed text, with the subtype the
/// request named and the bracketed list of that subtype's required parameters.
fn refusal_sentence(subtype: &str, parameters: &[&str]) -> String {
    format!(
        "Operation: {subtype} requires enabling passthrough, or defining all of the following \
         parameters in the parameter mapping: [{}]",
        parameters.join(", ")
    )
}

/// Create one HTTP api and answer its `ApiId`.
async fn http_api(emulator: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = call(
        emulator,
        "POST",
        "/v2/apis",
        Some(json!({ "name": name, "protocolType": "HTTP" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Send one `CreateIntegration` for an `AWS_PROXY` integration naming `subtype`.
async fn create_subtype_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    subtype: &str,
    request_parameters: Value,
) -> (u16, Value) {
    call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationSubtype": subtype,
            "payloadFormatVersion": "1.0",
            "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
            "requestParameters": request_parameters,
        })),
    )
    .await
}

/// Send one `UpdateIntegration`.
async fn patch_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    integration_id: &str,
    patch: Value,
) -> (u16, Value) {
    call(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        Some(patch),
    )
    .await
}

/// Read one integration back through `GetIntegration`.
async fn stored_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    integration_id: &str,
) -> Value {
    let (status, body) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetIntegration, got {status} {body}"
    );
    body
}

/// How many integrations the api holds, through `GetIntegrations`.
async fn integration_count(emulator: &MultiServiceRouter, api_id: &str) -> usize {
    let (status, body) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetIntegrations, got {status} {body}"
    );
    body["items"].as_array().map(Vec::len).unwrap_or_default()
}

/// Assert one refused write: the status, the modelled error and the exact sentence.
fn assert_refused(case: &str, expected: &str, status: u16, body: &Value) {
    assert_eq!(
        status, 400,
        "case {case:?}: expected 400 from the write, got {status} {body}"
    );
    assert_eq!(
        error_code(body),
        "BadRequestException",
        "case {case:?}: expected BadRequestException, got {body}"
    );
    assert_eq!(
        as_str(body, "message"),
        expected,
        "case {case:?}: expected the refusal {expected:?}, got {body}"
    );
}

/// Assert the message is the published order and NOT an order a reader could mistake for it:
/// the alphabetical one, or the reverse of the published one.
fn assert_not_reordered(case: &str, subtype: &str, published: &[&str], message: &str) {
    let mut sorted: Vec<&str> = published.to_vec();
    sorted.sort_unstable();
    let mut reversed: Vec<&str> = published.to_vec();
    reversed.reverse();
    for other in [sorted, reversed] {
        if other == published {
            continue;
        }
        let rendered = refusal_sentence(subtype, &other);
        assert_ne!(
            message, rendered,
            "case {case:?}: expected the published order {published:?}, got the list ordered \
             {other:?}"
        );
    }
}

/// The mappings that leave a subtype under-configured: nothing mapped at all, every single
/// required parameter left out in turn (with the kept ones carried in the REVERSE of the
/// published order), and only the LAST published parameter defined — so neither which
/// parameters are missing nor how the request carried the rest can be what the listed order
/// comes from.
fn incomplete_mappings(published: &[&str]) -> Vec<(String, Value)> {
    let mut cases = vec![("nothing mapped".to_string(), json!({}))];
    for dropped in published {
        let mut entries: Vec<(&str, &str)> = published
            .iter()
            .rev()
            .filter(|parameter| *parameter != dropped)
            .map(|parameter| (*parameter, DEFINED))
            .collect();
        entries.push(OPTIONAL_PARAMETER);
        cases.push((
            format!("every parameter but {dropped}, carried in reverse"),
            mapping(&entries),
        ));
    }
    if let Some(last) = published.last() {
        if published.len() > 1 {
            cases.push((
                format!("only {last} mapped"),
                mapping(&[(*last, DEFINED), OPTIONAL_PARAMETER]),
            ));
        }
    }
    cases
}

/// RULE: `CreateIntegration` refuses an under-configured subtype integration with the
/// subtype's whole required set in that subtype's own published order.
#[tokio::test]
async fn the_create_refusal_lists_the_published_parameter_order() {
    let emulator = emulator();

    // The sentence pinned literally for the four-parameter subtype, so its fixed text and its
    // published order are asserted against AWS and not merely agreed with the handler.
    assert_eq!(
        refusal_sentence(
            "AppConfig-GetConfiguration",
            &["Application", "Environment", "Configuration", "ClientId"],
        ),
        "Operation: AppConfig-GetConfiguration requires enabling passthrough, or defining all \
         of the following parameters in the parameter mapping: [Application, Environment, \
         Configuration, ClientId]",
        "the refusal must be worded as API Gateway words it"
    );

    for (index, (subtype, published)) in PUBLISHED_ORDER.iter().enumerate() {
        let expected = refusal_sentence(subtype, published);
        // A refused create stores nothing, so one api serves every row of this subtype.
        let api_id = http_api(&emulator, subtype, &format!("subtype-order-{index}")).await;
        for (label, request_parameters) in incomplete_mappings(published) {
            let case = format!("{subtype}: {label}");
            let (status, body) =
                create_subtype_integration(&emulator, &api_id, subtype, request_parameters).await;
            assert_refused(&case, &expected, status, &body);
            assert_not_reordered(&case, subtype, published, &as_str(&body, "message"));
        }
        assert_eq!(
            integration_count(&emulator, &api_id).await,
            0,
            "case {subtype:?}: a refused CreateIntegration must store no integration"
        );

        // The other half of the same gate: the mapping that defines every published parameter
        // is created, so the rows above are about the required set and not about the subtype.
        let complete: Vec<(&str, &str)> = published
            .iter()
            .map(|parameter| (*parameter, DEFINED))
            .collect();
        let (status, created) =
            create_subtype_integration(&emulator, &api_id, subtype, mapping(&complete)).await;
        assert_eq!(
            status, 201,
            "case {subtype:?}: a complete mapping must be created, got {status} {created}"
        );
    }
}

/// RULE: the same order is stated by `UpdateIntegration`, for a patch that drops a required
/// parameter and for one that takes a subtype ON, and the refused patch changes nothing.
#[tokio::test]
async fn the_update_refusal_lists_the_published_parameter_order() {
    let emulator = emulator();
    let api_id = http_api(&emulator, "the patched api", "subtype-order-patched").await;

    for (subtype, published) in PUBLISHED_ORDER {
        let expected = refusal_sentence(subtype, published);
        let complete: Vec<(&str, &str)> = published
            .iter()
            .map(|parameter| (*parameter, DEFINED))
            .collect();
        let (status, created) =
            create_subtype_integration(&emulator, &api_id, subtype, mapping(&complete)).await;
        assert_eq!(
            status, 201,
            "case {subtype:?}: expected 201 from CreateIntegration, got {status} {created}"
        );
        let integration_id = as_str(&created, "integrationId");
        let before = stored_integration(&emulator, &api_id, &integration_id).await;

        for (label, request_parameters) in incomplete_mappings(published) {
            let case = format!("{subtype} patched to: {label}");
            let (status, body) = patch_integration(
                &emulator,
                &api_id,
                &integration_id,
                json!({ "requestParameters": request_parameters }),
            )
            .await;
            assert_refused(&case, &expected, status, &body);
            assert_not_reordered(&case, subtype, published, &as_str(&body, "message"));
            let after = stored_integration(&emulator, &api_id, &integration_id).await;
            assert_eq!(
                after, before,
                "case {case:?}: a refused UpdateIntegration must leave the record as it was"
            );
        }
    }

    // An integration that names NO subtype, patched to take one on while mapping none of its
    // arguments: the same sentence, in the same published order, for each subtype in turn.
    let (status, proxy) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:handler",
            "payloadFormatVersion": "1.0",
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateIntegration for the proxy, got {status} {proxy}"
    );
    let proxy_id = as_str(&proxy, "integrationId");
    let before = stored_integration(&emulator, &api_id, &proxy_id).await;
    for (subtype, published) in PUBLISHED_ORDER {
        let case = format!("a proxy integration patched to take on {subtype}");
        let expected = refusal_sentence(subtype, published);
        let (status, body) = patch_integration(
            &emulator,
            &api_id,
            &proxy_id,
            json!({ "integrationSubtype": subtype }),
        )
        .await;
        assert_refused(&case, &expected, status, &body);
        assert_not_reordered(&case, subtype, published, &as_str(&body, "message"));
        let after = stored_integration(&emulator, &api_id, &proxy_id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused UpdateIntegration must leave the record as it was"
        );
    }
}
