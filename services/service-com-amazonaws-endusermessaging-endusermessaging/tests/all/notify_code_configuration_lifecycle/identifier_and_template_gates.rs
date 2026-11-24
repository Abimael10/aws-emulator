// SPDX-License-Identifier: CC0-1.0

//! The two gates every notify-code-configuration operation applies to a value a request
//! supplies, over the wire.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch` (this suite's parent
//! harness), so it reads the rendered restJson1 body a real SDK client would parse, and
//! each gate is driven on EVERY operation that accepts the member it judges — the
//! identifier on `GetNotifyCodeConfiguration`, `UpdateNotifyCodeConfiguration` and
//! `SendNotifyCodeVerification`, the channel parameters on
//! `CreateNotifyCodeConfiguration`, `UpdateNotifyCodeConfiguration` and that same send.
//!
//! The rules under test, each over a SET of inputs rather than one recorded request:
//!
//! * a `NotifyCodeConfigurationIdOrArn` is CLASSIFIED before any lookup: a value that is
//!   neither a bare identifier of this resource nor the ARN of one can name a
//!   configuration in no state, so it is `ValidationException` (400) stating the format
//!   failure against the value given, while a well-formed identifier that reaches no
//!   record is `ResourceNotFoundException` (404);
//! * the refusal is ahead of the state write: neither the stored configuration nor the
//!   set of configurations changes behind one;
//! * an `inlineTemplateBody` a request supplies for a freeform channel must carry the
//!   code placeholder the passcode is rendered at, or it is `ValidationException` (400)
//!   — on a create, on an update and on a send's channel overrides, for the text channel
//!   and for the voice one — and a body that carries it is stored and reported back;
//! * an update's empty body is this API's clear sentinel rather than a template, so it
//!   is served and clears the stored one.

use serde_json::{json, Value};

use super::{Emulator, Reply, ACCOUNT_ID, CONFIGURATION_TYPE, SIGNING_NAME};

/// The sentence AWS refuses a template body carrying no code placeholder with, entirely
/// fixed text.
const TEMPLATE_WITHOUT_A_CODE: &str = "template body must contain {{code}} placeholder";

/// A destination a send addresses, inside `DestinationIdentity`'s `@length(1..20)`.
const RECIPIENT: &str = "+12065550100";

/// The origination identity a send is made from.
const SENDER: &str = "+12065550000";

/// `UpdateNotifyCodeConfiguration` over the wire: a `PUT` on the configuration the
/// identifier names.
async fn update(em: &Emulator, id_or_arn: &str, body: Value) -> Reply {
    em.call(
        "PUT",
        &format!("/v1/notify-code-configurations/{id_or_arn}"),
        Some(body),
    )
    .await
}

/// `SendNotifyCodeVerification` over the wire.
async fn send(em: &Emulator, body: Value) -> Reply {
    em.call("POST", "/v1/notify-code-verifications/send", Some(body))
        .await
}

/// How many configurations the store reports, so a refused call can be shown to have
/// created none.
async fn configuration_count(em: &Emulator) -> usize {
    let listed = em.call("GET", "/v1/notify-code-configurations", None).await;
    listed.body["notifyCodeConfigurations"]
        .as_array()
        .unwrap_or_else(|| panic!("notifyCodeConfigurations is not a list in {}", listed.body))
        .len()
}

/// A configuration carrying a template on both freeform channels, so a case that
/// changes or refuses one member proves the stored ones survived it.
fn subject() -> Value {
    json!({
        "notifyCodeConfigurationName": "gate-subject",
        "codeConfigurationParameters": { "codeType": "NUMERIC", "codeLength": 6 },
        "channelParameters": {
            "text": { "inlineTemplateBody": "Your code is {{code}}" },
            "voice": { "inlineTemplateBody": "Your code is {{code}}", "voiceId": "Joanna" },
        },
    })
}

