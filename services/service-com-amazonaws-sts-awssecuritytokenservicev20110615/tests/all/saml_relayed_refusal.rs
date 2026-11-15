// SPDX-License-Identifier: CC0-1.0

//! Who refused a presented SAML authentication response, as `AssumeRoleWithSAML` reports it.
//!
//! RULE: `AssumeRoleWithSAML` does not read a presented response itself — it is read by the
//! internal service that holds the account's SAML manifests — so a refusal that reading
//! answered is reported as the fixed sentence of its class followed by the envelope the
//! relaying layer states around a failed call: `(Service: <the service called>; Status Code:
//! <the status it answered>; Error Code: <the exception it named>; Request ID: <that call's
//! own id, minted per call>; Proxy: null)`, the five fields in that order. The exception
//! names which step of that reading refused — looking the provider's manifest up, or reading
//! the presented response against the manifest it found.
//!
//! Three kinds of class carry NO envelope, and state the sentence alone:
//!
//! * the classes this service decides ITSELF — the role the REQUEST asked for against the
//!   pairs a verified response asserts, the validity window it answers its own expiry error
//!   for, and the provider's own report that the authentication failed — because no relayed
//!   call refused them;
//! * the SIGNATURE classes: a response whose signature does not check out, and one whose
//!   declared transforms a verifier will not accept. That refusal is the bare sentence, so a
//!   client telling this class from the others matches a message that is equal every time.
//!
//! The rows below span the class: which part of the presented value was wrong (its encoding,
//! its XML, its SAML-ness, its signature, its issuer, the session it names), whether the
//! manifest was there to read it against at all, and the three classes the service answers
//! on its own. Each row drives the real path end to end on one shared engine — the provider
//! and the role are registered through IAM's own operations and the response is signed here
//! — and the recorded cases (a non-base64 value, a response with its signature removed, a
//! response signed by another key, a response asserting another role, and a deleted provider)
//! are rows of it.

use service_common::query_error_envelope::is_request_id;

use super::saml_federation::{
    audience_condition, create_saml_provider, create_saml_role, emulator_with_iam, exchange,
    saml_assertion, IdentityProvider, Response, IDP_REJECTED_CLAIM_MESSAGE,
    ISSUER_NOT_IN_PROVIDER_MESSAGE, NOT_BASE64_MESSAGE, NOT_SIGNED_MESSAGE,
    PROVIDER_NOT_FOUND_MESSAGE, RESPONSE_EXPIRED_MESSAGE, ROLE_NOT_ASSERTED_MESSAGE,
    SESSION_NAME_PATTERN_MESSAGE, SESSION_NAME_REQUIRED_MESSAGE, SIGNATURE_INVALID_MESSAGE,
    STATUS_AUTHN_FAILED,
};
use super::sts_sessions::{element, emulator_account, member};

/// The service the relayed reading was made to, as the relaying layer names it.
pub(crate) const DISCOVERY_SERVICE: &str = "AWSOpenIdDiscoveryService";

/// The status that service answers a response it refused with, which the envelope states for
/// the RELAYED call — not this operation's own status.
pub(crate) const RELAYED_STATUS: u16 = 400;

/// The exception the reading of the presented response names when it will not read it as a
/// response of the provider's.
pub(crate) const INVALID_RESPONSE_EXCEPTION: &str = "AuthSamlInvalidSamlResponseException";

/// The exception the manifest lookup names when there is no manifest to read against.
pub(crate) const MANIFEST_NOT_FOUND_EXCEPTION: &str = "AuthSamlManifestNotFoundException";

/// What the proxy field states for a call that went through no proxy.
pub(crate) const NO_PROXY: &str = "null";

