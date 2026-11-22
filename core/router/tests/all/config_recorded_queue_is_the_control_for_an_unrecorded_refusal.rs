// SPDX-License-Identifier: CC0-1.0

//! What AWS Config answers about a queue it RECORDED and about one nothing ever created,
//! on the assembled emulator and over the provisioning sequence AWS documents.
//!
//! The rule under test: while a customer managed configuration recorder is RECORDING and
//! its recording group covers a resource type, Config discovers every resource of that
//! type the owning service holds and records a configuration item for it — so the
//! resource's `GetResourceConfigHistory` answers HTTP 200 with at least one item, whose
//! `arn` is the ARN the owning service minted and whose `configuration` carries the
//! resource's own live configuration, and `BatchGetResourceConfig` answers one
//! `baseConfigurationItem` for its key — while a well-formed key the account recorded
//! nothing for is refused HTTP 400 `ResourceNotDiscoveredException` by the history read
//! and answered by OMISSION, HTTP 200 with nothing in it, by `BatchGetResourceConfig`
//! (empty `baseConfigurationItems` and `unprocessedResourceKeys`) and by
//! `SelectResourceConfig` (empty `Results`).
//!
//! The recorded resource is the CONTROL the refusal is read against: a refusal answered by
//! an account that records nothing at all would say only that, so both legs are asserted
//! against one account, and the control is read again afterwards to show the refusal left
//! it where it was.
//!
//! Four crates have to agree for that to hold, which is why the suite lives here: IAM holds
//! the role the recorder runs as, Amazon S3 holds the delivery bucket and the policy in
//! force on it, Amazon SQS owns the queues and is the API the recorder reads them through,
//! and Config records, reads and refuses. Every service is mounted on ONE engine, exactly
//! as `core/router/src/lib.rs` mounts them.
//!
//! The rows differ along what the rule generalises over: for the recorded leg, how the
//! queue is CONFIGURED (the attributes the queue carries, including a FIFO queue and a
//! queue configured with nothing at all), and for the refused leg, the FORM of the key the
//! account holds nothing for (a queue url of the same shape as the control's, a bare queue
//! name, a url of another Region, and a key of another resource type altogether). A third
//! case reads the same rule for the ENDPOINT the client reached: a client pointed at the
//! emulator's own data port is handed queue urls at that address, and the configuration item
//! has to record the queue under the url that client holds.
//!
//! Nothing here asserts on elapsed time or sleeps for a span: the recording pass the clock
//! owes is waited for by polling the CONDITION under a generous deadline, and nothing here
//! calls `PutResourceConfig` — every configuration item asserted on is one the recorder
//! observed by itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The credential-scope header builder and the awsQuery helpers this crate already owns: a
// suite that writes its own drifts from the others in what it signs and how it reads XML.
use super::cloudformation_provisions_s3vectors_resources::authorization;
use super::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region every resource belongs
/// to.
const REGION: &str = "us-east-1";

/// Config's `X-Amz-Target` prefix (its model's service shape, `StarlingDoveService`) and
/// the signing name its credential scope carries (`sigv4Name: config`).
const CONFIG_TARGET_PREFIX: &str = "StarlingDoveService";
/// See [`CONFIG_TARGET_PREFIX`].
const CONFIG_SIGNING_NAME: &str = "config";

/// Amazon SQS' `X-Amz-Target` prefix and signing name.
const SQS_TARGET_PREFIX: &str = "AmazonSQS";
/// See [`SQS_TARGET_PREFIX`].
const SQS_SIGNING_NAME: &str = "sqs";

/// IAM's awsQuery API version, from that crate's `OPERATIONS.md` wire-protocol section.
const IAM_VERSION: &str = "2010-05-08";

/// The service principal Config assumes the recorder's role as, which the role's trust
/// policy therefore has to admit and the delivery bucket's policy has to allow.
const CONFIG_PRINCIPAL: &str = "config.amazonaws.com";

