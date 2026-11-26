// SPDX-License-Identifier: CC0-1.0

//! AgentCore Control resolves an online evaluation's execution role against IAM.
//!
//! RULE: an online evaluation RUNS AS the `evaluationExecutionRoleArn` the caller names, so
//! every write that stores one resolves that role against this account's IAM BEFORE anything
//! is persisted and refuses the request with the operation's modelled `ValidationException`
//! (HTTP 400) — in the fixed sentence `The provided execution role cannot be assumed. Please
//! verify the role exists and its trust policy allows this service to assume it.` — unless
//! IAM holds the role and its trust policy lets this service's principal
//! (`bedrock-agentcore.amazonaws.com`, the model's own `awsService.cloudTrailEventSource`)
//! assume it. A refusal has to be grounded in what IAM ANSWERED, so a role ARN of another
//! account — whose IAM this emulator is not the one to ask — is stored rather than refused on
//! an assumption.
//!
//! The role is resolved through the in-process callback, so a single-service router has no
//! IAM to ask: that is why this suite lives here rather than in the service's own crate. The
//! sibling suite `bedrock_agentcore_execution_role` pins the same rule for an agent runtime's
//! `roleArn`; this one pins it for the evaluation role, which has its own refusal sentence.
//!
//! The rows span what the rule generalises over: whether the role was ever created, what its
//! trust policy says (this service, another service, another action, an explicit `Deny`,
//! `Action` and `Principal.Service` as lists), which account the ARN names, and which
//! operation stores the role (`CreateOnlineEvaluationConfig`,
//! `UpdateOnlineEvaluationConfig`).
//!
//! Sources: `smithy_query(query="operation",operation="CreateOnlineEvaluationConfig,\
//! UpdateOnlineEvaluationConfig,GetOnlineEvaluationConfig")` for the members, the status
//! codes and `ValidationException` at HTTP 400, and
//! `smithy_query(query="service_info")` for the service principal.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The AgentCore restJson1 caller and the refusal readers this test crate already owns, so
// the AgentCore planes are called one way across every suite here.
use crate::bedrock_agentcore_data_plane_parent_resources::{call, error_code, message, REGION};
// The awsQuery caller IAM is driven through, already owned by `all/groundstation_recording_
// bucket_role.rs`: one `CreateRole` is one form-encoded POST whoever asks, so the encoder and
// the call are shared rather than written again here.
use crate::groundstation_recording_bucket_role::iam;

/// The account id every emulated ARN carries (`engine::emulator_account_id`).
const ACCOUNT: &str = "000000000000";

/// The service principal an AgentCore execution role's trust policy has to admit.
const SERVICE_PRINCIPAL: &str = "bedrock-agentcore.amazonaws.com";

/// The whole sentence a write whose evaluation execution role this service cannot assume is
/// refused with — fixed text, with no part of the request in it.
const ROLE_MESSAGE: &str = "The provided execution role cannot be assumed. Please verify the \
                            role exists and its trust policy allows this service to assume it.";

/// How long a wait for the clock-owed `ACTIVE` keeps looking, and how long it waits between
/// looks. GENEROUS on purpose: what is waited for is a condition, never an elapsed time.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);
const SETTLE_POLL: Duration = Duration::from_millis(25);

/// AgentCore Control and IAM over ONE shared engine, as `router/src/lib.rs` mounts them, so
/// the control plane's in-process callback reaches this very IAM.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol::EmulatedService::new(
            engine,
        ),
    );
    router
}

fn text(body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The ARN of a role of this account, under the default path.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// A trust policy with one statement, from the pieces that vary between rows: the effect, the
/// action(s) and the service principal(s), each either a string or a list, as IAM admits.
fn trust_policy(effect: &str, action: Value, service: Value) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": {"Service": service},
            "Action": action
        }]
    })
    .to_string()
}

/// A trust policy with TWO statements about the same principal and action: one `Allow` and
/// one explicit `Deny`. IAM's explicit deny wins, so the role cannot be assumed.
fn allow_then_deny_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {"Effect": "Allow", "Principal": {"Service": SERVICE_PRINCIPAL},
             "Action": "sts:AssumeRole"},
            {"Effect": "Deny", "Principal": {"Service": SERVICE_PRINCIPAL},
             "Action": "sts:AssumeRole"}
        ]
    })
    .to_string()
}

