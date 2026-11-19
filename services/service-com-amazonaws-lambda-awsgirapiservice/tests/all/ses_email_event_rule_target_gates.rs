// SPDX-License-Identifier: CC0-1.0

//! Which of a rule's Lambda targets an Amazon SES email event actually REACHES, end to end
//! over the wire: SES v2 publishes a configuration set's events onto the account's event bus,
//! EventBridge matches each rule against the envelope and hands it to that rule's targets, and
//! the function's own resource policy decides whether the invocation is made at all.
//!
//! The RULE the rows pin, one level above the two recorded cases:
//!
//! > An email event published by a configuration set's EventBridge destination invokes a
//! > rule's function target only when BOTH gates pass: the rule's own `EventPattern` matches
//! > the published envelope, and the FUNCTION's resource-based policy grants
//! > `lambda:InvokeFunction` to EventBridge's service principal for that rule. A rule whose
//! > pattern does not match the envelope, and a function whose policy does not admit this
//! > service's invocation for the rule being delivered, are each handed nothing — silently,
//! > with the send itself unaffected — while every other rule and target of the same event is
//! > delivered to as usual.
//!
//! The rows span what that rule generalises over: on the POLICY side, what the target grants
//! (nothing at all, the invoke to this service, the invoke to another service, the invoke
//! scoped by `aws:SourceArn` to the rule being delivered, and the same scoped to a different
//! rule); on the PATTERN side, which part of the envelope the pattern names (its source, its
//! `detail-type`, a field inside its `detail`), each once matching and once naming something
//! the envelope does not carry.
//!
//! Each row drives the whole pipeline — six crates on ONE engine, exactly as
//! `core/router/src/lib.rs` mounts them — and observes the outcome where a client observes
//! one: on the queue the invoked handler relays what it was handed onto.
//!
//! WHY the absence a negative row asserts is not a race: both gates are decided INSIDE the
//! `PutEvents` the publication makes, before that call answers, and the acceptance event
//! (`Email Sent`) of one send is published and fully dispatched before that send's delivery
//! event (`Email Delivered`). Every row therefore sends several times and waits for the
//! CONTROL rule — a second rule, over the delivery event, whose own target is granted the
//! invoke — to have relayed every generation before it reads the row's queue: by then each
//! generation's acceptance event has been dispatched, and the generations after the first
//! have given an invocation that should not have been made every chance to land.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_sesv2_simpleemailservice_v2::state::ComAmazonawsSesv2SimpleemailserviceV2State;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The sender every send is from, and the domain identity that authorises it.
const SENDER: &str = "sender@senders.example.com";
const SENDER_DOMAIN: &str = "senders.example.com";

/// The mailbox-simulator address every send is to: it accepts the message, so one send
/// produces the acceptance event the rows are about and the delivery event the control rule
/// reads.
const RECIPIENT: &str = "success@simulator.amazonses.com";

/// The `source` Amazon SES publishes a configuration set's email events under.
const SES_EVENT_SOURCE: &str = "aws.ses";

/// The `detail-type` of the acceptance event — the one the rule under test selects — and of
/// the delivery event the control rule selects.
const ACCEPTANCE_DETAIL_TYPE: &str = "Email Sent";
const DELIVERY_DETAIL_TYPE: &str = "Email Delivered";

/// The principal EventBridge invokes a resource-policy-governed target as, and the action
/// such an invocation performs.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `StatementId` every grant a row writes carries, which is what the next row removes it
/// by.
const GRANT_STATEMENT_ID: &str = "AllowTheInvoke";

/// Another service's principal, which is what the row granting the invoke to somebody else
/// grants it to.
const OTHER_PRINCIPAL: &str = "s3.amazonaws.com";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what the
/// handler needs on any host.
const FUNCTION_TIMEOUT: u64 = 60;

/// How long each wait for work the clock owes — the function becoming `Active`, the
/// publication, the delivery and the handler's own relay — looks before giving up, and how
/// long between two looks. The deadline is generous: what is under test is that the work
/// happens at all, never how fast this host gets to it.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How many sends each row makes. More than one, for the reason this module's own
/// documentation gives: the control rule having relayed every generation is what tells a
/// negative row that the acceptance event of the FIRST generation was dispatched and its
/// verdict taken.
const GENERATIONS: usize = 3;

