// SPDX-License-Identifier: CC0-1.0

//! What a PARAMETER MAP of an integration holds after a write, over the restJson1 wire, and
//! the response-selection expression a WebSocket api's integration derives.
//!
//! The first rule under test: in every mapping language this service accepts, an entry's
//! VALUE names WHERE the parameter's value comes from, so an entry carried with the EMPTY
//! string behind it names no source and configures no mapping — the write stores no key for
//! it, and neither the writing operation's answer nor any later read reports one. The empty
//! STATIC value is written quoted (`''`), which is a value like any other. A mapping
//! TEMPLATE is the boundary of the rule: its value is the payload the integration sends, not
//! a source, so a template carried empty keeps its key.
//!
//! The second rule: `Integration$IntegrationResponseSelectionExpression` is "supported only
//! for WebSocket APIs" and is DERIVED by the service rather than taken from any member of a
//! request, so every integration of a WEBSOCKET api reports the fixed expression its own
//! responses are selected by and an HTTP api's integration reports none.
//!
//! The rows span what the rules generalise over: the mapping LANGUAGE (a WebSocket api's
//! integration request parameters, an HTTP api's `<action>:<operand>` transformations on the
//! request and on the response, and an integration response's own parameters), the OPERAND a
//! key names, how many entries one map carries and how many are cleared, WHICH writer carried
//! the empty value (the create or the patch), and the api's protocol.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, carries_nothing, emulator};

/// The expression every integration of a WEBSOCKET api reports: the integration's own
/// response status is what its integration responses are selected by, and nothing in the
/// expression comes from the request.
const DERIVED_RESPONSE_SELECTION: &str = "${integration.response.statuscode}";

/// A WebSocket api, whose integrations carry integration responses and whose request
/// parameters map a method request parameter onto an integration request parameter.
fn websocket_api(name: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": "$request.body.action"
    })
}

/// An HTTP api, whose integrations carry the `<action>:<header|querystring|path>.<location>`
/// transformation language.
fn http_api(name: &str) -> Value {
    json!({ "name": name, "protocolType": "HTTP" })
}

/// An `AWS` integration of a WebSocket api, with `members` merged onto it.
fn websocket_integration(members: Value) -> Value {
    let mut request = json!({
        "integrationType": "AWS",
        "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:ws",
        "integrationMethod": "POST"
    });
    merge(&mut request, members);
    request
}

/// An `HTTP_PROXY` integration of an HTTP api, with `members` merged onto it.
fn http_integration(members: Value) -> Value {
    let mut request = json!({
        "integrationType": "HTTP_PROXY",
        "integrationUri": "https://example.com/upstream",
        "integrationMethod": "ANY",
        "payloadFormatVersion": "1.0"
    });
    merge(&mut request, members);
    request
}

/// Write every member of `members` onto `request`.
fn merge(request: &mut Value, members: Value) {
    for (member, value) in members.as_object().expect("the members are a JSON object") {
        request[member] = value.clone();
    }
}

/// Create one api and answer its `ApiId`.
async fn create_api(emulator: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let (status, created) = call(emulator, "POST", "/v2/apis", Some(request.clone())).await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateApi, got {status} {created}"
    );
    as_str(&created, "apiId")
}

/// Create one integration and answer `(IntegrationId, the record the create reported)`.
async fn create_integration(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    request: &Value,
) -> (String, Value) {
    let (status, created) = call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(request.clone()),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegration, got {status} {created}"
    );
    (as_str(&created, "integrationId"), created)
}

/// The integration as `GetIntegration` reports it.
async fn get_integration(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    integration_id: &str,
) -> Value {
    let (status, fetched) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetIntegration, got {status} {fetched}"
    );
    fetched
}

/// Patch one integration and answer the record it reported.
async fn patch_integration(
    emulator: &MultiServiceRouter,
    case: &str,
    api_id: &str,
    integration_id: &str,
    patch: &Value,
) -> Value {
    let (status, patched) = call(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        Some(patch.clone()),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from UpdateIntegration, got {status} {patched}"
    );
    patched
}