/// The resource type Config records an Amazon SQS queue under, and one other published
/// type, both `com.amazonaws.configservice#ResourceType` values
/// (`smithy_query(query="shape", shape="ResourceType")`).
const QUEUE_TYPE: &str = "AWS::SQS::Queue";
/// See [`QUEUE_TYPE`].
const BUCKET_TYPE: &str = "AWS::S3::Bucket";

/// The attribute Amazon SQS reports a queue's own ARN under
/// (`com.amazonaws.sqs#QueueAttributeName`), which is what the queue's configuration item
/// carries as its `arn`.
const QUEUE_ARN_ATTRIBUTE: &str = "QueueArn";

/// The names this suite provisions the account under: one recorder, one delivery bucket and
/// one role.
const RECORDER: &str = "sim2real-config-recorder";
/// See [`RECORDER`].
const DELIVERY_BUCKET: &str = "sim2real-config-delivery";
/// See [`RECORDER`].
const RECORDER_ROLE: &str = "sim2real-config-recorder-role";

/// The error a configuration-item read answers for a well-formed resource key the account
/// has recorded no configuration item for, and the status it carries
/// (`smithy_query(query="operation", operation="GetResourceConfigHistory")`).
const NOT_DISCOVERED: &str = "ResourceNotDiscoveredException";
/// See [`NOT_DISCOVERED`].
const CLIENT_ERROR: u16 = 400;

/// How long a poll waits for the recording pass the clock owes. A pass falls due a fraction
/// of a second after the call that makes it due, so this is many passes' worth of slack and
/// not a span any assertion here depends on: every wait ends on its CONDITION, and the
/// budget is there only so a wedged emulator fails instead of hanging.
const RECORDED_DEADLINE: Duration = Duration::from_secs(180);

/// How long a poll waits between two reads: short enough that the wait is the condition's
/// own, long enough not to spin.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// This emulator's account, which is the account every resource below belongs to.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The address a served emulator publishes its data port at, recorded the way the core
/// records it on boot — which is what every url a service hands a caller is built from, the
/// queue urls of the served-endpoint case among them.
const SERVED_AT: &str = "https://127.0.0.1:9297";

/// See [`SERVED_AT`]: the authority of that address, which is the host a client pointed at
/// the emulator's own endpoint addresses it by.
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// IAM, Amazon S3, Amazon SQS and Config over ONE shared engine.
struct Emulator {
    router: MultiServiceRouter,
    /// The host the Config and Amazon SQS calls address, for a client that reaches the
    /// emulator's OWN endpoint rather than resolving AWS's service hostnames; `None` for a
    /// client that addresses each service's own host.
    endpoint_host: Option<&'static str>,
}

impl Emulator {
    fn new() -> Self {
        Self::built(None)
    }

    /// The same account, reached the way a client of a SERVED emulator reaches it: the data
    /// port's own address recorded on the router as the core records it on boot, and every
    /// Config and Amazon SQS call addressed to that endpoint.
    fn served() -> Self {
        let emulator = Self::built(Some(SERVED_AUTHORITY));
        ::emulator_server::EmulatorService::serving_at(&emulator.router, SERVED_AT);
        emulator
    }

