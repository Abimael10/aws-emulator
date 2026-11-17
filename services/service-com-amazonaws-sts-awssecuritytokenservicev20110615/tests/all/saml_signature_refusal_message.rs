// SPDX-License-Identifier: CC0-1.0

//! What `AssumeRoleWithSAML` answers when no signature of the registered provider's covers
//! the response it is presented.
//!
//! RULE: the class in which a presented SAML authentication response carries no signature of
//! the registered provider's over what it says — the `SignatureValue` does not check out
//! against a key that provider publishes, or the signature declares a transform that selects
//! the signed content by an expression of its own — is refused with HTTP 400, `<Code>`
//! `InvalidIdentityToken` and the fixed sentence of that class ALONE: no parenthesized
//! envelope of a relayed call, nothing derived from the request, and therefore a message two
//! presentations of the same value report byte for byte alike. The envelope belongs to the
//! classes a relayed reading of the document answered, which keep it.
//!
//! The rows span the class along HOW the signature fails to cover the document: the signature
//! bits were altered, they are no base64 at all, they are absent from an element that is
//! there, the signed content changed after it was signed, and — with the reference digest
//! proven byte-identical to the baseline the service ACCEPTS, so nothing but the declared
//! transform differs — each expression-based transform of the XML signature vocabulary. The
//! recorded cases (a tampered `SignatureValue`, asserted twice for determinism, and the
//! XPath 1.0 variant) are rows of the table.
//!
//! Every row is written from ONE reading of the clock, the anchor its offsets count from, so
//! what a row says — and therefore the bytes its reference digests — is a function of the row
//! alone and not of which second of the host clock each of its timestamps was formatted in.

use super::saml_federation::{
    audience_condition, create_saml_provider, create_saml_role, emulator_with_iam, exchange,
    saml_assertion, IdentityProvider, Response, NOT_BASE64_MESSAGE, NOT_SIGNED_MESSAGE,
    PROVIDER_NOT_FOUND_MESSAGE, SIGNATURE_INVALID_MESSAGE,
};
use super::saml_relayed_refusal::{
    relayed_request_id, INVALID_RESPONSE_EXCEPTION, MANIFEST_NOT_FOUND_EXCEPTION,
};
use super::sts_sessions::{element, emulator_account, member, now_secs};

/// The transforms that select what a reference covers by an expression of their own, as W3C
/// "XML Signature Syntax and Processing" §6.6 registers them (XSLT, XPath and base64) and
/// W3C "XML-Signature XPath Filter 2.0" registers its own: none of them is a canonicalization
/// or the enveloped-signature transform, which are the only transforms a reference may name.
const XSLT_TRANSFORM: &str = "http://www.w3.org/TR/1999/REC-xslt-19991116";
const XPATH_TRANSFORM: &str = "http://www.w3.org/TR/1999/REC-xpath-19991116";
const XPATH_FILTER2_TRANSFORM: &str = "http://www.w3.org/2002/06/xmldsig-filter2";
const BASE64_TRANSFORM: &str = "http://www.w3.org/2000/09/xmldsig#base64";

/// The transforms the baseline response — the one the service accepts — names.
const ENVELOPED_SIGNATURE: &str = "http://www.w3.org/2000/09/xmldsig#enveloped-signature";
const EXCLUSIVE_C14N: &str = "http://www.w3.org/2001/10/xml-exc-c14n#";

/// How many times each row is presented: a refusal of this class carries nothing per call, so
/// every presentation has to report the very same bytes.
const PRESENTATIONS: usize = 2;

/// The text of the one `<ds:NAME>…</ds:NAME>` element of the presented response, read out of
/// the base64 a `SAMLAssertion` is.
fn signature_element(presented: &str, name: &str) -> String {
    let document = decoded(presented);
    let open = format!("<ds:{name}>");
    let close = format!("</ds:{name}>");
    let start = document
        .find(&open)
        .unwrap_or_else(|| panic!("the presented response carries no {open} in {document}"))
        + open.len();
    let end = document[start..]
        .find(&close)
        .unwrap_or_else(|| panic!("the {open} of the presented response is not closed"))
        + start;
    document[start..end].to_string()
}

/// The XML document the base64 `SAMLAssertion` `presented` carries.
fn decoded(presented: &str) -> String {
    let bytes = aws_smithy_types::base64::decode(presented).expect("the value is base64");
    String::from_utf8(bytes).expect("the presented response is UTF-8")
}

