// SPDX-License-Identifier: CC0-1.0

//! A Glue job run converges on the service's own clock, and its terminal `Glue Job State
//! Change` reaches the permissioned SQS target of its rule and no other, for the documents
//! in the FORMS a client's SDK submits them in.
//!
//! The RULE under test: **every document these services take is read as the JSON it is —
//! an IAM trust policy arriving on the `application/x-www-form-urlencoded` wire, an inline
//! identity policy on the same wire, an EventBridge `EventPattern` and an Amazon SQS queue
//! `Policy` in a JSON payload — so insignificant whitespace in the document and the encoding
//! the form wire chose for a space (`+` or `%20`, which that media type reads alike) change
//! NOTHING about what it grants.** A run whose role's trust document admits
//! `glue.amazonaws.com` therefore executes its script and comes to rest in `SUCCEEDED`
//! carrying no `ErrorMessage` — never in the failure state a role Glue cannot assume earns —
//! and the state change that terminal transition publishes is delivered to the queue whose own
//! policy admits `events.amazonaws.com` for the invoking rule and to no queue that admits it
//! nothing.
//!
//! Why this is the rule and not a detail of one client: a form value's `+` IS a space in that
//! media type, and JSON is whitespace-insensitive, so two documents that differ only in those
//! ways are ONE document. A reader that disagrees turns a correctly-configured role into a run
//! that fails with the assume-role refusal, and the whole event path behind that run —
//! publication, rule evaluation, target delivery — becomes unobservable while every resource
//! looks healthy.
//!
//! The rows differ along exactly that: how the form body encodes a space, and how the JSON
//! documents are written (compact, as a client's `json.dumps` writes them, and indented with
//! tabs and line breaks inside). The recorded case — compact documents — is one of them. Each
//! row carries its own role, job, rule and pair of queues, so a delivery that reached another
//! row's queue is visible as such, and all rows are judged over runs started together.
//!
//! What only a cross-service suite reaches: IAM is signed for the Region a client's IAM client
//! signs a GLOBAL service for while the job runs in another Region's endpoint, the script is
//! read from Amazon S3 as the job's role, the publication travels the emulator's own callback
//! into EventBridge, and the deliveries are real `SendMessage` calls on real queues.
//!
//! Nothing here waits on the clock: the due instants of the run lifecycle are left where the
//! service put them, each wait is a poll for the CONDITION it is about against a generous
//! deadline, and no assertion reads elapsed time.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;

/// The Region every regional call below is addressed in.
const REGION: &str = "us-west-2";

/// The Region a client's SDK signs IAM for: IAM is a global service, so its endpoint is one
/// for the whole partition and its requests are signed for this Region whatever Region the
/// client itself works in. The rows' roles are created through it and resolved by a job run
/// served in [`REGION`].
const IAM_SIGNING_REGION: &str = "us-east-1";

/// The XML namespace Amazon S3's own model declares, which a client's payload carries.
const S3_XMLNS: &str = "http://s3.amazonaws.com/doc/2006-03-01/";

/// The bucket the rows' script lives in, and the key it is staged under.
const BUCKET: &str = "xsvc-glue-run-document-forms";
const SCRIPT_KEY: &str = "scripts/job.py";

/// The script every row runs: it exits cleanly, which is what a succeeding job does.
const SCRIPT: &str = "print(\"ran\")\n";

/// The `source` and `detail-type` a Glue job run's terminal state is published under, and the
/// state a run that executed its script cleanly comes to rest in — spelled out here rather
/// than imported, so a wrong constant in the implementation cannot make this suite pass.
const SOURCE: &str = "aws.glue";
const DETAIL_TYPE: &str = "Glue Job State Change";
const SUCCEEDED: &str = "SUCCEEDED";

/// Every state a job run never leaves again
/// (`smithy_query(query="shape",shape="JobRunState",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")`).
const TERMINAL_RUN_STATES: [&str; 6] = [
    "STOPPED",
    "SUCCEEDED",
    "FAILED",
    "TIMEOUT",
    "ERROR",
    "EXPIRED",
];

/// The service principal Glue has to be admitted as to assume a job's execution role, and the
/// action a trust document grants it.
const GLUE_PRINCIPAL: &str = "glue.amazonaws.com";
const ASSUME_ROLE: &str = "sts:AssumeRole";

/// The service principal EventBridge delivers to an SQS target as, and the action such a
/// delivery performs on the queue.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const SEND_MESSAGE: &str = "sqs:SendMessage";

