// SPDX-License-Identifier: CC0-1.0

//! WHEN the log group API Gateway publishes for a stage's EXECUTION log comes into existence.
//!
//! The rule under test, one level above the recorded case:
//!
//! > `API-Gateway-Execution-Logs_{rest-api-id}/{stage-name}` is a log group API Gateway
//! > publishes under its OWN name — unlike an access-log `destinationArn`, which names a group
//! > of the account's — so a stage whose execution logging is ON has it in CloudWatch Logs from
//! > the moment the configuration that turns it on is accepted, before the stage has served any
//! > request. Execution logging is on when a method setting of the stage carries a
//! > `loggingLevel` of `ERROR` or `INFO` ("valid values are `OFF`, `ERROR`, and `INFO`",
//! > `MethodSetting$loggingLevel`), at whichever scope the patch addressed; a stage at `OFF`, a
//! > stage whose only logging setting is `dataTrace` (which adds payloads to a trace rather
//! > than turning one on), and a stage with no method setting at all have no such group. The
//! > group is the api's and the stage's own, so two stages of one api are two groups.
//!
//! The rows differ along what the rule generalises over: the `loggingLevel` the stage is
//! configured at, the SCOPE the setting was written at (`*/*`, so every method of the stage, as
//! against one `{resourcePath}/{httpMethod}`), the patch `op` that wrote it (`add` as much as
//! `replace`), and the settings that turn nothing on. Every row is asserted with NO request
//! served under it: the group's existence is the configuration's doing, not a delivery's.
//!
//! Only a cross-service suite can pin this: the api, its stage and the logging configuration
//! are API Gateway's, the role the account logs under is AWS IAM's, and whether a group exists
//! at all is CloudWatch Logs' — all three on ONE engine, as `core/router/src/lib.rs` mounts
//! them. Each assertion reads the answer back through CloudWatch Logs' own `FilterLogEvents`,
//! which reports `ResourceNotFoundException` for a group the account does not hold.

use serde_json::json;

use crate::account_logging_role::{create_role, replace_role, role_arn, trusting};
use crate::execute_api_data_plane::{deploy, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, gateway_with_iam_and_logs, Gateway};
use crate::stage_access_logging::{delivered, execution_log_group};

/// The stage every api below is deployed to, the path part of its one resource and the verb it
/// carries — the method a single-scope setting is written for.
const STAGE: &str = "live";
const PATH_PART: &str = "ping";
const RESOURCE_PATH: &str = "/ping";
const VERB: &str = "GET";
const STATUS: &str = "200";
const BODY: &str = r#"{"pong":true}"#;

/// The role the account writes its logs as, and the principal its trust policy has to admit
/// for API Gateway to write at all.
const ROLE_NAME: &str = "apigw-execution-log-group";
const ROLE_PATH: &str = "/service-role/";
const SERVICE_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// One row: the method-setting patch a stage is configured with, and whether that
/// configuration is execution logging turned ON.
struct Row {
    case: &'static str,
    /// The `op` of each patch operation, which is the two a method setting may be written by.
    op: &'static str,
    /// The method-setting pointers and the values written at them, in order.
    settings: &'static [(&'static str, &'static str)],
    /// Whether the stage then publishes its execution log group.
    published: bool,
}

/// The rows: each level, at each scope, written by each op, and the settings that turn
/// execution logging on for nothing.
const ROWS: &[Row] = &[
    Row {
        case: "INFO on every method of the stage",
        op: "replace",
        settings: &[("/*/*/logging/loglevel", "INFO")],
        published: true,
    },
    Row {
        case: "ERROR on every method of the stage",
        op: "replace",
        settings: &[("/*/*/logging/loglevel", "ERROR")],
        published: true,
    },
    Row {
        case: "INFO on one method alone",
        op: "replace",
        settings: &[("/~1ping/GET/logging/loglevel", "INFO")],
        published: true,
    },
    Row {
        case: "ERROR added rather than replaced",
        op: "add",
        settings: &[("/*/*/logging/loglevel", "ERROR")],
        published: true,
    },
    Row {
        case: "INFO beside the data trace, which is logging on with payloads",
        op: "replace",
        settings: &[
            ("/*/*/logging/loglevel", "INFO"),
            ("/*/*/logging/dataTrace", "true"),
        ],
        published: true,
    },
    Row {
        case: "the level set to OFF",
        op: "replace",
        settings: &[("/*/*/logging/loglevel", "OFF")],
        published: false,
    },
    Row {
        case: "the data trace alone, with no level at all",
        op: "replace",
        settings: &[("/*/*/logging/dataTrace", "true")],
        published: false,
    },
    Row {
        case: "a method setting that is not logging at all",
        op: "replace",
        settings: &[("/*/*/metrics/enabled", "true")],
        published: false,
    },
];

/// Install the account's CloudWatch role through AWS IAM's own `CreateRole` and API Gateway's
/// own `UpdateAccount`, which is what API Gateway publishes the account's log groups as.
async fn install_account_role(gateway: &Gateway) {
    create_role(gateway, ROLE_NAME, ROLE_PATH, &trusting(SERVICE_PRINCIPAL)).await;
    let installed = replace_role(gateway, &role_arn(ROLE_PATH, ROLE_NAME)).await;
    assert_eq!(
        installed.status, 200,
        "setup UpdateAccount /cloudwatchRoleArn: expected 200, got {} with {:?}",
        installed.status, installed.body
    );
}

