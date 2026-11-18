// SPDX-License-Identifier: CC0-1.0

//! WHEN the response-headers-policy surface evaluates the `If-Match` header, relative to the
//! store, end to end over HTTP.
//!
//! Every assertion goes through the router, so each case exercises routing, restXml
//! (de)serialization, the handler, the persisted state and serialization — the same path a
//! client takes. One rule, asserted on both conditional mutations of the surface
//! (`DeleteResponseHeadersPolicy` and `UpdateResponseHeadersPolicy`):
//!
//! * what the `If-Match` header CARRIES is a property of the request alone — no policy any
//!   identifier could name turns a header that holds no version token into one that holds a
//!   version — so a header that carries none is `InvalidIfMatchVersion` (HTTP 400, the
//!   service's fixed sentence for that code) BEFORE the identifier is resolved against the
//!   store, identically for an id that names a stored policy and for one that names nothing;
//! * once the header DOES carry a version token, the store decides: an id this account holds
//!   no policy for is `NoSuchResponseHeadersPolicy` (404), a token that is not the stored
//!   policy's current one is `PreconditionFailed` (412), and the current token is served.
//!
//! The rows differ along what the rule generalises over: every form a header takes that
//! carries no version token (absent, empty, a value that is no token at all, a token spelled
//! in lower case, a token too short to be one, a quoted non-token, the weak form of one),
//! crossed with the identifiers the request can address (the policy this run really created,
//! ids of four shapes the store holds nothing for, and an id this surface refuses to mutate at
//! all), crossed with the two operations. The contrast rows — the same identifiers under a
//! readable token — are what make the rule about the HEADER rather than about absence.
//!
//! The one refusal that still precedes the header is the request's own: a policy AWS published
//! is not this account's to mutate, which is decided from the published catalogue rather than
//! from the store, so it answers `IllegalDelete`/`IllegalUpdate` whatever the header carries —
//! asserted here over every entry of that catalogue so the two orderings cannot drift.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s, the policy writer and reader the
//! response headers policy suites' and the delete's wire path
//! [`crate::response_headers_policy_delete`]'s — reused rather than copied.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::MANAGED_RESPONSE_HEADERS_POLICIES;
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::response_headers_policy_delete::delete_policy;
use crate::response_headers_policy_lifecycle::{create_policy, get_policy, Policy};

/// The sentences CloudFront sends for these refusals, spelled out here rather than taken from
/// the crate, so a reworded refusal fails this suite.
const INVALID_IF_MATCH_VERSION_MESSAGE: &str =
    "The If-Match version is missing or not valid for the resource.";
const NO_SUCH_POLICY_MESSAGE: &str = "The specified response headers policy does not exist.";
/// The sentence this surface answers an identifier that is in no policy id's shape at all
/// with: the same code as the absent policy, told apart by the message alone.
const ID_FORMAT_MESSAGE: &str = "Id format is invalid";

/// Which conditional mutation of the surface a row drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Delete,
    Update,
}

impl Operation {
    /// The status this operation answers a request it SERVES with.
    fn served_status(self) -> u16 {
        match self {
            Operation::Delete => 204,
            Operation::Update => 200,
        }
    }

    /// The code this surface refuses a mutation of an AWS-published policy with.
    fn illegal_code(self) -> &'static str {
        match self {
            Operation::Delete => "IllegalDelete",
            Operation::Update => "IllegalUpdate",
        }
    }
}

/// `PUT /2020-05-31/response-headers-policy/{Id}` with a `ResponseHeadersPolicyConfig` payload
/// under the version token `if_match` — the route, the payload member and the header the model
/// binds `UpdateResponseHeadersPolicy` to.
async fn update_policy(
    router: &MultiServiceRouter,
    id: &str,
    body: &str,
    if_match: Option<&str>,
) -> Answer {
    call(
        router,
        "PUT",
        &format!("/{API_VERSION}/response-headers-policy/{id}"),
        Some(body),
        if_match,
    )
    .await
}

/// Drive `operation` for `id` under the header `if_match`, submitting `config` where the
/// operation states one.
async fn mutate(
    router: &MultiServiceRouter,
    operation: Operation,
    id: &str,
    config: &Policy,
    if_match: Option<&str>,
) -> Answer {
    match operation {
        Operation::Delete => delete_policy(router, id, if_match).await,
        Operation::Update => update_policy(router, id, &config.xml(), if_match).await,
    }
}

/// Create one policy and report its minted id and current version token.
async fn create(router: &MultiServiceRouter, row: &Policy) -> (String, String) {
    let answer = create_policy(router, &row.xml()).await;
    assert_eq!(
        answer.status, 201,
        "case {:?}: expected HTTP 201 from CreateResponseHeadersPolicy, got {} with {}",
        row.case, answer.status, answer.raw
    );
    (
        answer.root().text_at("Id").unwrap_or_default().to_string(),
        answer.header("etag").to_string(),
    )
}

