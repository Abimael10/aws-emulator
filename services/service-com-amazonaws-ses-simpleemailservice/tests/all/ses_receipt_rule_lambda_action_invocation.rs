// SPDX-License-Identifier: CC0-1.0

//! The `LambdaAction` of a receipt rule, carried out: a message this account accepts for a
//! recipient an ENABLED rule of the ACTIVE rule set applies to really reaches the function that
//! rule names — with Lambda, IAM, the token service and Amazon S3 mounted beside this service
//! on ONE engine exactly as the assembled router mounts them.
//!
//! THE RULE: `LambdaAction` "calls an Amazon Web Services Lambda function", and
//! `LambdaAction$InvocationType` says how — "a value of `Event` means that the function is
//! invoked asynchronously", "`RequestResponse` means that the execution of the function
//! immediately results in a response", "the default value is `Event`". So for every message
//! this account receives, each enabled rule of the active set whose `Recipients` conditions
//! apply to one of the message's recipients invokes the function its `LambdaAction` names ONCE,
//! under that invocation type, with the documented Amazon SES receipt event as the payload: one
//! record under the `aws:ses` event source, carrying the message's own `mail` envelope (the id
//! it was accepted under, the envelope sender, the recipient addresses its header block names,
//! its headers and the common ones a reader reaches by name) and the `receipt` of it (the
//! recipients the rule applied to, a verdict for each check Amazon SES made on the message, and
//! the action that called the function). A rule that applies to none of the message's
//! recipients invokes nothing.
//!
//! What the rows span — the FORM of the action and of the message, which is what the rule
//! generalises over: the invocation type the action names (`Event`, `RequestResponse`, and an
//! action naming none at all, which is stored as the member's documented default), whether the
//! rule asks for the scan and what the message's own content makes of it, and whether the rule's
//! recipient condition applies to the message at all. Every row drives the SAME trace; only the
//! rule and the message differ.
//!
//! Every function, grant, role and bucket here is made through Lambda's, IAM's and Amazon S3's
//! OWN operations over the in-process callback, and what each row asserts is the document the
//! function's own handler wrote into the object store when it RAN — so a row that passes is a
//! row where the handler really was handed this event.
//!
//! A message is received AFTER it is accepted, so every assertion waits for the CONDITION — the
//! object the handler writes being there — rather than for a span of time, and a row that must
//! see NO invocation waits for the object of a CONTROL rule of the same set instead, so "not
//! invoked" is told apart from "not invoked yet".

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_ses_simpleemailservice::state::received_scan::{
    VERDICT_FAIL, VERDICT_GRAY, VERDICT_PASS, VERDICT_PROCESSING_FAILED, SPAM_TEST_PATTERN,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{call, element, field, settle_verifications, Field, REGION};

/// The domain identity this account holds verified, and therefore the domain it receives mail
/// for.
const DOMAIN: &str = "lambda-inbound.example.com";

/// The mailbox the messages below are sent from.
const SENDER: &str = "sender@lambda-inbound.example.com";

/// The mailbox they are addressed to.
const RECIPIENT: &str = "rcpt@lambda-inbound.example.com";

/// A mailbox of the same domain no message below is addressed to, which is what the row about a
/// condition that does not apply names.
const OTHER_MAILBOX: &str = "someone-else@lambda-inbound.example.com";

/// The bucket the handler writes the event it was handed into — the proof that it ran.
const PROOF_BUCKET: &str = "lambda-action-proof";

/// The IAM API version an awsQuery call to IAM carries.
const IAM_VERSION: &str = "2010-05-08";

/// The service principal Amazon SES carries out a receipt rule's actions under, and so the
/// principal the function's own resource policy has to grant.
const SES_PRINCIPAL: &str = "ses.amazonaws.com";

/// The service principal a Lambda execution role is assumed by.
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The IAM action an invocation of a function performs, which is what the grant names.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `eventSource` and `eventVersion` a record of an Amazon SES receipt event carries.
const EVENT_SOURCE: &str = "aws:ses";
const EVENT_VERSION: &str = "1.0";

/// The `type` the receipt's action object carries for a `LambdaAction`.
const LAMBDA_ACTION_TYPE: &str = "Lambda";

/// The invocation types `LambdaAction$InvocationType` models, and the one a stored action that
/// named none carries ("the default value is `Event`").
const EVENT: &str = "Event";
const REQUEST_RESPONSE: &str = "RequestResponse";

/// How long a row waits for the object the handler writes, and how often it looks.
///
/// Generously longer than an in-process invocation takes: what a row asserts is the document,
/// never how quickly it arrived, so a loaded host makes this wait more patient and never less.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(120);

/// How long a row waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The handler every function below runs: it writes the event it was handed into the object
/// store, under the key its own environment names, through the AWS SDK the runtime supplies.
const PROOF_HANDLER: &str = r#"
import json
import os

import boto3


def handler(event, context):
    boto3.client("s3").put_object(
        Bucket=os.environ["PROOF_BUCKET"],
        Key=os.environ["PROOF_KEY"],
        Body=json.dumps(event).encode("utf-8"),
    )
    return {"records": len(event["Records"])}
"#;

/// The module of the deployment package that handler lives in, and so the `Handler` the
/// function names.
const HANDLER_MODULE: &str = "index.py";
const HANDLER: &str = "index.handler";

/// A .zip deployment package holding [`PROOF_HANDLER`], written in memory the way a caller
/// uploads one — which is what `FunctionCode$ZipFile` carries.
fn package() -> Vec<u8> {
    use std::io::Write as _;

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
    writer
        .start_file(HANDLER_MODULE, options)
        .expect("the archive entry starts");
    writer
        .write_all(PROOF_HANDLER.as_bytes())
        .expect("the archive entry is written");
    writer
        .finish()
        .expect("a finished archive")
        .into_inner()
}

/// The account this emulator serves, which owns every function, role and bucket here.
fn account() -> String {
    engine::EMULATOR_ACCOUNT_ID.to_string()
}

/// A router carrying this service beside the services one carried-out `LambdaAction` reaches:
/// Lambda (the function, its grant and the run), IAM and the token service (the execution role
/// the handler's own call is made as) and Amazon S3 (where that call writes), all over ONE
/// shared engine.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Verifying signatures is the emulator's concern, not this service's; the calls below sign
    // nothing, so the check is off and the scope header is read for routing only.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_ses_simpleemailservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    (router, engine)
}

