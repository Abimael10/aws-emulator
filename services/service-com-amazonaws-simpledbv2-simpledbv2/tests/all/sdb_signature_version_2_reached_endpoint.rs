// SPDX-License-Identifier: CC0-1.0

//! The Amazon SimpleDB 2009-04-15 API as reached by a client POINTED AT THIS EMULATOR:
//! Signature Version 2 over whatever address that client dialled.
//!
//! RULE: a request of that API is authenticated under the credential it carries among its
//! own form parameters — Signature Version 2 — whose signature is of the request alone, the
//! AUTHORITY it addressed included, so a request whose parameters sign to the signature it
//! presents is SERVED whatever address it was sent to, and every refusal of such a request
//! is that API's own query XML error document (`<Response><Errors><Error><Code>…`), never a
//! JSON `__type` document of the modelled `/v2/*` surface.
//!
//! The suite beside this one (`simpledb_query_api`) drives that wire at the service's
//! published endpoint host. What is pinned HERE is the address a client of this emulator
//! actually dials: a loopback authority, which names no service in the host and carries no
//! SigV4 credential scope either, so the request is claimed by the query wire form alone and
//! its signature is verified over that authority — port and all, which is what the scheme
//! signs (`POST`, the authority, the path, then the parameters).
//!
//! The rows span what the rule generalises over: the FORM of the authority the client
//! addressed and signed for (an IPv4 loopback, the same by name, an IPv6 loopback, another
//! port, the service's own endpoint host), the hash its `SignatureMethod` names, and — for
//! the refusal table — each way a request on that wire can be refused, from the credential
//! it presents through to the state the action is about.
//!
//! Every case drives the real wire path: the production `MultiServiceRouter` with request
//! authentication ON, the service's request-claiming seam and the handlers, so what is
//! asserted is what a SimpleDB client observes.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_simpledbv2_simpledbv2::service::sdb_query_api::API_VERSION;
// The published domain-name bounds a refusal of this API states, so no row writes a limit
// of its own.
use service_com_amazonaws_simpledbv2_simpledbv2::state::simpledb_domains::{
    MAX_DOMAIN_NAME_LEN, MIN_DOMAIN_NAME_LEN,
};

// The readings of this API's documents, and the emulator that verifies every request, are
// the ones the suite beside this one already owns: one copy of each, so the two suites
// cannot drift on what an answer of this API says or on what a client of it is configured
// with. The SIGNING is this suite's, because the authority is what it spans — and that one
// copy is what `simpledb_query_api` signs with too.
use crate::simpledb_query_api::{
    attribute_pairs, authenticating_emulator, published_credentials, text_of, texts_of,
    SDB_NAMESPACE,
};

/// The media type this API answers in, success and refusal alike.
const XML_CONTENT_TYPE: &str = "text/xml";

/// The emulator's own address, as a client pointed at it dials one: a loopback authority,
/// which names no service at all.
const LOOPBACK: &str = "127.0.0.1:9297";

/// The characters Signature Version 2 leaves unescaped in the string it signs: RFC 3986's
/// unreserved set.
const CANONICAL_UNRESERVED: &::percent_encoding::AsciiSet = &::percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// The hashes this API's `SignatureMethod` publishes.
const HMAC_SHA1: &str = "HmacSHA1";
const HMAC_SHA256: &str = "HmacSHA256";

/// The sentence this API states for a signature that is not the one the request's own
/// parameters sign to — the refusal two tables below both expect.
const SIGNATURE_DOES_NOT_MATCH: &str =
    "The request signature we calculated does not match the signature you provided. Check \
     your AWS Secret Access Key and signing method.";

/// How a case presents its credential. The key pair this emulator accepts is the one it
/// publishes at start ([`published_credentials`]), which is what a client of it is
/// configured with.
#[derive(Clone, Copy, Debug)]
enum Credential {
    /// The published access key, signed under its own secret — a conforming client.
    Published,
    /// The published access key, signed under another secret.
    AnotherSecret,
    /// An access key this emulator never minted, signed under the published secret.
    UnmintedKey,
    /// The fields exactly as written, with no signature computed at all — how a case about
    /// an incomplete credential presents one.
    AsWritten,
}

/// What the emulator answered: the status, the media type it stated, and the body.
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) content_type: String,
    pub(crate) body: String,
}

