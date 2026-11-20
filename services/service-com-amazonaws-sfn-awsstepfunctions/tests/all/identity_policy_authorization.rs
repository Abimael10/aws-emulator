// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the IAM authorization every call to this service is subject to,
//! driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM and STS mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the policies a decision reads are the
//! documents IAM's own handlers wrote and the session a call signs as is the one STS's own
//! `AssumeRole` minted.
//!
//! The rule under test: every operation of this API is a signed call, and Step Functions
//! decides it at the IAM layer BEFORE it is dispatched — served only where the
//! identity-based documents in force on the SIGNING principal allow
//! `states:<OperationName>` on the ARN the request names, and otherwise refused with HTTP
//! 400 `AccessDeniedException` carrying the one sentence AWS words an authorization
//! failure with. The decision is taken per request, so a statement just revoked refuses
//! the next call and one put back serves it again, and a refused call reads and writes
//! nothing.
//!
//! The rows differ along what the rule generalises over: the OPERATION (a start, a read of
//! one execution, a read of its history, a list of a machine's executions, a read of the
//! state machine, a listing that addresses no single resource, a read of a resource's
//! tags, and a create judged against the ARN its name mints), the KIND of principal (an
//! assumed-role session — the identity a Lambda function's own calls are made as — an IAM
//! user, and the account's own published credential, which no identity policy governs),
//! what the documents say (every action of this service, nothing of this service, an
//! explicit deny beside a wildcard allow), and — for the resource half of the decision —
//! what the grant's `Resource` element names (one state machine, another state machine,
//! both, every state machine of the Region, every resource).
//!
//! The catalog the gate reads a request's resource through is re-derived from this crate's
//! own Smithy model below, so a model that grows an operation, or renames the member one
//! addresses its resource by, fails here rather than silently deciding that call against
//! every resource.
//!
//! A `SendTask*` call names no ARN of its own, so the last test here takes the same rule
//! over the resource its TASK TOKEN belongs to — an activity's task and a callback
//! (`.waitForTaskToken`) task of a state machine's execution — with the token read back out
//! of the work this service handed it to.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::service::identity_authorization;
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    read_task_token, task_authorization, EVENT_TASK_SUBMITTED,
};
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element, SFN};
use crate::state_machine_lifecycle::{
    error_code, string, ACCOUNT, HELLO_DEFINITION, PAST_THE_EXECUTION_DELAY, REGION, ROLE_ARN,
};

/// This crate's Smithy model — the source the catalog in `service::identity_authorization`
/// is a transcription of, read back here so the two cannot drift.
pub(crate) const MODEL_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/smithy-model/com-amazonaws-sfn-awsstepfunctions.json"
);

/// The code and status AWS refuses an authorization failure with.
pub(crate) const ACCESS_DENIED: &str = "AccessDeniedException";
pub(crate) const ACCESS_DENIED_STATUS: u16 = 400;

/// The access-key id standing for the account's own published credential: neither STS nor
/// IAM holds a record of it, so it resolves to the account's ambient identity, which no
/// identity policy governs — the position AWS account root credentials are in.
pub(crate) const ACCOUNT_KEY: &str = "AKIDEXAMPLE";

/// The API versions the IAM and STS setup calls below are made at.
pub(crate) const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// A principal a call is signed as: the access key id it presents, and the ARN a refusal
/// must blame it by — what `sts:GetCallerIdentity` reports for that key.
pub(crate) struct Principal {
    pub(crate) access_key_id: String,
    pub(crate) arn: String,
}

