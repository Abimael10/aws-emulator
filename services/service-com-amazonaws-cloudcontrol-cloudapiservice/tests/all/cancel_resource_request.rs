// SPDX-License-Identifier: CC0-1.0

//! `CancelResourceRequest`: which resource operation requests can be canceled, and how the
//! two refusals are worded.
//!
//! The rule every case here holds the service to: **a cancel resolves its `RequestToken`
//! against the resource operation requests this service holds — a token no request answers
//! to is refused with the modelled `RequestTokenNotFoundException` (HTTP 404) naming that
//! token; a request in one of the two statuses a cancel acts on IS canceled: it abandons the
//! work it owed, reports `CANCEL_IN_PROGRESS`, explains that status with the message for a
//! cancellation the caller initiated, and reaches `CANCEL_COMPLETE` still carrying it; a
//! request a cancel ALREADY acted on is answered with that same cancellation, because the
//! operation is `@idempotent`; and a request in any other status is refused with
//! `ValidationException` (HTTP 400) naming the token and the status it is already in, because
//! "only resource operations requests with a status of PENDING or IN_PROGRESS can be
//! canceled" (`CancelResourceRequest`).**
//!
//! The rows differ along what the rule generalises over: which operation started the
//! request (`CREATE`, `UPDATE`, `DELETE`), which status it was in when the cancel arrived
//! (`IN_PROGRESS`, `CANCEL_IN_PROGRESS`, `SUCCESS`, `FAILED`, `CANCEL_COMPLETE`), the form
//! of a token no request answers to, and what the abandoned work leaves behind — a resource
//! that was never provisioned, one left exactly as it was, and one that is still there.

use serde_json::{json, Map, Value};

use super::emulator::{
    code, message, progress, text, Emulator, STATUS_CANCEL_COMPLETE, STATUS_CANCEL_IN_PROGRESS,
    STATUS_FAILED, STATUS_IN_PROGRESS, STATUS_SUCCESS,
};

/// A type whose resources live in this service's own store, so a case is about the request
/// rather than about one owning service.
const STREAM: &str = "AWS::Kinesis::Stream";

/// The `StatusMessage` a canceled request explains its status with, as a client reads it.
///
/// Spelled out here rather than taken from the crate, so what the case pins is the wording
/// on the wire rather than whatever the implementation happens to hold.
const CANCELLATION_MESSAGE: &str = "User Initialized the Cancellation";

