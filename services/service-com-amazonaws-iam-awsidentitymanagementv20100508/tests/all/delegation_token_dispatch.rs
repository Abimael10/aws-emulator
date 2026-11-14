// SPDX-License-Identifier: CC0-1.0

//! `SendDelegationToken`, end to end over the awsQuery wire.
//!
//! The rules being asserted:
//!
//! * a delegation request the account holds in the `ACCEPTED` state is finalized — "after
//!   the SendDelegationToken API call is successful, the request transitions to a FINALIZED
//!   state and cannot be rolled back" — which is what the read of that identifier reports
//!   afterwards, with every member the send is not about left as the account held it;
//! * a request in ANY OTHER state is refused with the modelled `InvalidInputException`
//!   (HTTP 400, awsQuery code `InvalidInput`), because "the delegation request must be in
//!   the ACCEPTED state when calling this API", and the request stays in the state the
//!   account holds it in;
//! * an identifier the account holds NO delegation request under references a resource
//!   entity that does not exist, so the call is refused with the modelled
//!   `NoSuchEntityException` (HTTP 404, awsQuery code `NoSuchEntity`) whose message
//!   describes the request asked about, and nothing is filed under that identifier.
//!
//! Each suite is a TABLE whose rows differ along what those rules generalise over: the
//! identifier's alphabet and both published length bounds, and the state the request was in
//! before the call — every value `stateType` declares, read from the model's own value set
//! (`ops::types::STATE_TYPE_VALUES`) rather than from the states one workflow happens to
//! pass through.
//!
//! The awsQuery harness is the one `tests/all/user_lifecycle.rs` owns, the delegation-request
//! fixtures are the ones `tests/all/delegation_request_reads.rs` and
//! `tests/all/delegation_request_acceptance.rs` own, so a request is built, filed and read
//! exactly one way.