/// Every form an `If-Match` takes that carries NO version token: `(case, header)`.
///
/// A version token is what this service's `ETag` header reports, so a value of any other form —
/// including one differing from a token only in case or width — carries none, and an absent
/// header carries none either (`IfMatch` is an optional member of both operations).
fn headers_carrying_no_token() -> Vec<(&'static str, Option<&'static str>)> {
    vec![
        ("no If-Match at all", None),
        ("an empty If-Match", Some("")),
        ("a value that is no version token", Some("not-a-version")),
        ("a token spelled in lower case", Some("e1stale1version")),
        ("a value too short to be a token", Some("E1")),
        ("a quoted value that is no token", Some("\"not-a-version\"")),
        ("the weak form of a value that is no token", Some("W/\"nope\"")),
    ]
}

/// The identifiers the store holds no policy of this account for, in the shapes a client can
/// send: `(case, value, the sentence the absent-policy code carries)`.
///
/// `minted` is the id this run's create really handed out and `name` the policy's own name, so
/// the rows are built from this run's record rather than from literals.
fn ids_naming_nothing(minted: &str, name: &str) -> Vec<(String, String, &'static str)> {
    vec![
        (
            "an id never minted".to_string(),
            "f9a6a0b4-8d39-4a1e-9f67-1b0f7f5d2c3e".to_string(),
            NO_SUCH_POLICY_MESSAGE,
        ),
        (
            "the minted id upper-cased".to_string(),
            minted.to_uppercase(),
            ID_FORMAT_MESSAGE,
        ),
        (
            "a value that is no policy id".to_string(),
            "not-a-policy-id".to_string(),
            ID_FORMAT_MESSAGE,
        ),
        (
            "the policy's own name".to_string(),
            name.to_string(),
            ID_FORMAT_MESSAGE,
        ),
    ]
}

/// The policy a row addresses when it addresses one that exists.
fn subject() -> Policy {
    Policy {
        case: "the policy the ordering is asserted around",
        name: "response-headers-policy-precondition-order".to_string(),
        comment: Some("the stored configuration".to_string()),
        ..Policy::default()
    }
}

/// The configuration a refused update submits — never stored, since every row below is refused
/// or is the one row that is served.
fn submitted() -> Policy {
    Policy {
        case: "the configuration the refused mutations submitted",
        name: "response-headers-policy-precondition-order".to_string(),
        comment: Some("submitted under the header rows".to_string()),
        ..Policy::default()
    }
}

/// A header carrying no version token is refused on the HEADER, whatever the identifier names:
/// the stored policy, an id of any shape the store holds nothing for — and every refusal leaves
/// the stored policy exactly as it was.
#[tokio::test]
async fn a_header_carrying_no_version_token_is_refused_before_the_store() {
    let router = emulator();
    let row = subject();
    let (id, etag) = create(&router, &row).await;
    let config = submitted();

    for operation in [Operation::Delete, Operation::Update] {
        for (header_case, if_match) in headers_carrying_no_token() {
            let mut targets: Vec<(String, String)> = vec![("the stored policy".to_string(), id.clone())];
            for (case, candidate, _) in ids_naming_nothing(&id, &row.name) {
                targets.push((case, candidate));
            }

            for (target_case, target) in targets {
                let case = format!("{operation:?}/{target_case}/{header_case}");
                let answer = mutate(&router, operation, &target, &config, if_match).await;
                assert_eq!(
                    answer.status, 400,
                    "case {case}: expected HTTP 400 for {target:?}, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    "InvalidIfMatchVersion",
                    "case {case}: expected InvalidIfMatchVersion for {target:?}, got {:?} with {}",
                    answer.error_code(),
                    answer.raw
                );
                let served = answer.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    served, INVALID_IF_MATCH_VERSION_MESSAGE,
                    "case {case}: expected {INVALID_IF_MATCH_VERSION_MESSAGE:?} for {target:?}, \
                     got {served:?} in {}",
                    answer.raw
                );
            }
        }
    }

    // Not one of those refusals touched the stored policy: it reads back at the version its
    // create minted, with the configuration the create persisted.
    let fetched = get_policy(&router, &id).await;
    assert_eq!(
        fetched.status, 200,
        "case after the header refusals: expected the policy to stay readable, got {} with {}",
        fetched.status, fetched.raw
    );
    assert_eq!(
        fetched.header("etag"),
        etag,
        "case after the header refusals: expected the version token {etag:?} to be untouched, \
         got {:?}",
        fetched.header("etag")
    );
    assert_eq!(
        fetched.root().find_text("Comment"),
        row.comment.as_deref(),
        "case after the header refusals: expected the stored configuration to be untouched, got \
         {}",
        fetched.raw
    );
}

