// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the rules `CreateIntegration` holds its input to.
//!
//! Three rules, each asserted over a table rather than over one request:
//!
//! * **An integration needs an `IntegrationUri` only when the URI is what names what it
//!   invokes.** `MOCK` invokes nothing, and an integration carrying an
//!   `IntegrationSubtype` names the AWS service action to invoke there
//!   (`CreateIntegration$IntegrationSubtype`: "Specifies the AWS service action to
//!   invoke"); `IntegrationUri` is not a required member of the input shape (only
//!   `ApiId` and `IntegrationType` are, `smithy_query(query="operation",
//!   operation="CreateIntegration")`), so neither form is incomplete and both are
//!   created.
//! * **`PayloadFormatVersion` is constrained by the integration type**: "Supported
//!   values for Lambda proxy integrations are 1.0 and 2.0. For all other integrations,
//!   1.0 is the only supported value" (`Integration$PayloadFormatVersion`).
//! * **A template selection expression may only name variables the service evaluates**:
//!   an unescaped `$` begins a variable reference, so the literal selection key
//!   `$default` has to be written escaped (`\$default`).
//! * **A first-class subtype integration is written only with the arguments its action
//!   requires.** An `IntegrationSubtype` "Specifies the AWS service action to invoke"
//!   (`CreateIntegration$IntegrationSubtype`) and the integration's `RequestParameters` are
//!   that action's own arguments, so a mapping that does not define every argument the
//!   action requires configures a call that could never be made and is refused at the write
//!   — on the create and on the update alike — naming the subtype and its whole required
//!   set.
//!
//! The rows span what each rule generalises over: the integration type, whether the
//! request carries a subtype, a URI, or both, the parent api's protocol, the payload
//! format version (each published value, one outside them), the form of the
//! selection expression (a bare variable, a braced one, an escaped literal, a plain
//! literal, a variable the service does not define), and — for the last rule — every
//! first-class subtype, each one of its required parameters left out, the value forms that
//! define nothing, and the values that have no required set at all. The recorded case that
//! filed the issue — an `SQS-SendMessage` subtype integration with no URI — is ONE row of the
//! first table.

use serde_json::{json, Map, Value};
use service_com_amazonaws_apigatewayv2_apigatewayv2::state::integration_subtype::{
    first_class_subtype, unmapped_required_parameters_message, FirstClassSubtype,
    FIRST_CLASS_SUBTYPES, STEP_FUNCTIONS_START_EXECUTION, STEP_FUNCTIONS_STOP_EXECUTION,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::api_lifecycle::{as_str, call, emulator, error_code};

/// The subtypes AWS names for first-class service integrations, as a request spells
/// them (`<Service>-<Action>`). `IntegrationSubtype` is an unconstrained string in the
/// model, so these are VALUES a caller sends, and the rule under test does not depend on
/// which one.
const SERVICE_ACTION_SUBTYPES: &[&str] = &[
    "SQS-SendMessage",
    "EventBridge-PutEvents",
    "StepFunctions-StartExecution",
    "StepFunctions-StartSyncExecution",
    "SQS-ReceiveMessage",
];

/// A `RequestParameters` map built from `(parameter, value)` pairs.
fn mapping(entries: &[(&str, &str)]) -> Value {
    let object: Map<String, Value> = entries
        .iter()
        .map(|(parameter, value)| ((*parameter).to_string(), json!(*value)))
        .collect();
    Value::Object(object)
}

/// A mapping defining every argument `subtype`'s action requires, which is what the service
/// holds a first-class subtype integration to: the parameters come from the catalog that
/// states them, so a row configures the subtype it names rather than some other one's
/// arguments.
fn required_mapping(subtype: &str) -> Value {
    let required = first_class_subtype(subtype).map_or(&[] as &[&str], |known| {
        known.required_parameters
    });
    let entries: Vec<(&str, &str)> = required
        .iter()
        .map(|parameter| (*parameter, "$request.body.value"))
        .collect();
    mapping(&entries)
}

fn http_api(name: &str) -> Value {
    json!({ "name": name, "protocolType": "HTTP" })
}

fn websocket_api(name: &str) -> Value {
    json!({
        "name": name,
        "protocolType": "WEBSOCKET",
        "routeSelectionExpression": "$request.body.action"
    })
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

/// Send one CreateIntegration and answer `(status, body)`.
async fn create_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    request: &Value,
) -> (u16, Value) {
    call(
        emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(request.clone()),
    )
    .await
}

/// How many integrations the api holds, so a refused create can be shown to have stored
/// nothing.
async fn integration_count(emulator: &MultiServiceRouter, api_id: &str) -> usize {
    let (status, listed) = call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from GetIntegrations, got {status} {listed}"
    );
    listed["items"].as_array().map_or(0, Vec::len)
}

