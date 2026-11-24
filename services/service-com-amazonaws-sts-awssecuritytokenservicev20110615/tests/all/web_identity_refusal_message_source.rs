// SPDX-License-Identifier: CC0-1.0

//! Where a `GetWebIdentityToken` refusal takes its `Message` from.
//!
//! RULE: a refusal answers with the wire contract its error shape declares — the status and
//! awsQuery code the MODEL gives that shape — and with the `Message` of its own error class:
//! the words the service answers that class with, with only the parts the class interpolates
//! taken from the request or from state. A shape's `@documentation` is not that message: it
//! is API-reference prose written for a reader of the reference, and no refusal may answer
//! with it.
//!
//! What each class's words ARE is pinned beside the rest of the operation in
//! `outbound_web_identity_token.rs`; this suite asserts the two halves that are a property of
//! the model rather than of a text — the status and code come from the shape's own traits, and
//! the `Message` is none of the documented paragraphs the operation's error shapes carry. Both
//! are read from the model on disk through the operation's own `errors` list, so the
//! expectations follow the model instead of a copy of it kept here, and a shape the model adds
//! to the operation later is held to the rule without this suite being edited.

use std::collections::BTreeMap;
use std::path::PathBuf;

use smithy_server_runtime::MultiServiceRouter;

use super::outbound_web_identity_token::{borrowed, enable_federation, token_params};
use super::sts_sessions::{
    call, element, emulator_with_iam, member, seed_demo_role, LONG_TERM_KEY,
};

/// The operation under test.
const ACTION: &str = "GetWebIdentityToken";

/// This service's model: the document every expectation below is read from.
const MODEL: &str = "smithy-model/com-amazonaws-sts-awssecuritytokenservicev20110615.json";

/// The namespace its shapes are named in.
const NAMESPACE: &str = "com.amazonaws.sts";

/// The one relying party every row names: what the rows differ along is the error class, not
/// the audience.
const AUDIENCE: [&str; 1] = ["https://relying.party"];

/// An algorithm the operation signs under, which every row names because none of them is
/// about the algorithm ("Valid values are RS256 (RSA with SHA-256) and ES384 (ECDSA using
/// P-384 curve with SHA-384)",
/// `smithy_query(query="doc",shape="GetWebIdentityTokenRequest",member="SigningAlgorithm")`).
const ADMITTED_ALGORITHM: &str = "RS256";

/// The modelled ceiling on `Tags` (`tagListType`, length 0..50), and the longest a tag's key
/// and value may be (`tagKeyType` length 1..128, `tagValueType` length 0..256) — the members
/// whose size drives the token payload.
const MAX_TAGS: usize = 50;
const MAX_TAG_KEY_CHARS: usize = 128;
const MAX_TAG_VALUE_CHARS: usize = 256;

/// The duration a row asks a token for when its refusal is not about the duration: the floor
/// of the member's published range (`webIdentityTokenDurationSecondsType`, range 60..3600).
const MIN_DURATION_SECONDS: i64 = 60;

/// How long the session the escalation row calls with is granted, and the duration that row
/// then asks a token for: inside the member's own range, but past what is left of the session.
const GRANTED_SESSION_SECONDS: i64 = 900;
const ESCALATING_DURATION_SECONDS: i64 = 3_600;

/// What a row sets up, which is how it provokes its refusal.
#[derive(Clone, Copy, Debug)]
enum Provoke {
    /// Leave the account's outbound web identity federation off.
    FederationOff,
    /// Ask, on a session credential, for a token outliving that session.
    OutlastTheSession,
    /// Carry a tag set whose claims pass the token payload budget.
    OversizedTagSet,
}

/// The wire contract one error shape declares: the status and awsQuery code a refusal of that
/// class answers with, and the paragraph the shape is DOCUMENTED with — which is what the
/// `Message` must not be — or `None` for a shape documented with nothing, which leaves the
/// message nothing to be compared against.
#[derive(Debug)]
struct ModelledError {
    status: u16,
    code: String,
    documented: Option<String>,
}

