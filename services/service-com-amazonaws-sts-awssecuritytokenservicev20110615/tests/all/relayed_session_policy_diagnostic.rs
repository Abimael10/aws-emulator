// SPDX-License-Identifier: CC0-1.0

//! The ROLE-ASSUMPTION mints relay their JSON parser's own diagnostic for an inline
//! session policy they cannot parse, and they parse it before the identity the request
//! presents is evaluated.
//!
//! RULE: for every operation that assumes a role with an inline session policy
//! (`AssumeRole`, `AssumeRoleWithSAML`, `AssumeRoleWithWebIdentity`), the document is
//! parsed as a member of the REQUEST — ahead of the identity material the request carries
//! — and one that does not parse is refused `MalformedPolicyDocument` / HTTP 400 whose
//! message "describes the specific error" (`MalformedPolicyDocumentException`, this
//! service's model) by relaying that parse: the fault it stopped on and what was expected
//! there, the START MARKER of the construct left unterminated, and the position the parse
//! stopped at — each position reported as a source-location clause whose source is
//! withheld. Every class of parse failure is reported this way, a document that merely ran
//! out of input included, which is the only class `GetFederationToken` words as a sentence
//! of its own (the last test here is that contrast, and
//! `malformed_session_policy.rs` owns that operation's rows).
//!
//! The rows differ along what the rule generalises over: WHICH construct the document left
//! unterminated (an array, an object, a nested object, a string), how far into the document
//! it opened, which line it opened and ended on, and the classes that stop on a character
//! instead and so name no start marker at all. They differ along the IDENTITY the request
//! presents as well — down to one a session really is vended for — because the ordering is a
//! fact about the request's members and not about the identity happening to be defective.
//!
//! Every call goes over the wire as the `application/x-www-form-urlencoded` awsQuery POST a
//! client makes, through the same router the emulator serves with — signed for `AssumeRole`
//! and `GetFederationToken`, and unsigned for the two federation mints, which the model
//! makes anonymous.

use super::malformed_session_policy::decode_xml_text;
use super::saml_federation::{
    audience_condition, create_saml_provider, create_saml_role, emulator_with_iam, saml_assertion,
    IdentityProvider, Response,
};
use super::sts_sessions::{call, element, emulator, emulator_account, member, LONG_TERM_KEY};
use super::web_identity_federation::unsigned_call;
use smithy_server_runtime::MultiServiceRouter;

/// The SOURCE every location clause of the diagnostic names. Fixed text: the submitted
/// document is withheld, and the clause says so by naming the parser feature that would
/// have put it there.
const WITHHELD_SOURCE: &str = "REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` disabled)";

/// One location clause, as the diagnostic reports a position in the document it was reading.
fn location_clause(line: usize, column: usize) -> String {
    format!("[Source: {WITHHELD_SOURCE}; line: {line}, column: {column}]")
}

/// A document the parser could not read, with everything the relayed diagnostic must say
/// about it.
struct Unparseable {
    case: &'static str,
    policy: &'static str,
    /// What the parse stopped on and what was expected there.
    reason: &'static str,
    /// Where the construct the parse was still inside OPENED, and `None` for a fault that
    /// left no construct open — such a fault names no start marker.
    start_marker: Option<(usize, usize)>,
    /// Where the parse stopped: one past the document's last character for a document that
    /// ran out of input, and the offending character's own position otherwise.
    stopped_at: (usize, usize),
    /// The character the parse stopped ON, and `None` for a document that ran out of input.
    offender: Option<char>,
}

