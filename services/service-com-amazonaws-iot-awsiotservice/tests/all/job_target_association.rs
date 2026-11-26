// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `AssociateTargetsWithJob`: which targets may join a job
//! that is already rolling out, and the ceiling the total is held under.
//!
//! Every case drives the real wire path — `MultiServiceRouter::dispatch` over a
//! restJson1 request signed for the `iot` signing name — so what is asserted is what an
//! AWS SDK client would see, not an internal function's return value.
//!
//! The rules under test, which are the criteria the operation documents:
//!
//! * "The job must have been created with the targetSelection field set to CONTINUOUS"
//!   and "The job status must currently be IN_PROGRESS" — a job of either other kind is
//!   refused `InvalidRequestException` (400);
//! * "The total number of targets associated with a job must not exceed 100" — past the
//!   ceiling the request is refused `LimitExceededException` and the job's existing
//!   target list is left as it was, not truncated;
//! * a target that joins the job gets that device's job execution, and a device already
//!   targeted gets no second one;
//! * a target naming a thing the registry does not hold is `ResourceNotFoundException`
//!   (404) and the job keeps the targets it had;
//! * a job the account does not hold is `ResourceNotFoundException` (404).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_common::iot_job_executions::JobExecutionStore;
use service_com_amazonaws_iot_awsiotservice::state::MAX_JOB_TARGETS;
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The account id every emulated ARN carries (`emulator_server::EMULATOR_ACCOUNT_ID`).
const ACCOUNT_ID: &str = "000000000000";
/// The region this suite signs for.
const REGION: &str = "ca-central-1";