/// The access key id every request presents. Request authentication is off here — the
/// delivery boundary under signature verification is pinned by
/// `glue_job_state_change_delivery_per_queue_policy.rs` — so only the credential scope is
/// read: its service field routes the request and its Region field is the Region the service
/// serves it in.
const ACCESS_KEY: &str = "AKIDGLUEDOCFORMS";

/// How long a run is waited for and how long a delivery is, and how often each is looked at.
/// Generous: what is asserted is that each happens at all, never how fast the host runs.
const SETTLE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(180);
const DELIVERY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(90);
const LOOK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// How long the withheld queues keep being looked at after every admitted delivery has
/// arrived, before concluding that nothing is coming. A bound on an ABSENCE, so it is the one
/// span a slow host may spend without changing the verdict.
const WITHHELD_WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

/// How a row's form body encodes a SPACE inside a document — the two encodings
/// `application/x-www-form-urlencoded` defines for one, which a reader of that media type has
/// to read alike.
#[derive(Debug, Clone, Copy)]
enum Space {
    /// `+`, which is what `urlencode` in a client's own SDK writes.
    Plus,
    /// `%20`, the percent escape.
    Percent,
}

/// How a row WRITES a JSON document: the whitespace between its tokens, which JSON does not
/// read.
#[derive(Debug, Clone, Copy)]
enum Written {
    /// No whitespace at all, which is the recorded shape.
    Compact,
    /// One space after every `:` and `,`, as a client's own `json.dumps` writes a document.
    Spaced,
    /// Indented over several lines, with tabs and line breaks inside.
    Indented,
}

impl Written {
    /// `document` as this row writes it.
    fn render(self, document: &Value) -> String {
        match self {
            Self::Compact => document.to_string(),
            Self::Spaced => spaced(document),
            Self::Indented => indented(document),
        }
    }
}

/// `document` with one space after every `:` and `,` outside its strings — the spacing a
/// client's own JSON serializer writes by default.
fn spaced(document: &Value) -> String {
    let compact = document.to_string();
    let mut out = String::with_capacity(compact.len() * 2);
    let mut inside_string = false;
    let mut escaped = false;
    for character in compact.chars() {
        out.push(character);
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if inside_string => escaped = true,
            '"' => inside_string = !inside_string,
            ':' | ',' if !inside_string => out.push(' '),
            _ => {}
        }
    }
    out
}

/// `document` indented over several lines, with a tab in the indent — insignificant
/// whitespace of every kind JSON allows between tokens.
fn indented(document: &Value) -> String {
    serde_json::to_string_pretty(document)
        .expect("a document renders")
        .replace("  ", "\t")
}

/// Percent-encode one form component, writing a space the way `space` says and leaving only
/// RFC 3986's unreserved characters literal.
///
/// The crate's shared encoder (`query_wire::form_encode`) writes a space as `%20` alone; the
/// encoding of a space is the very dimension these rows differ along, so both forms are
/// written here.
fn form_encode_space(value: &str, space: Space) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                encoded.push(char::from(byte));
            }
            b' ' => match space {
                Space::Plus => encoded.push('+'),
                Space::Percent => encoded.push_str("%20"),
            },
            other => encoded.push_str(&format!("%{other:02X}")),
        }
    }
    encoded
}

/// One answer off the wire.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The body as JSON, or `Value::Null` for one that is not.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }
}

