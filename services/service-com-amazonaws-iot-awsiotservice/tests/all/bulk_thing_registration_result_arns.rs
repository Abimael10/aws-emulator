// SPDX-License-Identifier: CC0-1.0

//! THE ARNs a bulk thing provisioning task's `RESULTS` report names its things by, with AWS
//! IoT, Amazon S3 and IAM mounted on ONE engine — the shape `core/router/src/lib.rs` mounts
//! them in — so the input file a task names is an object staged with `PutObject` and the role
//! it names is a role IAM actually holds.
//!
//! The RULE asserted here: every line of the input file that provisioned a thing contributes
//! exactly one row to the task's `RESULTS` report, and that row reports what provisioning the
//! line answered — the ARN of each resource it generated, under that resource's key, so the
//! thing's ARN is `response.ResourceArns.thing`. The set of things derivable from the report
//! is therefore the set of things the task registered: the ARN a row names is the very
//! `thingArn` `DescribeThing` reports for that thing, the report holds as many rows as the
//! task's own `successCount`, and a line that provisioned nothing contributes no row at all.
//!
//! The table spans what the rule generalises over rather than one recorded run: how many lines
//! the file has, which of them provision a thing (a line that is no parameter object, and one
//! whose thing the REGISTRY refuses), where the failing lines sit in the file, two lines that
//! provision the SAME thing, thing names spanning the modelled `ThingName` pattern and its
//! length bound, and the Region the task was signed for.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::SERVICE_PRINCIPAL;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The `awsQuery` form encoder and the XML tag reader are this crate's test suite's own, in
/// `topic_rule_kinesis_action`, which puts roles in IAM over the same wire.
use crate::topic_rule_kinesis_action::{form_encode, xml_text};

/// The Region the table's tasks are signed for.
const REGION: &str = "us-east-1";

/// A second Region, to pin that a reported ARN is scoped to the Region the task was signed
/// for rather than to a fixed one.
const OTHER_REGION: &str = "eu-west-1";

/// The account every ARN of this emulator is scoped to.
const ACCOUNT_ID: &str = "000000000000";

/// The bucket every input file below is staged in.
const BUCKET: &str = "iot-bulk-result-arns";

/// The `RESULTS` value of the modelled `ReportType` enum
/// (`smithy_query(query="shape",shape="ReportType")`).
const RESULTS: &str = "RESULTS";

/// The status a task holds while its run is still owed.
const IN_PROGRESS: &str = "InProgress";

/// The status a task that performed its run holds.
const COMPLETED: &str = "Completed";

/// How long a wait for a started task's completion may take before the test gives up.
///
/// Generous on purpose: the condition is polled, so a slow host takes longer rather than
/// failing, and nothing here asserts on elapsed time.
const COMPLETION_DEADLINE: Duration = Duration::from_secs(120);