/// Step Functions, IAM and STS over ONE engine, so the documents a decision reads are the
/// ones IAM's handlers wrote and the session a call signs as is one STS minted.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Default for Emulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite signs nothing: the `Authorization` header carries only the credential
        // scope, which is where a service reads the caller's access key id from.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Call one Step Functions operation with `access_key_id` in the SigV4 credential
    /// scope, and answer `(status, body)`.
    pub(crate) async fn call_as(
        &self,
        access_key_id: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        self.call_as_in_region(access_key_id, REGION, operation, body)
            .await
    }

    /// The same call signed for `region`: the Region a request was made FOR, which is the
    /// one an ARN this service writes for that request carries — a refusal's resource ARN
    /// included.
    pub(crate) async fn call_as_in_region(
        &self,
        access_key_id: &str,
        region: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{region}/{}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef",
            SFN.signing_name
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{}.{region}.amazonaws.com", SFN.signing_name),
            )
            .header(
                "content-type",
                format!("application/x-amz-json-{}", SFN.json_version),
            )
            .header("x-amz-target", format!("{}.{operation}", SFN.target_prefix))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "case {operation}: expected a JSON body, got {error} for {:?}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// One Step Functions call as the account's own credential, requiring a 200: the setup,
    /// and the readings a row checks its own effect with.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.call_as(ACCOUNT_KEY, operation, body).await;
        assert_eq!(
            status, 200,
            "case {operation}: the setup call must succeed, got {status} with {parsed:?}"
        );
        parsed
    }

    /// One awsQuery call to IAM or STS, signed with the account's own credential.
    pub(crate) async fn query(
        &self,
        signing_name: &str,
        version: &str,
        action: &str,
        params: &[(&str, String)],
    ) -> String {
        let mut body = format!("Action={action}&Version={version}");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {action:?}: the setup call must succeed, got {status} with {text}"
        );
        text
    }

    /// An IAM user carrying `policy` as its inline policy, with a long-term access key of
    /// its own.
    pub(crate) async fn user(&self, user_name: &str, policy: &Value) -> Principal {
        let created = self
            .query(
                "iam",
                IAM_VERSION,
                "CreateUser",
                &[("UserName", user_name.to_string())],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateUser answers the user's Arn, got {created}"));
        self.query(
            "iam",
            IAM_VERSION,
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
        let minted = self
            .query(
                "iam",
                IAM_VERSION,
                "CreateAccessKey",
                &[("UserName", user_name.to_string())],
            )
            .await;
        Principal {
            access_key_id: xml_element(&minted, "AccessKeyId")
                .unwrap_or_else(|| panic!("CreateAccessKey answers an AccessKeyId, got {minted}")),
            arn,
        }
    }

    /// An IAM role carrying `policy` as its inline policy, assumed through STS: the
    /// session's access key id and the session ARN — the identity a function's own calls
    /// are made as.
    pub(crate) async fn assumed_role(&self, role_name: &str, policy: &Value) -> Principal {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{ACCOUNT}:root") },
                "Action": "sts:AssumeRole",
            }]
        });
        self.query(
            "iam",
            IAM_VERSION,
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
        self.put_role_policy(role_name, policy).await;
        let assumed = self
            .query(
                "sts",
                STS_VERSION,
                "AssumeRole",
                &[
                    ("RoleArn", format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")),
                    ("RoleSessionName", "probe".to_string()),
                ],
            )
            .await;
        Principal {
            access_key_id: xml_element(&assumed, "AccessKeyId")
                .unwrap_or_else(|| panic!("AssumeRole answers an AccessKeyId, got {assumed}")),
            arn: xml_element(&assumed, "Arn")
                .unwrap_or_else(|| panic!("AssumeRole answers the session's Arn, got {assumed}")),
        }
    }

    /// An IAM role a STATE MACHINE acts as: trusted by this service's own principal, and
    /// allowed every action of this service, so the integration call a `Task` state makes is
    /// really served. Answers its ARN.
    pub(crate) async fn state_machine_role(&self, role_name: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": task_authorization::SERVICE_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }]
        });
        self.query(
            "iam",
            IAM_VERSION,
            "CreateRole",
            &[
                ("RoleName", role_name.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
        self.put_role_policy(role_name, &allowing(&["states:*"], &["*"]))
            .await;
        format!("arn:aws:iam::{ACCOUNT}:role/{role_name}")
    }

    /// Replace the inline policy of the role `role_name`, which is how a grant is put in
    /// place and withdrawn again.
    pub(crate) async fn put_role_policy(&self, role_name: &str, policy: &Value) {
        self.query(
            "iam",
            IAM_VERSION,
            "PutRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    }

    /// Replace the inline policy of the USER `user_name`, which is how a grant is put in
    /// place for a long-term key and withdrawn again.
    pub(crate) async fn put_user_policy(&self, user_name: &str, policy: &Value) {
        self.query(
            "iam",
            IAM_VERSION,
            "PutUserPolicy",
            &[
                ("UserName", user_name.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
    }

    /// Delete the role's inline policy altogether, leaving a role IAM holds with no
    /// document at all.
    async fn delete_role_policy(&self, role_name: &str) {
        self.query(
            "iam",
            IAM_VERSION,
            "DeleteRolePolicy",
            &[
                ("RoleName", role_name.to_string()),
                ("PolicyName", POLICY_NAME.to_string()),
            ],
        )
        .await;
    }

    /// A state machine named `name`, as its ARN.
    async fn machine(&self, name: &str) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": HELLO_DEFINITION,
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// One execution of `machine`, as its ARN, with the outcome the clock owed it already
    /// due.
    async fn execution(&self, machine: &str, name: &str) -> String {
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine, "name": name }),
            )
            .await;
        std::thread::sleep(PAST_THE_EXECUTION_DELAY);
        string(&started, "executionArn")
    }

    /// How many executions `machine` holds, read as the account.
    async fn execution_count(&self, machine: &str) -> usize {
        let listed = self
            .sfn("ListExecutions", &json!({ "stateMachineArn": machine }))
            .await;
        listed
            .get("executions")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    }
}

/// The inline-policy name every document below is written under.
const POLICY_NAME: &str = "inline";

/// An identity policy allowing `actions` on `resources`.
pub(crate) fn allowing(actions: &[&str], resources: &[&str]) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resources }]
    })
}

/// The sentence AWS states an authorization failure with: who was refused, which action,
/// on which resource, and which of IAM's rules refused it.
pub(crate) fn not_authorized(principal: &str, action: &str, resource: &str, reason: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// The clause a request no document in force allows is refused with.
pub(crate) fn no_grant(action: &str) -> String {
    format!("because no identity-based policy allows the {action} action")
}

/// The message a refusal carries, whichever casing the envelope spells it in.
pub(crate) fn message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The ARN naming every resource of the TYPE `resource_type` in the Region every call below
/// is signed for: what a request that names no resource of its own is judged against, and
/// what a refusal of such a call names — the API's own resource type with the resource name
/// wildcarded.
pub(crate) fn every_resource(resource_type: &str) -> String {
    every_resource_in(REGION, resource_type)
}

/// The same ARN for a call signed for `region`: the Region a request was made for is the
/// one the resource a refusal of it names belongs to.
pub(crate) fn every_resource_in(region: &str, resource_type: &str) -> String {
    format!("arn:aws:states:{region}:{ACCOUNT}:{resource_type}:*")
}

/// The resource type the state machine APIs of this service are about, which is the kind of
/// resource a call of one that names no resource of its own is judged against.
pub(crate) const STATE_MACHINE_TYPE: &str = "stateMachine";

/// The resource type an API that acts on NO resource of the account is judged against: IAM's
/// wildcard, standing for every resource type this service's ARNs carry, so
/// `every_resource(EVERY_TYPE)` is the ARN with both the type and the name wildcarded.
pub(crate) const EVERY_TYPE: &str = "*";

/// The ARN of the activity `name` in the Region every call below is signed for.
fn activity_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:activity:{name}")
}