/// The emulator this suite drives: the whole router over one engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn start() -> Self {
        let engine = aws_emulator_router::new_engine();
        let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
        router.set_request_authentication(false);
        Self { router }
    }

    /// Dispatch one request as it arrives on the wire. `scope` is the credential scope the
    /// router reads the request's service and Region off: `(signing name, Region)`.
    async fn send(
        &self,
        method: &str,
        host: &str,
        scope: (&str, &str),
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let (signing, region) = scope;
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={ACCESS_KEY}/20260101/{region}/{signing}/\
                     aws4_request, SignedHeaders=host, Signature=00"
                ),
            );
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let request = builder
            .body(Bytes::from(body.as_bytes().to_vec()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            body: String::from_utf8_lossy(response.body()).into_owned(),
        }
    }

    /// One Amazon S3 REST call, path-style as the emulator serves it, which must answer 200.
    async fn s3(&self, what: &str, method: &str, path: &str, body: &str) -> Answer {
        let host = format!("s3.{REGION}.amazonaws.com");
        let answered = self
            .send(method, &host, ("s3", REGION), path, &[], body)
            .await;
        assert_eq!(
            answered.status, 200,
            "setup: {what} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// One IAM `awsQuery` call, signed for the Region a client signs a global service for,
    /// with every parameter encoded as `space` writes one. Must answer 200.
    async fn iam(
        &self,
        what: &str,
        action: &str,
        parameters: &[(&str, String)],
        space: Space,
    ) -> Answer {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in parameters {
            body.push('&');
            body.push_str(&form_encode_space(name, space));
            body.push('=');
            body.push_str(&form_encode_space(value, space));
        }
        let answered = self
            .send(
                "POST",
                "iam.amazonaws.com",
                ("iam", IAM_SIGNING_REGION),
                "/",
                &[("content-type", "application/x-www-form-urlencoded")],
                &body,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "setup: {what} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered
    }

    /// One `X-Amz-Target` RPC call, which must answer 200.
    async fn rpc(
        &self,
        what: &str,
        signing: &str,
        content_type: &str,
        target: &str,
        body: &Value,
    ) -> Value {
        let host = format!("{signing}.{REGION}.amazonaws.com");
        let answered = self
            .send(
                "POST",
                &host,
                (signing, REGION),
                "/",
                &[("content-type", content_type), ("x-amz-target", target)],
                &body.to_string(),
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "{what}: {target} must answer 200, got {} with {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// One AWS Glue `awsJson1_1` call.
    async fn glue(&self, what: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            what,
            "glue",
            "application/x-amz-json-1.1",
            &format!("AWSGlue.{operation}"),
            body,
        )
        .await
    }

    /// One EventBridge `awsJson1_1` call.
    async fn events(&self, what: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            what,
            "events",
            "application/x-amz-json-1.1",
            &format!("AWSEvents.{operation}"),
            body,
        )
        .await
    }

    /// One Amazon SQS `awsJson1_0` call.
    async fn sqs(&self, what: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            what,
            "sqs",
            "application/x-amz-json-1.0",
            &format!("AmazonSQS.{operation}"),
            body,
        )
        .await
    }

    /// A queue called `name`: its url and the ARN a rule names it by.
    async fn given_queue(&self, case: &str, name: &str) -> (String, String) {
        let created = self
            .sqs(case, "CreateQueue", &json!({ "QueueName": name }))
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
            .to_string();
        let attributes = self
            .sqs(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
            .to_string();
        (url, arn)
    }

    /// Receive everything waiting on the queue at `url` and delete it; answers the parsed
    /// envelopes its message bodies carried.
    ///
    /// Each message received is DELETED, so a message this answered with cannot come back
    /// when its visibility timeout lapses and be read as a second delivery.
    async fn drain(&self, case: &str, url: &str) -> Vec<Value> {
        let received = self
            .sqs(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        let messages = received["Messages"].as_array().cloned().unwrap_or_default();
        let mut envelopes = Vec::with_capacity(messages.len());
        for message in &messages {
            if let Some(handle) = message["ReceiptHandle"].as_str() {
                self.sqs(
                    case,
                    "DeleteMessage",
                    &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
                )
                .await;
            }
            envelopes.push(
                serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
                    .unwrap_or(Value::Null),
            );
        }
        envelopes
    }

    /// Poll `GetJobRun` until the run comes to rest, exactly as a client waits for one,
    /// leaving the due instants of its lifecycle where the service put them.
    async fn run_at_rest(&self, case: &str, job: &str, run_id: &str) -> Value {
        let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
        loop {
            let reported = self
                .glue(
                    case,
                    "GetJobRun",
                    &json!({ "JobName": job, "RunId": run_id }),
                )
                .await;
            let run = reported["JobRun"].clone();
            let state = run["JobRunState"].as_str().unwrap_or_default().to_string();
            if TERMINAL_RUN_STATES.contains(&state.as_str()) {
                return run;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "case {case:?}: the run never came to rest; it reports {run}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
}

/// One row: the document forms a client's SDK submitted, and everything that row created.
struct Row {
    /// The case name every assertion of the row reports.
    case: &'static str,
    /// How the row's form body encodes a space.
    space: Space,
    /// How the row writes its JSON documents.
    written: Written,
    /// The job the row runs, and the run it started.
    job: String,
    run_id: String,
    /// The queue the row's rule may deliver to, and the sibling it may not.
    permissioned: String,
    withheld: String,
    /// What each of those queues has held so far.
    delivered: Vec<Value>,
    nothing: Vec<Value>,
}

/// The rows: the forms the documents of one correctly-configured deployment are submitted in.
fn rows() -> Vec<(&'static str, Space, Written)> {
    vec![
        (
            "compact documents, a space written as %20",
            Space::Percent,
            Written::Compact,
        ),
        (
            "documents spaced as a client's own serializer writes them, a space written as +",
            Space::Plus,
            Written::Spaced,
        ),
        (
            "indented documents carrying tabs and line breaks, a space written as %20",
            Space::Percent,
            Written::Indented,
        ),
        (
            "compact documents, a space written as +",
            Space::Plus,
            Written::Compact,
        ),
    ]
}

/// The trust document a Glue job's role carries, as this row writes it.
fn trust_document(written: Written) -> String {
    written.render(&json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": GLUE_PRINCIPAL },
            "Action": ASSUME_ROLE,
        }],
    }))
}

/// The grants a run's role needs to read its script: the object read, and the bucket listing
/// that decides WHICH refusal a missing object earns.
fn script_access(written: Written) -> String {
    written.render(&json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["s3:GetObject", "s3:ListBucket"],
            "Resource": [
                format!("arn:aws:s3:::{BUCKET}"),
                format!("arn:aws:s3:::{BUCKET}/*"),
            ],
        }],
    }))
}

