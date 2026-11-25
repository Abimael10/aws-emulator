// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the THROTTLE a stage's `methodSettings` put in front of a deployed
//! method: which requests the `execute-api` endpoint admits before the method's integration is
//! performed, and what a shed one is answered with.
//!
//! Every case drives the real wire path — an unsigned request to the api's own invoke host
//! through the multi-service router, with the control plane driven through the same emulator
//! ([`Gateway`]) — so what is asserted is what a client of a published invoke URL sees.
//!
//! The RULES under test, each over a SET of inputs rather than one recorded call:
//!
//! * `MethodSetting$throttlingRateLimit` ("specifies the throttling rate limit") and
//!   `MethodSetting$throttlingBurstLimit` ("specifies the throttling burst limit") are the
//!   token bucket the stage meters the method with: the burst is the capacity and the rate
//!   refills it, so a caller that waits for each answer is served what the bucket holds and
//!   shed once it is spent, and is served again once the rate has refilled a request;
//! * the burst is also how many requests the stage serves AT ONCE, so the submissions of one
//!   method in flight TOGETHER may draw ahead of the rate up to the burst: a burst of
//!   submissions larger than the allowance is PARTIALLY shed — the allowance is served by the
//!   integration and only the excess is refused — rather than shed whole, and what was drawn
//!   ahead is charged, so the method is served at its rate over time;
//! * a shed request is answered the api's own `THROTTLED` gateway response — HTTP 429, the
//!   `message` "Too Many Requests" and `TooManyRequestsException` in the `x-amzn-errortype`
//!   header a client classifies an answer by — with the integration never performed, so the
//!   body a served call carries cannot appear on a refusal;
//! * a method whose settings declare no limit that bounds anything — the sentinel that asks
//!   for no throttle at all, a limit of none, and a stage carrying no `methodSettings` entry
//!   for the method — is not metered: every request it serves is answered by the integration.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::execute_api_data_plane::{deploy, invoke, json_template, mock_method, Invoked};
use crate::rest_api_lifecycle::{create_api, Gateway};

/// The stage every api below is deployed to.
const STAGE: &str = "test";

/// The body the throttled method's `MOCK` integration answers: what proves a request was
/// SERVED by the integration, so no refusal may carry it.
const SERVED: &str = r#"{"served":true}"#;

/// The `message` the `THROTTLED` gateway response carries, which has no variable part.
const TOO_MANY_REQUESTS: &str = "Too Many Requests";

/// The exception class a shed request is reported in.
const TOO_MANY_REQUESTS_CLASS: &str = "TooManyRequestsException";

/// A rate so low that nothing it refills can be mistaken for a request the burst admitted:
/// one request every hundred seconds, which no case below waits for.
const UNREFILLED_RATE: f64 = 0.01;

/// How many submissions a burst case sends at once — more than any allowance it is metered
/// with, so the excess has to be shed, and few enough that the case is cheap to repeat.
const AT_ONCE: usize = 12;

/// How long a case waits for a condition it polls for: generous, because what it waits for is
/// the stage serving requests that arrived together and not a span of time.
const DEADLINE: Duration = Duration::from_secs(30);

/// How long a poll waits before looking again.
const POLL: Duration = Duration::from_millis(25);

/// A REST api carrying one `MOCK` `GET` on its root resource, deployed to [`STAGE`]: the api
/// id, ready to be invoked at `/{STAGE}/`.
async fn deployed_mock_api(gateway: &Gateway, name: &str) -> String {
    let (api, root) = create_api(gateway, name).await;
    mock_method(gateway, &api, &root, "GET", "200", json_template(SERVED)).await;
    deploy(gateway, &api, STAGE).await;
    api
}

/// `UpdateStage` writing the throttle `rate`/`burst` on the stage's whole-stage wildcard key,
/// which is the DEFAULT method throttling every method of the stage is metered at.
async fn throttled(gateway: &Gateway, api: &str, rate: &str, burst: &str, case: &str) -> Value {
    let answer = gateway
        .call(
            "PATCH",
            &format!("/restapis/{api}/stages/{STAGE}"),
            Some(json!({ "patchOperations": [
                { "op": "replace", "path": "/*/*/throttling/rateLimit", "value": rate },
                { "op": "replace", "path": "/*/*/throttling/burstLimit", "value": burst },
            ]})),
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "case {case}: expected 200 from UpdateStage, got {} with {:?}",
        answer.status, answer.body
    );
    answer.body
}

/// One `GET` of the deployed stage's root method.
async fn sent(gateway: &Gateway, api: &str) -> Invoked {
    invoke(gateway, api, "GET", &format!("/{STAGE}/"), &[]).await
}

