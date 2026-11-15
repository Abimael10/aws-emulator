// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests pinning WHEN the account-level CloudWatch Logs role gates a stage's
//! execution logging: the precondition is read off the account as it stands AT THE MOMENT
//! of the configuration call, and it gates only the direction that turns execution logging
//! ON.
//!
//! Every case drives the real wire path — a `restJson1` HTTP request through the
//! multi-service router into the mounted service — so what is asserted is what a client
//! sees, not what a handler returns. The emulator, the request builder and the
//! `CreateRestApi` / `CreateDeployment` / `GetStage` / `UpdateAccount` helpers are this
//! crate's existing harness, reused from `rest_api_lifecycle`, `stage_lifecycle` and
//! `account_settings`; the refusal's sentence, the `UpdateStage` call and its two assertions
//! are reused from the sibling `stage_execution_logging_account_role` suite, so the two suites
//! cannot drift apart on what the refusal says.
//!
//! The RULE under test: API Gateway writes a method's execution log events under the
//! account-level CloudWatch Logs role, so each `UpdateStage` is judged against the role the
//! account carries when that call arrives — a patch that turns execution logging on for any
//! method scope is refused `BadRequestException` (HTTP 400) with the fixed sentence of that
//! error class and writes nothing at all while the account carries no role, while a patch
//! that turns execution logging off, lowers it to `OFF` or removes the setting again needs
//! no role and is applied. Taking the account's role away is a change to the ACCOUNT record
//! only: it neither rewrites nor withdraws the logging a stage was already configured with.
//!
//! The rows span what that generalises over: the method SCOPE (the stage-wide wildcard and a
//! named method), both settings that turn execution logging on, the published group-and-name
//! spelling and the `MethodSetting` member spelling of the level, where in a multi-op list
//! the gated op sits (first and last), and which form the dial-back takes (an explicit `OFF`,
//! a `false`, and a `remove` of the setting).
//!
//! Sources for the spellings: `MethodSetting$loggingLevel` is documented "Valid values are
//! OFF, ERROR, and INFO" and `MethodSetting$dataTraceEnabled` is the full request/response
//! data trace (`smithy_query(query="shape", shape="MethodSetting")`); the grouped patch
//! paths (`/{method_setting_key}/logging/loglevel`, `/logging/dataTrace`) are API Gateway's
//! published method-setting paths, and `Stage$methodSettings` documents the key as
//! `{resource-path}/{HTTP-METHOD}` or `*/*`.

use serde_json::{json, Value};

use crate::account_settings::{patch_account, role_arn};
use crate::rest_api_lifecycle::{create_api, Gateway};
// The refusal this suite drives, the account pointer it patches, the `replace` builder, the
// `UpdateStage` call and the two assertions are the sibling suite's: that suite states the
// precondition itself, this one states WHEN it is evaluated, so both must hold the one
// spelling of the sentence and of the call.
use crate::stage_execution_logging_account_role::{
    assert_role_required, replace, reported_role, update_stage, CLOUDWATCH_ROLE_PATH,
};
use crate::stage_lifecycle::{create_deployed_stage, get_stage};

/// The stage every row configures.
const STAGE: &str = "prod";

