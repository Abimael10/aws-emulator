// SPDX-License-Identifier: CC0-1.0

//! Where an integration call a `Task` state makes is ADDRESSED, driven over each
//! service's own wire protocol through the production `MultiServiceRouter` with Step
//! Functions, Amazon SQS and IAM mounted on ONE engine — the shape
//! `core/router/src/lib.rs` mounts them in — so the queue a task sends to is the queue
//! SQS's own handlers hold and the policies the call is judged against are the documents
//! IAM's own handlers wrote.
//!
//! RULE: a request that addresses its target by a value carrying the service's own
//! ENDPOINT is made against the Region that endpoint names, so a `Task` state reaches a
//! resource of another Region through it — and the access request it is subject to is
//! over that resource's ARN in THAT Region. A value carrying only a name or an ARN names
//! no endpoint, so such a call is made in the Region the execution runs in.
//!
//! The one member here that carries an endpoint is `SendMessage`'s `QueueUrl` — "the URL
//! of the Amazon SQS queue to which a message is sent"
//! (`smithy_query(query="operation",operation="SendMessage",model_path="services/
//! service-com-amazonaws-sqs-amazonsqs/smithy-model/com-amazonaws-sqs-amazonsqs.json")`),
//! whose authority is that service's regional endpoint and whose last path segment is the
//! queue's own name.
//!
//! The tables span what the rule generalises over: the REGION the queue lives in (the
//! execution's own and three others, from three continents), the FORM of the url (a
//! regional endpoint host, and the legacy host that names no Region at all), the
//! integration FAMILY and the member SPELLING each family writes, and — for the
//! authorization half — which Region the grant on the queue names. A row's far queue has
//! a DECOY of the same name in the execution's own Region, so a message that went to the
//! near queue instead of the far one fails the row rather than passing it.
//!
//! The url a SERVED emulator reports for a queue names no Region at all — its authority is
//! the one address the emulator answers on — so where such a url is addressed is the
//! subject of `tests/all/served_queue_url_addressing.rs`, which borrows this harness.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::state_machine_lifecycle::{string, ACCOUNT, REGION};
use crate::task_execution_role::trust_document;
use crate::task_refusal_session_principal::{definition, session_principal};

/// How long a row waits for an execution to leave `RUNNING`. Generous, so a loaded host
/// cannot fail a row the emulator is still working through.
pub(crate) const PATIENCE: Duration = Duration::from_secs(20);

/// How long one look at a queue waits before looking again.
const POLL: Duration = Duration::from_millis(25);

/// The body every row's message carries, so a row reads back what it sent.
pub(crate) const BODY: &str = "cross-region";

