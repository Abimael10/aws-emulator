// SPDX-License-Identifier: CC0-1.0

//! The window a `GetWebIdentityToken` call asks for, against the published bounds of the
//! member that carries it.
//!
//! RULE: a request whose `DurationSeconds` falls inside the published range of its own
//! member is served a token whose validity window is exactly that many seconds — the
//! operation's documented default when the request names no duration — and a request whose
//! value falls outside that range is refused at the boundary with HTTP 400 and the
//! `ValidationError` code, stating the value as submitted, the member path it sits at and
//! the bound it broke, and vending no token at all.
//!
//! The rows differ along what the rule generalises over: the duration asked for (absent,
//! each published bound, one inside each bound, one between them, one past each bound, and
//! a value far past the maximum, including zero and a negative), with the window read off
//! the token's OWN `iat`/`exp` claims so the assertion is exact rather than a function of
//! how fast the host runs. Every served row is verified against the key set the account's
//! issuer publishes, so a row that is served is one a relying party could accept.

use super::outbound_web_identity_token::{
    borrowed, enable_federation, published_key_set, token_params,
};
use super::sts_sessions::{
    call, element, emulator_with_iam, member, timestamp_secs, LONG_TERM_KEY,
};

/// The operation under test.
const ACTION: &str = "GetWebIdentityToken";

/// The algorithm every row below signs under: one of the two the operation admits
/// (`smithy_query(query="doc",shape="GetWebIdentityTokenRequest",member="SigningAlgorithm")`).
/// Which algorithm signs a token is a rule of its own, pinned beside this one; here it is
/// held fixed so each row varies only in the duration it asks for.
const ALGORITHM: &str = "RS256";

/// The audience every row names. One is inside the member's own `@length` (1..10).
const AUDIENCE: &str = "https://relying.party/duration";

/// The published bounds of `webIdentityTokenDurationSecondsType`, the member's target
/// (`smithy_query(query="operation",operation="GetWebIdentityToken")` reports range
/// 60..3600).
const MIN_DURATION_SECONDS: i64 = 60;
const MAX_DURATION_SECONDS: i64 = 3600;

/// The window a request that names no duration gets: "If not specified, the default duration
/// is 300 seconds (5 minutes)"
/// (`smithy_query(query="doc",shape="GetWebIdentityTokenRequest",member="DurationSeconds")`).
const DEFAULT_DURATION_SECONDS: i64 = 300;

/// The error code AWS answers a value it will not accept with, on every protocol.
const VALIDATION_ERROR: &str = "ValidationError";

/// The member path a clause about this member names: the member's own name in lowerCamel,
/// which is how AWS spells a path in a validation message.
const DURATION_MEMBER_PATH: &str = "durationSeconds";

/// What a row expects of the call.
enum Expected {
    /// Served, with a validity window of exactly this many seconds.
    Window(i64),
    /// Refused at the boundary, stating the bound the submitted value broke.
    Outside(Bound),
}

/// Which published bound a refused row broke.
enum Bound {
    /// Below the member's minimum.
    Below(i64),
    /// Above the member's maximum.
    Above(i64),
}

/// The sentence AWS states one broken constraint in: the envelope counting the violations,
/// then the value as submitted, the member path and the requirement it broke.
fn range_clause(submitted: i64, bound: &Bound) -> String {
    let requirement = match bound {
        Bound::Below(min) => format!("Member must have value greater than or equal to {min}"),
        Bound::Above(max) => format!("Member must have value less than or equal to {max}"),
    };
    format!(
        "1 validation error detected: Value '{submitted}' at '{DURATION_MEMBER_PATH}' \
         failed to satisfy constraint: {requirement}"
    )
}

