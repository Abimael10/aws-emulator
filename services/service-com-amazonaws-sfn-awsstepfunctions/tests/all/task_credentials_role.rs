// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ROLE a `Task` state's own `Credentials` names, driven over
//! all three services' own wire protocols through the production `MultiServiceRouter`
//! with Step Functions, IAM and DynamoDB mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the policies the call is judged against
//! are the documents IAM's own handlers wrote and the far-end effect is read back
//! through DynamoDB's own API.
//!
//! RULE: a `Task` state's `Credentials.RoleArn` names the role the state's integration
//! call is made UNDER. Step Functions resolves the member in the state's own query
//! language, assumes that role — which the state machine's execution role must be allowed
//! to do, and whose own trust policy must admit that role
//! (`tests/all/task_credentials_role_trust.rs` holds what the trust policy decides) — and
//! makes the call as it, so the call is allowed or refused by THAT role's policies and not
//! by the execution role's. A role the execution role may not assume fails the task
//! instead, and no call is made for it.
//!
//! The rows differ along what the rule generalises over: the FORM the member is written in
//! (a literal ARN, a `RoleArn.$` reference path into the state's input, a `RoleArn.$`
//! intrinsic over a variable an earlier state assigned, and a JSONata expression), and
//! whether the role named may be assumed at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::state_machine_lifecycle::{string, REGION};

/// How each service's requests are addressed on the wire, from each model's own
/// `service_info` (service shape, `sigv4Name` and the `awsJson` version its protocol
/// declares).
const SFN: Wire = Wire {
    signing_name: "states",
    target_prefix: "AWSStepFunctions",
    json_version: "1.0",
};
/// See [`SFN`].
const DYNAMODB: Wire = Wire {
    signing_name: "dynamodb",
    target_prefix: "DynamoDB_20120810",
    json_version: "1.0",
};

/// How one service's requests are addressed on the wire.
struct Wire {
    signing_name: &'static str,
    target_prefix: &'static str,
    json_version: &'static str,
}

/// How long a poll waits for a condition the clock owes — a created table serving items,
/// an execution reaching a terminal status — before giving up.
///
/// Generous: what every loop below waits for is the CONDITION, so no row turns on how fast
/// this host runs.
pub(crate) const PATIENCE: Duration = Duration::from_secs(90);

/// How long one poll pauses between two looks.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// The table every row's task writes into.
pub(crate) const TABLE: &str = "credentialed";

/// The ASL error a task whose role could not be assumed reports: the work the state named
/// is what did not happen.
pub(crate) const TASK_FAILED_ERROR: &str = "States.TaskFailed";

/// The action the assume the service has to take is subject to, which a trust policy names.
pub(crate) const ASSUME_ACTION: &str = "sts:AssumeRole";