/// The fields a client posts to sign `parameters` for the authority `host`.
///
/// The ONE computation of this scheme's signature in this crate's suites, done the way a
/// CLIENT does it — from the scheme's published definition rather than from the service's own
/// code: the base64 HMAC, under `secret`, of the HTTP method, the authority the request is
/// sent to, the path, and every other parameter sorted by name and percent-encoded down to
/// the unreserved set.
pub(crate) fn signed_fields(
    host: &str,
    hash: &str,
    key_id: &str,
    secret: &str,
    parameters: &[(&str, &str)],
) -> Vec<(String, String)> {
    use ::base64::Engine as _;
    use ::hmac::Mac as _;

    let mut fields: Vec<(String, String)> = parameters
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect();
    // The credential parameters Signature Version 2 travels in, as a client adds them.
    fields.push(("AWSAccessKeyId".to_string(), key_id.to_string()));
    fields.push(("SignatureVersion".to_string(), "2".to_string()));
    fields.push(("SignatureMethod".to_string(), hash.to_string()));
    fields.push(("Timestamp".to_string(), "2026-01-01T00:00:00Z".to_string()));
    let mut canonical = fields.clone();
    canonical.sort_by(|(left, _), (right, _)| left.cmp(right));
    let encode =
        |text: &str| ::percent_encoding::utf8_percent_encode(text, CANONICAL_UNRESERVED).to_string();
    let query = canonical
        .iter()
        .map(|(name, value)| format!("{}={}", encode(name), encode(value)))
        .collect::<Vec<String>>()
        .join("&");
    let string_to_sign = format!("POST\n{host}\n/\n{query}");
    let signature = match hash {
        HMAC_SHA1 => {
            let mut mac =
                <::hmac::Hmac<::sha1::Sha1> as ::hmac::Mac>::new_from_slice(secret.as_bytes())
                    .expect("HMAC takes a key of any length");
            mac.update(string_to_sign.as_bytes());
            ::base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        }
        _ => {
            let mut mac =
                <::hmac::Hmac<::sha2::Sha256> as ::hmac::Mac>::new_from_slice(secret.as_bytes())
                    .expect("HMAC takes a key of any length");
            mac.update(string_to_sign.as_bytes());
            ::base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
        }
    };
    fields.push(("Signature".to_string(), signature));
    fields
}

/// `parameters` signed for `host` with the credential `credential` describes, and posted
/// there — one call as a conforming client of this API makes it.
async fn signed_query(
    router: &MultiServiceRouter,
    host: &str,
    hash: &str,
    credential: Credential,
    parameters: &[(&str, &str)],
) -> Answer {
    let (published_key, published_secret) = published_credentials();
    let (key_id, secret) = match credential {
        Credential::Published => (published_key, published_secret),
        Credential::AnotherSecret => (published_key, format!("{published_secret}-not-it")),
        Credential::UnmintedKey => ("AKIDNEVERMINTEDHERE".to_string(), published_secret),
        Credential::AsWritten => {
            let written: Vec<(String, String)> = parameters
                .iter()
                .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                .collect();
            return posted_form(router, host, &written).await;
        }
    };
    let fields = signed_fields(host, hash, &key_id, &secret, parameters);
    posted_form(router, host, &fields).await
}

/// One request signed for the authority `signed_for` but SENT to `sent_to` — the same
/// signing a conforming client does, addressed somewhere else.
async fn misdirected_query(
    router: &MultiServiceRouter,
    signed_for: &str,
    sent_to: &str,
    parameters: &[(&str, &str)],
) -> Answer {
    let (key_id, secret) = published_credentials();
    let fields = signed_fields(signed_for, HMAC_SHA256, &key_id, &secret, parameters);
    posted_form(router, sent_to, &fields).await
}

