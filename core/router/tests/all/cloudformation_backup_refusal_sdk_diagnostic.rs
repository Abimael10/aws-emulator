// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation resource whose create AWS Backup REFUSES reports that refusal as the
//! exception that service's own SDK raises, inside CloudFormation's handler envelope.
//!
//! The rule under test: **the `ResourceStatusReason` of a resource its provider could not
//! create carries the refused call's SDK exception message — the owning service's own
//! sentence followed by the diagnostic that service's SDK appends to it, which names the
//! service by the name its SDK client carries, the HTTP status it answered, the id of the
//! call and how many attempts the SDK made at it — quoted inside the provider envelope
//! `Resource handler returned message: "…" (RequestToken: …, HandlerErrorCode: …)`.** Two
//! crates have to agree for that to hold (CloudFormation makes the call and wraps the
//! refusal, AWS Backup words it), so the suite lives here rather than in either crate.
//!
//! The rows differ along what the rule generalises over: which `AWS::Backup::*` type the
//! stack declares and therefore which call of AWS Backup's is refused, what the refusal is
//! about (a schedule that is not a cron expression, a schedule that is not one at all, a
//! lifecycle leaving too little time in cold storage, a plan past the rule cap, a key
//! reference that is not an ARN), the `HandlerErrorCode` class the refusal falls in and
//! therefore the attempt count the diagnostic closes with, the Region deployed in, and both
//! template spellings.
//!
//! Every expected value is computed here from what the row declares. The two values one run
//! MINTS — the id of the refused call and the token of the resource request — are per-run,
//! so they are checked for their FORM, for being distinct from each other, and for being
//! this call's own rather than a value shared between rows.
//!
//! The awsQuery form encoder and the XML readers are the test crate's own
//! ([`crate::query_wire`]), not another copy.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cfn_pipes_pipe_stabilisation_failure_class::{
    BEFORE_THE_TOKEN, CREATE_FAILED, ENVELOPE_OPENS,
};
use crate::query_wire::{
    elements, form_encode, is_minted_uuid, members_of, opt_element, outputs_of,
};

/// How long a wait for a stack to reach a TERMINAL status may run before the suite calls it
/// a failure. What is waited for is the CONDITION — the stack out of its `_IN_PROGRESS`
/// statuses — and the deadline is only a safety net so a wait that will never be satisfied
/// still ends. It is far above every clock-owed span involved, so a loaded host cannot
/// decide the outcome, and nothing here is asserted about elapsed time.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long to wait between two looks at a condition that is not satisfied yet.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

// The resource status a refused create is recorded under, and the two fixed fragments of the
// provider envelope, are the ones the suite that first read this envelope states
// (`crate::cfn_pipes_pipe_stabilisation_failure_class`): the envelope is CloudFormation's one
// shape, so a second reading of it would drift from the first.

/// The name AWS Backup's own SDK client reports that service under, which is what the
/// diagnostic of a refused call states: that model's own `sdkId`, which is also the
/// `cloudFormationName` the `AWS::Backup::*` type names carry
/// (`smithy_query(query="service_info")` on that service's model: `sdkId: "Backup"`,
/// `cloudFormationName: Backup`).
const SDK_SERVICE: &str = "Backup";

/// The HTTP status AWS Backup answers each of the refusals below with: every error of
/// `CreateBackupVault` and `CreateBackupPlan` is modelled HTTP 400
/// (`smithy_query(operation="CreateBackupVault,CreateBackupPlan")`).
const REFUSAL_STATUS: u16 = 400;

/// How many attempts an AWS SDK made at a refusal it does not retry — a refused REQUEST,
/// which cannot clear on its own — and how many it made at one it does, a condition that can
/// (an exhausted service limit).
const SINGLE_ATTEMPT: u32 = 1;
const RETRIED_ATTEMPTS: u32 = 3;

/// The `HandlerErrorCode` classes the refusals below fall in, as CloudFormation's own
/// `HandlerErrorCode` value set spells them.
const INVALID_REQUEST: &str = "InvalidRequest";
const SERVICE_LIMIT_EXCEEDED: &str = "ServiceLimitExceeded";

/// The sentence AWS Backup words for a plan rule whose `ScheduleExpression` it cannot read
/// as a cron expression: the per-rule validation envelope's fixed prefix followed by that
/// rule's reason, which for this condition is the parse failure and quotes nothing the
/// request sent.
const CRON_INVALID_MESSAGE: &str =
    "Error in some rules due to : Cron expression is not valid: Invalid cron expression";