/// One row of the `IntegrationUri` table.
struct UriRow {
    case: String,
    /// `true` for an HTTP api, `false` for a WebSocket one.
    http: bool,
    request: Value,
    /// The members the stored record must report back, once created.
    expect_members: Vec<(&'static str, Value)>,
    /// `None` when the create must be accepted.
    expect_refused: bool,
}

fn uri_rows() -> Vec<UriRow> {
    let mut rows = Vec::new();

    // Every first-class service integration: AWS_PROXY plus a subtype, credentials and
    // the request parameters that subtype's action requires, and NO IntegrationUri.
    for subtype in SERVICE_ACTION_SUBTYPES {
        let parameters = required_mapping(subtype);
        rows.push(UriRow {
            case: format!("AWS_PROXY with the {subtype} subtype and no IntegrationUri"),
            http: true,
            request: json!({
                "integrationType": "AWS_PROXY",
                "integrationSubtype": subtype,
                "payloadFormatVersion": "1.0",
                "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
                "requestParameters": parameters,
            }),
            expect_members: vec![
                ("integrationType", json!("AWS_PROXY")),
                ("integrationSubtype", json!(*subtype)),
                (
                    "credentialsArn",
                    json!("arn:aws:iam::000000000000:role/apigw"),
                ),
                ("requestParameters", parameters),
            ],
            expect_refused: false,
        });
    }

    rows.push(UriRow {
        case: "a subtype integration carrying a URI as well".to_string(),
        http: true,
        request: json!({
            "integrationType": "AWS_PROXY",
            "integrationSubtype": "SQS-SendMessage",
            "integrationUri": "arn:aws:sqs:eu-west-1:000000000000:a-queue",
            "payloadFormatVersion": "1.0",
            "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
            "requestParameters": required_mapping("SQS-SendMessage"),
        }),
        expect_members: vec![
            ("integrationSubtype", json!("SQS-SendMessage")),
            (
                "integrationUri",
                json!("arn:aws:sqs:eu-west-1:000000000000:a-queue"),
            ),
        ],
        expect_refused: false,
    });
    rows.push(UriRow {
        case: "a subtype integration with no credentials, which are optional".to_string(),
        http: true,
        request: json!({
            "integrationType": "AWS_PROXY",
            "integrationSubtype": "EventBridge-PutEvents",
            "payloadFormatVersion": "1.0",
            "requestParameters": required_mapping("EventBridge-PutEvents"),
        }),
        expect_members: vec![("integrationSubtype", json!("EventBridge-PutEvents"))],
        expect_refused: false,
    });
    rows.push(UriRow {
        case: "MOCK, which invokes nothing, on a WebSocket api".to_string(),
        http: false,
        request: json!({ "integrationType": "MOCK" }),
        expect_members: vec![("integrationType", json!("MOCK"))],
        expect_refused: false,
    });
    rows.push(UriRow {
        case: "AWS_PROXY naming neither a URI nor a subtype".to_string(),
        http: true,
        request: json!({ "integrationType": "AWS_PROXY", "payloadFormatVersion": "1.0" }),
        expect_members: Vec::new(),
        expect_refused: true,
    });
    rows.push(UriRow {
        case: "HTTP_PROXY naming no URI".to_string(),
        http: true,
        request: json!({ "integrationType": "HTTP_PROXY", "payloadFormatVersion": "1.0" }),
        expect_members: Vec::new(),
        expect_refused: true,
    });
    rows.push(UriRow {
        case: "a WebSocket AWS integration naming no URI".to_string(),
        http: false,
        request: json!({ "integrationType": "AWS" }),
        expect_members: Vec::new(),
        expect_refused: true,
    });
    rows.push(UriRow {
        case: "a subtype on an integration that is not AWS_PROXY".to_string(),
        http: true,
        request: json!({
            "integrationType": "HTTP_PROXY",
            "integrationSubtype": "SQS-SendMessage",
            "payloadFormatVersion": "1.0"
        }),
        expect_members: Vec::new(),
        expect_refused: true,
    });

    rows
}

#[tokio::test]
async fn an_integration_needs_a_uri_only_when_the_uri_names_what_it_invokes() {
    let emulator = emulator();

    for (index, row) in uri_rows().into_iter().enumerate() {
        let request = if row.http {
            http_api(&format!("uri-rule-http-{index}"))
        } else {
            websocket_api(&format!("uri-rule-socket-{index}"))
        };
        let api_id = create_api(&emulator, &row.case, &request).await;

        let (status, body) = create_integration(&emulator, &api_id, &row.request).await;
        if row.expect_refused {
            assert_eq!(
                status, 400,
                "case {:?}: expected 400 from CreateIntegration, got {status} {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                "BadRequestException",
                "case {:?}: expected BadRequestException, got {body}",
                row.case
            );
            assert_eq!(
                integration_count(&emulator, &api_id).await,
                0,
                "case {:?}: a refused create must store no integration",
                row.case
            );
            continue;
        }

        assert_eq!(
            status, 201,
            "case {:?}: expected 201 from CreateIntegration, got {status} {body}",
            row.case
        );
        let integration_id = as_str(&body, "integrationId");
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected 200 from GetIntegration, got {status} {fetched}",
            row.case
        );
        assert_eq!(
            fetched, body,
            "case {:?}: GetIntegration must report what CreateIntegration stored",
            row.case
        );
        for (member, expected) in &row.expect_members {
            assert_eq!(
                fetched.get(*member),
                Some(expected),
                "case {:?}: wrong {member}, got {fetched}",
                row.case
            );
        }
        // An integration created from a subtype alone names no URI, and reports none.
        if row.request.get("integrationUri").is_none() {
            assert_eq!(
                fetched.get("integrationUri"),
                None,
                "case {:?}: an integration created with no URI must report none, got {fetched}",
                row.case
            );
        }
    }
}