/// The identifiers that are NOT identifiers of a notify code configuration, each inside
/// `NotifyCodeConfigurationIdOrArn`'s own `@length(1..256)` and `^[A-Za-z0-9_:/-]+$`
/// pattern so the value reaches the gate rather than the boundary constraint walk.
///
/// The rows differ along what the rule generalises over: the form the value takes (bare
/// value or ARN), which part of it is wrong, and how. The bare rows carry no resource
/// prefix, another resource's prefix, a prefix with no body, a body past
/// `NotifyCodeConfigurationId`'s `@length(1..64)`, and punctuation that shape's
/// `^[A-Za-z0-9_-]+$` excludes; the ARN rows name another vendor, another resource type
/// of this service, and a resource of the right type whose id is not one.
fn malformed_identifiers(name: &str) -> Vec<(&'static str, String)> {
    vec![
        ("the configuration's name", name.to_string()),
        ("a bare value with no resource prefix", "x".to_string()),
        (
            "another resource's prefix",
            "bp-00000000000000000".to_string(),
        ),
        ("the prefix with no body", "ncc-".to_string()),
        (
            "a body one past the bare identifier's maximum length",
            format!("ncc-{}", "z".repeat(61)),
        ),
        (
            "punctuation a bare identifier excludes",
            "ncc-a:b/c".to_string(),
        ),
        (
            "an ARN of another vendor",
            format!("arn:aws:sns:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/ncc-1"),
        ),
        (
            "an ARN of another resource type",
            format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:brand-profile/bp-00000000000000000"),
        ),
        (
            "an ARN whose resource id is not an identifier",
            format!("arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/not-an-id"),
        ),
    ]
}

/// Well-formed identifiers of this resource that reach no stored record: the 404 class
/// the gate above must not swallow.
fn unknown_identifiers() -> Vec<(&'static str, String)> {
    vec![
        ("a minted-shaped id", "ncc-00000000000000000".to_string()),
        ("the shortest body", "ncc-1".to_string()),
        (
            "a body at the bare identifier's maximum length",
            format!("ncc-{}", "z".repeat(60)),
        ),
        (
            "an ARN never issued",
            format!(
                "arn:aws:{SIGNING_NAME}:us-east-1:{ACCOUNT_ID}:{CONFIGURATION_TYPE}/ncc-11111111111111111"
            ),
        ),
    ]
}

/// Refuse the request as `ValidationException` (400) and state the message given.
fn refused_as_validation(case: &str, reply: &Reply, expected: &str, member: &str) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected 400, got {} with {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error.as_deref(),
        Some("ValidationException"),
        "case {case:?}: expected ValidationException, got {:?} with {}",
        reply.error,
        reply.body
    );
    assert_eq!(
        reply.top("message"),
        expected,
        "case {case:?}: expected message {expected:?}, got {}",
        reply.body
    );
    assert_eq!(
        reply.body["fieldList"][0]["path"].as_str(),
        Some(member),
        "case {case:?}: expected fieldList to name {member:?}, got {}",
        reply.body
    );
}

#[tokio::test]
async fn a_value_that_is_no_identifier_is_refused_before_the_lookup_on_every_operation() {
    let em = Emulator::new();
    let created = em.create(subject()).await;
    assert_eq!(
        created.status, 200,
        "the subject create expected 200, got {} with {}",
        created.status, created.body
    );
    let id = created.string("notifyCodeConfigurationId");
    let name = created.string("notifyCodeConfigurationName");
    let stored = created.resource().clone();
    let before = configuration_count(&em).await;

    for (case, identifier) in malformed_identifiers(&name) {
        let expected = format!("invalid notify code configuration ID format: {identifier}");

        let read = em.get(&identifier).await;
        refused_as_validation(
            &format!("{case} read back"),
            &read,
            &expected,
            "notifyCodeConfigurationId",
        );

        let written = update(
            &em,
            &identifier,
            json!({ "notifyCodeConfigurationName": "renamed-by-a-bad-identifier" }),
        )
        .await;
        refused_as_validation(
            &format!("{case} updated"),
            &written,
            &expected,
            "notifyCodeConfigurationId",
        );

        let sent = send(
            &em,
            json!({
                "channel": "TEXT",
                "destinationIdentity": RECIPIENT,
                "originationIdentity": SENDER,
                "notifyCodeConfiguration": identifier,
            }),
        )
        .await;
        refused_as_validation(
            &format!("{case} sent to"),
            &sent,
            &expected,
            "notifyCodeConfiguration",
        );
    }

    // Every refusal was ahead of the state write: the stored configuration and the set
    // of configurations are both what they were.
    let read_back = em.get(&id).await;
    assert_eq!(
        read_back.resource(),
        &stored,
        "the refused calls must leave the configuration unchanged, got {}",
        read_back.body
    );
    let after = configuration_count(&em).await;
    assert_eq!(
        after, before,
        "the refused calls must create nothing: expected {before} configurations, got {after}"
    );
}