    fn built(endpoint_host: Option<&'static str>) -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Signing is not the subject here, so signatures are not verified; the credential
        // scope is still carried, because it is what names the Region and routes the call.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_configservice_starlingdoveservice::EmulatedService::new(engine),
        );
        Self {
            router,
            endpoint_host,
        }
    }

    /// The host a call of `signing_name` addresses: the emulator's own endpoint where this
    /// account is reached that way, else that service's own regional host.
    fn host(&self, signing_name: &str) -> String {
        match self.endpoint_host {
            Some(host) => host.to_string(),
            None => format!("{signing_name}.{REGION}.amazonaws.com"),
        }
    }

    /// One awsQuery call to IAM, every component percent-encoded so a policy document
    /// travels verbatim.
    async fn iam_ok(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version={IAM_VERSION}");
        for (name, value) in members {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("iam.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", authorization("iam", REGION))
            .body(Bytes::from(body))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let body = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {body}"
        );
        body
    }

    /// One S3 call on the path-style address, which must be served.
    async fn s3_ok(&self, case: &str, path: &str, body: &str) {
        let request = http::Request::builder()
            .method("PUT")
            .uri(path)
            .header("host", format!("s3.{REGION}.amazonaws.com"))
            .header("content-type", "application/xml")
            .header("authorization", authorization("s3", REGION))
            .body(Bytes::from(body.to_string()))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = String::from_utf8_lossy(response.body()).to_string();
        assert!(
            (200..300).contains(&status),
            "case {case:?}: expected S3 PUT {path} to be served, got {status} with {answered}"
        );
    }

    /// One `awsJson` RPC call, answering the HTTP status and the parsed body.
    async fn rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        json_version: &str,
        operation: &str,
        body: &Value,
    ) -> (u16, Value) {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", self.host(signing_name))
            .header(
                "content-type",
                format!("application/x-amz-json-{json_version}"),
            )
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", authorization(signing_name, REGION))
            .body(Bytes::from(
                serde_json::to_vec(body).expect("the request serializes"),
            ))
            .expect("the test request is well formed");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One Config call (`awsJson1_1`).
    async fn config(&self, operation: &str, body: &Value) -> (u16, Value) {
        self.rpc(
            CONFIG_SIGNING_NAME,
            CONFIG_TARGET_PREFIX,
            "1.1",
            operation,
            body,
        )
        .await
    }

    /// A Config call that must answer 200.
    async fn config_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self.config(operation, body).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// An Amazon SQS call (`awsJson1_0`) that must answer 200.
    async fn sqs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let (status, parsed) = self
            .rpc(SQS_SIGNING_NAME, SQS_TARGET_PREFIX, "1.0", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from {operation}, got {status} with {parsed}"
        );
        parsed
    }

    /// Provision the account the way AWS documents it and START the recorder: the role
    /// Config assumes, the delivery bucket and the policy that admits the delivery, the
    /// recorder recording every supported resource type, and its delivery channel.
    ///
    /// Answers nothing: what it leaves behind is an account whose recorder is RECORDING,
    /// which is the precondition both legs below are read against.
    async fn given_recording(&self, case: &str) {
        let created = self
            .iam_ok(
                case,
                "CreateRole",
                &[
                    ("RoleName", RECORDER_ROLE),
                    ("AssumeRolePolicyDocument", &trust_policy()),
                ],
            )
            .await;
        let role_arn = element(&created, "Arn");
        self.s3_ok(case, &format!("/{DELIVERY_BUCKET}"), "").await;
        self.s3_ok(
            case,
            &format!("/{DELIVERY_BUCKET}?policy"),
            &delivery_bucket_policy(),
        )
        .await;
        self.config_ok(
            case,
            "PutConfigurationRecorder",
            &json!({
                "ConfigurationRecorder": {
                    "name": RECORDER,
                    "roleARN": role_arn,
                    "recordingGroup": {
                        "allSupported": true,
                        "includeGlobalResourceTypes": false,
                    },
                }
            }),
        )
        .await;
        self.config_ok(
            case,
            "PutDeliveryChannel",
            &json!({ "DeliveryChannel": { "s3BucketName": DELIVERY_BUCKET } }),
        )
        .await;
        self.config_ok(
            case,
            "StartConfigurationRecorder",
            &json!({ "ConfigurationRecorderName": RECORDER }),
        )
        .await;
        let status = self
            .config_ok(
                case,
                "DescribeConfigurationRecorderStatus",
                &json!({ "ConfigurationRecorderNames": [RECORDER] }),
            )
            .await;
        assert_eq!(
            status["ConfigurationRecordersStatus"][0]["recording"].as_bool(),
            Some(true),
            "case {case:?}: expected the started recorder to report that it is recording, got \
             {status}"
        );
    }

    /// Create the queue called `name` with `attributes` through Amazon SQS itself, and
    /// answer its url and the ARN Amazon SQS minted for it.
    async fn queue(&self, case: &str, name: &str, attributes: &[(&str, &str)]) -> (String, String) {
        let mut request = json!({ "QueueName": name });
        if !attributes.is_empty() {
            request["Attributes"] = attributes
                .iter()
                .map(|(attribute, value)| ((*attribute).to_string(), json!(value)))
                .collect();
        }
        let created = self.sqs_ok(case, "CreateQueue", &request).await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl, got {created}"))
            .to_string();
        let read = self
            .sqs_ok(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": &url, "AttributeNames": ["All"] }),
            )
            .await;
        let arn = read["Attributes"][QUEUE_ARN_ATTRIBUTE]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a {QUEUE_ARN_ATTRIBUTE}, got {read}"))
            .to_string();
        (url, arn)
    }

    /// The configuration items `GetResourceConfigHistory` reports for one resource key, or
    /// `None` for a key Config refuses as undiscovered — which is how the read answers a
    /// key the account recorded nothing for.
    async fn history(&self, case: &str, resource_type: &str, resource_id: &str) -> Option<Vec<Value>> {
        let (status, body) = self
            .config(
                "GetResourceConfigHistory",
                &json!({ "resourceType": resource_type, "resourceId": resource_id }),
            )
            .await;
        if status == CLIENT_ERROR && error_code(&body) == NOT_DISCOVERED {
            return None;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the history of {resource_id:?} must be served or refused as \
             undiscovered, got {status} with {body}"
        );
        Some(
            body["configurationItems"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// The configuration items of one resource key, waited for until the recording pass the
    /// clock owes has recorded at least one.
    ///
    /// Polls the CONDITION rather than sleeping a span, and fails only when the key still
    /// has no item at [`RECORDED_DEADLINE`], naming what the read last answered.
    async fn recorded_items(&self, case: &str, resource_type: &str, resource_id: &str) -> Vec<Value> {
        let deadline = Instant::now() + RECORDED_DEADLINE;
        loop {
            match self.history(case, resource_type, resource_id).await {
                Some(items) if !items.is_empty() => return items,
                // `None` is the refusal, an empty list a key with no item yet.
                reported => assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the running recorder to record a {resource_type} \
                     configuration item for {resource_id:?}, last read {reported:?}"
                ),
            }
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// What `BatchGetResourceConfig` answers for ONE resource key: its base configuration
    /// items and the keys it could not process.
    async fn batch(
        &self,
        case: &str,
        resource_type: &str,
        resource_id: &str,
    ) -> (Vec<Value>, Vec<Value>) {
        let answered = self
            .config_ok(
                case,
                "BatchGetResourceConfig",
                &json!({
                    "resourceKeys": [
                        { "resourceType": resource_type, "resourceId": resource_id },
                    ],
                }),
            )
            .await;
        (
            answered["baseConfigurationItems"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
            answered["unprocessedResourceKeys"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        )
    }

    /// The rows `SelectResourceConfig` answers for one resource key, read through the query
    /// a client asks that question with.
    async fn selected(&self, case: &str, resource_type: &str, resource_id: &str) -> Vec<Value> {
        let answered = self
            .config_ok(
                case,
                "SelectResourceConfig",
                &json!({
                    "Expression": format!(
                        "SELECT resourceId WHERE resourceId = '{resource_id}' AND resourceType \
                         = '{resource_type}'"
                    ),
                }),
            )
            .await;
        answered["Results"].as_array().cloned().unwrap_or_default()
    }
}

/// The wire error code of a Config refusal: awsJson1_1 carries it as the body's `__type`.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit(['#', '/'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// The `message` of a Config refusal.
fn error_message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The trust policy that lets Config assume the recorder's role: one `Allow` of
/// `sts:AssumeRole` to the Config service principal.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": CONFIG_PRINCIPAL },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The bucket policy AWS documents for a Config delivery channel's bucket: one `Allow` for
/// the Config service principal per S3 call a delivery performs, each conditioned on the
/// account the delivery is made for.
fn delivery_bucket_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "AWSConfigBucketPermissionsCheck",
                "Effect": "Allow",
                "Principal": { "Service": CONFIG_PRINCIPAL },
                "Action": ["s3:GetBucketAcl", "s3:ListBucket"],
                "Resource": format!("arn:aws:s3:::{DELIVERY_BUCKET}"),
                "Condition": { "StringEquals": { "AWS:SourceAccount": account() } },
            },
            {
                "Sid": "AWSConfigBucketDelivery",
                "Effect": "Allow",
                "Principal": { "Service": CONFIG_PRINCIPAL },
                "Action": "s3:PutObject",
                "Resource": format!("arn:aws:s3:::{DELIVERY_BUCKET}/*"),
                "Condition": {
                    "StringEquals": {
                        "s3:x-amz-acl": "bucket-owner-full-control",
                        "AWS:SourceAccount": account(),
                    },
                },
            },
        ],
    })
    .to_string()
}