/// One `ListResourceRequests` page, as the service reports it.
async fn listed_summaries(emulator: &Emulator) -> Vec<Value> {
    let mut request = Map::new();
    request.insert("MaxResults".to_string(), json!(100));
    let listed = emulator
        .cc_ok("ListResourceRequests", Value::Object(request))
        .await;
    listed["ResourceRequestStatusSummaries"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The request tokens one `ListResourceRequests` page reports.
async fn listed_tokens(emulator: &Emulator) -> Vec<String> {
    listed_summaries(emulator)
        .await
        .iter()
        .map(|summary| text(summary, "RequestToken"))
        .collect()
}

/// Cancel `request_token` and require the call to succeed, answering the `ProgressEvent` it
/// reports.
async fn cancel_ok(emulator: &Emulator, request_token: &str) -> Value {
    let (status, body) = emulator.cancel(request_token).await;
    assert_eq!(
        status, 200,
        "cancelling {request_token:?}: expected HTTP 200, got {status} with body {body:?}"
    );
    progress(&body)
}

#[tokio::test]
async fn a_running_request_is_canceled_and_its_work_is_abandoned() {
    let emulator = Emulator::new();

    // One running request per operation, each against its own resource: the resource an
    // UPDATE and a DELETE need is provisioned first, so cancelling them leaves something
    // for the case to look at. The two creates are independent, so they are started
    // together and waited for once.
    let seeded = [
        json!({ "StreamName": "patched", "ShardCount": 1 }),
        json!({ "StreamName": "doomed", "ShardCount": 2 }),
    ];
    let mut accepted = Vec::new();
    for desired in &seeded {
        let event = emulator.create_ok(STREAM, desired, None).await;
        accepted.push(text(&event, "RequestToken"));
    }
    let mut provisioned = Vec::new();
    for request_token in &accepted {
        provisioned.push(text(&emulator.settled(request_token).await, "Identifier"));
    }
    let (patched, doomed) = (provisioned[0].clone(), provisioned[1].clone());

    let creating = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "never-made" }), None)
            .await,
        "RequestToken",
    );
    let (status, body) = emulator
        .update(
            STREAM,
            &patched,
            &json!([{ "op": "replace", "path": "/ShardCount", "value": 9 }]),
            None,
        )
        .await;
    assert_eq!(
        status, 200,
        "the update: expected HTTP 200, got {status} with body {body:?}"
    );
    let updating = text(&progress(&body), "RequestToken");
    let (status, body) = emulator.delete(STREAM, &doomed).await;
    assert_eq!(
        status, 200,
        "the delete: expected HTTP 200, got {status} with body {body:?}"
    );
    let deleting = text(&progress(&body), "RequestToken");

    // Each cancel answers the request it names, in the status a cancellation that is still
    // running reports, and says when to ask again.
    let rows = [
        ("a create", &creating, "CREATE"),
        ("an update", &updating, "UPDATE"),
        ("a delete", &deleting, "DELETE"),
    ];
    for (case, request_token, operation) in rows {
        let canceled = cancel_ok(&emulator, request_token).await;
        assert_eq!(
            text(&canceled, "RequestToken"),
            *request_token,
            "case {case:?}: the cancel must report the request its token names, got \
             {canceled:?}"
        );
        assert_eq!(
            text(&canceled, "Operation"),
            operation,
            "case {case:?}: the cancel must report the operation the request ran, got \
             {canceled:?}"
        );
        assert_eq!(
            text(&canceled, "OperationStatus"),
            STATUS_CANCEL_IN_PROGRESS,
            "case {case:?}: a cancellation that has not completed must report \
             CANCEL_IN_PROGRESS, got {canceled:?}"
        );
        assert!(
            canceled["RetryAfter"].as_f64().is_some(),
            "case {case:?}: a request still being canceled must say when to ask again, got \
             {canceled:?}"
        );

        // `CancelResourceRequest` is `@idempotent`, and a request in one of the two
        // cancellation statuses is one this operation already acted on: the repeat answers
        // that same cancellation rather than starting a second one — the status it reports
        // is unchanged and the instant the cancellation completes stays exactly where the
        // first cancel put it.
        let repeated = cancel_ok(&emulator, request_token).await;
        assert_eq!(
            repeated, canceled,
            "case {case:?}: a repeat cancel must answer exactly what the first one did, \
             expected {canceled:?}, got {repeated:?}"
        );
        let after = progress(&emulator.status(request_token).await.1);
        assert_eq!(
            after["RetryAfter"], canceled["RetryAfter"],
            "case {case:?}: a repeat cancel must not move when the cancellation completes, \
             expected {:?}, got {:?}",
            canceled["RetryAfter"], after["RetryAfter"]
        );
    }

    // Each one then reaches the status a canceled request ends in, and nothing it owed was
    // applied: the create provisioned nothing, the update changed nothing, the delete
    // removed nothing.
    for (case, request_token, _) in rows {
        let settled = emulator
            .reaching(request_token, &[STATUS_CANCEL_COMPLETE])
            .await;
        assert!(
            settled["RetryAfter"].is_null(),
            "case {case:?}: a canceled request has nothing left to ask again for, got \
             {settled:?}"
        );
        assert!(
            settled["ResourceModel"].is_null(),
            "case {case:?}: a canceled request left no resource model behind, got {settled:?}"
        );
    }
    let (status, body) = emulator.get(STREAM, "never-made").await;
    assert_eq!(
        status, 404,
        "the canceled create must have provisioned nothing, got {status} with body {body:?}"
    );
    let kept = emulator.properties(STREAM, &patched).await;
    assert_eq!(
        kept,
        json!({ "StreamName": "patched", "ShardCount": 1 }),
        "the canceled update must have changed nothing, got {kept:?}"
    );
    let survivor = emulator.properties(STREAM, &doomed).await;
    assert_eq!(
        survivor,
        json!({ "StreamName": "doomed", "ShardCount": 2 }),
        "the canceled delete must have removed nothing, got {survivor:?}"
    );

    // And every canceled request is still one this service holds, resolvable by its token
    // and reported by the listing.
    let tokens = listed_tokens(&emulator).await;
    for (case, request_token, _) in rows {
        assert!(
            tokens.contains(request_token),
            "case {case:?}: a canceled request must still be listed, got {tokens:?}"
        );
    }
}