/// Step Functions, IAM and DynamoDB over ONE engine.
///
/// `pub(crate)` because `tests/all/task_credentials_role_trust.rs` holds the rest of this
/// rule — what the TARGET role's trust policy decides — over the same three services:
/// `tests/all.rs` is one crate root, so the wiring is stated here once.
pub(crate) struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One `awsJson` call to one service, requiring a 200. The `Authorization` header's
    /// credential scope is what routes the request and names the Region its ARNs carry.
    async fn json(&self, wire: &Wire, operation: &str, body: &Value) -> Value {
        let authorization = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef",
            wire.signing_name
        );
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{}.{REGION}.amazonaws.com", wire.signing_name),
            )
            .header(
                "content-type",
                format!("application/x-amz-json-{}", wire.json_version),
            )
            .header("x-amz-target", format!("{}.{operation}", wire.target_prefix))
            .header("authorization", authorization)
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request body serializes"),
            ))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
        assert_eq!(
            status, 200,
            "case {operation}: expected 200, got {status} with body {parsed:?}"
        );
        parsed
    }

    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        self.json(&SFN, operation, body).await
    }
    async fn ddb(&self, operation: &str, body: &Value) -> Value {
        self.json(&DYNAMODB, operation, body).await
    }

    /// One `awsQuery` call to IAM, signed with the same credential.
    async fn iam(&self, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "iam.amazonaws.com")
            .header("content-type", "application/x-www-form-urlencoded")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {action}: expected 200 from IAM, got {status} with {text}"
        );
        text
    }

    /// An IAM role trusting `trust`, carrying `policy` as its inline policy where one is
    /// given, by the ARN IAM's own `CreateRole` reported for it.
    pub(crate) async fn role(&self, name: &str, trust: &Value, policy: Option<&Value>) -> String {
        let created = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    ("AssumeRolePolicyDocument", trust.to_string()),
                ],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
        if let Some(policy) = policy {
            self.iam(
                "PutRolePolicy",
                &[
                    ("RoleName", name.to_string()),
                    ("PolicyName", "inline".to_string()),
                    ("PolicyDocument", policy.to_string()),
                ],
            )
            .await;
        }
        arn
    }

    /// Create the table every row writes into, wait until it serves items, and report the
    /// ARN DynamoDB minted for it.
    ///
    /// "You can perform read and write operations only on an ACTIVE table", and a created
    /// one is `CREATING` until the clock makes it active — so the wait is for THAT status
    /// and not for a span.
    pub(crate) async fn create_table(&self) -> String {
        let created = self
            .ddb(
                "CreateTable",
                &json!({
                    "TableName": TABLE,
                    "KeySchema": [{ "AttributeName": "pk", "KeyType": "HASH" }],
                    "AttributeDefinitions": [{ "AttributeName": "pk", "AttributeType": "S" }],
                    "BillingMode": "PAY_PER_REQUEST"
                }),
            )
            .await;
        let arn = created
            .get("TableDescription")
            .and_then(|described| described.get("TableArn"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("CreateTable answers a TableArn, got {created:?}"))
            .to_string();
        let deadline = Instant::now() + PATIENCE;
        loop {
            let described = self
                .ddb("DescribeTable", &json!({ "TableName": TABLE }))
                .await;
            let status = described
                .get("Table")
                .and_then(|table| table.get("TableStatus"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if status == "ACTIVE" {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "expected {TABLE} to become ACTIVE, got {described:?}"
            );
            ::tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// The item stored under `pk`, or `None` when the table holds none.
    pub(crate) async fn item(&self, pk: &str) -> Option<Value> {
        let read = self
            .ddb(
                "GetItem",
                &json!({ "TableName": TABLE, "Key": { "pk": { "S": pk } } }),
            )
            .await;
        read.get("Item").cloned()
    }

    /// Create a state machine under `role_arn` running `definition`, start one execution of
    /// it with `input`, and report `(the execution ARN, what it ended as)` — waiting for the
    /// execution to reach a TERMINAL status rather than for a span.
    pub(crate) async fn ran(
        &self,
        name: &str,
        definition: &Value,
        role_arn: &str,
        input: &Value,
    ) -> (String, Value) {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": role_arn
                }),
            )
            .await;
        let machine = string(&created, "stateMachineArn");
        let started = self
            .sfn(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": "only",
                    "input": input.to_string(),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let deadline = Instant::now() + PATIENCE;
        loop {
            let described = self
                .sfn("DescribeExecution", &json!({ "executionArn": execution }))
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return (execution, described);
            }
            assert!(
                Instant::now() < deadline,
                "expected {execution} to reach a terminal status, got {described:?}"
            );
            ::tokio::time::sleep(POLL_PAUSE).await;
        }
    }
}

/// The trust document that lets THIS service act as a role, which is what an EXECUTION
/// role carries.
pub(crate) fn trusts_the_service() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "states.amazonaws.com" },
            "Action": ASSUME_ACTION
        }]
    })
}

/// The trust document that lets the role `principal` take a role on, which is what a role a
/// `Task` state's `Credentials` names carries: the execution role is the identity the assume
/// is made as, so it is the principal that role has to trust.
pub(crate) fn trusts_the_role(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "AWS": principal },
            "Action": ASSUME_ACTION
        }]
    })
}

/// A policy granting `actions` on `resource` and nothing else.
pub(crate) fn grants(actions: &[&str], resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": resource }]
    })
}

