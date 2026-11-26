// SPDX-License-Identifier: CC0-1.0

//! A request on the Amazon SimpleDB 2009-04-15 wire that presents NO credential at all.
//!
//! RULE: that API carries its caller's credential among the request's own form parameters
//! and sends no `Authorization` header, so a request of it whose credential is absent is
//! refused — before the action it names is considered at all — with the code and sentence
//! that API publishes for an unauthenticated call (`MissingAuthenticationToken`, HTTP 403),
//! stated in the API's OWN query error document (`<Response><Errors><Error><Code>…`) as
//! `text/xml`, never as the JSON `__type` document the modelled `/v2/*` operations answer
//! in.
//!
//! Why it matters, and why it is this surface's refusal rather than the router's: a client
//! of this wire parses every answer with the query/XML parser, so a JSON error document is
//! not an error it can read — it is a parse failure, and the code never reaches the
//! client's error branch. The emulator's own authenticator renders a refusal in the
//! service's modelled envelope (this service answers `restJson1`), so a request of this
//! API whose credential it cannot see has to be refused by the surface that serves this
//! wire.
//!
//! The rows span what the rule generalises over: each action of this API a credential-less
//! request can name — the ones this emulator serves, one it answers `501` for, and one the
//! API does not have — an otherwise incomplete request, and a credential field that is
//! present but empty, which presents nothing. The suites beside this one
//! (`sdb_signature_version_2_reached_endpoint`, `simpledb_query_api`) span the credentials
//! a request DOES present.
//!
//! Every case drives the real wire path: the production `MultiServiceRouter` with request
//! authentication ON, so what is asserted is what a SimpleDB client observes.

use service_com_amazonaws_simpledbv2_simpledbv2::service::sdb_query_api::{
    API_VERSION, SERVED_ACTIONS, UNSERVED_ACTIONS,
};

// The emulator that verifies every request, the key pair it publishes, the posting of a
// form with no `Authorization` header, the client-side signing of this scheme and the
// readings of this API's documents are the ones the suites beside this one already own:
// one copy of each, so no two suites can drift on what a client sends or on what an answer
// of this API says.
use crate::sdb_signature_version_2_reached_endpoint::{posted_form, signed_fields, Answer};
use crate::simpledb_query_api::{authenticating_emulator, published_credentials, text_of, texts_of};

/// The media type this API answers in, success and refusal alike.
const XML_CONTENT_TYPE: &str = "text/xml";

/// The address a client pointed at this emulator dials: a loopback authority, which names
/// no service at all — the address the credential-less requests below are sent to.
const CLIENT_HOST: &str = "127.0.0.1:9297";

/// The hash a current client of this API names in its `SignatureMethod`, used by the signed
/// control calls below.
const HMAC_SHA256: &str = "HmacSHA256";

/// The code this API publishes for a call carrying no credential, and the sentence it
/// states for it.
const MISSING_AUTHENTICATION_TOKEN: &str = "MissingAuthenticationToken";
const MISSING_AUTHENTICATION_SENTENCE: &str =
    "The request must contain either a valid (registered) AWS access key ID or X.509 \
     certificate.";

/// One row: the request a client sends with no usable credential in it.
struct UnauthenticatedCase {
    case: &'static str,
    parameters: Vec<(&'static str, &'static str)>,
}

/// Post `parameters` as the form body of a `POST "/"` with no `Authorization` header and no
/// credential computed for it — a request of this API that presents nothing.
async fn unauthenticated(
    router: &smithy_server_runtime::MultiServiceRouter,
    parameters: &[(&'static str, &'static str)],
) -> Answer {
    let fields: Vec<(String, String)> = parameters
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    posted_form(router, CLIENT_HOST, &fields).await
}