/// One call the rule covers: the operation, the request, and the ARN that request
/// addresses — the resource the decision about it is made on.
struct Row {
    operation: &'static str,
    case: &'static str,
    body: Value,
    resource: String,
}

/// Every call the rows below are taken over, each addressing its resource through a
/// different member of the model: an execution ARN, a state machine ARN, a `resourceArn`,
/// the name a create mints its resource under, and a request that names no resource at
/// all.
fn rows(index: usize, machine: &str, execution: &str) -> Vec<Row> {
    vec![
        Row {
            case: "a start of an execution",
            operation: "StartExecution",
            body: json!({ "stateMachineArn": machine, "name": format!("run-{index}") }),
            resource: machine.to_string(),
        },
        Row {
            case: "a read of one execution",
            operation: "DescribeExecution",
            body: json!({ "executionArn": execution }),
            resource: execution.to_string(),
        },
        Row {
            case: "a read of one execution's history",
            operation: "GetExecutionHistory",
            body: json!({ "executionArn": execution }),
            resource: execution.to_string(),
        },
        Row {
            case: "a list of a state machine's executions",
            operation: "ListExecutions",
            body: json!({ "stateMachineArn": machine }),
            resource: machine.to_string(),
        },
        Row {
            case: "a read of the state machine",
            operation: "DescribeStateMachine",
            body: json!({ "stateMachineArn": machine }),
            resource: machine.to_string(),
        },
        Row {
            case: "a listing that addresses no single resource",
            operation: "ListStateMachines",
            body: json!({}),
            // A call that names no resource of this service is judged against every
            // resource of the kind its API is about, written as that type's wildcard ARN.
            resource: every_resource(STATE_MACHINE_TYPE),
        },
        Row {
            case: "a read of a resource's tags",
            operation: "ListTagsForResource",
            body: json!({ "resourceArn": machine }),
            resource: machine.to_string(),
        },
        Row {
            case: "a create, judged against the ARN its name mints",
            operation: "CreateActivity",
            body: json!({ "name": format!("probe-{index}") }),
            resource: activity_arn(&format!("probe-{index}")),
        },
    ]
}

/// What the documents in force on one caller do to the calls above.
enum Decided {
    /// Every call is served.
    Served,
    /// No readable document allows them, so each is refused for want of a grant.
    NoGrant,
    /// A document denies them explicitly, which the refusal names instead.
    ExplicitDeny,
}

/// How one caller of the rows above is made, and what its documents say.
struct CallerCase {
    case: &'static str,
    /// The inline policy the principal carries, or `None` for the account's own published
    /// credential, which holds no identity policy at all.
    policy: Option<Value>,
    /// Whether the principal is an assumed-role session (as against an IAM user), for a
    /// caller that carries a policy.
    session: bool,
    decided: Decided,
}

