// SPDX-License-Identifier: CC0-1.0

//! The WHOLE domain-controller security-log forwarding scenario, driven the way a client
//! drives it: a VPC and two subnets built in Amazon EC2, a Managed Microsoft AD created in
//! those subnets and POLLED to `Active`, the DS Data plane turned on and POLLED to
//! `Enabled`, an account-owned CloudWatch Logs group with the resource policy that lets
//! `ds.amazonaws.com` write to it, and then the three legs of the subscription — nothing
//! forwarded before it exists, every audited write forwarded while it does, nothing
//! forwarded while it is deleted, and delivery restored when it is created again.
//!
//! RULE: while a directory carries a log subscription, every write that changes one of that
//! directory's own objects is recorded by its domain controllers and forwarded into the
//! subscribed log group, where `logs:FilterLogEvents` reports it under the timestamp of the
//! write and in the stream of the controller that logged it; a write made while the
//! directory carries no subscription is forwarded neither then nor later, and nothing a
//! create replays carries a write from before it.
//!
//! This is the sibling of `ds_log_subscription_forwarding.rs`, which pins the per-write
//! records of each audited act. What this suite adds is the SCENARIO around them: the
//! placement resolved against EC2, the two clock-owed states a client has to poll through
//! before any write is served (`DirectoryDescription$Stage` and
//! `DescribeDirectoryDataAccess$DataAccessStatus`), the `startTime` window a client reads
//! the group with, the `storedBytes` it watches while the subscription is gone, and the
//! restore leg. Every wait here is a poll for the CONDITION, so the suite settles as soon
//! as the emulator does.
//!
//! The rows differ along what the rule generalises over: the form of the subscribed
//! `LogGroupName` the modelled pattern `^[-._/#A-Za-z0-9]+$` admits, the two Availability
//! Zones the directory's servers are placed in, and the Managed Microsoft AD `Edition` the
//! directory runs at — and each row runs the whole scenario, so one directory's records
//! cannot pass by being the only ones in the account.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and so the Region the subnets, the
/// directory and the log group live in.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The signing names the router picks each service by (`smithy_query(query="service_info")`).
const DS: &str = "ds";
const DS_DATA: &str = "ds-data";
const LOGS: &str = "logs";
const EC2: &str = "ec2";

/// The `X-Amz-Target` prefix each `awsJson1_1` service names an operation under: its service
/// shape's name. (`ds-data` is `restJson1` and carries none.)
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";

/// A password the modelled `Password` pattern admits.
const PASSWORD: &str = "Sim2Real#Pass99";

/// The `ShortName` the directory's NetBIOS name is created under.
const SHORT_NAME: &str = "SIM2REAL";

/// The service principal domain-controller log forwarding writes as, which is the principal
/// the log group's resource policy names.
const FORWARDING_PRINCIPAL: &str = "ds.amazonaws.com";