#[tokio::test]
async fn a_terminal_request_is_refused_in_the_status_it_reached() {
    let emulator = Emulator::new();

    // One request per terminal status NO cancellation produced: a create that succeeded,
    // and a create that the owning handler failed because the resource was already there.
    let succeeded = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "settled" }), None)
            .await,
        "RequestToken",
    );
    emulator.settled(&succeeded).await;
    let failed = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "settled" }), None)
            .await,
        "RequestToken",
    );
    let event = emulator.terminal(&failed).await;
    assert_eq!(
        text(&event, "OperationStatus"),
        STATUS_FAILED,
        "a second create of the same resource must fail, got {event:?}"
    );

    let rows = [
        ("a request that succeeded", &succeeded, STATUS_SUCCESS),
        ("a request the handler failed", &failed, STATUS_FAILED),
    ];
    for (case, request_token, reached) in rows {
        let (status, body) = emulator.cancel(request_token).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("ValidationException"),
            "case {case:?}: expected ValidationException, got {:?}",
            code(&body)
        );
        assert_eq!(
            message(&body),
            format!("Request {request_token} is already in status {reached}"),
            "case {case:?}: unexpected wording"
        );
        assert!(
            progress(&body).is_null(),
            "case {case:?}: a refused cancel reports no ProgressEvent, got {body:?}"
        );
        // The refusal changed nothing: the request is still in the status it had reached.
        let after = emulator.terminal(request_token).await;
        assert_eq!(
            text(&after, "OperationStatus"),
            reached,
            "case {case:?}: a refused cancel must leave the request as it was, got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_request_already_canceled_answers_that_cancellation_again() {
    let emulator = Emulator::new();

    // A request whose cancellation is over: nothing but `CancelResourceRequest` puts a
    // request in that status, so a further cancel is a repeat of the one that did.
    let canceled = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "abandoned" }), None)
            .await,
        "RequestToken",
    );
    cancel_ok(&emulator, &canceled).await;
    let settled = emulator
        .reaching(&canceled, &[STATUS_CANCEL_COMPLETE])
        .await;

    // Repeated, because an `@idempotent` operation answers the same thing however many
    // times it is called: each repeat reports the event the request already stands at, and
    // leaves it there.
    for repeat in 1..=3 {
        let event = cancel_ok(&emulator, &canceled).await;
        assert_eq!(
            event, settled,
            "repeat {repeat}: expected the cancellation the request already reached \
             ({settled:?}), got {event:?}"
        );
        let after = emulator.terminal(&canceled).await;
        assert_eq!(
            after, settled,
            "repeat {repeat}: a repeat cancel must leave the request as it was \
             ({settled:?}), got {after:?}"
        );
    }
}

#[tokio::test]
async fn a_token_no_request_answers_to_is_the_modelled_refusal() {
    let emulator = Emulator::new();

    // One request this service DID mint, so the store is not simply empty, and so the case
    // can show that a token the service issued resolves while these do not.
    let minted = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "issued" }), None)
            .await,
        "RequestToken",
    );

    // The rows differ along the form of the token: a well-formed random UUID, the nil UUID,
    // a short word, a value in the `+/=` half of the `RequestToken` pattern, and one at the
    // modelled maximum length of 128 characters.
    let tokens = [
        "3f8c6c4e-5d7a-4a1f-9a21-6f2b0c7d8e90",
        "00000000-0000-0000-0000-000000000000",
        "bogus",
        "AAAA=",
        &"t".repeat(128),
    ];
    for token in tokens {
        let (status, body) = emulator.cancel(token).await;
        assert_eq!(
            status, 404,
            "token {token:?}: expected HTTP 404, got {status} with body {body:?}"
        );
        assert!(
            code(&body).contains("RequestTokenNotFoundException"),
            "token {token:?}: expected RequestTokenNotFoundException, got {:?}",
            code(&body)
        );
        assert_eq!(
            message(&body),
            format!("Request with token {token} was not found"),
            "token {token:?}: unexpected wording"
        );
        assert_ne!(
            token, minted,
            "token {token:?}: the case needs a token this service did not mint"
        );
    }

    // A cancel of an unknown token minted nothing: none of them is a request this service
    // holds, so none of them is listed, and the one real request is untouched.
    let listed = listed_tokens(&emulator).await;
    for token in tokens {
        assert!(
            !listed.contains(&token.to_string()),
            "token {token:?}: a refused cancel must record no request, got {listed:?}"
        );
    }
    assert_eq!(
        listed,
        vec![minted.clone()],
        "only the request this service minted may be listed, got {listed:?}"
    );
    let still = emulator.status(&minted).await.1;
    assert_eq!(
        text(&progress(&still), "OperationStatus"),
        STATUS_IN_PROGRESS,
        "the request this service minted must be untouched, got {still:?}"
    );
}