/// Once the header carries a version token the STORE decides: an id naming no policy of this
/// account is the absent-policy refusal (whose sentence tells an unreadable identifier from a
/// well-formed one that names nothing), a token that is not the stored policy's is the failed
/// precondition, and the current token is served.
#[tokio::test]
async fn a_readable_token_hands_the_decision_to_the_store() {
    let row = subject();
    let config = submitted();

    for operation in [Operation::Delete, Operation::Update] {
        // A fresh emulator per operation, so the row that is SERVED cannot change what the
        // other operation sees.
        let router = emulator();
        let (id, etag) = create(&router, &row).await;
        let other = Policy {
            case: "another policy, whose token is not the subject's",
            name: "response-headers-policy-precondition-order-other".to_string(),
            ..Policy::default()
        };
        let (_, other_etag) = create(&router, &other).await;

        // An identifier naming nothing, under a token that IS readable: the store answers.
        for (target_case, target, sentence) in ids_naming_nothing(&id, &row.name) {
            for (token_case, token) in [
                ("the stored policy's current token", etag.as_str()),
                ("another policy's current token", other_etag.as_str()),
            ] {
                let case = format!("{operation:?}/{target_case}/{token_case}");
                let answer = mutate(&router, operation, &target, &config, Some(token)).await;
                assert_eq!(
                    answer.status, 404,
                    "case {case}: expected HTTP 404 for {target:?}, got {} with {}",
                    answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    "NoSuchResponseHeadersPolicy",
                    "case {case}: expected NoSuchResponseHeadersPolicy for {target:?}, got {:?} \
                     with {}",
                    answer.error_code(),
                    answer.raw
                );
                let served = answer.root().find_text("Message").unwrap_or_default();
                assert_eq!(
                    served, sentence,
                    "case {case}: expected {sentence:?} for {target:?}, got {served:?} in {}",
                    answer.raw
                );
            }
        }

        // The stored policy under a well-formed token that is not its own: the precondition
        // EVALUATED, and failed.
        let case = format!("{operation:?}/the stored policy/another policy's current token");
        let stale = mutate(&router, operation, &id, &config, Some(&other_etag)).await;
        assert_eq!(
            stale.status, 412,
            "case {case}: expected HTTP 412, got {} with {}",
            stale.status, stale.raw
        );
        assert_eq!(
            stale.error_code(),
            "PreconditionFailed",
            "case {case}: expected PreconditionFailed, got {:?} with {}",
            stale.error_code(),
            stale.raw
        );

        // And the stored policy under its own current token is SERVED, so no refusal above
        // came from the ordering refusing a call it must answer.
        let case = format!("{operation:?}/the stored policy/its own current token");
        let answer = mutate(&router, operation, &id, &config, Some(&etag)).await;
        assert_eq!(
            answer.status,
            operation.served_status(),
            "case {case}: expected HTTP {}, got {} with {}",
            operation.served_status(),
            answer.status,
            answer.raw
        );
    }
}

/// The one refusal ahead of the header: a policy AWS published is not this account's to mutate,
/// which is decided from the published catalogue rather than from the store, so it is the answer
/// whatever the header carries. Asserted over every entry of that catalogue.
#[tokio::test]
async fn a_published_policy_is_refused_ahead_of_the_header() {
    let router = emulator();
    let config = submitted();

    for operation in [Operation::Delete, Operation::Update] {
        for managed in MANAGED_RESPONSE_HEADERS_POLICIES {
            for (header_case, if_match) in headers_carrying_no_token() {
                let case = format!("{operation:?}/{}/{header_case}", managed.name);
                let answer = mutate(&router, operation, managed.id, &config, if_match).await;
                assert_eq!(
                    answer.status, 400,
                    "case {case}: expected HTTP 400 for {:?}, got {} with {}",
                    managed.id, answer.status, answer.raw
                );
                assert_eq!(
                    answer.error_code(),
                    operation.illegal_code(),
                    "case {case}: expected {} for {:?}, got {:?} with {}",
                    operation.illegal_code(),
                    managed.id,
                    answer.error_code(),
                    answer.raw
                );
            }

            // The published policy is still readable afterwards: nothing above removed or
            // rewrote what AWS published.
            let read = get_policy(&router, managed.id).await;
            assert_eq!(
                read.status, 200,
                "case {operation:?}/{}: expected the published policy to stay readable, got {} \
                 with {}",
                managed.name, read.status, read.raw
            );
        }
    }
}
