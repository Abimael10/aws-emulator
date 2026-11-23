// SPDX-License-Identifier: CC0-1.0

//! What a DEPLOYED stage's own `methodSettings` throttle does to the requests it serves.
//!
//! The rule under test, one level above the recorded case (one wildcard entry at
//! rate 1/burst 1, one `MOCK` method, repeated concurrent bursts):
//!
//! > The `throttlingRateLimit` and `throttlingBurstLimit` of the `methodSettings` entry in
//! > force for a method — the entry keyed by that method where the stage carries one, else
//! > the stage-wide `*/*` entry, which is the DEFAULT for every method of the stage — cap
//! > the rate at which the stage admits requests for that method, whoever is calling: the
//! > burst is how many it admits at once and the rate how many per second it refills at, so
//! > a request arriving when the method's bucket is empty is SHED with the stage's
//! > `THROTTLED` gateway response, `429` `{"message":"Too Many Requests"}`, and the
//! > integration is not performed for it. Either limit set to the one negative value a
//! > method setting takes (`-1`, which asks for no throttle), and a method no entry is in
//! > force for at all, admit every request.
//!
//! Every case drives the real wire path — the control plane over `restJson1` through the
//! multi-service router, the invokes against the api's own `execute-api` host — so what is
//! asserted is what a client sees. The emulator, the `MOCK` method stand-up, the deployment
//! and the invoke reader are this crate's existing harness
//! ([`crate::execute_api_data_plane`], [`crate::rest_api_lifecycle`]).
//!
//! Nothing here waits on the clock: a bucket starts FULL, so the first `burst` requests of a
//! throttled method are admitted whatever the host's speed, and the requests past them are
//! sent in one go — refilling the single token each extra request would need takes a whole
//! second at the rate every throttled row configures, so a shed request is a fact about the
//! bucket and not about how fast this machine runs.
//!
//! The rows differ along what the rule generalises over: which `methodSettings` key the
//! limits ride on (the stage-wide wildcard, the served method's own, another method's), the
//! burst (one, several), whether the rate or the burst carries the unthrottled sentinel, and
//! a stage with no method settings at all.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{deploy, invoke, json_template, mock_method, mock_resource};
use crate::rest_api_lifecycle::{create_api, Answer, Gateway};

/// The stage every row deploys to.
const STAGE: &str = "prod";

/// The resource every row's methods are put on, and the resource path it makes: `/items`.
const RESOURCE_PART: &str = "items";

/// That resource path as a `methodSettings` pointer writes it: "any slash ("/") character
/// appearing in path names must be escaped with "~1"" (`PatchOperation$path`).
const ESCAPED_RESOURCE: &str = "~1items";

/// What the `MOCK` method answers a request it serves.
const SERVED_BODY: &str = r#"{"at":"items"}"#;

/// The answer a shed request earns: the status and the sentence the stage's `THROTTLED`
/// gateway response carries.
const THROTTLED: (u16, &str) = (429, "Too Many Requests");

/// How many requests each row sends PAST the burst its limits admit.
///
/// One token refills per second at every rate a throttled row configures, so admitting all
/// of these would need this many seconds of refill — far more than sending them in process
/// can take, which is what makes "at least one was shed" a fact about the bucket rather than
/// about the host's speed.
const PAST_THE_BURST: usize = 24;

/// One `replace` of `path` with `value`.
fn replace(path: &str, value: &str) -> Value {
    json!({ "op": "replace", "path": path, "value": value })
}

/// `UpdateStage` for [`STAGE`] of `api` with `operations`, asserting the modelled 200.
async fn update_stage(gateway: &Gateway, api: &str, case: &str, operations: Vec<Value>) -> Answer {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({ "patchOperations": operations })),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: expected 200 from UpdateStage, got {} with {:?}",
        answer.status, answer.body
    );
    answer
}

/// An api carrying one resource with a `MOCK` `GET` and a `MOCK` `POST`, deployed to
/// [`STAGE`], with `settings` patched onto the stage. Answers the api id.
///
/// Two methods on one resource, because WHICH method a throttle is in force for is half the
/// rule: a row can throttle one and show the other admitted.
async fn deployed_api(gateway: &Gateway, case: &str, settings: &[(&str, &str)]) -> String {
    let (api, root) = create_api(gateway, case).await;
    let resource = mock_resource(
        gateway,
        &api,
        &root,
        RESOURCE_PART,
        "GET",
        "200",
        json_template(SERVED_BODY),
    )
    .await;
    mock_method(
        gateway,
        &api,
        &resource,
        "POST",
        "200",
        json_template(SERVED_BODY),
    )
    .await;
    deploy(gateway, &api, STAGE).await;
    if !settings.is_empty() {
        let operations = settings
            .iter()
            .map(|(path, value)| replace(path, value))
            .collect();
        update_stage(gateway, &api, case, operations).await;
    }
    api
}