/// Step Functions, Amazon SQS and IAM over ONE engine.
pub(crate) struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    pub(crate) fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// The same emulator, SERVED at `endpoint` — the state starting a data port there
    /// leaves it in (`MultiServiceRouter::set_url_base`), which is what makes the address
    /// every service announces its own rather than AWS's published host.
    pub(crate) fn served_at(endpoint: &str) -> Self {
        let base = EndpointUrl::parse(endpoint)
            .unwrap_or_else(|| panic!("a data port publishes a bare https url, not {endpoint:?}"));
        let mut emulator = Self::new();
        emulator.router.set_url_base(base);
        emulator
    }

    /// One `awsJson` RPC call to the service whose signing name is `signing_name`, in
    /// `region` — the Region is the credential scope's, which is what the service reads a
    /// request's Region from.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        region: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let host = format!("{signing_name}.{region}.amazonaws.com");
        let authorization = format!(
            "AWS4-HMAC-SHA256 \
             Credential=AKIDEXAMPLE/20260101/{region}/{signing_name}/aws4_request, \
             SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
        );
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", &host)
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
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
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// Call one Step Functions operation in the Region every state machine here lives in,
    /// requiring a 200.
    async fn sfn(&self, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .rpc("states", "AWSStepFunctions", REGION, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// Call one Amazon SQS operation IN `region`, requiring a 200.
    async fn sqs_in(&self, region: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .rpc("sqs", "AmazonSQS", region, operation, body)
            .await;
        assert_eq!(
            status, 200,
            "{operation} in {region}: expected 200, got {status} with body {parsed}"
        );
        parsed
    }

    /// One awsQuery call to IAM.
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
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
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
            "{action}: expected 200 from IAM, got {status} with {text}"
        );
        text
    }

    /// An IAM role carrying `policy` as its inline policy, by its own ARN.
    pub(crate) async fn role(&self, name: &str, policy: &Value) -> String {
        let created = self
            .iam(
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    ("AssumeRolePolicyDocument", trust_document().to_string()),
                ],
            )
            .await;
        let arn = xml_element(&created, "Arn")
            .unwrap_or_else(|| panic!("CreateRole answers the role Arn, got {created}"));
        self.iam(
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "inline".to_string()),
                ("PolicyDocument", policy.to_string()),
            ],
        )
        .await;
        arn
    }

    /// The queue named `name` in `region`, as that Region's own `CreateQueue` answers its
    /// url.
    pub(crate) async fn queue_in(&self, region: &str, name: &str) -> String {
        let created = self
            .sqs_in(region, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        string(&created, "QueueUrl")
    }

    /// Create a state machine under `role_arn` and return its ARN.
    pub(crate) async fn machine(&self, name: &str, definition: &str, role_arn: &str) -> String {
        let created = self
            .sfn(
                "CreateStateMachine",
                &json!({ "name": name, "definition": definition, "roleArn": role_arn }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// Start one execution and report it once it has left `RUNNING`.
    pub(crate) async fn ran(&self, machine_arn: &str, name: &str, case: &str) -> Value {
        let started = self
            .sfn(
                "StartExecution",
                &json!({ "stateMachineArn": machine_arn, "name": name }),
            )
            .await;
        let execution_arn = string(&started, "executionArn");
        let began = Instant::now();
        loop {
            let described = self
                .sfn(
                    "DescribeExecution",
                    &json!({ "executionArn": execution_arn }),
                )
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, got {described:?}"
            );
            ::tokio::time::sleep(POLL).await;
        }
    }

    /// The bodies the queue at `url` holds, read in `region`: looked at again until one
    /// arrives or `PATIENCE` runs out, so a row waits for the CONDITION rather than for a
    /// span. What the caller does with an empty answer is its own assertion.
    pub(crate) async fn bodies_in(&self, region: &str, url: &str) -> Vec<String> {
        let began = Instant::now();
        loop {
            let received = self
                .sqs_in(
                    region,
                    "ReceiveMessage",
                    &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
                )
                .await;
            let bodies: Vec<String> = received
                .get("Messages")
                .and_then(Value::as_array)
                .map(|messages| {
                    messages
                        .iter()
                        .map(|message| string(message, "Body"))
                        .collect()
                })
                .unwrap_or_default();
            if !bodies.is_empty() || began.elapsed() >= PATIENCE {
                return bodies;
            }
            ::tokio::time::sleep(POLL).await;
        }
    }

    /// How many messages the queue at `url` holds in `region`, as its own attribute
    /// reports: the reading for a queue that must stay EMPTY, which no receive can prove.
    pub(crate) async fn depth_in(&self, region: &str, url: &str) -> String {
        let reported = self
            .sqs_in(
                region,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["ApproximateNumberOfMessages"] }),
            )
            .await;
        reported
            .get("Attributes")
            .and_then(|attributes| attributes.get("ApproximateNumberOfMessages"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The ARN of the queue named `name` in `region`, which is the resource
/// `sqs:SendMessage` on it is taken on.
pub(crate) fn queue_arn(region: &str, name: &str) -> String {
    format!("arn:aws:sqs:{region}:{ACCOUNT}:{name}")
}

/// A policy granting `sqs:SendMessage` on `resource` alone.
pub(crate) fn send_grant(resource: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sqs:SendMessage",
            "Resource": resource,
        }],
    })
}

/// Which integration family a row names the send through, which is also what decides the
/// SPELLING of the members its `Parameters` carry: the optimized integration documents
/// them in its own PascalCase, and the AWS SDK family takes the invoked operation's own
/// members, which either spelling reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    Optimized,
    AwsSdk,
}