/// One row of the tables below: an api, the integration created on it, the map member the
/// write carries, and the map the record must hold afterwards.
struct Row {
    case: &'static str,
    api: fn(&str) -> Value,
    integration: fn(Value) -> Value,
    /// The map member the row writes — `requestParameters`, `responseParameters` or
    /// `requestTemplates`.
    member: &'static str,
    /// The map the create carries for `member`.
    created: Value,
    /// The map the patch carries for `member`, and the map the record must hold after it.
    patched: Value,
    expected: Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a WebSocket integration's request parameters, one of them cleared",
            api: websocket_api,
            integration: websocket_integration,
            member: "requestParameters",
            created: json!({
                "integration.request.querystring.stage": "method.request.querystring.stage",
                "integration.request.header.x-kept": "method.request.header.x-kept"
            }),
            patched: json!({
                "integration.request.querystring.stage": "",
                "integration.request.header.x-kept": "method.request.header.x-kept"
            }),
            expected: json!({
                "integration.request.header.x-kept": "method.request.header.x-kept"
            }),
        },
        Row {
            case: "an HTTP integration's header transformations, one of them cleared",
            api: http_api,
            integration: http_integration,
            member: "requestParameters",
            created: json!({
                "append:header.x-trace": "$context.requestId",
                "overwrite:header.x-kept": "'yes'"
            }),
            patched: json!({
                "append:header.x-trace": "",
                "overwrite:header.x-kept": "'yes'"
            }),
            expected: json!({ "overwrite:header.x-kept": "'yes'" }),
        },
        Row {
            case: "an operand that is no header: a query-string transformation cleared",
            api: http_api,
            integration: http_integration,
            member: "requestParameters",
            created: json!({
                "overwrite:querystring.page": "'2'",
                "append:querystring.kept": "'yes'"
            }),
            patched: json!({
                "overwrite:querystring.page": "",
                "append:querystring.kept": "'yes'"
            }),
            expected: json!({ "append:querystring.kept": "'yes'" }),
        },
        Row {
            case: "every entry of the map cleared at once",
            api: http_api,
            integration: http_integration,
            member: "requestParameters",
            created: json!({
                "append:header.x-trace": "$context.requestId",
                "overwrite:header.x-other": "'yes'"
            }),
            patched: json!({
                "append:header.x-trace": "",
                "overwrite:header.x-other": ""
            }),
            expected: json!({}),
        },
        Row {
            case: "the empty STATIC value, which is a value and not a cleared entry",
            api: http_api,
            integration: http_integration,
            member: "requestParameters",
            created: json!({ "append:header.x-trace": "$context.requestId" }),
            patched: json!({ "overwrite:header.x-static": "''" }),
            expected: json!({ "overwrite:header.x-static": "''" }),
        },
        Row {
            case: "a response transformation keyed on a status, one of them cleared",
            api: http_api,
            integration: http_integration,
            member: "responseParameters",
            created: json!({
                "200": {
                    "append:header.x-mapped": "'by-the-api'",
                    "overwrite:header.x-kept": "'yes'"
                }
            }),
            patched: json!({
                "200": {
                    "append:header.x-mapped": "",
                    "overwrite:header.x-kept": "'yes'"
                }
            }),
            expected: json!({ "200": { "overwrite:header.x-kept": "'yes'" } }),
        },
        Row {
            // The boundary of the rule: a mapping template's value is the payload the
            // integration sends, so the empty one is a payload and not a missing source.
            case: "a mapping template carried empty, which keeps its key",
            api: websocket_api,
            integration: websocket_integration,
            member: "requestTemplates",
            created: json!({ "$default": "{\"statusCode\": 200}" }),
            patched: json!({ "$default": "" }),
            expected: json!({ "$default": "" }),
        },
    ]
}