#[tokio::test]
async fn a_canceled_request_reports_the_message_that_explains_its_status() {
    let emulator = Emulator::new();

    // The resources the UPDATE and the DELETE below act on, and the one a second create
    // collides with. The creates are independent, so they are started together and waited
    // for once.
    let seeded = [
        json!({ "StreamName": "explained", "ShardCount": 1 }),
        json!({ "StreamName": "expiring", "ShardCount": 1 }),
        json!({ "StreamName": "occupied", "ShardCount": 1 }),
    ];
    let mut accepted = Vec::new();
    for desired in &seeded {
        accepted.push(text(
            &emulator.create_ok(STREAM, desired, None).await,
            "RequestToken",
        ));
    }
    let mut provisioned = Vec::new();
    for request_token in &accepted {
        provisioned.push(text(&emulator.settled(request_token).await, "Identifier"));
    }
    let (patched, doomed) = (provisioned[0].clone(), provisioned[1].clone());

    // One running request per operation, to be canceled, plus the two requests the clock
    // settles on its own: one that succeeds, and one the handler fails because the resource
    // a create names is already there. All five run together.
    let creating = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "unexplained" }), None)
            .await,
        "RequestToken",
    );
    let (http, body) = emulator
        .update(
            STREAM,
            &patched,
            &json!([{ "op": "replace", "path": "/ShardCount", "value": 4 }]),
            None,
        )
        .await;
    assert_eq!(
        http, 200,
        "the update: expected HTTP 200, got {http} with body {body:?}"
    );
    let updating = text(&progress(&body), "RequestToken");
    let (http, body) = emulator.delete(STREAM, &doomed).await;
    assert_eq!(
        http, 200,
        "the delete: expected HTTP 200, got {http} with body {body:?}"
    );
    let deleting = text(&progress(&body), "RequestToken");
    let succeeding = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "untouched" }), None)
            .await,
        "RequestToken",
    );
    let failing = text(
        &emulator
            .create_ok(STREAM, &json!({ "StreamName": "occupied" }), None)
            .await,
        "RequestToken",
    );

    // The rows differ along the operation the canceled request was running, because the
    // message explains the CANCELLATION rather than the work abandoned.
    let canceled = [
        ("a canceled create", &creating),
        ("a canceled update", &updating),
        ("a canceled delete", &deleting),
    ];
    for (case, request_token) in canceled {
        // The cancel's own answer already carries it: the status it reports is the
        // cancellation, so the message explaining that status is reported with it.
        let event = cancel_ok(&emulator, request_token).await;
        assert_eq!(
            text(&event, "OperationStatus"),
            STATUS_CANCEL_IN_PROGRESS,
            "case {case:?}: expected the cancellation to be in progress, got {event:?}"
        );
        assert_eq!(
            text(&event, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: the cancel must explain the status it reports, got {:?}",
            event["StatusMessage"]
        );

        // And so does the status a later reader resolves the token to, while the
        // cancellation is still running.
        let polled = progress(&emulator.status(request_token).await.1);
        assert_eq!(
            text(&polled, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: a request still being canceled must keep the message, got {:?}",
            polled["StatusMessage"]
        );
    }

    // The message survives the cancellation completing: the terminal status is still the
    // one the caller's cancellation put the request in.
    for (case, request_token) in canceled {
        let settled = emulator
            .reaching(request_token, &[STATUS_CANCEL_COMPLETE])
            .await;
        assert_eq!(
            text(&settled, "StatusMessage"),
            CANCELLATION_MESSAGE,
            "case {case:?}: a canceled request must explain its terminal status, got {:?}",
            settled["StatusMessage"]
        );
    }

    // A request NO cancel touched explains its own status instead: one that succeeded has
    // nothing to explain, and one the handler failed carries that handler's diagnostic —
    // so the cancellation message belongs to the cancellation and to nothing else.
    let succeeded = emulator.settled(&succeeding).await;
    assert!(
        succeeded["StatusMessage"].is_null(),
        "a request that succeeded has no status to explain, got {succeeded:?}"
    );
    let failed = emulator.terminal(&failing).await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "a second create of the same resource must fail, got {failed:?}"
    );
    let diagnostic = text(&failed, "StatusMessage");
    assert!(
        !diagnostic.is_empty() && diagnostic != CANCELLATION_MESSAGE,
        "a failed request must explain its own failure, got {diagnostic:?}"
    );

    // Every reader of a request agrees about the message, so the listing reports what
    // `GetResourceRequestStatus` reports for each of these five requests.
    let summaries = listed_summaries(&emulator).await;
    let expected: Vec<(&str, &String, Option<&str>)> = vec![
        ("a canceled create", &creating, Some(CANCELLATION_MESSAGE)),
        ("a canceled update", &updating, Some(CANCELLATION_MESSAGE)),
        ("a canceled delete", &deleting, Some(CANCELLATION_MESSAGE)),
        ("a request that succeeded", &succeeding, None),
        ("a request the handler failed", &failing, Some(&diagnostic)),
    ];
    for (case, request_token, wanted) in expected {
        let summary = summaries
            .iter()
            .find(|summary| text(summary, "RequestToken") == **request_token)
            .unwrap_or_else(|| {
                panic!("case {case:?}: the listing must report {request_token:?}, got {summaries:?}")
            });
        match wanted {
            Some(wanted) => assert_eq!(
                text(summary, "StatusMessage"),
                wanted,
                "case {case:?}: the listing must report the same message, got {:?}",
                summary["StatusMessage"]
            ),
            None => assert!(
                summary["StatusMessage"].is_null(),
                "case {case:?}: the listing must report no message, got {:?}",
                summary["StatusMessage"]
            ),
        }
    }
}