/// The sentence it words for a `Lifecycle` whose deletion falls less than the required
/// interval after the transition to cold storage — fixed text naming the two members and
/// the interval, quoting neither day count (the model's own rule for `DeleteAfterDays`:
/// "must be at least 90 days after the number of days specified in
/// `MoveToColdStorageAfterDays`").
const LIFECYCLE_MESSAGE: &str = "Invalid lifecycle. DeleteAfterDays cannot be less than 90 \
                                 days apart from MoveToColdStorageAfterDays";

/// The sentence it words for a member of the request whose value it cannot accept, whose one
/// variable part is the member it rejected — here the vault's key reference, which that
/// model types as an ARN.
const INVALID_KEY_MEMBER: &str = "EncryptionKeyArn";
fn invalid_parameter_message(member: &str) -> String {
    format!("Parameter {member} is invalid.")
}

/// How many rules one backup plan may hold, and the sentence AWS Backup words for a body
/// past that cap — this emulator's own quota wording for the limit class, built from the cap
/// itself (that service's `MAX_RULES_PER_PLAN`).
const RULE_CAP: usize = 100;
fn rule_cap_message() -> String {
    format!("a backup plan may hold at most {RULE_CAP} rules")
}

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_backup_cryocontrollerusermanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// The stack's status once it is no longer `*_IN_PROGRESS`, with the `DescribeStacks` answer
/// it was read from: looked at again until it settles or [`TERMINAL_DEADLINE`] passes, so
/// what the suite waits for is the CONDITION rather than a span of the host's clock.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    stack_name: &str,
) -> (String, String) {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks({stack_name}) must answer 200, got {status} with body {described}"
        );
        let reached = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reached.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return (reached, described);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The provider envelope CloudFormation reports a resource provider's answer in, taken apart.
struct Envelope {
    /// The provider's own message, which is the exception the called service's SDK raised.
    message: String,
    /// The token of the resource request the provider answered.
    token: String,
    /// The `HandlerErrorCode` class the refusal was filed under.
    class: String,
}

/// The fixed text of that envelope that follows the token: the class the refusal was filed
/// under. What precedes it — the head of the envelope and the token that closes the quoted
/// message — is stated once for every suite that reads one ([`ENVELOPE_OPENS`],
/// [`BEFORE_THE_TOKEN`]).
const ENVELOPE_CLASS: &str = ", HandlerErrorCode: ";

/// One `ResourceStatusReason` read as that envelope, or `None` for a reason that is not one.
fn envelope_of(reason: &str) -> Option<Envelope> {
    let rest = reason.strip_prefix(ENVELOPE_OPENS)?;
    let (message, rest) = rest.split_once(BEFORE_THE_TOKEN)?;
    let (token, class) = rest.split_once(ENVELOPE_CLASS)?;
    Some(Envelope {
        message: message.to_string(),
        token: token.to_string(),
        class: class.strip_suffix(')')?.to_string(),
    })
}

/// The exception an AWS SDK raises for a refused call, taken apart: the called service's own
/// sentence, and the diagnostic that SDK appends to it.
struct SdkException {
    sentence: String,
    request_id: String,
    attempts: u32,
}

/// One SDK exception message read as that pair, or `None` for a message carrying no
/// diagnostic of the expected shape.
fn sdk_exception_of(message: &str) -> Option<SdkException> {
    let opens = format!(" (Service: {SDK_SERVICE}, Status Code: {REFUSAL_STATUS}, Request ID: ");
    let (sentence, rest) = message.split_once(&opens)?;
    let (request_id, rest) = rest.split_once(") (SDK Attempt Count: ")?;
    Some(SdkException {
        sentence: sentence.to_string(),
        request_id: request_id.to_string(),
        attempts: rest.strip_suffix(')')?.parse().ok()?,
    })
}

/// One row of the table: a stack whose declaration AWS Backup refuses, and what the failed
/// resource's reason must then carry.
struct Row {
    case: &'static str,
    /// The Region the row deploys in: the call that is refused is made there.
    region: &'static str,
    stack_name: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The logical id of the resource whose create must fail.
    failed: &'static str,
    /// AWS Backup's own sentence for the refusal.
    sentence: fn() -> String,
    /// The `HandlerErrorCode` class it falls in, and the attempt count its class implies.
    class: &'static str,
    attempts: u32,
}

