// SPDX-License-Identifier: CC0-1.0

//! Asking the account's safety lever to hold nothing when it is already holding nothing.
//!
//! RULE: an engage is a HOLD on the account — it keeps every experiment of the account from
//! running — and the reason stored beside it records why, so FIS refuses an engage of a lever
//! that is already holding (`crate::safety_lever_status_change`). **A disengage holds nothing
//! back and so conflicts with nobody: one asked of a lever that is already disengaged is
//! ACCEPTED, changes nothing, and is answered with the lever exactly as it already stood —
//! the reason the request carried is not stored, because no status change took it up.**
//!
//! `UpdateSafetyLeverState` carries the target status and the reason for it together
//! (`smithy_query(query="shape",shape="UpdateSafetyLeverStateInput")`: `status` and `reason`,
//! both required), which is why the reason only ever reaches the lever as part of a status
//! change.
//!
//! The rows differ along what the rule generalises over: the HISTORY that brought the lever to
//! disengaged (untouched, released after the engage had taken effect, released while it was
//! still taking effect), HOW MANY times the disengage is repeated, and the reason each repeat
//! carries (one of its own, the very reason the lever stores, one that is only a space, a long
//! one). The control at the end is the other direction — an engage of a lever that is already
//! holding — so what this suite asserts is attributable to the direction of the request rather
//! than to the repeat.
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

/// The status a lever that is holding nothing reports.
const DISENGAGED: &str = "disengaged";

/// The refusal an engage of an already holding lever earns. The entire sentence is fixed
/// text: neither the request nor the stored state is interpolated into it.
const CONFLICT_MESSAGE: &str = "Cannot update Safety Lever reason without a status change";