/// A Lambda client over this emulator's own in-process callback.
fn lambda(router: &MultiServiceRouter) -> aws_sdk_lambda::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = aws_sdk_lambda::Config::builder()
        .behavior_version(aws_sdk_lambda::config::BehaviorVersion::latest())
        .region(aws_sdk_lambda::config::Region::new(REGION))
        .endpoint_url(format!("https://lambda.{REGION}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    aws_sdk_lambda::Client::from_conf(config)
}

/// An Amazon S3 client over the same callback.
fn s3(router: &MultiServiceRouter) -> aws_sdk_s3::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = aws_sdk_s3::Config::builder()
        .behavior_version(aws_sdk_s3::config::BehaviorVersion::latest())
        .region(aws_sdk_s3::config::Region::new(REGION))
        .endpoint_url(format!("https://s3.{REGION}.amazonaws.com"))
        .force_path_style(true)
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        .retry_config(aws_sdk_s3::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_s3::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_s3::config::IdentityCache::no_cache())
        .stalled_stream_protection(aws_sdk_s3::config::StalledStreamProtectionConfig::disabled())
        .build();
    aws_sdk_s3::Client::from_conf(config)
}

/// One awsQuery call to IAM: its parameters travel as a form body and its answer is XML.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[Field]) -> (u16, String) {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&crate::harness::form_encode(name));
        body.push('=');
        body.push_str(&crate::harness::form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("http://localhost/")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=0000"
            ),
        )
        .header("content-type", "application/x-www-form-urlencoded")
        .body(bytes::Bytes::from(body))
        .expect("the request builds");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// The execution role every function below runs as, created through IAM's own operations: it is