/// One expectation a `GetStage` body is read against: the member path to walk, and the value
/// at the end of it.
type Expectation = (Vec<&'static str>, Value);

/// One row: the configuration that turns execution logging on, what an accepted one makes
/// readable, the configuration that turns it back off, and what THAT makes readable.
struct Row {
    case: &'static str,
    /// The `patchOperations` that turn execution logging on for this row's scope — the list
    /// the account's role is the precondition of.
    enabling: Vec<Value>,
    /// What the stage reports once the enabling list has been applied.
    enabled: Vec<Expectation>,
    /// The `patchOperations` that turn this row's execution logging back off, which no
    /// account role is a precondition of.
    disabling: Vec<Value>,
    /// What the stage reports once the disabling list has been applied.
    disabled: Vec<Expectation>,
}

/// One `remove` of `path`.
fn remove(path: &str) -> Value {
    json!({ "op": "remove", "path": path })
}

/// The pointer into a `GetStage` body of `member` of the method settings reported under
/// `key`.
fn method_setting(key: &'static str, member: &'static str) -> Vec<&'static str> {
    vec!["methodSettings", key, member]
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "every method of the stage at INFO, lowered to OFF again",
            enabling: vec![replace("/*/*/logging/loglevel", "INFO")],
            enabled: vec![(method_setting("*/*", "loggingLevel"), json!("INFO"))],
            disabling: vec![replace("/*/*/logging/loglevel", "OFF")],
            disabled: vec![(method_setting("*/*", "loggingLevel"), json!("OFF"))],
        },
        Row {
            case: "every method at ERROR by the member spelling, removed again",
            enabling: vec![replace("/*/*/loggingLevel", "ERROR")],
            enabled: vec![(method_setting("*/*", "loggingLevel"), json!("ERROR"))],
            disabling: vec![remove("/*/*/logging/loglevel")],
            // The level is gone, and the ENTRY it was removed from is still there — which is
            // what distinguishes a removed setting from a stage that lost its method settings
            // altogether: `metricsEnabled` is the member such an entry carries unset.
            disabled: vec![
                (method_setting("*/*", "loggingLevel"), Value::Null),
                (method_setting("*/*", "metricsEnabled"), json!(false)),
            ],
        },
        Row {
            case: "one named method at INFO, lowered to OFF again",
            enabling: vec![replace("/~1pets/GET/logging/loglevel", "INFO")],
            enabled: vec![(method_setting("~1pets/GET", "loggingLevel"), json!("INFO"))],
            disabling: vec![replace("/~1pets/GET/logging/loglevel", "OFF")],
            disabled: vec![(method_setting("~1pets/GET", "loggingLevel"), json!("OFF"))],
        },
        Row {
            case: "the data trace of every method, turned off again",
            enabling: vec![replace("/*/*/logging/dataTrace", "true")],
            enabled: vec![(method_setting("*/*", "dataTraceEnabled"), json!(true))],
            disabling: vec![replace("/*/*/logging/dataTrace", "false")],
            disabled: vec![(method_setting("*/*", "dataTraceEnabled"), json!(false))],
        },
        Row {
            case: "the gated op FIRST in a list whose other op the stage would apply",
            enabling: vec![
                replace("/*/*/logging/loglevel", "INFO"),
                replace("/description", "logging on"),
            ],
            enabled: vec![
                (method_setting("*/*", "loggingLevel"), json!("INFO")),
                (vec!["description"], json!("logging on")),
            ],
            disabling: vec![
                replace("/*/*/logging/loglevel", "OFF"),
                replace("/description", "logging off"),
            ],
            disabled: vec![
                (method_setting("*/*", "loggingLevel"), json!("OFF")),
                (vec!["description"], json!("logging off")),
            ],
        },
        Row {
            case: "the gated op LAST in a list whose other op the stage would apply",
            enabling: vec![
                replace("/*/*/metrics/enabled", "true"),
                replace("/*/*/logging/dataTrace", "true"),
            ],
            enabled: vec![
                (method_setting("*/*", "metricsEnabled"), json!(true)),
                (method_setting("*/*", "dataTraceEnabled"), json!(true)),
            ],
            disabling: vec![
                replace("/*/*/logging/dataTrace", "false"),
                replace("/*/*/metrics/enabled", "false"),
            ],
            disabled: vec![
                (method_setting("*/*", "metricsEnabled"), json!(false)),
                (method_setting("*/*", "dataTraceEnabled"), json!(false)),
            ],
        },
    ]
}

/// The stage as `GetStage` reports it, asserting the modelled 200.
async fn stage(gateway: &Gateway, api: &str, case: &str, step: &str) -> Value {
    let answer = get_stage(gateway, api, STAGE).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?} at {step}: expected 200 from GetStage, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// Assert the stage body reports every expectation of `expectations`.
fn assert_reports(body: &Value, expectations: &[Expectation], case: &str, step: &str) {
    for (path, expected) in expectations {
        let mut actual = body;
        for member in path {
            actual = &actual[*member];
        }
        assert_eq!(
            actual, expected,
            "case {case:?} at {step}: expected {path:?} to be {expected:?}, got {actual:?}"
        );
    }
}