/// The documents: one per construct a document can be left inside, plus the faults that
/// stop on a character and so carry no start marker.
fn unparseable_documents() -> Vec<Unparseable> {
    vec![
        Unparseable {
            // The shape a client that assembled a statement list and never closed it sends:
            // the array opens late in the document, so the two positions differ.
            case: "a statement array left unterminated",
            policy: r#"{"Version": "2012-10-17", "Statement": ["#,
            reason: "Unexpected end-of-input: expected close marker for Array",
            start_marker: Some((1, 40)),
            stopped_at: (1, 41),
            offender: None,
        },
        Unparseable {
            case: "the outermost object left unterminated",
            policy: r#"{"Version":"2012-10-17""#,
            reason: "Unexpected end-of-input: expected close marker for Object",
            start_marker: Some((1, 1)),
            stopped_at: (1, 24),
            offender: None,
        },
        Unparseable {
            // The INNERMOST open mark is the one reported: the statement object, not the
            // array around it or the document object around that.
            case: "a statement object inside the array left unterminated",
            policy: r#"{"Statement":[{"Effect":"Allow""#,
            reason: "Unexpected end-of-input: expected close marker for Object",
            start_marker: Some((1, 15)),
            stopped_at: (1, 32),
            offender: None,
        },
        Unparseable {
            // A string carries no structure, so the fault is the string itself and there is
            // no container mark to report.
            case: "a string left unterminated",
            policy: r#"{"Version":"2012-1"#,
            reason: "Unexpected end-of-input: expected close marker for String",
            start_marker: None,
            stopped_at: (1, 19),
            offender: None,
        },
        Unparseable {
            // Both positions are on the document's SECOND line, and the column counts from
            // that line's start rather than from the document's.
            case: "an array left unterminated on the second line",
            policy: "{\n\"Statement\": [",
            reason: "Unexpected end-of-input: expected close marker for Array",
            start_marker: Some((2, 14)),
            stopped_at: (2, 15),
            offender: None,
        },
        Unparseable {
            case: "a field name that is not quoted",
            policy: "{not valid json",
            reason: "Unexpected character ('n' (code 110)): was expecting double-quote to \
                     start field name",
            start_marker: None,
            stopped_at: (1, 2),
            offender: Some('n'),
        },
        Unparseable {
            case: "nothing that can start a value",
            policy: "xyz",
            reason: "Unexpected character ('x' (code 120)): was expecting a valid value \
                     (JSON String, Number, Array, Object or token 'null', 'true' or 'false')",
            start_marker: None,
            stopped_at: (1, 1),
            offender: Some('x'),
        },
    ]
}

/// The whole message a row's document earns: the relayed reason, the start marker where the
/// document left a construct open, and the position the parse stopped at.
fn expected_message(row: &Unparseable) -> String {
    let start = match row.start_marker {
        Some((line, column)) => format!(" (start marker at {})", location_clause(line, column)),
        None => String::new(),
    };
    let (line, column) = row.stopped_at;
    format!(
        "Unexpected IOException: {}{start}\n at {}",
        row.reason,
        location_clause(line, column)
    )
}

/// The byte offset of the one-based `line`:`column` of `document`, counting columns in
/// bytes — the convention the diagnostic reports in. Computed from the submitted document
/// alone, so a reported position is checked against the document rather than against the
/// service's own bookkeeping.
fn byte_offset(case: &str, document: &str, line: usize, column: usize) -> usize {
    let mut start = 0;
    for _ in 1..line {
        start += document[start..]
            .find('\n')
            .unwrap_or_else(|| panic!("case {case:?}: {document:?} has no line {line}"))
            + 1;
    }
    start + column - 1
}

/// Every position the diagnostic reported, checked against the document it was reading: a
/// start marker names an opening mark, a ran-out parse stopped one past the last character,
/// and a parse that stopped on a character names that character.
fn assert_positions_name_the_document(row: &Unparseable) {
    let case = row.case;
    if let Some((line, column)) = row.start_marker {
        let offset = byte_offset(case, row.policy, line, column);
        let mark = row.policy.as_bytes().get(offset).copied();
        assert!(
            mark == Some(b'{') || mark == Some(b'['),
            "case {case:?}: the start marker at line {line}, column {column} must name an \
             opening mark of {:?}, got {:?}",
            row.policy,
            mark.map(char::from)
        );
    }
    let (line, column) = row.stopped_at;
    let offset = byte_offset(case, row.policy, line, column);
    match row.offender {
        None => assert_eq!(
            offset,
            row.policy.len(),
            "case {case:?}: a document that ran out of input stops one past its last \
             character, got offset {offset} of {:?}",
            row.policy
        ),
        Some(offender) => assert_eq!(
            row.policy[offset..].chars().next(),
            Some(offender),
            "case {case:?}: the reported position must name the offending character of {:?}",
            row.policy
        ),
    }
}