/// assumed by the Lambda service principal and may write the object its handler writes.
async fn given_execution_role(router: &MultiServiceRouter, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": LAMBDA_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = iam(
        router,
        "CreateRole",
        &[
            field("RoleName", name),
            field("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the execution role\": expected CreateRole to answer 200, got {status}: {created}"
    );
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:PutObject"],
            "Resource": format!("arn:aws:s3:::{PROOF_BUCKET}/*"),
        }],
    })
    .to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            field("RoleName", name),
            field("PolicyName", "write-the-proof"),
            field("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the execution role\": expected PutRolePolicy to answer 200, got {status}: \
         {written}"
    );
    element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole must answer the role's Arn, got {created}"))
}

/// The bucket the handlers write into, created through Amazon S3's own `CreateBucket`.
async fn given_proof_bucket(router: &MultiServiceRouter) {
    s3(router)
        .create_bucket()
        .bucket(PROOF_BUCKET)
        .send()
        .await
        .unwrap_or_else(|error| panic!("creating {PROOF_BUCKET}: {error}"));
}

/// Create the function `name`, running [`PROOF_HANDLER`] as `role_arn` and writing its proof at
/// `proof_key`, and answer the ARN Lambda minted for it.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    proof_key: &str,
) -> String {
    let code = aws_sdk_lambda::types::FunctionCode::builder()
        .zip_file(aws_smithy_types::Blob::new(package()))
        .build();
    let environment = aws_sdk_lambda::types::Environment::builder()
        .variables("PROOF_BUCKET", PROOF_BUCKET)
        .variables("PROOF_KEY", proof_key)
        .build();
    let created = lambda(router)
        .create_function()
        .function_name(name)
        .role(role_arn)
        .runtime(aws_sdk_lambda::types::Runtime::Python312)
        .handler(HANDLER)
        .code(code)
        .environment(environment)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: creating the function {name}: {error}"));
    let arn = created
        .function_arn()
        .unwrap_or_else(|| panic!("case {case:?}: the created function has an ARN"))
        .to_string();
    awaited_active(router, case, name).await;
    arn
}

/// Wait until Lambda reports `name` as `Active` — the state a function has to be in to be
/// invoked — by looking again until it is, as a client does.
async fn awaited_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let read = lambda(router)
            .get_function()
            .function_name(name)
            .send()
            .await
            .unwrap_or_else(|error| panic!("case {case:?}: reading the function {name}: {error}"));
        let state = read
            .configuration()
            .and_then(|configuration| configuration.state())
            .cloned();
        if state == Some(aws_sdk_lambda::types::State::Active) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {name} did not become Active within {deadline_secs}s, \
             its state is {state:?}",
            deadline_secs = DELIVERY_DEADLINE.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Grant the Amazon SES service principal the invocation of `name`, scoped to the receipt rule
/// `rule` of the rule set `rule_set` — the grant `AddPermission` writes.
async fn given_invoke_grant(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    rule_set: &str,
    rule: &str,
) {
    lambda(router)
        .add_permission()
        .function_name(name)
        .statement_id("ses-receipt-invoke")
        .action(INVOKE_ACTION)
        .principal(SES_PRINCIPAL)
        .source_account(account())
        .source_arn(format!(
            "arn:aws:ses:{REGION}:{}:receipt-rule-set/{rule_set}:receipt-rule/{rule}",
            account()
        ))
        .send()
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: granting the invocation of {name}: {error}")
        });
}

/// One rule of a row's rule set: the `LambdaAction` it carries, and the recipients it applies
/// to.
struct Rule {
    /// `ReceiptRule$Name`, which also names the rule's own function and proof key.
    name: String,
    /// `ReceiptRule$Recipients`, empty for a rule stating no condition at all.
    recipients: Vec<String>,
    /// `ReceiptRule$ScanEnabled`.
    scan_enabled: bool,
    /// `LambdaAction$InvocationType`, or `None` for an action naming none.
    invocation_type: Option<&'static str>,
    /// The ARN Lambda minted for the rule's function.
    function_arn: String,
}

impl Rule {
    /// The key the rule's own function writes its proof at.
    fn proof_key(&self) -> String {
        format!("{}.json", self.name)
    }
}