#[tokio::test]
async fn a_token_lasts_exactly_the_duration_its_request_asks_for_inside_the_published_range() {
    let (router, _engine) = emulator_with_iam();
    let issuer = enable_federation(&router).await;
    let key_set = published_key_set(&router, &issuer).await;

    let rows: Vec<(String, Option<i64>, Expected)> = vec![
        (
            "no duration at all".to_string(),
            None,
            Expected::Window(DEFAULT_DURATION_SECONDS),
        ),
        (
            "the published minimum".to_string(),
            Some(MIN_DURATION_SECONDS),
            Expected::Window(MIN_DURATION_SECONDS),
        ),
        (
            "one second inside the minimum".to_string(),
            Some(MIN_DURATION_SECONDS + 1),
            Expected::Window(MIN_DURATION_SECONDS + 1),
        ),
        (
            "half an hour, between the bounds".to_string(),
            Some(1_800),
            Expected::Window(1_800),
        ),
        (
            "one second inside the maximum".to_string(),
            Some(MAX_DURATION_SECONDS - 1),
            Expected::Window(MAX_DURATION_SECONDS - 1),
        ),
        (
            "the published maximum".to_string(),
            Some(MAX_DURATION_SECONDS),
            Expected::Window(MAX_DURATION_SECONDS),
        ),
        (
            "one second past the minimum".to_string(),
            Some(MIN_DURATION_SECONDS - 1),
            Expected::Outside(Bound::Below(MIN_DURATION_SECONDS)),
        ),
        (
            "zero seconds".to_string(),
            Some(0),
            Expected::Outside(Bound::Below(MIN_DURATION_SECONDS)),
        ),
        (
            "a negative duration".to_string(),
            Some(-1),
            Expected::Outside(Bound::Below(MIN_DURATION_SECONDS)),
        ),
        (
            "one second past the maximum".to_string(),
            Some(MAX_DURATION_SECONDS + 1),
            Expected::Outside(Bound::Above(MAX_DURATION_SECONDS)),
        ),
        (
            "a whole day, far past the maximum".to_string(),
            Some(86_400),
            Expected::Outside(Bound::Above(MAX_DURATION_SECONDS)),
        ),
    ];

    for (case, duration, expected) in rows {
        let params = token_params(&[AUDIENCE], ALGORITHM, duration);
        let (status, body) = call(&router, LONG_TERM_KEY, ACTION, &borrowed(&params)).await;

        match expected {
            Expected::Window(seconds) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected 200, got {status} {body}"
                );
                let token = member(&case, &body, "WebIdentityToken");
                let verified =
                    service_common::jwt::verify(&token, &key_set).unwrap_or_else(|defect| {
                        panic!(
                            "case {case:?}: the token does not verify against the published \
                             key set: {defect}"
                        )
                    });
                let issued_at = verified.numeric_date("iat").unwrap_or_else(|| {
                    panic!("case {case:?}: no iat claim in {:?}", verified.claims)
                });
                let expires = verified.numeric_date("exp").unwrap_or_else(|| {
                    panic!("case {case:?}: no exp claim in {:?}", verified.claims)
                });
                assert_eq!(
                    expires - issued_at,
                    seconds,
                    "case {case:?}: expected a window of {seconds} seconds, got {}",
                    expires - issued_at
                );
                assert_eq!(
                    timestamp_secs(&case, &member(&case, &body, "Expiration")),
                    expires,
                    "case {case:?}: expected the reported Expiration to be the token's own \
                     exp claim: {body}"
                );
            }
            Expected::Outside(bound) => {
                let submitted = duration.expect("a refused row names a duration");
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400, got {status} {body}"
                );
                assert_eq!(
                    element(&body, "Code").as_deref(),
                    Some(VALIDATION_ERROR),
                    "case {case:?}: expected the {VALIDATION_ERROR} code, got {body}"
                );
                assert_eq!(
                    element(&body, "Message").as_deref(),
                    Some(range_clause(submitted, &bound).as_str()),
                    "case {case:?}: expected the clause naming the bound it broke, got {body}"
                );
                assert_eq!(
                    element(&body, "WebIdentityToken"),
                    None,
                    "case {case:?}: a refused request was vended a token: {body}"
                );
            }
        }
    }
}