/// RULE: an entry of a parameter map whose value is the empty string names no source for its
/// parameter, so the patch stores no key for it — in the answer and in every later read.
#[tokio::test]
async fn a_patched_parameter_carried_with_no_value_is_stored_under_no_key() {
    let emulator = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let api_id = create_api(
            &emulator,
            row.case,
            &(row.api)(&format!("cleared-patch-{index}")),
        )
        .await;
        let member = row.member;
        let (integration_id, created) = create_integration(
            &emulator,
            row.case,
            &api_id,
            &(row.integration)(json!({ member: row.created })),
        )
        .await;
        // Every entry this map was created with names a source, so the fresh record holds the
        // map as the create carried it: what the patch below changes is one entry of it.
        assert_eq!(
            created.get(member),
            Some(&row.created),
            "case {:?}: expected CreateIntegration to report {} {}, got {created}",
            row.case,
            member,
            row.created
        );

        let patched = patch_integration(
            &emulator,
            row.case,
            &api_id,
            &integration_id,
            &json!({ member: row.patched }),
        )
        .await;
        assert_eq!(
            patched.get(row.member),
            Some(&row.expected),
            "case {:?}: expected UpdateIntegration to report {} {}, got {patched}",
            row.case,
            row.member,
            row.expected
        );

        // And the write is what a later caller sees, not just what this call answered.
        let fetched = get_integration(&emulator, row.case, &api_id, &integration_id).await;
        assert_eq!(
            fetched.get(row.member),
            Some(&row.expected),
            "case {:?}: expected GetIntegration to report {} {}, got {fetched}",
            row.case,
            row.member,
            row.expected
        );
    }
}

/// RULE: the same reading at the CREATE — an entry carried with no value configures no
/// mapping, so the fresh record holds no key for it either.
#[tokio::test]
async fn a_created_parameter_carried_with_no_value_is_stored_under_no_key() {
    let emulator = emulator();

    for (index, row) in rows().into_iter().enumerate() {
        let api_id = create_api(
            &emulator,
            row.case,
            &(row.api)(&format!("cleared-create-{index}")),
        )
        .await;
        // The map the PATCH row carries, carried by the create instead: the entries that
        // name a source are stored and the cleared one is not.
        let member = row.member;
        let (integration_id, created) = create_integration(
            &emulator,
            row.case,
            &api_id,
            &(row.integration)(json!({ member: row.patched })),
        )
        .await;
        assert_eq!(
            created.get(row.member),
            Some(&row.expected),
            "case {:?}: expected CreateIntegration to report {} {}, got {created}",
            row.case,
            row.member,
            row.expected
        );
        let fetched = get_integration(&emulator, row.case, &api_id, &integration_id).await;
        assert_eq!(
            fetched.get(row.member),
            Some(&row.expected),
            "case {:?}: expected GetIntegration to report {} {}, got {fetched}",
            row.case,
            row.member,
            row.expected
        );
    }
}