/// One row of the payload-format-version table.
struct PayloadRow {
    case: String,
    http: bool,
    request: Value,
    accepted: bool,
}

fn payload_rows() -> Vec<PayloadRow> {
    let lambda = "arn:aws:lambda:eu-west-1:000000000000:function:handler";
    let mut rows = Vec::new();

    // A Lambda proxy integration: both published values, and one outside them.
    for (version, accepted) in [("1.0", true), ("2.0", true), ("3.0", false), ("2", false)] {
        rows.push(PayloadRow {
            case: format!("AWS_PROXY invoking a function with PayloadFormatVersion {version}"),
            http: true,
            request: json!({
                "integrationType": "AWS_PROXY",
                "integrationUri": lambda,
                "payloadFormatVersion": version
            }),
            accepted,
        });
    }
    // Every other integration: 1.0 only. An AWS_PROXY integration naming a service
    // action through its subtype is one of them — it invokes no function.
    for (version, accepted) in [("1.0", true), ("2.0", false)] {
        rows.push(PayloadRow {
            case: format!("HTTP_PROXY with PayloadFormatVersion {version}"),
            http: true,
            request: json!({
                "integrationType": "HTTP_PROXY",
                "integrationUri": "https://example.com/upstream",
                "payloadFormatVersion": version
            }),
            accepted,
        });
        rows.push(PayloadRow {
            case: format!("an SQS-SendMessage subtype with PayloadFormatVersion {version}"),
            http: true,
            request: json!({
                "integrationType": "AWS_PROXY",
                "integrationSubtype": "SQS-SendMessage",
                "payloadFormatVersion": version,
                "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
                "requestParameters": required_mapping("SQS-SendMessage"),
            }),
            accepted,
        });
        rows.push(PayloadRow {
            case: format!("a WebSocket AWS integration with PayloadFormatVersion {version}"),
            http: false,
            request: json!({
                "integrationType": "AWS",
                "integrationUri": lambda,
                "payloadFormatVersion": version
            }),
            accepted,
        });
    }

    rows
}