/// Every role the rows name, with the trust document it carries.
fn role_fixtures() -> Vec<(&'static str, String)> {
    vec![
        (
            "evaluation-role",
            trust_policy(
                "Allow",
                json!("sts:AssumeRole"),
                json!(SERVICE_PRINCIPAL),
            ),
        ),
        (
            "evaluation-role-lists",
            trust_policy(
                "Allow",
                json!(["sts:TagSession", "sts:AssumeRole"]),
                json!(["lambda.amazonaws.com", SERVICE_PRINCIPAL]),
            ),
        ),
        (
            "evaluation-role-wildcard",
            trust_policy("Allow", json!("sts:*"), json!(SERVICE_PRINCIPAL)),
        ),
        (
            "lambda-only-role",
            trust_policy(
                "Allow",
                json!("sts:AssumeRole"),
                json!("lambda.amazonaws.com"),
            ),
        ),
        (
            "other-action-role",
            trust_policy("Allow", json!("s3:GetObject"), json!(SERVICE_PRINCIPAL)),
        ),
        (
            "denied-role",
            trust_policy("Deny", json!("sts:AssumeRole"), json!(SERVICE_PRINCIPAL)),
        ),
        ("allow-then-deny-role", allow_then_deny_policy()),
    ]
}

async fn create_roles(router: &MultiServiceRouter) {
    for (name, document) in role_fixtures() {
        let (status, answered) = iam(
            router,
            "CreateRole",
            &[
                ("RoleName", name),
                ("AssumeRolePolicyDocument", document.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: CreateRole {name:?} must succeed, got {status} {answered}"
        );
    }
}

/// One row: the execution role a write names, and whether the rule admits it.
struct Row {
    case: &'static str,
    /// The name the configuration of this row is created under
    /// (`EvaluationConfigName` is `^[a-zA-Z][a-zA-Z0-9_]{0,47}$`).
    name: &'static str,
    role_arn: String,
    accepted: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a role that exists and trusts this service",
            name: "trustedRole",
            role_arn: role_arn("evaluation-role"),
            accepted: true,
        },
        Row {
            case: "a trust policy whose Action and Service are lists",
            name: "trustedLists",
            role_arn: role_arn("evaluation-role-lists"),
            accepted: true,
        },
        Row {
            case: "a trust policy allowing the whole sts namespace",
            name: "trustedWildcard",
            role_arn: role_arn("evaluation-role-wildcard"),
            accepted: true,
        },
        Row {
            // Another account's IAM is not this emulator's to ask, so nothing is known about
            // the role and the create is not refused on an assumption.
            case: "a role of another account",
            name: "foreignRole",
            role_arn: "arn:aws:iam::123456789012:role/evaluation-role".to_string(),
            accepted: true,
        },
        Row {
            case: "a role that was never created",
            name: "absentRole",
            role_arn: role_arn("never-created-role"),
            accepted: false,
        },
        Row {
            case: "a role that trusts another service only",
            name: "lambdaRole",
            role_arn: role_arn("lambda-only-role"),
            accepted: false,
        },
        Row {
            case: "a role whose statement names another action",
            name: "otherActionRole",
            role_arn: role_arn("other-action-role"),
            accepted: false,
        },
        Row {
            case: "a role whose statement denies the assume",
            name: "deniedRole",
            role_arn: role_arn("denied-role"),
            accepted: false,
        },
        Row {
            case: "a role that both allows and explicitly denies the assume",
            name: "allowThenDenyRole",
            role_arn: role_arn("allow-then-deny-role"),
            accepted: false,
        },
    ]
}

/// A create naming `role_arn`, with the one evaluation subject a create has to name.
fn create_body(name: &str, role_arn: &str) -> Value {
    json!({
        "onlineEvaluationConfigName": name,
        "rule": {"samplingConfig": {"samplingPercentage": 10.0}},
        "dataSourceConfig": {"cloudWatchLogs": {
            "serviceNames": ["agentservice"],
            "logGroupNames": ["/aws/agentcore/execution-role"]
        }},
        "evaluators": [{"evaluatorId": "Builtin.Correctness"}],
        "evaluationExecutionRoleArn": role_arn,
        "enableOnCreate": false
    })
}

async fn read_config(router: &MultiServiceRouter, id: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("/online-evaluation-configs/{id}"),
        None,
    )
    .await
}