/// RULE: each `UpdateStage` is judged against the role the account carries when it arrives —
/// so the configuration accepted with a role is refused once that role is taken away, the
/// settings it already wrote stay exactly as they were, and the patch that turns execution
/// logging back off is applied with no role at all.
#[tokio::test]
async fn the_logging_role_gates_each_configuration_call_as_the_account_stands_then() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // One emulator per row, so the account another row configured cannot decide this
        // row's answers.
        let gateway = Gateway::new();
        let (api, _root) = create_api(&gateway, &format!("logging-role-now-{index}")).await;
        create_deployed_stage(&gateway, &api, STAGE).await;

        // 1. With the account's role established, the configuration that turns execution
        //    logging on is applied, and reads back through an independent GetStage.
        let configured = role_arn(&format!("apigw-logs-{index}"));
        patch_account(&gateway, "replace", CLOUDWATCH_ROLE_PATH, Some(&configured)).await;
        let applied = update_stage(&gateway, &api, STAGE, &row.enabling).await;
        assert_eq!(
            applied.status, 200,
            "case {case:?} at the enabling patch: expected 200 with the account role set, got \
             {} with {:?}",
            applied.status, applied.body
        );
        let enabled = stage(&gateway, &api, case, "the read after enabling").await;
        assert_reports(&enabled, &row.enabled, case, "the read after enabling");

        // 2. Taking the role away is a change to the ACCOUNT alone: the stage keeps reporting
        //    the configuration it was given.
        patch_account(&gateway, "replace", CLOUDWATCH_ROLE_PATH, Some("")).await;
        assert_eq!(
            reported_role(&gateway).await,
            Value::Null,
            "case {case:?}: expected the cleared account to report no role"
        );
        let after_clearing = stage(&gateway, &api, case, "the read after clearing the role").await;
        assert_eq!(
            after_clearing, enabled,
            "case {case:?}: expected clearing the account role to leave the stage at {enabled:?}, \
             got {after_clearing:?}"
        );

        // 3. And the very same configuration, re-issued against the account as it stands NOW,
        //    is refused — writing nothing, every op of the list included.
        let refused = update_stage(&gateway, &api, STAGE, &row.enabling).await;
        assert_role_required(&refused, &format!("{case:?} at the re-issued enabling patch"));
        let after_refusal = stage(&gateway, &api, case, "the read after the refusal").await;
        assert_eq!(
            after_refusal, after_clearing,
            "case {case:?}: expected the refused patch to leave the stage at {after_clearing:?}, \
             got {after_refusal:?}"
        );

        // 4. Turning execution logging back off is the other DIRECTION, which the role is no
        //    precondition of: it is applied on an account that carries none.
        let disabled = update_stage(&gateway, &api, STAGE, &row.disabling).await;
        assert_eq!(
            disabled.status, 200,
            "case {case:?} at the disabling patch: expected 200 on a role-less account, got {} \
             with {:?}",
            disabled.status, disabled.body
        );
        let read = stage(&gateway, &api, case, "the read after disabling").await;
        assert_reports(&read, &row.disabled, case, "the read after disabling");

        // 5. The gate is the account's CURRENT setting rather than a one-off: the enabling
        //    list is refused again, and the stage stays where the dial-back left it — so no op
        //    of the refused list, gated or not, reached the record.
        let refused_again = update_stage(&gateway, &api, STAGE, &row.enabling).await;
        assert_role_required(
            &refused_again,
            &format!("{case:?} at the enabling patch after the dial-back"),
        );
        let unchanged = stage(&gateway, &api, case, "the read after the second refusal").await;
        assert_eq!(
            unchanged, read,
            "case {case:?}: expected the second refusal to leave the stage at {read:?}, got \
             {unchanged:?}"
        );
        assert_reports(
            &unchanged,
            &row.disabled,
            case,
            "the read after the second refusal",
        );

        // 6. Restoring a role lets the same configuration through again, which is what makes
        //    the refusals above a precondition rather than a terminal state of the stage.
        patch_account(&gateway, "replace", CLOUDWATCH_ROLE_PATH, Some(&configured)).await;
        let accepted = update_stage(&gateway, &api, STAGE, &row.enabling).await;
        assert_eq!(
            accepted.status, 200,
            "case {case:?} at the enabling patch after restoring the role: expected 200, got {} \
             with {:?}",
            accepted.status, accepted.body
        );
        let restored = stage(&gateway, &api, case, "the read after restoring the role").await;
        assert_reports(
            &restored,
            &row.enabled,
            case,
            "the read after restoring the role",
        );
    }
}
