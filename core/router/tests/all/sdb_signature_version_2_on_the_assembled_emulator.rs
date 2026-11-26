// SPDX-License-Identifier: CC0-1.0

//! The Amazon SimpleDB 2009-04-15 AWS Query API as a conforming client reaches it on the
//! ASSEMBLED emulator with request authentication ON — the emulator a client is pointed at.
//!
//! RULE: that API publishes Signature Version 2, which carries the caller's credential
//! among the request's own form parameters and sends no `Authorization` header at all, so a
//! request whose parameters sign to the signature it presents is SERVED the called action's
//! 2009-04-15 XML document, and a request of that API that presents no credential is
//! refused in that API's OWN query error document — `<Response><Errors><Error><Code>…` —
//! never in the JSON `__type` envelope the modelled `/v2/*` operations answer in.
//!
//! Why this suite lives here rather than in the simpledbv2 crate: the per-service suites
//! build a router holding that service alone, and a credential is verified by the ROUTER —
//! so the only place the two halves meet as a client meets them is the assembled emulator,
//! every service mounted, authentication on. That is where the defect this suite pins was
//! observed: the request was refused `AccessDenied` before any handler ran, in a JSON body
//! a query-protocol client's parser rejects outright, so no client could create a domain and
//! the whole export surface that takes one was unreachable.
//!
//! The rows span what the rule generalises over: how the request names this endpoint (a
//! loopback authority, which names no service, and the published endpoint host), the action
//! called across one domain's whole lifecycle, and values carrying characters the canonical
//! signing string has to escape — plus, for the refusal table, each action a
//! credential-less request can name.
//!
//! The request plumbing (how a query call is addressed, posted and read) is
//! `zz_sdb_probe.rs`; the SIGNING is this suite's, because no suite of this test crate signs
//! this scheme yet.

use std::collections::BTreeMap;

use crate::query_wire::{elements, form_encode, opt_element};
use crate::zz_sdb_probe::{
    emulator_verifying, on_a_sized_thread, query, Addressed, Answer, API_VERSION, SDB_NAMESPACE,
};

/// The credential parameters Signature Version 2 travels in, as a client adds them.
const ACCESS_KEY_ID_PARAMETER: &str = "AWSAccessKeyId";
const SIGNATURE_VERSION_PARAMETER: &str = "SignatureVersion";
const SIGNATURE_METHOD_PARAMETER: &str = "SignatureMethod";
const TIMESTAMP_PARAMETER: &str = "Timestamp";
const SIGNATURE_PARAMETER: &str = "Signature";

/// The one signature version this API publishes, and the hash a current client names.
const SIGNATURE_VERSION: &str = "2";
const HMAC_SHA256: &str = "HmacSHA256";

/// The instant a case's credential states. It is inside the string the signature covers, so
/// it cannot be altered without invalidating the signature; its value is the caller's.
const TIMESTAMP: &str = "2026-01-01T00:00:00Z";

/// The code and sentence this API publishes for a call carrying no credential.
const MISSING_AUTHENTICATION_TOKEN: &str = "MissingAuthenticationToken";
const MISSING_AUTHENTICATION_SENTENCE: &str =
    "The request must contain either a valid (registered) AWS access key ID or X.509 \
     certificate.";

/// The key pair this emulator accepts: the one it publishes at start, which is what a client
/// of it is configured with.
fn published_credentials() -> (String, String) {
    let published = ::emulator_server::request_authentication::SigV4Authenticator::minted_credentials()
        .expect("an authenticating emulator publishes its key pair");
    (
        published.access_key_id.clone(),
        published.secret_access_key.clone(),
    )
}

/// `parameters` plus the Signature Version 2 credential a client computes for the authority
/// `host` — the signing done the way a CLIENT does it, from the scheme's published
/// definition rather than from the service's own code.
fn signed_fields(host: &str, parameters: &[(&str, &str)]) -> Vec<(String, String)> {
    let (key_id, secret) = published_credentials();
    let mut fields: Vec<(String, String)> = parameters
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    fields.push((ACCESS_KEY_ID_PARAMETER.to_string(), key_id));
    fields.push((
        SIGNATURE_VERSION_PARAMETER.to_string(),
        SIGNATURE_VERSION.to_string(),
    ));
    fields.push((
        SIGNATURE_METHOD_PARAMETER.to_string(),
        HMAC_SHA256.to_string(),
    ));
    fields.push((TIMESTAMP_PARAMETER.to_string(), TIMESTAMP.to_string()));
    let signature = signature_of(host, &secret, &fields);
    fields.push((SIGNATURE_PARAMETER.to_string(), signature));
    fields
}

