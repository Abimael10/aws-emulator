// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `AssociateTargetsWithJob` where the target is a THING
//! GROUP, and of the target entries it refuses.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value. What the devices
//! got is read back through `ListJobExecutionsForJob`, so the association is proved by
//! the reader that serves the job's executions rather than by the write's own answer.
//!
//! The rules under test:
//!
//! * a thing-group target is EXPANDED into the devices the group holds, so each member
//!   gets that job's `QUEUED` execution, a member already targeted gets no second one,
//!   and a group holding no devices queues nothing while still joining the job's
//!   `targets`;
//! * a group whose devices this service cannot enumerate names no devices to roll out
//!   to, so it is refused — `ResourceNotFoundException` (404) for a group the account
//!   does not hold, `InvalidRequestException` (400) for a dynamic group, whose members
//!   are the things its query selects — and the job is left exactly as it was;
//! * a `targets` entry that is no addressable IoT thing or thing-group ARN is
//!   `InvalidRequestException` (400), stating the constraint it violated;
//! * one request's `targets` list is bounded: at the ceiling the call is served, past it
//!   it is `LimitExceededException` and the job keeps the targets it had.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_iot_awsiotservice::state::MAX_JOB_TARGETS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for; the ARNs the readers report carry it.
const REGION: &str = "eu-west-1";

/// One emulator: a router with this service mounted on its own engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn dispatch(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let host = format!("iot.{REGION}.amazonaws.com");
        let payload = body.map_or_else(Bytes::new, |value| Bytes::from(value.to_string()));
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host)
            .header("content-type", "application/json")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iot/aws4_request, \
                     SignedHeaders=host, Signature=0123456789abcdef"
                ),
            )
            .body(payload)
            .expect("the job request is well formed");
        Reply::of(self.router.dispatch(request).await)
    }

    /// `CreateThing`: `POST /things/{thingName}`.
    async fn create_thing(&self, case: &str, thing_name: &str) {
        let reply = self
            .dispatch("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await;
        reply.expect_ok(case, "CreateThing");
    }

    /// `CreateThingGroup`: `POST /thing-groups/{thingGroupName}`.
    async fn create_thing_group(&self, case: &str, group_name: &str) {
        let reply = self
            .dispatch("POST", &format!("/thing-groups/{group_name}"), Some(&json!({})))
            .await;
        reply.expect_ok(case, "CreateThingGroup");
    }

    /// `UpdateIndexingConfiguration`: `POST /indexing/config`, the call that brings the
    /// fleet index a dynamic thing group's query runs against into existence — a dynamic
    /// group cannot be created for an account whose thing index is off.
    async fn enable_thing_indexing(&self, case: &str) {
        let reply = self
            .dispatch(
                "POST",
                "/indexing/config",
                Some(&json!({
                    "thingIndexingConfiguration": { "thingIndexingMode": "REGISTRY" }
                })),
            )
            .await;
        reply.expect_ok(case, "UpdateIndexingConfiguration");
    }

    /// `CreateDynamicThingGroup`: `POST /dynamic-thing-groups/{thingGroupName}`, over the
    /// request body that records the query the group's membership is derived from — so a
    /// case may record that query however it likes.
    async fn create_dynamic_thing_group(&self, case: &str, group_name: &str, body: &Value) {
        let reply = self
            .dispatch(
                "POST",
                &format!("/dynamic-thing-groups/{group_name}"),
                Some(body),
            )
            .await;
        reply.expect_ok(case, "CreateDynamicThingGroup");
    }

    /// `AddThingToThingGroup`: `PUT /thing-groups/addThingToThingGroup`.
    async fn add_thing_to_thing_group(&self, case: &str, group_name: &str, thing_name: &str) {
        let reply = self
            .dispatch(
                "PUT",
                "/thing-groups/addThingToThingGroup",
                Some(&json!({ "thingGroupName": group_name, "thingName": thing_name })),
            )
            .await;
        reply.expect_ok(case, "AddThingToThingGroup");
    }

    /// `CreateJob`: `PUT /jobs/{jobId}`, over `targets`, rolling out continuously.
    async fn create_continuous_job(&self, case: &str, job_id: &str, targets: &[String]) {
        let reply = self
            .dispatch(
                "PUT",
                &format!("/jobs/{job_id}"),
                Some(&json!({
                    "targets": targets,
                    "document": "{}",
                    "targetSelection": "CONTINUOUS",
                })),
            )
            .await;
        reply.expect_ok(case, "CreateJob");
    }

    /// `AssociateTargetsWithJob`: `POST /jobs/{jobId}/targets`.
    async fn associate(&self, job_id: &str, targets: &[String]) -> Reply {
        self.dispatch(
            "POST",
            &format!("/jobs/{job_id}/targets"),
            Some(&json!({ "targets": targets })),
        )
        .await
    }

    /// The same call carrying the operation's two OPTIONAL members: the `comment`
    /// "describing why the job was associated with the targets" and the `namespaceId`
    /// query parameter.
    async fn associate_with_optional_members(
        &self,
        job_id: &str,
        targets: &[String],
        comment: &str,
        namespace_id: &str,
    ) -> Reply {
        self.dispatch(
            "POST",
            &format!("/jobs/{job_id}/targets?namespaceId={namespace_id}"),
            Some(&json!({ "targets": targets, "comment": comment })),
        )
        .await
    }

    /// `DescribeJob`: `GET /jobs/{jobId}`.
    async fn describe_job(&self, job_id: &str) -> Reply {
        self.dispatch("GET", &format!("/jobs/{job_id}"), None).await
    }

    /// The targets `DescribeJob` reports the job is pointed at.
    async fn job_targets(&self, job_id: &str) -> Vec<String> {
        let reply = self.describe_job(job_id).await;
        reply.json()["job"]["targets"]
            .as_array()
            .unwrap_or_else(|| panic!("no job targets in {}", reply.text()))
            .iter()
            .map(|target| target.as_str().unwrap_or_default().to_string())
            .collect()
    }

    /// Which DEVICES the job queued work on, and in which status, as
    /// `ListJobExecutionsForJob` (`GET /jobs/{jobId}/things`) reports them — sorted, so
    /// the assertion is about the SET of devices and not the store's order.
    async fn job_executions(&self, job_id: &str) -> Vec<(String, String)> {
        let reply = self
            .dispatch("GET", &format!("/jobs/{job_id}/things"), None)
            .await;
        let mut executions: Vec<(String, String)> = reply.json()["executionSummaries"]
            .as_array()
            .unwrap_or_else(|| panic!("no executionSummaries in {}", reply.text()))
            .iter()
            .map(|summary| {
                let thing_arn = summary["thingArn"].as_str().unwrap_or_default();
                let thing_name = thing_arn.rsplit('/').next().unwrap_or_default().to_string();
                let status = summary["jobExecutionSummary"]["status"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                (thing_name, status)
            })
            .collect();
        executions.sort();
        executions
    }
}

/// A dispatched response, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Bytes,
}