/// One emulator: the shared engine plus a router with this service mounted.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { engine, router }
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

    /// `CreateThing`: `POST /things/{thingName}`. A job targets DEVICES of this account,
    /// so every targeted thing is registered before the job that names it.
    async fn create_thing(&self, thing_name: &str) {
        let reply = self
            .dispatch("POST", &format!("/things/{thing_name}"), Some(&json!({})))
            .await;
        assert_eq!(
            reply.status,
            200,
            "registering {thing_name}: expected 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// Create one job over `targets`, with the `targetSelection` given.
    async fn create_job(
        &self,
        case: &str,
        job_id: &str,
        targets: &[String],
        target_selection: Option<&str>,
    ) {
        let mut body = json!({ "targets": targets, "document": "{}" });
        if let Some(target_selection) = target_selection {
            body["targetSelection"] = json!(target_selection);
        }
        let reply = self
            .dispatch("PUT", &format!("/jobs/{job_id}"), Some(&body))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CreateJob to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    async fn associate(&self, job_id: &str, targets: &[String]) -> Reply {
        self.dispatch(
            "POST",
            &format!("/jobs/{job_id}/targets"),
            Some(&json!({ "targets": targets })),
        )
        .await
    }

    async fn describe_job(&self, job_id: &str) -> Reply {
        self.dispatch("GET", &format!("/jobs/{job_id}"), None).await
    }

    async fn cancel_job(&self, case: &str, job_id: &str) {
        let reply = self
            .dispatch("PUT", &format!("/jobs/{job_id}/cancel"), Some(&json!({})))
            .await;
        assert_eq!(
            reply.status,
            200,
            "case {case}: expected CancelJob to answer 200, got {} body {}",
            reply.status,
            reply.text()
        );
    }

    /// How many executions of `job_id` the SHARED execution store holds for
    /// `thing_name` — what targeting a device with a job creates.
    fn executions_of(&self, job_id: &str, thing_name: &str) -> usize {
        let store = self.engine.state::<JobExecutionStore>();
        let store = store.lock();
        store
            .executions_for(thing_name)
            .iter()
            .filter(|record| record.job_id == job_id)
            .count()
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

    fn error_code(&self) -> String {
        self.json()["__type"]
            .as_str()
            .unwrap_or_else(|| panic!("no __type in {}", self.text()))
            .to_string()
    }

    /// The targets a `DescribeJob` answer reports the job pointed at.
    fn job_targets(&self) -> Vec<String> {
        self.json()["job"]["targets"]
            .as_array()
            .unwrap_or_else(|| panic!("no job targets in {}", self.text()))
            .iter()
            .map(|target| target.as_str().unwrap_or_default().to_string())
            .collect()
    }
}

/// The thing ARN a target entry names a device by.
fn thing_arn(thing_name: &str) -> String {
    format!("arn:aws:iot:{REGION}:{ACCOUNT_ID}:thing/{thing_name}")
}

/// RULE: targets join a CONTINUOUS job that is rolling out, each newly named device
/// getting that job's execution, and a device already targeted getting no second one.
#[tokio::test]
async fn targets_join_a_continuous_job_and_get_its_executions() {
    // The rows span what the rule generalises over: how many targets one call adds, and
    // whether they are devices the job already carries.
    struct Case {
        label: &'static str,
        add: Vec<String>,
        expected_targets: Vec<String>,
    }
    let cases = vec![
        Case {
            label: "one device",
            add: vec![thing_arn("laser-2")],
            expected_targets: vec![thing_arn("laser-1"), thing_arn("laser-2")],
        },
        Case {
            label: "two more devices at once",
            add: vec![thing_arn("laser-3"), thing_arn("laser-4")],
            expected_targets: vec![
                thing_arn("laser-1"),
                thing_arn("laser-2"),
                thing_arn("laser-3"),
                thing_arn("laser-4"),
            ],
        },
        Case {
            label: "a device the job already carries",
            add: vec![thing_arn("laser-2")],
            expected_targets: vec![
                thing_arn("laser-1"),
                thing_arn("laser-2"),
                thing_arn("laser-3"),
                thing_arn("laser-4"),
            ],
        },
    ];

    let emulator = Emulator::new();
    for index in 1..=4 {
        emulator.create_thing(&format!("laser-{index}")).await;
    }
    emulator
        .create_job(
            "rolling",
            "rolling",
            &[thing_arn("laser-1")],
            Some("CONTINUOUS"),
        )
        .await;

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
        assert_eq!(
            associated.json()["jobId"],
            json!("rolling"),
            "case {}: the answer must name the job, got {}",
            case.label,
            associated.text()
        );
        // The job now reports the targets it is pointed at, the ones it already had
        // included and each device once.
        let reported = emulator.describe_job("rolling").await.job_targets();
        assert_eq!(
            reported, case.expected_targets,
            "case {}: expected the targets {:?}, got {reported:?}",
            case.label, case.expected_targets
        );
        // Targeting a device with a job IS creating that device's execution, and a
        // device targeted twice still has one.
        for target in &case.add {
            let thing_name = target.rsplit('/').next().unwrap_or_default();
            assert_eq!(
                emulator.executions_of("rolling", thing_name),
                1,
                "case {}: expected exactly one execution of the job for {thing_name}",
                case.label
            );
        }
    }
}

/// RULE: the total number of targets a job carries is capped, and the request past the
/// cap leaves the job's existing targets exactly as they were.
#[tokio::test]
async fn the_total_number_of_targets_is_capped() {
    let emulator = Emulator::new();
    // Every device the case names, including the ones the over-cap rows ask for: a job
    // targets devices of this account.
    for index in 0..MAX_JOB_TARGETS + 10 {
        emulator.create_thing(&format!("laser-{index}")).await;
    }
    let first = thing_arn("laser-0");
    emulator
        .create_job("cap", "capped", std::slice::from_ref(&first), Some("CONTINUOUS"))
        .await;

    // One target short of the cap joins the job, so the job sits exactly at it.
    let filling: Vec<String> = (1..MAX_JOB_TARGETS)
        .map(|index| thing_arn(&format!("laser-{index}")))
        .collect();
    let filled = emulator.associate("capped", &filling).await;
    assert_eq!(
        filled.status,
        200,
        "case filling the job to the cap: expected 200, got {} body {}",
        filled.status,
        filled.text()
    );
    let at_cap = emulator.describe_job("capped").await.job_targets();
    assert_eq!(
        at_cap.len(),
        MAX_JOB_TARGETS,
        "case filling the job to the cap: expected {MAX_JOB_TARGETS} targets, got {}",
        at_cap.len()
    );

    // The rows span the ways a request passes the cap: one target too many, and a batch
    // that would take the total far past it.
    for (label, add) in [
        (
            "one target past the cap",
            vec![thing_arn(&format!("laser-{MAX_JOB_TARGETS}"))],
        ),
        (
            "a batch that would take the total far past the cap",
            (MAX_JOB_TARGETS..MAX_JOB_TARGETS + 10)
                .map(|index| thing_arn(&format!("laser-{index}")))
                .collect(),
        ),
    ] {
        let refused = emulator.associate("capped", &add).await;
        assert_eq!(
            refused.status,
            410,
            "case {label}: expected the modelled 410, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "LimitExceededException",
            "case {label}: got {}",
            refused.text()
        );
        assert!(
            refused.json()["message"]
                .as_str()
                .is_some_and(|message| message.contains(&MAX_JOB_TARGETS.to_string())),
            "case {label}: the refusal must name the limit, got {}",
            refused.text()
        );
        // The job is left pointed at exactly what it carried, not truncated.
        let after = emulator.describe_job("capped").await.job_targets();
        assert_eq!(
            after, at_cap,
            "case {label}: expected the job's targets unchanged, got {} of them",
            after.len()
        );
        // And nothing was queued for the devices the refused request named.
        for target in &add {
            let thing_name = target.rsplit('/').next().unwrap_or_default();
            assert_eq!(
                emulator.executions_of("capped", thing_name),
                0,
                "case {label}: expected no execution for the refused target {thing_name}"
            );
        }
    }
}

/// RULE: only a CONTINUOUS job that is still rolling out takes further targets, and a
/// job the account does not hold is a 404.
#[tokio::test]
async fn a_job_that_takes_no_further_targets_is_refused() {
    let emulator = Emulator::new();
    for index in 1..=2 {
        emulator.create_thing(&format!("laser-{index}")).await;
    }
    let target = thing_arn("laser-1");
    let joining = vec![thing_arn("laser-2")];

    // A SNAPSHOT job is complete once the things it was created for are done, so nothing
    // joins it later; a job whose creator named no selection is such a snapshot job, and
    // is refused for the same reason.
    emulator
        .create_job(
            "snapshot",
            "snapshot-job",
            std::slice::from_ref(&target),
            Some("SNAPSHOT"),
        )
        .await;
    emulator
        .create_job(
            "unselected",
            "unselected-job",
            std::slice::from_ref(&target),
            None,
        )
        .await;
    // A CONTINUOUS job that has been cancelled has left the status that admits targets.
    emulator
        .create_job(
            "cancelled",
            "cancelled-job",
            std::slice::from_ref(&target),
            Some("CONTINUOUS"),
        )
        .await;
    emulator.cancel_job("cancelled", "cancelled-job").await;

    for (label, job_id) in [
        ("a snapshot job", "snapshot-job"),
        ("a job whose creator named no target selection", "unselected-job"),
        ("a cancelled job", "cancelled-job"),
    ] {
        let refused = emulator.associate(job_id, &joining).await;
        assert_eq!(
            refused.status,
            400,
            "case {label}: expected 400, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "InvalidRequestException",
            "case {label}: got {}",
            refused.text()
        );
        let reported = emulator.describe_job(job_id).await.job_targets();
        assert_eq!(
            reported,
            vec![target.clone()],
            "case {label}: expected the job's targets unchanged, got {reported:?}"
        );
    }

    let missing = emulator.associate("no-such-job", &joining).await;
    assert_eq!(
        missing.status,
        404,
        "case a job the account does not hold: expected 404, got {} body {}",
        missing.status,
        missing.text()
    );
    assert_eq!(
        missing.error_code(),
        "ResourceNotFoundException",
        "case a job the account does not hold: got {}",
        missing.text()
    );
}

/// RULE: a thing target names a DEVICE OF THIS ACCOUNT, so an entry naming a `thingName`
/// the registry does not hold is the modelled `ResourceNotFoundException` (404) stating
/// the jobs plane's sentence for it, and the job keeps the targets it had. A list past
/// the ceiling is still the `LimitExceededException`, since the size of the request is
/// decided before any name is looked up.
#[tokio::test]
async fn a_target_naming_a_device_the_registry_does_not_hold_is_refused() {
    let emulator = Emulator::new();
    emulator.create_thing("laser-1").await;
    let target = thing_arn("laser-1");
    emulator
        .create_job(
            "registry",
            "rolling",
            std::slice::from_ref(&target),
            Some("CONTINUOUS"),
        )
        .await;

    // The rows span what the refused entry is: one unregistered device, a registered
    // device beside an unregistered one, and the device the job already carries beside
    // an unregistered one.
    let rows: Vec<(&str, Vec<String>, &str)> = vec![
        (
            "one device the registry does not hold",
            vec![thing_arn("laser-absent")],
            "laser-absent",
        ),
        (
            "a device the registry does not hold beside a registered one",
            vec![thing_arn("laser-1"), thing_arn("laser-second-absent")],
            "laser-second-absent",
        ),
        (
            "two devices the registry does not hold",
            vec![thing_arn("laser-absent-a"), thing_arn("laser-absent-b")],
            // The walk refuses the FIRST entry that names no device.
            "laser-absent-a",
        ),
    ];

    for (label, add, named) in &rows {
        let refused = emulator.associate("rolling", add).await;
        assert_eq!(
            refused.status,
            404,
            "case {label}: expected 404, got {} body {}",
            refused.status,
            refused.text()
        );
        assert_eq!(
            refused.error_code(),
            "ResourceNotFoundException",
            "case {label}: got {}",
            refused.text()
        );
        assert_eq!(
            refused.json()["message"],
            json!(format!("Thing {named} cannot be found.")),
            "case {label}: expected the refusal to name the thing, got {}",
            refused.text()
        );
        // Nothing was written: not the job's targets, and not one device's execution.
        let reported = emulator.describe_job("rolling").await.job_targets();
        assert_eq!(
            reported,
            vec![target.clone()],
            "case {label}: expected the job's targets unchanged, got {reported:?}"
        );
        for entry in add {
            let thing_name = entry.rsplit('/').next().unwrap_or_default();
            let expected = usize::from(thing_name == "laser-1");
            assert_eq!(
                emulator.executions_of("rolling", thing_name),
                expected,
                "case {label}: a refused association must queue nothing new for \
                 {thing_name}"
            );
        }
    }

    // One target past the ceiling, every one of them a device the registry does not
    // hold: the size of the list decides, so the answer is the 410 and not the 404.
    let past_the_ceiling: Vec<String> = (0..=MAX_JOB_TARGETS)
        .map(|index| thing_arn(&format!("laser-absent-{index}")))
        .collect();
    let refused = emulator.associate("rolling", &past_the_ceiling).await;
    assert_eq!(
        refused.status,
        410,
        "case a list past the ceiling of devices the registry does not hold: expected \
         the modelled 410, got {} body {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "LimitExceededException",
        "case a list past the ceiling of devices the registry does not hold: got {}",
        refused.text()
    );
}