/// How long a poll for a clock-owed state keeps looking before it gives up.
///
/// Generous on purpose: the spans this suite polls through are a few seconds of the
/// emulator's own clock, and nothing here asserts on elapsed time, so the deadline only has
/// to outlast a loaded host.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// How long one pass of a poll waits before looking again. Short, so a row settles as soon
/// as the state it waits for does.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The four services over ONE shared engine, as `core/router/src/lib.rs` mounts them.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // These rows exercise handler behaviour, not request signing.
        router.set_request_authentication(false);
        router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
            Arc::clone(&engine),
        ));
        router.register_service(
            service_com_amazonaws_directoryservice_directoryservice_20150416::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_directoryservicedata_directoryservicedata::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is
    /// what routes the request; the signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
             /aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// One JSON call of any of the three JSON services: its status and its parsed body.
    async fn json(
        &self,
        signing_name: &str,
        uri: &str,
        target: Option<String>,
        body: Value,
    ) -> (u16, Value) {
        let mut request = http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("authorization", Self::authorization(signing_name));
        if let Some(target) = target {
            request = request.header("x-amz-target", target);
        }
        let response = self
            .router
            .dispatch(
                request
                    .body(Bytes::from(body.to_string()))
                    .expect("a well-formed request"),
            )
            .await;
        let status = response.status().as_u16();
        let raw = response.into_body();
        let parsed = if raw.is_empty() {
            json!({})
        } else {
            serde_json::from_slice(&raw).unwrap_or(Value::Null)
        };
        (status, parsed)
    }

    /// One ec2Query action that must succeed, answered with its XML body.
    async fn ec2(&self, action: &str, params: &[(&str, &str)], case: &str) -> String {
        let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
        for (name, value) in params {
            body.push('&');
            body.push_str(&form_encode(name));
            body.push('=');
            body.push_str(&form_encode(value));
        }
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{EC2}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-www-form-urlencoded")
            .header("authorization", Self::authorization(EC2))
            .body(Bytes::from(body))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(response.body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: EC2 {action} must succeed, got {status} with {text}"
        );
        text
    }

    /// One Directory Service control-plane call that must succeed.
    async fn ds(&self, operation: &str, body: Value, case: &str) -> Value {
        let (status, parsed) = self
            .json(
                DS,
                "/",
                Some(format!("{DS_TARGET_PREFIX}.{operation}")),
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ds {operation} to answer 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One Directory Service DATA-plane call that must succeed.
    async fn ds_data(&self, route: &str, directory_id: &str, body: Value, case: &str) -> Value {
        let (status, parsed) = self
            .json(
                DS_DATA,
                &format!("{route}?DirectoryId={directory_id}"),
                None,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected ds-data {route} to answer 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One CloudWatch Logs call that must succeed.
    async fn logs(&self, operation: &str, body: Value, case: &str) -> Value {
        let (status, parsed) = self
            .json(
                LOGS,
                "/",
                Some(format!("{LOGS_TARGET_PREFIX}.{operation}")),
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected logs {operation} to answer 200, got {status} with \
             {parsed}"
        );
        parsed
    }

    /// One VPC of the account carrying one subnet per entry of `zones`, created through
    /// EC2's own operations, as `(vpc id, subnet ids)`.
    async fn vpc(&self, second_octet: u8, zones: &[&str], case: &str) -> (String, Vec<String>) {
        let cidr = format!("10.{second_octet}.0.0/16");
        let vpc_id = element(
            &self.ec2("CreateVpc", &[("CidrBlock", &cidr)], case).await,
            "vpcId",
        );
        let mut subnets = Vec::new();
        for (index, zone) in zones.iter().enumerate() {
            let subnet_cidr = format!("10.{second_octet}.{}.0/24", index + 1);
            subnets.push(element(
                &self
                    .ec2(
                        "CreateSubnet",
                        &[
                            ("VpcId", vpc_id.as_str()),
                            ("CidrBlock", subnet_cidr.as_str()),
                            ("AvailabilityZone", zone),
                        ],
                        case,
                    )
                    .await,
                "subnetId",
            ));
        }
        (vpc_id, subnets)
    }

    /// The log group the account owns for its directory's security logs, with the resource
    /// policy that lets the forwarding principal open a stream in it and write to it — the
    /// setup AWS requires before a directory's logs can be forwarded anywhere.
    ///
    /// Answers the group's own ARN, as CloudWatch Logs reports it.
    async fn log_group(&self, name: &str, case: &str) -> String {
        self.logs("CreateLogGroup", json!({ "logGroupName": name }), case)
            .await;
        let arn = self
            .described_group(name, case)
            .await
            .get("arn")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected the created group to carry an ARN"))
            .to_string();
        // The policy a client attaches so the service principal may write into this one
        // group: the two actions CloudWatch Logs admits in a resource policy
        // (`logs:CreateLogStream` and `logs:PutLogEvents`), on this group's own streams.
        let document = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "DirectoryServiceLogForwarding",
                "Effect": "Allow",
                "Principal": { "Service": FORWARDING_PRINCIPAL },
                "Action": ["logs:CreateLogStream", "logs:PutLogEvents"],
                "Resource": format!("{}:*", arn.trim_end_matches(":*")),
            }],
        });
        self.logs(
            "PutResourcePolicy",
            json!({
                "policyName": format!("ds-forwarding{}", name.replace('/', "-")),
                "policyDocument": document.to_string(),
            }),
            case,
        )
        .await;
        arn
    }

    /// What `DescribeLogGroups` reports about the one group `name`.
    async fn described_group(&self, name: &str, case: &str) -> Value {
        let answer = self
            .logs(
                "DescribeLogGroups",
                json!({ "logGroupNamePrefix": name }),
                case,
            )
            .await;
        answer["logGroups"]
            .as_array()
            .and_then(|groups| {
                groups
                    .iter()
                    .find(|group| group["logGroupName"] == json!(name))
                    .cloned()
            })
            .unwrap_or_else(|| panic!("case {case:?}: expected the group {name:?}, got {answer}"))
    }

    /// The volume `DescribeLogGroups` reports the group `name` as storing.
    async fn stored_bytes(&self, name: &str, case: &str) -> i64 {
        self.described_group(name, case)
            .await
            .get("storedBytes")
            .and_then(Value::as_i64)
            .unwrap_or_default()
    }

    /// Every event the group `name` holds from `start_ms` onwards, as a client reads it:
    /// `logs:FilterLogEvents` over the window it is interested in.
    async fn forwarded(&self, name: &str, start_ms: i64, case: &str) -> Vec<Value> {
        let answer = self
            .logs(
                "FilterLogEvents",
                json!({ "logGroupName": name, "startTime": start_ms }),
                case,
            )
            .await;
        answer["events"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected events, got {answer}"))
            .clone()
    }

    /// The first event of the group `name` whose message names `account`, waited for the way
    /// a client waits for a delivery: by looking again until it is there.
    async fn delivered(&self, name: &str, start_ms: i64, account: &str, case: &str) -> Value {
        let started = Instant::now();
        loop {
            let events = self.forwarded(name, start_ms, case).await;
            if let Some(event) = events
                .iter()
                .find(|event| names_account(event, account))
                .cloned()
            {
                return event;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: expected a forwarded record naming {account:?} in {name:?}, got \
                 {events:?}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// The directory's `Stage`, as `DescribeDirectories` reports it.
    async fn stage(&self, directory_id: &str, case: &str) -> String {
        let answer = self
            .ds(
                "DescribeDirectories",
                json!({ "DirectoryIds": [directory_id] }),
                case,
            )
            .await;
        answer["DirectoryDescriptions"][0]["Stage"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a Stage, got {answer}"))
            .to_string()
    }

    /// The directory's `DataAccessStatus`, as `DescribeDirectoryDataAccess` reports it.
    async fn data_access(&self, directory_id: &str, case: &str) -> String {
        let answer = self
            .ds(
                "DescribeDirectoryDataAccess",
                json!({ "DirectoryId": directory_id }),
                case,
            )
            .await;
        answer["DataAccessStatus"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a DataAccessStatus, got {answer}"))
            .to_string()
    }

    /// Poll `read` until it answers `wanted`, which is how a client gets past a state the
    /// clock owes. Answers every value it saw on the way, so a row can assert that the
    /// transient state was observable at all.
    async fn settled_on<'a, Read, Fut>(
        &'a self,
        what: &str,
        wanted: &str,
        case: &str,
        read: Read,
    ) -> Vec<String>
    where
        Read: Fn(&'a Self) -> Fut,
        Fut: std::future::Future<Output = String>,
    {
        let started = Instant::now();
        let mut seen: Vec<String> = Vec::new();
        loop {
            let value = read(self).await;
            if seen.last() != Some(&value) {
                seen.push(value.clone());
            }
            if value == wanted {
                return seen;
            }
            assert!(
                started.elapsed() < POLL_DEADLINE,
                "case {case:?}: expected {what} to settle on {wanted:?}, saw {seen:?}"
            );
            tokio::time::sleep(POLL_PAUSE).await;
        }
    }

    /// The `DomainControllerId`s of the directory's controllers that are in service.
    async fn serving_controllers(&self, directory_id: &str, case: &str) -> Vec<String> {
        let answer = self
            .ds(
                "DescribeDomainControllers",
                json!({ "DirectoryId": directory_id }),
                case,
            )
            .await;
        answer["DomainControllers"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected DomainControllers, got {answer}"))
            .iter()
            .filter(|controller| controller["Status"] == json!("Active"))
            .filter_map(|controller| controller["DomainControllerId"].as_str())
            .map(str::to_string)
            .collect()
    }

    /// The `LogGroupName` of every log subscription of the directory.
    async fn subscriptions(&self, directory_id: &str, case: &str) -> Vec<String> {
        let answer = self
            .ds(
                "ListLogSubscriptions",
                json!({ "DirectoryId": directory_id }),
                case,
            )
            .await;
        answer["LogSubscriptions"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected LogSubscriptions, got {answer}"))
            .iter()
            .filter_map(|subscription| subscription["LogGroupName"].as_str())
            .map(str::to_string)
            .collect()
    }

    /// Create a user in the directory's realm through the DS Data plane, which is a write
    /// the directory's controllers audit.
    async fn create_user(&self, directory_id: &str, account: &str, case: &str) {
        self.ds_data(
            "/Users/CreateUser",
            directory_id,
            json!({ "SAMAccountName": account, "GivenName": "Sim", "Surname": "Real" }),
            case,
        )
        .await;
    }
}

/// Whether one forwarded record is about the account `account`: the record is the domain
/// controller's own Security-log entry, and the account it is about is the one it names.
fn names_account(event: &Value, account: &str) -> bool {
    let message = event["message"].as_str().unwrap_or_default();
    serde_json::from_str::<Value>(message)
        .ok()
        .and_then(|record| {
            record
                .get("TargetAccountName")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .is_some_and(|named| named == account)
}

/// `at` in milliseconds since the Unix epoch, the unit every CloudWatch Logs timestamp is
/// expressed in and the one a client's `startTime` window is given in.
fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

/// One row of the scenario: a directory of its own, placed in its own two zones, forwarding
/// to its own log group.
struct ScenarioCase {
    case: &'static str,
    /// The directory's fully qualified name.
    domain: &'static str,
    /// The Managed Microsoft AD edition the directory runs at.
    edition: &'static str,
    /// The `LogGroupName` the subscription names, varied along the form the modelled
    /// pattern `^[-._/#A-Za-z0-9]+$` admits.
    log_group_name: &'static str,
    /// The two Availability Zones the directory's servers are placed across.
    zones: [&'static str; 2],
    /// The second octet of the row's own VPC, so the rows do not share a network.
    second_octet: u8,
}

/// RULE: a directory forwards its domain controllers' security records into the log group
/// its subscription names, for exactly the writes made while that subscription exists.
#[tokio::test]
async fn the_log_forwarding_scenario_settles_and_delivers_only_while_subscribed() {
    let emulator = Emulator::new();

    let cases = [
        ScenarioCase {
            case: "a path-shaped group, two zones, the Standard edition",
            domain: "sim2real-standard.example.com",
            edition: "Standard",
            log_group_name: "/aws/directoryservice/sim2real-standard",
            zones: ["us-west-2a", "us-west-2b"],
            second_octet: 80,
        },
        ScenarioCase {
            case: "every character class the group pattern admits, two other zones, Enterprise",
            domain: "sim2real-enterprise.example.com",
            edition: "Enterprise",
            log_group_name: "Group-1_2.3/four#five",
            zones: ["us-west-2c", "us-west-2a"],
            second_octet: 81,
        },
    ];

    for ScenarioCase {
        case,
        domain,
        edition,
        log_group_name,
        zones,
        second_octet,
    } in &cases
    {
        let case = *case;
        // The window the client reads the group with, opened before anything is created, as
        // the check's own `startTime` is.
        let run_start_ms = epoch_millis(SystemTime::now());

        // The account's own log group and the resource policy that admits the forwarding.
        emulator.log_group(log_group_name, case).await;

        // The placement is EC2's: both subnets are ones EC2 handed out, in two different
        // Availability Zones.
        let (vpc_id, subnets) = emulator.vpc(*second_octet, zones, case).await;
        let created = emulator
            .ds(
                "CreateMicrosoftAD",
                json!({
                    "Name": domain,
                    "ShortName": SHORT_NAME,
                    "Password": PASSWORD,
                    "Edition": edition,
                    "VpcSettings": { "VpcId": vpc_id, "SubnetIds": subnets },
                }),
                case,
            )
            .await;
        let directory_id = created["DirectoryId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a DirectoryId, got {created}"))
            .to_string();
        // The modelled `DirectoryId` pattern, `^d-[0-9a-f]{10}$`
        // (`smithy_query(query="shape",shape="DirectoryId")`).
        let identifier = directory_id.strip_prefix("d-").unwrap_or_default();
        assert!(
            identifier.len() == 10 && identifier.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "case {case:?}: expected a modelled DirectoryId, got {directory_id:?}"
        );

        // The two states a client has to poll through before any write is served: the
        // directory's own stage, and the DS Data plane's status. Both are owed by the
        // clock, so both are observed transient first and then settled.
        let stages = emulator
            .settled_on("the directory's Stage", "Active", case, |emulator| {
                let directory_id = directory_id.clone();
                async move { emulator.stage(&directory_id, case).await }
            })
            .await;
        assert!(
            stages.len() > 1 && stages.first().is_some_and(|first| first != "Active"),
            "case {case:?}: expected the create to answer before the directory was in \
             service, saw {stages:?}"
        );

        emulator
            .ds(
                "EnableDirectoryDataAccess",
                json!({ "DirectoryId": &directory_id }),
                case,
            )
            .await;
        emulator
            .settled_on("the DataAccessStatus", "Enabled", case, |emulator| {
                let directory_id = directory_id.clone();
                async move { emulator.data_access(&directory_id, case).await }
            })
            .await;

        // The records come from the directory's own domain controllers, so the stream each
        // is written to is one of theirs.
        let controllers = emulator.serving_controllers(&directory_id, case).await;
        assert!(
            !controllers.is_empty(),
            "case {case:?}: expected a directory in service to report a domain controller in \
             service"
        );

        // LEG 1 — before the subscription: the write is served and forwarded nowhere.
        emulator
            .create_user(&directory_id, "preSubUser", case)
            .await;
        let before = emulator.forwarded(log_group_name, run_start_ms, case).await;
        assert!(
            before.is_empty(),
            "case {case:?}: a write made before CreateLogSubscription must not be forwarded, \
             got {before:?}"
        );

        emulator
            .ds(
                "CreateLogSubscription",
                json!({ "DirectoryId": &directory_id, "LogGroupName": log_group_name }),
                case,
            )
            .await;
        let listed = emulator.subscriptions(&directory_id, case).await;
        assert_eq!(
            listed,
            vec![log_group_name.to_string()],
            "case {case:?}: expected the directory to report exactly the subscription just \
             created, got {listed:?}"
        );

        // LEG 2 — while it exists: the write reaches the group, stamped at the instant of
        // the write and in the stream of a controller that logged it.
        let wrote_from = epoch_millis(SystemTime::now());
        emulator
            .create_user(&directory_id, "duringSubUser", case)
            .await;
        let wrote_until = epoch_millis(SystemTime::now());
        let event = emulator
            .delivered(log_group_name, run_start_ms, "duringSubUser", case)
            .await;
        let timestamp = event["timestamp"].as_i64().unwrap_or_default();
        assert!(
            (wrote_from..=wrote_until).contains(&timestamp),
            "case {case:?}: expected the record stamped at the instant of the write, within \
             [{wrote_from}, {wrote_until}], got {timestamp}"
        );
        let stream = event["logStreamName"].as_str().unwrap_or_default();
        assert!(
            controllers.iter().any(|controller| controller == stream),
            "case {case:?}: expected the record in the stream of one of {controllers:?}, got \
             {event}"
        );
        // Nothing from before the subscription was transferred along with it.
        let delivered = emulator.forwarded(log_group_name, run_start_ms, case).await;
        assert!(
            !delivered
                .iter()
                .any(|event| names_account(event, "preSubUser")),
            "case {case:?}: the write made before the subscription must never be forwarded, \
             got {delivered:?}"
        );

        // LEG 3 — while the subscription is deleted: the write is served, the group gains
        // nothing, and the volume it is metered at does not move.
        let metered_before = emulator.stored_bytes(log_group_name, case).await;
        emulator
            .ds(
                "DeleteLogSubscription",
                json!({ "DirectoryId": &directory_id }),
                case,
            )
            .await;
        assert!(
            emulator
                .subscriptions(&directory_id, case)
                .await
                .is_empty(),
            "case {case:?}: expected the deleted subscription to be reported no longer"
        );
        emulator
            .create_user(&directory_id, "withheldUser", case)
            .await;

        // LEG 4 — created again: delivery resumes for the writes that follow it, and the
        // write made while no subscription existed is still nowhere, which is what tells
        // "withheld" from "the pipeline was never live".
        emulator
            .ds(
                "CreateLogSubscription",
                json!({ "DirectoryId": &directory_id, "LogGroupName": log_group_name }),
                case,
            )
            .await;
        emulator
            .create_user(&directory_id, "afterResubUser", case)
            .await;
        emulator
            .delivered(log_group_name, run_start_ms, "afterResubUser", case)
            .await;

        let whole_window = emulator.forwarded(log_group_name, run_start_ms, case).await;
        assert!(
            !whole_window
                .iter()
                .any(|event| names_account(event, "withheldUser")),
            "case {case:?}: a write made while the subscription was deleted must be forwarded \
             neither then nor after it is created again, got {whole_window:?}"
        );
        let accounts: Vec<&str> = ["duringSubUser", "afterResubUser"].to_vec();
        for account in accounts {
            assert!(
                whole_window
                    .iter()
                    .any(|event| names_account(event, account)),
                "case {case:?}: expected the window to hold the record for {account:?}, got \
                 {whole_window:?}"
            );
        }
        assert_eq!(
            whole_window.len(),
            2,
            "case {case:?}: expected exactly the two subscribed writes to be forwarded, got \
             {whole_window:?}"
        );
        let metered_after = emulator.stored_bytes(log_group_name, case).await;
        assert_eq!(
            metered_after, metered_before,
            "case {case:?}: expected the group's metered volume to move only when its \
             metering next falls due, got {metered_after} after {metered_before}"
        );

        // A record names the account acted on, the directory it was acted on in and the
        // domain that directory owns, so a reader can tell whose log it is reading.
        let message = event["message"].as_str().unwrap_or_default();
        let record: Value = serde_json::from_str(message).unwrap_or_else(|_| {
            panic!("case {case:?}: expected one JSON object per record, got {message:?}")
        });
        for (member, expected) in [
            ("DirectoryId", json!(&directory_id)),
            ("TargetDomainName", json!(domain)),
            ("TargetAccountName", json!("duringSubUser")),
            ("Channel", json!("Security")),
        ] {
            assert_eq!(
                record[member], expected,
                "case {case:?}: expected the record's {member} to be {expected}, got {record}"
            );
        }
    }
}