/// The vault every plan row declares beside its plan, so the rule's `TargetBackupVault`
/// names a vault that really exists: that vault is created, the plan is refused, and the
/// rollback takes the vault with it.
fn plan_template(vault: &str, plan: &str, rule: Value) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Vault": {
                "Type": "AWS::Backup::BackupVault",
                "Properties": { "BackupVaultName": vault }
            },
            "Plan": {
                "Type": "AWS::Backup::BackupPlan",
                "Properties": {
                    "BackupPlan": {
                        "BackupPlanName": plan,
                        "BackupPlanRule": [rule]
                    }
                }
            }
        },
        "Outputs": {"PlanId": {"Value": {"Ref": "Plan"}}}
    })
    .to_string()
}

/// A plan declaring one more rule than a plan may hold, every rule well formed in its own
/// right: what the refusal is about is the SIZE of the list.
fn over_cap_template(vault: &str, plan: &str) -> String {
    let rules: Vec<Value> = (0..=RULE_CAP)
        .map(|index| {
            json!({
                "RuleName": format!("Rule-{index}"),
                "TargetBackupVault": {"Ref": "Vault"},
                "ScheduleExpression": "cron(0 5 ? * * *)"
            })
        })
        .collect();
    json!({
        "Resources": {
            "Vault": {
                "Type": "AWS::Backup::BackupVault",
                "Properties": { "BackupVaultName": vault }
            },
            "Plan": {
                "Type": "AWS::Backup::BackupPlan",
                "Properties": {
                    "BackupPlan": { "BackupPlanName": plan, "BackupPlanRule": rules }
                }
            }
        }
    })
    .to_string()
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a plan rule whose schedule carries the wrong number of cron fields",
            region: "us-west-2",
            stack_name: "bkp-reason-cron-fields",
            template: || {
                plan_template(
                    "bkp-reason-cron-fields-vault",
                    "bkp-reason-cron-fields-plan",
                    json!({
                        "RuleName": "Daily",
                        "TargetBackupVault": {"Ref": "Vault"},
                        "ScheduleExpression": "cron(0 12 * * ?)"
                    }),
                )
            },
            failed: "Plan",
            sentence: || CRON_INVALID_MESSAGE.to_string(),
            class: INVALID_REQUEST,
            attempts: SINGLE_ATTEMPT,
        },
        Row {
            case: "a plan rule whose schedule is not a cron expression at all",
            region: "us-east-1",
            stack_name: "bkp-reason-cron-rate",
            template: || {
                plan_template(
                    "bkp-reason-cron-rate-vault",
                    "bkp-reason-cron-rate-plan",
                    json!({
                        "RuleName": "Hourly",
                        "TargetBackupVault": {"Ref": "Vault"},
                        "ScheduleExpression": "rate(1 day)"
                    }),
                )
            },
            failed: "Plan",
            sentence: || CRON_INVALID_MESSAGE.to_string(),
            class: INVALID_REQUEST,
            attempts: SINGLE_ATTEMPT,
        },
        Row {
            case: "a plan rule whose lifecycle leaves too little time in cold storage, in YAML",
            region: "eu-west-1",
            stack_name: "bkp-reason-lifecycle",
            template: || {
                [
                    "Resources:",
                    "  Vault:",
                    "    Type: AWS::Backup::BackupVault",
                    "    Properties:",
                    "      BackupVaultName: bkp-reason-lifecycle-vault",
                    "  Plan:",
                    "    Type: AWS::Backup::BackupPlan",
                    "    Properties:",
                    "      BackupPlan:",
                    "        BackupPlanName: bkp-reason-lifecycle-plan",
                    "        BackupPlanRule:",
                    "          - RuleName: Daily",
                    "            TargetBackupVault: !Ref Vault",
                    "            ScheduleExpression: cron(0 5 ? * * *)",
                    "            Lifecycle:",
                    "              MoveToColdStorageAfterDays: 30",
                    "              DeleteAfterDays: 60",
                ]
                .join("\n")
            },
            failed: "Plan",
            sentence: || LIFECYCLE_MESSAGE.to_string(),
            class: INVALID_REQUEST,
            attempts: SINGLE_ATTEMPT,
        },
        Row {
            case: "a plan declaring more rules than one plan may hold",
            region: "ap-southeast-2",
            stack_name: "bkp-reason-rule-cap",
            template: || {
                over_cap_template("bkp-reason-rule-cap-vault", "bkp-reason-rule-cap-plan")
            },
            failed: "Plan",
            sentence: rule_cap_message,
            class: SERVICE_LIMIT_EXCEEDED,
            attempts: RETRIED_ATTEMPTS,
        },
        Row {
            case: "a vault whose key reference is not an ARN",
            region: "eu-central-1",
            stack_name: "bkp-reason-vault-key",
            template: || {
                json!({
                    "Resources": {
                        "Vault": {
                            "Type": "AWS::Backup::BackupVault",
                            "Properties": {
                                "BackupVaultName": "bkp-reason-vault-key-vault",
                                "EncryptionKeyArn": "not-a-key-arn"
                            }
                        }
                    }
                })
                .to_string()
            },
            failed: "Vault",
            sentence: || invalid_parameter_message(INVALID_KEY_MEMBER),
            class: INVALID_REQUEST,
            attempts: SINGLE_ATTEMPT,
        },
    ]
}

