// SPDX-License-Identifier: CC0-1.0

//! How `CreateComputer` refuses a `DirectoryId` that resolves to no directory able to
//! service the join, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body,
//! dispatched by the multi-service router (`crate::harness`), and builds the directories it
//! acts on through the public creates. Nothing reaches into the state struct.
//!
//! The rule under test: a computer account is created IN the directory's own domain, by
//! that directory, so a well-formed `DirectoryId` that resolves to no directory serving a
//! domain — one the store does not hold, one still being built by the transition its create
//! owes, one whose record the account has deleted — is refused with HTTP 400 and the
//! modelled `DirectoryUnavailableException`, worded in the directory-lookup family's
//! sentence (the quoted id the request sent, that no such directory is there, and this
//! call's own request id). The class is what tells a caller to wait for a directory rather
//! than to stop using the id, and it is the SAME for every one of those cases, so the
//! diagnostic discloses neither where in its build a directory is nor whether a record for
//! it is held at all. `EntityDoesNotExistException`, which this operation also models, is
//! the class for an entity the join could not find WITHIN a directory, and is never the
//! answer for the directory that serves it.
//!
//! The closing case is the other half of the same rule: the identical call is ADMITTED once
//! the directory reports itself in service, which is observed by polling the reporting
//! operation rather than by waiting a fixed time.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::directory_lifecycle::{create_directory, minimal_create};
use crate::harness::{call_raw, ok, poll_until_absent, poll_until_in_service, service};
use crate::microsoft_ad::{create_microsoft_ad, minimal_microsoft_ad};

/// The header AWS reports a response's own request id in.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The modelled class for a directory that cannot service the join.
const UNAVAILABLE: &str = "DirectoryUnavailableException";

/// The other not-found class this operation models, which is never the answer for the
/// directory itself.
const DOES_NOT_EXIST: &str = "EntityDoesNotExistException";

/// Well-formed `DirectoryId`s no directory is ever created under: the form comes from the
/// model (`DirectoryId` is `^d-[0-9a-f]{10}$`,
/// `smithy_query(query="operation",operation="CreateComputer")`), so the boundary admits
/// each one and the handler has to resolve it in the store. The rows span the alphabet the
/// pattern admits.
const ABSENT_DIRECTORY_IDS: [(&str, &str); 4] = [
    ("d-0000000000", "an absent id of all zeros"),
    ("d-ffffffffff", "an absent id of all f"),
    ("d-abcdef0123", "an absent id using every hex digit"),
    ("d-9f8e7d6c5b", "an absent id of interleaved digits and letters"),
];

/// A `ComputerName` inside the modelled `len 1..15`.
const COMPUTER_NAME: &str = "joiner";

/// A one-time join `Password` inside the modelled `len 8..64` and its
/// `^[\u{0020}-\u{00FF}]+$`.
const JOIN_PASSWORD: &str = "Cmp-1-Time!Pw";

/// The `CreateComputer` body for `directory_id`.
fn join(directory_id: &str) -> Value {
    json!({
        "DirectoryId": directory_id,
        "ComputerName": COMPUTER_NAME,
        "Password": JOIN_PASSWORD
    })
}

/// The sentence this service states for a directory the request named and the service does
/// not have, with the request id the answer itself reported appended.
fn no_such_directory(directory_id: &str, request_id: &str) -> String {
    format!("Directory: '{directory_id}' does not exist. : RequestId: {request_id}")
}

/// One `CreateComputer` call: its status, the `__type` it reported, its `Message` and the
/// request id the response's own header carried.
async fn answered(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) -> (u16, String, String, Option<String>) {
    let response = call_raw(router, "CreateComputer", join(directory_id)).await;
    let status = response.status().as_u16();
    let header = response
        .headers()
        .get(REQUEST_ID_HEADER)
        .map(|value| value.to_str().expect("a textual header").to_string());
    let payload = response.into_body();
    let parsed: Value = serde_json::from_slice(&payload)
        .unwrap_or_else(|_| panic!("case {case:?}: expected a JSON body, got {payload:?}"));
    (
        status,
        parsed["__type"].as_str().unwrap_or_default().to_string(),
        parsed["Message"].as_str().unwrap_or_default().to_string(),
        header,
    )
}