/// `CreateReceiptRuleSet` for `rule_set`, then one `CreateReceiptRule` per rule, in order.
async fn given_rule_set(
    router: &MultiServiceRouter,
    case: &str,
    rule_set: &str,
    rules: &[Rule],
) {
    let (status, body) = call(
        router,
        "CreateReceiptRuleSet",
        &[field("RuleSetName", rule_set)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating the rule set {rule_set}: expected 200, got {status} with {body}"
    );
    for (index, rule) in rules.iter().enumerate() {
        let mut params = vec![field("RuleSetName", rule_set)];
        // "The name of an existing rule after which the new rule is placed" — so each rule
        // after the first names the one before it and the set evaluates them in this order.
        if let Some(previous) = index.checked_sub(1).map(|before| rules[before].name.clone()) {
            params.push(field("After", previous));
        }
        params.push(field("Rule.Name", rule.name.clone()));
        params.push(field("Rule.Enabled", "true"));
        params.push(field(
            "Rule.ScanEnabled",
            if rule.scan_enabled { "true" } else { "false" },
        ));
        for (at, recipient) in rule.recipients.iter().enumerate() {
            params.push(field(
                format!("Rule.Recipients.member.{}", at + 1),
                recipient.clone(),
            ));
        }
        params.push(field(
            "Rule.Actions.member.1.LambdaAction.FunctionArn",
            rule.function_arn.clone(),
        ));
        if let Some(invocation_type) = rule.invocation_type {
            params.push(field(
                "Rule.Actions.member.1.LambdaAction.InvocationType",
                invocation_type,
            ));
        }
        let (status, body) = call(router, "CreateReceiptRule", &params).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the function grants this rule the invocation, so creating the rule \
             {} must answer 200, got {status} with {body}",
            rule.name
        );
    }
    let (status, body) = call(
        router,
        "SetActiveReceiptRuleSet",
        &[field("RuleSetName", rule_set)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: activating {rule_set}: expected 200, got {status} with {body}"
    );
}

/// The `InvocationType` `DescribeReceiptRule` reports for the rule `rule` of `rule_set` — the
/// stored action read back through this service's own reader.
async fn described_invocation_type(
    router: &MultiServiceRouter,
    case: &str,
    rule_set: &str,
    rule: &str,
) -> String {
    let (status, body) = call(
        router,
        "DescribeReceiptRule",
        &[field("RuleSetName", rule_set), field("RuleName", rule)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: describing the rule {rule}: expected 200, got {status} with {body}"
    );
    element(&body, "InvocationType").unwrap_or_else(|| {
        panic!("case {case:?}: the stored rule must carry an InvocationType, got {body}")
    })
}

/// `VerifyDomainIdentity` for the domain this account receives mail for, settled past the span
/// the request leaves the record pending for.
async fn given_verified_domain(router: &MultiServiceRouter, engine: &Arc<engine::InMemoryEngine>) {
    let (status, body) = call(router, "VerifyDomainIdentity", &[field("Domain", DOMAIN)]).await;
    assert_eq!(
        status, 200,
        "verifying {DOMAIN}: expected 200, got {status} with {body}"
    );
    settle_verifications(engine);
}

/// A small RFC 5322 message from [`SENDER`] to `to`, with `subject` and `body`.
fn plain_message(to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {SENDER}\r\nTo: {to}\r\nSubject: {subject}\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/plain; charset=UTF-8\r\n\r\n{body}\r\n"
    )
    .into_bytes()
}

/// `SendRawEmail` carrying `raw` for `to`, answering the `MessageId` it was accepted under.
async fn send_raw(router: &MultiServiceRouter, case: &str, to: &str, raw: &[u8]) -> String {
    let params = vec![
        field("Source", SENDER),
        field("Destinations.member.1", to),
        field("RawMessage.Data", aws_smithy_types::base64::encode(raw)),
    ];
    let (status, body) = call(router, "SendRawEmail", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: sending: expected 200, got {status} with {body}"
    );
    element(&body, "MessageId")
        .unwrap_or_else(|| panic!("case {case:?}: expected a MessageId, got {body}"))
}

/// The document Amazon S3 holds at `key`, and `None` when it holds nothing there.
async fn proof(router: &MultiServiceRouter, case: &str, key: &str) -> Option<Value> {
    let read = s3(router)
        .get_object()
        .bucket(PROOF_BUCKET)
        .key(key)
        .send()
        .await
        .ok()?;
    let body = read
        .body
        .collect()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: reading {key}'s body: {error}"))
        .to_vec();
    Some(
        serde_json::from_slice(&body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: the handler must have written the event as JSON ({error}), got \
                 {:?}",
                String::from_utf8_lossy(&body)
            )
        }),
    )
}