use service_com_amazonaws_iam_awsidentitymanagementv20100508::ops::types::{
    DelegationRequest, STATE_TYPE_VALUES,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::delegation_request_acceptance::file;
use crate::delegation_request_reads::{held_delegation_requests, seed};
use crate::user_lifecycle::{
    account_id, call, element, elements, error_code, router, router_with_engine,
};

/// The state a request has to be in for its token to be sent: "the delegation request must
/// be in the ACCEPTED state when calling this API" (`smithy_query(query="doc",
/// shape="SendDelegationToken")`), one of the values `stateType` declares.
const STATE_ACCEPTED: &str = "ACCEPTED";

/// The state a request whose token has been sent reaches: "after the SendDelegationToken API
/// call is successful, the request transitions to a FINALIZED state and cannot be rolled
/// back" (same documentation).
const STATE_FINALIZED: &str = "FINALIZED";

/// The sentence this operation refuses a request that is not in the accepted state with:
/// entirely fixed text, stating the state the API requires.
const NOT_ACCEPTED: &str =
    "The delegation request must be in the ACCEPTED state when calling this API.";

/// The published maximum length of `delegationRequestIdType`: "len 16..128, pattern
/// `^[\w-]+$`" (`smithy_query(query="operation", operation="SendDelegationToken")`).
const MAX_ID_CHARACTERS: usize = 128;

/// The identifiers the not-found suite spans: the whole published alphabet of
/// `delegationRequestIdType` (letters in both cases, digits, `_`, `-`) and BOTH published
/// length bounds.
fn delegation_request_ids() -> Vec<String> {
    vec![
        // The published minimum length.
        "abcdef0123456789".to_string(),
        // The published maximum length.
        "z".repeat(MAX_ID_CHARACTERS),
        // The whole published alphabet, in a mixed case.
        "Partner_Delegation-4k".to_string(),
        // All digits, with a leading run of zeroes: an identifier is a handle, not a number.
        "00000000000000055555".to_string(),
        // Hyphen-separated, as a minted handle often reads.
        "1b2c-3d4e-5f60-7a89".to_string(),
    ]
}

/// One `SendDelegationToken` call.
async fn send(router: &MultiServiceRouter, delegation_request_id: &str) -> (u16, String) {
    call(
        router,
        "SendDelegationToken",
        &[("DelegationRequestId", delegation_request_id)],
    )
    .await
}

/// One `GetDelegationRequest` call: the implemented read every round trip is proved through.
async fn get(router: &MultiServiceRouter, delegation_request_id: &str) -> (u16, String) {
    call(
        router,
        "GetDelegationRequest",
        &[("DelegationRequestId", delegation_request_id)],
    )
    .await
}

/// One `AcceptDelegationRequest` call: the transition that makes a filed request sendable.
async fn accept(router: &MultiServiceRouter, case: &str, delegation_request_id: &str) {
    let (status, body) = call(
        router,
        "AcceptDelegationRequest",
        &[("DelegationRequestId", delegation_request_id)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the fixture AcceptDelegationRequest must succeed, got {status}: {body}"
    );
}

/// The members this operation does NOT write, which the read must report unchanged across a
/// send: everything the filing and the acceptance stored but the state and the last-updated
/// instant.
const UNTOUCHED_MEMBERS: &[&str] = &[
    "DelegationRequestId",
    "OwnerAccountId",
    "Description",
    "RequestMessage",
    "PermissionPolicy",
    "OwnerId",
    "ApproverId",
    "RequestorId",
    "RequestorName",
    "Notes",
    "RejectionReason",
    "RedirectUrl",
    "PolicyTemplateArn",
    "SessionDuration",
    "OnlySendByOwner",
    "CreateDate",
    "ExpirationTime",
];

/// RULE: an identifier the account holds no delegation request under references "a resource
/// entity that does not exist", so the send is refused HTTP 404 `NoSuchEntity` with the
/// message describing the request asked about, and nothing is filed under that identifier.
#[tokio::test]
async fn an_identifier_the_account_holds_nothing_under_is_refused_as_a_missing_entity() {
    for delegation_request_id in delegation_request_ids() {
        let (router, engine) = router_with_engine();
        // The account holds ANOTHER request, accepted and so sendable, so what is asserted
        // is the lookup of the identifier the request named rather than whether the account
        // holds any sendable request at all.
        seed(
            &engine,
            &DelegationRequest {
                delegation_request_id: Some("held1234567890ab".to_string()),
                state: Some(STATE_ACCEPTED.to_string()),
                ..DelegationRequest::default()
            },
        );

        let (status, body) = send(&router, &delegation_request_id).await;
        assert_eq!(
            status, 404,
            "case {delegation_request_id:?}: expected 404, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "NoSuchEntity",
            "case {delegation_request_id:?}: expected NoSuchEntity, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            format!("Delegation request not found: {delegation_request_id}"),
            "case {delegation_request_id:?}: the refusal must describe the delegation \
             request asked about, got {:?}",
            element(&body, "Message")
        );

        // The refused send filed nothing, and left the request the account does hold alone.
        assert_eq!(
            held_delegation_requests(&engine),
            vec!["held1234567890ab".to_string()],
            "case {delegation_request_id:?}: a refused send persists nothing, got {:?}",
            held_delegation_requests(&engine)
        );
        let (status, after) = get(&router, "held1234567890ab").await;
        assert_eq!(
            status, 200,
            "case {delegation_request_id:?}: the held request must still be readable, got \
             {status}: {after}"
        );
        assert_eq!(
            element(&after, "State"),
            STATE_ACCEPTED,
            "case {delegation_request_id:?}: a refused send must not move another request's \
             state, got {:?}",
            element(&after, "State")
        );
    }
}

/// RULE: a request in any state but `ACCEPTED` has no accepted delegation to send a token
/// for, so the send is refused HTTP 400 `InvalidInput` with the sentence stating the state
/// the API requires, and the request stays in the state the account holds it in. The rows
/// are every value `stateType` declares, so the rule is asserted over the whole state set.
#[tokio::test]
async fn a_request_in_any_other_state_is_refused_as_invalid_input() {
    for (row, state) in STATE_TYPE_VALUES.iter().enumerate() {
        let (router, engine) = router_with_engine();
        let delegation_request_id = format!("sendfromstate{row:0>4}");
        seed(
            &engine,
            &DelegationRequest {
                delegation_request_id: Some(delegation_request_id.clone()),
                description: Some("a request the send decides on its state".to_string()),
                requestor_id: Some(account_id().to_string()),
                state: Some((*state).to_string()),
                ..DelegationRequest::default()
            },
        );

        let case = format!("a request in {state}");
        let (status, body) = send(&router, &delegation_request_id).await;
        if *state == STATE_ACCEPTED {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the accepted request's token to be sent, got \
                 {status}: {body}"
            );
            continue;
        }
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {case:?}: expected InvalidInput, got {:?}: {body}",
            error_code(&body)
        );
        assert_eq!(
            element(&body, "Message"),
            NOT_ACCEPTED,
            "case {case:?}: expected {NOT_ACCEPTED:?}, got {:?}",
            element(&body, "Message")
        );

        // The refused send left the request as the account held it.
        let (status, after) = get(&router, &delegation_request_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the request must still be readable, got {status}: {after}"
        );
        assert_eq!(
            element(&after, "State"),
            *state,
            "case {case:?}: a refused send must not move the state, got {:?}",
            element(&after, "State")
        );
    }
}