#[tokio::test]
async fn a_well_formed_identifier_that_reaches_no_record_is_still_a_404() {
    let em = Emulator::new();
    // One live configuration, so a miss is a miss rather than an empty store.
    em.create(subject()).await;

    for (case, identifier) in unknown_identifiers() {
        for (operation, reply) in [
            ("read back", em.get(&identifier).await),
            (
                "updated",
                update(
                    &em,
                    &identifier,
                    json!({ "notifyCodeConfigurationName": "renamed" }),
                )
                .await,
            ),
            (
                "sent to",
                send(
                    &em,
                    json!({
                        "channel": "TEXT",
                        "destinationIdentity": RECIPIENT,
                        "originationIdentity": SENDER,
                        "notifyCodeConfiguration": identifier,
                    }),
                )
                .await,
            ),
        ] {
            assert_eq!(
                reply.status, 404,
                "case {case:?} {operation}: expected 404, got {} with {}",
                reply.status, reply.body
            );
            assert_eq!(
                reply.error.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case:?} {operation}: expected ResourceNotFoundException, got {:?} with {}",
                reply.error,
                reply.body
            );
        }
    }
}

/// The template bodies a freeform channel can carry that render NO passcode: the rows
/// differ in how the placeholder is missing — absent altogether, spelled with one brace
/// pair, spaced inside the braces, named without braces, and a body that is only the
/// other channel's wording.
fn bodies_without_a_code() -> Vec<(&'static str, &'static str)> {
    vec![
        ("no placeholder at all", "Your sign-in code"),
        ("single braces", "Your code is {code}"),
        ("spaces inside the braces", "Your code is {{ code }}"),
        ("the word alone", "Your code: code"),
        ("another template's placeholder", "Your code is {{passcode}}"),
    ]
}

/// The template bodies that DO render a passcode: the placeholder alone, inside a
/// sentence, more than once, and beside other text in each position.
fn bodies_with_a_code() -> Vec<(&'static str, &'static str)> {
    vec![
        ("the placeholder alone", "{{code}}"),
        ("inside a sentence", "Your code is {{code}}, hurry"),
        ("twice", "{{code}} — again: {{code}}"),
        ("at the end", "Sign-in code {{code}}"),
    ]
}

/// The freeform channels an `inlineTemplateBody` arrives on, with the `channel` a send
/// delivering over each one names.
const FREEFORM_CHANNELS: [(&str, &str); 2] = [("text", "TEXT"), ("voice", "VOICE")];

#[tokio::test]
async fn a_supplied_template_body_without_the_code_placeholder_is_refused_on_every_operation() {
    let em = Emulator::new();
    let created = em.create(subject()).await;
    assert_eq!(
        created.status, 200,
        "the subject create expected 200, got {} with {}",
        created.status, created.body
    );
    let id = created.string("notifyCodeConfigurationId");
    let stored = created.resource()["channelParameters"].clone();
    let before = configuration_count(&em).await;

    for (channel, wire_channel) in FREEFORM_CHANNELS {
        for (what, body) in bodies_without_a_code() {
            let case = format!("{channel}: {what}");
            let member = format!("/channelParameters/{channel}/inlineTemplateBody");

            let minted = em
                .create(json!({
                    "notifyCodeConfigurationName": "template-gate",
                    "channelParameters": { channel: { "inlineTemplateBody": body } },
                }))
                .await;
            refused_as_validation(
                &format!("{case} created"),
                &minted,
                TEMPLATE_WITHOUT_A_CODE,
                &member,
            );

            let written = update(
                &em,
                &id,
                json!({ "channelParameters": { channel: { "inlineTemplateBody": body } } }),
            )
            .await;
            refused_as_validation(
                &format!("{case} updated"),
                &written,
                TEMPLATE_WITHOUT_A_CODE,
                &member,
            );

            let sent = send(
                &em,
                json!({
                    "channel": wire_channel,
                    "destinationIdentity": RECIPIENT,
                    "originationIdentity": SENDER,
                    "overrideChannelParameters": { channel: { "inlineTemplateBody": body } },
                }),
            )
            .await;
            refused_as_validation(
                &format!("{case} sent with"),
                &sent,
                TEMPLATE_WITHOUT_A_CODE,
                &format!("/overrideChannelParameters/{channel}/inlineTemplateBody"),
            );
        }
    }

    // Each refusal was ahead of the state write: the stored channel parameters are what
    // they were, and no configuration was minted by a refused create.
    let read_back = em.get(&id).await;
    assert_eq!(
        read_back.member("channelParameters"),
        &stored,
        "the refused calls must leave the stored channelParameters unchanged, got {}",
        read_back.body
    );
    let after = configuration_count(&em).await;
    assert_eq!(
        after, before,
        "the refused creates must create nothing: expected {before} configurations, got {after}"
    );
}

