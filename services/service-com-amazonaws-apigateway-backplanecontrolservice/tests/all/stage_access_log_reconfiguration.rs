// SPDX-License-Identifier: CC0-1.0

//! WHICH `accessLogSettings` a record is delivered by: the ones the stage carries at the moment
//! it serves the request, however many times they were reconfigured before it.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A deployed stage renders and delivers its access-log record by the `accessLogSettings` it
//! > carries WHEN IT SERVES a request — the `destinationArn` ("the ARN of the CloudWatch Logs
//! > log group or Kinesis Data Firehose delivery stream to receive access logs") and the
//! > `format` ("a single line format of the access logs of data, as specified by selected
//! > $context variables") are read off the stage per request, not fixed when logging was first
//! > configured. So the settings an `UpdateStage` patch stores govern every LATER request and no
//! > earlier one: re-pointing the destination sends the next records to the new group and leaves
//! > the records already in the old one alone, changing the format renders the next records by
//! > the new one, a stage not yet configured delivers nothing, and a stage whose configuration
//! > was removed delivers nothing more. Every `$context` reference is resolved from THAT
//! > request and the stage serving it, whichever configuration was in force.
//!
//! This is the half neither sibling suite crosses. `stage_access_logging` and
//! `stage_proxy_access_log_context` configure a stage ONCE and then span the requests it serves;
//! `stage_access_log_record_rendering` spans the formats, each on its own stage, and removes a
//! configuration at the end of one stage's life. What is pinned here is the SEQUENCE: one stage
//! taken through unconfigured, configured, re-pointed, re-rendered, re-pointed back and removed,
//! with every record every phase left read back after each one — which is what makes "the
//! settings `UpdateStage` stores reach the emitter, and the emitter reads the CURRENT ones"
//! observable rather than inferred.
//!
//! Only a cross-service suite can pin it: the api, its stage and the logging configuration are
//! API Gateway's, the run that answers each request is AWS Lambda's, and the groups the records
//! land in are CloudWatch Logs' — all three on ONE engine, as `core/router/src/lib.rs` mounts
//! them. Every record is read back through CloudWatch Logs' own `FilterLogEvents`, so a record
//! that never reached that store cannot pass.
//!
//! The rows differ along what the rule generalises over: the CONFIGURATION in force (none, each
//! of three formats, each of two destinations, and removed), the DESTINATION a phase's record
//! belongs in, the VERB and the resource shape of the request served under it, and the STATUS
//! the run answered with.

use std::collections::BTreeMap;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::proxy_binary_media_types::{
    apigw, lambda, member, package, wait_until_active, FUNCTION_TIMEOUT, HANDLER, REGION, ROLE,
};
use crate::stage_access_log_absent_destination::destination_arn;
use crate::stage_access_logging::{
    create_log_group, delivered, emulator, install_account_role, invoke,
};
use crate::stage_proxy_access_log_context::{child, proxy_method};

/// The one stage every phase below reconfigures, which `$context.stage` reports.
const STAGE: &str = "edge";

/// The two destinations the stage is pointed at over its life, so WHICH group a phase's record
/// belongs in is a property of the configuration in force rather than of the suite.
const FIRST_GROUP: &str = "/apigw/reconfigured/first";
const SECOND_GROUP: &str = "/apigw/reconfigured/second";

/// The function every method of the api proxies to.
const FUNCTION: &str = "apigw-access-log-reconfigured";

/// The three formats the stage renders by over its life. Each selects `$context.requestId`,
/// which `AccessLogSettings$format` requires of a format at all ("the format must include at
/// least $context.requestId"), and each selects a DIFFERENT set of references in a different
/// shape, so a record says which configuration rendered it.
const JSON_FORMAT: &str = concat!(
    r#"{"marker":"reconfigured","requestId":"$context.requestId","#,
    r#""httpMethod":"$context.httpMethod","path":"$context.path","#,
    r#""status":"$context.status","stage":"$context.stage","#,
    r#""resourcePath":"$context.resourcePath"}"#
);
const TERSE_FORMAT: &str = "$context.requestId $context.httpMethod $context.path $context.status";
const LABELLED_FORMAT: &str = concat!(
    "stage=$context.stage resourcePath=$context.resourcePath path=$context.path ",
    "status=$context.status id=$context.requestId"
);

/// The handler behind every method: the query string says which status the run answers with, so
/// one package serves every phase and the status in each record is one a RUN returned.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    query = event.get("queryStringParameters") or {}
    status = int(query.get("status") or "200")
    return {
        "statusCode": status,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"answered": status}),
    }
"#;

/// What one served request resolves the `$context` references of a format to.
struct Resolved {
    request_id: String,
    verb: &'static str,
    path: &'static str,
    resource_path: &'static str,
    status: u16,
}