impl Family {
    /// The `Resource` a `Task` of this family names.
    pub(crate) fn resource(self) -> &'static str {
        match self {
            Self::Optimized => "arn:aws:states:::sqs:sendMessage",
            Self::AwsSdk => "arn:aws:states:::aws-sdk:sqs:sendMessage",
        }
    }

    /// The parameters it carries for a send of `BODY` to `url`.
    pub(crate) fn parameters(self, url: &str) -> Value {
        match self {
            Self::Optimized => json!({ "QueueUrl": url, "MessageBody": BODY }),
            Self::AwsSdk => json!({ "queueUrl": url, "messageBody": BODY }),
        }
    }

    /// The error a `Task` of this family reports for a refusal Amazon SQS answered: that
    /// service's base SDK exception, named as the client this family is served by names
    /// the service — the earlier client's own name through an optimized `Resource`, and
    /// the current AWS SDK's name through the AWS SDK family.
    pub(crate) fn refusal_error(self) -> &'static str {
        match self {
            Self::Optimized => "SQS.AmazonSQSException",
            Self::AwsSdk => "Sqs.AmazonSQSException",
        }
    }
}

/// RULE: the send reaches the queue the url's own endpoint names, whatever Region the
/// execution runs in.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_send_is_addressed_to_the_region_the_queue_url_names() {
    // (case, the Region the queue lives in, the family the Task names it through)
    let rows: [(&str, &str, Family); 5] = [
        (
            "the execution's own Region, which the url names like any other",
            REGION,
            Family::Optimized,
        ),
        ("another Region of the same continent", "us-east-1", Family::Optimized),
        ("a Region on another continent, through the AWS SDK family", "eu-west-1", Family::AwsSdk),
        ("a Region on a third continent", "ap-southeast-2", Family::Optimized),
        ("a fourth, through the AWS SDK family", "sa-east-1", Family::AwsSdk),
    ];

    let emulator = Emulator::new();

    for (index, (case, queue_region, family)) in rows.into_iter().enumerate() {
        let name = format!("addressed-{index}");
        let url = emulator.queue_in(queue_region, &name).await;
        // The url a client is handed carries the Region of the create, which is what
        // addresses the send.
        assert!(
            url.contains(&format!("sqs.{queue_region}.")),
            "case {case:?}: expected a url on {queue_region}'s endpoint, got {url:?}"
        );
        // A queue of the SAME NAME in the execution's own Region, so a send resolved
        // against the call's Region instead of the url's reaches the wrong queue and the
        // row fails.
        let decoy = (queue_region != REGION).then_some(());
        let decoy_url = match decoy {
            Some(()) => Some(emulator.queue_in(REGION, &name).await),
            None => None,
        };

        let role_arn = emulator
            .role(
                &format!("AddressedRole{index}"),
                &send_grant(&queue_arn(queue_region, &name)),
            )
            .await;
        let machine = emulator
            .machine(
                &format!("addressed-{index}"),
                &definition(family.resource(), &family.parameters(&url)),
                &role_arn,
            )
            .await;
        let described = emulator.ran(&machine, "only", case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("SUCCEEDED"), None),
            "case {case:?}: expected SUCCEEDED, got {described:?}"
        );

        let bodies = emulator.bodies_in(queue_region, &url).await;
        assert_eq!(
            bodies,
            vec![BODY.to_string()],
            "case {case:?}: expected the message in the queue of {queue_region}, got {bodies:?}"
        );
        if let Some(decoy_url) = decoy_url {
            let depth = emulator.depth_in(REGION, &decoy_url).await;
            assert_eq!(
                depth, "0",
                "case {case:?}: expected the queue of the same name in {REGION} to stay empty, \
                 got {depth} message(s) in {decoy_url:?}"
            );
        }
    }
}

/// RULE, at its boundary: a url whose host names NO Region names no endpoint either, so
/// the send is made in the Region the execution runs in.
///
/// The legacy host Amazon SQS publishes (`https://queue.amazonaws.com/<account>/<name>`)
/// is such a url: its authority carries no Region label at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_url_naming_no_region_is_addressed_where_the_execution_runs() {
    let emulator = Emulator::new();
    let case = "the legacy host, which names no Region";
    let name = "legacy-host";
    // The queue really lives in the execution's own Region; the url the definition
    // carries names the same account and name on a host with no Region label.
    let url = emulator.queue_in(REGION, name).await;
    let legacy = format!("https://queue.amazonaws.com/{ACCOUNT}/{name}");

    let role_arn = emulator
        .role("LegacyHostRole", &send_grant(&queue_arn(REGION, name)))
        .await;
    let machine = emulator
        .machine(
            "legacy-host",
            &definition(
                Family::Optimized.resource(),
                &Family::Optimized.parameters(&legacy),
            ),
            &role_arn,
        )
        .await;
    let described = emulator.ran(&machine, "only", case).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected SUCCEEDED, got {described:?}"
    );
    let bodies = emulator.bodies_in(REGION, &url).await;
    assert_eq!(
        bodies,
        vec![BODY.to_string()],
        "case {case:?}: expected the message in the queue of {REGION}, got {bodies:?}"
    );
}