/// Assert `answer` is this API's own refusal of an unauthenticated call: HTTP 403, the
/// published code and sentence, in the query error envelope as `text/xml`, with nothing of
/// the modelled surface's JSON document in it.
fn assert_unauthenticated(answer: &Answer, case: &str) {
    let Answer {
        status,
        content_type,
        body,
    } = answer;
    assert_eq!(
        *status, 403,
        "case {case:?}: expected HTTP 403 for a request presenting no credential, got \
         {status} with {body}"
    );
    assert!(
        content_type.starts_with(XML_CONTENT_TYPE),
        "case {case:?}: a refusal on this wire is {XML_CONTENT_TYPE}, got {content_type:?} \
         with {body}"
    );
    assert!(
        !body.contains("__type"),
        "case {case:?}: a refusal on this wire is never the JSON error document of the \
         modelled surface, got {body}"
    );
    assert!(
        body.contains("<Response><Errors><Error>"),
        "case {case:?}: a refusal is this API's own query error envelope, got {body}"
    );
    // Read with a real XML parser, which is what a client of this wire reads the answer
    // with: a body it cannot parse never reaches the client's error branch at all.
    assert_eq!(
        text_of(body, "Code").as_deref(),
        Some(MISSING_AUTHENTICATION_TOKEN),
        "case {case:?}: expected the code this API publishes for an unauthenticated call, \
         got {body}"
    );
    assert_eq!(
        text_of(body, "Message").as_deref(),
        Some(MISSING_AUTHENTICATION_SENTENCE),
        "case {case:?}: expected the sentence this API states for \
         {MISSING_AUTHENTICATION_TOKEN}, got {body}"
    );
    assert!(
        text_of(body, "RequestID").is_some_and(|id| !id.is_empty()),
        "case {case:?}: the refusal names the request it answers, got {body}"
    );
}