/// The cause Step Functions fails a `Task` state with when the execution role may not take
/// on the role that state's `Credentials` named.
///
/// Recorded behavior: every word of it is fixed — the service assumes the target role
/// itself, so what it reports is its own sentence about that assume — and the only variable
/// parts are the two roles, the state machine's `roleArn` and the role the state resolved.
pub(crate) fn unassumable_cause(execution_role: &str, target: &str) -> String {
    format!(
        "The role {execution_role} is not authorized to assume the task state's role, {target}."
    )
}

/// One row: the case, the `Credentials` block the `Task` state carries, and the query
/// language that state is written in.
struct Row {
    case: &'static str,
    /// Built from the ARN of the role the row names, so each form resolves to the same
    /// role.
    credentials: fn(&str) -> Value,
    jsonata: bool,
}

/// The four forms a `Credentials.RoleArn` can be written in.
///
/// `RoleArn` and `RoleArn.$` are the two fields the member carries
/// (`smithy_query(query="doc",shape="TaskCredentials",member="roleArn")` documents the
/// role itself), and a JSONata state writes the field as that language's own expression.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a literal RoleArn",
            credentials: |role| json!({ "RoleArn": role }),
            jsonata: false,
        },
        Row {
            case: "a RoleArn.$ reference path into the state's input",
            credentials: |_role| json!({ "RoleArn.$": "$.role" }),
            jsonata: false,
        },
        Row {
            case: "a RoleArn.$ intrinsic over a variable an earlier state assigned",
            credentials: |_role| json!({ "RoleArn.$": "States.Format('{}', $role)" }),
            jsonata: false,
        },
        Row {
            case: "a JSONata RoleArn expression",
            credentials: |_role| json!({ "RoleArn": "{% $states.input.role %}" }),
            jsonata: true,
        },
    ]
}

/// The definition a row runs: a `Pass` that assigns the role out of the execution input —
/// which is what the intrinsic form reads — and then the `Task` that writes `pk` into the
/// table under the role its `Credentials` names.
///
/// `credentials` is `None` for the control, whose task is made as the execution role.
pub(crate) fn definition(credentials: Option<Value>, jsonata: bool, pk: &str) -> Value {
    let mut task = match jsonata {
        true => json!({
            "Type": "Task",
            "QueryLanguage": "JSONata",
            "Resource": "arn:aws:states:::dynamodb:putItem",
            "Arguments": {
                "TableName": TABLE,
                "Item": { "pk": { "S": pk } },
            },
            "End": true,
        }),
        false => json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::dynamodb:putItem",
            "Parameters": {
                "TableName": TABLE,
                "Item": { "pk": { "S": pk } },
            },
            "End": true,
        }),
    };
    if let Some(credentials) = credentials {
        task.as_object_mut()
            .expect("the state is an object")
            .insert("Credentials".to_string(), credentials);
    }
    json!({
        "StartAt": "Remember",
        "States": {
            // The variable the intrinsic form reads; every other form ignores it.
            "Remember": {
                "Type": "Pass",
                "Assign": { "role": "{% $states.input.role %}" },
                "QueryLanguage": "JSONata",
                "Next": "Work",
            },
            "Work": task,
        },
    })
}

