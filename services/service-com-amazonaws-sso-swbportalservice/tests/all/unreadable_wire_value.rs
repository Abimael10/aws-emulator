// SPDX-License-Identifier: CC0-1.0

//! What this service answers a request whose WIRE VALUE the modelled member it is bound to
//! cannot be read from — a value that never becomes a value at all, so no rule of the model
//! has anything to judge.
//!
//! RULE: such a request, from an AUTHENTICATED caller, is refused with the error this
//! service's model declares for a problem with the input to the request —
//! `InvalidRequestException`, HTTP 400, the same error every other request this service
//! will not read earns (an absent `@required` parameter, a value outside a declared
//! constraint, a page token it never issued) — and never with an error shape the model does
//! not declare, which is what a restJson1 SDK resolves the error from and so all it can map
//! onto a modelled error. The sentence names the member, in the lowerCamel path this
//! service's refusals name a member at, and the modelled type the value had to be one of.
//!
//! The rows span what the rule generalises over:
//!
//! * the FORM of the unreadable text — a word, a decimal, scientific notation, a value past
//!   the modelled integer's width, a malformed sign, a radix prefix, text around a readable
//!   number, and the parameter stated with no value at all;
//! * the operations that bind a non-string member on the wire: `max_result`
//!   (`@httpQuery("max_result")` over `MaxResultType`, an `integer` — this service's only
//!   non-string wire-bound member, as `smithy_query(query="operation",
//!   operation="ListAccounts,ListAccountRoles")` gives it) is modelled on both listings;
//! * the OTHER side of the rule: a text that IS a value of the member's type is read and
//!   then judged by the model's own `@range`, so a bound inside it is served and one
//!   outside it is refused with the framework's own clause about that range — both under
//!   the same modelled code, so the two classes are not told apart by the error a client
//!   branches on;
//! * precedence: the same unreadable request presenting no usable session stays the
//!   portal's authentication refusal, because this service authenticates before it reads.

use http::StatusCode;
use service_com_amazonaws_sso_swbportalservice::state::INVALID_SESSION_MESSAGE;

use crate::portal_session::{assert_refusal, body_text, error_code, error_message, Portal};

/// The error this service's model declares for a problem with the input to the request, a
/// service-common error of all four bound operations
/// (`smithy_query(query="service_info")`'s `common_errors`).
const INVALID_REQUEST: &str = "InvalidRequestException";

/// The account the role listing is asked for: one the fixture really holds, so a row that
/// is not refused by the rule under test is served out of state.
const KNOWN_ACCOUNT: &str = "111111111111";

/// The sign-in the readable rows are served to.
const ACCESS_TOKEN: &str = "tok-unreadable";

/// The two routes that bind `max_result`, each with the query it needs to name a route at
/// all: `(the operation, the path and its required parameters)`.
fn bound_listings() -> Vec<(&'static str, String)> {
    vec![
        ("ListAccounts", "/assignment/accounts".to_string()),
        (
            "ListAccountRoles",
            format!("/assignment/roles?account_id={KNOWN_ACCOUNT}"),
        ),
    ]
}

/// Every way a `max_result` text fails to be a value of the member's modelled type: none of
/// these is an integer the member can hold, so none of them ever becomes a value with a
/// range to check.
fn unreadable_bounds() -> Vec<(&'static str, String)> {
    vec![
        ("a word", "not-a-number".to_string()),
        ("a decimal", "1.5".to_string()),
        ("a whole number written with a point", "5.0".to_string()),
        ("scientific notation", "1e2".to_string()),
        ("a radix prefix", "0x10".to_string()),
        ("a malformed sign", "--1".to_string()),
        ("a trailing sign", "1-".to_string()),
        ("digits with text after them", "5roles".to_string()),
        ("a thousands separator", "1%2C000".to_string()),
        // One past the modelled `integer`'s width, and far past it: a number no `integer`
        // holds is no more readable than a word.
        (
            "one past the modelled integer width",
            (i64::from(i32::MAX) + 1).to_string(),
        ),
        (
            "far past the modelled integer width",
            "99999999999999999999".to_string(),
        ),
        (
            "one below the modelled integer width",
            (i64::from(i32::MIN) - 1).to_string(),
        ),
        // Stated with no value at all, and with a value made of space: neither is a text
        // the member's type can be read from.
        ("stated with no value", String::new()),
        ("a value made of space", "%20".to_string()),
    ]
}

/// The URI for `listing` carrying `max_result=<value>`.
fn uri_of(listing: &str, value: &str) -> String {
    let separator = if listing.contains('?') { '&' } else { '?' };
    format!("{listing}{separator}max_result={value}")
}

/// The sentence this service states for the page bound: the envelope over the clause that
/// names the member — `maxResults`, the lowerCamel path a refusal of this service names a
/// member at — and the modelled type the value had to be one of (`MaxResultType` is an
/// `integer`).
const UNREADABLE_BOUND_MESSAGE: &str = "1 validation error detected: Value at 'maxResults' \
                                        failed to satisfy constraint: Member must be a \
                                        valid Integer";