/// Every call of this service is served only where the documents in force on the signing
/// principal allow `states:<OperationName>` on the ARN the request names; every other
/// caller is refused with HTTP 400 `AccessDeniedException` naming the principal, the
/// action and that ARN, and the refused call changes nothing.
#[tokio::test]
async fn a_call_is_served_only_where_the_signing_principals_policies_allow_the_action() {
    let callers = vec![
        CallerCase {
            case: "an assumed-role session whose role allows every action of this service",
            policy: Some(allowing(&["states:*"], &["*"])),
            session: true,
            decided: Decided::Served,
        },
        CallerCase {
            case: "an assumed-role session whose role allows nothing of this service",
            policy: Some(allowing(&["kms:Decrypt"], &["*"])),
            session: true,
            decided: Decided::NoGrant,
        },
        CallerCase {
            case: "an assumed-role session whose role is denied this service explicitly",
            policy: Some(json!({
                "Version": "2012-10-17",
                "Statement": [
                    { "Effect": "Allow", "Action": "states:*", "Resource": "*" },
                    { "Effect": "Deny", "Action": "states:*", "Resource": "*" },
                ]
            })),
            session: true,
            decided: Decided::ExplicitDeny,
        },
        CallerCase {
            case: "an IAM user whose policy allows every action of this service",
            policy: Some(allowing(&["states:*"], &["*"])),
            session: false,
            decided: Decided::Served,
        },
        CallerCase {
            case: "an IAM user whose documents say nothing about this service",
            policy: Some(allowing(&["sts:GetCallerIdentity"], &["*"])),
            session: false,
            decided: Decided::NoGrant,
        },
        CallerCase {
            case: "the account's own published credential",
            policy: None,
            session: false,
            decided: Decided::Served,
        },
    ];

    for (index, caller) in callers.into_iter().enumerate() {
        let emulator = Emulator::new();
        let machine = emulator.machine(&format!("authz-{index}")).await;
        let execution = emulator.execution(&machine, "first").await;
        let principal = match (&caller.policy, caller.session) {
            (Some(policy), true) => {
                emulator
                    .assumed_role(&format!("sfn-caller-{index}"), policy)
                    .await
            }
            (Some(policy), false) => emulator.user(&format!("sfn-user-{index}"), policy).await,
            (None, _) => Principal {
                access_key_id: ACCOUNT_KEY.to_string(),
                arn: format!("arn:aws:iam::{ACCOUNT}:root"),
            },
        };

        for Row {
            case: row,
            operation,
            body,
            resource,
        } in rows(index, &machine, &execution)
        {
            let case = caller.case;
            let before = emulator.execution_count(&machine).await;
            let (status, answer) = emulator
                .call_as(&principal.access_key_id, operation, &body)
                .await;
            let reason = match caller.decided {
                Decided::Served => {
                    assert_eq!(
                        status, 200,
                        "case {case:?} / {row:?}: a call the documents in force allow is served, \
                         got {status} with {answer:?}"
                    );
                    continue;
                }
                Decided::NoGrant => no_grant(&format!("states:{operation}")),
                Decided::ExplicitDeny => {
                    "with an explicit deny in an identity-based policy".to_string()
                }
            };
            assert_eq!(
                (status, error_code(&answer).as_str()),
                (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                "case {case:?} / {row:?}: a call the documents in force do not allow is refused, \
                 got {status} with {answer:?}"
            );
            let expected = not_authorized(
                &principal.arn,
                &format!("states:{operation}"),
                &resource,
                &reason,
            );
            let stated = message(&answer);
            assert_eq!(
                stated, expected,
                "case {case:?} / {row:?}: expected {expected:?}, got {stated:?}"
            );
            // A refused call changed nothing: a refused start started no execution.
            let after = emulator.execution_count(&machine).await;
            assert_eq!(
                after, before,
                "case {case:?} / {row:?}: a refused call must change nothing, and the machine \
                 held {before} execution(s) before it"
            );
        }
    }
}

/// The decision is taken per request against the documents as they read NOW: revoking a
/// statement refuses the next call, deleting the document altogether keeps refusing it,
/// and putting the statement back serves it again — and nothing a refused call asked for
/// happened.
#[tokio::test]
async fn revoking_a_statement_refuses_the_next_call_and_restoring_it_serves_again() {
    let emulator = Emulator::new();
    let machine = emulator.machine("revocation").await;
    let execution = emulator.execution(&machine, "first").await;
    let role = "sfn-revocation";
    let granted = allowing(&["states:StartExecution", "states:DescribeExecution"], &["*"]);
    let principal = emulator.assumed_role(role, &granted).await;

    // (case, the document in force — `None` deletes it altogether, whether the two calls
    // below are served)
    let phases: Vec<(&str, Option<Value>, bool)> = vec![
        ("the grant in force", Some(granted.clone()), true),
        (
            "the statement revoked, another left in its place",
            Some(allowing(&["states:ListExecutions"], &["*"])),
            false,
        ),
        ("the document deleted altogether", None, false),
        ("the grant put back", Some(granted.clone()), true),
    ];

    let mut started = emulator.execution_count(&machine).await;
    for (phase_index, (phase, document, served)) in phases.into_iter().enumerate() {
        match &document {
            Some(document) => emulator.put_role_policy(role, document).await,
            None => emulator.delete_role_policy(role).await,
        }
        // Each phase is asked TWICE: a decision taken against the documents in force is
        // the same decision on the next call, so nothing of it latches.
        for attempt in 1..=2 {
            let (status, answer) = emulator
                .call_as(
                    &principal.access_key_id,
                    "DescribeExecution",
                    &json!({ "executionArn": execution }),
                )
                .await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {phase:?} (attempt {attempt}): the read is served, got {status} with \
                     {answer:?}"
                );
                assert_eq!(
                    string(&answer, "executionArn"),
                    execution,
                    "case {phase:?} (attempt {attempt}): the read reports the execution asked \
                     for, got {answer:?}"
                );
            } else {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                    "case {phase:?} (attempt {attempt}): the read is refused, got {status} with \
                     {answer:?}"
                );
                let expected = not_authorized(
                    &principal.arn,
                    "states:DescribeExecution",
                    &execution,
                    &no_grant("states:DescribeExecution"),
                );
                assert_eq!(
                    message(&answer),
                    expected,
                    "case {phase:?} (attempt {attempt}): expected {expected:?}, got {answer:?}"
                );
            }

            let name = format!("run-{phase_index}-{attempt}");
            let (status, answer) = emulator
                .call_as(
                    &principal.access_key_id,
                    "StartExecution",
                    &json!({ "stateMachineArn": machine, "name": name }),
                )
                .await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {phase:?} (attempt {attempt}): the start is served, got {status} with \
                     {answer:?}"
                );
                started += 1;
            } else {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                    "case {phase:?} (attempt {attempt}): the start is refused, got {status} with \
                     {answer:?}"
                );
                let expected = not_authorized(
                    &principal.arn,
                    "states:StartExecution",
                    &machine,
                    &no_grant("states:StartExecution"),
                );
                assert_eq!(
                    message(&answer),
                    expected,
                    "case {phase:?} (attempt {attempt}): expected {expected:?}, got {answer:?}"
                );
            }
            // A refused start started nothing, and a served one started exactly one.
            let held = emulator.execution_count(&machine).await;
            assert_eq!(
                held, started,
                "case {phase:?} (attempt {attempt}): expected {started} execution(s) to have \
                 been started, got {held}"
            );
        }
    }
}