/// One `POST "/"` to `host` carrying `fields` as its form body and NO `Authorization`
/// header — which is what a Signature Version 2 request is: its credential travels in the
/// body.
pub(crate) async fn posted_form(
    router: &MultiServiceRouter,
    host: &str,
    fields: &[(String, String)],
) -> Answer {
    let mut form = ::form_urlencoded::Serializer::new(String::new());
    for (name, value) in fields {
        form.append_pair(name, value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header(
            "content-type",
            "application/x-www-form-urlencoded; charset=utf-8",
        )
        .body(bytes::Bytes::from(form.finish()))
        .expect("a well-formed test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body = String::from_utf8_lossy(&response.into_body()).to_string();
    Answer {
        status,
        content_type,
        body,
    }
}

/// Assert `answer` is this API's own answer for `action`: HTTP 200, the action's response
/// element in the 2009-04-15 namespace, and the `ResponseMetadata` every answer carries.
fn assert_served(answer: &Answer, case: &str, action: &str) {
    let Answer {
        status,
        content_type,
        body,
    } = answer;
    assert_eq!(
        *status, 200,
        "case {case:?}: expected HTTP 200 from the {action} this client signed, got {status} \
         with {body}"
    );
    assert!(
        content_type.starts_with(XML_CONTENT_TYPE),
        "case {case:?}: this API answers {XML_CONTENT_TYPE}, got {content_type:?} with {body}"
    );
    assert!(
        body.contains(&format!("<{action}Response xmlns=\"{SDB_NAMESPACE}\">")),
        "case {case:?}: expected the {action}Response envelope in the 2009-04-15 namespace, \
         got {body}"
    );
    assert!(
        text_of(body, "RequestId").is_some_and(|id| !id.is_empty()),
        "case {case:?}: every answer names the request it answers, got {body}"
    );
    assert!(
        text_of(body, "BoxUsage").is_some_and(|usage| usage.parse::<f64>().is_ok()),
        "case {case:?}: every answer reports a BoxUsage, got {body}"
    );
}

/// Assert `answer` is this API's own REFUSAL document — the query error envelope — naming
/// `code` and stating `message` at `status`, and nothing of the modelled surface's JSON error
/// document.
fn assert_refused_in_query_envelope(
    answer: &Answer,
    case: &str,
    status: u16,
    code: &str,
    message: &str,
) {
    let Answer {
        status: answered,
        content_type,
        body,
    } = answer;
    assert_eq!(
        *answered, status,
        "case {case:?}: expected HTTP {status}, got {answered} with {body}"
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
    assert_eq!(
        text_of(body, "Code").as_deref(),
        Some(code),
        "case {case:?}: expected the code {code} in this API's own error envelope, got {body}"
    );
    assert_eq!(
        text_of(body, "Message").as_deref(),
        Some(message),
        "case {case:?}: expected the sentence this API publishes for {code}, got {body}"
    );
    assert!(
        text_of(body, "RequestID").is_some_and(|id| !id.is_empty()),
        "case {case:?}: the refusal names the request it answers, got {body}"
    );
}

/// The names `ListDomains` reports over the signed wire at `host`, sorted.
async fn listed_domains(router: &MultiServiceRouter, host: &str) -> Vec<String> {
    let answer = signed_query(
        router,
        host,
        HMAC_SHA256,
        Credential::Published,
        &[("Action", "ListDomains"), ("Version", API_VERSION)],
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "reading back the account's domains must be served, got {} with {}",
        answer.status, answer.body
    );
    let mut names = texts_of(&answer.body, "DomainName");
    names.sort();
    names
}

/// One row of the addressed-authority table: the authority the client dialled and signed
/// for, the hash it signed with, and the domain it works on.
struct AuthorityCase {
    case: &'static str,
    host: &'static str,
    hash: &'static str,
    domain: &'static str,
}

/// RULE: the Signature Version 2 a request of this API carries is verified over the request
/// itself — the authority it was sent to included — so a conforming client is served at
/// whatever address it dialled this emulator on, including a loopback authority that names no
/// service at all, and what it writes there is what it reads back.
///
/// The rows span the forms of that authority: an IPv4 loopback with the emulator's own port,
/// the same by name, an IPv6 loopback, another port, and the service's published endpoint
/// host — with both hashes this API's `SignatureMethod` publishes across them.
#[tokio::test]
async fn a_conforming_client_is_served_at_whatever_address_it_dialled() {
    let rows = [
        AuthorityCase {
            case: "the emulator's own loopback address, which names no service",
            host: LOOPBACK,
            hash: HMAC_SHA256,
            domain: "loopback-domain",
        },
        AuthorityCase {
            case: "the loopback address written by name",
            host: "localhost:9297",
            hash: HMAC_SHA256,
            domain: "localhost-domain",
        },
        AuthorityCase {
            case: "an IPv6 loopback address",
            host: "[::1]:9297",
            hash: HMAC_SHA1,
            domain: "ipv6-domain",
        },
        AuthorityCase {
            case: "a loopback address on another port",
            host: "127.0.0.1:4566",
            hash: HMAC_SHA256,
            domain: "other-port-domain",
        },
        AuthorityCase {
            case: "the service's own published endpoint host",
            host: "sdb.us-west-2.amazonaws.com",
            hash: HMAC_SHA1,
            domain: "endpoint-host-domain",
        },
    ];

    let router = authenticating_emulator();
    let mut created: Vec<String> = Vec::new();
    for row in &rows {
        let AuthorityCase {
            case,
            host,
            hash,
            domain,
        } = row;
        let created_domain = signed_query(
            &router,
            host,
            hash,
            Credential::Published,
            &[
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", domain),
            ],
        )
        .await;
        assert_served(&created_domain, case, "CreateDomain");

        // What the client does next: write an item and read it back, so the domain the
        // signed call created is a domain of the ONE store this endpoint answers from.
        let written = signed_query(
            &router,
            host,
            hash,
            Credential::Published,
            &[
                ("Action", "PutAttributes"),
                ("Version", API_VERSION),
                ("DomainName", domain),
                ("ItemName", "item-widget"),
                ("Attribute.0.Name", "color"),
                ("Attribute.0.Value", "blue"),
            ],
        )
        .await;
        assert_served(&written, case, "PutAttributes");

        let read = signed_query(
            &router,
            host,
            hash,
            Credential::Published,
            &[
                ("Action", "GetAttributes"),
                ("Version", API_VERSION),
                ("DomainName", domain),
                ("ItemName", "item-widget"),
            ],
        )
        .await;
        assert_served(&read, case, "GetAttributes");
        assert_eq!(
            attribute_pairs(&read.body),
            vec![("color".to_string(), "blue".to_string())],
            "case {case:?}: the pair written over this wire must be read back verbatim, got {}",
            read.body
        );

        created.push((*domain).to_string());
        created.sort();
        assert_eq!(
            listed_domains(&router, host).await,
            created,
            "case {case:?}: every domain created over this wire must be one the account \
             holds, whichever address it was created from"
        );
    }
}

/// One row of the refusal table: how the request is written, how its credential is
/// presented, and the status, published code and sentence it must be refused with.
///
/// The sentence's variable parts are taken from the ROW's own request (the value and
/// parameter it named, the action, the version), and the bounds a domain-name refusal states
/// come from the published constants rather than being written out again.
struct RefusalCase {
    case: &'static str,
    credential: Credential,
    parameters: Vec<(&'static str, &'static str)>,
    status: u16,
    code: &'static str,
    message: String,
}

/// RULE: every refusal of a request that arrived in this API's wire form is stated in this
/// API's OWN query error envelope — `<Response><Errors><Error><Code>…<Message>…` with the
/// request id beside it, as `text/xml` — never as the JSON `__type` error document the
/// modelled `/v2/*` operations answer in, so a query-protocol client can read the code and
/// branch on it.
///
/// The rows span the ways such a request can be refused, from the outside in: the signature
/// version it states, the access key and signature it presents, the signature itself, the
/// action and version it names, the parameters it carries, and the state the action is about.
/// Each is addressed to the emulator's own loopback authority — the address a client of this
/// emulator dials — so the refusal is the one that client sees.
#[tokio::test]
async fn every_refusal_on_this_wire_is_this_apis_own_error_document() {
    let rows = vec![
        RefusalCase {
            case: "a signature computed under another secret",
            credential: Credential::AnotherSecret,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "refused-domain"),
            ],
            status: 403,
            code: "SignatureDoesNotMatch",
            message: SIGNATURE_DOES_NOT_MATCH.to_string(),
        },
        RefusalCase {
            case: "an access key this emulator never minted",
            credential: Credential::UnmintedKey,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "refused-domain"),
            ],
            status: 403,
            code: "InvalidClientTokenId",
            message: "The AWS Access Key Id you provided does not exist in our records."
                .to_string(),
        },
        RefusalCase {
            case: "a signature version this API does not publish",
            credential: Credential::AsWritten,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "refused-domain"),
                ("SignatureVersion", "1"),
                ("AWSAccessKeyId", "AKIDEXAMPLE"),
                ("Signature", "abc"),
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: "Value (\"1\") for parameter SignatureVersion is invalid. \
                      SignatureVersion must be 2."
                .to_string(),
        },
        RefusalCase {
            case: "the scheme stated with no access key at all",
            credential: Credential::AsWritten,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "refused-domain"),
                ("SignatureVersion", "2"),
            ],
            status: 403,
            code: "MissingAuthenticationToken",
            message: "The request must contain either a valid (registered) AWS access key ID \
                      or X.509 certificate."
                .to_string(),
        },
        RefusalCase {
            case: "an access key with no signature beside it",
            credential: Credential::AsWritten,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "refused-domain"),
                ("SignatureVersion", "2"),
                ("AWSAccessKeyId", "AKIDEXAMPLE"),
            ],
            status: 403,
            code: "IncompleteSignature",
            message: "The request signature does not conform to AWS standards.".to_string(),
        },
        RefusalCase {
            case: "a signed request naming an action this API does not have",
            credential: Credential::Published,
            parameters: vec![("Action", "NotAnAction"), ("Version", API_VERSION)],
            status: 400,
            code: "InvalidAction",
            message: "The action NotAnAction is not valid for this web service.".to_string(),
        },
        RefusalCase {
            case: "a signed request naming an action this emulator does not serve yet",
            credential: Credential::Published,
            parameters: vec![("Action", "Select"), ("Version", API_VERSION)],
            status: 501,
            code: "NotImplemented",
            message: "action Select is not implemented in this emulator".to_string(),
        },
        RefusalCase {
            case: "a signed request naming a version no Amazon SimpleDB API declares",
            credential: Credential::Published,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", "2011-01-01"),
                ("DomainName", "refused-domain"),
            ],
            status: 400,
            code: "NoSuchVersion",
            message: "The requested version (2011-01-01) is not valid.".to_string(),
        },
        RefusalCase {
            case: "a signed request leaving out the parameter its action requires",
            credential: Credential::Published,
            parameters: vec![("Action", "CreateDomain"), ("Version", API_VERSION)],
            status: 400,
            code: "MissingParameter",
            message: "The request must contain the parameter DomainName".to_string(),
        },
        RefusalCase {
            case: "a signed request carrying a domain name this API does not admit",
            credential: Credential::Published,
            parameters: vec![
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "no"),
            ],
            status: 400,
            code: "InvalidParameterValue",
            message: format!(
                "Value (\"no\") for parameter DomainName is invalid. DomainName must be \
                 between {MIN_DOMAIN_NAME_LEN} and {MAX_DOMAIN_NAME_LEN} characters long and \
                 may contain only a-z, A-Z, 0-9, '_', '-' and '.'."
            ),
        },
        RefusalCase {
            case: "a signed read of a domain the account does not hold",
            credential: Credential::Published,
            parameters: vec![
                ("Action", "GetAttributes"),
                ("Version", API_VERSION),
                ("DomainName", "never-created-domain"),
                ("ItemName", "item-widget"),
            ],
            status: 400,
            code: "NoSuchDomain",
            message: "The specified domain does not exist.".to_string(),
        },
    ];

    let router = authenticating_emulator();
    for row in &rows {
        let RefusalCase {
            case,
            credential,
            parameters,
            status,
            code,
            message,
        } = row;
        let answer = signed_query(&router, LOOPBACK, HMAC_SHA256, *credential, parameters).await;
        assert_refused_in_query_envelope(&answer, case, *status, code, message);
    }

    assert!(
        listed_domains(&router, LOOPBACK).await.is_empty(),
        "case after every row: a refused request writes nothing"
    );
}