/// How long the poll loop waits between two reads of a task's status.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// A string member of the body, or `None` when it is absent or not a string.
    fn text(&self, name: &str) -> Option<String> {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// An integer member of the body, or `None` when it is absent or not a number.
    fn number(&self, name: &str) -> Option<i64> {
        self.body.get(name).and_then(Value::as_i64)
    }

    /// The `resourceLinks` member, or an empty list when it is absent.
    fn links(&self) -> Vec<String> {
        self.body
            .get("resourceLinks")
            .and_then(Value::as_array)
            .map(|links| {
                links
                    .iter()
                    .map(|link| link.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The emulator: the IoT control plane, Amazon S3 and IAM on ONE shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router }
    }

    /// One request through the real router, claimed by the service its credential scope names.
    /// Signature verification is off in this harness, so the signature itself is never checked.
    async fn send(
        &self,
        method: &str,
        path: &str,
        signing_name: &str,
        region: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Reply {
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{signing_name}.{region}.amazonaws.com{path}"))
            .header("host", format!("{signing_name}.{region}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{region}/{signing_name}/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", content_type)
            .body(bytes::Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let body = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        Reply { status, body }
    }

    /// One IoT control-plane call: restJson1, the body carrying the operation's members.
    async fn iot(&self, method: &str, path: &str, region: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        self.send(method, path, "iot", region, "application/json", payload)
            .await
    }

    /// One path-style request to the emulator's Amazon S3 front door.
    async fn s3(&self, method: &str, path: &str, region: &str, body: Vec<u8>) -> u16 {
        self.send(method, path, "s3", region, "application/octet-stream", body)
            .await
            .status
    }

    /// One IAM call over the `awsQuery` protocol, answered with its XML body.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, &str)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("https://iam.{REGION}.amazonaws.com/"))
            .header("host", format!("iam.{REGION}.amazonaws.com"))
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 \
                     Credential=AKIDTESTIOT/20260101/{REGION}/iam/aws4_request, \
                     SignedHeaders=host, Signature=0000"
                ),
            )
            .header("content-type", "application/x-www-form-urlencoded")
            .body(bytes::Bytes::from(body.into_bytes()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: {action} answered {status}: {text}"
        );
        text
    }

    /// A GET of a link the service handed out, answered with its status and its body's bytes.
    ///
    /// The link is an absolute URL, so the ADDRESS it names is what is fetched: the path and
    /// query of the link go back through the very router that minted it.
    async fn fetch(&self, link: &str, region: &str) -> (u16, Vec<u8>) {
        let address = link_address(link);
        let request = http::Request::builder()
            .method("GET")
            .uri(format!("https://s3.{region}.amazonaws.com{address}"))
            .header("host", format!("s3.{region}.amazonaws.com"))
            .body(bytes::Bytes::new())
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, response.into_body().to_vec())
    }

    /// `PUT /<bucket>` — `CreateBucket` — so the staged objects have a bucket to live in.
    async fn given_bucket(&self, region: &str) {
        let status = self.s3("PUT", &format!("/{BUCKET}"), region, Vec::new()).await;
        assert_eq!(
            status, 200,
            "the input file bucket must be created first, got {status}"
        );
    }

    /// `PUT /<bucket>/<key>` — `PutObject` — staging `contents` as an input file.
    async fn given_object(&self, case: &str, key: &str, region: &str, contents: &str) {
        let status = self
            .s3(
                "PUT",
                &format!("/{BUCKET}/{key}"),
                region,
                contents.as_bytes().to_vec(),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the input file must be staged, got {status}"
        );
    }

    /// A role trusting this service, answered with the ARN IAM persisted.
    async fn given_role(&self, case: &str, name: &str) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": SERVICE_PRINCIPAL },
                "Action": "sts:AssumeRole",
            }],
        })
        .to_string();
        let created = self
            .iam(
                case,
                "CreateRole",
                &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
            )
            .await;
        xml_text(&created, "Arn")
            .unwrap_or_else(|| panic!("case {case:?}: CreateRole reported no Arn: {created}"))
    }

    /// `POST /things/{thingName}` — `CreateThing`, for a name a case registers before the task
    /// runs.
    async fn create_thing(&self, case: &str, thing_name: &str, request: &Value) {
        let reply = self
            .iot("POST", &format!("/things/{thing_name}"), REGION, Some(request))
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: {thing_name} must be registered first, got {} {}",
            reply.status, reply.body
        );
    }

    /// `POST /thing-registration-tasks` — `StartThingRegistrationTask`, answered with the
    /// `taskId` it minted.
    async fn start_task(
        &self,
        case: &str,
        key: &str,
        region: &str,
        role_arn: &str,
        template_body: &str,
    ) -> String {
        let reply = self
            .iot(
                "POST",
                "/thing-registration-tasks",
                region,
                Some(&json!({
                    "inputFileBucket": BUCKET,
                    "inputFileKey": key,
                    "roleArn": role_arn,
                    "templateBody": template_body,
                })),
            )
            .await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: the task must start, got {} {}",
            reply.status, reply.body
        );
        reply
            .text("taskId")
            .unwrap_or_else(|| panic!("case {case:?}: expected a taskId, got {}", reply.body))
    }

    /// `GET /thing-registration-tasks/{taskId}` — `DescribeThingRegistrationTask`.
    async fn describe_task(&self, task_id: &str, region: &str) -> Reply {
        self.iot(
            "GET",
            &format!("/thing-registration-tasks/{task_id}"),
            region,
            None,
        )
        .await
    }

    /// `GET /things/{thingName}` — `DescribeThing`, the registry's own reader of a thing.
    async fn describe_thing(&self, thing_name: &str, region: &str) -> Reply {
        self.iot("GET", &format!("/things/{thing_name}"), region, None)
            .await
    }

    /// Read the task until its run is no longer owed, and answer what it settled as.
    ///
    /// The CONDITION is polled — the task's own reported status — rather than a span of the
    /// clock being waited out, so a slow host takes more passes rather than failing.
    async fn awaiting_completion(&self, case: &str, task_id: &str, region: &str) -> Reply {
        let deadline = Instant::now() + COMPLETION_DEADLINE;
        loop {
            let described = self.describe_task(task_id, region).await;
            assert_eq!(
                described.status, 200,
                "case {case:?}: expected the task to read back, got {} {}",
                described.status, described.body
            );
            if described.text("status").as_deref() != Some(IN_PROGRESS) {
                return described;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the task was still {IN_PROGRESS} when the wait ran out"
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Every row of the task's `RESULTS` report, read by following each link the listing
    /// reports and splitting the object it serves into its lines.
    async fn result_rows(&self, case: &str, task_id: &str, region: &str) -> Vec<Value> {
        let listed = self
            .iot(
                "GET",
                &format!("/thing-registration-tasks/{task_id}/reports?reportType={RESULTS}"),
                region,
                None,
            )
            .await;
        assert_eq!(
            listed.status, 200,
            "case {case:?}: expected the {RESULTS} report to be listed, got {} {}",
            listed.status, listed.body
        );
        let mut rows = Vec::new();
        for link in listed.links() {
            let (status, bytes) = self.fetch(&link, region).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the link {link:?} to serve its object, got {status}"
            );
            let text = String::from_utf8(bytes)
                .unwrap_or_else(|_| panic!("case {case:?}: a report object must be text"));
            for row in text.lines().filter(|row| !row.trim().is_empty()) {
                rows.push(serde_json::from_str::<Value>(row).unwrap_or_else(|_| {
                    panic!("case {case:?}: every report row must be JSON, got {row:?}")
                }));
            }
        }
        rows
    }
}

/// The path and query of `link` — the address a caller follows it to.
fn link_address(link: &str) -> String {
    let after_scheme = link.split_once("://").map_or(link, |(_, rest)| rest);
    let from_path = after_scheme
        .find('/')
        .unwrap_or_else(|| panic!("a link must carry a path, got {link:?}"));
    after_scheme[from_path..].to_string()
}

/// `arn:aws:iot:<region>:<account>:thing/<thingName>`, the ARN of a thing of this account.
fn thing_arn(region: &str, thing_name: &str) -> String {
    format!("arn:aws:iot:{region}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// A provisioning template naming its thing from the `SerialNumber` parameter — the structure
/// AWS documents for a provisioning template.
fn template() -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": {
                    "ThingName": { "Ref": "SerialNumber" },
                    "AttributePayload": { "model": "a1" },
                },
            },
        },
    })
    .to_string()
}