/// RULE: a send of an accepted request takes it to `FINALIZED` — which is what the read of
/// that identifier reports afterwards — and the transition "cannot be rolled back", so a
/// second send of the same request is refused in the not-accepted class.
#[tokio::test]
async fn a_send_finalizes_the_accepted_request_once() {
    let router = router();

    // Rows: a request filed with no target account and one filed targeted at this account,
    // the two filings `CreateDelegationRequest` distinguishes, so the send is asserted over
    // both routes a request reaches the accepted state by.
    let rows: Vec<(&str, Option<&str>)> = vec![
        ("a request targeted at no account", None),
        ("a request targeted at this account", Some(account_id())),
    ];

    for (row, (label, owner)) in rows.into_iter().enumerate() {
        let case = format!("sending the token of {label}");
        let delegation_request_id =
            file(&router, &case, &format!("workflow-send-{row}"), owner).await;
        accept(&router, &case, &delegation_request_id).await;

        let (status, before) = get(&router, &delegation_request_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the accepted request must be readable, got {status}: {before}"
        );
        assert_eq!(
            element(&before, "State"),
            STATE_ACCEPTED,
            "case {case:?}: the fixture must leave the request accepted, got {:?}",
            element(&before, "State")
        );

        let (status, body) = send(&router, &delegation_request_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the send to be served, got {status}: {body}"
        );

        let (status, after) = get(&router, &delegation_request_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the finalized request must be readable, got {status}: {after}"
        );
        assert_eq!(
            element(&after, "State"),
            STATE_FINALIZED,
            "case {case:?}: a sent request reaches FINALIZED, got {:?}",
            element(&after, "State")
        );
        for member in UNTOUCHED_MEMBERS {
            assert_eq!(
                elements(&after, member),
                elements(&before, member),
                "case {case:?}: the send must leave {member} as the account held it, got \
                 {:?} after {:?}",
                elements(&after, member),
                elements(&before, member)
            );
        }

        // The transition "cannot be rolled back": the request is no longer accepted, so a
        // second send is refused in the not-accepted class and the state stays FINALIZED.
        let (status, body) = send(&router, &delegation_request_id).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected a second send to be refused, got {status}: {body}"
        );
        assert_eq!(
            error_code(&body),
            "InvalidInput",
            "case {case:?}: expected InvalidInput from the second send, got {:?}: {body}",
            error_code(&body)
        );
        let (status, after) = get(&router, &delegation_request_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the finalized request must still be readable, got {status}: \
             {after}"
        );
        assert_eq!(
            element(&after, "State"),
            STATE_FINALIZED,
            "case {case:?}: a refused second send must leave the request finalized, got {:?}",
            element(&after, "State")
        );
    }
}