/// The rows: one credential-less request per action this API publishes, plus the two ways a
/// request can be credential-less beside sending no credential field at all.
fn rows() -> Vec<UnauthenticatedCase> {
    let rows: Vec<UnauthenticatedCase> = vec![
        UnauthenticatedCase {
            case: "CreateDomain, the first call a client of this API makes",
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
            ],
        },
        UnauthenticatedCase {
            case: "PutAttributes, a write",
            parameters: vec![
                ("Action", "PutAttributes"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
                ("ItemName", "item-widget"),
                ("Attribute.0.Name", "colour"),
                ("Attribute.0.Value", "blue"),
            ],
        },
        UnauthenticatedCase {
            case: "GetAttributes, a read",
            parameters: vec![
                ("Action", "GetAttributes"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
                ("ItemName", "item-widget"),
            ],
        },
        UnauthenticatedCase {
            case: "DeleteAttributes, a delete",
            parameters: vec![
                ("Action", "DeleteAttributes"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
                ("ItemName", "item-widget"),
            ],
        },
        UnauthenticatedCase {
            case: "DeleteDomain",
            parameters: vec![
                ("Action", "DeleteDomain"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
            ],
        },
        UnauthenticatedCase {
            case: "DomainMetadata",
            parameters: vec![
                ("Action", "DomainMetadata"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
            ],
        },
        UnauthenticatedCase {
            case: "ListDomains, which takes no parameter of its own",
            parameters: vec![("Action", "ListDomains"), ("Version", API_VERSION)],
        },
        UnauthenticatedCase {
            case: "an action this API has that this emulator does not serve yet, whose own \
                   501 the credential is judged before",
            parameters: vec![("Action", "Select"), ("Version", API_VERSION)],
        },
        UnauthenticatedCase {
            case: "BatchPutAttributes, another action this emulator does not serve yet",
            parameters: vec![("Action", "BatchPutAttributes"), ("Version", API_VERSION)],
        },
        UnauthenticatedCase {
            case: "BatchDeleteAttributes, the last action this API publishes",
            parameters: vec![("Action", "BatchDeleteAttributes"), ("Version", API_VERSION)],
        },
        UnauthenticatedCase {
            case: "an action this API does not have, whose InvalidAction the credential is \
                   judged before",
            parameters: vec![("Action", "NotAnAction"), ("Version", API_VERSION)],
        },
        UnauthenticatedCase {
            case: "a request that is incomplete as well, whose MissingParameter the \
                   credential is judged before",
            parameters: vec![("Action", "CreateDomain"), ("Version", API_VERSION)],
        },
        UnauthenticatedCase {
            case: "an access key field that is there but empty, which presents nothing",
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "unauthenticated-domain"),
                ("SignatureVersion", "2"),
                ("AWSAccessKeyId", ""),
                ("Signature", "YmFzZTY0c2lnbmF0dXJl"),
            ],
        },
        UnauthenticatedCase {
            case: "a request naming no version either, so nothing but its action states \
                   this API",
            parameters: vec![
                ("Action", "CreateDomain"),
                ("DomainName", "unauthenticated-domain"),
            ],
        },
    ];
    // Every action this API publishes is covered by a row above: the rule is about the
    // request's credential, so no action of this surface may answer a credential-less
    // request anything else.
    for action in SERVED_ACTIONS.iter().chain(UNSERVED_ACTIONS) {
        assert!(
            rows.iter()
                .any(|row| row.parameters.contains(&("Action", action))),
            "every action this API publishes needs a row of its own, and {action} has none"
        );
    }
    rows
}

/// The domain names a signed `ListDomains` reports, sorted — the read-back every case's
/// claim about what was written is judged by.
async fn listed_domains(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let (key_id, secret) = published_credentials();
    let fields = signed_fields(
        CLIENT_HOST,
        HMAC_SHA256,
        &key_id,
        &secret,
        &[("Action", "ListDomains"), ("Version", API_VERSION)],
    );
    let answer = posted_form(router, CLIENT_HOST, &fields).await;
    assert_eq!(
        answer.status, 200,
        "reading the account's domains back must be served, got {} with {}",
        answer.status, answer.body
    );
    let mut names = texts_of(&answer.body, "DomainName");
    names.sort();
    names
}

#[tokio::test]
async fn a_request_presenting_no_credential_is_refused_in_this_apis_own_error_document() {
    let router = authenticating_emulator();
    for row in rows() {
        let answer = unauthenticated(&router, &row.parameters).await;
        assert_unauthenticated(&answer, row.case);
    }

    // Nothing any row named was created: a refused request writes nothing, and the signed
    // read that proves it is served by the same emulator — so the refusals above are about
    // the credential the rows did not present, not about the emulator refusing this wire.
    assert!(
        listed_domains(&router).await.is_empty(),
        "case after every row: a request refused for its credential writes nothing"
    );
}

/// RULE, the other half: the refusal is about the CREDENTIAL, so the very same request,
/// signed the way this API publishes, is served — and what it then writes is readable back.
#[tokio::test]
async fn the_same_request_signed_is_served_and_its_write_is_read_back() {
    let router = authenticating_emulator();
    let (key_id, secret) = published_credentials();
    let domain = "credential-decides-domain";

    // Unsigned: refused for the credential.
    let refused = unauthenticated(
        &router,
        &[
            ("Action", "CreateDomain"),
            ("Version", API_VERSION),
            ("DomainName", domain),
        ],
    )
    .await;
    assert_unauthenticated(&refused, "the unsigned call");
    assert!(
        listed_domains(&router).await.is_empty(),
        "case the unsigned call: it must have created nothing, got the domains above"
    );

    // Signed: served, in this API's own namespace, and the domain is one the account holds.
    for (case, parameters) in [
        (
            "CreateDomain",
            vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", domain),
            ],
        ),
        (
            "PutAttributes",
            vec![
                ("Action", "PutAttributes"),
                ("Version", API_VERSION),
                ("DomainName", domain),
                ("ItemName", "item-widget"),
                ("Attribute.0.Name", "colour"),
                ("Attribute.0.Value", "blue"),
            ],
        ),
    ] {
        let fields = signed_fields(CLIENT_HOST, HMAC_SHA256, &key_id, &secret, &parameters);
        let answer = posted_form(&router, CLIENT_HOST, &fields).await;
        assert_eq!(
            answer.status, 200,
            "case {case} signed: expected HTTP 200, got {} with {}",
            answer.status, answer.body
        );
        assert!(
            answer.body.contains(&format!("<{case}Response")),
            "case {case} signed: expected that action's own response envelope, got {}",
            answer.body
        );
    }
    assert_eq!(
        listed_domains(&router).await,
        vec![domain.to_string()],
        "case the signed calls: the domain they created must be one the account holds"
    );
}