/// The handler every function deploys. It relays the WHOLE envelope EventBridge handed it onto
/// the queue named in its environment, so what a target was invoked with — and whether it was
/// invoked at all — is readable through Amazon SQS afterwards.
const RELAY_SOURCE: &str = r#"
import json
import os

import boto3


def lambda_handler(event, context):
    boto3.client("sqs").send_message(
        QueueUrl=os.environ["RELAY_QUEUE_URL"],
        MessageBody=json.dumps(event, sort_keys=True),
    )
    return {"relayed": event["detail-type"]}
"#;

/// What the function a rule targets grants EventBridge.
#[derive(Clone, Copy)]
enum Grant {
    /// No resource policy at all: the function has granted this service nothing.
    None,
    /// The invoke, to the service principal named here, with no condition.
    To(&'static str),
    /// The invoke to EventBridge, scoped by `aws:SourceArn` to the rule under test.
    ScopedToTheRule,
    /// The same, scoped to the OTHER rule of the same bus.
    ScopedToAnotherRule,
}

/// One row of the table.
struct Row {
    case: &'static str,
    /// The `EventPattern` the rule under test carries. Takes nothing: every row's pattern is
    /// written over the acceptance event's own envelope.
    pattern: fn() -> Value,
    /// What the function that rule targets grants.
    grant: Grant,
    /// Whether that function must be invoked with the acceptance event of every send.
    invoked: bool,
}

/// The pattern a rule carrying the matching one over the acceptance event names: this
/// service's source, the acceptance event's own `detail-type`, and the `eventType` its detail
/// is of.
fn matching_pattern() -> Value {
    json!({
        "source": [SES_EVENT_SOURCE],
        "detail-type": [ACCEPTANCE_DETAIL_TYPE],
        "detail": {"eventType": ["Send"]},
    })
}

const ROWS: &[Row] = &[
    Row {
        case: "the pattern matches and the function grants this service the invoke",
        pattern: matching_pattern,
        grant: Grant::To(EVENTS_PRINCIPAL),
        invoked: true,
    },
    Row {
        case: "the grant is scoped to the ARN of the rule being delivered",
        pattern: matching_pattern,
        grant: Grant::ScopedToTheRule,
        invoked: true,
    },
    Row {
        case: "the function carries no resource policy at all",
        pattern: matching_pattern,
        grant: Grant::None,
        invoked: false,
    },
    Row {
        case: "the function grants the invoke to another service's principal",
        pattern: matching_pattern,
        grant: Grant::To(OTHER_PRINCIPAL),
        invoked: false,
    },
    Row {
        case: "the grant is scoped to another rule's ARN",
        pattern: matching_pattern,
        grant: Grant::ScopedToAnotherRule,
        invoked: false,
    },
    Row {
        case: "the pattern names another event source",
        pattern: || {
            json!({
                "source": ["aws.ec2"],
                "detail-type": [ACCEPTANCE_DETAIL_TYPE],
            })
        },
        grant: Grant::To(EVENTS_PRINCIPAL),
        invoked: false,
    },
    Row {
        case: "the pattern names another email event's detail-type",
        pattern: || {
            json!({
                "source": [SES_EVENT_SOURCE],
                "detail-type": ["Email Bounced"],
            })
        },
        grant: Grant::To(EVENTS_PRINCIPAL),
        invoked: false,
    },
    Row {
        case: "the pattern filters on a detail value the envelope does not carry",
        pattern: || {
            json!({
                "source": [SES_EVENT_SOURCE],
                "detail-type": [ACCEPTANCE_DETAIL_TYPE],
                "detail": {"eventType": ["Bounce"]},
            })
        },
        grant: Grant::To(EVENTS_PRINCIPAL),
        invoked: false,
    },
];

/// The six services this pipeline needs on ONE engine: SES v2 publishes, EventBridge matches
/// and hands over, Lambda runs the handler and supplies it an SDK, IAM holds the execution
/// role's grants, the token service vends the session the handler's call is signed as, and
/// Amazon SQS serves the relay queues.
struct Pipeline {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Pipeline {
    fn new() -> Self {
        let engine = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These cases exercise the services, not SigV4: the credential scope is all the
        // header carries. The handler's OWN call is signed by the runtime with the credential
        // it was given.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Pipeline { router, engine }
    }

