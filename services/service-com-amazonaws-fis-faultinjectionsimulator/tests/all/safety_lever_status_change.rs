// SPDX-License-Identifier: CC0-1.0

//! RULE: `UpdateSafetyLeverState` applies a STATUS CHANGE. The state a request carries is a
//! status together with the reason for it, and the reason is not independently writable, so
//! a request naming the status the lever is already held at is refused with
//! `ConflictException` (HTTP 409) and leaves the stored state — its reason included — as the
//! last accepted status change set it.
//!
//! A lever whose engage is still taking effect is already held engaged, so it answers a
//! further engage exactly as a settled one does: whether the clock has moved it from
//! `engaging` to `engaged` yet decides nothing.
//!
//! The rows differ along what the rule generalises over: the status the lever is held at
//! (the one it has before any update, one taking effect, one settled, one it was taken back
//! to), the status the request asks for, whether the request's reason differs from the stored
//! one, and how many updates the lever has already taken.
//!
//! The HTTP harness below is local to this suite, as each sibling suite's is.

use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_fis_faultinjectionsimulator::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;
use std::sync::Arc;

/// The id the account's own safety lever is named by: FIS gives an account one lever and
/// publishes no API that creates or enumerates one.
const LEVER_ID: &str = "default";

/// The `state.reason` the lever carries before any update has stored one.
const INITIAL_REASON: &str = "Initial default status";

/// The region every case here signs for.
const REGION: &str = "us-east-1";

/// The refusal a reason-only update earns. The entire sentence is fixed text: neither the
/// request nor the stored state is interpolated into it.
const CONFLICT_MESSAGE: &str = "Cannot update Safety Lever reason without a status change";

/// How long a case keeps looking for the transition the clock owes the lever before giving
/// up. Generous: the emulator's span is non-zero and well under this, and the case waits for
/// the CONDITION rather than for a time.
const POLL_BUDGET: Duration = Duration::from_secs(30);

/// How long to wait between two looks at the lever while polling.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// A SigV4 credential scope naming this service, for `region`.
fn signed_for(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/fis/aws4_request, \
         SignedHeaders=host, Signature=abc"
    )
}

/// One emulator: a router carrying only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// What the emulator answered: the HTTP status and the JSON body.
struct Answer {
    status: u16,
    body: Value,
}

impl Answer {
    /// The wire error code (`__type`), or the empty string on a success.
    fn error_code(&self) -> &str {
        self.body["__type"].as_str().unwrap_or_default()
    }

    /// The `message` the answer carries, or the empty string.
    fn message(&self) -> &str {
        self.body["message"].as_str().unwrap_or_default()
    }
}

async fn send(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body
        .map(|value| Bytes::from(serde_json::to_vec(&value).expect("the test body serializes")))
        .unwrap_or_default();
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "fis.us-east-1.amazonaws.com")
        .header("authorization", signed_for(REGION))
        .header("content-type", "application/json")
        .body(payload)
        .expect("the test request is well formed");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "{method} {uri} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    Answer { status, body }
}

/// `UpdateSafetyLeverState`: ask the lever for `status`, giving `reason` for it.
async fn update_lever(router: &MultiServiceRouter, status: &str, reason: &str) -> Answer {
    send(
        router,
        "PATCH",
        &format!("/safetyLevers/{LEVER_ID}/state"),
        Some(json!({ "state": { "status": status, "reason": reason } })),
    )
    .await
}

/// The lever as `GetSafetyLever` reports it: its status and its reason.
async fn read_lever(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let answer = send(router, "GET", &format!("/safetyLevers/{LEVER_ID}"), None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from GetSafetyLever, got {} with {:?}",
        answer.status, answer.body
    );
    let state = &answer.body["safetyLever"]["state"];
    (
        state["status"].as_str().unwrap_or_default().to_string(),
        state["reason"].as_str().unwrap_or_default().to_string(),
    )
}

