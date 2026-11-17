// SPDX-License-Identifier: CC0-1.0

//! What a client POLLING an environment sees while a lifecycle is in flight, end to end over
//! HTTP through the router this service mounts in production.
//!
//! Two rules, both about a non-terminal status:
//!
//! 1. Every non-terminal status a lifecycle walks lasts one whole transition span
//!    (`state::TRANSITION_SPAN`), never a fraction of one — so a lifecycle of several phases
//!    takes one span per phase, and each phase is observable to a client reading the
//!    environment at the interval a client polls at (seconds). A phase the API reports and
//!    nobody can observe is a transition this service claims to make and never shows: the
//!    poller below reads at a realistic interval and has to SEE each phase of an accepted
//!    update, rather than finding the whole update already settled on its first read.
//! 2. A request that needs a settled environment is refused by the STATUS the environment is
//!    actually in: HTTP 400 `ValidationException`, stating that the operation in flight has
//!    to finish first and naming no environment — the caller addressed one, so its name
//!    tells it nothing. The rows span the non-terminal statuses an environment can be in
//!    when such a request arrives (a create still running, each phase of an update in
//!    flight, and a teardown that has started).
//!
//! The poll interval here is deliberately a CLIENT's, not a fine-grained one: the suite that
//! pins the ORDER of an update's phases reads as fast as it can
//! (`tests/all/environment_update_status_sequence.rs`), and this one pins that the phases are
//! there to be seen by a caller polling the way callers do. Every wait is a poll for the
//! condition with a generous deadline; nothing here asserts on the clock.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_mwaa_amazonmwaa::state;

/// A signed-looking request header so the router's `can_handle` resolves this service by
/// its SigV4 credential scope, the way an SDK request does. Signature verification is off
/// in this harness, so the signature value is never checked.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTEST/20260101/us-east-1/airflow/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The canonical emulator account id every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// The interval a CLIENT polls a lifecycle at: a whole second, which is what a waiter in a
/// script, an SDK or a CI step reads at. The phases a lifecycle walks have to be observable
/// to a reader this coarse — the suite that pins the ORDER of the phases reads as fast as it
/// can (`tests/all/environment_update_status_sequence.rs`), while this one pins that a
/// client's own poll sees them.
const CLIENT_POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How long any poll here waits for the condition it is waiting on before calling the
/// lifecycle broken. Generous: it bounds a condition that has to HOLD, never how fast the
/// host runs.
const POLL_DEADLINE: Duration = Duration::from_secs(120);

/// One HTTP answer, reduced to what these tests assert on.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body)
            .unwrap_or_else(|error| panic!("expected a JSON body, got {error}: {}", self.text()))
    }

    /// The modelled error code, which restJson1 carries in `__type`.
    fn error_code(&self) -> String {
        self.json()
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string()
    }

    fn message(&self) -> String {
        let body = self.json();
        body.get("message")
            .or_else(|| body.get("Message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// The emulator, assembled as the router assembles it: one shared engine behind one mounted
/// service.
struct Harness {
    router: MultiServiceRouter,
}

impl Harness {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_mwaa_amazonmwaa::EmulatedService::new(Arc::clone(&engine)),
        );
        Self { router }
    }

    async fn send(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let payload = body
            .map(|value| serde_json::to_vec(&value).expect("a serialisable body"))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "api.airflow.us-east-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        Answer {
            status: response.status().as_u16(),
            body: response.into_body().to_vec(),
        }
    }

    async fn create(&self, name: &str) -> Answer {
        self.send(
            "PUT",
            &format!("/environments/{name}"),
            Some(json!({
                "ExecutionRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/mwaa-execution"),
                "SourceBucketArn": "arn:aws:s3:::my-airflow-bucket",
                "DagS3Path": "dags",
                "EnvironmentClass": "mw1.small",
                "MaxWorkers": 1,
                "NetworkConfiguration": {
                    "SubnetIds": ["subnet-00000000000000001", "subnet-00000000000000002"],
                    "SecurityGroupIds": ["sg-00000000000000001"],
                },
            })),
        )
        .await
    }

    async fn update(&self, name: &str, patch: Value) -> Answer {
        self.send("PATCH", &format!("/environments/{name}"), Some(patch))
            .await
    }

    async fn delete(&self, name: &str) -> Answer {
        self.send("DELETE", &format!("/environments/{name}"), None)
            .await
    }

    /// The `Environment` a `GetEnvironment` reports.
    async fn environment(&self, name: &str) -> Value {
        let answer = self.send("GET", &format!("/environments/{name}"), None).await;
        assert_eq!(
            answer.status,
            200,
            "GetEnvironment({name}) must succeed, got {} {}",
            answer.status,
            answer.text()
        );
        answer
            .json()
            .get("Environment")
            .cloned()
            .unwrap_or_else(|| panic!("GetEnvironment({name}) answered without an Environment"))
    }

    async fn status_of(&self, name: &str) -> String {
        self.environment(name)
            .await
            .get("Status")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("GetEnvironment({name}) reported no Status"))
            .to_string()
    }

    /// Poll at a CLIENT's interval until the environment settles into `AVAILABLE`, answering
    /// every status that poll observed.
    async fn client_poll_until_available(&self, name: &str) -> BTreeSet<String> {
        let deadline = Instant::now() + POLL_DEADLINE;
        let mut observed: BTreeSet<String> = BTreeSet::new();
        loop {
            let status = self.status_of(name).await;
            observed.insert(status.clone());
            if status == state::STATUS_AVAILABLE {
                return observed;
            }
            assert!(
                Instant::now() < deadline,
                "{name} never reached {}, observed {observed:?}",
                state::STATUS_AVAILABLE
            );
            tokio::time::sleep(CLIENT_POLL_INTERVAL).await;
        }
    }
}