/// RULE, its authorization half: the access request the send is subject to is over the
/// queue's ARN in the Region its url names, so a grant on that ARN allows the call and a
/// grant on the same name in another Region does not — and the refusal is the one Amazon
/// SQS itself would have answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_far_queues_own_arn_is_what_the_send_is_authorized_over() {
    /// What a row grants `sqs:SendMessage` on.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Grant {
        /// The far queue's own ARN, in the Region its url names.
        FarQueue,
        /// The same queue NAME in the Region the execution runs in: another resource.
        NearQueue,
        /// Nothing about the action at all.
        Unrelated,
    }

    // (case, the Region the queue lives in, what the role grants, whether the call is made)
    let rows: [(&str, &str, Grant, bool); 4] = [
        ("the far queue's own ARN", "us-east-1", Grant::FarQueue, true),
        (
            "the same name in the execution's Region, which is another queue",
            "us-east-1",
            Grant::NearQueue,
            false,
        ),
        ("nothing about the action", "eu-west-1", Grant::Unrelated, false),
        (
            "the far queue's own ARN, on a third continent",
            "ap-northeast-1",
            Grant::FarQueue,
            true,
        ),
    ];

    let emulator = Emulator::new();

    for (index, (case, queue_region, grant, allowed)) in rows.into_iter().enumerate() {
        let name = format!("authorized-{index}");
        let url = emulator.queue_in(queue_region, &name).await;
        let far_arn = queue_arn(queue_region, &name);
        let policy = match grant {
            Grant::FarQueue => send_grant(&far_arn),
            Grant::NearQueue => send_grant(&queue_arn(REGION, &name)),
            // The log writes a state machine makes, which say nothing about any send.
            Grant::Unrelated => json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Effect": "Allow",
                    "Action": ["logs:CreateLogDelivery", "logs:PutLogEvents"],
                    "Resource": "*",
                }],
            }),
        };
        let role_arn = emulator
            .role(&format!("AuthorizedRole{index}"), &policy)
            .await;
        let machine = emulator
            .machine(
                &format!("authorized-{index}"),
                &definition(
                    Family::Optimized.resource(),
                    &Family::Optimized.parameters(&url),
                ),
                &role_arn,
            )
            .await;
        let described = emulator.ran(&machine, "only", case).await;

        if allowed {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: expected SUCCEEDED, got {described:?}"
            );
            let bodies = emulator.bodies_in(queue_region, &url).await;
            assert_eq!(
                bodies,
                vec![BODY.to_string()],
                "case {case:?}: expected the message in the queue of {queue_region}, got \
                 {bodies:?}"
            );
            continue;
        }

        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some("SQS.AmazonSQSException")),
            "case {case:?}: expected the send to be refused, got {described:?}"
        );
        // The refusal is Amazon SQS's own answer to an unauthorized call: its sentence
        // names the session the call was made as, the action in the lower case that
        // service words a refusal in, and the FAR queue's ARN; the metadata names that
        // service's client, the status it answers an authorization failure with and the
        // code it names the class by.
        let cause = string(&described, "cause");
        let principal = session_principal(&role_arn, &cause);
        let expected = format!(
            "User: {principal} is not authorized to perform: sqs:sendmessage on resource: \
             {far_arn} because no identity-based policy allows the sqs:sendmessage action \
             (Service: AmazonSQS; Status Code: 403; Error Code: AccessDenied; "
        );
        assert!(
            cause.starts_with(&expected) && cause.ends_with("Proxy: null)"),
            "case {case:?}: expected the cause to open {expected:?} and close \"Proxy: null)\", \
             got {cause:?}"
        );
        // And nothing was sent: the call the policies withheld was never made.
        let depth = emulator.depth_in(queue_region, &url).await;
        assert_eq!(
            depth, "0",
            "case {case:?}: expected the far queue to stay empty, got {depth} message(s)"
        );
    }
}