/// Look again until the configuration reports `ACTIVE`, which is what a caller polling the
/// status field does: an update is refused while the create's own work is still under way.
/// The deadline only catches a configuration that never settles at all.
async fn poll_until_active(router: &MultiServiceRouter, id: &str, case: &str) {
    let started = Instant::now();
    loop {
        let (status, body) = read_config(router, id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the read of {id} is served while polling for ACTIVE, got {status} \
             ({body})"
        );
        if text(&body, "status") == "ACTIVE" {
            return;
        }
        assert!(
            started.elapsed() < SETTLE_DEADLINE,
            "case {case:?}: {id} never reported ACTIVE, last read {body}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

#[tokio::test]
async fn a_config_is_written_only_for_a_role_this_service_may_assume() {
    let router = emulator();
    create_roles(&router).await;

    for row in rows() {
        let case = row.case;
        let (status, answered) = call(
            &router,
            "POST",
            "/online-evaluation-configs/create",
            Some(create_body(row.name, &row.role_arn)),
        )
        .await;

        if row.accepted {
            assert_eq!(
                status, 202,
                "case {case:?} ({}): CreateOnlineEvaluationConfig must succeed, got {status} \
                 ({answered})",
                row.role_arn
            );
            // The stored configuration carries the role it was given, read back through the
            // service's own reader rather than off the create's envelope.
            let id = text(&answered, "onlineEvaluationConfigId");
            let (status, read) = read_config(&router, &id).await;
            assert_eq!(
                (status, text(&read, "evaluationExecutionRoleArn").as_str()),
                (200, row.role_arn.as_str()),
                "case {case:?}: the stored configuration must carry the role it was given, got \
                 {status} ({read})"
            );
            continue;
        }

        assert_eq!(
            (status, error_code(&answered).as_str()),
            (400, "ValidationException"),
            "case {case:?} ({}): a role this service cannot assume must be refused 400 \
             ValidationException, got {status} {} ({answered})",
            row.role_arn,
            error_code(&answered)
        );
        assert_eq!(
            message(&answered),
            ROLE_MESSAGE,
            "case {case:?}: got {answered}"
        );
        assert!(
            answered.get("onlineEvaluationConfigId").is_none(),
            "case {case:?}: a refused create mints nothing, got {answered}"
        );

        // The refused create reserved nothing: the same name, with a role this service may
        // assume, is accepted.
        let (status, created) = call(
            &router,
            "POST",
            "/online-evaluation-configs/create",
            Some(create_body(row.name, &role_arn("evaluation-role"))),
        )
        .await;
        assert_eq!(
            status, 202,
            "case {case:?}: the refused create reserved nothing, so the same name with an \
             assumable role is accepted, got {status} ({created})"
        );
    }
}

/// `UpdateOnlineEvaluationConfig` applies the same gate, and a refused update leaves the
/// stored configuration exactly as it was.
#[tokio::test]
async fn a_refused_update_leaves_the_stored_role_alone() {
    let router = emulator();
    create_roles(&router).await;

    let held = role_arn("evaluation-role");
    let (status, created) = call(
        &router,
        "POST",
        "/online-evaluation-configs/create",
        Some(create_body("updatedConfig", &held)),
    )
    .await;
    assert_eq!(
        status, 202,
        "setup: CreateOnlineEvaluationConfig must succeed, got {status} ({created})"
    );
    let id = text(&created, "onlineEvaluationConfigId");

    // (case, the role the update names, the status it must answer)
    let updates: [(&str, String, u16); 4] = [
        (
            "an update onto a role that was never created",
            role_arn("never-created-role"),
            400,
        ),
        (
            "an update onto a role that trusts another service only",
            role_arn("lambda-only-role"),
            400,
        ),
        (
            "an update onto a role whose statement denies the assume",
            role_arn("denied-role"),
            400,
        ),
        (
            "an update onto a role this service may assume",
            role_arn("evaluation-role-lists"),
            202,
        ),
    ];

    let mut stored = held.clone();
    for (case, role, expected_status) in updates {
        poll_until_active(&router, &id, case).await;
        let (status, answered) = call(
            &router,
            "PUT",
            &format!("/online-evaluation-configs/{id}"),
            Some(json!({"evaluationExecutionRoleArn": role})),
        )
        .await;
        assert_eq!(
            status, expected_status,
            "case {case:?} ({role}): expected HTTP {expected_status}, got {status} ({answered})"
        );
        if expected_status == 400 {
            assert_eq!(
                (error_code(&answered).as_str(), message(&answered).as_str()),
                ("ValidationException", ROLE_MESSAGE),
                "case {case:?}: expected the modelled refusal, got {answered}"
            );
        } else {
            stored = role.clone();
        }

        // Whatever the answer, the stored role is the last one an ACCEPTED write put there.
        let (status, read) = read_config(&router, &id).await;
        assert_eq!(
            (status, text(&read, "evaluationExecutionRoleArn").as_str()),
            (200, stored.as_str()),
            "case {case:?}: a refused update must leave the stored role alone, got {status} \
             ({read})"
        );
    }

    // The configuration is still the one the create minted, in the region the calls are
    // signed for.
    let (_, read) = read_config(&router, &id).await;
    assert_eq!(
        text(&read, "onlineEvaluationConfigArn"),
        format!("arn:aws:bedrock-agentcore:{REGION}:{ACCOUNT}:online-evaluation-config/{id}"),
        "the updates left the configuration's identity alone, got {read}"
    );
}