/// The region every case here signs for.
const REGION: &str = "us-east-1";

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

    /// The `safetyLever` this answer reports.
    fn lever(&self) -> &Value {
        &self.body["safetyLever"]
    }

    /// The reported `(status, reason)` pair.
    fn state(&self) -> (String, String) {
        let state = &self.lever()["state"];
        (
            state["status"].as_str().unwrap_or_default().to_string(),
            state["reason"].as_str().unwrap_or_default().to_string(),
        )
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

/// `GetSafetyLever`, required to answer.
async fn read_lever(router: &MultiServiceRouter, case: &str) -> Answer {
    let answer = send(router, "GET", &format!("/safetyLevers/{LEVER_ID}"), None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected HTTP 200 from GetSafetyLever, got {} with {:?}",
        answer.status, answer.body
    );
    answer
}

/// Look at the lever until it reports `status`, or [`POLL_BUDGET`] runs out. Waits for the
/// CONDITION, so how fast the host runs makes no difference.
async fn poll_lever_until(router: &MultiServiceRouter, case: &str, status: &str) {
    let deadline = std::time::Instant::now() + POLL_BUDGET;
    loop {
        let read = read_lever(router, case).await;
        if read.state().0 == status {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected the lever to reach {status:?}, got {:?}",
            read.state()
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// One update that brings the lever to the state a row asserts the rule on.
struct Prior {
    /// The status it asks for.
    requested: &'static str,
    /// The reason it carries, which it stores because it is a status change.
    reason: &'static str,
    /// Whether to wait for the lever to settle at `settles_at` before going on.
    settles_at: Option<&'static str>,
}

/// One row: the history, the reason the lever ends up holding, and the repeats to make.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The updates that bring the lever to disengaged, each a status change.
    prior: Vec<Prior>,
    /// The reason the lever stores once `prior` has been applied.
    standing_reason: &'static str,
    /// The reason each repeated disengage carries, in order.
    repeats: Vec<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the untouched lever, which has never held anything",
            prior: Vec::new(),
            standing_reason: INITIAL_REASON,
            repeats: vec!["a reason of its own"],
        },
        Row {
            case: "the untouched lever, asked five times over",
            prior: Vec::new(),
            standing_reason: INITIAL_REASON,
            repeats: vec!["first", "second", "third", "fourth", "fifth"],
        },
        Row {
            case: "the untouched lever, asked under the very reason it stores",
            prior: Vec::new(),
            standing_reason: INITIAL_REASON,
            repeats: vec![INITIAL_REASON, INITIAL_REASON],
        },
        Row {
            case: "the untouched lever, asked under a reason of one space",
            prior: Vec::new(),
            standing_reason: INITIAL_REASON,
            repeats: vec![" "],
        },
        Row {
            case: "a lever released after its engage had taken effect",
            prior: vec![
                Prior {
                    requested: "engaged",
                    reason: "held for the drill",
                    settles_at: Some("engaged"),
                },
                Prior {
                    requested: DISENGAGED,
                    reason: "released after the drill",
                    settles_at: None,
                },
            ],
            standing_reason: "released after the drill",
            repeats: vec!["released again", "and once more"],
        },
        Row {
            case: "a lever released while its engage was still taking effect",
            prior: vec![
                Prior {
                    requested: "engaged",
                    reason: "held for a moment",
                    settles_at: None,
                },
                Prior {
                    requested: DISENGAGED,
                    reason: "released while it was taking effect",
                    settles_at: None,
                },
            ],
            standing_reason: "released while it was taking effect",
            repeats: vec![
                "a reason long enough to carry a whole sentence about why the lever is not \
                 holding anything back right now",
            ],
        },
    ]
}

#[tokio::test]
async fn a_disengage_of_a_lever_that_holds_nothing_changes_nothing() {
    for Row {
        case,
        prior,
        standing_reason,
        repeats,
    } in rows()
    {
        let router = emulator();

        for (index, step) in prior.iter().enumerate() {
            let applied = update_lever(&router, step.requested, step.reason).await;
            assert_eq!(
                applied.status, 200,
                "case {case:?}: prior step {index} asking for {:?} is a status change and must \
                 be accepted, got {} with {:?}",
                step.requested, applied.status, applied.body
            );
            if let Some(status) = step.settles_at {
                poll_lever_until(&router, case, status).await;
            }
        }

        // What the lever stands at before the repeats: the state the rule says every one of
        // them answers with.
        let standing = read_lever(&router, case).await;
        assert_eq!(
            standing.state(),
            (DISENGAGED.to_string(), standing_reason.to_string()),
            "case {case:?}: expected the lever to stand disengaged under {standing_reason:?}, \
             got {:?}",
            standing.body
        );

        for (index, reason) in repeats.iter().enumerate() {
            let answered = update_lever(&router, DISENGAGED, reason).await;
            assert_eq!(
                answered.status, 200,
                "case {case:?}: repeat {index} asks a lever that holds nothing to hold \
                 nothing, so it must be accepted; got {} with {:?}",
                answered.status, answered.body
            );
            assert_eq!(
                answered.lever(),
                standing.lever(),
                "case {case:?}: repeat {index} changes nothing, so it must report the lever as \
                 it already stood ({:?}), got {:?}",
                standing.lever(),
                answered.lever()
            );

            // Round trip: the read reports the same lever, so the reason the repeat carried
            // reached no stored state.
            let read = read_lever(&router, case).await;
            assert_eq!(
                read.lever(),
                standing.lever(),
                "case {case:?}: after repeat {index} carrying {reason:?} the lever must still \
                 be the one it was ({:?}), got {:?}",
                standing.lever(),
                read.lever()
            );
        }
    }
}

#[tokio::test]
async fn an_engage_of_a_lever_that_is_already_holding_is_still_refused() {
    let case = "the other direction: a repeat of an engage";
    let router = emulator();

    let engaged = update_lever(&router, "engaged", "held for the drill").await;
    assert_eq!(
        engaged.status, 200,
        "case {case:?}: expected the engage to be accepted, got {} with {:?}",
        engaged.status, engaged.body
    );
    poll_lever_until(&router, case, "engaged").await;
    let standing = read_lever(&router, case).await;

    let refused = update_lever(&router, "engaged", "held for something else").await;
    assert_eq!(
        (refused.status, refused.error_code(), refused.message()),
        (409, "ConflictException", CONFLICT_MESSAGE),
        "case {case:?}: expected the engage of a holding lever to be refused, got {} with {:?}",
        refused.status,
        refused.body
    );
    let read = read_lever(&router, case).await;
    assert_eq!(
        read.lever(),
        standing.lever(),
        "case {case:?}: a refused engage leaves the lever alone ({:?}), got {:?}",
        standing.lever(),
        read.lever()
    );

    // And the disengage that genuinely releases it still applies its own reason.
    let released = update_lever(&router, DISENGAGED, "the lever is released").await;
    assert_eq!(
        released.state(),
        (DISENGAGED.to_string(), "the lever is released".to_string()),
        "case {case:?}: expected the release to store its own reason, got {:?}",
        released.body
    );
}