    /// One answered request: its status, headers and parsed body.
    async fn answered(
        &self,
        request: http::Request<bytes::Bytes>,
    ) -> (u16, http::HeaderMap, Value) {
        let response = self.router.dispatch(request).await;
        let (parts, body) = response.into_parts();
        let parsed = if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body)
                .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
        };
        (parts.status.as_u16(), parts.headers, parsed)
    }

    /// One `X-Amz-Target` RPC call that must be served.
    async fn rpc(
        &self,
        case: &str,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization(signing_name))
            .body(bytes::Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let (status, _headers, parsed) = self.answered(request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {signing_name}:{operation} must be served, got {status} {parsed}"
        );
        parsed
    }

    async fn events(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "events", "AWSEvents", "1.1", operation, body)
            .await
    }

    async fn sqs(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "sqs", "AmazonSQS", "1.0", operation, body)
            .await
    }

    /// One call on SES v2's restJson1 surface that must be served.
    async fn ses(&self, case: &str, method: &str, path: &str, body: Option<Value>) -> Value {
        let payload = match &body {
            Some(value) => bytes::Bytes::from(value.to_string()),
            None => bytes::Bytes::new(),
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://email.{REGION}.amazonaws.com{path}"))
            .header("host", format!("email.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("ses"))
            .body(payload)
            .expect("the test request is well formed");
        let (status, _headers, parsed) = self.answered(request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: SES {method} {path} must be served, got {status} {parsed}"
        );
        parsed
    }

    /// One Lambda restJson1 call, as its status and parsed body.
    async fn lambda(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let payload = if body.is_null() {
            bytes::Bytes::new()
        } else {
            bytes::Bytes::from(body.to_string())
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://lambda.{REGION}.amazonaws.com{path}"))
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", authorization("lambda"))
            .body(payload)
            .expect("the test request is well formed");
        let (status, _headers, parsed) = self.answered(request).await;
        (status, parsed)
    }

    /// One awsQuery call to IAM that must be served.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) {
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
            .header("authorization", authorization("iam"))
            .body(bytes::Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: iam:{action} must be served, got {status} {answered}"
        );
    }

    /// The account with a verified sending domain, so every send below is authorised.
    ///
    /// The verification is a transition the CLOCK owes the identity, so it is brought forward
    /// on the record rather than waited out: what this suite is about happens after the send.
    async fn given_verified_sender(&self, case: &str) {
        self.ses(
            case,
            "POST",
            "/v2/email/identities",
            Some(json!({"EmailIdentity": SENDER_DOMAIN})),
        )
        .await;
        let state = self
            .engine
            .state::<ComAmazonawsSesv2SimpleemailserviceV2State>();
        let mut store = state.lock();
        let past = SystemTime::now()
            .checked_sub(Duration::from_secs(1))
            .expect("an instant before now");
        for record in store.identities.values_mut() {
            record.verification_due_at = past;
        }
        store.settle(SystemTime::now());
    }

    /// A configuration set whose one event destination publishes the acceptance and delivery
    /// events of every send onto the account's default bus.
    async fn given_destination(&self, case: &str, set: &str) {
        self.ses(
            case,
            "POST",
            "/v2/email/configuration-sets",
            Some(json!({"ConfigurationSetName": set})),
        )
        .await;
        self.ses(
            case,
            "POST",
            &format!("/v2/email/configuration-sets/{set}/event-destinations"),
            Some(json!({
                "EventDestinationName": "relay-bus",
                "EventDestination": {
                    "Enabled": true,
                    "MatchingEventTypes": ["SEND", "DELIVERY"],
                    "EventBridgeDestination": {"EventBusArn": default_bus_arn()},
                },
            })),
        )
        .await;
    }

    /// A relay queue an invoked handler sends what it was handed onto; answers its url and
    /// ARN.
    async fn given_queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({"QueueName": name}))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateQueue reported no QueueUrl: {created}"))
            .to_string();
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: GetQueueAttributes reported no QueueArn: {attributes}")
            })
            .to_string();
        (url, arn)
    }

    /// The execution role both functions run as: trusted by Lambda, and carrying the one grant
    /// a handler's own call needs. Answers the role's ARN.
    async fn given_execution_role(&self, case: &str, name: &str, queue_arns: &[&str]) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": {"Service": "lambda.amazonaws.com"},
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        self.iam(
            case,
            "CreateRole",
            &[
                ("RoleName", name.to_string()),
                ("AssumeRolePolicyDocument", trust),
            ],
        )
        .await;
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Action": "sqs:SendMessage",
                "Resource": queue_arns,
            }],
        })
        .to_string();
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "relay-the-event".to_string()),
                ("PolicyDocument", document),
            ],
        )
        .await;
        format!("arn:aws:iam::{}:role/{name}", account())
    }

    /// A relay function past the transition its creation armed; answers its ARN.
    async fn given_function(
        &self,
        case: &str,
        name: &str,
        role_arn: &str,
        relay_url: &str,
    ) -> String {
        let (status, created) = self
            .lambda(
                "POST",
                "/2015-03-31/functions",
                &json!({
                    "FunctionName": name,
                    "Role": role_arn,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {"RELAY_QUEUE_URL": relay_url}},
                    "Code": {"ZipFile": package()},
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: CreateFunction must answer 201, got {status} {created}"
        );
        let arn = created["FunctionArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateFunction reported no ARN: {created}"))
            .to_string();
        let deadline = Instant::now() + BUDGET;
        loop {
            let (_, configuration) = self
                .lambda(
                    "GET",
                    &format!("/2015-03-31/functions/{name}/configuration"),
                    &Value::Null,
                )
                .await;
            if configuration["State"].as_str() == Some("Active") {
                return arn;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: Lambda never reported {name:?} Active, last read {configuration}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// Grant `principal` the invocation on `function`, optionally scoped to the resource
    /// `source_arn` names: "for Lambda functions, Amazon SNS topics and Amazon SQS queues,
    /// EventBridge relies on resource-based policies" (`PutTargets`), and `AddPermission`
    /// is how a client writes one.
    async fn given_invoke_grant(
        &self,
        case: &str,
        function: &str,
        principal: &str,
        source_arn: Option<&str>,
    ) {
        let mut body = json!({
            "StatementId": GRANT_STATEMENT_ID,
            "Action": INVOKE_ACTION,
            "Principal": principal,
        });
        if let Some(source_arn) = source_arn {
            body["SourceArn"] = json!(source_arn);
        }
        let (status, granted) = self
            .lambda(
                "POST",
                &format!("/2015-03-31/functions/{function}/policy"),
                &body,
            )
            .await;
        assert_eq!(
            status, 201,
            "case {case:?}: AddPermission must answer 201, got {status} {granted}"
        );
    }

    /// A rule of the default bus carrying `pattern`, with `function_arn` as its one target;
    /// answers the rule's own ARN as the service minted it.
    async fn given_rule(
        &self,
        case: &str,
        name: &str,
        pattern: &Value,
        function_arn: &str,
    ) -> String {
        let put = self
            .events(
                case,
                "PutRule",
                &json!({
                    "Name": name,
                    "EventPattern": pattern.to_string(),
                    "State": "ENABLED",
                }),
            )
            .await;
        let rule_arn = put["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: PutRule reported no RuleArn: {put}"))
            .to_string();
        let answered = self
            .events(
                case,
                "PutTargets",
                &json!({"Rule": name, "Targets": [{"Id": "relay", "Arn": function_arn}]}),
            )
            .await;
        assert_eq!(
            answered["FailedEntryCount"],
            json!(0),
            "case {case:?}: PutTargets reported a failed entry: {answered}"
        );
        rule_arn
    }

    /// `SendEmail` through `set`, answering the `MessageId` it minted.
    async fn send(&self, case: &str, set: &str) -> String {
        let answered = self
            .ses(
                case,
                "POST",
                "/v2/email/outbound-emails",
                Some(json!({
                    "FromEmailAddress": SENDER,
                    "Destination": {"ToAddresses": [RECIPIENT]},
                    "ConfigurationSetName": set,
                    "Content": {"Simple": {
                        "Subject": {"Data": "a message under test"},
                        "Body": {"Text": {"Data": "hello"}},
                    }},
                })),
            )
            .await;
        let message_id = answered["MessageId"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !message_id.is_empty(),
            "case {case:?}: an accepted send must answer a MessageId, got {answered}"
        );
        message_id
    }

    /// Take the grant `given_invoke_grant` wrote off `function` again, so the next row's own
    /// grant shape is the only statement the function's policy carries. A function that
    /// carries no such statement answers `ResourceNotFoundException`, which is the same
    /// starting point and is accepted here as one.
    async fn revoke_invoke_grant(&self, case: &str, function: &str) {
        let (status, answered) = self
            .lambda(
                "DELETE",
                &format!("/2015-03-31/functions/{function}/policy/{GRANT_STATEMENT_ID}"),
                &Value::Null,
            )
            .await;
        assert!(
            status == 204 || status == 404,
            "case {case:?}: RemovePermission must either remove the statement or report none, \
             got {status} {answered}"
        );
    }

    /// Take everything waiting on `url` off it, so what a row reads afterwards is its own.
    async fn emptied(&self, case: &str, url: &str) {
        while !self.relayed(case, url).await.is_empty() {}
    }

    /// Every envelope waiting on `url` right now.
    async fn relayed(&self, case: &str, url: &str) -> Vec<Value> {
        let answered = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
            )
            .await;
        answered["Messages"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|message| {
                let body = message["Body"].as_str().unwrap_or_default();
                serde_json::from_str::<Value>(body).unwrap_or_else(|error| {
                    panic!("case {case:?}: a relayed event is not JSON ({error}): {body:?}")
                })
            })
            .collect()
    }

    /// Drain `url` until it has carried `wanted` envelopes, or the budget is spent.
    async fn drained(&self, case: &str, url: &str, wanted: usize) -> Vec<Value> {
        let deadline = Instant::now() + BUDGET;
        let mut held: Vec<Value> = Vec::new();
        while held.len() < wanted {
            held.extend(self.relayed(case, url).await);
            if held.len() >= wanted || Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
        held
    }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAEMULATOR0000000/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=00"
    )
}