/// The request id the envelope in `message` states, once `message` is shown to be `sentence`
/// followed by the five relayed fields in order. Panics naming the case otherwise.
pub(crate) fn relayed_request_id(
    case: &str,
    message: &str,
    sentence: &str,
    exception: &str,
) -> String {
    let head = format!(
        "{sentence} (Service: {DISCOVERY_SERVICE}; Status Code: {RELAYED_STATUS}; \
         Error Code: {exception}; Request ID: "
    );
    let tail = format!("; Proxy: {NO_PROXY})");
    let Some(request_id) = message
        .strip_prefix(&head)
        .and_then(|rest| rest.strip_suffix(&tail))
    else {
        panic!("case {case:?}: expected {head:?}<request id>{tail:?}, got {message:?}");
    };
    assert!(
        is_request_id(request_id),
        "case {case:?}: expected the relayed call's own request id, got {request_id:?}"
    );
    request_id.to_string()
}

/// The ARN IAM keys a SAML provider of this account by.
fn provider_arn_of(name: &str) -> String {
    format!("arn:aws:iam::{}:saml-provider/{name}", emulator_account())
}

/// What a row expects of the refusal: the HTTP status, the `<Code>`, the fixed sentence of
/// its class, and the exception of the relayed call that refused it — `None` for a class that
/// carries no envelope (the signature classes, and the classes this service decides itself).
struct Refusal {
    status: u16,
    code: &'static str,
    sentence: String,
    relayed: Option<&'static str>,
}

/// The message the refusal of `case` reported, checked against `expected`.
fn assert_refusal(case: &str, status: u16, body: &str, expected: &Refusal) -> String {
    assert_eq!(
        status, expected.status,
        "case {case:?}: expected HTTP {}, got {status} {body}",
        expected.status
    );
    assert_eq!(
        element(body, "Code").as_deref(),
        Some(expected.code),
        "case {case:?}: expected <Code>{}</Code>, got {body}",
        expected.code
    );
    assert!(
        element(body, "Credentials").is_none(),
        "case {case:?}: a refused exchange must vend no credential, got {body}"
    );
    let reported = member(case, body, "Message").replace("&amp;", "&");
    match expected.relayed {
        Some(exception) => {
            relayed_request_id(case, &reported, &expected.sentence, exception);
        }
        None => {
            assert_eq!(
                reported, expected.sentence,
                "case {case:?}: expected the sentence {:?} alone, got {reported:?}",
                expected.sentence
            );
            assert!(
                !reported.contains("(Service:"),
                "case {case:?}: a class that carries no envelope relays nothing, got \
                 {reported:?}"
            );
        }
    }
    reported
}

