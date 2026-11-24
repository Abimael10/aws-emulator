// SPDX-License-Identifier: CC0-1.0

//! Verifying a SENDING DOMAIN the way a client does: publishing the Easy DKIM records SES
//! handed out, in the account's own DNS.
//!
//! The rule under test: **a domain email identity is verified for sending exactly when
//! Amazon SES FINDS the Easy DKIM CNAME records its owner was told to publish — one record
//! per token of `DkimAttributes.Tokens`, at `<token>._domainkey.<domain>`, carrying
//! `<token>.dkim.amazonses.com` — in the DNS the account publishes for that domain. Until
//! then `GetEmailIdentity` reports `VerificationStatus` `PENDING` and
//! `VerifiedForSendingStatus` false, however many reads are made, because nothing but those
//! records can complete the verification; an email-address identity carries no such records
//! and reaches its verification on its own.**
//!
//! That is what every client of a verified sending domain does: `CreateEmailIdentity` for
//! the domain, publish the three CNAMEs it answers in that domain's hosted zone, then poll
//! `GetEmailIdentity` until it reports itself verified. Two crates have to agree for the
//! rule to hold — Route 53 holds the zone and its record sets, SESv2 answers for the
//! identity — so the suite lives here: a crate-local test only ever sees one side of it.
//!
//! The rows differ along what the rule generalises over: which zone of the account publishes
//! the name (the domain's own apex zone, and a parent zone covering a subdomain identity),
//! how many of the tokens are published (none, some, all), whether the record published
//! carries the value SES names, and whether the zone that carries it is one the public DNS
//! can see at all. The domain names are this suite's own; the rule names none.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The endpoint host a Route 53 request addresses — Route 53 is global — which is also the
/// routing signal the router resolves that service from.
const ROUTE53_HOST: &str = "route53.amazonaws.com";

/// How long a poll waits for the condition it is looking for, and how often it looks.
///
/// The identity's move to `SUCCESS` is owed on the emulator's own accelerated schedule once
/// the records are there, so what is waited for takes well under a second; the deadline is
/// generous so a loaded host makes the wait more patient and never less, and is only here so
/// a failure is a failure rather than a hang.
const DEADLINE: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_millis(50);

/// How many times a case that must NOT reach verification reads the identity back.
///
/// Nothing is waited for: an identity whose records are not published is not waiting on the
/// clock at all, so every read of it answers the same thing and reading it several times is
/// what shows that.
const READS_WITHOUT_PUBLICATION: usize = 3;

/// The two refusals the ROUTER itself answers, which no row here may provoke.
const ROUTER_REFUSALS: &[&str] = &["AmbiguousService", "UnknownService"];

/// The two services this suite exercises, over ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
fn build_router() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These rows exercise handler behaviour, not SigV4.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sesv2_simpleemailservice_v2::EmulatedService::new(Arc::clone(&engine)),
    );
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the signal
/// the router resolves a routing key from. The signature itself is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One answered request: its status and its body as text.
struct Answer {
    status: u16,
    text: String,
}

/// Send one request, refusing at the door the two answers the ROUTER itself produces: a
/// request that reached nobody, or one several services claimed.
async fn send(router: &MultiServiceRouter, case: &str, leg: &str, request: http::Request<Bytes>) -> Answer {
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let text = String::from_utf8_lossy(response.body()).to_string();
    for refusal in ROUTER_REFUSALS {
        assert!(
            !text.contains(refusal) && !error_type.contains(refusal),
            "case {case:?}: {leg} must be served by exactly one service, got HTTP {status}: {text}"
        );
    }
    Answer { status, text }
}