/// How Config words the refusal of a resource key it has recorded nothing for, with the id
/// and the type taken from the request.
fn not_discovered_message(resource_id: &str, resource_type: &str) -> String {
    format!(
        "Resource {resource_id} of resourceType:{resource_type} is unknown or has not been \
         discovered"
    )
}

/// One queue of the recorded leg: how it is named and how it is CONFIGURED.
struct Recorded {
    case: &'static str,
    name: &'static str,
    attributes: &'static [(&'static str, &'static str)],
}

/// The queues the recorded leg creates under the running recorder. The rows differ along the
/// queue's own configuration — which attributes it carries, a FIFO queue among them, and a
/// queue configured with nothing at all — because what a configuration item records is the
/// resource's configuration, whatever that configuration is.
const RECORDED_QUEUES: [Recorded; 4] = [
    Recorded {
        case: "a queue whose visibility timeout is configured",
        name: "sim2real-cfg-visibility",
        attributes: &[("VisibilityTimeout", "137")],
    },
    Recorded {
        case: "a queue carrying a delay, a retention period and a receive wait",
        name: "sim2real-cfg-delayed",
        attributes: &[
            ("DelaySeconds", "45"),
            ("MessageRetentionPeriod", "600"),
            ("ReceiveMessageWaitTimeSeconds", "7"),
        ],
    },
    Recorded {
        case: "a FIFO queue deduplicating on content",
        name: "sim2real-cfg-ordered.fifo",
        attributes: &[("FifoQueue", "true"), ("ContentBasedDeduplication", "true")],
    },
    Recorded {
        case: "a queue created with no attributes at all",
        name: "sim2real-cfg-plain",
        attributes: &[],
    },
];