/// An api with one `MOCK` resource, deployed to `stage`. Answers the api id.
async fn deployed(gateway: &Gateway, name: &str, stage: &str) -> String {
    let (api, root) = create_api(gateway, name).await;
    mock_resource(
        gateway,
        &api,
        &root,
        PATH_PART,
        VERB,
        STATUS,
        json_template(BODY),
    )
    .await;
    deploy(gateway, &api, stage).await;
    api
}

/// Write `settings` on the stage `stage` of `api`, each under the row's own `op`, asserting the
/// configuration was accepted.
async fn configure(gateway: &Gateway, api: &str, stage: &str, case: &str, row: &Row) {
    let operations: Vec<_> = row
        .settings
        .iter()
        .map(|(path, value)| json!({ "op": row.op, "path": path, "value": value }))
        .collect();
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{stage}"),
            Some(json!({ "patchOperations": operations })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected UpdateStage to store {:?}, got {} with {:?}",
        row.settings, answer.status, answer.body
    );
}

/// RULE: the stage's execution log group is there as soon as a configuration turning execution
/// logging on is accepted, and only for such a configuration — in both directions, with no
/// request served.
#[tokio::test]
async fn turning_execution_logging_on_publishes_the_stages_log_group_before_any_request() {
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;

    for (rank, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One api per row, so the group each row is judged by is its own.
        let api = deployed(&gateway, &format!("execution-log-group-{rank}"), STAGE).await;
        let group = execution_log_group(&api, STAGE);
        assert_eq!(
            delivered(&gateway.router, &group).await,
            None,
            "case {case:?}: expected no execution log group {group:?} before the stage is \
             configured at all"
        );

        configure(&gateway, &api, STAGE, case, row).await;

        let reported = delivered(&gateway.router, &group).await;
        if row.published {
            assert_eq!(
                reported,
                Some(Vec::new()),
                "case {case:?}: expected API Gateway to publish {group:?}, holding no record \
                 yet, got {reported:?}"
            );
        } else {
            assert_eq!(
                reported, None,
                "case {case:?}: expected no execution log group {group:?} for a configuration \
                 that turns execution logging on for nothing, got {reported:?}"
            );
        }
    }
}

/// RULE, the part about WHOSE group it is: the group names the api and the stage, so turning
/// logging on for one stage of an api publishes that stage's group and no other's.
#[tokio::test]
async fn each_stage_publishes_its_own_group_and_leaves_the_other_stages_alone() {
    let other_stage = "spare";
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = deployed(&gateway, "execution-log-group-per-stage", STAGE).await;
    deploy(&gateway, &api, other_stage).await;

    let case = "INFO on one stage of an api that carries two";
    configure(
        &gateway,
        &api,
        STAGE,
        case,
        &Row {
            case,
            op: "replace",
            settings: &[("/*/*/logging/loglevel", "INFO")],
            published: true,
        },
    )
    .await;

    let configured = execution_log_group(&api, STAGE);
    let untouched = execution_log_group(&api, other_stage);
    assert_eq!(
        delivered(&gateway.router, &configured).await,
        Some(Vec::new()),
        "case {case:?}: expected the configured stage's group {configured:?} to be published"
    );
    assert_eq!(
        delivered(&gateway.router, &untouched).await,
        None,
        "case {case:?}: expected the other stage's group {untouched:?} to stay absent"
    );
}

/// RULE, the prerequisite: API Gateway publishes a stage's execution log group as the ACCOUNT's
/// CloudWatch role, so an account that has none has the configuration refused and no group
/// comes into existence.
#[tokio::test]
async fn an_account_with_no_cloudwatch_role_publishes_no_execution_log_group() {
    let gateway = gateway_with_iam_and_logs();
    let api = deployed(&gateway, "execution-log-group-no-role", STAGE).await;
    let case = "INFO asked for by an account carrying no CloudWatch role";
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({
                "patchOperations": [
                    { "op": "replace", "path": "/*/*/logging/loglevel", "value": "INFO" },
                ],
            })),
        )
        .await;
    assert_eq!(
        answer.status, 400,
        "case {case:?}: expected the configuration to be refused, got {} with {:?}",
        answer.status, answer.body
    );
    let group = execution_log_group(&api, STAGE);
    assert_eq!(
        delivered(&gateway.router, &group).await,
        None,
        "case {case:?}: expected no execution log group {group:?} for a refused configuration"
    );
    // And the request the stage serves meanwhile leaves nothing behind either: the stage logs
    // nothing at all while the account has no role.
    let answered = crate::stage_access_logging::invoke(
        &gateway.router,
        &api,
        STAGE,
        VERB,
        RESOURCE_PATH,
    )
    .await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the stage to serve its mocked method, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        delivered(&gateway.router, &group).await,
        None,
        "case {case:?}: expected {group:?} to stay absent after a request too"
    );
}