/// The queue policy the permissioned target carries: the send, to the events principal, for
/// the invoking rule — as this row writes it.
fn queue_policy(written: Written, queue_arn: &str, rule_arn: &str) -> String {
    written.render(&json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": { "Service": EVENTS_PRINCIPAL },
            "Action": SEND_MESSAGE,
            "Resource": queue_arn,
            "Condition": { "ArnEquals": { "aws:SourceArn": rule_arn } },
        }],
    }))
}

/// The pattern the row's rule matches its job's terminal state change with, as this row writes
/// it.
fn event_pattern(written: Written, job: &str) -> String {
    written.render(&json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": { "jobName": [job], "state": [SUCCEEDED] },
    }))
}

/// RULE: a document is read as the JSON it is, whatever insignificant whitespace it carries
/// and whichever encoding of a space the form wire used — so a job run under a role whose
/// trust document admits Glue converges on `SUCCEEDED` with no `ErrorMessage`, and the state
/// change it publishes reaches the target whose own policy admits the events principal for the
/// invoking rule and no other target.
#[tokio::test]
async fn the_documents_a_client_submits_are_read_as_the_json_they_are() {
    let emulator = Emulator::start();

    // The script every row's run executes, staged through Amazon S3's own operations.
    emulator
        .s3(
            "the bucket",
            "PUT",
            &format!("/{BUCKET}"),
            &format!(
                "<CreateBucketConfiguration xmlns=\"{S3_XMLNS}\">\
                 <LocationConstraint>{REGION}</LocationConstraint>\
                 </CreateBucketConfiguration>"
            ),
        )
        .await;
    emulator
        .s3("the script", "PUT", &format!("/{BUCKET}/{SCRIPT_KEY}"), SCRIPT)
        .await;

    let mut built: Vec<Row> = Vec::new();
    for (index, (case, space, written)) in rows().into_iter().enumerate() {
        // The role the row's run is performed as, created and granted through IAM's own
        // operations, on the global endpoint a client's SDK addresses.
        let role_name = format!("glue-run-document-forms-{index}");
        let created = emulator
            .iam(
                case,
                "CreateRole",
                &[
                    ("RoleName", role_name.clone()),
                    ("AssumeRolePolicyDocument", trust_document(written)),
                ],
                space,
            )
            .await;
        let role_arn = element(&created.body, "Arn");
        assert!(
            !role_arn.is_empty(),
            "case {case:?}: CreateRole must answer the role's ARN, got {}",
            created.body
        );
        emulator
            .iam(
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", role_name.clone()),
                    ("PolicyName", "read-the-script".to_string()),
                    ("PolicyDocument", script_access(written)),
                ],
                space,
            )
            .await;

        // The job, the rule that matches its terminal state change, and the two queues.
        let job = format!("glue_run_document_forms_{index}");
        emulator
            .glue(
                case,
                "CreateJob",
                &json!({
                    "Name": &job,
                    "Role": &role_arn,
                    "Command": {
                        "Name": "pythonshell",
                        "PythonVersion": "3.9",
                        "ScriptLocation": format!("s3://{BUCKET}/{SCRIPT_KEY}"),
                    },
                    "MaxCapacity": 0.0625,
                    "Timeout": 5,
                }),
            )
            .await;

        let rule = format!("glue-run-document-forms-{index}");
        let put = emulator
            .events(
                case,
                "PutRule",
                &json!({
                    "Name": &rule,
                    "State": "ENABLED",
                    "EventPattern": event_pattern(written, &job),
                }),
            )
            .await;
        let rule_arn = put["RuleArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: PutRule must answer the rule's ARN, got {put}"))
            .to_string();

        let (permissioned, permissioned_arn) = emulator
            .given_queue(case, &format!("gdf-ok-{index}"))
            .await;
        let (withheld, withheld_arn) = emulator
            .given_queue(case, &format!("gdf-broken-{index}"))
            .await;
        emulator
            .sqs(
                case,
                "SetQueueAttributes",
                &json!({
                    "QueueUrl": &permissioned,
                    "Attributes": {
                        "Policy": queue_policy(written, &permissioned_arn, &rule_arn),
                    },
                }),
            )
            .await;
        let targeted = emulator
            .events(
                case,
                "PutTargets",
                &json!({
                    "Rule": &rule,
                    "Targets": [
                        { "Id": "broken", "Arn": withheld_arn },
                        { "Id": "ok", "Arn": permissioned_arn },
                    ],
                }),
            )
            .await;
        assert_eq!(
            targeted["FailedEntryCount"],
            json!(0),
            "case {case:?}: every queue target must be accepted, got {targeted}"
        );

        // The runs are started together and judged afterwards, so one row's lifecycle does
        // not wait on the row before it.
        let started = emulator
            .glue(case, "StartJobRun", &json!({ "JobName": &job }))
            .await;
        let run_id = started["JobRunId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: StartJobRun must answer a JobRunId, got {started}")
            })
            .to_string();

        built.push(Row {
            case,
            space,
            written,
            job,
            run_id,
            permissioned,
            withheld,
            delivered: Vec::new(),
            nothing: Vec::new(),
        });
    }

    // Each run comes to rest in the state its script reached, carrying no error at all.
    for row in &built {
        let run = emulator
            .run_at_rest(row.case, &row.job, &row.run_id)
            .await;
        assert_eq!(
            run["JobRunState"].as_str(),
            Some(SUCCEEDED),
            "case {:?} ({:?}, {:?}): a run whose role's trust document admits {GLUE_PRINCIPAL} \
             must execute its script and come to rest in {SUCCEEDED}, got {run}",
            row.case,
            row.space,
            row.written
        );
        assert!(
            run.get("ErrorMessage").is_none_or(Value::is_null),
            "case {:?}: a run that succeeded carries no ErrorMessage, got {run}",
            row.case
        );
    }

    // Every admitted delivery is waited for until it is there; the withheld ones are looked
    // at throughout and for a further window afterwards.
    let deadline = std::time::Instant::now() + DELIVERY_DEADLINE;
    loop {
        for row in &mut built {
            let delivered = emulator.drain(row.case, &row.permissioned).await;
            row.delivered.extend(delivered);
            let nothing = emulator.drain(row.case, &row.withheld).await;
            row.nothing.extend(nothing);
        }
        let outstanding: Vec<&str> = built
            .iter()
            .filter(|row| row.delivered.is_empty())
            .map(|row| row.case)
            .collect();
        if outstanding.is_empty() {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no event reached the permissioned queue of {outstanding:?} before the deadline"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    let watching = std::time::Instant::now();
    while watching.elapsed() < WITHHELD_WINDOW {
        for row in &mut built {
            let delivered = emulator.drain(row.case, &row.permissioned).await;
            row.delivered.extend(delivered);
            let nothing = emulator.drain(row.case, &row.withheld).await;
            row.nothing.extend(nothing);
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    for row in &built {
        let case = row.case;
        assert!(
            row.nothing.is_empty(),
            "case {case:?}: the sibling queue, whose policy admits {EVENTS_PRINCIPAL} nothing, \
             must receive no event, got {:?}",
            row.nothing
        );
        assert_eq!(
            row.delivered.len(),
            1,
            "case {case:?}: expected exactly one delivered event, got {:?}",
            row.delivered
        );
        let envelope = &row.delivered[0];
        for (pointer, expected) in [
            ("/source", json!(SOURCE)),
            ("/detail-type", json!(DETAIL_TYPE)),
            ("/region", json!(REGION)),
            ("/detail/jobName", json!(&row.job)),
            ("/detail/jobRunId", json!(&row.run_id)),
            ("/detail/state", json!(SUCCEEDED)),
        ] {
            let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {pointer} to be {expected}, got {actual} in {envelope}"
            );
        }
    }
}
