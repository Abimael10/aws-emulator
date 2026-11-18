// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the two SENTENCES `ConfirmSubscription` tells its two refusals apart
//! with.
//!
//! The RULE asserted here: `ConfirmSubscription` refuses two different conditions under one
//! status and one error code (HTTP 400 / `InvalidParameter`), and the MESSAGE is what
//! distinguishes them:
//!
//! * a `Token` that is not of a confirmation token's FORM — a run of exactly
//!   [`CONFIRMATION_TOKEN_HEX_CHARS`] hexadecimal characters, which is what a confirmation
//!   token is and nothing else is — faults the request member, and is refused with the bare
//!   parameter sentence `Invalid parameter: Token`;
//! * a `Token` OF that form which matches no pending confirmation of the topic named faults
//!   the token: an expired, superseded or foreign confirmation link, refused with the wholly
//!   fixed `Invalid token`.
//!
//! Neither sentence takes any part from the request: the token is a short-lived secret sent to
//! the endpoint, so it is never echoed and no reason clause is read off it.
//!
//! The rows differ along what the rule generalises over: the FORM of the value presented as a
//! token (a bare word, one character, whitespace, a URL, a run of one letter at one digest's
//! width, non-hexadecimal characters at a token's width, one character short of and one past
//! that width, an ARN), and — on the other side of the boundary — tokens OF the form that
//! confirm nothing (a run of one hexadecimal digit at a token's width, the same in upper case,
//! another topic's real token, and a real token with one character changed). No refused call
//! confirms anything.
//!
//! Every call goes over the wire through the router the emulator serves in production,
//! so nothing but persisted state carries information between calls.

use service_com_amazonaws_sns_amazonsimplenotificationservice::state::CONFIRMATION_TOKEN_HEX_CHARS;

use super::topics_subscriptions_publish::{Answer, Sns};

/// The sentence AWS refuses a token that confirms nothing with, stated here as the
/// recorded response words it rather than read from the crate, so the test pins the
/// text and not just the constant.
const INVALID_TOKEN: &str = "Invalid token";

/// The sentence AWS refuses a value that is not of a confirmation token's form with — its
/// bare `Invalid parameter: <member>` frame, whose only variable part is the member name the
/// model spells `Token`. Stated here as the recorded response words it, for the same reason.
const INVALID_PARAMETER_TOKEN: &str = "Invalid parameter: Token";

/// Subscribe `endpoint` over `protocol`, asking for the ARN in every case.
async fn subscribe(sns: &Sns, topic: &str, protocol: &str, endpoint: &str) -> String {
    let answer = sns
        .call(
            "Subscribe",
            &[
                ("TopicArn", topic),
                ("Protocol", protocol),
                ("Endpoint", endpoint),
                ("ReturnSubscriptionArn", "true"),
            ],
        )
        .await;
    assert_eq!(
        answer.status, 200,
        "Subscribe over {protocol} must be served, got {} / {}",
        answer.status, answer.body
    );
    answer
        .element("SubscriptionArn")
        .unwrap_or_else(|| panic!("no SubscriptionArn in {}", answer.body))
}

/// The confirmation token the subscription under `arn` holds, read from persisted state
/// because AWS sends it to the endpoint and no response carries it.
fn token_of(sns: &Sns, arn: &str) -> String {
    let state = sns.state();
    let guard = state.lock();
    guard.subscriptions[arn]
        .confirmation_token
        .clone()
        .expect("a subscription awaiting confirmation holds a token")
}

/// One `ConfirmSubscription` call.
async fn confirm(sns: &Sns, topic: &str, token: &str) -> Answer {
    sns.call(
        "ConfirmSubscription",
        &[("TopicArn", topic), ("Token", token)],
    )
    .await
}

/// Whether the subscription under `arn` is still awaiting confirmation.
fn pending(sns: &Sns, arn: &str) -> bool {
    sns.state().lock().subscriptions[arn].pending_confirmation
}

