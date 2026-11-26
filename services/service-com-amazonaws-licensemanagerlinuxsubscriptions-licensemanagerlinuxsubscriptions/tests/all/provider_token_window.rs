// SPDX-License-Identifier: CC0-1.0

//! The VALIDITY WINDOW of the access token a `RegisterSubscriptionProvider` call names:
//! a registration is judged against the window the token itself declares (`exp` / `nbf`,
//! RFC 7519 §4.1.4/§4.1.5), not against its form alone, at the two instants the credential
//! has to work at.
//!
//! The rules under test:
//!
//! 1. at the instant the credential is PRESENTED: a token already at or past its own
//!    expiry can be made usable by no later instant, so the registration is refused with
//!    the service's one sentence about a provider token it cannot use (HTTP 400,
//!    `ValidationException`) and nothing is created;
//! 2. at the instant the validation running behind an admitted registration falls due —
//!    the instant this service would first present the token: a token whose window covers
//!    it becomes usable (`ACTIVE`, with the subscription data retrieval that status reports
//!    on and nothing to report about the token), and a token whose window does not (its
//!    `nbf` has not been reached) lands on `INVALID` carrying the service's sentence as its
//!    `SubscriptionProviderStatusMessage` and no retrieval time;
//! 3. a token naming NO expiry stays acceptable — an offline refresh token is issued to be
//!    held without one — and so does a claim that is no NumericDate at all, which is no
//!    expiry (`service_common::jwt::CompactJws::numeric_date`: "a claim that is not a JSON
//!    number is no NumericDate and reads as absent");
//! 4. a registration the validation rejected is not stuck: pointed at a secret holding a
//!    token inside its window it is validated again and becomes usable, which is how a
//!    client that reads `INVALID` recovers.
//!
//! Every call goes through the router's `dispatch`, with Secrets Manager mounted beside
//! this service on one engine, so the secret a row stores is the secret the handler reads.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::harness::{
    create_secret, get_provider, harness_with_secrets_manager, is_iso8601_utc, jwt, list_providers,
    listed, member, provider_arn, register, stored_provider_count, token_header, Harness, Reply,
    ACTIVE, INVALID, PENDING, RED_HAT, TOKEN_REFUSAL_MESSAGE,
};

/// The issuer an offline token names: the provider's single sign-on realm.
const SSO_ISSUER: &str = "https://sso.example-provider.com/auth/realms/external";

/// What a registration naming a secret that holds the row's token must do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// Refused at the call itself: nothing is created.
    Refused,
    /// Taken (`PENDING`), with the validation behind it landing on this status.
    Lands(&'static str),
}

/// One row: the window claims the registered offline token declares, and what a
/// registration naming it must do.
struct Case {
    /// How the row reads in an assertion message.
    label: &'static str,
    /// The window claims the token's payload carries, beside the claims that make it an
    /// offline token.
    window: Value,
    expected: Expected,
}

/// Whole seconds since the epoch — the NumericDate form `exp` and `nbf` are written in
/// (RFC 7519 §2).
fn unix_seconds(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or_default()
}

/// An offline token whose payload declares the offline token type, the issuer and `window`:
/// what makes it the credential a registration needs, plus the row's validity claims.
fn offline_token(window: &Value) -> String {
    let mut payload = json!({
        "typ": "Offline",
        "iss": SSO_ISSUER,
        "scope": "offline_access",
    });
    for (claim, value) in window
        .as_object()
        .expect("a row's window claims are a JSON object")
    {
        payload[claim] = value.clone();
    }
    jwt(&token_header(), &payload, false)
}