/// `presented` with the text of its `ds:SignatureValue` replaced by what `replacement` makes
/// of it, re-encoded as a `SAMLAssertion`: everything else — the `SignedInfo` the signature
/// was computed over and the document the reference digests — is left exactly as signed.
fn with_signature_value(presented: &str, replacement: impl FnOnce(&str) -> String) -> String {
    let document = decoded(presented);
    let signed = signature_element(presented, "SignatureValue");
    let replaced = document.replace(
        &format!("<ds:SignatureValue>{signed}</ds:SignatureValue>"),
        &format!("<ds:SignatureValue>{}</ds:SignatureValue>", replacement(&signed)),
    );
    assert_ne!(
        replaced, document,
        "the SignatureValue of the presented response must have been replaced"
    );
    aws_smithy_types::base64::encode(replaced.as_bytes())
}

/// The recorded case: the same response with its signature bits altered — one bit of the
/// signature flipped, so the value is still the base64 of a signature of the right length and
/// is no longer the provider's signature over this `SignedInfo`.
fn tampered_signature(presented: &str) -> String {
    with_signature_value(presented, |signed| {
        let mut bytes = aws_smithy_types::base64::decode(signed).expect("the signature is base64");
        let last = bytes.last_mut().expect("the signature carries bytes");
        *last ^= 0x01;
        aws_smithy_types::base64::encode(bytes.as_slice())
    })
}

/// What a row expects of a refusal: this sentence, and the exception of the relayed reading
/// that answered it — `None` for the signature class, which states its sentence alone.
struct Expected {
    status: u16,
    code: &'static str,
    sentence: &'static str,
    relayed: Option<&'static str>,
}

/// The message the refusal reported, checked against `expected`: the status, the code, that no
/// credential was vended, and the message — for the signature class, the sentence and nothing
/// else at all.
fn assert_refusal(case: &str, status: u16, body: &str, expected: &Expected) -> String {
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
            relayed_request_id(case, &reported, expected.sentence, exception);
        }
        None => {
            assert_eq!(
                reported, expected.sentence,
                "case {case:?}: expected the sentence {:?} and nothing else, got {reported:?}",
                expected.sentence
            );
            assert!(
                !reported.contains('('),
                "case {case:?}: this class relays nothing, so its message carries no \
                 parenthesized envelope, got {reported:?}"
            );
        }
    }
    reported
}