/// The signature those fields carry: the base64 HMAC-SHA256, under the caller's secret, of
/// the canonical string this scheme defines — the HTTP method, the authority addressed, the
/// path, then every parameter sorted by name and percent-encoded down to RFC 3986's
/// unreserved set ([`form_encode`]).
///
/// The HMAC itself is computed by the maintained `aws-sigv4` crate this test crate already
/// declares, which renders a MAC in hex; it is decoded and re-encoded in the base64 this
/// scheme carries a signature in. (`core/router/Cargo.toml` is a pinned path in this
/// repository, so this test crate can take no dependency of its own — see the note in
/// `query_wire.rs`.)
fn signature_of(host: &str, secret: &str, fields: &[(String, String)]) -> String {
    let canonical: BTreeMap<&str, &str> = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let query = canonical
        .iter()
        .map(|(name, value)| format!("{}={}", form_encode(name), form_encode(value)))
        .collect::<Vec<String>>()
        .join("&");
    let string_to_sign = format!("POST\n{host}\n/\n{query}");
    let rendered = ::aws_sigv4::sign::v4::calculate_signature(
        secret.as_bytes(),
        string_to_sign.as_bytes(),
    );
    let mac = ::hex::decode(rendered).expect("the crate renders the MAC in hex");
    ::aws_smithy_types::base64::encode(&mac)
}

/// One call of this API as a conforming client makes it: `parameters` signed for the
/// authority `addressed` sends the request to, and posted there with no `Authorization`
/// header.
fn signed_query(
    router: &smithy_server_runtime::MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    addressed: Addressed,
    parameters: &[(&str, &str)],
) -> Answer {
    let fields = signed_fields(&addressed.host(), parameters);
    let borrowed: Vec<(&str, &str)> = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    query(router, runtime, addressed, &borrowed)
}

/// One call that must be SERVED: HTTP 200 and that action's own response envelope in this
/// API's namespace, naming the request it answers.
fn assert_served(answer: &Answer, case: &str, action: &str) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected HTTP 200 from the {action} this client signed, got {answer:?}"
    );
    assert!(
        answer
            .body
            .contains(&format!("<{action}Response xmlns=\"{SDB_NAMESPACE}\">")),
        "case {case}: expected the {action}Response envelope in the 2009-04-15 namespace, \
         got {answer:?}"
    );
    assert!(
        opt_element(&answer.body, "RequestId").is_some_and(|id| !id.is_empty()),
        "case {case}: every answer names the request it answers, got {answer:?}"
    );
}

/// One call that must be refused as unauthenticated: this API's own error document, with the
/// published code and sentence, and nothing of the modelled surface's JSON envelope.
fn assert_unauthenticated(answer: &Answer, case: &str) {
    assert_eq!(
        answer.status, 403,
        "case {case}: expected HTTP 403 for a request presenting no credential, got {answer:?}"
    );
    assert!(
        !answer.body.contains("__type"),
        "case {case}: a refusal on this wire is never the JSON error document of the \
         modelled surface, got {answer:?}"
    );
    assert!(
        answer.body.contains("<Response><Errors><Error>"),
        "case {case}: a refusal is this API's own query error envelope, got {answer:?}"
    );
    assert_eq!(
        opt_element(&answer.body, "Code").as_deref(),
        Some(MISSING_AUTHENTICATION_TOKEN),
        "case {case}: expected the code this API publishes for an unauthenticated call, got \
         {answer:?}"
    );
    assert_eq!(
        opt_element(&answer.body, "Message").as_deref(),
        Some(MISSING_AUTHENTICATION_SENTENCE),
        "case {case}: expected the sentence this API states for \
         {MISSING_AUTHENTICATION_TOKEN}, got {answer:?}"
    );
    assert!(
        opt_element(&answer.body, "RequestID").is_some_and(|id| !id.is_empty()),
        "case {case}: the refusal names the request it answers, got {answer:?}"
    );
}

/// The attribute values one case writes and reads back: a plain value, ones carrying the
/// characters the canonical signing string has to escape, and one outside ASCII — so what
/// the signature covers is the request a client really sends.
fn attribute_values() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a plain value", "blue"),
        ("a value carrying spaces", "deep blue grey"),
        ("a value carrying reserved characters", "a/b+c=d?e:f"),
        ("a value outside ASCII", "bleu profond \u{e9}t\u{e9}"),
    ]
}

