// SPDX-License-Identifier: CC0-1.0

//! Cancelling a resource operation request whose work the OWNING service's own API carries
//! out.
//!
//! The rule every case here holds the service to: **a request a cancel acts on explains the
//! status that cancel put it in with the one message a user-initiated cancellation carries,
//! and every reader of that request sees it — the cancel's own answer, a later
//! `GetResourceRequestStatus` while the cancellation is still running, the
//! `CANCEL_COMPLETE` the request settles at, the `@idempotent` repeat that echoes it, and
//! the `ListResourceRequests` summary — whatever the operation was doing and WHEREVER the
//! work it abandoned was being carried out; a request no cancel touched explains nothing.**
//!
//! The rows differ along how far the abandoned work had got when the cancel arrived: a
//! create the owning service had not been called for yet, and one that service had already
//! TAKEN and was still running its own lifecycle for — the state a create of this type is
//! held in for as long as a gate its template declares is waiting.
//!
//! `cancel_resource_request` holds the same rule for the types whose work this service's own
//! store applies, so what the two suites assert together is that the message belongs to the
//! CANCELLATION rather than to where the abandoned work lived. One further case cannot be
//! driven from here: a cancel that lands while the call this service made is still out, which
//! no order of requests pins down — the store's own tests hold that one
//! (`state::owning_api`).

use std::time::{Duration, Instant};

use serde_json::{json, Map, Value};

use super::emulator::{
    element, progress, text, Emulator, SETTLED_INTERVAL, STATUS_CANCEL_COMPLETE,
    STATUS_CANCEL_IN_PROGRESS, STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_PENDING, STATUS_SUCCESS,
};

/// The type whose resource operations CloudFormation carries out through its own API.
const STACK: &str = "AWS::CloudFormation::Stack";

/// The `Operation` a create request reports.
const OPERATION_CREATE: &str = "CREATE";

/// The `StatusMessage` a cancelled request explains its status with, as a client reads it.
///
/// Spelled out here rather than taken from the crate, so what a case pins is the wording on
/// the wire rather than whatever the implementation happens to hold.
const CANCELLATION_MESSAGE: &str = "User Initialized the Cancellation";

/// How long a wait in this suite runs before the case fails.
///
/// Every wait here is for a status reached through ANOTHER service's own lifecycle — a
/// stack's create runs that service's own deployment, each step a further request of this
/// emulator — so the budget is far above anything that flow takes, including on a host
/// running the whole workspace's tests at once. No case waits for a span: each re-reads
/// until the status it is after is there or this deadline passes.
const STACK_BUDGET: Duration = Duration::from_secs(180);

/// A template whose only resource is a gate waiting for a signal no case here ever sends, so
/// the stack stays in its own create lifecycle and the request following it stays
/// non-terminal — which is what a cancel of work in flight needs.
fn gated_template() -> String {
    json!({
        "Resources": {
            "Gate": {
                "Type": "AWS::CloudFormation::WaitCondition",
                "CreationPolicy": { "ResourceSignal": { "Count": 1, "Timeout": "PT10M" } },
            }
        }
    })
    .to_string()
}

/// A template with no gate at all — a wait condition HANDLE, which is a resource of
/// CloudFormation's own that nothing waits on — so the stack's create completes and the
/// request following it succeeds.
fn ungated_template() -> String {
    json!({ "Resources": { "Handle": { "Type": "AWS::CloudFormation::WaitConditionHandle" } } })
        .to_string()
}

/// The desired state a create of this type carries: the stack's name and the template it is
/// deployed from.
fn desired_state(stack_name: &str, template: &str) -> Value {
    json!({ "StackName": stack_name, "TemplateBody": template })
}