/// The whole table: every class the reading of a presented response is refused as carries its
/// sentence and the envelope of the call that refused it, every SIGNATURE class carries its
/// sentence alone, and so does every class this service decides itself.
#[tokio::test]
async fn each_refusal_states_its_class_and_the_relayed_call_that_refused_it() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "relayed-idp", &provider).await;
    // A second registered provider, whose key is one the named provider does not publish.
    let other = IdentityProvider::mint("https://other.example.test/saml");
    create_saml_provider(&router, "relayed-other-idp", &other).await;
    let role_arn =
        create_saml_role(&router, "relayed-role", &provider_arn, audience_condition()).await;

    let valid = || Response::valid(&provider, &role_arn, &provider_arn);
    let signed = |row: Response| saml_assertion(&provider, &row);
    let relayed = |sentence: &str, exception: &'static str| Refusal {
        status: 400,
        code: "InvalidIdentityToken",
        sentence: sentence.to_string(),
        relayed: Some(exception),
    };
    let own = |status: u16, code: &'static str, sentence: &str| Refusal {
        status,
        code,
        sentence: sentence.to_string(),
        relayed: None,
    };

    // One row: the case, the provider ARN the request names, the value it presents, and the
    // refusal the rule answers it with.
    let rows: Vec<(&str, String, String, Refusal)> = vec![
        (
            "a value that is not base64 at all",
            provider_arn.clone(),
            "not-base64".to_string(),
            relayed(NOT_BASE64_MESSAGE, INVALID_RESPONSE_EXCEPTION),
        ),
        (
            "base64 of bytes that are no XML document",
            provider_arn.clone(),
            aws_smithy_types::base64::encode("plain text, not a document"),
            relayed(
                "Invalid SAMLResponse: the value is not a well-formed XML document",
                INVALID_RESPONSE_EXCEPTION,
            ),
        ),
        (
            "base64 of a well-formed document that is no SAML Response",
            provider_arn.clone(),
            aws_smithy_types::base64::encode("<note><to>aws</to></note>"),
            relayed(
                "Invalid SAMLResponse: the document is not a SAML 2.0 Response",
                INVALID_RESPONSE_EXCEPTION,
            ),
        ),
        (
            // The recorded forged case: the signature element removed outright.
            "a response carrying no signature at all",
            provider_arn.clone(),
            signed(Response {
                signed: false,
                ..valid()
            }),
            relayed(NOT_SIGNED_MESSAGE, INVALID_RESPONSE_EXCEPTION),
        ),
        (
            // A SIGNATURE class: the recorded refusal is the bare sentence.
            "a response signed by a key the named provider does not publish",
            provider_arn.clone(),
            saml_assertion(&other, &Response::valid(&other, &role_arn, &provider_arn)),
            own(400, "InvalidIdentityToken", SIGNATURE_INVALID_MESSAGE),
        ),
        (
            "a response altered after it was signed",
            provider_arn.clone(),
            signed(Response {
                altered_after_signing: true,
                ..valid()
            }),
            own(400, "InvalidIdentityToken", SIGNATURE_INVALID_MESSAGE),
        ),
        (
            "a response from an issuer the named provider is not registered for",
            provider_arn.clone(),
            signed(Response {
                issuer: "https://impostor.example.test/saml".to_string(),
                ..valid()
            }),
            relayed(ISSUER_NOT_IN_PROVIDER_MESSAGE, INVALID_RESPONSE_EXCEPTION),
        ),
        (
            "a response naming no session at all",
            provider_arn.clone(),
            signed(Response {
                name_id: String::new(),
                names_session: false,
                ..valid()
            }),
            relayed(SESSION_NAME_REQUIRED_MESSAGE, INVALID_RESPONSE_EXCEPTION),
        ),
        (
            "a response naming a session outside the character class",
            provider_arn.clone(),
            signed(Response {
                name_id: "not a session name".to_string(),
                ..valid()
            }),
            relayed(SESSION_NAME_PATTERN_MESSAGE, INVALID_RESPONSE_EXCEPTION),
        ),
        (
            // The lookup rather than the reading: there is no manifest of that provider to
            // read the response against.
            "a provider ARN the account holds no manifest for",
            provider_arn_of("never-registered"),
            signed(valid()),
            relayed(PROVIDER_NOT_FOUND_MESSAGE, MANIFEST_NOT_FOUND_EXCEPTION),
        ),
        (
            // Decided here, against the request: the response verified, and asserts another
            // role than the one asked for.
            "a verified response asserting only another role",
            provider_arn.clone(),
            signed(Response {
                asserted: vec![(
                    format!("arn:aws:iam::{}:role/another", emulator_account()),
                    provider_arn.clone(),
                )],
                ..valid()
            }),
            own(400, "InvalidIdentityToken", ROLE_NOT_ASSERTED_MESSAGE),
        ),
        (
            "a verified response asserting no role at all",
            provider_arn.clone(),
            signed(Response {
                asserted: Vec::new(),
                ..valid()
            }),
            own(400, "InvalidIdentityToken", ROLE_NOT_ASSERTED_MESSAGE),
        ),
        (
            "a response whose provider reported that authentication failed",
            provider_arn.clone(),
            signed(Response {
                status: STATUS_AUTHN_FAILED,
                ..valid()
            }),
            own(403, "IDPRejectedClaim", IDP_REJECTED_CLAIM_MESSAGE),
        ),
        (
            "a response whose validity window has passed",
            provider_arn.clone(),
            signed(Response {
                not_on_or_after_offset: Some(-60),
                conditions_not_on_or_after_offset: Some(600),
                ..valid()
            }),
            own(400, "ExpiredTokenException", RESPONSE_EXPIRED_MESSAGE),
        ),
    ];

    for (case, principal, assertion, expected) in rows {
        let (status, body) = exchange(&router, &role_arn, &principal, &assertion).await;
        assert_refusal(case, status, &body, &expected);
    }
}

