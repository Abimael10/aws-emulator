// SPDX-License-Identifier: CC0-1.0

//! The two CloudWatch Logs puts that configure a delivery to a Kinesis data stream SERVE a
//! target they can deliver to, and re-read that verdict from current state on every put.
//!
//! RULE: `PutDestination` and `PutSubscriptionFilter` validate the delivery they configure by
//! assuming the `roleArn` the request names and test-delivering a message to the stream the
//! `targetArn`/`destinationArn` names, so each is refused ONLY on a ground the configuration
//! itself establishes — a value that is not an ARN at all, or IAM reporting a role of this
//! account whose trust policy admits no spelling of this service's principal. A stream this
//! account holds `ACTIVE` behind a role this service may assume and that allows the put is
//! CONFIGURED: 200, reported from the stored record. The verdict is read from the CURRENT
//! state at every put, so a target deleted and recreated under the same name — which presents
//! the identical ARN — is configured again by an identical reissued put, and delivery through
//! it resumes without replaying what was ingested while it was gone.
//!
//! This suite needs CloudWatch Logs, IAM and Kinesis on one engine — CloudWatch Logs reaches
//! IAM and the destination through the in-process callback, so a single-service router has
//! neither to ask — which is why it lives here rather than in the CloudWatch Logs crate.
//!
//! The first test's rows differ along what the rule generalises over: the capacity mode the
//! target stream was created in and how many shards it holds, which spelling of this service's
//! principal the role's trust document names and how many of them it names, and the two
//! grounds a put IS refused on (a target that is not an ARN, and a role trusted by another
//! service only). Each served row is driven through the whole logical-destination lifecycle,
//! because a destination that cannot be created makes `DescribeDestinations`,
//! `PutDestinationPolicy` and `DeleteDestination` unreachable. The second test is the
//! re-validation half of the rule, over a target that goes away and comes back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_kinesis_kinesis_20131202::state::{
    shard_id, SHARD_ITERATOR_TYPE_TRIM_HORIZON, STREAM_ACTIVE, STREAM_MODE_ON_DEMAND,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for, and the one every ARN below names.
const REGION: &str = "us-west-2";

/// The error code both puts refuse an undeliverable configuration with.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The error code Kinesis reports a stream it no longer holds with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence CloudWatch Logs refuses a configuration whose test delivery to a Kinesis data
/// stream could not be made with. Fixed text for that error class; its only variable part is
/// the KIND of stream the configuration names.
const UNDELIVERABLE: &str = "Could not deliver test message to specified Kinesis stream. Check \
                             if the given Kinesis stream is in ACTIVE state.";

/// The sentence `PutDestination` refuses a `targetArn` or `roleArn` that is not an ARN with:
/// the two are parsed together, so the refusal names neither member.
const ARN_PARSE_FAILED: &str = "Error parsing ARN from given role or target";

/// How long a wait for a stream to reach a status of its own is given. Generous on purpose:
/// what is under test is the status the stream settles in, never how fast the host gets there.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How often such a wait looks again.
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// The actions a delivery role is given on the destination stream. Source:
/// `PutDestination$roleArn` — "grants CloudWatch Logs permissions to call the Amazon Kinesis
/// PutRecord operation on the destination stream" — and its batch sibling.
const DELIVERY_ACTIONS: [&str; 2] = ["kinesis:PutRecord", "kinesis:PutRecords"];

