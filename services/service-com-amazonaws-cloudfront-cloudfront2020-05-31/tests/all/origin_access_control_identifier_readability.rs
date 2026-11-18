// SPDX-License-Identifier: CC0-1.0

//! Which of the ONE not-found code's two sentences an origin access control `Id` earns, end to
//! end over HTTP.
//!
//! The rule under test: an operation addressed by an origin access control `Id`
//! (`GetOriginAccessControl`, `GetOriginAccessControlConfig`, `UpdateOriginAccessControl`,
//! `DeleteOriginAccessControl` — `smithy_query(query="operation",
//! operation="GetOriginAccessControl")` reports `Id` as a required `@httpLabel` on a plain
//! `string`, with no `@pattern` and no `@length`, and `errors: [AccessDenied,
//! NoSuchOriginAccessControl]`) answers that one code and its 404 for every `Id` it cannot
//! resolve, so the MESSAGE is all that tells the two conditions apart:
//!
//! * a value this account holds as a resource of ANOTHER kind of the one identifier form
//!   CloudFront draws a distribution, a streaming distribution, an origin access identity and
//!   an origin access control from was issued for THAT kind, and a value written in no
//!   identifier alphabet this service issues one in was never an identifier at all — each is
//!   the identifier the service reports it cannot read, "Id format is invalid";
//! * every OTHER value the store holds no control under is the control reported ABSENT — "The
//!   specified origin access control does not exist." — whatever letter it opens with and
//!   however wide it is, the namespace letter and the minted width being no part of the
//!   decision.
//!
//! The rows span what the rule generalises over: the unreadable rows carry the minted
//! identifiers of TWO other kinds of that form (a distribution's and an origin access
//! identity's) and five values that break the identifier alphabet in different ways (a minted
//! identifier in lower case, a UUID, a bare word, a caller reference, a domain name), and the
//! absent rows vary the form of a value nothing answers to — the control's own identifier with
//! a character changed, an identifier of the form that was never minted, a public key
//! identifier (a kind outside that form), bodies wider and narrower than the one this service
//! issues, an unminted namespace letter and a body of digits alone. Every row is driven through
//! all four operations, and the live resources are read back afterwards to show the refusals
//! touched nothing.
//!
//! The wire harness and the create helpers are the crate's own.

use smithy_server_runtime::MultiServiceRouter;

use crate::announced_location_uri::origin_access_identity_xml;
use crate::distribution_lifecycle::{call, config, create_distribution, emulator, Answer, API_VERSION};
use crate::origin_access_control_lifecycle::{create_control, get_control, table};
use crate::public_key_lifecycle::{create_key, pem, Key};

/// The sentence a control the store holds nothing for earns, spelled out here rather than taken
/// from the crate, so a reworded refusal fails this suite.
const NO_SUCH_CONTROL: &str = "The specified origin access control does not exist.";

/// The sentence an identifier issued for another kind of the shared form earns.
const UNREADABLE_IDENTIFIER: &str = "Id format is invalid";

/// One operation addressed by an origin access control `Id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    GetOriginAccessControl,
    GetOriginAccessControlConfig,
    UpdateOriginAccessControl,
    DeleteOriginAccessControl,
}

/// Every operation of this resource addressed by its `Id`.
const OPS: &[Op] = &[
    Op::GetOriginAccessControl,
    Op::GetOriginAccessControlConfig,
    Op::UpdateOriginAccessControl,
    Op::DeleteOriginAccessControl,
];

impl Op {
    /// Send this operation at `id`, carrying the live control's current version token so the
    /// conditional paths are never refused for a precondition, and a configuration the update
    /// accepts so it is never refused for its payload.
    async fn send(
        self,
        router: &MultiServiceRouter,
        id: &str,
        document: &str,
        version: &str,
    ) -> Answer {
        match self {
            Self::GetOriginAccessControl => get_control(router, id).await,
            Self::GetOriginAccessControlConfig => {
                call(
                    router,
                    "GET",
                    &format!("/{API_VERSION}/origin-access-control/{id}/config"),
                    None,
                    None,
                )
                .await
            }
            Self::UpdateOriginAccessControl => {
                call(
                    router,
                    "PUT",
                    &format!("/{API_VERSION}/origin-access-control/{id}/config"),
                    Some(document),
                    Some(version),
                )
                .await
            }
            Self::DeleteOriginAccessControl => {
                call(
                    router,
                    "DELETE",
                    &format!("/{API_VERSION}/origin-access-control/{id}"),
                    None,
                    Some(version),
                )
                .await
            }
        }
    }
}