/// A `Task`'s `Credentials.RoleArn` decides WHO the call is made as: the call is allowed by
/// that role's own policies, which the execution role does not carry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_is_made_under_the_role_its_credentials_names() {
    let chain = Chain::new();
    let table = chain.create_table().await;
    // The EXECUTION role may do exactly one thing: assume the target role. It carries no
    // grant over the table at all, so a call made as IT cannot write one.
    let execution_role = chain
        .role(
            "ExecRole",
            &trusts_the_service(),
            // The one thing this role may do. It carries no grant over the table at all,
            // which is what the control below rests on.
            Some(&grants(&[ASSUME_ACTION], "*")),
        )
        .await;
    // And the TARGET role may write the table and nothing else. Its own trust policy names
    // the execution role, which is the identity the assume is made as.
    let target_role = chain
        .role(
            "TargetRole",
            &trusts_the_role(&execution_role),
            Some(&grants(&["dynamodb:PutItem"], &table)),
        )
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let pk = format!("under-the-role-{index}");
        let (execution, described) = chain
            .ran(
                &format!("credentialed-{index}"),
                &definition(
                    Some((row.credentials)(&target_role)),
                    row.jsonata,
                    &pk,
                ),
                &execution_role,
                &json!({ "role": target_role }),
            )
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the task made under the target role to succeed, got \
             {described:?}"
        );
        assert!(
            chain.item(&pk).await.is_some(),
            "case {case:?}: expected the call made under the target role to have written {pk}"
        );
        // And the history reports the role the task was made under, which is the member
        // `TaskScheduledEventDetails$taskCredentials` carries.
        let history = chain
            .sfn(
                "GetExecutionHistory",
                &json!({ "executionArn": execution, "maxResults": 100 }),
            )
            .await;
        let reported = history
            .get("events")
            .and_then(Value::as_array)
            .unwrap_or(&Vec::new())
            .iter()
            .find_map(|event| {
                Some(
                    event
                        .get("taskScheduledEventDetails")?
                        .get("taskCredentials")?
                        .get("roleArn")?
                        .as_str()?
                        .to_string(),
                )
            });
        assert_eq!(
            reported.as_deref(),
            Some(target_role.as_str()),
            "case {case:?}: expected the scheduled task to report the role it was made \
             under, got {reported:?}"
        );
    }

    // The CONTROL: the same work with no `Credentials` is made as the execution role,
    // whose policies grant nothing over the table — so the call is not made at all.
    let pk = "as-the-execution-role";
    let (_execution, described) = chain
        .ran(
            "uncredentialed",
            &definition(None, false, pk),
            &execution_role,
            &json!({ "role": target_role }),
        )
        .await;
    let case = "no Credentials at all, so the call is made as the execution role";
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("FAILED"),
        "case {case:?}: expected the execution role's own policies to refuse the call, got \
         {described:?}"
    );
    assert_eq!(
        chain.item(pk).await,
        None,
        "case {case:?}: a call that was not made must have written nothing"
    );
}

/// A `Credentials.RoleArn` the execution role may NOT assume fails the task, and no call is
/// made for it: "specify a target role that the state machine's execution role can assume".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_role_the_execution_role_may_not_assume_fails_the_task() {
    let chain = Chain::new();
    let table = chain.create_table().await;
    let execution_role = chain
        .role(
            "ExecRole",
            &trusts_the_service(),
            // A grant over ONE named role, which is not the role the row names below.
            Some(&grants(
                &[ASSUME_ACTION],
                "arn:aws:iam::000000000000:role/Permitted",
            )),
        )
        .await;
    // The role the task asks for: it may write the table, but neither document covers the
    // assume — its trust policy names this service rather than the execution role, and the
    // execution role carries no grant over it either.
    let stranger = chain
        .role(
            "Stranger",
            &trusts_the_service(),
            Some(&grants(&["dynamodb:PutItem"], &table)),
        )
        .await;

    let case = "a role neither the target's trust nor the execution role's grants cover";
    let pk = "never-written";
    let (_execution, described) = chain
        .ran(
            "unassumable-target",
            &definition(Some(json!({ "RoleArn": stranger })), false, pk),
            &execution_role,
            &json!({ "role": stranger }),
        )
        .await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("FAILED"),
        "case {case:?}: expected the task to fail, got {described:?}"
    );
    assert_eq!(
        described.get("error").and_then(Value::as_str),
        Some(TASK_FAILED_ERROR),
        "case {case:?}: expected {TASK_FAILED_ERROR}, got {described:?}"
    );
    let cause = described
        .get("cause")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        cause,
        unassumable_cause(&execution_role, &stranger),
        "case {case:?}: expected this service's own sentence for a target role it could not \
         take on, got {cause:?}"
    );
    assert_eq!(
        chain.item(pk).await,
        None,
        "case {case:?}: a call that was not made must have written nothing"
    );
}