/// Look at the lever until it reports `status`, or [`POLL_BUDGET`] runs out, and report what
/// it read last. Waits for the CONDITION, so how fast the host runs makes no difference.
async fn poll_lever_until(router: &MultiServiceRouter, case: &str, status: &str) -> (String, String) {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let read = read_lever(router, case).await;
        if read.0 == status || std::time::Instant::now() >= deadline {
            return read;
        }
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// What the lever does with one request.
#[derive(Debug, Clone, Copy)]
enum Outcome {
    /// It is a status change: accepted, reported at this status straight away, and the
    /// request's reason is what the lever stores.
    Applied(&'static str),
    /// It changes no status: refused as a reason-only update, and nothing is stored.
    Conflict,
}

/// One request a row makes.
#[derive(Debug, Clone, Copy)]
struct Step {
    /// The status to poll the lever to before making this request, for a row that needs the
    /// lever SETTLED first. `None` makes the request right away, which is how a row reaches
    /// a lever whose engage is still taking effect.
    settled_at: Option<&'static str>,
    /// The status the request asks for.
    requested: &'static str,
    /// The reason it carries.
    reason: &'static str,
    outcome: Outcome,
}

/// The sequences of updates the rule is asserted over. See the suite docs for what the rows
/// span.
fn rows() -> Vec<(&'static str, Vec<Step>)> {
    vec![
        (
            "the status an untouched lever is already held at",
            vec![Step {
                settled_at: None,
                requested: "disengaged",
                reason: "a reason of its own",
                outcome: Outcome::Conflict,
            }],
        ),
        (
            "a repeat of an engage that is still taking effect",
            vec![
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "the reason the lever was engaged for",
                    outcome: Outcome::Applied("engaging"),
                },
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "another reason, while it takes effect",
                    outcome: Outcome::Conflict,
                },
            ],
        ),
        (
            "a repeat of a settled engage with a different reason",
            vec![
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "the reason the lever was engaged for",
                    outcome: Outcome::Applied("engaging"),
                },
                Step {
                    settled_at: Some("engaged"),
                    requested: "engaged",
                    reason: "a reason nobody engaged it for",
                    outcome: Outcome::Conflict,
                },
            ],
        ),
        (
            "a repeat of a settled engage with the very same reason",
            vec![
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "one reason, twice",
                    outcome: Outcome::Applied("engaging"),
                },
                Step {
                    settled_at: Some("engaged"),
                    requested: "engaged",
                    reason: "one reason, twice",
                    outcome: Outcome::Conflict,
                },
            ],
        ),
        (
            "a status change, then a repeat of the status it reached",
            vec![
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "engaged first",
                    outcome: Outcome::Applied("engaging"),
                },
                Step {
                    settled_at: Some("engaged"),
                    requested: "disengaged",
                    reason: "disengaged next",
                    outcome: Outcome::Applied("disengaged"),
                },
                Step {
                    settled_at: None,
                    requested: "disengaged",
                    reason: "disengaged again",
                    outcome: Outcome::Conflict,
                },
            ],
        ),
        (
            "an engage of a lever taken back to disengaged",
            vec![
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "engaged first",
                    outcome: Outcome::Applied("engaging"),
                },
                Step {
                    settled_at: Some("engaged"),
                    requested: "disengaged",
                    reason: "disengaged next",
                    outcome: Outcome::Applied("disengaged"),
                },
                Step {
                    settled_at: None,
                    requested: "engaged",
                    reason: "engaged once more",
                    outcome: Outcome::Applied("engaging"),
                },
            ],
        ),
    ]
}