impl Reply {
    fn of(response: http::Response<Bytes>) -> Self {
        Self {
            status: response.status().as_u16(),
            body: response.into_body(),
        }
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|e| panic!("response body is not JSON ({e}): {}", self.text()))
    }

    /// The modelled error class the refusal names.
    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    fn message(&self) -> String {
        self.json()["message"]
            .as_str()
            .unwrap_or_else(|| panic!("no message in {}", self.text()))
            .to_string()
    }

    fn expect_ok(&self, case: &str, operation: &str) {
        assert_eq!(
            self.status,
            200,
            "case {case}: expected {operation} to answer 200, got {} body {}",
            self.status,
            self.text()
        );
    }
}

/// The ARN a `targets` entry names a DEVICE by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// The ARN a `targets` entry names a THING GROUP by.
fn thing_group_arn(group_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/{group_name}")
}

/// Every device named here is queued, so every expectation is this one status.
const QUEUED: &str = "QUEUED";

/// RULE: a thing-group target is expanded into the devices the group holds, so each
/// member gets the job's queued execution and a member already targeted gets no second
/// one.
///
/// The GROUP targets name registered groups, because a group is expanded into the devices
/// it holds and one the registry does not hold names none. The DEVICE targets are
/// deliberately not registered: an execution is keyed by the thing NAME, so this surface
/// takes a device target whether or not the registry holds it, and the data plane then
/// answers that name's queue reads from the execution this call creates.
#[tokio::test]
async fn thing_group_targets_are_expanded_into_their_member_devices() {
    let emulator = Emulator::new();
    // Three groups spanning what the expansion generalises over: how many devices a
    // group holds (two, one, none).
    for thing_name in ["laser-1", "laser-2", "laser-3"] {
        emulator.create_thing("setup", thing_name).await;
    }
    emulator.create_thing_group("setup", "north").await;
    emulator
        .add_thing_to_thing_group("setup", "north", "laser-1")
        .await;
    emulator
        .add_thing_to_thing_group("setup", "north", "laser-2")
        .await;
    emulator.create_thing_group("setup", "south").await;
    emulator
        .add_thing_to_thing_group("setup", "south", "laser-3")
        .await;
    emulator.create_thing_group("setup", "empty").await;

    let gateway = thing_arn("gateway-0");
    emulator
        .create_continuous_job("setup", "rolling", std::slice::from_ref(&gateway))
        .await;

    // Each row: what one call adds, the targets the job then reports, and the devices
    // the job has executions on afterwards.
    struct Case {
        label: &'static str,
        add: Vec<String>,
        expected_targets: Vec<String>,
        expected_devices: Vec<&'static str>,
    }
    let cases = vec![
        Case {
            label: "a group holding two devices",
            add: vec![thing_group_arn("north")],
            expected_targets: vec![thing_arn("gateway-0"), thing_group_arn("north")],
            expected_devices: vec!["gateway-0", "laser-1", "laser-2"],
        },
        Case {
            label: "a group holding no devices",
            add: vec![thing_group_arn("empty")],
            expected_targets: vec![
                thing_arn("gateway-0"),
                thing_group_arn("north"),
                thing_group_arn("empty"),
            ],
            expected_devices: vec!["gateway-0", "laser-1", "laser-2"],
        },
        Case {
            label: "a group and a device in one call",
            add: vec![thing_group_arn("south"), thing_arn("gateway-1")],
            expected_targets: vec![
                thing_arn("gateway-0"),
                thing_group_arn("north"),
                thing_group_arn("empty"),
                thing_group_arn("south"),
                thing_arn("gateway-1"),
            ],
            expected_devices: vec![
                "gateway-0",
                "gateway-1",
                "laser-1",
                "laser-2",
                "laser-3",
            ],
        },
        Case {
            label: "a group the job already carries",
            add: vec![thing_group_arn("north")],
            expected_targets: vec![
                thing_arn("gateway-0"),
                thing_group_arn("north"),
                thing_group_arn("empty"),
                thing_group_arn("south"),
                thing_arn("gateway-1"),
            ],
            expected_devices: vec![
                "gateway-0",
                "gateway-1",
                "laser-1",
                "laser-2",
                "laser-3",
            ],
        },
    ];

    let job_arn = emulator.describe_job("rolling").await.json()["job"]["jobArn"]
        .as_str()
        .expect("the created job reports its ARN")
        .to_string();

    for case in &cases {
        let associated = emulator.associate("rolling", &case.add).await;
        assert_eq!(
            associated.status,
            200,
            "case {}: expected 200, got {} body {}",
            case.label,
            associated.status,
            associated.text()
        );
        // The answer names the job it associated the targets with, by the id and the ARN
        // the job's own reader reports.
        assert_eq!(
            associated.json()["jobId"],
            json!("rolling"),
            "case {}: expected the answer to name the job, got {}",
            case.label,
            associated.text()
        );
        assert_eq!(
            associated.json()["jobArn"],
            json!(job_arn),
            "case {}: expected the job's ARN {job_arn}, got {}",
            case.label,
            associated.text()
        );

        let reported = emulator.job_targets("rolling").await;
        assert_eq!(
            reported, case.expected_targets,
            "case {}: expected the targets {:?}, got {reported:?}",
            case.label, case.expected_targets
        );

        // One execution per targeted device, however many targets name it.
        let expected: Vec<(String, String)> = case
            .expected_devices
            .iter()
            .map(|thing_name| ((*thing_name).to_string(), QUEUED.to_string()))
            .collect();
        let executions = emulator.job_executions("rolling").await;
        assert_eq!(
            executions, expected,
            "case {}: expected the job's executions {expected:?}, got {executions:?}",
            case.label
        );
    }

    // The two OPTIONAL members change nothing about which devices the job rolls out to:
    // a target joins the job just the same.
    let with_optional = emulator
        .associate_with_optional_members(
            "rolling",
            &[thing_arn("gateway-2")],
            "rolling out to the spare gateway",
            "greengrass",
        )
        .await;
    assert_eq!(
        with_optional.status,
        200,
        "case a call carrying comment and namespaceId: expected 200, got {} body {}",
        with_optional.status,
        with_optional.text()
    );
    let executions = emulator.job_executions("rolling").await;
    assert!(
        executions.contains(&("gateway-2".to_string(), QUEUED.to_string())),
        "case a call carrying comment and namespaceId: expected gateway-2 to be queued, \
         got {executions:?}"
    );
}