/// This emulator: the three services the rule spans, on ONE shared engine, with request
/// authentication off — each call carries a credential scope (which is what names the Region)
/// without a computed signature.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// Send one request to the service `signing_name` serves, answering its status and body.
    async fn send(
        &self,
        signing_name: &str,
        content_type: &str,
        target: Option<&str>,
        body: String,
    ) -> (u16, String) {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
                     aws4_request, SignedHeaders=host;x-amz-date, Signature=deadbeef"
                ),
            )
            .header("content-type", content_type);
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let request = request
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, String::from_utf8_lossy(response.body()).to_string())
    }

    /// One `awsJson1_1` operation, answering its status and parsed body.
    async fn json(
        &self,
        signing_name: &str,
        prefix: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let (status, text) = self
            .send(
                signing_name,
                "application/x-amz-json-1.1",
                Some(&format!("{prefix}.{operation}")),
                body.to_string(),
            )
            .await;
        let parsed = if text.is_empty() {
            Value::Null
        } else {
            serde_json::from_str(&text).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One CloudWatch Logs operation.
    async fn logs(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.json("logs", "Logs_20140328", operation, body).await
    }

    /// One CloudWatch Logs operation that must succeed.
    async fn logs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.logs(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: logs {operation} must succeed, got {status} {parsed}"
        );
        parsed
    }

    /// One Kinesis operation.
    async fn kinesis(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.json("kinesis", "Kinesis_20131202", operation, body)
            .await
    }

    /// One Kinesis operation that must succeed.
    async fn kinesis_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.kinesis(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: kinesis {operation} must succeed, got {status} {parsed}"
        );
        parsed
    }

    /// One `awsQuery` IAM action that must succeed, its parameters percent-encoded so a policy
    /// document's braces and quotes survive the form body.
    async fn iam_ok(&self, case: &str, action: &str, parameters: &[(&str, String)]) {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in parameters {
            body.push('&');
            body.push_str(name);
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let (status, answered) = self
            .send("iam", "application/x-www-form-urlencoded", None, body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: IAM {action} must succeed, got {status} {answered}"
        );
    }

    /// Create the delivery role `role`: IAM holds it with the trust document `trust`, and an
    /// inline policy allowing the destination service's own write actions on `stream`.
    async fn delivery_role(&self, case: &str, role: &str, trust: &str, stream: &str) {
        self.iam_ok(
            case,
            "CreateRole",
            &[
                ("RoleName", role.to_string()),
                ("AssumeRolePolicyDocument", trust.to_string()),
            ],
        )
        .await;
        self.iam_ok(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", role.to_string()),
                ("PolicyName", "delivery".to_string()),
                ("PolicyDocument", delivery_policy(stream)),
            ],
        )
        .await;
    }

    /// Create the Kinesis data stream `name` and wait for it to report `ACTIVE`, answering the
    /// `StreamARN` Kinesis itself reports for it.
    ///
    /// `shards` is `None` for an on-demand stream, which names no shard count of its own.
    async fn active_stream(&self, case: &str, name: &str, shards: Option<i32>) -> String {
        let mut request = json!({"StreamName": name});
        match shards {
            Some(count) => {
                request["ShardCount"] = json!(count);
            }
            None => {
                request["StreamModeDetails"] = json!({"StreamMode": STREAM_MODE_ON_DEMAND});
            }
        }
        self.kinesis_ok(case, "CreateStream", &request).await;
        self.await_active(case, name).await
    }

    /// Wait until the stream `name` reports `ACTIVE`, answering the `StreamARN` reported with
    /// it. The wait looks again until the status holds or the deadline passes; it bounds
    /// nothing by how long the transition took.
    async fn await_active(&self, case: &str, name: &str) -> String {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let summary = self
                .kinesis_ok(case, "DescribeStreamSummary", &json!({"StreamName": name}))
                .await;
            let reported = &summary["StreamDescriptionSummary"];
            if reported["StreamStatus"].as_str() == Some(STREAM_ACTIVE) {
                return reported["StreamARN"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {case:?}: an ACTIVE stream reports its ARN, got {summary}")
                    })
                    .to_string();
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected stream {name:?} to reach {STREAM_ACTIVE}, last reported \
                 {summary}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// Wait until Kinesis holds no stream under `name` at all.
    async fn await_absent(&self, case: &str, name: &str) {
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            let (status, answered) = self
                .kinesis("DescribeStreamSummary", &json!({"StreamName": name}))
                .await;
            if status == 400 && error_code(&answered) == RESOURCE_NOT_FOUND {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected stream {name:?} to be gone, last answer was {status} \
                 {answered}"
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// The PARTITION KEY of every record the one-shard stream `name` holds, oldest first.
    ///
    /// The key tells the two writes a configured filter makes apart without decompressing
    /// anything: a `PutSubscriptionFilter`'s health check is keyed on the log group it
    /// validates the filter for, and an envelope of log data grouped by log stream on the log
    /// stream it came from ("By default, log data is grouped by log stream").
    async fn delivered_keys(&self, case: &str, name: &str) -> Vec<String> {
        let iterator = self
            .kinesis_ok(
                case,
                "GetShardIterator",
                &json!({
                    "StreamName": name,
                    "ShardId": shard_id(0),
                    "ShardIteratorType": SHARD_ITERATOR_TYPE_TRIM_HORIZON,
                }),
            )
            .await["ShardIterator"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: GetShardIterator reports an iterator"))
            .to_string();
        let answered = self
            .kinesis_ok(
                case,
                "GetRecords",
                &json!({"ShardIterator": iterator, "Limit": 100}),
            )
            .await;
        answered["Records"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|record| {
                record["PartitionKey"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a record carries a PartitionKey"))
                    .to_string()
            })
            .collect()
    }
}

/// Percent-encode one `application/x-www-form-urlencoded` component, leaving only RFC 3986's
/// unreserved characters literal — so a policy document travels as written.
fn form_encode(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The wire error code of an `awsJson` error body (`__type`, which may carry a namespace).
fn error_code(body: &Value) -> String {
    let raw = body["__type"].as_str().unwrap_or_default();
    raw.rsplit(['#', '/']).next().unwrap_or(raw).to_string()
}

/// A trust policy letting `services` (a JSON string or array of them) assume the role, as a
/// client writes one.
fn trust(services: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":{services}}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// The inline policy a delivery role carries: the destination service's own write actions on
/// the destination stream.
fn delivery_policy(stream_arn: &str) -> String {
    let actions: Vec<String> = DELIVERY_ACTIONS
        .iter()
        .map(|action| format!("\"{action}\""))
        .collect();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\"Action\":[{}],\
         \"Resource\":\"{stream_arn}\"}}]}}",
        actions.join(",")
    )
}

/// The ARN of the Kinesis data stream `name`, in the Region every call here is signed for.
fn stream_arn(name: &str) -> String {
    format!(
        "arn:aws:kinesis:{REGION}:{}:stream/{name}",
        engine::emulator_account_id()
    )
}

/// The IAM role ARN of this account that `name` names. An IAM ARN carries no Region.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// The ARN `PutDestination` derives for the logical destination `name`. Source:
/// `TagResource$resourceArn` — "arn:aws:logs:Region:account-id:destination:destination-name".
fn destination_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{}:destination:{name}",
        engine::emulator_account_id()
    )
}

/// An access policy granting one account the registration of a subscription filter against the
/// destination `name`, which is what `PutDestinationPolicy` attaches to one.
fn access_policy(name: &str) -> String {
    let account = engine::emulator_account_id();
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Sid\":\"cross-account\",\
         \"Effect\":\"Allow\",\"Principal\":{{\"AWS\":\"{account}\"}},\
         \"Action\":\"logs:PutSubscriptionFilter\",\"Resource\":\"{}\"}}]}}",
        destination_arn(name)
    )
}

/// What a put names its target by: a stream this account holds, or a value that is not an ARN
/// at all.
enum Target {
    /// A Kinesis data stream created with that many shards, or on-demand for `None`.
    Stream(Option<i32>),
    /// A `targetArn` of no form at all, the one thing about the target a put parses.
    NotAnArn(&'static str),
}

/// One row: how the target is named, what the role's trust document says, and the put's answer.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// The row's own suffix: its stream, role and destination are named from it, so no row
    /// collides with another.
    name: &'static str,
    target: Target,
    /// The trust document IAM holds for the row's role.
    trust: String,
    /// `None` when the put must be served, else the sentence it must be refused with.
    refusal: Option<&'static str>,
}

/// The table: every form of reachable Kinesis target, and the two grounds a put is refused on.
fn rows() -> Vec<Row> {
    let regional = format!("logs.{REGION}.amazonaws.com");
    vec![
        Row {
            case: "an on-demand stream behind the Regional spelling of this service",
            name: "on-demand-regional",
            target: Target::Stream(None),
            trust: trust(&format!("\"{regional}\"")),
            refusal: None,
        },
        Row {
            case: "a one-shard provisioned stream behind the Regional spelling",
            name: "one-shard-regional",
            target: Target::Stream(Some(1)),
            trust: trust(&format!("\"{regional}\"")),
            refusal: None,
        },
        Row {
            case: "a two-shard provisioned stream behind the global spelling",
            name: "two-shard-global",
            target: Target::Stream(Some(2)),
            trust: trust("\"logs.amazonaws.com\""),
            refusal: None,
        },
        Row {
            case: "an on-demand stream behind a role trusting both spellings",
            name: "on-demand-both",
            target: Target::Stream(None),
            trust: trust(&format!("[\"logs.amazonaws.com\",\"{regional}\"]")),
            refusal: None,
        },
        Row {
            case: "a target that is not an ARN at all",
            name: "unparsable-target",
            target: Target::NotAnArn("not-an-arn"),
            trust: trust(&format!("\"{regional}\"")),
            refusal: Some(ARN_PARSE_FAILED),
        },
        Row {
            case: "a reachable stream behind a role another service only may assume",
            name: "untrusted-role",
            target: Target::Stream(Some(1)),
            trust: trust("\"ec2.amazonaws.com\""),
            refusal: Some(UNDELIVERABLE),
        },
    ]
}

/// RULE: `PutDestination` serves every form of Kinesis target this account holds `ACTIVE`
/// behind a role this service may assume — and the destination it stores is then readable,
/// policy-bearing and deletable — while a target that is not an ARN and a role this service
/// cannot assume are each refused with their own sentence and store nothing.
#[tokio::test]
async fn put_destination_serves_a_reachable_kinesis_target_and_runs_its_lifecycle() {
    let aws = Emulator::new();
    for row in rows() {
        let case = row.case;
        let stream = format!("stream-{}", row.name);
        let role = format!("role-{}", row.name);
        let destination = format!("destination-{}", row.name);

        // The target, as the row names it: a stream polled to ACTIVE presents the ARN Kinesis
        // itself reports for it, which is what the put is handed.
        let target = match row.target {
            Target::Stream(shards) => {
                let reported = aws.active_stream(case, &stream, shards).await;
                assert_eq!(
                    reported,
                    stream_arn(&stream),
                    "case {case:?}: the stream's own ARN is derived from its name, got {reported}"
                );
                reported
            }
            Target::NotAnArn(value) => value.to_string(),
        };
        aws.delivery_role(case, &role, &row.trust, &stream_arn(&stream))
            .await;

        let (status, body) = aws
            .logs(
                "PutDestination",
                &json!({
                    "destinationName": &destination,
                    "targetArn": &target,
                    "roleArn": role_arn(&role),
                    "tags": {"owner": "emulator"},
                }),
            )
            .await;

        if let Some(expected) = row.refusal {
            assert_eq!(
                status, 400,
                "case {case:?}: expected the put to be refused, got {status} {body}"
            );
            assert_eq!(
                (error_code(&body).as_str(), body["message"].as_str()),
                (INVALID_PARAMETER, Some(expected)),
                "case {case:?}: expected {INVALID_PARAMETER} {expected:?}, got {body}"
            );
            let described = aws
                .logs_ok(
                    case,
                    "DescribeDestinations",
                    &json!({"DestinationNamePrefix": &destination}),
                )
                .await;
            assert_eq!(
                described["destinations"].as_array().map(Vec::len),
                Some(0),
                "case {case:?}: a refused put stores no destination, got {described}"
            );
            continue;
        }

        assert_eq!(
            status, 200,
            "case {case:?}: expected the put to be served, got {status} {body}"
        );
        let reported = &body["destination"];
        assert_eq!(
            (
                reported["destinationName"].as_str(),
                reported["targetArn"].as_str(),
                reported["roleArn"].as_str(),
                reported["arn"].as_str(),
            ),
            (
                Some(destination.as_str()),
                Some(target.as_str()),
                Some(role_arn(&role).as_str()),
                Some(destination_arn(&destination).as_str()),
            ),
            "case {case:?}: the put reports the stored destination, got {body}"
        );
        let created = reported["creationTime"].as_i64().unwrap_or_default();
        assert!(
            created > 0,
            "case {case:?}: the destination carries a creation time, got {body}"
        );
        // "By default, PutDestination does not set any access policy with the destination."
        assert!(
            reported["accessPolicy"].is_null(),
            "case {case:?}: the put attaches no access policy, got {body}"
        );

        // The destination the put stored is the one the account holds.
        let described = aws
            .logs_ok(
                case,
                "DescribeDestinations",
                &json!({"DestinationNamePrefix": &destination}),
            )
            .await;
        assert_eq!(
            described["destinations"].as_array().map(Vec::len),
            Some(1),
            "case {case:?}: the account holds exactly the destination just created, got \
             {described}"
        );
        assert_eq!(
            (
                described["destinations"][0]["targetArn"].as_str(),
                described["destinations"][0]["creationTime"].as_i64(),
            ),
            (Some(target.as_str()), Some(created)),
            "case {case:?}: the stored destination is reported unchanged, got {described}"
        );

        // The access policy a destination is given round-trips on the read of it, and the
        // creation time survives the update.
        let policy = access_policy(&destination);
        aws.logs_ok(
            case,
            "PutDestinationPolicy",
            &json!({"destinationName": &destination, "accessPolicy": &policy}),
        )
        .await;
        let described = aws
            .logs_ok(
                case,
                "DescribeDestinations",
                &json!({"DestinationNamePrefix": &destination}),
            )
            .await;
        assert_eq!(
            (
                described["destinations"][0]["accessPolicy"].as_str(),
                described["destinations"][0]["creationTime"].as_i64(),
            ),
            (Some(policy.as_str()), Some(created)),
            "case {case:?}: the access policy round-trips on the destination, got {described}"
        );

        // And the destination is deletable, which is the end of the lifecycle the put opens.
        aws.logs_ok(
            case,
            "DeleteDestination",
            &json!({"destinationName": &destination}),
        )
        .await;
        let described = aws
            .logs_ok(
                case,
                "DescribeDestinations",
                &json!({"DestinationNamePrefix": &destination}),
            )
            .await;
        assert_eq!(
            described["destinations"].as_array().map(Vec::len),
            Some(0),
            "case {case:?}: the deleted destination is gone, got {described}"
        );
    }
}

/// The log group, log stream, filter and target of the re-validation test.
const GROUP: &str = "/aws/emulator/revalidated";
const LOG_STREAM: &str = "instance-one";
const FILTER: &str = "to-kinesis";
const TARGET_STREAM: &str = "revalidated-target";
const DELIVERY_ROLE: &str = "revalidated-delivery";

/// The filter pattern every event below is selected by, and the two messages: one the pattern
/// matches, one it does not.
const PATTERN: &str = "MATCHED";
const MATCHING: &str = "first MATCHED line";
const UNMATCHED: &str = "first ignored line";

/// RULE: `PutSubscriptionFilter` resolves its destination from CURRENT state on every put, so a
/// direct Kinesis destination that is deleted and recreated under the same name — presenting
/// the identical ARN — is configured again by an identical reissued put, after which delivery
/// resumes and what was ingested during the outage is not replayed to it.
#[tokio::test]
async fn a_subscription_filter_to_kinesis_revalidates_a_recreated_target() {
    let case = "a Kinesis destination deleted and recreated under the same name";
    let aws = Emulator::new();

    aws.logs_ok(case, "CreateLogGroup", &json!({"logGroupName": GROUP}))
        .await;
    aws.logs_ok(
        case,
        "CreateLogStream",
        &json!({"logGroupName": GROUP, "logStreamName": LOG_STREAM}),
    )
    .await;
    let first_arn = aws.active_stream(case, TARGET_STREAM, Some(1)).await;
    aws.delivery_role(
        case,
        DELIVERY_ROLE,
        &trust(&format!("\"logs.{REGION}.amazonaws.com\"")),
        &first_arn,
    )
    .await;

    // The put the issue is about: a direct destination naming an ACTIVE Kinesis stream, under a
    // role this service may assume and that allows the write, is CONFIGURED.
    let configure = json!({
        "logGroupName": GROUP,
        "filterName": FILTER,
        "filterPattern": PATTERN,
        "destinationArn": &first_arn,
        "roleArn": role_arn(DELIVERY_ROLE),
    });
    let (status, body) = aws.logs("PutSubscriptionFilter", &configure).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the first put to be served, got {status} {body}"
    );
    let held = aws
        .logs_ok(
            case,
            "DescribeSubscriptionFilters",
            &json!({"logGroupName": GROUP}),
        )
        .await;
    assert_eq!(
        (
            held["subscriptionFilters"][0]["filterName"].as_str(),
            held["subscriptionFilters"][0]["destinationArn"].as_str(),
            held["subscriptionFilters"][0]["roleArn"].as_str(),
        ),
        (
            Some(FILTER),
            Some(first_arn.as_str()),
            Some(role_arn(DELIVERY_ROLE).as_str()),
        ),
        "case {case:?}: the group holds the filter as configured, got {held}"
    );

    // Delivery through it: the put's own health check, keyed on the log group, then the
    // envelope of the matching event, keyed on the log stream.
    aws.logs_ok(
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": LOG_STREAM,
            "logEvents": [
                {"timestamp": now_ms(), "message": MATCHING},
                {"timestamp": now_ms(), "message": UNMATCHED},
            ],
        }),
    )
    .await;
    let keys = aws.delivered_keys(case, TARGET_STREAM).await;
    assert_eq!(
        keys,
        vec![GROUP.to_string(), LOG_STREAM.to_string()],
        "case {case:?}: expected the health check and one envelope on the target, got {keys:?}"
    );

    // The outage: the target goes away entirely, and an event ingested while it is gone is
    // stored by the log group all the same.
    aws.kinesis_ok(
        case,
        "DeleteStream",
        &json!({"StreamName": TARGET_STREAM, "EnforceConsumerDeletion": true}),
    )
    .await;
    aws.await_absent(case, TARGET_STREAM).await;
    let outage_line = "outage MATCHED line";
    aws.logs_ok(
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": LOG_STREAM,
            "logEvents": [{"timestamp": now_ms(), "message": outage_line}],
        }),
    )
    .await;

    // The target comes back under the SAME name, so it presents the identical ARN.
    let second_arn = aws.active_stream(case, TARGET_STREAM, Some(1)).await;
    assert_eq!(
        second_arn, first_arn,
        "case {case:?}: a stream recreated under the same name presents the same ARN, got \
         {second_arn}"
    );

    // The documented repair: reissue the IDENTICAL put. It is served, because the destination
    // is resolved from current state rather than from a verdict cached per ARN.
    let (status, body) = aws.logs("PutSubscriptionFilter", &configure).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the reissued put to be served, got {status} {body}"
    );

    // Delivery resumes for what is ingested from here on, and the outage event is not replayed:
    // the recreated target holds the re-put's health check and the new batch's envelope, and
    // nothing else.
    let resumed_line = "resumed MATCHED line";
    aws.logs_ok(
        case,
        "PutLogEvents",
        &json!({
            "logGroupName": GROUP,
            "logStreamName": LOG_STREAM,
            "logEvents": [{"timestamp": now_ms(), "message": resumed_line}],
        }),
    )
    .await;
    let keys = aws.delivered_keys(case, TARGET_STREAM).await;
    assert_eq!(
        keys,
        vec![GROUP.to_string(), LOG_STREAM.to_string()],
        "case {case:?}: expected the re-put's health check and only the new envelope on the \
         recreated target, got {keys:?}"
    );

    // Every ingested event stays readable whatever the delivery did, the outage one included.
    let page = aws
        .logs_ok(
            case,
            "GetLogEvents",
            &json!({"logGroupName": GROUP, "logStreamName": LOG_STREAM}),
        )
        .await;
    let messages: Vec<String> = page["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|event| {
            event["message"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a stored event carries a message"))
                .to_string()
        })
        .collect();
    assert_eq!(
        messages,
        vec![
            MATCHING.to_string(),
            UNMATCHED.to_string(),
            outage_line.to_string(),
            resumed_line.to_string(),
        ],
        "case {case:?}: every ingested event stays readable, got {messages:?}"
    );
}

/// The current wall clock in epoch milliseconds, which is what a log event is stamped with.
fn now_ms() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("the clock is after the Unix epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit in an i64")
}