/// The rows, against `now` in whole seconds: the dimension they differ along is WHERE the
/// token's window sits relative to the instant it is presented and the instant the
/// validation behind the registration falls due.
fn cases(now: i64) -> Vec<Case> {
    vec![
        Case {
            label: "a token naming no expiry at all",
            window: json!({}),
            expected: Expected::Lands(ACTIVE),
        },
        Case {
            label: "a token expiring a day from now",
            window: json!({ "exp": now + 86_400 }),
            expected: Expected::Lands(ACTIVE),
        },
        Case {
            label: "a token valid since an hour ago and expiring in an hour",
            window: json!({ "nbf": now - 3_600, "exp": now + 3_600 }),
            expected: Expected::Lands(ACTIVE),
        },
        Case {
            label: "a token whose expiry claim is no NumericDate",
            window: json!({ "exp": "tomorrow" }),
            expected: Expected::Lands(ACTIVE),
        },
        // The published bound: `exp` is "the expiration time ON OR AFTER which the JWT MUST
        // NOT be accepted for processing" (RFC 7519 §4.1.4), so a token whose expiry is the
        // current second is already past it, while the rows above are inside it.
        Case {
            label: "a token whose expiry is the current second",
            window: json!({ "exp": now }),
            expected: Expected::Refused,
        },
        Case {
            label: "a token that expired an hour ago",
            window: json!({ "exp": now - 3_600 }),
            expected: Expected::Refused,
        },
        Case {
            label: "a token that expired in the first seconds of the epoch",
            window: json!({ "exp": 1 }),
            expected: Expected::Refused,
        },
        // A NumericDate may be fractional (RFC 7519 §2: "Non-integer values can be used"),
        // and this one is still an hour past.
        Case {
            label: "a token whose fractional expiry is an hour past",
            window: json!({ "exp": (now - 3_600) as f64 + 0.5 }),
            expected: Expected::Refused,
        },
        Case {
            label: "a token that is not valid until an hour from now",
            window: json!({ "nbf": now + 3_600, "exp": now + 86_400 }),
            expected: Expected::Lands(INVALID),
        },
        Case {
            label: "a token that is not valid until a day from now",
            window: json!({ "nbf": now + 86_400, "exp": now + 172_800 }),
            expected: Expected::Lands(INVALID),
        },
    ]
}

/// Read the registration back until the validation behind it has landed, and answer the
/// reply that reported its terminal status.
///
/// The emulator never sleeps for that transition — it records the instant it falls due and
/// materializes it on the next call that reports a status — so the test LOOKS AGAIN until
/// the status is terminal instead of waiting a fixed time, under a deadline far above the
/// span a registration owes.
async fn terminal_report(h: &Harness, arn: &str, case: &str) -> Reply {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let reply = get_provider(h, arn).await;
        let status = member(&reply, case, "SubscriptionProviderStatus")
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no SubscriptionProviderStatus in {:?}",
                    reply.body
                )
            })
            .to_string();
        if status != PENDING {
            return reply;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the registration still reports {PENDING}: the validation behind \
             it never landed"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The `SubscriptionProviderStatusMessage` a 200 body reports, or `None` when it carries
/// none.
fn status_message(reply: &Reply, case: &str) -> Option<String> {
    match reply.ok(case).get("SubscriptionProviderStatusMessage") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the status message must be a string, got {value:?}")
                })
                .to_string(),
        ),
    }
}

/// The `LastSuccessfulDataRetrievalTime` a 200 body reports, or `None`.
fn retrieval_time(reply: &Reply, case: &str) -> Option<String> {
    match reply.ok(case).get("LastSuccessfulDataRetrievalTime") {
        None | Some(Value::Null) => None,
        Some(value) => Some(
            value
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the retrieval time must be a string, got {value:?}")
                })
                .to_string(),
        ),
    }
}

/// Rules 1, 2 and 3 over the whole table.
#[tokio::test]
async fn the_registered_tokens_own_window_decides_the_registration() {
    let now = unix_seconds(SystemTime::now());
    for (index, case) in cases(now).into_iter().enumerate() {
        // An emulator per row: the account holds ONE registration per source, so a row
        // whose registration is taken must be the call that creates it.
        let h = harness_with_secrets_manager();
        let label = case.label;
        let secret_arn = create_secret(
            &h,
            &format!("provider-window-{index:02}"),
            &json!({ "SecretString": offline_token(&case.window) }),
        )
        .await;

        let reply = register(
            &h,
            &json!({ "SubscriptionProviderSource": RED_HAT, "SecretArn": secret_arn }),
        )
        .await;

        match case.expected {
            Expected::Refused => {
                reply.refused(label, (400, "ValidationException", TOKEN_REFUSAL_MESSAGE));
                assert_eq!(
                    stored_provider_count(&h),
                    0,
                    "case {label:?}: a refused registration must create nothing"
                );
                let listing = list_providers(&h, &json!({})).await;
                assert!(
                    listed(&listing, label).is_empty(),
                    "case {label:?}: a refused registration must leave the list empty, got {:?}",
                    listing.body
                );
            }
            Expected::Lands(expected_status) => {
                let arn = provider_arn(&reply, label);
                assert_eq!(
                    member(&reply, label, "SubscriptionProviderStatus"),
                    Some(PENDING),
                    "case {label:?}: a taken registration reports the status of the \
                     registration action, got {:?}",
                    reply.body
                );

                let settled = terminal_report(&h, &arn, label).await;
                assert_eq!(
                    member(&settled, label, "SubscriptionProviderStatus"),
                    Some(expected_status),
                    "case {label:?}: expected {expected_status}, got {:?}",
                    settled.body
                );
                let message = status_message(&settled, label);
                let retrieved = retrieval_time(&settled, label);
                if expected_status == ACTIVE {
                    assert_eq!(
                        message, None,
                        "case {label:?}: a usable registration has nothing to report about \
                         its token, got {message:?}"
                    );
                    let retrieved = retrieved.unwrap_or_else(|| {
                        panic!(
                            "case {label:?}: a usable registration reports the retrieval that \
                             status is about, got {:?}",
                            settled.body
                        )
                    });
                    assert!(
                        is_iso8601_utc(&retrieved),
                        "case {label:?}: the retrieval time must be ISO-8601 UTC, got \
                         {retrieved:?}"
                    );
                } else {
                    assert_eq!(
                        message.as_deref(),
                        Some(TOKEN_REFUSAL_MESSAGE),
                        "case {label:?}: a rejected token must be reported, got {message:?}"
                    );
                    assert_eq!(
                        retrieved, None,
                        "case {label:?}: no retrieval can have happened with a token the \
                         provider does not honour, got {retrieved:?}"
                    );
                }
            }
        }
    }
}