/// The access-log configuration a phase puts the stage in.
enum Configuration {
    /// Never configured: the stage carries no `accessLogSettings` at all yet.
    Unconfigured,
    /// Pointed at `group` and rendered by `format`, whose record for a request is `expected`.
    Set {
        group: &'static str,
        format: &'static str,
        expected: fn(&Resolved) -> String,
    },
    /// The configuration removed again (`remove /accessLogSettings`).
    Removed,
}

/// One phase: the configuration the stage is put in, and the one request it then serves.
struct Phase {
    case: &'static str,
    configuration: Configuration,
    /// The verb the request is sent under, which `$context.httpMethod` reports.
    verb: &'static str,
    /// What the client asks for within the stage, query string included.
    asked_for: &'static str,
    /// The `$context.path` of that request: the stage-qualified path, with no query string.
    path: &'static str,
    /// The `$context.resourcePath` of that request: the template that matched.
    resource_path: &'static str,
    /// The status the function's run answers with, which `$context.status` reports in decimal.
    status: u16,
}

/// The record [`JSON_FORMAT`] renders, member for member as that format spells it.
fn rendered_json(resolved: &Resolved) -> String {
    format!(
        "{{\"marker\":\"reconfigured\",\"requestId\":\"{id}\",\"httpMethod\":\"{verb}\",\
         \"path\":\"{path}\",\"status\":\"{status}\",\"stage\":\"{STAGE}\",\
         \"resourcePath\":\"{resource_path}\"}}",
        id = resolved.request_id,
        verb = resolved.verb,
        path = resolved.path,
        status = resolved.status,
        resource_path = resolved.resource_path,
    )
}

/// The record [`TERSE_FORMAT`] renders.
fn rendered_terse(resolved: &Resolved) -> String {
    format!(
        "{id} {verb} {path} {status}",
        id = resolved.request_id,
        verb = resolved.verb,
        path = resolved.path,
        status = resolved.status,
    )
}

/// The record [`LABELLED_FORMAT`] renders.
fn rendered_labelled(resolved: &Resolved) -> String {
    format!(
        "stage={STAGE} resourcePath={resource_path} path={path} status={status} id={id}",
        resource_path = resolved.resource_path,
        path = resolved.path,
        status = resolved.status,
        id = resolved.request_id,
    )
}

/// The phases, in the order one stage is taken through them.
fn phases() -> Vec<Phase> {
    vec![
        Phase {
            case: "the stage before any logging was configured on it",
            configuration: Configuration::Unconfigured,
            verb: "GET",
            asked_for: "/ping",
            path: "/edge/ping",
            resource_path: "/ping",
            status: 200,
        },
        Phase {
            case: "configured at the first destination, rendering the JSON format",
            configuration: Configuration::Set {
                group: FIRST_GROUP,
                format: JSON_FORMAT,
                expected: rendered_json,
            },
            verb: "GET",
            asked_for: "/ping",
            path: "/edge/ping",
            resource_path: "/ping",
            status: 200,
        },
        Phase {
            case: "re-pointed at the second destination, still the JSON format",
            configuration: Configuration::Set {
                group: SECOND_GROUP,
                format: JSON_FORMAT,
                expected: rendered_json,
            },
            verb: "POST",
            asked_for: "/ping?status=201",
            path: "/edge/ping",
            resource_path: "/ping",
            status: 201,
        },
        Phase {
            case: "re-rendered by a second format at that same destination",
            configuration: Configuration::Set {
                group: SECOND_GROUP,
                format: TERSE_FORMAT,
                expected: rendered_terse,
            },
            verb: "GET",
            asked_for: "/items/42",
            path: "/edge/items/42",
            resource_path: "/items/{id}",
            status: 200,
        },
        Phase {
            case: "re-pointed back at the first destination with a third format",
            configuration: Configuration::Set {
                group: FIRST_GROUP,
                format: LABELLED_FORMAT,
                expected: rendered_labelled,
            },
            verb: "DELETE",
            asked_for: "/ping?status=404",
            path: "/edge/ping",
            resource_path: "/ping",
            status: 404,
        },
        Phase {
            case: "the configuration removed again",
            configuration: Configuration::Removed,
            verb: "GET",
            asked_for: "/items/other",
            path: "/edge/items/other",
            resource_path: "/items/{id}",
            status: 200,
        },
    ]
}

/// Point the stage's access logging at `group`, rendered by `format`, and read the stored
/// configuration back through `GetStage`: what the emitter is expected to follow is what the
/// stage itself reports carrying.
async fn configure_access_logging(
    router: &MultiServiceRouter,
    api: &str,
    case: &str,
    group: &str,
    format: &str,
) {
    let destination = destination_arn(group, false);
    apigw(
        router,
        case,
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": "/accessLogSettings/destinationArn",
                    "value": destination,
                },
                { "op": "replace", "path": "/accessLogSettings/format", "value": format },
            ],
        })),
        200,
    )
    .await;
    let read = apigw(
        router,
        case,
        "GET",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        200,
    )
    .await;
    assert_eq!(
        read["accessLogSettings"],
        json!({ "destinationArn": destination, "format": format }),
        "case {case:?}: expected the stage to report the configuration just stored, got {:?}",
        read["accessLogSettings"]
    );
}