/// Assert the join of `directory_id` is refused the one way the rule states.
async fn assert_refused_as_unavailable(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) {
    let (status, code, message, header) = answered(router, directory_id, case).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the join to be refused with 400, got {status} with \
         {code:?} {message:?}"
    );
    assert!(
        code.contains(UNAVAILABLE),
        "case {case:?}: expected {UNAVAILABLE}, got {code:?} with {message:?}"
    );
    assert!(
        !code.contains(DOES_NOT_EXIST),
        "case {case:?}: expected the directory's own class rather than {DOES_NOT_EXIST}, \
         got {code:?}"
    );
    let reported = header.unwrap_or_else(|| {
        panic!("case {case:?}: expected the {REQUEST_ID_HEADER} header, got none")
    });
    assert_eq!(
        message,
        no_such_directory(directory_id, &reported),
        "case {case:?}: expected the lookup sentence for the id the request sent, got \
         {message:?}"
    );
}

/// Delete `directory_id` and wait until the account no longer holds a record for it, so a
/// later call names an id the store cannot resolve. The waiting is the harness's one polling
/// loop ([`poll_until_absent`]), so this case waits for the condition and not for a span.
async fn delete_and_wait_until_gone(
    router: &MultiServiceRouter,
    directory_id: &str,
    case: &str,
) {
    ok(
        router,
        "DeleteDirectory",
        json!({ "DirectoryId": directory_id }),
        case,
    )
    .await;
    poll_until_absent(router, directory_id, case).await;
}

#[tokio::test]
async fn a_join_against_an_id_no_directory_is_stored_under_is_refused_as_unavailable() {
    let router = service();
    // One stored, serving directory, so a refusal cannot come from the store being empty.
    let stored =
        create_directory(&router, minimal_create("joined.example.com"), "the fixture").await;
    poll_until_in_service(&router, &stored, "the fixture").await;

    for (absent, case) in ABSENT_DIRECTORY_IDS {
        assert_refused_as_unavailable(&router, absent, case).await;
    }

    // An id the account HELD and deleted is an id the store cannot resolve either, and is
    // refused the same way.
    let deleted =
        create_directory(&router, minimal_create("gone.example.com"), "a deleted id").await;
    delete_and_wait_until_gone(&router, &deleted, "a deleted id").await;
    assert_refused_as_unavailable(&router, &deleted, "a deleted id").await;

    // The fixture itself still serves the very same call, so the refusals above were about
    // the id and not about the operation.
    let answer = ok(&router, "CreateComputer", join(&stored), "the fixture").await;
    assert_eq!(
        answer["Computer"]["ComputerName"],
        json!(COMPUTER_NAME),
        "case \"the fixture\": expected the account to be created in a serving directory, \
         got {answer}"
    );
}

#[tokio::test]
async fn a_join_against_a_directory_still_being_built_is_refused_as_unavailable() {
    // Both directory types a create of this service mints, so the refusal is the join
    // family's rather than one type's.
    for (case, microsoft_ad) in [
        ("a Simple AD directory still being built", false),
        ("a Managed Microsoft AD directory still being built", true),
    ] {
        let router = service();
        // The join is attempted on the call right after the create, while the directory is
        // still in the transient stage its create owes.
        let directory_id = if microsoft_ad {
            create_microsoft_ad(&router, minimal_microsoft_ad("building.example.com"), case).await
        } else {
            create_directory(&router, minimal_create("building.example.com"), case).await
        };
        assert_refused_as_unavailable(&router, &directory_id, case).await;

        // The refusal was about the directory not serving yet: polled until it reports
        // itself in service, the identical call is admitted and the account it created is
        // reported back.
        poll_until_in_service(&router, &directory_id, case).await;
        let answer = ok(&router, "CreateComputer", join(&directory_id), case).await;
        assert_eq!(
            answer["Computer"]["ComputerName"],
            json!(COMPUTER_NAME),
            "case {case:?}: expected the identical call to be admitted once the directory \
             serves, got {answer}"
        );
    }
}