/// The decision is made about the ARN the request NAMES, so a grant is read for its
/// `Resource` element as well as for its action: a statement scoped to one state machine
/// covers the calls about that machine and is a REFUSAL — not an empty answer — for
/// another.
#[tokio::test]
async fn a_grant_is_read_for_the_resource_the_request_names() {
    let emulator = Emulator::new();
    let first = emulator.machine("scoped-first").await;
    let second = emulator.machine("scoped-second").await;
    let every_machine = format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:*");
    let role = "sfn-scoped";
    let principal = emulator
        .assumed_role(role, &allowing(&["states:ListExecutions"], &[&first]))
        .await;

    // (case, what the grant's `Resource` element names, whether the list of the first and
    // of the second machine is served)
    let scopes: Vec<(&str, Vec<String>, [bool; 2])> = vec![
        (
            "a grant scoped to the first machine",
            vec![first.clone()],
            [true, false],
        ),
        (
            "a grant scoped to the second machine",
            vec![second.clone()],
            [false, true],
        ),
        (
            "a grant naming both machines",
            vec![first.clone(), second.clone()],
            [true, true],
        ),
        (
            "a grant scoped to every state machine of the Region",
            vec![every_machine.clone()],
            [true, true],
        ),
        (
            "a grant scoped to every resource",
            vec!["*".to_string()],
            [true, true],
        ),
    ];

    for (case, resources, served) in scopes {
        let named: Vec<&str> = resources.iter().map(String::as_str).collect();
        emulator
            .put_role_policy(role, &allowing(&["states:ListExecutions"], &named))
            .await;
        for (machine, served) in [&first, &second].into_iter().zip(served) {
            let (status, answer) = emulator
                .call_as(
                    &principal.access_key_id,
                    "ListExecutions",
                    &json!({ "stateMachineArn": machine }),
                )
                .await;
            if served {
                assert_eq!(
                    status, 200,
                    "case {case:?} / {machine}: a list the grant covers is served, got {status} \
                     with {answer:?}"
                );
                assert!(
                    answer.get("executions").and_then(Value::as_array).is_some(),
                    "case {case:?} / {machine}: a served list reports its executions, got \
                     {answer:?}"
                );
            } else {
                assert_eq!(
                    (status, error_code(&answer).as_str()),
                    (ACCESS_DENIED_STATUS, ACCESS_DENIED),
                    "case {case:?} / {machine}: a list outside the grant is REFUSED rather than \
                     answered empty, got {status} with {answer:?}"
                );
                let expected = not_authorized(
                    &principal.arn,
                    "states:ListExecutions",
                    machine,
                    &no_grant("states:ListExecutions"),
                );
                assert_eq!(
                    message(&answer),
                    expected,
                    "case {case:?} / {machine}: expected {expected:?}, got {answer:?}"
                );
            }
        }
    }

    // The action half of the same decision: the grant names one action, so another
    // operation about the very machine it names is refused too.
    emulator
        .put_role_policy(role, &allowing(&["states:ListExecutions"], &[&first]))
        .await;
    let (status, answer) = emulator
        .call_as(
            &principal.access_key_id,
            "DescribeStateMachine",
            &json!({ "stateMachineArn": first }),
        )
        .await;
    assert_eq!(
        (status, error_code(&answer).as_str()),
        (ACCESS_DENIED_STATUS, ACCESS_DENIED),
        "case \"an action the grant does not name\": expected a refusal, got {status} with \
         {answer:?}"
    );
    let expected = not_authorized(
        &principal.arn,
        "states:DescribeStateMachine",
        &first,
        &no_grant("states:DescribeStateMachine"),
    );
    assert_eq!(
        message(&answer),
        expected,
        "case \"an action the grant does not name\": expected {expected:?}, got {answer:?}"
    );
}

/// The gate reads a request's resource through a catalog keyed by operation
/// (`service::identity_authorization::ADDRESSED_BY`), which is a transcription of this
/// crate's Smithy model: every operation the model binds carries a row, every member a row
/// names is a member of THAT operation's own input, and every member of an input that
/// names an ARN of this service's own resources is listed for it. Its companion
/// (`service::identity_authorization::ADDRESSED_BY_TASK_TOKEN`) is the same transcription
/// for the operations that name their resource through a task token instead: exactly the
/// ones whose input declares `taskToken`.
///
/// So a model that grows an operation, renames a member, or moves a resource ARN onto
/// another operation fails here rather than leaving that call judged against every
/// resource (or judged by a member its own operation does not model).
#[test]
fn the_catalog_of_addressed_members_is_the_model_s_own() {
    let source = std::fs::read_to_string(MODEL_PATH).unwrap_or_else(|error| {
        panic!("the Smithy model at {MODEL_PATH} must be readable: {error}")
    });
    let model: Value = serde_json::from_str(&source).expect("the Smithy model parses as JSON");
    let shapes = model
        .get("shapes")
        .and_then(Value::as_object)
        .expect("the Smithy model carries a shapes map");

    // The member a request carries the task token of the task it is about in, which is how
    // an operation that names no ARN of its own still names a resource.
    let task_token_member = "taskToken";

    // The members of an input that name an ARN of a resource of THIS service: every member
    // the catalog may list, read off the model's own member names rather than guessed at.
    // A `roleArn` names an IAM role, and a `taskToken` is no ARN — it names its resource
    // through the token's own contents — so neither is one.
    let names_a_resource = |member: &str| {
        matches!(
            member,
            "executionArn"
                | "stateMachineArn"
                | "stateMachineAliasArn"
                | "stateMachineVersionArn"
                | "mapRunArn"
                | "activityArn"
                | "resourceArn"
        )
    };

    let mut bound = 0usize;
    for (shape_id, shape) in shapes {
        if shape.get("type").and_then(Value::as_str) != Some("operation") {
            continue;
        }
        let operation = shape_id.rsplit('#').next().unwrap_or(shape_id);
        bound += 1;
        let listed = identity_authorization::addressed_by(operation);
        assert!(
            identity_authorization::ADDRESSED_BY
                .iter()
                .any(|(named, _)| *named == operation),
            "case {operation:?}: the model binds this operation, which the catalog does not \
             carry a row for"
        );
        // The input shape's own members, which is what a row may name.
        let input = shape
            .get("input")
            .and_then(|input| input.get("target"))
            .and_then(Value::as_str)
            .expect("every operation of this model declares an input shape");
        let members = shapes
            .get(input)
            .and_then(|shape| shape.get("members"))
            .and_then(Value::as_object);
        let member_names: Vec<&str> = members
            .map(|members| members.keys().map(String::as_str).collect())
            .unwrap_or_default();
        for named in listed {
            assert!(
                member_names.contains(named),
                "case {operation:?}: the catalog names {named:?}, which {input} does not declare; \
                 its members are {member_names:?}"
            );
        }
        for member in &member_names {
            if !names_a_resource(member) {
                continue;
            }
            assert!(
                listed.contains(member),
                "case {operation:?}: {input} declares {member:?}, a resource ARN of this \
                 service, which the catalog does not list for it; it lists {listed:?}"
            );
        }
        // An operation is judged through its task token exactly when its own input carries
        // one, so the two catalogs cannot drift from the model either way.
        let carries_token = member_names.contains(&task_token_member);
        let judged_by_token = identity_authorization::addressed_by_task_token(operation);
        assert_eq!(
            judged_by_token, carries_token,
            "case {operation:?}: {input} declares {task_token_member:?}={carries_token}, so \
             expected the token catalog to carry it={carries_token}, got {judged_by_token}"
        );
    }
    assert_eq!(
        bound,
        identity_authorization::ADDRESSED_BY.len(),
        "expected one catalog row per operation the model binds, got {} row(s) for {bound} \
         operation(s)",
        identity_authorization::ADDRESSED_BY.len()
    );
}