/// Assert that `answer` is the integration's own 200.
fn assert_served(answer: &Invoked, case: &str) {
    assert_eq!(
        (answer.status, answer.body.as_str()),
        (200, SERVED),
        "case {case}: expected the integration's 200 {SERVED:?}, got {} with {:?}",
        answer.status,
        answer.body
    );
}

/// Assert that `answer` is the `THROTTLED` refusal, with the integration never performed.
fn assert_shed(answer: &Invoked, case: &str) {
    assert_eq!(
        answer.status, 429,
        "case {case}: expected 429 from the stage's throttle, got {} with {:?}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.message().as_deref(),
        Some(TOO_MANY_REQUESTS),
        "case {case}: expected the sentence {TOO_MANY_REQUESTS:?}, got {:?}",
        answer.body
    );
    assert_eq!(
        answer.header("x-amzn-errortype"),
        Some(TOO_MANY_REQUESTS_CLASS),
        "case {case}: expected the class {TOO_MANY_REQUESTS_CLASS:?}, got {:?}",
        answer.header("x-amzn-errortype")
    );
    assert!(
        !answer.body.contains("served"),
        "case {case}: a shed request must not carry the integration's own body, got {:?}",
        answer.body
    );
}

/// One row of the bucket table: what a failing assertion calls it, the pair the patch writes,
/// and how many requests a caller that waits for each answer is served before it is shed.
struct Bucket {
    case: &'static str,
    rate: &'static str,
    burst: &'static str,
    /// How many requests the bucket's own capacity serves, `None` for a pair that meters
    /// nothing at all — every request of such a method is served.
    capacity: Option<usize>,
}

/// RULE: the pair a `methodSettings` entry states is the token bucket the stage meters the
/// method with — the burst is the capacity, the rate refills it — so a caller that waits for
/// each answer is served what the bucket holds and shed once it is spent, and a pair that
/// bounds nothing meters nothing.
#[tokio::test]
async fn a_caller_that_waits_for_each_answer_is_served_what_the_methods_bucket_holds() {
    // Rows differ along the pair the settings state: a burst of one, a wider burst, the
    // sentinel that asks for no throttle on either member, and a limit of none.
    let rows = [
        Bucket {
            case: "a burst of one",
            rate: "0.01",
            burst: "1",
            capacity: Some(1),
        },
        Bucket {
            case: "a burst of three",
            rate: "0.01",
            burst: "3",
            capacity: Some(3),
        },
        Bucket {
            case: "a burst of one at a fractional rate",
            rate: "0.5",
            burst: "1",
            capacity: Some(1),
        },
        Bucket {
            // `-1` is the one negative either member takes: it asks for NO throttle at all
            // (`crate::update_stage` pins the control plane's reading of it).
            case: "the unthrottled sentinel on both members",
            rate: "-1",
            burst: "-1",
            capacity: None,
        },
        Bucket {
            case: "the unthrottled sentinel on the burst alone",
            rate: "10",
            burst: "-1",
            capacity: None,
        },
        Bucket {
            case: "a burst of none",
            rate: "10",
            burst: "0",
            capacity: None,
        },
        Bucket {
            case: "a rate of none",
            rate: "0",
            burst: "10",
            capacity: None,
        },
    ];
    for row in rows {
        let case = row.case;
        let gateway = Gateway::new();
        let api = deployed_mock_api(&gateway, "bucket-throttle").await;
        throttled(&gateway, &api, row.rate, row.burst, case).await;

        // The capacity the bucket holds, taken by a caller that waits for every answer: each
        // request arrives with nothing of the method in flight, so the bucket alone answers
        // it.
        let capacity = row.capacity.unwrap_or(4);
        for index in 0..capacity {
            let answer = sent(&gateway, &api).await;
            assert_served(&answer, &format!("{case}: request {index} of the capacity"));
        }
        // And the request past it, which a metered method sheds and an unmetered one serves.
        let past = sent(&gateway, &api).await;
        match row.capacity {
            Some(_) => assert_shed(&past, &format!("{case}: the request past the capacity")),
            None => assert_served(&past, &format!("{case}: a method nothing meters")),
        }
    }
}