#[tokio::test]
async fn a_template_body_that_renders_the_code_is_stored_and_reported_back() {
    let em = Emulator::new();

    for (channel, wire_channel) in FREEFORM_CHANNELS {
        for (what, body) in bodies_with_a_code() {
            let case = format!("{channel}: {what}");

            let minted = em
                .create(json!({
                    "notifyCodeConfigurationName": "template-accepted",
                    "channelParameters": { channel: { "inlineTemplateBody": body } },
                }))
                .await;
            assert_eq!(
                minted.status, 200,
                "case {case:?} created: expected 200, got {} with {}",
                minted.status, minted.body
            );
            let id = minted.string("notifyCodeConfigurationId");
            let read_back = em.get(&id).await;
            assert_eq!(
                read_back.member("channelParameters")[channel]["inlineTemplateBody"],
                json!(body),
                "case {case:?} created: the supplied template must be stored, got {}",
                read_back.body
            );

            // The same body replaces a stored one through the update path.
            let replaced = update(
                &em,
                &id,
                json!({
                    "channelParameters": {
                        channel: { "inlineTemplateBody": format!("Updated: {body}") }
                    }
                }),
            )
            .await;
            assert_eq!(
                replaced.status, 200,
                "case {case:?} updated: expected 200, got {} with {}",
                replaced.status, replaced.body
            );
            assert_eq!(
                em.get(&id).await.member("channelParameters")[channel]["inlineTemplateBody"],
                json!(format!("Updated: {body}")),
                "case {case:?} updated: the update must be stored, got {}",
                replaced.body
            );

            let sent = send(
                &em,
                json!({
                    "channel": wire_channel,
                    "destinationIdentity": RECIPIENT,
                    "originationIdentity": SENDER,
                    "overrideChannelParameters": { channel: { "inlineTemplateBody": body } },
                }),
            )
            .await;
            assert_eq!(
                sent.status, 200,
                "case {case:?} sent with: expected 200, got {} with {}",
                sent.status, sent.body
            );
            assert!(
                sent.body["verificationId"].is_string(),
                "case {case:?} sent with: expected a verificationId, got {}",
                sent.body
            );
        }
    }
}

#[tokio::test]
async fn an_update_clears_a_stored_template_with_this_apis_empty_sentinel() {
    let em = Emulator::new();

    for (channel, _) in FREEFORM_CHANNELS {
        let created = em.create(subject()).await;
        let id = created.string("notifyCodeConfigurationId");

        let cleared = update(
            &em,
            &id,
            json!({ "channelParameters": { channel: { "inlineTemplateBody": "" } } }),
        )
        .await;
        assert_eq!(
            cleared.status, 200,
            "case {channel:?}: the clear sentinel is not a template, expected 200, got {} with {}",
            cleared.status, cleared.body
        );
        let read_back = em.get(&id).await;
        assert!(
            read_back.member("channelParameters")[channel]["inlineTemplateBody"].is_null(),
            "case {channel:?}: the cleared body must no longer be reported, got {}",
            read_back.body
        );
    }
}