#[tokio::test]
async fn the_payload_format_version_an_integration_may_carry_follows_its_type() {
    let emulator = emulator();

    for (index, row) in payload_rows().into_iter().enumerate() {
        let request = if row.http {
            http_api(&format!("payload-http-{index}"))
        } else {
            websocket_api(&format!("payload-socket-{index}"))
        };
        let api_id = create_api(&emulator, &row.case, &request).await;

        let (status, body) = create_integration(&emulator, &api_id, &row.request).await;
        if row.accepted {
            assert_eq!(
                status, 201,
                "case {:?}: expected 201 from CreateIntegration, got {status} {body}",
                row.case
            );
            assert_eq!(
                as_str(&body, "payloadFormatVersion"),
                as_str(&row.request, "payloadFormatVersion"),
                "case {:?}: the stored PayloadFormatVersion must be the one sent, got {body}",
                row.case
            );
        } else {
            assert_eq!(
                status, 400,
                "case {:?}: expected 400 from CreateIntegration, got {status} {body}",
                row.case
            );
            assert_eq!(
                error_code(&body),
                "BadRequestException",
                "case {:?}: expected BadRequestException, got {body}",
                row.case
            );
            assert_eq!(
                integration_count(&emulator, &api_id).await,
                0,
                "case {:?}: a refused create must store no integration",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn a_template_selection_expression_may_only_name_variables_the_service_evaluates() {
    let emulator = emulator();

    // Every row is a WebSocket `AWS` integration: "The template selection expression for
    // the integration. Supported only for WebSocket APIs"
    // (`Integration$TemplateSelectionExpression`).
    let rows = [
        ("a bare request variable", "$request.body.action", true),
        ("a braced request variable", "${request.body.action}", true),
        ("a context variable", "$context.routeKey", true),
        ("a stage variable", "$stageVariables.stage", true),
        ("the escaped selection key", "\\$default", true),
        ("a plain literal naming no variable", "action", true),
        ("the unescaped selection key", "$default", false),
        ("a variable the service does not define", "$notAVariable", false),
        ("an unescaped key beside a variable", "$request.body.x $default", false),
    ];

    for (index, (case, expression, accepted)) in rows.into_iter().enumerate() {
        let api_id = create_api(
            &emulator,
            case,
            &websocket_api(&format!("template-expression-{index}")),
        )
        .await;
        let request = json!({
            "integrationType": "AWS",
            "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:ws",
            "integrationMethod": "POST",
            "requestTemplates": { "application/json": "{\"body\": $input.json('$')}" },
            "templateSelectionExpression": expression
        });

        let (status, body) = create_integration(&emulator, &api_id, &request).await;
        if accepted {
            assert_eq!(
                status, 201,
                "case {case:?} ({expression:?}): expected 201 from CreateIntegration, got {status} {body}"
            );
            assert_eq!(
                as_str(&body, "templateSelectionExpression"),
                expression,
                "case {case:?}: the stored expression must be the one sent, got {body}"
            );
        } else {
            assert_eq!(
                status, 400,
                "case {case:?} ({expression:?}): expected 400 from CreateIntegration, got {status} {body}"
            );
            assert_eq!(
                error_code(&body),
                "BadRequestException",
                "case {case:?}: expected BadRequestException, got {body}"
            );
            assert!(
                as_str(&body, "message").contains("selection expression"),
                "case {case:?}: the refusal must say what it refused, got {body}"
            );
            assert_eq!(
                integration_count(&emulator, &api_id).await,
                0,
                "case {case:?}: a refused create must store no integration"
            );
        }
    }
}

/// One integration, as its reader reports it.
async fn get_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    integration_id: &str,
) -> (u16, Value) {
    call(
        emulator,
        "GET",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        None,
    )
    .await
}

/// Send one UpdateIntegration and answer `(status, body)`.
async fn update_integration(
    emulator: &MultiServiceRouter,
    api_id: &str,
    integration_id: &str,
    patch: &Value,
) -> (u16, Value) {
    call(
        emulator,
        "PATCH",
        &format!("/v2/apis/{api_id}/integrations/{integration_id}"),
        Some(patch.clone()),
    )
    .await
}

/// The create a subtype row sends: the subtype, configured with `request_parameters`.
fn subtype_integration(subtype: &str, request_parameters: Value) -> Value {
    json!({
        "integrationType": "AWS_PROXY",
        "integrationSubtype": subtype,
        "payloadFormatVersion": "1.0",
        "credentialsArn": "arn:aws:iam::000000000000:role/apigw",
        "requestParameters": request_parameters,
    })
}

/// Assert one refusal of a write: the status, the modelled error and the exact sentence.
fn assert_refused(case: &str, expected_message: &str, status: u16, body: &Value) {
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
        expected_message,
        "case {case:?}: wrong refusal sentence, got {body}"
    );
}

#[tokio::test]
async fn a_subtype_integration_is_created_only_with_the_parameters_its_action_requires() {
    let emulator = emulator();

    // The refusal is API Gateway's own sentence, pinned here once per shape of the bracketed
    // list — one required parameter and several — so its wording is asserted and not merely
    // agreed between the handler and this test.
    assert_eq!(
        unmapped_required_parameters_message(
            STEP_FUNCTIONS_STOP_EXECUTION.name,
            STEP_FUNCTIONS_STOP_EXECUTION.required_parameters,
        ),
        "Operation: StepFunctions-StopExecution requires enabling passthrough, or defining \
         all of the following parameters in the parameter mapping: [ExecutionArn]",
        "the refusal must be worded as API Gateway words it"
    );
    assert_eq!(
        unmapped_required_parameters_message("SQS-SendMessage", &["QueueUrl", "MessageBody"]),
        "Operation: SQS-SendMessage requires enabling passthrough, or defining all of the \
         following parameters in the parameter mapping: [QueueUrl, MessageBody]",
        "a required set of more than one must be listed comma-separated inside the brackets, \
         in that subtype's own order"
    );

    for (index, subtype) in FIRST_CLASS_SUBTYPES.iter().enumerate() {
        let expected =
            unmapped_required_parameters_message(subtype.name, subtype.required_parameters);
        let complete = required_mapping(subtype.name);

        // Every required parameter defined: created, and reported back by the reader.
        let case = format!("{} with every required parameter", subtype.name);
        let api_id = create_api(
            &emulator,
            &case,
            &http_api(&format!("subtype-complete-{index}")),
        )
        .await;
        let (status, created) = create_integration(
            &emulator,
            &api_id,
            &subtype_integration(subtype.name, complete.clone()),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?}: expected 201 from CreateIntegration, got {status} {created}"
        );
        let integration_id = as_str(&created, "integrationId");
        let (status, fetched) = get_integration(&emulator, &api_id, &integration_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegration, got {status} {fetched}"
        );
        assert_eq!(
            fetched["requestParameters"], complete,
            "case {case:?}: expected the stored mapping to be the one sent, got {fetched}"
        );
        assert_eq!(
            as_str(&fetched, "integrationSubtype"),
            subtype.name,
            "case {case:?}: expected the stored subtype, got {fetched}"
        );

        // Each ONE of them left out, and then all of them: refused, and nothing stored.
        let mut incomplete: Vec<(String, Value)> = Vec::new();
        for dropped in subtype.required_parameters {
            let mut entries: Vec<(&str, &str)> = subtype
                .required_parameters
                .iter()
                .filter(|parameter| *parameter != dropped)
                .map(|parameter| (*parameter, "$request.body.value"))
                .collect();
            // An optional argument of the same action in its place, so the refusal is about
            // the required parameter and not about a mapping that defines nothing at all.
            entries.push(("Region", "eu-west-1"));
            incomplete.push((
                format!("{} without {dropped}", subtype.name),
                mapping(&entries),
            ));
        }
        incomplete.push((
            format!("{} with an empty RequestParameters", subtype.name),
            json!({}),
        ));
        for (refused, (case, parameters)) in incomplete.into_iter().enumerate() {
            let api_id = create_api(
                &emulator,
                &case,
                &http_api(&format!("subtype-refused-{index}-{refused}")),
            )
            .await;
            let (status, body) = create_integration(
                &emulator,
                &api_id,
                &subtype_integration(subtype.name, parameters),
            )
            .await;
            assert_refused(&case, &expected, status, &body);
            assert_eq!(
                integration_count(&emulator, &api_id).await,
                0,
                "case {case:?}: a refused CreateIntegration must store no integration"
            );
        }
    }
}

#[tokio::test]
async fn a_required_parameter_is_defined_only_by_a_value_under_its_own_name() {
    /// One row: the case, the mapping it sends for the one parameter the action requires,
    /// and whether the write takes it.
    struct ValueRow<'r> {
        case: &'static str,
        entries: Vec<(&'r str, &'static str)>,
        accepted: bool,
    }
    let emulator = emulator();
    let subtype = &STEP_FUNCTIONS_STOP_EXECUTION;
    let parameter = subtype.required_parameters[0];
    let lowered = parameter.to_lowercase();
    let expected = unmapped_required_parameters_message(subtype.name, subtype.required_parameters);

    // Rows differ along the VALUE and the SPELLING the mapping carries for the one parameter
    // the action requires.
    let rows = [
        ValueRow {
            case: "a mapping expression over the request",
            entries: vec![(parameter, "$request.body.arn")],
            accepted: true,
        },
        ValueRow {
            case: "a static value",
            entries: vec![(parameter, "a-static-value")],
            accepted: true,
        },
        ValueRow {
            case: "an empty value",
            entries: vec![(parameter, "")],
            accepted: false,
        },
        ValueRow {
            case: "a blank value",
            entries: vec![(parameter, "   ")],
            accepted: false,
        },
        ValueRow {
            case: "another casing of the parameter's name",
            entries: vec![(lowered.as_str(), "$request.body.arn")],
            accepted: false,
        },
        ValueRow {
            case: "an optional parameter of the same action only",
            entries: vec![("Cause", "stopped-by-the-route")],
            accepted: false,
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let api_id = create_api(&emulator, case, &http_api(&format!("subtype-value-{index}"))).await;
        let (status, body) = create_integration(
            &emulator,
            &api_id,
            &subtype_integration(subtype.name, mapping(&row.entries)),
        )
        .await;
        if row.accepted {
            assert_eq!(
                status, 201,
                "case {case:?}: expected 201 from CreateIntegration, got {status} {body}"
            );
        } else {
            assert_refused(case, &expected, status, &body);
            assert_eq!(
                integration_count(&emulator, &api_id).await,
                0,
                "case {case:?}: a refused CreateIntegration must store no integration"
            );
        }
    }
}

#[tokio::test]
async fn an_update_cannot_leave_a_subtype_integration_without_its_required_parameters() {
    let emulator = emulator();
    let api_id = create_api(
        &emulator,
        "the patched api",
        &http_api("subtype-patched"),
    )
    .await;

    // The integration the patches below are made against: a start of a workflow, complete.
    let start = &STEP_FUNCTIONS_START_EXECUTION;
    let stored_mapping = required_mapping(start.name);
    let (status, created) = create_integration(
        &emulator,
        &api_id,
        &subtype_integration(start.name, stored_mapping.clone()),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateIntegration, got {status} {created}"
    );
    let integration_id = as_str(&created, "integrationId");

    // A Lambda proxy integration, which names no subtype at all, so a patch can be shown to
    // be refused for TAKING ON a subtype as well as for dropping one of its parameters. Its
    // payload format version is the one every integration type supports, so the refusal
    // below is about the subtype's parameters and not about the version.
    let (status, proxy) = create_integration(
        &emulator,
        &api_id,
        &json!({
            "integrationType": "AWS_PROXY",
            "integrationUri": "arn:aws:lambda:eu-west-1:000000000000:function:handler",
            "payloadFormatVersion": "1.0"
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateIntegration for the proxy, got {status} {proxy}"
    );
    let proxy_id = as_str(&proxy, "integrationId");

    // Rows differ along WHICH record is patched and HOW the patch would leave it
    // unconfigured: the mapping replaced by one without the required parameter, replaced by
    // an empty map, and a subtype taken on by an integration whose mapping defines none of
    // its arguments.
    struct PatchRow<'r> {
        case: &'static str,
        /// The integration the patch is sent for.
        patched: &'r str,
        patch: Value,
        /// The subtype the merged record would name, whose required set the refusal states.
        subtype: &'static FirstClassSubtype,
    }
    let rows = [
        PatchRow {
            case: "a mapping patched to drop the parameter the action requires",
            patched: integration_id.as_str(),
            patch: json!({ "requestParameters": { "Name": "$request.body.name" } }),
            subtype: start,
        },
        PatchRow {
            case: "a mapping patched to an empty map",
            patched: integration_id.as_str(),
            patch: json!({ "requestParameters": {} }),
            subtype: start,
        },
        PatchRow {
            case: "a subtype taken on by an integration that maps none of its arguments",
            patched: proxy_id.as_str(),
            patch: json!({ "integrationSubtype": STEP_FUNCTIONS_STOP_EXECUTION.name }),
            subtype: &STEP_FUNCTIONS_STOP_EXECUTION,
        },
    ];
    for row in &rows {
        let case = row.case;
        let (_, before) = get_integration(&emulator, &api_id, row.patched).await;
        let expected = unmapped_required_parameters_message(
            row.subtype.name,
            row.subtype.required_parameters,
        );
        let (status, body) = update_integration(&emulator, &api_id, row.patched, &row.patch).await;
        assert_refused(case, &expected, status, &body);
        let (_, after) = get_integration(&emulator, &api_id, row.patched).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused UpdateIntegration must leave the record as it was"
        );
    }

    // A patch that keeps every required parameter defined is applied, so the refusals above
    // are about the required set and not about patching a subtype integration at all.
    let mut replacement = stored_mapping.clone();
    replacement
        .as_object_mut()
        .expect("the mapping of the stored integration is an object")
        .insert("Name".to_string(), json!("$request.body.name"));
    let (status, patched) = update_integration(
        &emulator,
        &api_id,
        &integration_id,
        &json!({ "requestParameters": replacement.clone() }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected 200 from UpdateIntegration for a complete mapping, got {status} {patched}"
    );
    assert_eq!(
        patched["requestParameters"], replacement,
        "expected the patched mapping to be the one sent, got {patched}"
    );
}

#[tokio::test]
async fn an_integration_naming_no_first_class_subtype_is_created_as_it_came() {
    let emulator = emulator();

    // A value this service states no required set for carries no requirement: the rows
    // differ along the VALUE of the member — a subtype of a service with no catalogued
    // action, another casing of a catalogued one, and a value naming no action at all.
    let rows: &[&str] = &["S3-PutObject", "sqs-sendmessage", "not-an-action"];
    for (index, subtype) in rows.iter().enumerate() {
        let api_id = create_api(
            &emulator,
            subtype,
            &http_api(&format!("subtype-unknown-{index}")),
        )
        .await;
        let (status, created) =
            create_integration(&emulator, &api_id, &subtype_integration(subtype, json!({}))).await;
        assert_eq!(
            status, 201,
            "case {subtype:?}: expected 201 from CreateIntegration, got {status} {created}"
        );
        assert_eq!(
            as_str(&created, "integrationSubtype"),
            *subtype,
            "case {subtype:?}: expected the subtype to be stored as it came, got {created}"
        );
    }
}

/// The required set of every first-class subtype, in the order AWS's own refusal lists it.
///
/// Written out here rather than read off the catalog under test, so the ORDER is asserted
/// against AWS and not merely agreed between the handler and this suite. Source: the
/// parameter order API Gateway publishes for each subtype in its AWS-service integration
/// reference for HTTP APIs, which is the order its refusal reproduces — note `SQS-SendMessage`
/// and `SQS-DeleteMessage`, two actions of one service whose two parameters are listed the
/// opposite way round, and `Kinesis-PutRecord` and `AppConfig-GetConfiguration`, neither of
/// which is alphabetical.
const PUBLISHED_PARAMETER_ORDER: &[(&str, &[&str])] = &[
    (
        "AppConfig-GetConfiguration",
        &["Application", "Environment", "Configuration", "ClientId"],
    ),
    ("EventBridge-PutEvents", &["Detail", "DetailType", "Source"]),
    ("Kinesis-PutRecord", &["StreamName", "Data", "PartitionKey"]),
    ("SQS-DeleteMessage", &["ReceiptHandle", "QueueUrl"]),
    ("SQS-PurgeQueue", &["QueueUrl"]),
    ("SQS-ReceiveMessage", &["QueueUrl"]),
    ("SQS-SendMessage", &["QueueUrl", "MessageBody"]),
    ("StepFunctions-StartExecution", &["StateMachineArn"]),
    ("StepFunctions-StartSyncExecution", &["StateMachineArn"]),
    ("StepFunctions-StopExecution", &["ExecutionArn"]),
];

/// RULE: the refusal of a subtype integration whose mapping does not define the parameters its
/// action requires lists that subtype's WHOLE required set in the order AWS publishes for THAT
/// subtype — a per-subtype order, never a sorted one.
#[tokio::test]
async fn the_refusal_lists_a_subtype_s_required_set_in_that_subtype_s_own_order() {
    let emulator = emulator();
    // Rows differ along the SUBTYPE, which is what the order is a fact about: every subtype
    // the catalog states a required set for, from a one-parameter action to a four-parameter
    // one, including two of one service that order their parameters oppositely.
    for (index, (subtype, published)) in PUBLISHED_PARAMETER_ORDER.iter().enumerate() {
        let case = format!("{subtype} with nothing mapped");
        let api_id =
            create_api(&emulator, &case, &http_api(&format!("subtype-order-{index}"))).await;
        let (status, body) =
            create_integration(&emulator, &api_id, &subtype_integration(subtype, json!({}))).await;
        let expected = format!(
            "Operation: {subtype} requires enabling passthrough, or defining all of the \
             following parameters in the parameter mapping: [{}]",
            published.join(", ")
        );
        assert_refused(&case, &expected, status, &body);
        // The same list, as the catalog this service renders that sentence from states it.
        let stated = first_class_subtype(subtype)
            .unwrap_or_else(|| panic!("case {case:?}: expected a catalogued subtype"))
            .required_parameters;
        assert_eq!(
            stated, *published,
            "case {case:?}: expected the catalog to state {published:?}, got {stated:?}"
        );
    }
}

/// RULE: that order is the published one and NOT a sorted one, which is a difference a client
/// sees for every subtype whose published order is not alphabetical — and every catalogued
/// subtype has its order asserted, so one added to the catalog cannot arrive unchecked.
#[test]
fn the_listed_order_is_the_published_one_and_not_a_sorted_one() {
    for subtype in FIRST_CLASS_SUBTYPES {
        let published = PUBLISHED_PARAMETER_ORDER
            .iter()
            .find(|(name, _)| *name == subtype.name)
            .map(|(_, order)| *order)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: every catalogued subtype must state its published order",
                    subtype.name
                )
            });
        assert_eq!(
            subtype.required_parameters, published,
            "case {:?}: expected {published:?}, got {:?}",
            subtype.name, subtype.required_parameters
        );
    }
    // The four subtypes whose published order differs from the alphabetical one: for each,
    // the rendered sentence must differ from the one a sorted list would render.
    let unsorted = [
        "AppConfig-GetConfiguration",
        "Kinesis-PutRecord",
        "SQS-DeleteMessage",
        "SQS-SendMessage",
    ];
    for subtype in unsorted {
        let known = first_class_subtype(subtype)
            .unwrap_or_else(|| panic!("case {subtype:?}: expected a catalogued subtype"));
        let mut sorted: Vec<&str> = known.required_parameters.to_vec();
        sorted.sort_unstable();
        let rendered = unmapped_required_parameters_message(subtype, known.required_parameters);
        let if_sorted = unmapped_required_parameters_message(subtype, &sorted);
        assert_ne!(
            rendered, if_sorted,
            "case {subtype:?}: expected the published order rather than a sorted one, got \
             {rendered:?}"
        );
    }
}
