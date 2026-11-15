// SPDX-License-Identifier: CC0-1.0

//! WHICH destinations a stage's access logging actually writes into: the log group CloudWatch
//! Logs already holds, and no other.
//!
//! The rule under test, one level above the recorded case:
//!
//! > A stage's `accessLogSettings` `destinationArn` names a log group of the ACCOUNT's, so
//! > delivering a record into it CREATES no group: a stage pointed at a group CloudWatch Logs
//! > holds leaves one record per request it answered in that group, and a stage pointed at a
//! > name CloudWatch Logs holds no group under delivers nothing and leaves the account holding
//! > no such group — however many requests the stage goes on serving, and whichever published
//! > form of the ARN named it. The stage itself is unaffected either way: it answers its
//! > traffic exactly as it would with no logging configured at all. The only group API Gateway
//! > brings into existence is the one it publishes under its OWN name, the stage's execution
//! > log group `API-Gateway-Execution-Logs_{api-id}/{stage}`.
//!
//! Only a cross-service suite can pin this: the api, its stage and the logging configuration
//! are API Gateway's, the role the account logs under is AWS IAM's, and whether a group exists
//! at all is CloudWatch Logs' — all three on ONE engine, as `core/router/src/lib.rs` mounts
//! them. Every assertion reads the outcome back through CloudWatch Logs' own
//! `FilterLogEvents`, which answers `ResourceNotFoundException` for a group the account does
//! not hold, so a group that was conjured up by a delivery cannot pass.
//!
//! The rows differ along what the rule generalises over: whether the destination group EXISTS,
//! which published form of the log-group ARN named it (the group itself and everything under
//! it), the shape of the group's name, and how many requests the stage served under the
//! configuration.

use serde_json::json;

use crate::account_logging_role::{create_role, replace_role, role_arn, trusting};
use crate::execute_api_data_plane::{deploy, json_template, mock_resource};
use crate::rest_api_lifecycle::{create_api, gateway_with_iam_and_logs, Gateway};
use crate::stage_access_logging::{create_log_group, delivered, invoke};

/// The Region every ARN below names, which is the one the harness signs and invokes in.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, the path part of its one resource and the path a request
/// addresses it by.
const STAGE: &str = "live";
const PATH_PART: &str = "ping";
const RESOURCE_PATH: &str = "/ping";

/// The role the account writes its logs as, and the principal its trust policy has to admit
/// for API Gateway to be able to write at all.
/// The NAME is `pub(crate)` because the sibling suite that embeds an inline policy on this same
/// role ([`crate::stage_access_log_denied_destination`]) has to name it; the path and the
/// principal are read only by [`install_account_role`].
pub(crate) const ROLE_NAME: &str = "apigw-absent-destination";
const ROLE_PATH: &str = "/service-role/";
const SERVICE_PRINCIPAL: &str = "apigateway.amazonaws.com";

/// The one `MOCK` method the stage serves — "for testing without actually invoking the
/// backend" (`IntegrationType`) — so what is delivered is the stage's own doing and no
/// backend's.
const VERB: &str = "GET";
const STATUS: &str = "200";
const BODY: &str = r#"{"pong":true}"#;

/// The format every row renders by; the rule under test is about the DESTINATION, so one
/// format carrying the request's own id is enough for every row.
const FORMAT: &str = "$context.requestId $context.httpMethod $context.status";

/// One row: the destination a stage is pointed at, whether CloudWatch Logs holds that group
/// already, and how many requests are served under it.
struct Row {
    case: &'static str,
    /// The log group the `destinationArn` names.
    group: &'static str,
    /// Whether the group is created — by the account, through CloudWatch Logs' own
    /// `CreateLogGroup` — before the stage is pointed at it.
    exists: bool,
    /// Whether the `destinationArn` names everything UNDER the group (the `:*` form) rather
    /// than the group itself: both published forms name the same group.
    everything_under: bool,
    /// How many requests the stage serves under this configuration.
    requests: usize,
}

/// The rows: a destination that exists in each ARN form, and absent destinations in each ARN
/// form, with one and with several requests served under them.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a destination the account holds, named by the group's own arn",
            group: "/apigw/absent/held",
            exists: true,
            everything_under: false,
            requests: 1,
        },
        Row {
            case: "a destination the account holds, named by everything under the group",
            group: "/apigw/absent/held-everything-under",
            exists: true,
            everything_under: true,
            requests: 2,
        },
        Row {
            case: "a destination the account holds no group under",
            group: "/apigw/absent/missing",
            exists: false,
            everything_under: false,
            requests: 1,
        },
        Row {
            case: "an absent destination named by everything under the group",
            group: "/apigw/absent/missing-everything-under",
            exists: false,
            everything_under: true,
            requests: 1,
        },
        Row {
            case: "an absent destination served several requests, each of them lost",
            group: "apigw-absent-flat-name",
            exists: false,
            everything_under: false,
            requests: 3,
        },
    ]
}

/// The ARN of the log group `name`, as a stage's `destinationArn` names one: the group itself,
/// or — with `everything_under` — everything under it.
///
/// `pub(crate)` because the sibling suite about the SAME destinations beside a delivering stage
/// ([`crate::stage_access_log_denied_destination`]) names them the same way: one reading, so two
/// suites cannot point a stage at two different ARN forms while believing they agree.
pub(crate) fn destination_arn(name: &str, everything_under: bool) -> String {
    let account = engine::emulator_account_id();
    let everything = if everything_under { ":*" } else { "" };
    format!("arn:aws:logs:{REGION}:{account}:log-group:{name}{everything}")
}