/// The catalog the gate reads the RESOURCE TYPE of an API through
/// (`service::identity_authorization::API_RESOURCE_TYPE`) is a transcription of the same
/// model: every operation the model binds carries a row, and the type each row names is one
/// of this service's own ARN resource types — or the empty one, for an API about no single
/// type.
///
/// So an operation added to the model with no row fails here rather than silently leaving
/// that call judged against the both-wildcards ARN, and the catalog of the ownership check
/// (`service::identity_authorization::OWNERSHIP_CHECKED`) is read back beside it: every
/// operation it names is one the model binds, and each is about a state machine's aliases
/// or versions.
#[test]
fn the_catalog_of_api_resource_types_is_the_model_s_own() {
    let source = std::fs::read_to_string(MODEL_PATH).unwrap_or_else(|error| {
        panic!("the Smithy model at {MODEL_PATH} must be readable: {error}")
    });
    let model: Value = serde_json::from_str(&source).expect("the Smithy model parses as JSON");
    let shapes = model
        .get("shapes")
        .and_then(Value::as_object)
        .expect("the Smithy model carries a shapes map");

    // The resource types this service's own ARNs carry, which is what a row may name.
    let types = ["stateMachine", "activity", "execution", "mapRun"];
    let mut bound: Vec<String> = Vec::new();
    for (shape_id, shape) in shapes {
        if shape.get("type").and_then(Value::as_str) != Some("operation") {
            continue;
        }
        let operation = shape_id.rsplit('#').next().unwrap_or(shape_id).to_string();
        let row = identity_authorization::API_RESOURCE_TYPE
            .iter()
            .find(|(named, _)| *named == operation.as_str());
        let (_, resource_type) = row.unwrap_or_else(|| {
            panic!(
                "case {operation:?}: the model binds this operation, which the resource-type \
                 catalog does not carry a row for"
            )
        });
        assert!(
            resource_type.is_empty() || types.contains(resource_type),
            "case {operation:?}: the row names the resource type {resource_type:?}, which is \
             none of this service's own {types:?}"
        );
        // What the gate answers for the row, which is the ARN a refusal of such a call
        // names: a wildcard of that type, and every resource where the row names none.
        let judged = identity_authorization::any_resource(&operation, REGION);
        let expected = if resource_type.is_empty() {
            every_resource("*")
        } else {
            every_resource(resource_type)
        };
        assert_eq!(
            judged, expected,
            "case {operation:?}: expected a call naming no resource of its own to be judged \
             against {expected:?}, got {judged:?}"
        );
        bound.push(operation);
    }
    assert_eq!(
        bound.len(),
        identity_authorization::API_RESOURCE_TYPE.len(),
        "expected one catalog row per operation the model binds, got {} row(s) for {} \
         operation(s)",
        identity_authorization::API_RESOURCE_TYPE.len(),
        bound.len()
    );
    // Every ownership-checked operation is one the model binds, and each is about a state
    // machine's aliases or versions — which is what makes the account part of the reference
    // it names.
    for operation in identity_authorization::OWNERSHIP_CHECKED {
        assert!(
            bound.iter().any(|named| named == operation),
            "case {operation:?}: the ownership check names an operation this model does not \
             bind"
        );
        assert!(
            operation.contains("Alias") || operation.contains("Version"),
            "case {operation:?}: the ownership check is about a state machine's aliases and \
             versions, which this operation's name does not name"
        );
        assert!(
            identity_authorization::ownership_checked(operation, "not-an-arn"),
            "case {operation:?}: expected the catalog's own operation to be ownership-checked"
        );
    }
}