/// RULE: a group whose devices this service cannot enumerate names no devices to roll
/// out to, so the call is refused and the job is left exactly as it was.
#[tokio::test]
async fn a_group_target_whose_devices_are_unknown_is_refused() {
    let emulator = Emulator::new();
    // A dynamic group's members are the things its query selects, so the account's thing
    // index has to be on before one can be created at all.
    emulator.enable_thing_indexing("setup").await;
    // Two dynamic groups, differing in HOW their query was recorded: one left to the
    // defaults the create fills in, one naming the index and the query version itself.
    // Either way the group's devices are the things its query selects.
    emulator
        .create_dynamic_thing_group("setup", "derived", &json!({ "queryString": "thingName:*" }))
        .await;
    emulator
        .create_dynamic_thing_group(
            "setup",
            "derived-explicit",
            // `indexName` is "Currently one index is supported: AWS_Things" and
            // `queryVersion` "Currently one query version is supported: 2017-09-30"
            // (smithy_query(query="operation",operation="CreateDynamicThingGroup")).
            &json!({
                "queryString": "attributes.site:north",
                "indexName": "AWS_Things",
                "queryVersion": "2017-09-30",
            }),
        )
        .await;

    let gateway = thing_arn("gateway-0");
    emulator
        .create_continuous_job("setup", "rolling", std::slice::from_ref(&gateway))
        .await;
    let before_targets = emulator.job_targets("rolling").await;
    let before_executions = emulator.job_executions("rolling").await;

    // Each row: the group named, and the modelled refusal it earns.
    for (label, group_name, status, error_code) in [
        (
            "a group the account does not hold",
            "absent",
            404,
            "ResourceNotFoundException",
        ),
        (
            "a dynamic group, whose devices are the things its query selects",
            "derived",
            400,
            "InvalidRequestException",
        ),
        (
            "a dynamic group whose query names the index and version explicitly",
            "derived-explicit",
            400,
            "InvalidRequestException",
        ),
    ] {
        let refused = emulator
            .associate("rolling", &[thing_group_arn(group_name)])
            .await;
        assert_eq!(
            refused.status, status,
            "case {label}: expected {status}, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            error_code,
            "case {label}: expected {error_code}, got {}",
            refused.text()
        );
        // Nothing was written: not the job's targets, and not one device's execution.
        let targets = emulator.job_targets("rolling").await;
        assert_eq!(
            targets, before_targets,
            "case {label}: expected the job's targets unchanged, got {targets:?}"
        );
        let executions = emulator.job_executions("rolling").await;
        assert_eq!(
            executions, before_executions,
            "case {label}: expected the job's executions unchanged, got {executions:?}"
        );
    }
}