/// The envelope's request id belongs to the relayed CALL, so each refusal that carries one
/// reports its own: two presentations of the same value answer the same class, the same status
/// and the same sentence, and never the same request id — while a class carrying no envelope
/// answers byte for byte alike, which is what a client matching the signature class's sentence
/// depends on.
#[tokio::test]
async fn every_relayed_refusal_reports_its_own_request_id() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "repeated-idp", &provider).await;
    let role_arn =
        create_saml_role(&router, "repeated-role", &provider_arn, audience_condition()).await;
    let valid = || Response::valid(&provider, &role_arn, &provider_arn);

    // One row: the case, the value presented, the sentence of its class, and the exception of
    // the relayed call — `None` for a class that carries no envelope.
    let rows: Vec<(&str, String, &str, Option<&'static str>)> = vec![
        (
            "a value that is not base64",
            "~~~~".to_string(),
            NOT_BASE64_MESSAGE,
            Some(INVALID_RESPONSE_EXCEPTION),
        ),
        (
            "a response carrying no signature",
            saml_assertion(
                &provider,
                &Response {
                    signed: false,
                    ..valid()
                },
            ),
            NOT_SIGNED_MESSAGE,
            Some(INVALID_RESPONSE_EXCEPTION),
        ),
        (
            "a response altered after it was signed",
            saml_assertion(
                &provider,
                &Response {
                    altered_after_signing: true,
                    ..valid()
                },
            ),
            SIGNATURE_INVALID_MESSAGE,
            None,
        ),
        (
            "a response asserting another role",
            saml_assertion(
                &provider,
                &Response {
                    asserted: Vec::new(),
                    ..valid()
                },
            ),
            ROLE_NOT_ASSERTED_MESSAGE,
            None,
        ),
    ];

    for (case, assertion, sentence, exception) in rows {
        let mut messages = Vec::new();
        for presentation in 1..=3 {
            let (status, body) = exchange(&router, &role_arn, &provider_arn, &assertion).await;
            let expected = Refusal {
                status: 400,
                code: "InvalidIdentityToken",
                sentence: sentence.to_string(),
                relayed: exception,
            };
            let reported = assert_refusal(
                &format!("{case}, presentation {presentation}"),
                status,
                &body,
                &expected,
            );
            messages.push(reported);
        }
        match exception {
            // The id is per call, so the three messages differ in exactly that field.
            Some(exception) => {
                let ids: Vec<String> = messages
                    .iter()
                    .enumerate()
                    .map(|(presentation, message)| {
                        relayed_request_id(
                            &format!("{case}, presentation {}", presentation + 1),
                            message,
                            sentence,
                            exception,
                        )
                    })
                    .collect();
                for (first, second) in [(0usize, 1usize), (1, 2), (0, 2)] {
                    assert_ne!(
                        ids[first], ids[second],
                        "case {case:?}: expected each refusal its own request id, got \
                         {:?} twice",
                        ids[first]
                    );
                }
            }
            // Nothing is relayed, so nothing varies: the same refusal answers alike.
            None => {
                for (presentation, message) in messages.iter().enumerate() {
                    assert_eq!(
                        message, &messages[0],
                        "case {case:?}: presentation {} must answer exactly as the first did, \
                         expected {:?}, got {message:?}",
                        presentation + 1,
                        messages[0]
                    );
                }
            }
        }
    }
}