/// A conforming Signature Version 2 client is served the whole 2009-04-15 lifecycle on the
/// assembled, authenticating emulator — whatever address it dialled this emulator on.
#[test]
fn a_conforming_signature_version_2_client_is_served_on_the_assembled_emulator() {
    on_a_sized_thread("sdb-sigv2-assembled-served", || {
        let (router, runtime) = emulator_verifying();

        for addressed in [Addressed::Nothing, Addressed::EndpointHost] {
            for (index, (value_case, value)) in attribute_values().into_iter().enumerate() {
                let case = format!("{value_case} addressed {addressed:?}");
                let domain = format!("{}-sigv2-domain-{index}", addressed.tag());

                let created = signed_query(
                    &router,
                    &runtime,
                    addressed,
                    &[
                        ("Action", "CreateDomain"),
                        ("Version", API_VERSION),
                        ("DomainName", &domain),
                    ],
                );
                assert_served(&created, &case, "CreateDomain");

                let written = signed_query(
                    &router,
                    &runtime,
                    addressed,
                    &[
                        ("Action", "PutAttributes"),
                        ("Version", API_VERSION),
                        ("DomainName", &domain),
                        ("ItemName", "item-widget"),
                        ("Attribute.0.Name", "colour"),
                        ("Attribute.0.Value", value),
                    ],
                );
                assert_served(&written, &case, "PutAttributes");

                let read = signed_query(
                    &router,
                    &runtime,
                    addressed,
                    &[
                        ("Action", "GetAttributes"),
                        ("Version", API_VERSION),
                        ("DomainName", &domain),
                        ("ItemName", "item-widget"),
                    ],
                );
                assert_served(&read, &case, "GetAttributes");
                assert_eq!(
                    opt_element(&read.body, "Value").as_deref(),
                    Some(value),
                    "case {case}: the value written over this wire must be read back \
                     verbatim, got {read:?}"
                );

                let listed = signed_query(
                    &router,
                    &runtime,
                    addressed,
                    &[("Action", "ListDomains"), ("Version", API_VERSION)],
                );
                assert_served(&listed, &case, "ListDomains");
                assert!(
                    elements(&listed.body, "DomainName").contains(&domain),
                    "case {case}: the domain this client created must be one the account \
                     holds, got {listed:?}"
                );

                let deleted = signed_query(
                    &router,
                    &runtime,
                    addressed,
                    &[
                        ("Action", "DeleteDomain"),
                        ("Version", API_VERSION),
                        ("DomainName", &domain),
                    ],
                );
                assert_served(&deleted, &case, "DeleteDomain");
                let after = signed_query(
                    &router,
                    &runtime,
                    addressed,
                    &[("Action", "ListDomains"), ("Version", API_VERSION)],
                );
                assert!(
                    !elements(&after.body, "DomainName").contains(&domain),
                    "case {case}: the deleted domain must be gone from the listing, got \
                     {after:?}"
                );
            }
        }
    });
}

/// A request of this API that presents NO credential is refused in this API's own error
/// document — whatever action it names, and whether or not it states this API's version —
/// and it writes nothing.
#[test]
fn an_unauthenticated_request_on_this_wire_is_refused_in_the_apis_own_document() {
    on_a_sized_thread("sdb-sigv2-assembled-refused", || {
        let (router, runtime) = emulator_verifying();

        let rows: Vec<(&str, Vec<(&str, &str)>)> = vec![
            (
                "CreateDomain, the first call a client makes",
                vec![
                    ("Action", "CreateDomain"),
                    ("Version", API_VERSION),
                    ("DomainName", "unauthenticated-domain"),
                ],
            ),
            (
                "PutAttributes, a write",
                vec![
                    ("Action", "PutAttributes"),
                    ("Version", API_VERSION),
                    ("DomainName", "unauthenticated-domain"),
                    ("ItemName", "item-widget"),
                    ("Attribute.0.Name", "colour"),
                    ("Attribute.0.Value", "blue"),
                ],
            ),
            (
                "GetAttributes, a read",
                vec![
                    ("Action", "GetAttributes"),
                    ("Version", API_VERSION),
                    ("DomainName", "unauthenticated-domain"),
                    ("ItemName", "item-widget"),
                ],
            ),
            (
                "ListDomains, which takes no parameter of its own",
                vec![("Action", "ListDomains"), ("Version", API_VERSION)],
            ),
            (
                "an action this API has that this emulator does not serve yet",
                vec![("Action", "Select"), ("Version", API_VERSION)],
            ),
            (
                "an action this API does not have",
                vec![("Action", "NotAnAction"), ("Version", API_VERSION)],
            ),
            (
                "a request naming no version either, so only its action states this API",
                vec![
                    ("Action", "CreateDomain"),
                    ("DomainName", "unauthenticated-domain"),
                ],
            ),
        ];

        for (case, parameters) in rows {
            let answer = query(&router, &runtime, Addressed::Nothing, &parameters);
            assert_unauthenticated(&answer, case);
        }

        // The refusals above are about the credential, not about this wire: the same
        // emulator serves a signed read, and it shows no row wrote anything.
        let listed = signed_query(
            &router,
            &runtime,
            Addressed::Nothing,
            &[("Action", "ListDomains"), ("Version", API_VERSION)],
        );
        assert_served(&listed, "after every row", "ListDomains");
        assert!(
            elements(&listed.body, "DomainName").is_empty(),
            "case after every row: a request refused for its credential writes nothing, got \
             {listed:?}"
        );
    });
}