/// RULE: a `targets` entry that is no addressable IoT thing or thing-group ARN is
/// refused, stating the constraint it violated.
#[tokio::test]
async fn a_target_that_is_no_addressable_iot_resource_is_refused() {
    let emulator = Emulator::new();
    let gateway = thing_arn("gateway-0");
    emulator
        .create_continuous_job("setup", "rolling", std::slice::from_ref(&gateway))
        .await;
    let before_targets = emulator.job_targets("rolling").await;
    let before_executions = emulator.job_executions("rolling").await;

    // The classes an entry can fail in. `targets` is "A list of thing group ARNs that
    // define the targets of the job"
    // (smithy_query(query="operation",operation="AssociateTargetsWithJob")), and a thing
    // or thing-group name is `ThingName` / `ThingGroupName`: len 1..128 matching
    // ^[a-zA-Z0-9:_-]+$ (smithy_query(query="shape",shape="ThingName")).
    struct Case {
        label: &'static str,
        target: String,
        /// A fragment the refusal must state about WHY the entry is no target.
        states: &'static str,
    }
    let cases = vec![
        Case {
            label: "a bare name, not an ARN at all",
            target: "laser-1".to_string(),
            states: "syntax",
        },
        Case {
            label: "an ARN of another service",
            target: format!("arn:aws:s3:::{ACCOUNT_ID}-bucket/laser-1"),
            states: "syntax",
        },
        Case {
            label: "an IoT ARN whose resource names no type",
            target: format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing"),
            states: "syntax",
        },
        Case {
            label: "an IoT resource type a job cannot be sent to",
            target: format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:billinggroup/invoiced"),
            states: "billinggroup",
        },
        Case {
            label: "a thing name outside the modelled character class",
            target: format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/laser one"),
            states: "laser one",
        },
        Case {
            label: "a thing-group name outside the modelled character class",
            target: format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thinggroup/north west"),
            states: "north west",
        },
        Case {
            label: "an empty thing name",
            target: format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/"),
            states: "characters matching",
        },
    ];

    for case in &cases {
        let refused = emulator
            .associate("rolling", std::slice::from_ref(&case.target))
            .await;
        assert_eq!(
            refused.status,
            400,
            "case {}: expected 400, got {} body {}",
            case.label,
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {}: expected InvalidRequestException, got {}",
            case.label,
            refused.text()
        );
        let message = refused.message();
        assert!(
            message.contains(case.states),
            "case {}: expected the refusal to state {:?}, got {message:?}",
            case.label,
            case.states
        );
        let targets = emulator.job_targets("rolling").await;
        assert_eq!(
            targets, before_targets,
            "case {}: expected the job's targets unchanged, got {targets:?}",
            case.label
        );
        let executions = emulator.job_executions("rolling").await;
        assert_eq!(
            executions, before_executions,
            "case {}: expected the job's executions unchanged, got {executions:?}",
            case.label
        );
    }
}