#[tokio::test]
async fn a_queue_created_under_a_running_recorder_acquires_its_configuration_item() {
    let emulator = Emulator::new();
    emulator.given_recording("the recorded leg").await;

    // Every queue is created AFTER the recorder started recording, so each item asserted on
    // is one the recorder discovered by itself.
    let mut created: Vec<(&Recorded, String, String)> = Vec::new();
    for row in &RECORDED_QUEUES {
        let (url, arn) = emulator.queue(row.case, row.name, row.attributes).await;
        created.push((row, url, arn));
    }

    for (row, url, arn) in &created {
        let case = row.case;
        let items = emulator.recorded_items(case, QUEUE_TYPE, url).await;
        let latest = &items[0];
        assert_eq!(
            (
                latest["resourceType"].as_str(),
                latest["resourceId"].as_str(),
                latest["arn"].as_str(),
                latest["awsRegion"].as_str(),
                latest["accountId"].as_str(),
            ),
            (
                Some(QUEUE_TYPE),
                Some(url.as_str()),
                Some(arn.as_str()),
                Some(REGION),
                Some(account().as_str()),
            ),
            "case {case:?}: expected the item to record the queue Amazon SQS holds, got {latest}"
        );
        assert!(
            latest["configurationItemStatus"]
                .as_str()
                .is_some_and(|status| !status.is_empty()),
            "case {case:?}: expected the item to report the status it was recorded with, got \
             {latest}"
        );

        // The item's `configuration` is the queue's own live configuration, so every
        // attribute the row configured is in it under the name Amazon SQS reports it by.
        let configuration: Value = serde_json::from_str(
            latest["configuration"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: expected a configuration, got {latest}")),
        )
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON configuration, got {latest}"));
        for (attribute, value) in row.attributes {
            assert_eq!(
                configuration[*attribute].as_str(),
                Some(*value),
                "case {case:?}: expected the recorded configuration to carry {attribute} as \
                 {value:?}, got {configuration}"
            );
        }
        assert_eq!(
            configuration[QUEUE_ARN_ATTRIBUTE].as_str(),
            Some(arn.as_str()),
            "case {case:?}: expected the recorded configuration to carry the queue's own ARN, \
             got {configuration}"
        );

        // The same inventory read through the batch surface: one base configuration item for
        // the key, and nothing left unprocessed.
        let (held, unprocessed) = emulator.batch(case, QUEUE_TYPE, url).await;
        assert_eq!(
            (
                held.len(),
                held.first().and_then(|item| item["resourceId"].as_str()),
                held.first().and_then(|item| item["arn"].as_str()),
                unprocessed.len(),
            ),
            (1, Some(url.as_str()), Some(arn.as_str()), 0),
            "case {case:?}: expected one base configuration item for the recorded key and \
             nothing unprocessed, got {held:?} and {unprocessed:?}"
        );

        // And through the query surface, which answers a row per recorded resource matched.
        let selected = emulator.selected(case, QUEUE_TYPE, url).await;
        assert_eq!(
            selected.len(),
            1,
            "case {case:?}: expected the query to answer one row for the recorded queue, got \
             {selected:?}"
        );
    }
}

/// One key of the refused leg: a well-formed resource key the account recorded nothing for.
struct Absent {
    case: &'static str,
    resource_type: &'static str,
    /// Built from the control queue's url, so a row can name a key of the SAME shape as the
    /// control's and differ from it only in the resource it names.
    resource_id: fn(&str) -> String,
}

/// The keys the refused leg asks about. The rows differ along the FORM of the id — a queue
/// url of the same shape as the control's, the queue's bare name, a url naming another
/// Region's endpoint — and along the resource TYPE, which the refusal's message carries
/// verbatim.
const ABSENT_KEYS: [Absent; 4] = [
    Absent {
        case: "a queue url of the same shape as the control's",
        resource_type: QUEUE_TYPE,
        resource_id: |control| sibling_url(control, "sim2real-cfg-never-created"),
    },
    Absent {
        case: "a bare queue name nothing was created under",
        resource_type: QUEUE_TYPE,
        resource_id: |_control| "sim2real-cfg-never-created".to_string(),
    },
    Absent {
        case: "a queue url naming another Region's endpoint",
        resource_type: QUEUE_TYPE,
        resource_id: |_control| {
            format!(
                "https://sqs.eu-west-1.amazonaws.com/{}/sim2real-cfg-never-created",
                engine::emulator_account_id()
            )
        },
    },
    Absent {
        case: "a key of another resource type altogether",
        resource_type: BUCKET_TYPE,
        resource_id: |_control| "sim2real-cfg-never-created-bucket".to_string(),
    },
];

/// The url a queue called `name` would have at the endpoint `control` names: the control's
/// own url with its last segment replaced, which is what makes a row's key the same SHAPE as
/// the control's.
fn sibling_url(control: &str, name: &str) -> String {
    match control.rsplit_once('/') {
        Some((prefix, _)) => format!("{prefix}/{name}"),
        None => name.to_string(),
    }
}

#[tokio::test]
async fn an_unrecorded_key_is_refused_by_the_history_and_omitted_by_the_batch_and_query_reads() {
    let case = "the refused leg";
    let emulator = Emulator::new();
    emulator.given_recording(case).await;

    // The CONTROL: a queue the running recorder really recorded, so a refusal below means
    // "this resource is not recorded" rather than "this account records nothing at all".
    let (control, control_arn) = emulator
        .queue(case, "sim2real-cfg-control", &[("VisibilityTimeout", "137")])
        .await;
    let recorded = emulator.recorded_items(case, QUEUE_TYPE, &control).await;
    assert_eq!(
        recorded[0]["arn"].as_str(),
        Some(control_arn.as_str()),
        "case {case:?}: expected the control item to carry the queue's own ARN, got {recorded:?}"
    );

    for row in &ABSENT_KEYS {
        let case = row.case;
        let resource_type = row.resource_type;
        let resource_id = (row.resource_id)(&control);

        // The history read REFUSES the key, naming the id and the type the request sent.
        let (status, refused) = emulator
            .config(
                "GetResourceConfigHistory",
                &json!({ "resourceType": resource_type, "resourceId": &resource_id }),
            )
            .await;
        assert_eq!(
            (status, error_code(&refused), error_message(&refused)),
            (
                CLIENT_ERROR,
                NOT_DISCOVERED.to_string(),
                not_discovered_message(&resource_id, resource_type),
            ),
            "case {case:?}: expected the history of an unrecorded key to be refused, got \
             {status} with {refused}"
        );

        // The batch read answers the same absence by OMISSION: served, with nothing in it
        // and nothing left unprocessed.
        let (held, unprocessed) = emulator.batch(case, resource_type, &resource_id).await;
        assert_eq!(
            (held.len(), unprocessed.len()),
            (0, 0),
            "case {case:?}: expected the batch read to omit an unrecorded key from both of its \
             lists, got {held:?} and {unprocessed:?}"
        );

        // And so does the query surface.
        let selected = emulator.selected(case, resource_type, &resource_id).await;
        assert_eq!(
            selected.len(),
            0,
            "case {case:?}: expected the query to answer no row for an unrecorded key, got \
             {selected:?}"
        );
    }

    // The refusals changed nothing about the control: it is still readable through both
    // reads, which is what makes the contrast between the two legs a statement about the
    // resources and not about the account.
    let items = emulator
        .history(case, QUEUE_TYPE, &control)
        .await
        .unwrap_or_default();
    assert_eq!(
        items.first().and_then(|item| item["arn"].as_str()),
        Some(control_arn.as_str()),
        "case {case:?}: expected the recorded control to still be readable, got {items:?}"
    );
    let (held, unprocessed) = emulator.batch(case, QUEUE_TYPE, &control).await;
    assert_eq!(
        (
            held.len(),
            held.first().and_then(|item| item["arn"].as_str()),
            unprocessed.len(),
        ),
        (1, Some(control_arn.as_str()), 0),
        "case {case:?}: expected the batch read to still answer the control's key, got {held:?} \
         and {unprocessed:?}"
    );
}

#[tokio::test]
async fn a_recorded_queue_is_identified_by_the_url_the_client_was_handed() {
    let case = "a client reaching the emulator's own endpoint";
    let emulator = Emulator::served();
    emulator.given_recording(case).await;

    let (url, arn) = emulator
        .queue(case, "sim2real-cfg-served", &[("VisibilityTimeout", "137")])
        .await;
    // The row is only about this addressing while the url the client was handed really names
    // the endpoint it reached.
    assert!(
        url.contains(SERVED_AUTHORITY),
        "case {case:?}: expected Amazon SQS to hand back a url at the endpoint the call \
         reached, got {url:?}"
    );

    // The recorder reads this account's queues through Amazon SQS itself, on a call of its
    // own; the item it records still names the queue by the url the CLIENT holds, so the
    // client's own id resolves.
    let items = emulator.recorded_items(case, QUEUE_TYPE, &url).await;
    assert_eq!(
        (
            items[0]["resourceId"].as_str(),
            items[0]["resourceType"].as_str(),
            items[0]["arn"].as_str(),
        ),
        (Some(url.as_str()), Some(QUEUE_TYPE), Some(arn.as_str())),
        "case {case:?}: expected the item to record the queue under the client's own url, got \
         {items:?}"
    );
    let (held, unprocessed) = emulator.batch(case, QUEUE_TYPE, &url).await;
    assert_eq!(
        (
            held.len(),
            held.first().and_then(|item| item["resourceId"].as_str()),
            unprocessed.len(),
        ),
        (1, Some(url.as_str()), 0),
        "case {case:?}: expected the batch read to answer the client's own key, got {held:?} and \
         {unprocessed:?}"
    );
}