/// Install the account's CloudWatch role through AWS IAM's own `CreateRole` and API Gateway's
/// own `UpdateAccount`, which is what API Gateway writes the account's logs as: with it in
/// place, nothing but the destination decides whether a record lands.
///
/// `pub(crate)` for the reason [`destination_arn`] gives: it is the same prerequisite for every
/// suite about what a stage DELIVERS, and one copy is what keeps them installing one role.
pub(crate) async fn install_account_role(gateway: &Gateway) {
    create_role(gateway, ROLE_NAME, ROLE_PATH, &trusting(SERVICE_PRINCIPAL)).await;
    let installed = replace_role(gateway, &role_arn(ROLE_PATH, ROLE_NAME)).await;
    assert_eq!(
        installed.status, 200,
        "setup UpdateAccount /cloudwatchRoleArn: expected 200, got {} with {:?}",
        installed.status, installed.body
    );
}

/// Point the stage's access logging at `group`, in the ARN form the row asks for.
async fn configure_access_logging(gateway: &Gateway, api: &str, case: &str, row: &Row) {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({
                "patchOperations": [
                    {
                        "op": "replace",
                        "path": "/accessLogSettings/destinationArn",
                        "value": destination_arn(row.group, row.everything_under),
                    },
                    { "op": "replace", "path": "/accessLogSettings/format", "value": FORMAT },
                ],
            })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected UpdateStage to store the logging configuration whether or not \
         the destination exists, got {} with {:?}",
        answer.status, answer.body
    );
}

/// An api with one `MOCK` resource, deployed to the stage. Answers the api id.
async fn deployed(gateway: &Gateway, name: &str) -> String {
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
    deploy(gateway, &api, STAGE).await;
    api
}

/// RULE: a record lands in the destination the account HOLDS, and a destination the account
/// holds no group under takes none and is not created by the delivery.
#[tokio::test]
async fn a_record_reaches_a_destination_that_exists_and_an_absent_one_is_never_created() {
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = deployed(&gateway, "access-log-absent-destination").await;

    for row in rows() {
        let case = row.case;
        if row.exists {
            create_log_group(&gateway.router, row.group).await;
        }
        configure_access_logging(&gateway, &api, case, &row).await;

        let mut answered_ids: Vec<String> = Vec::new();
        for request in 0..row.requests {
            let answered = invoke(&gateway.router, &api, STAGE, VERB, RESOURCE_PATH).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: request {request} expected the stage to serve its traffic \
                 whatever the destination, got {} with {:?}",
                answered.status,
                answered.text()
            );
            answered_ids.push(
                answered
                    .request_id
                    .clone()
                    .unwrap_or_else(|| panic!("case {case:?}: the answer named no request id")),
            );
        }

        let reported = delivered(&gateway.router, row.group).await;
        if !row.exists {
            assert_eq!(
                reported, None,
                "case {case:?}: expected CloudWatch Logs to hold no group {:?} at all after \
                 {} request(s), got {reported:?}",
                row.group, row.requests
            );
            continue;
        }
        let records = reported.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the destination {:?} the account created to still be \
                 there",
                row.group
            )
        });
        let expected: Vec<String> = answered_ids
            .iter()
            .map(|request_id| format!("{request_id} {VERB} 200"))
            .collect();
        assert_eq!(
            records, expected,
            "case {case:?}: expected one record per request served in {:?}, got {records:?}",
            row.group
        );
    }
}

/// RULE, the other half: the group API Gateway publishes under its OWN name — the stage's
/// execution log group — IS created by the delivery, while the access-log destination the same
/// stage was pointed at stays absent.
#[tokio::test]
async fn the_stages_own_execution_log_group_is_created_while_the_destination_stays_absent() {
    let gateway = gateway_with_iam_and_logs();
    install_account_role(&gateway).await;
    let api = deployed(&gateway, "access-log-absent-execution").await;
    let case = "an absent destination beside execution logging turned on";
    let group = "/apigw/absent/execution-beside";
    configure_access_logging(
        &gateway,
        &api,
        case,
        &Row {
            case,
            group,
            exists: false,
            everything_under: false,
            requests: 1,
        },
    )
    .await;
    // `loggingLevel` INFO on every method of the stage, which is what turns the execution log
    // on ("specifies the logging level for this method", `MethodSetting$loggingLevel`).
    let logging = gateway
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
        logging.status, 200,
        "case {case:?}: expected the method setting to be stored, got {} with {:?}",
        logging.status, logging.body
    );

    let answered = invoke(&gateway.router, &api, STAGE, VERB, RESOURCE_PATH).await;
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected the stage to serve the mocked method, got {} with {:?}",
        answered.status,
        answered.text()
    );

    let execution = format!("API-Gateway-Execution-Logs_{api}/{STAGE}");
    let traced = delivered(&gateway.router, &execution)
        .await
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected API Gateway to publish its own execution log group \
                 {execution:?}"
            )
        });
    assert!(
        !traced.is_empty(),
        "case {case:?}: expected the trace of the request in {execution:?}, got {traced:?}"
    );
    assert_eq!(
        delivered(&gateway.router, group).await,
        None,
        "case {case:?}: expected the access-log destination {group:?} to stay absent"
    );
}