/// RULE: an integration RESPONSE's own parameters are the same mapping language, so an entry
/// carried with no value is stored under no key there either, whichever writer carried it.
#[tokio::test]
async fn an_integration_responses_parameters_are_read_by_the_same_rule() {
    let emulator = emulator();
    let case = "an integration response whose parameters carry one cleared entry";

    let api_id = create_api(&emulator, case, &websocket_api("cleared-response")).await;
    let (integration_id, _) = create_integration(
        &emulator,
        case,
        &api_id,
        &json!({ "integrationType": "MOCK" }),
    )
    .await;
    let collection = format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses");

    let (status, created) = call(
        &emulator,
        "POST",
        &collection,
        Some(json!({
            "integrationResponseKey": "$default",
            "responseParameters": {
                "integration.response.header.x-kept": "'yes'",
                "integration.response.header.x-cleared": ""
            }
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected 201 from CreateIntegrationResponse, got {status} {created}"
    );
    let response_id = as_str(&created, "integrationResponseId");
    let kept = json!({ "integration.response.header.x-kept": "'yes'" });
    assert_eq!(
        created.get("responseParameters"),
        Some(&kept),
        "case {case:?}: expected the create to report {kept}, got {created}"
    );

    let member = format!("{collection}/{response_id}");
    let (status, patched) = call(
        &emulator,
        "PATCH",
        &member,
        Some(json!({
            "responseParameters": {
                "integration.response.header.x-kept": "'yes'",
                "integration.response.header.x-patched": ""
            }
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from UpdateIntegrationResponse, got {status} {patched}"
    );
    assert_eq!(
        patched.get("responseParameters"),
        Some(&kept),
        "case {case:?}: expected the patch to report {kept}, got {patched}"
    );

    let (status, fetched) = call(&emulator, "GET", &member, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetIntegrationResponse, got {status} {fetched}"
    );
    assert_eq!(
        fetched.get("responseParameters"),
        Some(&kept),
        "case {case:?}: expected the read-back to report {kept}, got {fetched}"
    );
}

/// One row of the derived-expression table: the case, the api it is read on, the
/// integration created there, and the expression that integration must report.
type DerivedRow = (&'static str, fn(&str) -> Value, Value, Option<&'static str>);

/// RULE: every integration of a WEBSOCKET api reports the derived
/// `IntegrationResponseSelectionExpression`, and an integration of an api of the other
/// protocol reports none — from every operation that reports an integration.
#[tokio::test]
async fn a_websocket_integration_reports_the_derived_response_selection_expression() {
    let emulator = emulator();

    // Rows span the protocols the member is defined for and the integration TYPES each
    // protocol admits: the expression belongs to the api's protocol and to nothing the
    // request carried.
    let rows: &[DerivedRow] = &[
        (
            "an AWS integration of a WebSocket api",
            websocket_api,
            websocket_integration(json!({})),
            Some(DERIVED_RESPONSE_SELECTION),
        ),
        (
            "a MOCK integration of a WebSocket api",
            websocket_api,
            json!({ "integrationType": "MOCK" }),
            Some(DERIVED_RESPONSE_SELECTION),
        ),
        (
            "an HTTP_PROXY integration of a WebSocket api",
            websocket_api,
            json!({
                "integrationType": "HTTP_PROXY",
                "integrationUri": "https://example.com/upstream",
                "integrationMethod": "ANY"
            }),
            Some(DERIVED_RESPONSE_SELECTION),
        ),
        (
            "an HTTP_PROXY integration of an HTTP api",
            http_api,
            http_integration(json!({})),
            None,
        ),
        (
            "a Lambda proxy integration of an HTTP api",
            http_api,
            json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:handler",
                "payloadFormatVersion": "2.0"
            }),
            None,
        ),
    ];

    for (index, (case, api, integration, expected)) in rows.iter().enumerate() {
        let api_id = create_api(&emulator, case, &api(&format!("derived-{index}"))).await;
        let (integration_id, created) =
            create_integration(&emulator, case, &api_id, integration).await;
        let patched = patch_integration(
            &emulator,
            case,
            &api_id,
            &integration_id,
            &json!({ "description": "patched" }),
        )
        .await;
        let fetched = get_integration(&emulator, case, &api_id, &integration_id).await;
        let (status, listed) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/integrations"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegrations, got {status} {listed}"
        );
        let listed = listed["items"][0].clone();

        for (reader, reported) in [
            ("CreateIntegration", created),
            ("UpdateIntegration", patched),
            ("GetIntegration", fetched),
            ("GetIntegrations", listed),
        ] {
            match expected {
                Some(expression) => assert_eq!(
                    reported
                        .get("integrationResponseSelectionExpression")
                        .and_then(Value::as_str),
                    Some(*expression),
                    "case {case:?}: expected {reader} to report {expression:?}, got {reported}"
                ),
                None => assert!(
                    carries_nothing(&reported, "integrationResponseSelectionExpression"),
                    "case {case:?}: expected {reader} to report no response-selection \
                     expression, got {reported}"
                ),
            }
        }
    }
}