/// Every way a presented response fails to carry the provider's signature over what it says is
/// refused with the bare sentence of that class, the same bytes on every presentation.
#[tokio::test]
async fn a_signature_that_does_not_cover_the_response_is_refused_with_the_bare_sentence() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "signature-idp", &provider).await;
    // A second registered provider, whose key is one the named provider does not publish: a
    // response correctly signed by it is signed by nothing the named provider stands behind.
    let other = IdentityProvider::mint("https://other.example.test/saml");
    create_saml_provider(&router, "signature-other-idp", &other).await;
    let role_arn =
        create_saml_role(&router, "signature-role", &provider_arn, audience_condition()).await;

    // One reading of the clock, which every row below counts its offsets from: the baseline
    // and the rows that must digest the very same bytes as it are then written from one
    // anchor, however long the host takes between them.
    let anchor = now_secs();
    let valid = || Response {
        anchor,
        ..Response::valid(&provider, &role_arn, &provider_arn)
    };
    let signed = |row: Response| saml_assertion(&provider, &row);
    // The baseline the service ACCEPTS, so each row below differs from a federated exchange in
    // exactly what it alters.
    let baseline = signed(valid());
    let (status, body) = exchange(&router, &role_arn, &provider_arn, &baseline).await;
    assert_eq!(
        status, 200,
        "case \"the baseline this table varies from\": expected HTTP 200, got {status} {body}"
    );
    assert!(
        element(&body, "AccessKeyId").is_some(),
        "case \"the baseline this table varies from\": expected a vended credential, got {body}"
    );
    let baseline_digest = signature_element(&baseline, "DigestValue");

    // One row: the case and the `SAMLAssertion` it presents for the role and provider above.
    let mut rows: Vec<(&str, String)> = vec![
        (
            // The recorded case.
            "a response whose SignatureValue was tampered with",
            tampered_signature(&baseline),
        ),
        (
            "a response whose SignatureValue is not base64 at all",
            with_signature_value(&baseline, |_| "~~not base64~~".to_string()),
        ),
        (
            "a response whose SignatureValue element is empty",
            with_signature_value(&baseline, |_| String::new()),
        ),
        (
            "a response carrying another registered provider's signature",
            with_signature_value(&baseline, |_| {
                signature_element(
                    &saml_assertion(
                        &other,
                        &Response {
                            anchor,
                            ..Response::valid(&other, &role_arn, &provider_arn)
                        },
                    ),
                    "SignatureValue",
                )
            }),
        ),
        (
            "a response altered after it was signed",
            signed(Response {
                altered_after_signing: true,
                ..valid()
            }),
        ),
    ];
    // The transform rows: the reference digests the very same bytes as the accepted baseline,
    // so the declared transform is the whole difference.
    for transform in [
        XPATH_TRANSFORM,
        XPATH_FILTER2_TRANSFORM,
        XSLT_TRANSFORM,
        BASE64_TRANSFORM,
    ] {
        let presented = signed(Response {
            transforms: vec![ENVELOPED_SIGNATURE, EXCLUSIVE_C14N, transform],
            ..valid()
        });
        assert_eq!(
            signature_element(&presented, "DigestValue"),
            baseline_digest,
            "case {transform:?}: the variant must digest the same bytes as the accepted \
             baseline"
        );
        rows.push((transform, presented));
    }

    for (case, presented) in rows {
        let expected = Expected {
            status: 400,
            code: "InvalidIdentityToken",
            sentence: SIGNATURE_INVALID_MESSAGE,
            relayed: None,
        };
        let mut messages = Vec::new();
        for presentation in 1..=PRESENTATIONS {
            let (status, body) = exchange(&router, &role_arn, &provider_arn, &presented).await;
            messages.push(assert_refusal(
                &format!("{case}, presentation {presentation}"),
                status,
                &body,
                &expected,
            ));
        }
        for (presentation, message) in messages.iter().enumerate() {
            assert_eq!(
                message, &messages[0],
                "case {case:?}: presentation {} must answer the very same bytes as the first, \
                 expected {:?}, got {message:?}",
                presentation + 1,
                messages[0]
            );
        }
    }
}

/// The envelope is attached per refusal class: the classes a relayed reading of the presented
/// document — or of the manifest it would be read against — answered carry it, while the
/// signature class presented to the very same role and provider does not.
#[tokio::test]
async fn only_the_classes_a_relayed_reading_refused_carry_the_envelope() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "per-class-idp", &provider).await;
    let role_arn =
        create_saml_role(&router, "per-class-role", &provider_arn, audience_condition()).await;
    let valid = || Response::valid(&provider, &role_arn, &provider_arn);
    let signed = |row: Response| saml_assertion(&provider, &row);
    let unregistered = format!(
        "arn:aws:iam::{}:saml-provider/never-registered",
        emulator_account()
    );

    // One row: the case, the provider ARN the request names, the value it presents, and what
    // the rule answers it with.
    let rows: Vec<(&str, String, String, Expected)> = vec![
        (
            "a value that is not base64 at all",
            provider_arn.clone(),
            "not-base64".to_string(),
            Expected {
                status: 400,
                code: "InvalidIdentityToken",
                sentence: NOT_BASE64_MESSAGE,
                relayed: Some(INVALID_RESPONSE_EXCEPTION),
            },
        ),
        (
            "a response nothing signed at all",
            provider_arn.clone(),
            signed(Response {
                signed: false,
                ..valid()
            }),
            Expected {
                status: 400,
                code: "InvalidIdentityToken",
                sentence: NOT_SIGNED_MESSAGE,
                relayed: Some(INVALID_RESPONSE_EXCEPTION),
            },
        ),
        (
            "a provider ARN the account holds no manifest for",
            unregistered,
            signed(valid()),
            Expected {
                status: 400,
                code: "InvalidIdentityToken",
                sentence: PROVIDER_NOT_FOUND_MESSAGE,
                relayed: Some(MANIFEST_NOT_FOUND_EXCEPTION),
            },
        ),
        (
            "a response whose SignatureValue was tampered with",
            provider_arn.clone(),
            tampered_signature(&signed(valid())),
            Expected {
                status: 400,
                code: "InvalidIdentityToken",
                sentence: SIGNATURE_INVALID_MESSAGE,
                relayed: None,
            },
        ),
    ];

    for (case, principal, presented, expected) in rows {
        let (status, body) = exchange(&router, &role_arn, &principal, &presented).await;
        assert_refusal(case, status, &body, &expected);
    }
}