/// Remove the stage's access logging configuration again, and read back that it carries none.
async fn remove_access_logging(router: &MultiServiceRouter, api: &str, case: &str) {
    let removed = apigw(
        router,
        case,
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [{ "op": "remove", "path": "/accessLogSettings" }],
        })),
        200,
    )
    .await;
    assert_eq!(
        removed["accessLogSettings"],
        Value::Null,
        "case {case:?}: expected the stage to report no accessLogSettings, got {:?}",
        removed["accessLogSettings"]
    );
    let read = apigw(
        router,
        case,
        "GET",
        &format!("/restapis/{api}/stages/{STAGE}"),
        None,
        200,
    )
    .await;
    assert_eq!(
        read["accessLogSettings"],
        Value::Null,
        "case {case:?}: expected the stored stage to carry none either, got {:?}",
        read["accessLogSettings"]
    );
}

/// The whole setup: the function, an api whose methods all proxy to it, the grant that admits
/// the invocation, and a deployment released to [`STAGE`]. Answers the api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let created = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup CreateFunction: expected HTTP 201 from Lambda, got {} with {:?}",
        created.status,
        created.text()
    );
    let function_arn = member("CreateFunction", &created.json(), "FunctionArn");

    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "reconfigured-access-logged-api" })),
        201,
    )
    .await;
    let root = member("CreateRestApi", &api, "rootResourceId");
    let api = member("CreateRestApi", &api, "id");

    let ping = child(router, &api, &root, "ping").await;
    for verb in ["GET", "POST", "DELETE"] {
        proxy_method(router, &api, &ping, verb, &function_arn).await;
    }
    let items = child(router, &api, &root, "items").await;
    let item = child(router, &api, &items, "{id}").await;
    proxy_method(router, &api, &item, "GET", &function_arn).await;

    // "Grants a principal permission to use a function" (`AddPermission`): the grant that
    // admits the invocation API Gateway makes as its own service principal.
    let granted = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{account}:{api}/*/*",
                account = engine::emulator_account_id(),
            ),
        })),
    )
    .await;
    assert_eq!(
        granted.status,
        201,
        "setup AddPermission: expected HTTP 201 from Lambda, got {} with {:?}",
        granted.status,
        granted.text()
    );

    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
    wait_until_active(router, FUNCTION).await;
    api
}

/// Every request is logged by the configuration in force when the stage served it: the records
/// each destination holds after each phase are exactly the ones the phases up to it asked for.
#[tokio::test]
async fn a_stage_delivers_by_the_access_log_settings_it_carries_when_it_serves_a_request() {
    let router = emulator();
    let api = deployed(&router).await;
    install_account_role(&router).await;
    // Both destinations are log groups of the ACCOUNT's, created before the stage is pointed at
    // either: API Gateway delivers into a group that is there and brings none into existence.
    create_log_group(&router, FIRST_GROUP).await;
    create_log_group(&router, SECOND_GROUP).await;

    // What each group has to hold, grown by the record the phase just served asked for.
    let mut expected: BTreeMap<&str, Vec<String>> =
        [(FIRST_GROUP, Vec::new()), (SECOND_GROUP, Vec::new())].into();

    for phase in phases() {
        let case = phase.case;
        match &phase.configuration {
            Configuration::Unconfigured => {}
            Configuration::Set { group, format, .. } => {
                configure_access_logging(&router, &api, case, group, format).await;
            }
            Configuration::Removed => remove_access_logging(&router, &api, case).await,
        }

        let answered = invoke(&router, &api, STAGE, phase.verb, phase.asked_for).await;
        assert_eq!(
            answered.status, phase.status,
            "case {case:?}: expected HTTP {} from the stage, got {} with {:?}",
            phase.status,
            answered.status,
            answered.text()
        );
        let request_id = answered
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the response named no request id"));
        assert!(
            !request_id.is_empty(),
            "case {case:?}: expected a non-empty request id, got {request_id:?}"
        );

        if let Configuration::Set {
            group,
            expected: render,
            ..
        } = &phase.configuration
        {
            let resolved = Resolved {
                request_id,
                verb: phase.verb,
                path: phase.path,
                resource_path: phase.resource_path,
                status: phase.status,
            };
            expected
                .get_mut(group)
                .unwrap_or_else(|| panic!("case {case:?}: no destination {group:?} in this suite"))
                .push(render(&resolved));
        }

        for (group, has_to_hold) in &expected {
            let records = delivered(&router, group)
                .await
                .unwrap_or_else(|| panic!("case {case:?}: no group {group:?} at all"));
            assert_eq!(
                &records, has_to_hold,
                "case {case:?}: expected {group:?} to hold {has_to_hold:?}, got {records:?}"
            );
        }
    }
}