/// A provisioning template whose thing name is stated by the template itself, so every line of
/// the file provisions the SAME thing.
fn fixed_name_template(thing_name: &str) -> String {
    json!({
        "Parameters": { "SerialNumber": { "Type": "String" } },
        "Resources": {
            "thing": {
                "Type": "AWS::IoT::Thing",
                "Properties": { "ThingName": thing_name },
            },
        },
    })
    .to_string()
}

/// One line of an input file provisioning the thing `serial`.
fn line(serial: &str) -> String {
    json!({ "SerialNumber": serial }).to_string()
}

/// The `response.ResourceArns.thing` of a report row: the ARN of the thing the row's line
/// registered.
fn reported_thing_arn(row: &Value) -> Option<String> {
    row.get("response")?
        .get("ResourceArns")?
        .get("thing")?
        .as_str()
        .map(str::to_string)
}

/// One row of the report table: an input file, and the things its run must report ARNs for.
struct Row {
    case: &'static str,
    /// The provisioning template the file is run under.
    template_body: String,
    /// The input file's own bytes — newline delimited JSON.
    input_file: String,
    /// The line of the file each reported row must name, with the thing that line registered,
    /// in the report's own order.
    reported: Vec<(usize, String)>,
}

/// Every line that provisioned a thing is reported with the ARN of the thing it registered,
/// and a line that provisioned none is reported by no row at all.
#[tokio::test]
async fn every_provisioned_line_is_reported_with_the_arn_of_its_thing() {
    let emulator = Emulator::new();
    emulator.given_bucket(REGION).await;
    let role_arn = emulator
        .given_role("the result table", "bulk-result-reader")
        .await;

    // A name the registry already holds with a DIFFERENT configuration: the line naming it is
    // one the registry refuses, so it provisions nothing and is reported by no result row.
    emulator
        .create_thing(
            "a line the registry refuses",
            "arn-held",
            &json!({ "attributePayload": { "attributes": { "model": "other" } } }),
        )
        .await;

    // `ThingName` is `len 1..128, pattern ^[a-zA-Z0-9:_-]+$`
    // (`smithy_query(query="shape",shape="ThingName")`): the name forms below span that
    // pattern's classes and its upper length bound.
    let longest = "a".repeat(128);
    let spanning = [
        "ARN-Upper",
        "arn_under",
        "arn:colon:3",
        "0123456789",
        longest.as_str(),
    ];

    let rows: Vec<Row> = vec![
        Row {
            case: "one line",
            template_body: template(),
            input_file: line("arn-solo"),
            reported: vec![(1, "arn-solo".to_string())],
        },
        Row {
            case: "every line provisions a thing",
            template_body: template(),
            input_file: [line("arn-1"), line("arn-2"), line("arn-3")].join("\n"),
            reported: vec![
                (1, "arn-1".to_string()),
                (2, "arn-2".to_string()),
                (3, "arn-3".to_string()),
            ],
        },
        Row {
            case: "a line that is no parameter object",
            template_body: template(),
            // The second line of the file provisions nothing, so the rows report the first and
            // the third — by the line of the FILE each one is about.
            input_file: [line("arn-first"), "not json".to_string(), line("arn-last")].join("\n"),
            reported: vec![(1, "arn-first".to_string()), (3, "arn-last".to_string())],
        },
        Row {
            case: "a line the registry refuses",
            template_body: template(),
            input_file: [line("arn-held"), line("arn-after-the-refusal")].join("\n"),
            reported: vec![(2, "arn-after-the-refusal".to_string())],
        },
        Row {
            case: "two lines provisioning the same thing",
            template_body: fixed_name_template("arn-repeated"),
            input_file: [line("ignored-a"), line("ignored-b")].join("\n"),
            reported: vec![
                (1, "arn-repeated".to_string()),
                (2, "arn-repeated".to_string()),
            ],
        },
        Row {
            case: "names spanning the modelled ThingName",
            template_body: template(),
            input_file: spanning
                .iter()
                .map(|name| line(name))
                .collect::<Vec<String>>()
                .join("\n"),
            reported: spanning
                .iter()
                .enumerate()
                .map(|(index, name)| (index + 1, (*name).to_string()))
                .collect(),
        },
        Row {
            case: "no line provisions a thing",
            template_body: template(),
            input_file: ["not json".to_string(), json!({}).to_string()].join("\n"),
            reported: vec![],
        },
    ];

    // Every task is started first and polled afterwards, so the runs' completions are owed
    // from the same point rather than one after another.
    let mut started = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let key = format!("devices/{index}.json");
        emulator
            .given_object(row.case, &key, REGION, &row.input_file)
            .await;
        let task_id = emulator
            .start_task(row.case, &key, REGION, &role_arn, &row.template_body)
            .await;
        started.push((index, task_id));
    }

    for (index, task_id) in started {
        let row = &rows[index];
        let case = row.case;
        let described = emulator.awaiting_completion(case, &task_id, REGION).await;
        assert_eq!(
            described.text("status").as_deref(),
            Some(COMPLETED),
            "case {case:?}: expected the run to complete, got {}",
            described.body
        );

        let reported = emulator.result_rows(case, &task_id, REGION).await;

        // The row of a provisioned line: the line of the file it is about, and the ARN of the
        // thing that line registered.
        let expected: Vec<(usize, String)> = row
            .reported
            .iter()
            .map(|(line_number, thing_name)| (*line_number, thing_arn(REGION, thing_name)))
            .collect();
        let actual: Vec<(usize, String)> = reported
            .iter()
            .map(|row| {
                let line_number = row
                    .get("lineNumber")
                    .and_then(Value::as_u64)
                    .unwrap_or_default() as usize;
                (
                    line_number,
                    reported_thing_arn(row).unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected the {RESULTS} report to name {expected:?}, got {reported:?}"
        );

        // The two sources of truth agree: as many rows as the task counted successes, and the
        // things the report names are the things the registry holds, read back through the
        // registry's own reader with the very ARN the row reported.
        assert_eq!(
            described.number("successCount"),
            Some(expected.len() as i64),
            "case {case:?}: expected {} reported row(s) to match successCount, got {}",
            expected.len(),
            described.body
        );
        let names: BTreeSet<String> = reported
            .iter()
            .filter_map(reported_thing_arn)
            .map(|arn| arn.rsplit('/').next().unwrap_or_default().to_string())
            .collect();
        let expected_names: BTreeSet<String> = row
            .reported
            .iter()
            .map(|(_, thing_name)| thing_name.clone())
            .collect();
        assert_eq!(
            names, expected_names,
            "case {case:?}: expected the report's thing names to be {expected_names:?}, got \
             {names:?}"
        );
        for thing_name in &expected_names {
            let read = emulator.describe_thing(thing_name, REGION).await;
            assert_eq!(
                (read.status, read.text("thingArn")),
                (200, Some(thing_arn(REGION, thing_name))),
                "case {case:?}: the ARN the report names {thing_name} by must be the one \
                 DescribeThing reports, got {} {}",
                read.status,
                read.body
            );
        }
    }
}

/// A reported ARN is scoped to the Region the task was signed for.
#[tokio::test]
async fn a_reported_arn_is_scoped_to_the_region_the_task_was_signed_for() {
    let emulator = Emulator::new();
    emulator.given_bucket(REGION).await;
    let role_arn = emulator.given_role("regions", "bulk-result-regions").await;

    // A different thing per Region: the registry is keyed by thing name, so a name already
    // registered would be the thing that is already there rather than a second one.
    let cases = [(REGION, "arn-home"), (OTHER_REGION, "arn-abroad")];
    let template_body = template();
    let mut started = Vec::new();
    for (region, thing_name) in cases {
        let key = format!("devices/{region}.json");
        emulator
            .given_object(region, &key, REGION, &line(thing_name))
            .await;
        let task_id = emulator
            .start_task(region, &key, region, &role_arn, &template_body)
            .await;
        started.push((region, thing_name, task_id));
    }

    for (region, thing_name, task_id) in started {
        emulator.awaiting_completion(region, &task_id, region).await;
        let reported = emulator.result_rows(region, &task_id, region).await;
        let arns: Vec<String> = reported
            .iter()
            .filter_map(reported_thing_arn)
            .collect();
        assert_eq!(
            arns,
            vec![thing_arn(region, thing_name)],
            "case {region:?}: expected the row to name the thing by its {region} ARN, got \
             {reported:?}"
        );
    }
}