/// The document at `key`, waited for until it is there: the wait is for the CONDITION, looked
/// for again every [`POLL_INTERVAL`] until [`DELIVERY_DEADLINE`] passes, because a message is
/// received after it is accepted and nothing a sender is told says when.
async fn awaited_proof(router: &MultiServiceRouter, case: &str, key: &str) -> Value {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        if let Some(document) = proof(router, case, key).await {
            return document;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function the rule names was never handed the message \
             ({PROOF_BUCKET}/{key} is still absent after {deadline_secs}s)",
            deadline_secs = DELIVERY_DEADLINE.as_secs()
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The verdict object `member` of a receipt, asserted to be an object carrying a `status` out
/// of the value set a verdict is reported in, and answered as that status.
fn verdict_status(case: &str, receipt: &Value, member: &str) -> String {
    let verdict = &receipt[member];
    let status = verdict["status"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected {member} to be an object carrying a status, got {verdict}")
    });
    // The value set is the one this service reports a verdict in, taken from the service itself
    // rather than written out here.
    let published = [
        VERDICT_PASS,
        VERDICT_FAIL,
        VERDICT_GRAY,
        VERDICT_PROCESSING_FAILED,
    ];
    assert!(
        published.contains(&status),
        "case {case:?}: expected {member}'s status to be one of {published:?}, got {status:?}"
    );
    status.to_string()
}

/// One row: the rule the row's set carries, the message it sends, and what the function must
/// have been handed.
struct Row {
    /// What this row is called in every assertion it makes.
    case: &'static str,
    /// The rule's own name, which also names its function and its proof key.
    rule: &'static str,
    /// `ReceiptRule$Recipients` of that rule, empty for a rule stating no condition.
    recipients: &'static [&'static str],
    /// `ReceiptRule$ScanEnabled`.
    scan_enabled: bool,
    /// `LambdaAction$InvocationType`, or `None` for an action naming none.
    invocation_type: Option<&'static str>,
    /// The subject of the message the row sends.
    subject: &'static str,
    /// Whether the message's body carries the published spam test pattern.
    spam_pattern: bool,
    /// Whether the rule must invoke its function for that message.
    invoked: bool,
    /// The `invocationType` the event's action must report, for a row that is invoked.
    reported_invocation_type: &'static str,
    /// The `status` the event's `spamVerdict` must report.
    spam_verdict: &'static str,
}

/// The rows, in the order they run. Each gets a rule set and functions of its own, so one row's
/// active set and proof objects cannot be read by another.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an Event invocation for a condition naming the recipient's own address",
            rule: "event-exact",
            recipients: &[RECIPIENT],
            scan_enabled: false,
            invocation_type: Some(EVENT),
            subject: "the event invocation",
            spam_pattern: false,
            invoked: true,
            reported_invocation_type: EVENT,
            spam_verdict: VERDICT_PASS,
        },
        Row {
            case: "a RequestResponse invocation for a condition naming the recipient's domain",
            rule: "request-response-domain",
            recipients: &[DOMAIN],
            scan_enabled: false,
            invocation_type: Some(REQUEST_RESPONSE),
            subject: "the request-response invocation",
            spam_pattern: false,
            invoked: true,
            reported_invocation_type: REQUEST_RESPONSE,
            spam_verdict: VERDICT_PASS,
        },
        Row {
            case: "an action naming no invocation type, over a rule stating no condition",
            rule: "default-invocation-type",
            recipients: &[],
            scan_enabled: false,
            invocation_type: None,
            subject: "the default invocation type",
            spam_pattern: false,
            invoked: true,
            reported_invocation_type: EVENT,
            spam_verdict: VERDICT_PASS,
        },
        Row {
            case: "a scanning rule over a message carrying the published spam test pattern",
            rule: "scanned-spam",
            recipients: &[RECIPIENT],
            scan_enabled: true,
            invocation_type: Some(EVENT),
            subject: "the scanned message",
            spam_pattern: true,
            invoked: true,
            reported_invocation_type: EVENT,
            spam_verdict: VERDICT_FAIL,
        },
        Row {
            case: "a condition naming another mailbox of the same domain",
            rule: "another-mailbox",
            recipients: &[OTHER_MAILBOX],
            scan_enabled: false,
            invocation_type: Some(EVENT),
            subject: "the message the rule does not apply to",
            spam_pattern: false,
            invoked: false,
            reported_invocation_type: EVENT,
            spam_verdict: VERDICT_PASS,
        },
    ]
}