#[tokio::test]
async fn a_client_polling_at_its_own_interval_observes_every_phase_of_a_lifecycle() {
    let harness = Harness::new();
    let name = "PolledLifecycle";
    assert_eq!(
        harness.create(name).await.status,
        200,
        "the fixture create must succeed"
    );

    // The create: its one non-terminal status is there to be seen by a client polling at its
    // own interval, not only by one reading as fast as it can.
    let creating = harness.client_poll_until_available(name).await;
    assert!(
        creating.contains(state::STATUS_CREATING),
        "a poller must observe {} before the create settles, observed {creating:?}",
        state::STATUS_CREATING
    );

    // The update: both of its non-terminal statuses, each a phase of its own.
    let accepted = harness.update(name, json!({"MaxWorkers": 2})).await;
    assert_eq!(
        accepted.status,
        200,
        "the update must be accepted, got {} {}",
        accepted.status,
        accepted.text()
    );
    let updating = harness.client_poll_until_available(name).await;
    for phase in [state::STATUS_CREATING_SNAPSHOT, state::STATUS_UPDATING] {
        assert!(
            updating.contains(phase),
            "a poller must observe {phase} before the update settles, observed {updating:?}"
        );
    }

    // And the update it watched is the one that landed: the phases are not a status
    // animation over an environment nothing was written to.
    let settled = harness.environment(name).await;
    assert_eq!(
        settled.get("MaxWorkers"),
        Some(&json!(2)),
        "MaxWorkers must read back as the update stored it, got {:?}",
        settled.get("MaxWorkers")
    );
    assert_eq!(
        settled.pointer("/LastUpdate/Status"),
        Some(&json!(state::UPDATE_STATUS_SUCCESS)),
        "LastUpdate.Status must report the update landed, got {:?}",
        settled.pointer("/LastUpdate/Status")
    );
}

#[tokio::test]
async fn an_update_of_an_environment_with_an_operation_in_flight_is_refused_by_its_status() {
    let harness = Harness::new();
    let name = "StatusGated";
    assert_eq!(
        harness.create(name).await.status,
        200,
        "the fixture create must succeed"
    );

    // A create answers before the environment is ready, so this read lands in the create's
    // own non-terminal status — the first row of the table.
    let creating = harness.status_of(name).await;
    assert_eq!(
        creating,
        state::STATUS_CREATING,
        "the fixture environment must still be creating, got {creating}"
    );
    assert_refused_by_status(&harness, name, &creating).await;
    // The refused update changed nothing: the environment is still running on the
    // configuration its create stored.
    assert_eq!(
        harness.environment(name).await.get("MaxWorkers"),
        Some(&json!(1)),
        "a refused update must leave the stored configuration alone"
    );

    harness.client_poll_until_available(name).await;
    assert_eq!(
        harness.update(name, json!({"MaxWorkers": 2})).await.status,
        200,
        "an update of a settled environment must be accepted"
    );

    // Each phase of the accepted update refuses a second one, by the status that phase
    // reports. The phases are walked by the clock, so the row is taken where the poll finds
    // the environment rather than at a time this test chooses.
    let mut refused_in: BTreeSet<String> = BTreeSet::new();
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let status = harness.status_of(name).await;
        if status == state::STATUS_AVAILABLE {
            break;
        }
        assert_refused_by_status(&harness, name, &status).await;
        refused_in.insert(status);
        assert!(
            Instant::now() < deadline,
            "the update never settled, refusals observed in {refused_in:?}"
        );
        tokio::time::sleep(CLIENT_POLL_INTERVAL).await;
    }
    for phase in [state::STATUS_CREATING_SNAPSHOT, state::STATUS_UPDATING] {
        assert!(
            refused_in.contains(phase),
            "a second update must be refused in {phase} too, refused in {refused_in:?}"
        );
    }

    // And a teardown that has started is the last row: the environment is in DELETING, and
    // an update of it is refused by that status.
    assert_eq!(
        harness.delete(name).await.status,
        200,
        "the delete must be accepted"
    );
    let deleting = harness.status_of(name).await;
    assert_eq!(
        deleting,
        state::STATUS_DELETING,
        "the environment must be tearing down, got {deleting}"
    );
    assert_refused_by_status(&harness, name, &deleting).await;
}

/// An update of `name` is refused by the `status` the environment is in: the modelled
/// `ValidationException` at HTTP 400, worded from that status and naming no environment.
///
/// The sentence is written out HERE rather than read back from the service, so a change to
/// the wording this suite is about fails this suite: AWS words this class of refusal
/// "Environments with <STATUS> status must complete previous operation before initiating a
/// new operation.", whose only variable part is the status the environment is in.
async fn assert_refused_by_status(harness: &Harness, name: &str, status: &str) {
    let expected = format!(
        "Environments with {status} status must complete previous operation before \
         initiating a new operation."
    );
    let refused = harness.update(name, json!({"MaxWorkers": 5})).await;
    assert_eq!(
        refused.status, 400,
        "case {status}: an update needs a settled environment, got {} {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.error_code(),
        "ValidationException",
        "case {status}: the refusal's error code, got {}",
        refused.text()
    );
    assert_eq!(
        refused.message(),
        expected,
        "case {status}: expected {expected:?}, got {:?}",
        refused.message()
    );
    assert!(
        !refused.message().contains(name),
        "case {status}: the refusal names no environment, got {:?}",
        refused.message()
    );
}