/// Every value that is not a confirmation token of the topic named is refused with the
/// sentence its own condition carries, and nothing is confirmed.
#[tokio::test]
async fn the_two_token_refusals_carry_their_own_sentences() {
    let sns = Sns::new();
    let topic = sns.topic("invalid-token-refusal").await;
    let other = sns.topic("invalid-token-other-topic").await;

    // One awaiting subscription per confirmable protocol, so the refusal does not
    // depend on which subscriber has to confirm.
    let awaiting = [
        subscribe(&sns, &topic, "https", "https://example.test/hook").await,
        subscribe(&sns, &topic, "http", "http://example.test/hook").await,
        subscribe(&sns, &topic, "email", "ops@example.test").await,
    ];
    // A subscription of ANOTHER topic: its token is a real token, for a topic this call
    // does not name.
    let foreign_token = {
        let elsewhere = subscribe(&sns, &other, "https", "https://elsewhere.test/hook").await;
        token_of(&sns, &elsewhere)
    };
    let elsewhere = subscribe(&sns, &other, "http", "http://elsewhere.test/hook").await;

    // A real token with ONE character changed: of the form, and the value of nothing.
    let altered = {
        let mut characters: Vec<char> = token_of(&sns, &awaiting[0]).chars().collect();
        characters[0] = if characters[0] == '0' { '1' } else { '0' };
        characters.into_iter().collect::<String>()
    };
    // One digest's worth of hexadecimal characters: far short of a token's width, which is
    // the width this service mints and SNS's own tokens carry.
    let one_digest_wide = "a".repeat(64);
    let token_width = "a".repeat(CONFIRMATION_TOKEN_HEX_CHARS);
    let token_width_upper = "ABCDEF0123456789".repeat(CONFIRMATION_TOKEN_HEX_CHARS / 16);
    let one_short = "f".repeat(CONFIRMATION_TOKEN_HEX_CHARS - 1);
    let one_past = "f".repeat(CONFIRMATION_TOKEN_HEX_CHARS + 1);
    let not_hexadecimal = "z".repeat(CONFIRMATION_TOKEN_HEX_CHARS);

    // (case, the value presented as a token, the sentence the condition carries)
    let rows: Vec<(&str, String, &str)> = vec![
        ("a bare word", "nope".to_string(), INVALID_PARAMETER_TOKEN),
        ("one character", "a".to_string(), INVALID_PARAMETER_TOKEN),
        ("whitespace", " ".to_string(), INVALID_PARAMETER_TOKEN),
        (
            "a value shaped like a URL",
            "https://example.test/?Token=x".to_string(),
            INVALID_PARAMETER_TOKEN,
        ),
        (
            "one digest's worth of hexadecimal characters",
            one_digest_wide,
            INVALID_PARAMETER_TOKEN,
        ),
        (
            "one character short of a token's width",
            one_short,
            INVALID_PARAMETER_TOKEN,
        ),
        (
            "one character past a token's width",
            one_past,
            INVALID_PARAMETER_TOKEN,
        ),
        (
            "a token's width of characters that are not hexadecimal",
            not_hexadecimal,
            INVALID_PARAMETER_TOKEN,
        ),
        (
            "the subscription's own ARN",
            awaiting[0].clone(),
            INVALID_PARAMETER_TOKEN,
        ),
        (
            "a token's width of one hexadecimal digit",
            token_width,
            INVALID_TOKEN,
        ),
        (
            "a token's width in upper-case hexadecimal",
            token_width_upper,
            INVALID_TOKEN,
        ),
        (
            "the token of another topic's subscription",
            foreign_token,
            INVALID_TOKEN,
        ),
        (
            "a real token with one character changed",
            altered,
            INVALID_TOKEN,
        ),
    ];

    for (case, token, expected) in &rows {
        let answer = confirm(&sns, &topic, token).await;
        assert_eq!(
            answer.status, 400,
            "case {case:?}: expected HTTP 400, got {} / {}",
            answer.status, answer.body
        );
        assert_eq!(
            answer.code(),
            "InvalidParameter",
            "case {case:?}: expected the InvalidParameter code, got {}",
            answer.body
        );
        assert_eq!(
            answer.element("Message").as_deref(),
            Some(*expected),
            "case {case:?}: expected exactly {expected:?}, got {}",
            answer.body
        );
        assert!(
            answer.element("SubscriptionArn").is_none(),
            "case {case:?}: a refused confirmation must report no subscription, got {}",
            answer.body
        );
        assert!(
            // A one- or few-character value is letters that occur in the sentence itself;
            // what must never be reproduced is the token-shaped secret the caller presented.
            token.chars().count() < 20 || !answer.body.contains(token.as_str()),
            "case {case:?}: the refusal must not echo the token, got {}",
            answer.body
        );
    }

    for arn in &awaiting {
        assert!(
            pending(&sns, arn),
            "the refused confirmations must leave {arn} awaiting confirmation"
        );
    }
    assert!(
        pending(&sns, &elsewhere),
        "the refused confirmations must leave the other topic's subscription awaiting \
         confirmation"
    );
    // Neither refusal created a subscription: the topic holds exactly the ones subscribed.
    let listed = sns
        .call("ListSubscriptionsByTopic", &[("TopicArn", &topic)])
        .await;
    assert_eq!(
        listed.elements("SubscriptionArn").len(),
        awaiting.len(),
        "expected the topic to hold exactly the subscribed endpoints, got {}",
        listed.body
    );

    // The control: the token the service minted for one of them confirms THAT
    // subscription, so the refusals above are about the token and not about the call.
    let token = token_of(&sns, &awaiting[0]);
    let answer = confirm(&sns, &topic, &token).await;
    assert_eq!(
        answer.status, 200,
        "the minted token must confirm the subscription, got {} / {}",
        answer.status, answer.body
    );
    assert_eq!(
        answer.element("SubscriptionArn").as_deref(),
        Some(awaiting[0].as_str()),
        "the confirmation must report the subscription's ARN, got {}",
        answer.body
    );
    assert!(
        !pending(&sns, &awaiting[0]),
        "the confirmed subscription must no longer be awaiting confirmation"
    );
}

/// A token this service minted is of the form it judges by, so the form judgement never
/// refuses a real token: every minted token is a run of hexadecimal characters of the one
/// width, and each confirms its own subscription.
#[tokio::test]
async fn a_minted_token_is_of_the_form_the_refusal_judges_by() {
    let sns = Sns::new();
    let topic = sns.topic("minted-token-form").await;

    for (protocol, endpoint) in &[
        ("https", "https://example.test/one"),
        ("http", "http://example.test/two"),
        ("email", "ops@example.test"),
        ("email-json", "alerts@example.test"),
    ] {
        let case = format!("{protocol} -> {endpoint}");
        let arn = subscribe(&sns, &topic, protocol, endpoint).await;
        let token = token_of(&sns, &arn);
        assert_eq!(
            token.chars().count(),
            CONFIRMATION_TOKEN_HEX_CHARS,
            "case {case:?}: expected a {CONFIRMATION_TOKEN_HEX_CHARS}-character token, got {}",
            token.chars().count()
        );
        assert!(
            token.chars().all(|character| character.is_ascii_hexdigit()),
            "case {case:?}: expected a hexadecimal token, got {token:?}"
        );
        let answer = confirm(&sns, &topic, &token).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: the minted token must confirm its subscription, got {} / {}",
            answer.status, answer.body
        );
        assert!(
            !pending(&sns, &arn),
            "case {case:?}: the confirmed subscription must no longer be pending"
        );
    }
}