/// The control rule a row that expects NO invocation appends to its set: it states no condition,
/// so it applies to every recipient of the account, and it is the LAST rule of the set — the
/// object its function writes is what proves this message's steps are finished.
const CONTROL_RULE: &str = "control";

#[tokio::test]
async fn an_accepted_message_is_handed_to_the_function_its_matching_rule_names() {
    let (router, engine) = emulator();
    given_verified_domain(&router, &engine).await;
    given_proof_bucket(&router).await;
    let role = given_execution_role(&router, "ses-receipt-proof-writer").await;

    for row in rows() {
        let case = row.case;
        let rule_set = format!("{}-set", row.rule);
        let mut rules = Vec::new();
        // The rule's own function, granted the invocation for THIS rule of THIS set — which is
        // what makes the action one this service stores at all.
        let function_arn =
            given_function(&router, case, row.rule, &role, &format!("{}.json", row.rule)).await;
        given_invoke_grant(&router, case, row.rule, &rule_set, row.rule).await;
        rules.push(Rule {
            name: row.rule.to_string(),
            recipients: row.recipients.iter().map(|r| (*r).to_string()).collect(),
            scan_enabled: row.scan_enabled,
            invocation_type: row.invocation_type,
            function_arn,
        });
        if !row.invoked {
            let control = format!("{}-{CONTROL_RULE}", row.rule);
            let control_arn =
                given_function(&router, case, &control, &role, &format!("{control}.json")).await;
            given_invoke_grant(&router, case, &control, &rule_set, &control).await;
            rules.push(Rule {
                name: control,
                recipients: Vec::new(),
                scan_enabled: false,
                invocation_type: Some(EVENT),
                function_arn: control_arn,
            });
        }
        given_rule_set(&router, case, &rule_set, &rules).await;

        // The stored action reads back carrying the invocation type it will be invoked under,
        // whether the request named one or left it to the member's documented default.
        let stored = described_invocation_type(&router, case, &rule_set, row.rule).await;
        assert_eq!(
            stored, row.reported_invocation_type,
            "case {case:?}: expected the stored rule to carry the invocation type \
             {:?}, got {stored:?}",
            row.reported_invocation_type
        );

        let body = if row.spam_pattern {
            format!("a message carrying {SPAM_TEST_PATTERN} in its body")
        } else {
            "the body of the message".to_string()
        };
        let raw = plain_message(RECIPIENT, row.subject, &body);
        let message_id = send_raw(&router, case, RECIPIENT, &raw).await;

        if !row.invoked {
            // The control rule's object is this message's barrier: its steps are carried out in
            // order, so once the LAST rule's function has written, nothing more is coming.
            let control_key = format!("{}-{CONTROL_RULE}.json", row.rule);
            awaited_proof(&router, case, &control_key).await;
            let unexpected = proof(&router, case, &format!("{}.json", row.rule)).await;
            assert!(
                unexpected.is_none(),
                "case {case:?}: a rule that applies to none of the message's recipients must \
                 invoke nothing, got {unexpected:?}"
            );
            continue;
        }

        let event = awaited_proof(&router, case, &rules[0].proof_key()).await;
        let records = event["Records"].as_array().unwrap_or_else(|| {
            panic!("case {case:?}: expected the event to carry a Records list, got {event}")
        });
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: expected exactly one record, got {event}"
        );
        let record = &records[0];
        assert_eq!(
            record["eventSource"].as_str(),
            Some(EVENT_SOURCE),
            "case {case:?}: expected the record's eventSource, got {record}"
        );
        assert_eq!(
            record["eventVersion"].as_str(),
            Some(EVENT_VERSION),
            "case {case:?}: expected the record's eventVersion, got {record}"
        );

        let mail = &record["ses"]["mail"];
        assert_eq!(
            mail["messageId"].as_str(),
            Some(message_id.as_str()),
            "case {case:?}: expected the id the send was accepted under, got {mail}"
        );
        // "The email address that the message was sent from (the envelope MAIL FROM address)":
        // Amazon SES relays the message under an address of its own, whose local part names
        // this message.
        let source = mail["source"].as_str().unwrap_or_default();
        assert!(
            source.starts_with(&format!("{message_id}@")),
            "case {case:?}: expected the envelope sender of this message, got {source:?}"
        );
        let destination: Vec<&str> = mail["destination"]
            .as_array()
            .map(|addresses| {
                addresses
                    .iter()
                    .filter_map(|address| address.as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            destination.contains(&RECIPIENT),
            "case {case:?}: expected the message's own recipient addresses to name \
             {RECIPIENT:?}, got {destination:?}"
        );
        assert_eq!(
            mail["headersTruncated"].as_bool(),
            Some(false),
            "case {case:?}: this message's whole header block is reported, got {mail}"
        );
        let headers: Vec<(String, String)> = mail["headers"]
            .as_array()
            .map(|written| {
                written
                    .iter()
                    .map(|header| {
                        (
                            header["name"].as_str().unwrap_or_default().to_string(),
                            header["value"].as_str().unwrap_or_default().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            headers.contains(&("Subject".to_string(), row.subject.to_string())),
            "case {case:?}: expected the message's own Subject field among its headers, got \
             {headers:?}"
        );
        assert_eq!(
            mail["commonHeaders"]["subject"].as_str(),
            Some(row.subject),
            "case {case:?}: expected the subject the message was sent with, got {mail}"
        );
        assert_eq!(
            mail["commonHeaders"]["to"],
            json!([RECIPIENT]),
            "case {case:?}: expected the To field the message carries, got {mail}"
        );

        let receipt = &record["ses"]["receipt"];
        assert_eq!(
            receipt["recipients"],
            json!([RECIPIENT]),
            "case {case:?}: expected the recipients the rule applied to, got {receipt}"
        );
        assert_eq!(
            receipt["action"],
            json!({
                "type": LAMBDA_ACTION_TYPE,
                "functionArn": rules[0].function_arn,
                "invocationType": row.reported_invocation_type,
            }),
            "case {case:?}: expected the action that called the function, got {receipt}"
        );
        assert!(
            receipt["processingTimeMillis"].is_u64(),
            "case {case:?}: expected the processing time as a whole number of milliseconds, got \
             {receipt}"
        );
        assert!(
            receipt["timestamp"].as_str().is_some_and(|at| !at.is_empty()),
            "case {case:?}: expected the receipt to be stamped, got {receipt}"
        );
        let spam = verdict_status(case, receipt, "spamVerdict");
        assert_eq!(
            spam, row.spam_verdict,
            "case {case:?}: expected the spam verdict {:?} for this message, got {spam:?}",
            row.spam_verdict
        );
        let virus = verdict_status(case, receipt, "virusVerdict");
        assert_eq!(
            virus, VERDICT_PASS,
            "case {case:?}: this message carries no virus test signature, got {virus:?}"
        );
        let spf = verdict_status(case, receipt, "spfVerdict");
        assert_eq!(
            spf, VERDICT_PASS,
            "case {case:?}: the submitting host is this account's own sender, got {spf:?}"
        );
        // Each remaining check is reported too, out of the same value set.
        for member in ["dkimVerdict", "dmarcVerdict"] {
            verdict_status(case, receipt, member);
        }
    }
}