/// An instant written as a SAML `xs:dateTime`, which is the `DateTime` format of the model's
/// own timestamps.
fn at(secs: i64) -> String {
    aws_smithy_types::DateTime::from_secs(secs)
        .fmt(aws_smithy_types::date_time::Format::DateTime)
        .expect("an ISO-8601 instant")
}

/// The attribute value of the FIRST `NAME="…"` of the presented response.
fn attribute(presented: &str, name: &str) -> String {
    let document = decoded(presented);
    let open = format!("{name}=\"");
    let start = document
        .find(&open)
        .unwrap_or_else(|| panic!("the presented response carries no {open} in {document}"))
        + open.len();
    let end = document[start..]
        .find('"')
        .unwrap_or_else(|| panic!("the {open} of the presented response is not closed"))
        + start;
    document[start..end].to_string()
}

/// What a row states is fixed by the row's own anchor, not by the clock at the moment each of
/// its timestamps happens to be written: a row anchored at an instant states exactly the
/// instants that anchor fixes, two rows sharing one anchor digest the very same bytes whatever
/// the host does between writing them, and the pair is served and refused as the rule says —
/// so the table above compares a variant's reference digest against the accepted baseline's
/// without the host's speed entering into it.
#[tokio::test]
async fn an_anchored_row_states_the_instants_its_anchor_fixes() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "anchored-idp", &provider).await;
    let role_arn =
        create_saml_role(&router, "anchored-role", &provider_arn, audience_condition()).await;
    let clock = now_secs();
    // The window each row below states, counted from its own anchor.
    const ISSUED: i64 = -45;
    const CLOSES: i64 = 300;

    // The rows differ along where the anchor sits relative to the clock: well behind it, just
    // behind it, on it, and ahead of it — each one a response the rule serves.
    for shift in [-120_i64, -1, 0, 30] {
        let case = format!("a row anchored {shift}s from the clock");
        let anchor = clock + shift;
        let row = |transforms: Vec<&'static str>| Response {
            anchor,
            issued_at_offset: ISSUED,
            not_before_offset: ISSUED,
            not_on_or_after_offset: Some(CLOSES),
            transforms,
            ..Response::valid(&provider, &role_arn, &provider_arn)
        };

        let accepted = saml_assertion(&provider, &row(vec![ENVELOPED_SIGNATURE, EXCLUSIVE_C14N]));
        for (member_name, expected) in [
            ("IssueInstant", at(anchor + ISSUED)),
            ("NotBefore", at(anchor + ISSUED)),
            ("NotOnOrAfter", at(anchor + CLOSES)),
        ] {
            let stated = attribute(&accepted, member_name);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected {member_name} {expected:?} as the anchor fixes it, \
                 got {stated:?}"
            );
        }

        // The same row written again, and the row that differs from it in nothing but the
        // transform its reference declares: both cover the very same bytes.
        let again = saml_assertion(&provider, &row(vec![ENVELOPED_SIGNATURE, EXCLUSIVE_C14N]));
        let variant = saml_assertion(
            &provider,
            &row(vec![ENVELOPED_SIGNATURE, EXCLUSIVE_C14N, XPATH_TRANSFORM]),
        );
        let digest = signature_element(&accepted, "DigestValue");
        for (written, what) in [(&again, "written again"), (&variant, "the transform variant")] {
            let covered = signature_element(written, "DigestValue");
            assert_eq!(
                covered, digest,
                "case {case:?}: {what} must digest the same bytes as the row it varies from, \
                 expected {digest:?}, got {covered:?}"
            );
        }

        let (status, body) = exchange(&router, &role_arn, &provider_arn, &accepted).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the anchored response to be served, got {status} {body}"
        );
        assert!(
            element(&body, "AccessKeyId").is_some(),
            "case {case:?}: expected a vended credential, got {body}"
        );
        let (status, body) = exchange(&router, &role_arn, &provider_arn, &variant).await;
        assert_refusal(
            &case,
            status,
            &body,
            &Expected {
                status: 400,
                code: "InvalidIdentityToken",
                sentence: SIGNATURE_INVALID_MESSAGE,
                relayed: None,
            },
        );
    }
}