/// RULE: the burst is how many requests the stage serves AT ONCE, so the submissions of one
/// method in flight TOGETHER may draw ahead of the rate up to the burst — a burst larger than
/// the allowance is partially shed rather than shed whole — and what they drew is CHARGED, so
/// the method is held to its rate afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn submissions_that_arrive_together_are_served_the_burst_and_the_excess_is_shed() {
    // Rows differ along the burst the settings state, against the same larger burst of
    // submissions: an allowance of one request at once, and a wider one. The rate refills
    // nothing a case could mistake for the allowance.
    for burst in [1_usize, 3] {
        let case = format!("a burst of {burst} met by {AT_ONCE} submissions at once");
        let gateway = Arc::new(Gateway::new());
        let api = deployed_mock_api(&gateway, "burst-throttle").await;
        throttled(
            &gateway,
            &api,
            &UNREFILLED_RATE.to_string(),
            &burst.to_string(),
            &case,
        )
        .await;
        // The bucket's own capacity, spent by a caller that waits for each answer, so what a
        // submission of the burst is served with can only be the allowance it drew ahead of
        // the rate.
        for index in 0..burst {
            let answer = sent(&gateway, &api).await;
            assert_served(&answer, &format!("{case}: request {index} of the capacity"));
        }
        assert_shed(
            &sent(&gateway, &api).await,
            &format!("{case}: the request past the spent bucket"),
        );

        // Then the burst: submissions arrive together until the stage serves some of them —
        // whether any two of them are in flight at the same instant is the host's business,
        // so the case looks again until they are rather than waiting a span of time. A
        // submission the throttle sheds spends nothing, so an attempt that overlapped with
        // none leaves the accounting as it was.
        let deadline = Instant::now() + DEADLINE;
        let mut served = 0usize;
        let mut shed = 0usize;
        while Instant::now() < deadline {
            let mut submissions = Vec::with_capacity(AT_ONCE);
            for _ in 0..AT_ONCE {
                let gateway = Arc::clone(&gateway);
                let api = api.clone();
                submissions.push(tokio::spawn(async move {
                    let answer = sent(&gateway, &api).await;
                    (answer.status, answer.body)
                }));
            }
            served = 0;
            shed = 0;
            for submission in submissions {
                let (status, body) = submission.await.expect("the submission was answered");
                match status {
                    200 => {
                        assert_eq!(
                            body, SERVED,
                            "case {case}: a served submission carries the integration's own \
                             body, got {body:?}"
                        );
                        served += 1;
                    }
                    _ => {
                        assert_eq!(
                            status, 429,
                            "case {case}: a submission is either served or shed 429, got \
                             {status} with {body:?}"
                        );
                        shed += 1;
                    }
                }
            }
            if served > 0 {
                break;
            }
            // Nothing of this attempt overlapped, and a shed submission spends nothing, so
            // look again rather than waiting out a span of time.
            tokio::time::sleep(POLL).await;
        }
        assert!(
            served > 0 && served <= burst,
            "case {case}: expected the allowance of at most {burst} served at once and at \
             least one of them, got {served} served"
        );
        assert_eq!(
            shed,
            AT_ONCE - served,
            "case {case}: expected the excess of the burst shed, got {served} served and {shed} \
             shed of {AT_ONCE}"
        );
        // What the burst drew ahead of the rate is charged, so the next caller that waits for
        // its answer is shed until the rate has repaid it — which this rate never does within
        // the case.
        assert_shed(
            &sent(&gateway, &api).await,
            &format!("{case}: the request after the draw"),
        );
    }
}

/// RULE: the rate refills the bucket, so a method shed at one instant is served again once the
/// rate has refilled a request — a throttled method shapes its traffic rather than closing.
#[tokio::test]
async fn a_shed_method_is_served_again_once_its_rate_refills_the_bucket() {
    // Rows differ along the rate the settings state, each with the narrowest burst: the
    // refilled request arrives sooner the higher the rate, and the case waits for the request
    // to be served rather than for the span either rate names.
    for rate in ["5", "2.5", "20"] {
        let case = format!("a burst of one refilled at {rate} requests a second");
        let gateway = Gateway::new();
        let api = deployed_mock_api(&gateway, "refill-throttle").await;
        throttled(&gateway, &api, rate, "1", &case).await;

        assert_served(&sent(&gateway, &api).await, &format!("{case}: the first request"));
        assert_shed(
            &sent(&gateway, &api).await,
            &format!("{case}: the request past the spent bucket"),
        );

        // Look again until the refilled request is served, under a deadline far longer than
        // any of these rates needs.
        let deadline = Instant::now() + DEADLINE;
        let mut refilled = None;
        while Instant::now() < deadline {
            let answer = sent(&gateway, &api).await;
            if answer.status == 200 {
                refilled = Some(answer);
                break;
            }
            assert_shed(&answer, &format!("{case}: a request before the refill"));
            tokio::time::sleep(POLL).await;
        }
        let refilled =
            refilled.unwrap_or_else(|| panic!("case {case}: the rate never refilled a request"));
        assert_served(&refilled, &format!("{case}: the refilled request"));
    }
}