/// The live resources these rows address, and the document and version token the conditional
/// operations submit.
struct Live {
    control_id: String,
    control_document: String,
    control_etag: String,
    distribution_id: String,
    origin_access_identity_id: String,
    public_key_id: String,
}

/// Stand up one origin access control, one distribution, one origin access identity and one
/// public key: the control is what the rows address, and the other three are the identifiers a
/// row submits in its place.
async fn live(router: &MultiServiceRouter) -> Live {
    let control = table()[0].with_name("the control these rows address", "oac-readability");
    let control_document = control.xml();
    let created = create_control(router, &control_document).await;
    assert_eq!(
        created.status, 201,
        "case creating the control: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let control_id = created.root().text_at("Id").unwrap_or_default().to_string();

    let distribution = create_distribution(
        router,
        &config("the distribution these rows name", "oac-readability").xml(),
    )
    .await;
    assert_eq!(
        distribution.status, 201,
        "case creating the distribution: expected HTTP 201, got {} with {}",
        distribution.status, distribution.raw
    );

    let identity = call(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-access-identity/cloudfront"),
        Some(&origin_access_identity_xml("oac-readability-oai")),
        None,
    )
    .await;
    assert_eq!(
        identity.status, 201,
        "case creating the origin access identity: expected HTTP 201, got {} with {}",
        identity.status, identity.raw
    );

    let key = create_key(
        router,
        &Key {
            case: "the public key these rows name".to_string(),
            caller_reference: "oac-readability-key".to_string(),
            name: "oac-readability-key".to_string(),
            encoded_key: pem(1, 'A'),
            comment: None,
        }
        .xml(),
    )
    .await;
    assert_eq!(
        key.status, 201,
        "case creating the public key: expected HTTP 201, got {} with {}",
        key.status, key.raw
    );

    Live {
        control_etag: created.header("etag").to_string(),
        control_document,
        control_id,
        distribution_id: distribution.root().text_at("Id").unwrap_or_default().to_string(),
        origin_access_identity_id: identity.root().text_at("Id").unwrap_or_default().to_string(),
        public_key_id: key.root().text_at("Id").unwrap_or_default().to_string(),
    }
}

/// Assert one refusal: its status, its code and — the point of this suite — its message.
fn assert_refusal(answer: &Answer, case: &str, message: &str) {
    assert_eq!(
        answer.status, 404,
        "case {case}: expected HTTP 404, got {} with {}",
        answer.status, answer.raw
    );
    assert_eq!(
        answer.error_code(),
        "NoSuchOriginAccessControl",
        "case {case}: expected NoSuchOriginAccessControl, got {}",
        answer.raw
    );
    let actual = answer.root().find_text("Message").unwrap_or_default();
    assert_eq!(
        actual, message,
        "case {case}: expected the sentence {message:?}, got {actual:?}"
    );
}

/// The identifiers this account holds as a resource of another kind of the shared form: each
/// was issued for that kind, so an origin access control route cannot read it.
fn issued_for_another_kind(live: &Live) -> Vec<(String, String)> {
    vec![
        (
            "a distribution's own minted identifier".to_string(),
            live.distribution_id.clone(),
        ),
        (
            "an origin access identity's own minted identifier".to_string(),
            live.origin_access_identity_id.clone(),
        ),
    ]
}

/// The values nothing of the shared form answers to, which are therefore the control reported
/// absent: the rows vary the namespace letter, the body width and the resource kind, and
/// include a kind of this service whose identifiers are drawn from ANOTHER form (a public key),
/// which this route is not the reader of.
fn no_resource_answers_to(live: &Live) -> Vec<(String, String)> {
    let held = &live.control_id;
    let body = &held[1..];
    let mut changed: Vec<char> = held.chars().collect();
    if let Some(last) = changed.last_mut() {
        *last = if *last == 'A' { 'B' } else { 'A' };
    }
    let mut short = held.clone();
    short.pop();

    vec![
        (
            "the control's own identifier with its last character changed".to_string(),
            changed.into_iter().collect(),
        ),
        (
            "an identifier of the shared form that was never minted".to_string(),
            format!("E{}", "0".repeat(body.len())),
        ),
        (
            "a public key identifier, a kind drawn from another form".to_string(),
            live.public_key_id.clone(),
        ),
        ("a body one character short".to_string(), short),
        (
            "a body one character too long".to_string(),
            format!("{held}Z"),
        ),
        (
            "a far wider body than this service issues".to_string(),
            format!("E{body}{body}"),
        ),
        (
            "a namespace letter no kind of this service mints".to_string(),
            format!("W{body}"),
        ),
        (
            "a body of digits alone".to_string(),
            "0123456789012".to_string(),
        ),
    ]
}

/// The values that are written in no identifier alphabet CloudFront issues one in, which this
/// route therefore cannot read at all: the rows vary HOW the alphabet is broken.
fn outside_the_identifier_alphabet(live: &Live) -> Vec<(String, String)> {
    vec![
        (
            "the control's own identifier in lower case".to_string(),
            live.control_id.to_lowercase(),
        ),
        (
            "a UUID, the namespace the policy kinds are keyed by".to_string(),
            "00000000-0000-0000-0000-000000000000".to_string(),
        ),
        ("a bare word".to_string(), "not-an-identifier".to_string()),
        (
            "the control's caller reference rather than its identifier".to_string(),
            "oac-readability".to_string(),
        ),
        (
            "a domain name rather than an identifier".to_string(),
            "d111111abcdef8.cloudfront.net".to_string(),
        ),
    ]
}

#[tokio::test]
async fn an_identifier_issued_for_another_kind_of_the_shared_form_is_reported_unreadable() {
    let router = emulator();
    let live = live(&router).await;

    for op in OPS {
        for (case, id) in issued_for_another_kind(&live) {
            let answer = op
                .send(&router, &id, &live.control_document, &live.control_etag)
                .await;
            assert_refusal(&answer, &format!("{op:?} with {case} ({id:?})"), UNREADABLE_IDENTIFIER);
        }
    }

    // Every refusal above touched nothing: the control is still readable at its own identifier.
    let read = get_control(&router, &live.control_id).await;
    assert_eq!(
        read.status, 200,
        "case the live control: expected it to survive every refusal, got {} with {}",
        read.status, read.raw
    );
}

#[tokio::test]
async fn a_value_this_service_issues_no_identifier_in_is_reported_unreadable() {
    let router = emulator();
    let live = live(&router).await;

    for op in OPS {
        for (case, id) in outside_the_identifier_alphabet(&live) {
            let answer = op
                .send(&router, &id, &live.control_document, &live.control_etag)
                .await;
            assert_refusal(
                &answer,
                &format!("{op:?} with {case} ({id:?})"),
                UNREADABLE_IDENTIFIER,
            );
        }
    }

    let read = get_control(&router, &live.control_id).await;
    assert_eq!(
        read.status, 200,
        "case the live control: expected it to survive every refusal, got {} with {}",
        read.status, read.raw
    );
}

#[tokio::test]
async fn every_other_identifier_the_store_holds_nothing_under_is_the_absent_control() {
    let router = emulator();
    let live = live(&router).await;

    for op in OPS {
        for (case, id) in no_resource_answers_to(&live) {
            let answer = op
                .send(&router, &id, &live.control_document, &live.control_etag)
                .await;
            assert_refusal(&answer, &format!("{op:?} with {case} ({id:?})"), NO_SUCH_CONTROL);
        }
    }

    // And the control the account really holds is still readable at its own identifier.
    let read = get_control(&router, &live.control_id).await;
    assert_eq!(
        read.status, 200,
        "case the live control: expected it to survive every refusal, got {} with {}",
        read.status, read.raw
    );
}