/// How long a poll below waits for a condition this service owes — a task queued on an
/// activity, a callback task's token handed to the work it called.
///
/// Generous: nothing here measures how fast the host is, so the deadline is only a backstop
/// against a condition that never holds at all.
const TOKEN_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll below waits between two looks at the condition it waits FOR.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The child a nested-execution callback task starts: it runs to its own end, because what
/// the parent waits for is the TOKEN and not the child.
const CALLBACK_CHILD: &str =
    r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","Result":{"ok":true},"End":true}}}"#;

/// The optimized integration a callback task below is handed over by: a nested Step
/// Functions execution, which carries the token as the child's own input and so needs no
/// other service mounted.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::states:startExecution.waitForTaskToken";

/// An ACTIVITY and a state machine whose one `Task` hands its work to that activity: the two
/// together are what queues a task a worker can be handed a token for.
pub(crate) async fn activity_handoff(emulator: &Emulator, name: &str) -> (String, String) {
    let created = emulator
        .sfn("CreateActivity", &json!({ "name": name }))
        .await;
    let activity = string(&created, "activityArn");
    let definition = json!({
        "StartAt": "HandOff",
        "States": { "HandOff": { "Type": "Task", "Resource": activity, "End": true } },
    });
    let machine = emulator
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": format!("{name}-machine"),
                "definition": definition.to_string(),
                "roleArn": ROLE_ARN,
            }),
        )
        .await;
    (activity, string(&machine, "stateMachineArn"))
}