/// A role ARN of this account that names no role at all: the federation rows never reach
/// the role, because the document is refused first.
fn probe_role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/relayed-policy-probe",
        emulator_account()
    )
}

/// A SAML provider ARN of this account that the account has NOT registered, so a request
/// naming it can verify nothing: the identity material of the `AssumeRoleWithSAML` rows.
fn unregistered_provider_arn() -> String {
    format!(
        "arn:aws:iam::{}:saml-provider/relayed-policy-unregistered",
        emulator_account()
    )
}

/// What a value the account holds no SAML provider for is refused with, and what a value
/// that is no identity token at all is refused with: the identity-side answers these rows
/// must NOT get while their policy is malformed.
const INVALID_IDENTITY_TOKEN: &str = "InvalidIdentityToken";

/// The operations the rule covers, each with the other parameters it requires and whether a
/// client holds a credential when it calls it.
struct Mint {
    action: &'static str,
    base: Vec<(&'static str, String)>,
    signed: bool,
}

/// `AssumeRole` against a role this caller may assume (so its own authorization step is
/// behind the policy parse), and the two anonymous federation mints, each presenting
/// identity material nothing can verify — which is what makes the ordering visible.
fn role_assumption_mints(assumable_role_arn: &str) -> Vec<Mint> {
    vec![
        Mint {
            action: "AssumeRole",
            base: vec![
                ("RoleArn", assumable_role_arn.to_string()),
                ("RoleSessionName", "probe".to_string()),
            ],
            signed: true,
        },
        Mint {
            action: "AssumeRoleWithSAML",
            base: vec![
                ("RoleArn", probe_role_arn()),
                ("PrincipalArn", unregistered_provider_arn()),
                ("SAMLAssertion", "not-a-saml-response".to_string()),
            ],
            signed: false,
        },
        Mint {
            action: "AssumeRoleWithWebIdentity",
            base: vec![
                ("RoleArn", probe_role_arn()),
                ("RoleSessionName", "probe".to_string()),
                ("WebIdentityToken", "not-a-jwt".to_string()),
            ],
            signed: false,
        },
    ]
}

/// One call of `mint` carrying `policy`, made the way a client of that operation makes it.
async fn mint_with_policy(
    router: &MultiServiceRouter,
    mint: &Mint,
    policy: Option<&str>,
) -> (u16, String) {
    let mut params = mint.base.clone();
    if let Some(policy) = policy {
        params.push(("Policy", policy.to_string()));
    }
    if mint.signed {
        call(router, LONG_TERM_KEY, mint.action, &params).await
    } else {
        unsigned_call(router, mint.action, &params).await
    }
}

/// Every class of parse failure, on every role-assumption mint: HTTP 400,
/// `MalformedPolicyDocument`, and the relayed diagnostic of the parse that read the
/// submitted document.
#[tokio::test]
async fn a_role_assumption_relays_the_parser_diagnostic_for_every_unparseable_policy() {
    let (router, engine) = emulator();
    let (assumable, _role_id) = super::sts_sessions::seed_demo_role(&engine);

    for mint in role_assumption_mints(&assumable) {
        for row in unparseable_documents() {
            let case = format!("{}: {}", mint.action, row.case);
            assert_positions_name_the_document(&row);
            let (status, body) = mint_with_policy(&router, &mint, Some(row.policy)).await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body}"
            );
            assert_eq!(
                element(&body, "Code").as_deref(),
                Some("MalformedPolicyDocument"),
                "case {case:?}: expected a MalformedPolicyDocument, got {body}"
            );
            let message = decode_xml_text(&member(&case, &body, "Message"));
            assert_eq!(
                message,
                expected_message(&row),
                "case {case:?}: expected the relayed parser diagnostic, got {message:?}"
            );
            assert!(
                !message.contains(row.policy),
                "case {case:?}: the refusal must not echo the submitted document, got \
                 {message:?}"
            );
            assert!(
                element(&body, "Credentials").is_none(),
                "case {case:?}: a refused mint must vend no credential, got {body}"
            );
        }
    }
}