#[tokio::test]
async fn an_update_that_changes_no_status_is_refused_and_stores_nothing() {
    for (case, steps) in rows() {
        let router = emulator();
        // What the lever holds before any update: the state FIS seeded it with.
        let mut stored = (
            "disengaged".to_string(),
            INITIAL_REASON.to_string(),
        );

        for (index, step) in steps.iter().enumerate() {
            if let Some(status) = step.settled_at {
                let settled = poll_lever_until(&router, case, status).await;
                assert_eq!(
                    settled.0, status,
                    "case {case:?}: step {index} needs the lever settled at {status:?}, \
                     got {settled:?}"
                );
                stored.0 = settled.0.clone();
            }

            let answer = update_lever(&router, step.requested, step.reason).await;
            match step.outcome {
                Outcome::Applied(immediate) => {
                    assert_eq!(
                        answer.status, 200,
                        "case {case:?}: step {index} asks for {:?} while the lever is held at \
                         {:?}, a change, so it must be accepted; got {} with {:?}",
                        step.requested, stored.0, answer.status, answer.body
                    );
                    let reported = &answer.body["safetyLever"]["state"];
                    assert_eq!(
                        reported["status"].as_str(),
                        Some(immediate),
                        "case {case:?}: step {index} expected the lever to report {immediate:?}, \
                         got {reported:?}"
                    );
                    assert_eq!(
                        reported["reason"].as_str(),
                        Some(step.reason),
                        "case {case:?}: step {index} expected the accepted reason to be stored, \
                         got {reported:?}"
                    );
                    stored = (immediate.to_string(), step.reason.to_string());
                }
                Outcome::Conflict => {
                    assert_eq!(
                        answer.status, 409,
                        "case {case:?}: step {index} asks for {:?} while the lever is already \
                         held there, so it must be refused with HTTP 409; got {} with {:?}",
                        step.requested, answer.status, answer.body
                    );
                    assert_eq!(
                        answer.error_code(),
                        "ConflictException",
                        "case {case:?}: step {index} expected ConflictException, got {:?}",
                        answer.body
                    );
                    assert_eq!(
                        answer.message(),
                        CONFLICT_MESSAGE,
                        "case {case:?}: step {index} expected the conflict's own wording, \
                         got {:?}",
                        answer.body
                    );
                    assert!(
                        answer.body["safetyLever"].is_null(),
                        "case {case:?}: step {index} must report no lever on a refusal, \
                         got {:?}",
                        answer.body
                    );
                }
            }

            // Whatever the step answered, the lever now reports the state the rule leaves it
            // in: a refusal stores nothing, so the reason is still the one the last accepted
            // status change set.
            let (status, reason) = read_lever(&router, case).await;
            assert_eq!(
                reason, stored.1,
                "case {case:?}: after step {index} the stored reason must be the last accepted \
                 one, got {reason:?} with status {status:?}"
            );
        }
    }
}

/// The reason an engaged lever reports is the reason of the engage that engaged it, however
/// many reason-only updates were refused in between — which is what a client reading the
/// lever to learn why it is engaged depends on.
#[tokio::test]
async fn a_refused_reason_never_reaches_the_engaged_levers_reason() {
    let case = "repeated reason-only updates of an engaged lever";
    let router = emulator();
    let engaged_for = "the reason the lever was engaged for";

    let accepted = update_lever(&router, "engaged", engaged_for).await;
    assert_eq!(
        accepted.status, 200,
        "case {case:?}: expected the engage to be accepted, got {} with {:?}",
        accepted.status, accepted.body
    );
    let (status, reason) = poll_lever_until(&router, case, "engaged").await;
    assert_eq!(
        status, "engaged",
        "case {case:?}: expected the engage to have taken effect, got {status:?}"
    );
    assert_eq!(
        reason, engaged_for,
        "case {case:?}: expected the engage's own reason to be stored, got {reason:?}"
    );

    // The rows here differ along the reason the refused request carries: a different one,
    // the stored one, and one that is empty of everything but a space — none of them is a
    // status change, so none of them is stored.
    for refused_reason in ["a different reason", engaged_for, " "] {
        let refused = update_lever(&router, "engaged", refused_reason).await;
        assert_eq!(
            refused.status, 409,
            "case {case:?}: expected HTTP 409 for reason {refused_reason:?}, got {} with {:?}",
            refused.status, refused.body
        );
        let (status, reason) = read_lever(&router, case).await;
        assert_eq!(
            status, "engaged",
            "case {case:?}: a refused update leaves the status alone, got {status:?}"
        );
        assert_eq!(
            reason, engaged_for,
            "case {case:?}: a refused update leaves the reason alone, got {reason:?}"
        );
    }

    // And a genuine change still applies its own reason afterwards.
    let disengaged = update_lever(&router, "disengaged", "the lever is released").await;
    assert_eq!(
        disengaged.status, 200,
        "case {case:?}: expected the disengage to be accepted, got {} with {:?}",
        disengaged.status, disengaged.body
    );
    let (status, reason) = read_lever(&router, case).await;
    assert_eq!(
        status, "disengaged",
        "case {case:?}: expected the disengage to land at once, got {status:?}"
    );
    assert_eq!(
        reason, "the lever is released",
        "case {case:?}: expected the disengage's own reason to be stored, got {reason:?}"
    );
}