/// Every row's failed resource reports the refusal as AWS Backup's SDK raises it: that
/// service's own sentence, the diagnostic naming the service, the status and the call, and
/// the attempt count its refusal class implies — all of it inside the provider envelope,
/// under the `HandlerErrorCode` class the refusal falls in.
#[tokio::test]
async fn a_refused_backup_call_reaches_the_failed_resource_with_its_sdk_diagnostic() {
    let router = build_router();
    // Every id the rows read: each is the id of ONE call, so no two rows may report the same
    // one and no row may report its own request's token as it.
    let mut minted: BTreeSet<String> = BTreeSet::new();

    for row in rows() {
        let case = row.case;
        let region = row.region;
        let template = (row.template)();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[("StackName", row.stack_name), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (stack_status, described) = settled_stack(&router, region, row.stack_name).await;
        assert_eq!(
            stack_status, "ROLLBACK_COMPLETE",
            "case {case:?}: expected a refused create to roll the stack back, got {described}"
        );
        assert!(
            outputs_of(&described).is_empty(),
            "case {case:?}: expected no output for a resource nothing created, got {described}"
        );

        // The failure is recorded against the resource the call was about.
        let (status, events) = cloudformation(
            &router,
            region,
            "DescribeStackEvents",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with body \
             {events}"
        );
        let failed: Vec<(String, String)> = members_of(&events)
            .into_iter()
            .filter(|event| {
                elements(event, "ResourceStatus").first().map(String::as_str)
                    == Some(CREATE_FAILED)
            })
            .map(|event| {
                (
                    elements(event, "LogicalResourceId")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                    elements(event, "ResourceStatusReason")
                        .first()
                        .cloned()
                        .unwrap_or_default(),
                )
            })
            .filter(|(logical_id, _)| logical_id != row.stack_name)
            .collect();
        assert_eq!(
            failed
                .iter()
                .map(|(id, _)| id.as_str())
                .collect::<Vec<_>>(),
            vec![row.failed],
            "case {case:?}: expected {:?} to be the one resource recorded {CREATE_FAILED}, got \
             {failed:?} in {events}",
            row.failed
        );
        let reason = failed[0].1.clone();

        // The reason is the provider envelope, filed under the class the refusal falls in.
        let envelope = envelope_of(&reason).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the reason to be the provider envelope opening \
                 {ENVELOPE_OPENS:?}, got {reason:?}"
            )
        });
        assert_eq!(
            envelope.class, row.class,
            "case {case:?}: expected the refusal filed under {:?}, got {:?} in {reason:?}",
            row.class, envelope.class
        );
        assert!(
            is_minted_uuid(&envelope.token),
            "case {case:?}: expected the resource request's token to be a uuid, got {:?}",
            envelope.token
        );

        // And what it quotes is the exception AWS Backup's own SDK raised: that service's
        // sentence, then the diagnostic of the refused call.
        let exception = sdk_exception_of(&envelope.message).unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected the quoted message to close with the diagnostic \
                 \"(Service: {SDK_SERVICE}, Status Code: {REFUSAL_STATUS}, Request ID: <id>) \
                 (SDK Attempt Count: <n>)\", got {:?}",
                envelope.message
            )
        });
        let expected = (row.sentence)();
        assert_eq!(
            exception.sentence, expected,
            "case {case:?}: expected the quoted message to open with {expected:?}, got {:?}",
            exception.sentence
        );
        assert_eq!(
            exception.attempts, row.attempts,
            "case {case:?}: expected the diagnostic to close with {} attempts, got {}",
            row.attempts, exception.attempts
        );
        assert!(
            is_minted_uuid(&exception.request_id),
            "case {case:?}: expected the call's id to be a uuid, got {:?}",
            exception.request_id
        );
        assert_ne!(
            exception.request_id, envelope.token,
            "case {case:?}: expected the refused call's id and the resource request's token to \
             be different identifiers, got {:?} for both",
            exception.request_id
        );
        for id in [exception.request_id.clone(), envelope.token.clone()] {
            assert!(
                minted.insert(id.clone()),
                "case {case:?}: expected every identifier to be this call's own, got {id:?} a \
                 second time"
            );
        }
    }
}