/// RULE: one request's `targets` list is bounded — at the ceiling the call is served,
/// past it the job keeps the targets it had.
#[tokio::test]
async fn one_request_may_name_at_most_the_target_ceiling() {
    let emulator = Emulator::new();
    let gateway = thing_arn("gateway-0");
    emulator
        .create_continuous_job("ceiling", "rolling", std::slice::from_ref(&gateway))
        .await;

    // One target past the ceiling in a SINGLE call, before anything joined the job: the
    // list itself is too long, whatever the job already carries.
    let past: Vec<String> = (0..=MAX_JOB_TARGETS)
        .map(|index| thing_arn(&format!("laser-{index}")))
        .collect();
    let refused = emulator.associate("rolling", &past).await;
    assert_eq!(
        refused.status,
        410,
        "case one target past the ceiling: expected the modelled 410, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "LimitExceededException",
        "case one target past the ceiling: got {}",
        refused.text()
    );
    assert!(
        refused.message().contains(&MAX_JOB_TARGETS.to_string()),
        "case one target past the ceiling: expected the refusal to name the limit, got {:?}",
        refused.message()
    );
    let targets = emulator.job_targets("rolling").await;
    assert_eq!(
        targets,
        vec![gateway.clone()],
        "case one target past the ceiling: expected the job's targets unchanged, got {targets:?}"
    );

    // Exactly at the ceiling, counting the target the job already carries: served, and
    // every device it names ends up with the job's queued execution.
    let mut at_ceiling = vec![gateway.clone()];
    at_ceiling.extend(
        (1..MAX_JOB_TARGETS).map(|index| thing_arn(&format!("laser-{index}"))),
    );
    assert_eq!(
        at_ceiling.len(),
        MAX_JOB_TARGETS,
        "the case at the ceiling must name exactly {MAX_JOB_TARGETS} targets"
    );
    let served = emulator.associate("rolling", &at_ceiling).await;
    assert_eq!(
        served.status,
        200,
        "case exactly at the ceiling: expected 200, got {} body {}",
        served.status,
        served.text()
    );
    let targets = emulator.job_targets("rolling").await;
    assert_eq!(
        targets.len(),
        MAX_JOB_TARGETS,
        "case exactly at the ceiling: expected {MAX_JOB_TARGETS} targets, got {}",
        targets.len()
    );
    let executions = emulator.job_executions("rolling").await;
    assert_eq!(
        executions.len(),
        MAX_JOB_TARGETS,
        "case exactly at the ceiling: expected one execution per targeted device, got {}",
        executions.len()
    );
    assert!(
        executions.iter().all(|(_, status)| status == QUEUED),
        "case exactly at the ceiling: expected every execution queued, got {executions:?}"
    );
}