/// One answered invoke, as a row asserts on it: the status, the body verbatim and the
/// `message` member a gateway response carries.
type Answered = (u16, String, Option<String>);

/// Send `count` requests of `verb` to the deployed resource, answering each one in order.
async fn statuses(gateway: &Gateway, api: &str, verb: &str, count: usize) -> Vec<Answered> {
    let mut answered = Vec::with_capacity(count);
    for _ in 0..count {
        let invoked = invoke(gateway, api, verb, &format!("/{STAGE}/{RESOURCE_PART}"), &[]).await;
        answered.push((invoked.status, invoked.body.clone(), invoked.message()));
    }
    answered
}

/// One row: the `methodSettings` patches the stage carries, and how many requests the method
/// under test admits at once — `None` for a row no throttle is in force for.
struct Row {
    case: &'static str,
    settings: &'static [(&'static str, &'static str)],
    burst: Option<usize>,
}

/// The rows: every way the rule's "the entry in force, else none" resolves, and both
/// spellings of "no throttle at all".
const ROWS: &[Row] = &[
    Row {
        case: "the stage-wide entry at rate 1 and burst 1",
        settings: &[
            ("/*/*/throttling/rateLimit", "1"),
            ("/*/*/throttling/burstLimit", "1"),
        ],
        burst: Some(1),
    },
    Row {
        case: "the stage-wide entry at rate 1 and burst 3",
        settings: &[
            ("/*/*/throttling/rateLimit", "1"),
            ("/*/*/throttling/burstLimit", "3"),
        ],
        burst: Some(3),
    },
    Row {
        case: "the served method's own entry at rate 1 and burst 2",
        settings: &[
            ("/~1items/GET/throttling/rateLimit", "1"),
            ("/~1items/GET/throttling/burstLimit", "2"),
        ],
        burst: Some(2),
    },
    Row {
        case: "the served method's own entry over a laxer stage-wide one",
        settings: &[
            ("/*/*/throttling/rateLimit", "10000"),
            ("/*/*/throttling/burstLimit", "5000"),
            ("/~1items/GET/throttling/rateLimit", "1"),
            ("/~1items/GET/throttling/burstLimit", "1"),
        ],
        burst: Some(1),
    },
    Row {
        case: "a stage carrying no method settings at all",
        settings: &[],
        burst: None,
    },
    Row {
        case: "the unthrottled sentinel on both limits",
        settings: &[
            ("/*/*/throttling/rateLimit", "-1"),
            ("/*/*/throttling/burstLimit", "-1"),
        ],
        burst: None,
    },
    Row {
        case: "the unthrottled sentinel on the rate alone",
        settings: &[
            ("/*/*/throttling/rateLimit", "-1"),
            ("/*/*/throttling/burstLimit", "1"),
        ],
        burst: None,
    },
    Row {
        case: "the unthrottled sentinel on the burst alone",
        settings: &[
            ("/*/*/throttling/rateLimit", "1"),
            ("/*/*/throttling/burstLimit", "-1"),
        ],
        burst: None,
    },
    Row {
        case: "another method's entry alone",
        settings: &[
            ("/~1items/POST/throttling/rateLimit", "1"),
            ("/~1items/POST/throttling/burstLimit", "1"),
        ],
        burst: None,
    },
];

#[tokio::test]
async fn a_stages_method_settings_throttle_sheds_the_requests_past_its_burst() {
    for row in ROWS {
        // One emulator per row: a bucket belongs to its api's stage, so a row must not
        // inherit the requests another row's method already spent.
        let gateway = Gateway::new();
        let api = deployed_api(&gateway, row.case, row.settings).await;

        let Some(burst) = row.burst else {
            // No throttle is in force for this method: every request is served, however
            // many arrive.
            let answered = statuses(&gateway, &api, "GET", PAST_THE_BURST + 1).await;
            let shed: Vec<&Answered> = answered
                .iter()
                .filter(|(status, _, _)| *status != 200)
                .collect();
            assert!(
                shed.is_empty(),
                "case {:?}: no throttle is in force, so every request must be served, got \
                 {shed:?} among {answered:?}",
                row.case
            );
            continue;
        };

        // The bucket starts FULL, so exactly the burst is admitted at once.
        let admitted = statuses(&gateway, &api, "GET", burst).await;
        for (index, (status, body, _)) in admitted.iter().enumerate() {
            assert_eq!(
                (*status, body.as_str()),
                (200, SERVED_BODY),
                "case {:?}: request {index} is within the burst of {burst}, so it must be \
                 served, got {status} {body:?}",
                row.case
            );
        }

        // And past it the stage sheds, with the `THROTTLED` gateway response.
        let past = statuses(&gateway, &api, "GET", PAST_THE_BURST).await;
        let shed: Vec<&Answered> = past
            .iter()
            .filter(|(status, _, _)| *status != 200)
            .collect();
        assert!(
            !shed.is_empty(),
            "case {:?}: a burst of {burst} at rate 1 cannot admit {PAST_THE_BURST} further \
             requests, got {past:?}",
            row.case
        );
        for (status, body, message) in shed {
            assert_eq!(
                (*status, message.as_deref()),
                (THROTTLED.0, Some(THROTTLED.1)),
                "case {:?}: a shed request is answered through the stage's THROTTLED gateway \
                 response, got {status} {body:?}",
                row.case
            );
        }
    }
}