/// The ORDERING: the identity material a federation mint presents is evaluated only after
/// the inline policy has parsed, so the SAME unverifiable identity earns the policy's
/// refusal while the document is malformed and its own refusal once the document parses.
#[tokio::test]
async fn the_presented_identity_is_evaluated_only_after_the_policy_parses() {
    let (router, engine) = emulator();
    let (assumable, _role_id) = super::sts_sessions::seed_demo_role(&engine);
    // A document that IS a JSON policy document, for the rows that get past the parse.
    let well_formed = r#"{"Version":"2012-10-17","Statement":[]}"#;

    for mint in role_assumption_mints(&assumable) {
        if mint.signed {
            // `AssumeRole`'s caller is the credential it signs with, not material the
            // request carries, so it has no identity step to be ordered against; its own
            // ordering — authorization first — is `authorization_before_policy_parsing.rs`.
            continue;
        }
        let case = format!("{}: a malformed policy with an unverifiable identity", mint.action);
        let (status, body) = mint_with_policy(&router, &mint, Some("{not valid json")).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            element(&body, "Code").as_deref(),
            Some("MalformedPolicyDocument"),
            "case {case:?}: the document is read before the identity is, got {body}"
        );

        // The same request with a document that parses reaches the identity, which is what
        // proves the rows above refuse the document rather than everything.
        for (row, policy) in [
            ("a well-formed policy", Some(well_formed)),
            ("no policy at all", None),
        ] {
            let case = format!("{}: {row} with an unverifiable identity", mint.action);
            let (status, body) = mint_with_policy(&router, &mint, policy).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body}"
            );
            assert_eq!(
                element(&body, "Code").as_deref(),
                Some(INVALID_IDENTITY_TOKEN),
                "case {case:?}: expected the presented identity's own refusal, got {body}"
            );
        }
    }
}

/// A well-formed session policy: it narrows the session's permissions and nothing else, so a
/// request carrying it is served whenever the same request without it is.
const WELL_FORMED_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[]}"#;