/// RULE: each refusal answers with the status and code its shape declares, and with a
/// `Message` that is none of the prose the operation's error shapes are documented with.
///
/// Rows span the three conditions the operation refuses — one per modelled error shape, which
/// is what the rule generalises over — each driven end to end over the wire.
#[tokio::test]
async fn every_refusal_answers_its_modelled_contract_and_not_the_models_prose() {
    let modelled = modelled_errors();

    // One row: the case, what it provokes, and the error shape it is refused by.
    let rows: [(&str, Provoke, &str); 3] = [
        (
            "the account has outbound web identity federation off",
            Provoke::FederationOff,
            "OutboundWebIdentityFederationDisabledException",
        ),
        (
            "a token outliving the session that asked for it",
            Provoke::OutlastTheSession,
            "SessionDurationEscalationException",
        ),
        (
            "fifty tags at the longest key and value the model admits",
            Provoke::OversizedTagSet,
            "JWTPayloadSizeExceededException",
        ),
    ];

    for (case, provoke, shape) in rows {
        let expected = modelled
            .get(shape)
            .unwrap_or_else(|| panic!("case {case:?}: {ACTION} does not declare {shape}"));

        // One account per row, so what decides each refusal is only what that row set up.
        let (router, engine) = emulator_with_iam();
        if !matches!(provoke, Provoke::FederationOff) {
            enable_federation(&router).await;
        }
        let (caller, params) = match provoke {
            Provoke::FederationOff => (
                LONG_TERM_KEY.to_string(),
                token_params(&AUDIENCE, ADMITTED_ALGORITHM, Some(MIN_DURATION_SECONDS)),
            ),
            Provoke::OutlastTheSession => (
                assumed_role_session(&router, &engine, case).await,
                token_params(
                    &AUDIENCE,
                    ADMITTED_ALGORITHM,
                    Some(ESCALATING_DURATION_SECONDS),
                ),
            ),
            Provoke::OversizedTagSet => {
                let mut params =
                    token_params(&AUDIENCE, ADMITTED_ALGORITHM, Some(MIN_DURATION_SECONDS));
                for at in 1..=MAX_TAGS {
                    params.push((
                        format!("Tags.member.{at}.Key"),
                        format!("{at:02}{}", "k".repeat(MAX_TAG_KEY_CHARS - 2)),
                    ));
                    params.push((
                        format!("Tags.member.{at}.Value"),
                        "v".repeat(MAX_TAG_VALUE_CHARS),
                    ));
                }
                (LONG_TERM_KEY.to_string(), params)
            }
        };

        // The status and the code are the shape's own, read from the model rather than written
        // here.
        let (status, body) = call(&router, &caller, ACTION, &borrowed(&params)).await;
        assert_eq!(
            (status, element(&body, "Code").as_deref()),
            (expected.status, Some(expected.code.as_str())),
            "case {case:?}: expected {} {}, got {status} {body}",
            expected.status,
            expected.code
        );
        assert!(
            element(&body, "WebIdentityToken").is_none(),
            "case {case:?}: a refused call must vend no token: {body}"
        );

        // The message is the error class's own words: it says something, and it is not the
        // prose ANY of the operation's error shapes is documented with. Both sides of that
        // comparison are read as PROSE ([`as_prose`]), so a documented paragraph served with
        // its markup or with its original whitespace is still recognised as that paragraph.
        let message = member(case, &body, "Message");
        assert!(
            !message.trim().is_empty(),
            "case {case:?}: a refusal must say what it refused: {body}"
        );
        let as_read = as_prose(&message);
        for (documented_by, declared) in &modelled {
            let Some(documented) = &declared.documented else {
                continue;
            };
            assert_ne!(
                &as_read, documented,
                "case {case:?}: the Message is the documented prose of {documented_by}, which \
                 is API-reference text and not what this error class answers with: got \
                 {message:?}"
            );
        }
    }
}

/// Mint a real assumed-role session lasting [`GRANTED_SESSION_SECONDS`] and report the access
/// key id it was issued under, so the ceiling the escalation row is measured against is one
/// `sts:AssumeRole` really granted.
async fn assumed_role_session(
    router: &MultiServiceRouter,
    engine: &engine::InMemoryEngine,
    case: &str,
) -> String {
    let (role_arn, _role_id) = seed_demo_role(engine);
    let (status, body) = call(
        router,
        LONG_TERM_KEY,
        "AssumeRole",
        &[
            ("RoleArn", role_arn),
            ("RoleSessionName", "probe".to_string()),
            ("DurationSeconds", GRANTED_SESSION_SECONDS.to_string()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: AssumeRole must succeed, got {status} {body}"
    );
    member(case, &body, "AccessKeyId")
}

/// The markup a documented paragraph is written in, compiled ONCE: `@documentation` is HTML,
/// and what a reader sees is the text left when the tags are taken out.
static MARKUP: ::std::sync::LazyLock<::regex::Regex> = ::std::sync::LazyLock::new(|| {
    ::regex::Regex::new("<[^>]*>").expect("a well-formed markup pattern")
});

/// `text` as the PROSE a reader sees: its markup taken out and the runs of space that leaves
/// collapsed. Markup is what makes a documented paragraph a DOCUMENT, so reading both the
/// paragraph and the message this way is what makes the comparison about the TEXT rather than
/// about how it was marked up.
fn as_prose(text: &str) -> String {
    MARKUP
        .replace_all(text, " ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The wire contract of every error shape `ACTION` declares, keyed by shape name: the status
/// and awsQuery code its traits give it, and its documented paragraph as prose ([`as_prose`])
/// — exactly what a reader of the API reference sees.
fn modelled_errors() -> BTreeMap<String, ModelledError> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(MODEL);
    let text = ::std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    let model: ::serde_json::Value = ::serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("{} is not JSON: {error}", path.display()));
    let shapes = &model["shapes"];
    let errors = shapes[format!("{NAMESPACE}#{ACTION}")]["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("{ACTION} declares no errors in {}", path.display()))
        .clone();

    let modelled: BTreeMap<String, ModelledError> = errors
        .iter()
        .map(|error| {
            let target = error["target"]
                .as_str()
                .unwrap_or_else(|| panic!("an error of {ACTION} names no shape: {error}"));
            let traits = &shapes[target]["traits"];
            let query_error = &traits["aws.protocols#awsQueryError"];
            // The name of the shape is the code a refusal of that class carries when the
            // shape overrides it with none of its own.
            let name = target.split_once('#').map_or(target, |(_, name)| name);
            let code = query_error["code"].as_str().unwrap_or(name).to_string();
            let status = query_error["httpResponseCode"]
                .as_u64()
                .or_else(|| traits["smithy.api#httpError"].as_u64())
                .unwrap_or_else(|| panic!("{target} declares no refusal status"));
            let status = u16::try_from(status)
                .unwrap_or_else(|_| panic!("{target} declares {status} as its status"));
            // A shape documented with nothing has no paragraph to be answered with, so there
            // is nothing for the message to be compared against.
            let documented = traits["smithy.api#documentation"]
                .as_str()
                .map(as_prose)
                .filter(|prose| !prose.is_empty());
            (
                name.to_string(),
                ModelledError {
                    status,
                    code,
                    documented,
                },
            )
        })
        .collect();
    assert!(
        !modelled.is_empty(),
        "{ACTION} must declare the errors it refuses with in {}",
        path.display()
    );
    modelled
}