/// Rule 4: the registration a rejected token left `INVALID` is reported that way by the
/// listing too, and pointing it at a secret holding a token inside its window validates it
/// again and makes it usable.
#[tokio::test]
async fn a_rejected_registration_becomes_usable_when_pointed_at_a_live_token() {
    let h = harness_with_secrets_manager();
    let now = unix_seconds(SystemTime::now());
    let case = "a registration re-pointed from a token not yet valid to a live one";

    let rejected_secret = create_secret(
        &h,
        "provider-window-not-yet-valid",
        &json!({ "SecretString": offline_token(&json!({ "nbf": now + 3_600, "exp": now + 86_400 })) }),
    )
    .await;
    let arn = provider_arn(
        &register(
            &h,
            &json!({ "SubscriptionProviderSource": RED_HAT, "SecretArn": rejected_secret }),
        )
        .await,
        case,
    );

    let settled = terminal_report(&h, &arn, case).await;
    assert_eq!(
        member(&settled, case, "SubscriptionProviderStatus"),
        Some(INVALID),
        "case {case:?}: a token the validation does not honour leaves the registration \
         {INVALID}, got {:?}",
        settled.body
    );
    // The listing projects the same stored registration, so it reports the same verdict.
    let listing = list_providers(&h, &json!({})).await;
    let rows = listed(&listing, case);
    assert_eq!(
        rows.iter()
            .map(|(_, _, _, status)| status.as_str())
            .collect::<Vec<&str>>(),
        vec![INVALID],
        "case {case:?}: the listing must report the registration {INVALID}, got {:?}",
        listing.body
    );

    // The same source, pointed at another secret: the stored credential is replaced, so
    // the registration is validated again.
    let live_secret = create_secret(
        &h,
        "provider-window-live",
        &json!({ "SecretString": offline_token(&json!({ "exp": now + 86_400 })) }),
    )
    .await;
    let again = register(
        &h,
        &json!({ "SubscriptionProviderSource": RED_HAT, "SecretArn": live_secret }),
    )
    .await;
    assert_eq!(
        provider_arn(&again, case),
        arn,
        "case {case:?}: a registration of the same source addresses the registration already \
         made"
    );
    assert_eq!(
        member(&again, case, "SubscriptionProviderStatus"),
        Some(PENDING),
        "case {case:?}: a replaced credential puts the registration back in {PENDING}, got {:?}",
        again.body
    );

    let revalidated = terminal_report(&h, &arn, case).await;
    assert_eq!(
        member(&revalidated, case, "SubscriptionProviderStatus"),
        Some(ACTIVE),
        "case {case:?}: a token inside its window makes the registration usable, got {:?}",
        revalidated.body
    );
    assert_eq!(
        status_message(&revalidated, case),
        None,
        "case {case:?}: the rejected token's message must not outlive it, got {:?}",
        revalidated.body
    );
    assert!(
        retrieval_time(&revalidated, case).is_some(),
        "case {case:?}: a usable registration reports the retrieval that status is about, got \
         {:?}",
        revalidated.body
    );
    assert_eq!(
        stored_provider_count(&h),
        1,
        "case {case:?}: the account holds one registration per source"
    );
}