/// The ordering does not depend on the identity being BAD: the inline document is parsed
/// ahead of the SAML provider lookup and ahead of the response reading, so a request whose
/// authentication response the account's REGISTERED provider really signed — one the same
/// request gets a session for once its document parses — is still refused the document's
/// `MalformedPolicyDocument`.
///
/// The identity rows differ in how far the identity would get on its own: all the way (a
/// freshly signed response of the registered provider), as far as the response reading (a
/// value that is no response at all), and no further than the provider lookup (an ARN the
/// account registered nothing for). The document's refusal is the answer to every one.
#[tokio::test]
async fn a_malformed_policy_is_refused_even_when_the_presented_identity_verifies() {
    let (router, _engine) = emulator_with_iam();
    let provider = IdentityProvider::mint("https://idp.example.test/saml");
    let provider_arn = create_saml_provider(&router, "relayed-policy-idp", &provider).await;
    let role_arn = create_saml_role(
        &router,
        "saml-relayed-policy",
        &provider_arn,
        audience_condition(),
    )
    .await;
    let verifiable = saml_assertion(
        &provider,
        &Response::valid(&provider, &role_arn, &provider_arn),
    );

    // One identity row: the case, the provider ARN the request names and the response it
    // presents.
    let identities: Vec<(&str, String, String)> = vec![
        (
            "a response the registered provider signed",
            provider_arn.clone(),
            verifiable.clone(),
        ),
        (
            "a value that is no authentication response",
            provider_arn.clone(),
            "not-base64".to_string(),
        ),
        (
            "a provider the account never registered",
            unregistered_provider_arn(),
            verifiable.clone(),
        ),
    ];

    let saml_mint = |principal_arn: &str, assertion: &str| Mint {
        action: "AssumeRoleWithSAML",
        base: vec![
            ("RoleArn", role_arn.clone()),
            ("PrincipalArn", principal_arn.to_string()),
            ("SAMLAssertion", assertion.to_string()),
        ],
        signed: false,
    };

    // The verifying identity really is one a session is vended for, with a document that
    // parses and with no document at all — which is what makes the refusals below the
    // document's and not the identity's.
    let verifying = saml_mint(&provider_arn, &verifiable);
    for (row, policy) in [
        ("a well-formed policy", Some(WELL_FORMED_POLICY)),
        ("no policy at all", None),
    ] {
        let case = format!("AssumeRoleWithSAML: {row} with a verifying identity");
        let (status, body) = mint_with_policy(&router, &verifying, policy).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the exchange to be served, got {status} {body}"
        );
        assert!(
            element(&body, "AccessKeyId").is_some(),
            "case {case:?}: expected a federated session's credential, got {body}"
        );
    }

    for (identity, principal_arn, assertion) in &identities {
        let mint = saml_mint(principal_arn, assertion);
        for row in unparseable_documents() {
            let case = format!("AssumeRoleWithSAML: {} with {identity}", row.case);
            assert_positions_name_the_document(&row);
            let (status, body) = mint_with_policy(&router, &mint, Some(row.policy)).await;

            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} {body}"
            );
            assert_eq!(
                element(&body, "Code").as_deref(),
                Some("MalformedPolicyDocument"),
                "case {case:?}: the document is read before the identity is, got {body}"
            );
            let message = decode_xml_text(&member(&case, &body, "Message"));
            assert_eq!(
                message,
                expected_message(&row),
                "case {case:?}: expected the relayed parser diagnostic, got {message:?}"
            );
            assert!(
                element(&body, "AccessKeyId").is_none(),
                "case {case:?}: a refused mint must vend no credential, got {body}"
            );
        }
    }
}

/// The one class the two readings word differently, on the SAME documents: a role
/// assumption relays the diagnostic, and `GetFederationToken` answers its own fixed
/// sentence.
#[tokio::test]
async fn only_get_federation_token_words_a_policy_that_ran_out_of_input_as_one_sentence() {
    let (router, engine) = emulator();
    let (assumable, _role_id) = super::sts_sessions::seed_demo_role(&engine);
    let federation = Mint {
        action: "GetFederationToken",
        base: vec![("Name", "probe".to_string())],
        signed: true,
    };
    let assume_role = Mint {
        action: "AssumeRole",
        base: vec![
            ("RoleArn", assumable.clone()),
            ("RoleSessionName", "probe".to_string()),
        ],
        signed: true,
    };
    /// The whole sentence `GetFederationToken` answers for a document that ran out of
    /// input, spelled out here rather than read off the service.
    const ONE_SENTENCE: &str = "Syntax errors in policy.";

    for row in unparseable_documents() {
        if row.offender.is_some() {
            // A document that stopped ON a character is described by both operations the
            // same way; `malformed_session_policy.rs` owns those rows.
            continue;
        }
        let case = format!("GetFederationToken: {}", row.case);
        let (status, body) = mint_with_policy(&router, &federation, Some(row.policy)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        assert_eq!(
            element(&body, "Code").as_deref(),
            Some("MalformedPolicyDocument"),
            "case {case:?}: expected a MalformedPolicyDocument, got {body}"
        );
        let message = decode_xml_text(&member(&case, &body, "Message"));
        assert_eq!(
            message, ONE_SENTENCE,
            "case {case:?}: expected exactly {ONE_SENTENCE:?}, got {message:?}"
        );

        // ... and the same document on a role assumption carries the diagnostic instead.
        let case = format!("AssumeRole: {}", row.case);
        let (status, body) = mint_with_policy(&router, &assume_role, Some(row.policy)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {body}"
        );
        let message = decode_xml_text(&member(&case, &body, "Message"));
        assert_eq!(
            message,
            expected_message(&row),
            "case {case:?}: expected the relayed parser diagnostic, got {message:?}"
        );
    }
}