#[tokio::test]
async fn the_stage_wide_entry_meters_each_method_on_its_own_bucket() {
    // The stage-wide entry is the DEFAULT for every method of the stage, so the two methods
    // of one resource are metered apart: spending the GET's whole burst leaves the POST its
    // own, and each is shed only once ITS bucket is empty.
    let case = "the stage-wide default over two methods of one resource";
    let gateway = Gateway::new();
    let api = deployed_api(
        &gateway,
        case,
        &[
            ("/*/*/throttling/rateLimit", "1"),
            ("/*/*/throttling/burstLimit", "1"),
        ],
    )
    .await;

    for verb in ["GET", "POST"] {
        let admitted = statuses(&gateway, &api, verb, 1).await;
        assert_eq!(
            admitted
                .first()
                .map(|(status, body, _)| (*status, body.as_str())),
            Some((200, SERVED_BODY)),
            "case {case:?}: the {verb} method's own burst must admit its first request, got \
             {admitted:?}"
        );
        let past = statuses(&gateway, &api, verb, PAST_THE_BURST).await;
        let shed: Vec<&Answered> = past
            .iter()
            .filter(|(status, _, _)| *status != 200)
            .collect();
        assert!(
            !shed.is_empty(),
            "case {case:?}: the {verb} method's burst of 1 at rate 1 cannot admit \
             {PAST_THE_BURST} further requests, got {past:?}"
        );
        for (status, body, message) in shed {
            assert_eq!(
                (*status, message.as_deref()),
                (THROTTLED.0, Some(THROTTLED.1)),
                "case {case:?}: a shed {verb} request is answered through the stage's \
                 THROTTLED gateway response, got {status} {body:?}"
            );
        }
    }
}

#[tokio::test]
async fn the_limits_in_force_are_read_for_every_request() {
    // The entry in force is read per request, so moving its limits to the unthrottled
    // sentinel puts the method back to admitting everything — no bucket frozen at the limits
    // a patch once wrote — and moving them back sheds again.
    let case = "the throttling entry moved to the sentinel and back";
    let gateway = Gateway::new();
    let api = deployed_api(
        &gateway,
        case,
        &[
            ("/*/*/throttling/rateLimit", "1"),
            ("/*/*/throttling/burstLimit", "1"),
        ],
    )
    .await;

    let past = statuses(&gateway, &api, "GET", PAST_THE_BURST + 1).await;
    assert!(
        past.iter().any(|(status, _, _)| *status == THROTTLED.0),
        "case {case:?}: expected the configured throttle to shed a request, got {past:?}"
    );

    update_stage(
        &gateway,
        &api,
        case,
        vec![replace("/*/*/throttling/rateLimit", "-1")],
    )
    .await;
    let after = statuses(&gateway, &api, "GET", PAST_THE_BURST + 1).await;
    let shed: Vec<&Answered> = after
        .iter()
        .filter(|(status, _, _)| *status != 200)
        .collect();
    assert!(
        shed.is_empty(),
        "case {case:?}: with the sentinel in force every request must be served, got {shed:?} \
         among {after:?}"
    );

    // And the per-method entry, addressed by the escaped spelling of its resource half,
    // throttles the method the wildcard entry defaulted for.
    update_stage(
        &gateway,
        &api,
        case,
        vec![
            replace(
                &format!("/{ESCAPED_RESOURCE}/GET/throttling/rateLimit"),
                "1",
            ),
            replace(
                &format!("/{ESCAPED_RESOURCE}/GET/throttling/burstLimit"),
                "1",
            ),
        ],
    )
    .await;
    let again = statuses(&gateway, &api, "GET", PAST_THE_BURST + 1).await;
    assert!(
        again.iter().any(|(status, _, _)| *status == THROTTLED.0),
        "case {case:?}: expected the per-method entry to shed a request, got {again:?}"
    );
}