/// Call one Route 53 operation over its restXml wire, requiring HTTP `expected`.
async fn route53(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    uri: &str,
    body: &str,
    expected: u16,
) -> String {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{ROUTE53_HOST}{uri}"))
        .header(http::header::HOST, ROUTE53_HOST)
        .header(http::header::AUTHORIZATION, authorization("route53"))
        .header(http::header::CONTENT_TYPE, "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let answered = send(router, case, &format!("Route 53 {method} {uri}"), request).await;
    assert_eq!(
        answered.status, expected,
        "case {case:?}: Route 53 {method} {uri} expected HTTP {expected}, got {} with body {}",
        answered.status, answered.text
    );
    answered.text
}

/// Call one SESv2 operation over its restJson1 wire, requiring HTTP `expected`.
async fn sesv2(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = match &body {
        Some(value) => Bytes::from(value.to_string()),
        None => Bytes::new(),
    };
    let host = format!("email.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header(http::header::HOST, host)
        .header(http::header::AUTHORIZATION, authorization("ses"))
        .header(http::header::CONTENT_TYPE, "application/json")
        .body(payload)
        .expect("a well-formed request");
    let answered = send(router, case, &format!("SESv2 {method} {path}"), request).await;
    assert_eq!(
        answered.status, expected,
        "case {case:?}: SESv2 {method} {path} expected HTTP {expected}, got {} with body {}",
        answered.status, answered.text
    );
    serde_json::from_str(&answered.text).unwrap_or(Value::Null)
}

/// The text of the first `<name>…</name>` in `xml`, or `None` when it carries none.
fn element(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// Create a hosted zone for `zone_name` and answer its id, the way a client creates the zone
/// it publishes a domain's records in. `private` creates it as a private zone, which the
/// public DNS cannot see.
async fn given_hosted_zone(
    router: &MultiServiceRouter,
    case: &str,
    zone_name: &str,
    private: bool,
) -> String {
    let vpc = if private {
        format!(
            "<VPC><VPCId>vpc-0123456789abcdef0</VPCId><VPCRegion>{REGION}</VPCRegion></VPC>"
        )
    } else {
        String::new()
    };
    let created = route53(
        router,
        case,
        "POST",
        "/2013-04-01/hostedzone",
        &format!(
            "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
             <Name>{zone_name}</Name><CallerReference>{case}-{zone_name}</CallerReference>\
             <HostedZoneConfig><Comment>the zone the records go in</Comment>\
             <PrivateZone>{private}</PrivateZone></HostedZoneConfig>{vpc}\
             </CreateHostedZoneRequest>"
        ),
        201,
    )
    .await;
    let id = element(&created, "Id")
        .unwrap_or_else(|| panic!("case {case:?}: CreateHostedZone must answer the zone id: {created}"));
    id.rsplit('/').next().unwrap_or_default().to_string()
}

/// UPSERT one CNAME record set at `name` carrying `value` into the zone `zone_id`, which is
/// how the account publishes the record SES is waiting for.
async fn given_published_cname(
    router: &MultiServiceRouter,
    case: &str,
    zone_id: &str,
    name: &str,
    value: &str,
) {
    route53(
        router,
        case,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone_id}/rrset"),
        &format!(
            "<ChangeResourceRecordSetsRequest \
             xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><ChangeBatch>\
             <Comment>an Easy DKIM record</Comment><Changes><Change><Action>UPSERT</Action>\
             <ResourceRecordSet><Name>{name}</Name><Type>CNAME</Type><TTL>1800</TTL>\
             <ResourceRecords><ResourceRecord><Value>{value}</Value></ResourceRecord>\
             </ResourceRecords></ResourceRecordSet></Change></Changes></ChangeBatch>\
             </ChangeResourceRecordSetsRequest>"
        ),
        200,
    )
    .await;
}

/// The Easy DKIM record SES tells the owner of `domain` to publish for `token`: the host it
/// sits at, and the value it carries.
///
/// Source: the `DkimAttributes$Tokens` documentation ("a set of unique strings that you use
/// to create a set of CNAME records that you add to the DNS configuration for your domain")
/// and the Amazon SES Easy DKIM setup instructions, which give the name
/// `<token>._domainkey.<domain>` and the value `<token>.dkim.amazonses.com`.
fn easy_dkim_record(token: &str, domain: &str) -> (String, String) {
    (
        format!("{token}._domainkey.{domain}"),
        format!("{token}.dkim.amazonses.com"),
    )
}

/// Create the domain identity `domain` and answer the Easy DKIM tokens its owner has to
/// publish.
async fn given_domain_identity(router: &MultiServiceRouter, case: &str, domain: &str) -> Vec<String> {
    let created = sesv2(
        router,
        case,
        "POST",
        "/v2/email/identities",
        Some(json!({ "EmailIdentity": domain })),
        200,
    )
    .await;
    assert_eq!(
        created["IdentityType"],
        json!("DOMAIN"),
        "case {case:?}: {domain} is a DOMAIN identity, got {created}"
    );
    assert_eq!(
        created["VerifiedForSendingStatus"],
        json!(false),
        "case {case:?}: a create answers an identity nothing has published records for as \
         unverified, got {created}"
    );
    let tokens: Vec<String> = created["DkimAttributes"]["Tokens"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: Easy DKIM must answer its tokens: {created}"))
        .iter()
        .map(|token| token.as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        tokens.len() >= 2,
        "case {case:?}: Easy DKIM publishes a set of tokens, got {tokens:?}"
    );
    tokens
}

/// Read the identity `domain` back.
async fn identity(router: &MultiServiceRouter, case: &str, domain: &str) -> Value {
    sesv2(
        router,
        case,
        "GET",
        &format!("/v2/email/identities/{domain}"),
        None,
        200,
    )
    .await
}

/// What a row publishes of the records SES asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Publication {
    /// Nothing at all: the owner never touched the zone.
    None,
    /// Every token, at the name and with the value SES named.
    Every,
    /// All but the last token, so the set is incomplete.
    AllButOne,
    /// Every token, but each pointing at another host than the one SES named.
    EveryAtTheWrongValue,
}

/// One row: the identity, the zone the records are published in, and what is published.
struct Row {
    case: &'static str,
    /// The email identity created.
    identity: &'static str,
    /// The zone the records are published in, which covers the identity's name.
    zone: &'static str,
    /// Whether that zone is private, which the public DNS cannot see.
    private_zone: bool,
    published: Publication,
    /// Whether the identity must reach `VerifiedForSendingStatus` true.
    verified: bool,
}

/// RULE: a domain identity is verified exactly when the Easy DKIM records SES handed out are
/// published, with the names and values SES named, in a zone of the account the public DNS
/// can see.
#[tokio::test]
async fn a_domain_identity_is_verified_only_once_its_easy_dkim_records_are_published() {
    let rows = [
        Row {
            case: "nothing published",
            identity: "unpublished.example.com",
            zone: "unpublished.example.com",
            private_zone: false,
            published: Publication::None,
            verified: false,
        },
        Row {
            case: "every record published in the domain's own zone",
            identity: "published.example.com",
            zone: "published.example.com",
            private_zone: false,
            published: Publication::Every,
            verified: true,
        },
        Row {
            case: "every record published in a parent zone covering a subdomain identity",
            identity: "mail.parent.example.net",
            zone: "parent.example.net",
            private_zone: false,
            published: Publication::Every,
            verified: true,
        },
        Row {
            case: "one record of the set missing",
            identity: "partial.example.com",
            zone: "partial.example.com",
            private_zone: false,
            published: Publication::AllButOne,
            verified: false,
        },
        Row {
            case: "every record published pointing somewhere else",
            identity: "misdirected.example.com",
            zone: "misdirected.example.com",
            private_zone: false,
            published: Publication::EveryAtTheWrongValue,
            verified: false,
        },
        Row {
            case: "every record published in a private zone",
            identity: "internal.example.com",
            zone: "internal.example.com",
            private_zone: true,
            published: Publication::Every,
            // A private zone "is visible only in the VPCs that you specify", so nothing SES
            // looks up in the public DNS is answered from it.
            verified: false,
        },
    ];

    for row in rows {
        let (router, _engine) = build_router();
        let case = row.case;
        let tokens = given_domain_identity(&router, case, row.identity).await;
        let zone_id = given_hosted_zone(&router, case, row.zone, row.private_zone).await;

        let published: Vec<&String> = match row.published {
            Publication::None => Vec::new(),
            Publication::Every | Publication::EveryAtTheWrongValue => tokens.iter().collect(),
            Publication::AllButOne => tokens.iter().take(tokens.len() - 1).collect(),
        };
        for token in published {
            let (name, value) = easy_dkim_record(token, row.identity);
            let value = match row.published {
                // A CNAME at the right name pointing at another host proves nothing: the
                // record has to carry the value SES named.
                Publication::EveryAtTheWrongValue => format!("{token}.dkim.example.org"),
                _ => value,
            };
            given_published_cname(&router, case, &zone_id, &name, &value).await;
        }

        if row.verified {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let read = identity(&router, case, row.identity).await;
                if read["VerifiedForSendingStatus"] == json!(true) {
                    assert_eq!(
                        read["VerificationStatus"],
                        json!("SUCCESS"),
                        "case {case:?}: a verified identity reports its verification SUCCESS, \
                         got {read}"
                    );
                    assert_eq!(
                        read["DkimAttributes"]["Status"],
                        json!("SUCCESS"),
                        "case {case:?}: the records SES looked for were found, so DKIM is set \
                         up, got {read}"
                    );
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: the identity must reach VerifiedForSendingStatus true once \
                     its records are published, last read {read}"
                );
                tokio::time::sleep(POLL).await;
            }
        } else {
            // Nothing can complete this verification, so nothing is waited for: every read
            // answers the same thing.
            for read_number in 1..=READS_WITHOUT_PUBLICATION {
                let read = identity(&router, case, row.identity).await;
                assert_eq!(
                    read["VerifiedForSendingStatus"],
                    json!(false),
                    "case {case:?}: read {read_number} must report VerifiedForSendingStatus \
                     false, got {read}"
                );
                assert_eq!(
                    read["VerificationStatus"],
                    json!("PENDING"),
                    "case {case:?}: read {read_number} must report the verification still \
                     PENDING, got {read}"
                );
            }
        }
    }
}

/// RULE, the other half: the records are a DOMAIN's. An email-address identity publishes no
/// DNS record — it is verified by the owner of the mailbox SES sends its verification message
/// to — so it reaches its verification with nothing published at all.
#[tokio::test]
async fn an_email_address_identity_is_verified_with_no_records_published() {
    let case = "an address identity";
    let (router, _engine) = build_router();
    let created = sesv2(
        &router,
        case,
        "POST",
        "/v2/email/identities",
        Some(json!({ "EmailIdentity": "ada@mailbox.example.com" })),
        200,
    )
    .await;
    assert_eq!(
        created["IdentityType"],
        json!("EMAIL_ADDRESS"),
        "case {case:?}: an identity with a local part is an address, got {created}"
    );

    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = identity(&router, case, "ada@mailbox.example.com").await;
        if read["VerifiedForSendingStatus"] == json!(true) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: an address identity reaches its verification without a published \
             record, last read {read}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The stand-in for the DNS publication above, for a suite of this target that serves no DNS
/// of its own and only needs a sending domain to exist.
///
/// A domain identity is verified by the Easy DKIM records its owner publishes
/// ([`a_domain_identity_is_verified_only_once_its_easy_dkim_records_are_published`] is where
/// that rule is asserted), so a suite whose router mounts no Route 53 has no way to publish
/// them: it stands in for the publication by moving the verification instant every identity
/// of the account carries into the past, on the record every operation reads. That is the
/// same seam this target's other sending suites use, stated once here.
///
/// The moved instant is then PUBLISHED to the account's identity registry through the
/// service's own publication, so the other SES surfaces over those same identities read the
/// instant this one holds — which is what the service itself does on its next call. A surface
/// that has already ADOPTED the identity decides its verification from the instant it adopted
/// (the registry never copies a verification onto a record a surface already holds), so the
/// stand-in moves the instant on the classic surface's own copy too.
pub(crate) fn given_dkim_publication_stood_in_for(engine: &engine::InMemoryEngine) {
    let state = engine
        .state::<service_com_amazonaws_sesv2_simpleemailservice_v2::state::ComAmazonawsSesv2SimpleemailserviceV2State>();
    let past = SystemTime::now()
        .checked_sub(Duration::from_secs(1))
        .expect("an instant before now");
    let names: Vec<String> = {
        let mut store = state.lock();
        for record in store.identities.values_mut() {
            record.verification_due_at = past;
        }
        store
            .identities
            .values()
            .map(|record| record.name.clone())
            .collect()
    };
    for name in names {
        service_com_amazonaws_sesv2_simpleemailservice_v2::state::publish_identity(engine, &name);
    }
    let classic = engine
        .state::<service_com_amazonaws_ses_simpleemailservice::state::ComAmazonawsSesSimpleemailserviceState>();
    for identity in classic.lock().identities.values_mut() {
        identity.verification_due = Some(past);
    }
}