/// A token of an OUTSTANDING activity task, as `GetActivityTask` hands it to a worker: one
/// execution of `machine` queues a task on `activity`, and the poll is repeated until the
/// worker is handed its token or the deadline passes.
pub(crate) async fn activity_task_token(
    emulator: &Emulator,
    case: &str,
    activity: &str,
    machine: &str,
    run: &str,
) -> String {
    emulator
        .sfn(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": run }),
        )
        .await;
    let deadline = Instant::now() + TOKEN_DEADLINE;
    loop {
        let polled = emulator
            .sfn(
                "GetActivityTask",
                &json!({ "activityArn": activity, "workerName": run }),
            )
            .await;
        if let Some(token) = polled.get("taskToken").and_then(Value::as_str) {
            return token.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected a task queued on {activity}, got {polled:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The parent state machine every callback token below is minted by, with the child its
/// `Task` starts: one `Task` asking for the callback pattern over a nested execution, which
/// is handed the token through `$$.Task.Token`.
///
/// `role_arn` is the role the machine acts as, which this service assumes to make the
/// integration call — so it has to be one this service may assume.
pub(crate) async fn callback_machine(emulator: &Emulator, name: &str, role_arn: &str) -> String {
    let child = emulator
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": format!("{name}-child"),
                "definition": CALLBACK_CHILD,
                "roleArn": role_arn,
            }),
        )
        .await;
    let child_arn = string(&child, "stateMachineArn");
    let definition = json!({
        "StartAt": "WaitForCallback",
        "States": { "WaitForCallback": {
            "Type": "Task",
            "Resource": CALLBACK_RESOURCE,
            "Parameters": {
                "stateMachineArn": child_arn,
                "input": { "token.$": "$$.Task.Token" },
            },
            "End": true,
        }},
    });
    let parent = emulator
        .sfn(
            "CreateStateMachine",
            &json!({
                "name": format!("{name}-parent"),
                "definition": definition.to_string(),
                "roleArn": role_arn,
            }),
        )
        .await;
    string(&parent, "stateMachineArn")
}

/// A token of an OUTSTANDING callback (`.waitForTaskToken`) task, with the execution waiting
/// at it: one execution of `machine` hands its token to the child it starts, so the token is
/// read back out of the child's own input — the work outside holding it, exactly as a Lambda
/// handler holds the token its invocation was given.
pub(crate) async fn callback_task_token(
    emulator: &Emulator,
    case: &str,
    machine: &str,
    run: &str,
) -> (String, String) {
    let started = emulator
        .sfn(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": run }),
        )
        .await;
    let execution = string(&started, "executionArn");
    let deadline = Instant::now() + TOKEN_DEADLINE;
    loop {
        let history = emulator
            .sfn(
                "GetExecutionHistory",
                &json!({ "executionArn": execution }),
            )
            .await;
        let submitted = history
            .get("events")
            .and_then(Value::as_array)
            .and_then(|events| {
                events
                    .iter()
                    .find(|event| {
                        event.get("type").and_then(Value::as_str) == Some(EVENT_TASK_SUBMITTED)
                    })
                    .cloned()
            });
        if let Some(event) = submitted {
            let details = event.get("taskSubmittedEventDetails").unwrap_or_else(|| {
                panic!("case {case:?}: expected the submission's details in {event:?}")
            });
            let answered: Value = serde_json::from_str(&string(details, "output"))
                .unwrap_or_else(|why| panic!("case {case:?}: the submission's output is JSON: {why}"));
            let child_execution = string(&answered, "ExecutionArn");
            let described = emulator
                .sfn(
                    "DescribeExecution",
                    &json!({ "executionArn": child_execution }),
                )
                .await;
            let input: Value = serde_json::from_str(&string(&described, "input"))
                .unwrap_or_else(|why| panic!("case {case:?}: the child's input is JSON: {why}"));
            return (string(&input, "token"), execution);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the callback task to be handed over, got {history:?}"
        );
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// One `SendTask*` call the rule below is taken over: which operation, and which kind of
/// resource the token it carries names.
struct TokenRow {
    case: &'static str,
    operation: &'static str,
    /// The members the call carries beside its `taskToken`.
    extra: Value,
    /// Whether the token names an ACTIVITY (else the EXECUTION waiting at a callback task).
    activity: bool,
}

/// RULE: a `SendTask*` call names no ARN of its own — the task it settles is named by the
/// task token this service minted for that task — so it is judged against the RESOURCE THAT
/// TASK BELONGS TO: the activity a worker was handed the task by, and for a callback
/// (`.waitForTaskToken`) task, whose token names the execution waiting at it, the state
/// machine that execution is a run of. A caller whose identity-based documents allow the
/// action on that resource is served; one whose grant names another resource of this
/// service, or nothing of this service at all, is refused HTTP 400 `AccessDeniedException`
/// naming the principal, the action and that same resource — and the refused call settles
/// nothing, which the call served after it proves by settling the task that is still
/// outstanding.
///
/// The rows differ along what the rule generalises over: the OPERATION (all three calls that
/// carry a task token) and the KIND of task the token names (an activity's task, and a
/// callback task of a state machine's execution).
#[tokio::test]
async fn a_send_task_call_is_judged_against_the_resource_its_token_names() {
    let rows = [
        TokenRow {
            case: "a success on a callback task",
            operation: "SendTaskSuccess",
            extra: json!({ "output": r#"{"approved":true}"# }),
            activity: false,
        },
        TokenRow {
            case: "a heartbeat on a callback task",
            operation: "SendTaskHeartbeat",
            extra: json!({}),
            activity: false,
        },
        TokenRow {
            case: "a failure on a callback task",
            operation: "SendTaskFailure",
            extra: json!({ "error": "WorkerSaidNo", "cause": "the work outside refused" }),
            activity: false,
        },
        TokenRow {
            case: "a success on an activity task",
            operation: "SendTaskSuccess",
            extra: json!({ "output": r#"{"approved":true}"# }),
            activity: true,
        },
        TokenRow {
            case: "a heartbeat on an activity task",
            operation: "SendTaskHeartbeat",
            extra: json!({}),
            activity: true,
        },
        TokenRow {
            case: "a failure on an activity task",
            operation: "SendTaskFailure",
            extra: json!({ "error": "WorkerSaidNo", "cause": "the worker refused" }),
            activity: true,
        },
    ];

    let emulator = Emulator::new();
    let machine_role = emulator.state_machine_role("sfn-token-machines").await;
    let (activity, activity_machine) = activity_handoff(&emulator, "token-judged").await;
    let callback_parent = callback_machine(&emulator, "token-judged", &machine_role).await;

    for (index, row) in rows.iter().enumerate() {
        let TokenRow {
            case,
            operation,
            extra,
            activity: of_an_activity,
        } = row;
        let run = format!("token-run-{index}");
        // What the token NAMES (the activity a worker polls, or the execution waiting at a
        // callback task), and the resource of this service that task BELONGS TO — the two
        // differ exactly for a callback task, whose execution is a run of a state machine.
        let (token, named, judged) = if *of_an_activity {
            let token =
                activity_task_token(&emulator, case, &activity, &activity_machine, &run).await;
            (token, activity.clone(), activity.clone())
        } else {
            let (token, execution) =
                callback_task_token(&emulator, case, &callback_parent, &run).await;
            (token, execution, callback_parent.clone())
        };
        let reads_as = read_task_token(&token).unwrap_or_else(|| {
            panic!("case {case:?}: expected a token this service issued, got {token:?}")
        });
        assert_eq!(
            reads_as, named,
            "case {case:?}: expected the token to name {named}, got {reads_as}"
        );

        let mut request = json!({ "taskToken": token });
        let members = request.as_object_mut().expect("the request is an object");
        for (member, value) in extra.as_object().expect("the extra members are an object") {
            members.insert(member.clone(), value.clone());
        }
        let action = format!("states:{operation}");
        let role = format!("sfn-token-{index}");
        let elsewhere = format!("{judged}-other");

        // (the case's own wording, the document in force, whether the call is served)
        let decisions = [
            (
                "nothing of this service",
                allowing(&["s3:GetObject"], &["*"]),
                false,
            ),
            (
                "the action on another resource of this service",
                allowing(&[action.as_str()], &[elsewhere.as_str()]),
                false,
            ),
            (
                "the action on the resource the token belongs to",
                allowing(&[action.as_str()], &[judged.as_str()]),
                true,
            ),
        ];
        let principal = emulator.assumed_role(&role, &decisions[0].1).await;
        for (allowed, document, served) in &decisions {
            emulator.put_role_policy(&role, document).await;
            let (status, answer) = emulator
                .call_as(&principal.access_key_id, operation, &request)
                .await;
            let decided = format!("{case:?} allowing {allowed:?}");
            if *served {
                assert_eq!(
                    status, 200,
                    "case {decided}: expected the call to be served, got {status} with {answer:?}"
                );
                continue;
            }
            assert_eq!(
                status, ACCESS_DENIED_STATUS,
                "case {decided}: expected {ACCESS_DENIED_STATUS}, got {status} with {answer:?}"
            );
            assert_eq!(
                error_code(&answer),
                ACCESS_DENIED,
                "case {decided}: expected {ACCESS_DENIED}, got {answer:?}"
            );
            let expected = not_authorized(&principal.arn, &action, &judged, &no_grant(&action));
            assert_eq!(
                message(&answer),
                expected,
                "case {decided}: expected {expected:?}, got {answer:?}"
            );
        }
    }
}