/// Poll the request `token` names until it reports one of `wanted`, answering the
/// `ProgressEvent` it reports then.
///
/// Local to this suite because every wait here follows another SERVICE's own lifecycle, which
/// the harness's own budget is not sized for. The wait is for the STATUS and never for a
/// span.
async fn reaching(case: &str, emulator: &Emulator, token: &str, wanted: &[&str]) -> Value {
    let started = Instant::now();
    loop {
        let (status, body) = emulator.status(token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             body {body:?}"
        );
        let event = progress(&body);
        let reached = text(&event, "OperationStatus");
        if wanted.contains(&reached.as_str()) {
            return event;
        }
        assert!(
            started.elapsed() < STACK_BUDGET,
            "case {case:?}: expected the request to report one of {wanted:?} within \
             {STACK_BUDGET:?}, it is still {reached:?}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    }
}

/// Poll the request `token` names until it names the resource the OWNING service assigned,
/// answering that identifier — which is reported only once that service has TAKEN the work,
/// so it is also what says the work is now running THERE.
async fn awaited_identifier(case: &str, emulator: &Emulator, token: &str) -> String {
    let started = Instant::now();
    loop {
        let (status, body) = emulator.status(token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             body {body:?}"
        );
        let event = progress(&body);
        let reached = text(&event, "OperationStatus");
        assert_eq!(
            reached, STATUS_IN_PROGRESS,
            "case {case:?}: expected the create to be held open by the stack's own lifecycle, \
             got {event:?}"
        );
        let identifier = text(&event, "Identifier");
        if !identifier.is_empty() {
            return identifier;
        }
        assert!(
            started.elapsed() < STACK_BUDGET,
            "case {case:?}: expected the create to name the stack it provisions within \
             {STACK_BUDGET:?}, it still reports {event:?}"
        );
        tokio::time::sleep(SETTLED_INTERVAL).await;
    }
}

/// The `ProgressEventSummary` `ListResourceRequests` reports for `token`.
async fn listed_summary(case: &str, emulator: &Emulator, token: &str) -> Value {
    let mut request = Map::new();
    request.insert("MaxResults".to_string(), json!(100));
    let listed = emulator
        .cc_ok("ListResourceRequests", Value::Object(request))
        .await;
    let summaries = listed["ResourceRequestStatusSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    summaries
        .into_iter()
        .find(|summary| text(summary, "RequestToken") == token)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the listing to report the request {token:?}, got {listed:?}")
        })
}

/// One row of the cancellation table: what the case is, the stack's name, and whether the
/// cancel waits until the owning service has TAKEN the work before it arrives.
struct Row {
    case: &'static str,
    stack: &'static str,
    after_the_owning_service_took_the_work: bool,
}

#[tokio::test]
async fn a_cancelled_request_explains_its_status_wherever_the_work_was_running() {
    let rows = [
        Row {
            case: "cancelled as soon as the create was accepted",
            stack: "cc-cancel-accepted",
            after_the_owning_service_took_the_work: false,
        },
        Row {
            case: "cancelled while the owning service was still creating the stack",
            stack: "cc-cancel-mid-flight",
            after_the_owning_service_took_the_work: true,
        },
    ];
    for row in rows {
        let case = row.case;
        let emulator = Emulator::with_cloudformation();
        let accepted = emulator
            .create_ok(STACK, &desired_state(row.stack, &gated_template()), None)
            .await;
        let token = text(&accepted, "RequestToken");
        let started = text(&accepted, "OperationStatus");
        assert!(
            started == STATUS_IN_PROGRESS || started == STATUS_PENDING,
            "case {case:?}: expected the create to be answered in a status a cancel acts on, \
             got {accepted:?}"
        );

        if row.after_the_owning_service_took_the_work {
            // The owning service has the work: the request names the stack that service
            // assigned an id to, and that id is the handle its own read reports.
            let identifier = awaited_identifier(case, &emulator, &token).await;
            let described = emulator
                .cfn_ok("DescribeStacks", &[("StackName", row.stack)])
                .await;
            assert_eq!(
                element(&described, "StackId"),
                identifier,
                "case {case:?}: expected the running request to name the stack id the owning \
                 service assigned, got {identifier:?} against {described}"
            );
        }

        // The cancel's own answer already carries the message: the status it reports is the
        // cancellation, so what explains that status is reported with it.
        let (status, body) = emulator.cancel(&token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the cancellation to be accepted, got {status} with body \
             {body:?}"
        );
        let cancelled = progress(&body);
        assert_eq!(
            text(&cancelled, "RequestToken"),
            token,
            "case {case:?}: the cancel must report the request its token names, got \
             {cancelled:?}"
        );
        assert_eq!(
            text(&cancelled, "Operation"),
            OPERATION_CREATE,
            "case {case:?}: the cancel must report the operation the request ran, got \
             {cancelled:?}"
        );
        assert_eq!(
            text(&cancelled, "OperationStatus"),
            STATUS_CANCEL_IN_PROGRESS,
            "case {case:?}: a cancellation that has not completed must report \
             CANCEL_IN_PROGRESS, got {cancelled:?}"
        );
        assert_eq!(
            text(&cancelled, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: the cancel must explain the status it reports, got {:?}",
            cancelled["StatusMessage"]
        );

        // And so does the status a later reader resolves the token to, in whichever of the
        // two cancellation statuses the request stands in by then.
        let (status, body) = emulator.status(&token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from GetResourceRequestStatus, got {status} with \
             body {body:?}"
        );
        let polled = progress(&body);
        let reported = text(&polled, "OperationStatus");
        assert!(
            reported == STATUS_CANCEL_IN_PROGRESS || reported == STATUS_CANCEL_COMPLETE,
            "case {case:?}: a cancelled request must report a cancellation status, got \
             {polled:?}"
        );
        assert_eq!(
            text(&polled, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: a cancelled request must keep the message a later reader reads, \
             got {:?}",
            polled["StatusMessage"]
        );

        // The message survives the cancellation completing: the terminal status is still the
        // one the caller's cancellation put the request in, and the abandoned work left no
        // resource model behind and nothing to poll for.
        let settled = reaching(case, &emulator, &token, &[STATUS_CANCEL_COMPLETE]).await;
        assert_eq!(
            text(&settled, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: a cancelled request must explain its terminal status, got {:?}",
            settled["StatusMessage"]
        );
        assert!(
            settled["RetryAfter"].is_null(),
            "case {case:?}: a settled request must name no instant to poll at, got {settled:?}"
        );
        assert!(
            settled["ResourceModel"].is_null(),
            "case {case:?}: a cancelled request left no resource model behind, got {settled:?}"
        );

        // A repeat of the `@idempotent` cancel echoes that same terminal event, and leaves
        // the request exactly where the first cancel settled it.
        let (status, body) = emulator.cancel(&token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected a repeat cancel to be accepted, got {status} with body \
             {body:?}"
        );
        assert_eq!(
            progress(&body),
            settled,
            "case {case:?}: a repeat cancel must echo the cancellation the request reached \
             ({settled:?}), got {body:?}"
        );
        let again = reaching(case, &emulator, &token, &[STATUS_CANCEL_COMPLETE]).await;
        assert_eq!(
            again, settled,
            "case {case:?}: nothing after the cancellation may move the request, expected \
             {settled:?}, got {again:?}"
        );

        // Every reader of the request agrees about the message, the listing included.
        let summary = listed_summary(case, &emulator, &token).await;
        assert_eq!(
            text(&summary, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: the listing must report the message the request carries, got \
             {summary:?}"
        );
        assert_eq!(
            text(&summary, "OperationStatus"),
            STATUS_CANCEL_COMPLETE,
            "case {case:?}: the listing must report the status the request reached, got \
             {summary:?}"
        );
    }
}

#[tokio::test]
async fn a_request_no_cancel_touched_explains_nothing() {
    // The control for the rule above: a request of the SAME type, whose work the same owning
    // service carried out to the end, has no status to explain — so the cancellation message
    // belongs to the cancellation and to nothing else.
    let case = "a create the stack's own lifecycle completed";
    let emulator = Emulator::with_cloudformation();
    let accepted = emulator
        .create_ok(
            STACK,
            &desired_state("cc-cancel-control", &ungated_template()),
            None,
        )
        .await;
    let token = text(&accepted, "RequestToken");
    let settled = reaching(
        case,
        &emulator,
        &token,
        &[STATUS_SUCCESS, STATUS_FAILED, STATUS_CANCEL_COMPLETE],
    )
    .await;
    assert_eq!(
        text(&settled, "OperationStatus"),
        STATUS_SUCCESS,
        "case {case:?}: expected the create to succeed, got {settled:?}"
    );
    assert!(
        settled["StatusMessage"].is_null(),
        "case {case:?}: a request that succeeded has no status to explain, got {settled:?}"
    );
}