/// RULE: the authority the request addressed is INSIDE the string its signature covers, so a
/// signature computed for one authority does not authenticate the same request sent to
/// another — and that refusal, too, is this API's own error document.
///
/// The rows differ in how the authority sent to differs from the authority signed for: the
/// port, the host, and the whole published endpoint host.
#[tokio::test]
async fn a_signature_computed_for_another_authority_does_not_authenticate_the_request() {
    let rows: &[(&str, &str, &str)] = &[
        (
            "signed for one port, sent to another",
            LOOPBACK,
            "127.0.0.1:4566",
        ),
        (
            "signed for the loopback address, sent to its name",
            LOOPBACK,
            "localhost:9297",
        ),
        (
            "signed for the loopback address, sent to the published endpoint host",
            LOOPBACK,
            "sdb.us-west-2.amazonaws.com",
        ),
    ];

    let router = authenticating_emulator();
    for (case, signed_for, sent_to) in rows {
        // The same signing, addressed where it was signed for: served.
        let signed = signed_query(
            &router,
            signed_for,
            HMAC_SHA256,
            Credential::Published,
            &[
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "cross-signed-domain"),
            ],
        )
        .await;
        assert_served(&signed, case, "CreateDomain");

        // And addressed elsewhere: refused, with nothing written.
        let misdirected = misdirected_query(
            &router,
            signed_for,
            sent_to,
            &[
                ("Action", "CreateDomain"),
                ("Version", API_VERSION),
                ("DomainName", "misdirected-domain"),
            ],
        )
        .await;
        assert_refused_in_query_envelope(
            &misdirected,
            case,
            403,
            "SignatureDoesNotMatch",
            SIGNATURE_DOES_NOT_MATCH,
        );
    }

    assert_eq!(
        listed_domains(&router, LOOPBACK).await,
        vec!["cross-signed-domain".to_string()],
        "case after every row: only a request whose signature covered the authority it was \
         sent to may have written anything"
    );
}