/// RULE: a text the modelled member's type cannot be read from is refused 400
/// `InvalidRequestException`, with the sentence naming that member — on every operation
/// that binds the member, for every form of unreadable text.
#[tokio::test]
async fn a_value_the_modelled_type_cannot_be_read_from_is_an_invalid_request() {
    let portal = Portal::new();
    portal.sign_in(ACCESS_TOKEN, &[KNOWN_ACCOUNT]);

    for (operation, listing) in bound_listings() {
        for (form, value) in unreadable_bounds() {
            let case = format!("{operation} with {form} as max_result ({value:?})");
            let response = portal.get(&uri_of(&listing, &value), Some(ACCESS_TOKEN)).await;
            assert_refusal(
                &case,
                &response,
                StatusCode::BAD_REQUEST,
                INVALID_REQUEST,
                UNREADABLE_BOUND_MESSAGE,
            );
        }
    }

    // The sweep spent nothing: the session is still served afterwards, and nothing of a
    // listing reached any refusal above (`assert_refusal` pins the whole body).
    let served = portal.get("/assignment/accounts", Some(ACCESS_TOKEN)).await;
    assert_eq!(
        served.status(),
        StatusCode::OK,
        "case \"a live session after every refusal\": expected 200, got {} {:?}",
        served.status(),
        body_text(&served)
    );
}

/// RULE (the other side): a text that IS a value of the member's type is READ, and then
/// judged by the `@range` the model declares on it — so a bound inside the range is served
/// and one outside it is refused under the SAME modelled code, with the framework's own
/// clause about the range rather than this service's unreadable-value sentence.
///
/// The bounds come from the model (`MaxResultType`, range 1..100): each published bound,
/// one past each of them, and a value in between.
#[tokio::test]
async fn a_readable_bound_is_judged_by_the_modelled_range() {
    let portal = Portal::new();
    portal.sign_in(ACCESS_TOKEN, &[KNOWN_ACCOUNT]);

    // (case, the text sent, whether the request is served)
    let rows: &[(&str, &str, bool)] = &[
        ("the published minimum", "1", true),
        ("inside the range", "2", true),
        ("the published maximum", "100", true),
        ("one below the published minimum", "0", false),
        ("one past the published maximum", "101", false),
        ("a negative bound", "-1", false),
        // A readable integer at each end of the modelled `integer` domain: read, then
        // refused by the range.
        ("the smallest integer the member holds", "-2147483648", false),
        ("the largest integer the member holds", "2147483647", false),
    ];

    for (operation, listing) in bound_listings() {
        for (case, value, served) in rows {
            let case = format!("{operation} with {case} as max_result ({value:?})");
            let response = portal.get(&uri_of(&listing, value), Some(ACCESS_TOKEN)).await;
            if *served {
                assert_eq!(
                    response.status(),
                    StatusCode::OK,
                    "case {case:?}: a bound inside the modelled range is served, got {} {:?}",
                    response.status(),
                    body_text(&response)
                );
                continue;
            }
            assert_eq!(
                (response.status(), error_code(&response)),
                (StatusCode::BAD_REQUEST, INVALID_REQUEST.to_string()),
                "case {case:?}: a bound outside the modelled range is refused as an invalid \
                 request, got {} {:?}",
                response.status(),
                body_text(&response)
            );
            // The value WAS read, so the sentence is about the range it broke and not
            // about the type it could not be read as.
            let stated = error_message(&response);
            assert!(
                stated.contains("at 'maxResults' failed to satisfy constraint: Member must have \
                                 value")
                    && stated != UNREADABLE_BOUND_MESSAGE,
                "case {case:?}: expected the clause about the modelled range, got {stated:?}"
            );
        }
    }
}

/// RULE (precedence): this service authenticates before it reads, so the same unreadable
/// request presenting no usable session is still the portal's one authentication refusal —
/// the unreadable value is answered only once the caller is authenticated.
#[tokio::test]
async fn an_unreadable_value_is_read_only_after_the_caller_is_authenticated() {
    let portal = Portal::new();
    portal.sign_in(ACCESS_TOKEN, &[KNOWN_ACCOUNT]);

    // Every class of credential that names no usable session: none at all, an empty
    // header, and a token that was never issued.
    let unusable: &[(&str, Option<&str>)] = &[
        ("no bearer-token header at all", None),
        ("an empty header value", Some("")),
        ("a token that was never issued", Some("tok-never-issued")),
    ];

    for (operation, listing) in bound_listings() {
        for (form, value) in unreadable_bounds() {
            let uri = uri_of(&listing, &value);
            for (class, credential) in unusable {
                let case = format!("{operation} with {form} as max_result, and {class}");
                let response = portal.get(&uri, *credential).await;
                assert_refusal(
                    &case,
                    &response,
                    StatusCode::UNAUTHORIZED,
                    "UnauthorizedException",
                    INVALID_SESSION_MESSAGE,
                );
            }
        }
    }
}