/// The account every emulated resource of this suite belongs to, as the engine holds it.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN of the account's default event bus in this Region.
fn default_bus_arn() -> String {
    format!("arn:aws:events:{REGION}:{}:event-bus/default", account())
}

/// `application/x-www-form-urlencoded` for one awsQuery parameter value.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(char::from(byte));
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// A .zip deployment package holding the relay handler as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries
/// it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(RELAY_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// Both gates, over every shape the rule generalises over.
///
/// ONE emulator serves every row, and each row REPLACES the configuration the rule is about —
/// its rule's own `EventPattern`, and the one statement its target's resource policy carries —
/// which is how a client changes either: `PutRule` over an existing rule, and
/// `AddPermission`/`RemovePermission` over an existing function. So the rows also pin that a
/// reconfiguration decides the NEXT event rather than the configuration the rule was created
/// with, and the suite runs two handlers rather than two per row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_email_event_reaches_a_rules_function_only_through_both_gates() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let setting_up = "the pipeline every row drives";
    let pipeline = Pipeline::new();
    let (row_url, row_queue_arn) = pipeline.given_queue(setting_up, "row-relay").await;
    let (control_url, control_queue_arn) = pipeline.given_queue(setting_up, "control-relay").await;
    let role_arn = pipeline
        .given_execution_role(
            setting_up,
            "ses-event-relay-role",
            &[&row_queue_arn, &control_queue_arn],
        )
        .await;
    let row_function = "row-relay-fn";
    let control_function = "control-relay-fn";
    let row_function_arn = pipeline
        .given_function(setting_up, row_function, &role_arn, &row_url)
        .await;
    let control_function_arn = pipeline
        .given_function(setting_up, control_function, &role_arn, &control_url)
        .await;
    pipeline.given_verified_sender(setting_up).await;
    pipeline.given_destination(setting_up, "relay-set").await;
    // The CONTROL rule, whose own target is granted the invoke and whose pattern selects every
    // send's delivery event. Its ARN is also the one the "scoped to another rule" row names: a
    // rule of the same bus that the row's own delivery is not made for.
    let control_rule_arn = pipeline
        .given_rule(
            setting_up,
            "control-rule",
            &json!({
                "source": [SES_EVENT_SOURCE],
                "detail-type": [DELIVERY_DETAIL_TYPE],
            }),
            &control_function_arn,
        )
        .await;
    pipeline
        .given_invoke_grant(setting_up, control_function, EVENTS_PRINCIPAL, None)
        .await;

    for row in ROWS {
        let case = row.case;
        let row_rule_arn = pipeline
            .given_rule(case, "row-rule", &(row.pattern)(), &row_function_arn)
            .await;
        pipeline.revoke_invoke_grant(case, row_function).await;
        match row.grant {
            Grant::None => {}
            Grant::To(principal) => {
                pipeline
                    .given_invoke_grant(case, row_function, principal, None)
                    .await;
            }
            Grant::ScopedToTheRule => {
                pipeline
                    .given_invoke_grant(case, row_function, EVENTS_PRINCIPAL, Some(&row_rule_arn))
                    .await;
            }
            Grant::ScopedToAnotherRule => {
                pipeline
                    .given_invoke_grant(
                        case,
                        row_function,
                        EVENTS_PRINCIPAL,
                        Some(&control_rule_arn),
                    )
                    .await;
            }
        }
        // What the previous row left behind, so what this row reads is its own.
        pipeline.emptied(case, &row_url).await;
        pipeline.emptied(case, &control_url).await;

        let mut sent: Vec<String> = Vec::new();
        for _ in 0..GENERATIONS {
            sent.push(pipeline.send(case, "relay-set").await);
        }

        // The control rule reporting that each generation was published, matched and handed
        // over — which is what tells the rows below that every verdict has been taken.
        let control = pipeline.drained(case, &control_url, GENERATIONS).await;
        assert_eq!(
            control.len(),
            GENERATIONS,
            "case {case:?}: expected the control rule's function to be invoked with the \
             delivery event of all {GENERATIONS} sends, got {control:?}"
        );
        for envelope in &control {
            assert_eq!(
                envelope["detail-type"],
                json!(DELIVERY_DETAIL_TYPE),
                "case {case:?}: expected the control rule to relay only the delivery event, \
                 got {envelope}"
            );
        }

        if row.invoked {
            let relayed = pipeline.drained(case, &row_url, GENERATIONS).await;
            assert_eq!(
                relayed.len(),
                GENERATIONS,
                "case {case:?}: expected the rule's function to be invoked with the \
                 acceptance event of all {GENERATIONS} sends, got {relayed:?}"
            );
            for envelope in &relayed {
                assert_eq!(
                    envelope["source"],
                    json!(SES_EVENT_SOURCE),
                    "case {case:?}: expected source {SES_EVENT_SOURCE:?}, got {envelope}"
                );
                assert_eq!(
                    envelope["detail-type"],
                    json!(ACCEPTANCE_DETAIL_TYPE),
                    "case {case:?}: expected detail-type {ACCEPTANCE_DETAIL_TYPE:?}, got \
                     {envelope}"
                );
                assert_eq!(
                    envelope["detail"]["eventType"],
                    json!("Send"),
                    "case {case:?}: expected detail.eventType \"Send\", got {envelope}"
                );
                let message_id = envelope["detail"]["mail"]["messageId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    sent.contains(&message_id),
                    "case {case:?}: expected the envelope to name one of the sent messages \
                     {sent:?}, got {envelope}"
                );
            }
        } else {
            let relayed = pipeline.relayed(case, &row_url).await;
            assert!(
                relayed.is_empty(),
                "case {case:?}: expected the rule's function never to be invoked, got \
                 {relayed:?}"
            );
        }
    }
}
