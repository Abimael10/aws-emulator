// SPDX-License-Identifier: CC0-1.0

//! A requested public certificate for a name NOBODY could prove control of, settling on
//! the REAL clock: the waiter pattern a client runs, driven over both services' wires —
//! Certificate Manager's `POST /` awsJson1_1 and Route 53's restXml `/2013-04-01/...` —
//! on ONE engine, exactly as `core/router/src/lib.rs` mounts them.
//!
//! RULES under test:
//!
//! 1. Such a request does not wait: "a certificate enters status `PENDING_VALIDATION`
//!    upon being requested, UNLESS it fails for any of the reasons given in the
//!    troubleshooting topic Certificate request fails" (`smithy_query(query="doc",
//!    shape="com.amazonaws.acm#CertificateDetail", member="Status")`). The CLOCK owes it a
//!    terminal `CertificateStatus` `FAILED` carrying the `FailureReason` that "exists only
//!    when the certificate status is FAILED" and names verification ACM has to obtain
//!    outside the account, `ADDITIONAL_VERIFICATION_REQUIRED` (`smithy_query(query="shape",
//!    shape="com.amazonaws.acm#FailureReason")`) — with nothing issued and the status
//!    stable across reads.
//! 2. `ValidationStatus` is "the validation status of the domain name"
//!    (`DomainValidation$ValidationStatus` doc) — a verdict about ONE name — and "each
//!    domain name that you specify must be validated" (`RequestCertificate` doc), so the
//!    name ACM stopped at carries `FAILED` while the names behind it, never judged, stay
//!    `PENDING_VALIDATION`.
//! 3. A name is reported as the request wrote it: `DomainNameString`'s own `@pattern`
//!    admits a leading `*.` label, so `DomainName` and — under a method that mails nothing
//!    — `ValidationDomain` echo the requested name verbatim, wildcard label and letter
//!    case included. Under `EMAIL`, `ValidationDomain` is "the domain name that ACM used
//!    to send domain validation emails" (`DomainValidation$ValidationDomain` doc), i.e.
//!    that name's mail suffix, which carries no wildcard label.
//! 4. `ValidationEmails` is "a list of email addresses that ACM used to send domain
//!    validation emails" (`DomainValidation$ValidationEmails` doc), and ACM mails nothing
//!    for a request it settles without ever waiting on proof of domain control, so no
//!    entry of such a certificate reports a validation address.
//! 5. Whether a name has a holder at all is the DNS service's fact: a name the account is
//!    itself authoritative for in the public DNS — a PUBLIC hosted zone it created through
//!    Route 53 — is one whose holder could publish the evidence, so that request is NOT
//!    refused up front. It is still waiting once the undelegated requests made beside it
//!    have settled.
//!
//! What this suite adds over the owning crate's own
//! (`unvalidatable_name_certificate_failure.rs`, `delegated_zone_certificate_validation.rs`,
//! which move the service's stored due instant into the past) is the WALL CLOCK: nothing
//! here touches the store's bookkeeping, so the outcome has to fall due on its own and be
//! reported by an ordinary poll — the waiter that spun to its own timeout against a
//! certificate parked in `PENDING_VALIDATION`. It lives here because the delegation half of
//! rule 5 is Route 53's fact, read by Certificate Manager out of the one shared engine.
//!
//! The rows span what the rules generalise over: EVERY entry of the three catalogs the
//! service publishes for names no account can prove control of (its reserved top-level
//! labels, its reserved second-level names and its provider-operated domains, whose
//! sources are RFC 2606/6761/6762/7686, IANA's Example Domains and the provider's own
//! zones), the form of the covered name (a plain label, a wildcard, several labels deep,
//! mixed case), the `ValidationMethod` asked for (every modelled value and the absence of
//! one) and the position of the offending name in the certificate's name set.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_acm_certificatemanager::state::{
    FAILURE_REASON_ADDITIONAL_VERIFICATION_REQUIRED, PROVIDER_OPERATED_DOMAINS,
    RESERVED_SECOND_LEVEL_NAMES, RESERVED_TOP_LEVEL_LABELS, VALIDATION_SETTLE_DELAY,
};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for; the ARNs the services mint belong to it.
const REGION: &str = "us-west-2";

/// The endpoint host a Route 53 request addresses — Route 53 is global — which is also
/// the routing signal the router resolves that service from.
const ROUTE53_HOST: &str = "route53.amazonaws.com";

/// `CertificateStatus` values this suite reads.
const FAILED: &str = "FAILED";
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";

/// How often a poll looks again. Far below the span the outcome falls due within, so the
/// transition is observed rather than slept through.
const POLL: Duration = Duration::from_millis(50);

/// How long a poll of the REAL clock may run before the test calls it a failure: far
/// above the span this emulator owes a requested certificate's outcome, so an expired
/// budget is a real defect rather than a loaded-host artefact.
fn settle_deadline() -> Duration {
    VALIDATION_SETTLE_DELAY * 20
}

/// The two services this suite exercises, over ONE engine: the certificate's outcome is
/// decided from the DNS the other service owns, so a single store is what makes the
/// hand-off real.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These rows exercise routing and handler behaviour, not SigV4.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_route53_awsdnsv20130401::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_acm_certificatemanager::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` — the
/// signal the router resolves a routing key from. The signature itself is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=deadbeef"
    )
}

/// Call one Certificate Manager operation over its awsJson1_1 wire.
async fn acm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let host = format!("acm.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header(http::header::HOST, host)
        .header(http::header::CONTENT_TYPE, "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header(http::header::AUTHORIZATION, authorization("acm"))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Call one Certificate Manager operation that must answer 200.
async fn acm_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = acm(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answered}"
    );
    answered
}

/// Request one certificate and answer the ARN the service minted for it.
async fn request_certificate(router: &MultiServiceRouter, case: &str, body: &Value) -> String {
    let answered = acm_ok(router, case, "RequestCertificate", body).await;
    answered["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CertificateArn in {answered}"))
        .to_string()
}

/// `DescribeCertificate`'s `Certificate` member for `arn`.
async fn describe(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let answered = acm_ok(
        router,
        case,
        "DescribeCertificate",
        &json!({ "CertificateArn": arn }),
    )
    .await;
    answered["Certificate"].clone()
}

/// Every certificate ARN `ListCertificates` reports in `status`, paging to the end.
async fn arns_in_status(router: &MultiServiceRouter, case: &str, status: &str) -> Vec<String> {
    let mut arns = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut body = json!({ "CertificateStatuses": [status], "MaxItems": 100 });
        if let Some(token) = next.as_ref() {
            body["NextToken"] = json!(token);
        }
        let answered = acm_ok(router, case, "ListCertificates", &body).await;
        for summary in answered["CertificateSummaryList"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: no CertificateSummaryList in {answered}"))
        {
            arns.push(
                summary["CertificateArn"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        next = answered["NextToken"].as_str().map(str::to_string);
        if next.is_none() {
            return arns;
        }
        assert!(
            arns.len() < 1_000,
            "case {case:?}: the ListCertificates pagination did not terminate, saw {arns:?}"
        );
    }
}

/// Create a PUBLIC hosted zone for `apex` through Route 53's own restXml operation, so
/// the account really is authoritative for that name in this engine. The created zone is
/// addressed by nothing here — what matters is that the DNS service holds it — so the
/// answer is only required to be the documented 201.
async fn create_public_zone(router: &MultiServiceRouter, case: &str, apex: &str) {
    let body = format!(
        "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
         <Name>{apex}</Name><CallerReference>{apex}</CallerReference>\
         </CreateHostedZoneRequest>"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{ROUTE53_HOST}/2013-04-01/hostedzone"))
        .header(http::header::HOST, ROUTE53_HOST)
        .header(http::header::CONTENT_TYPE, "application/xml")
        .header(http::header::AUTHORIZATION, authorization("route53"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 201,
        "case {case:?}: CreateHostedZone for {apex} must answer 201, got {status} with {text}"
    );
}

/// One row of the refusal table: what it exercises, the `RequestCertificate` body, the
/// names the certificate covers in the order it covers them, the name ACM stops at, and
/// whether the method in force is the one ACM mails under (which decides what
/// `ValidationDomain` reports).
struct Row {
    case: String,
    body: Value,
    names: Vec<String>,
    stops_at: String,
    mails: bool,
}

/// Every suffix the service publishes as one no evidence from the account could cover:
/// the special-use top-level labels, the reserved documentation second-level names and
/// the domains the provider operates. Read from the production catalogs rather than
/// written out here, so a catalog that grows is covered with it.
fn unvalidatable_suffixes() -> Vec<&'static str> {
    RESERVED_TOP_LEVEL_LABELS
        .iter()
        .chain(RESERVED_SECOND_LEVEL_NAMES)
        .chain(PROVIDER_OPERATED_DOMAINS)
        .copied()
        .collect()
}

/// The forms a covered name takes under one of those suffixes. Cycled over the catalogs
/// with a different period from the methods, so the pairing of form and method varies
/// rather than one form being asserted only under one method.
const FORMS: &[&str] = &["plain", "wildcard", "deep", "mixed case"];

/// Every `ValidationMethod` the model declares (`smithy_query(query="shape",
/// shape="com.amazonaws.acm#ValidationMethod")`), plus the absence of one — which ACM
/// validates by email.
const METHODS: &[&str] = &["DNS", "EMAIL", "HTTP"];

/// The name one row covers under `suffix`, in the form `FORMS[form]` names.
fn covered_name(form: usize, suffix: &str) -> String {
    match FORMS[form] {
        "wildcard" => format!("*.{suffix}"),
        "deep" => format!("a.b.{suffix}"),
        // DNS labels are case-insensitive, so the catalogs must match whatever case the
        // request spelled — and the response must echo that spelling back.
        "mixed case" => format!("Mixed.{}", suffix.to_uppercase()),
        _ => format!("www.{suffix}"),
    }
}

/// The refusal rows: one per catalog entry, cycling the name forms and the validation
/// methods, plus two rows that put the offending name somewhere other than first and one
/// that pairs a wildcard with the method ACM mails under.
fn rows() -> Vec<Row> {
    let mut rows = Vec::new();
    for (index, suffix) in unvalidatable_suffixes().into_iter().enumerate() {
        let name = covered_name(index % FORMS.len(), suffix);
        // Every fourth row names no method at all, which ACM validates by email.
        let method = if index % 4 == 3 {
            None
        } else {
            Some(METHODS[index % METHODS.len()])
        };
        let mut body = json!({ "DomainName": name });
        if let Some(method) = method {
            body["ValidationMethod"] = json!(method);
        }
        rows.push(Row {
            case: format!("{name} validated by {}", method.unwrap_or("no named method")),
            body,
            names: vec![name.clone()],
            stops_at: name,
            mails: method.unwrap_or("EMAIL") == "EMAIL",
        });
    }

    // A wildcard under the method ACM mails: the entry echoes the wildcard name, while
    // the domain ACM would mail is the one it covers.
    let wildcard = "*.mail-wild.example.org".to_string();
    rows.push(Row {
        case: format!("the email-validated wildcard {wildcard}"),
        body: json!({ "DomainName": wildcard, "ValidationMethod": "EMAIL" }),
        names: vec![wildcard.clone()],
        stops_at: wildcard,
        mails: true,
    });

    // The offending name LAST: the request still fails, and the verdict lands on it
    // rather than on the ordinary name ACM never finished judging.
    let ordinary = "front.probe-domain.com".to_string();
    let behind = "*.eu-west-1.elb.amazonaws.com".to_string();
    rows.push(Row {
        case: format!("the ordinary name {ordinary} beside the alternative name {behind}"),
        body: json!({
            "DomainName": ordinary,
            "SubjectAlternativeNames": [behind, "back.probe-domain.com"],
            "ValidationMethod": "DNS",
        }),
        names: vec![ordinary, behind.clone(), "back.probe-domain.com".to_string()],
        stops_at: behind,
        mails: false,
    });

    // The offending name FIRST, with ordinary names behind it: those are never judged.
    let first = "deep.sub.example.com".to_string();
    rows.push(Row {
        case: format!("the undelegated first name {first} with ordinary names behind it"),
        body: json!({
            "DomainName": first,
            "SubjectAlternativeNames": ["one.probe-domain.net", "two.probe-domain.net"],
            "ValidationMethod": "EMAIL",
        }),
        names: vec![
            first.clone(),
            "one.probe-domain.net".to_string(),
            "two.probe-domain.net".to_string(),
        ],
        stops_at: first,
        mails: true,
    });
    rows
}

/// Poll `DescribeCertificate` for `arn` until it reports a status other than
/// `PENDING_VALIDATION`, which is what a client's waiter does. Nothing touches the
/// service's stored instants: if the transition needed a call to start it, this loop
/// would never leave the pending status.
async fn settled(router: &MultiServiceRouter, case: &str, arn: &str, started: Instant) -> Value {
    loop {
        let detail = describe(router, case, arn).await;
        if detail["Status"].as_str() != Some(PENDING_VALIDATION) {
            return detail;
        }
        assert!(
            started.elapsed() < settle_deadline(),
            "case {case:?}: polling for a terminal status gave up after {:?}: still {detail}",
            started.elapsed()
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The `DomainValidationOptions` of a described certificate.
fn validations(case: &str, detail: &Value) -> Vec<Value> {
    detail["DomainValidationOptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no DomainValidationOptions in {detail}"))
        .clone()
}

/// Assert everything the terminal failure of one row implies (rules 1-4).
fn assert_settled_failure(row: &Row, detail: &Value) {
    let case = row.case.as_str();

    // RULE 1: the terminal status, its reason, and nothing issued.
    assert_eq!(
        detail["Status"].as_str(),
        Some(FAILED),
        "case {case:?}: a name no evidence from this account could cover fails the \
         request, got {detail}"
    );
    assert_eq!(
        detail["FailureReason"].as_str(),
        Some(FAILURE_REASON_ADDITIONAL_VERIFICATION_REQUIRED),
        "case {case:?}: a FAILED certificate reports the reason it failed for, got {detail}"
    );
    assert_eq!(
        detail["Type"].as_str(),
        Some("AMAZON_ISSUED"),
        "case {case:?}: a requested certificate stays AMAZON_ISSUED, got {detail}"
    );
    for absent in ["Serial", "IssuedAt", "NotBefore", "NotAfter"] {
        assert!(
            detail[absent].is_null(),
            "case {case:?}: {absent} cannot exist without issuance, got {detail}"
        );
    }

    // RULE 3, on the certificate itself: the primary name as the request wrote it.
    let requested_primary = row.body["DomainName"].as_str().unwrap_or_default();
    assert_eq!(
        detail["DomainName"].as_str(),
        Some(requested_primary),
        "case {case:?}: expected the requested DomainName {requested_primary} echoed \
         verbatim, got {detail}"
    );

    let entries = validations(case, detail);
    assert_eq!(
        entries.len(),
        row.names.len(),
        "case {case:?}: expected one validation per covered name, got {detail}"
    );

    // RULE 2: the verdict is about ONE name — the one ACM stopped at — and every name is
    // keyed as the request wrote it.
    let reported: Vec<(&str, &str)> = entries
        .iter()
        .map(|entry| {
            (
                entry["DomainName"].as_str().unwrap_or_default(),
                entry["ValidationStatus"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    let expected: Vec<(&str, &str)> = row
        .names
        .iter()
        .map(|name| {
            let status = if name.eq_ignore_ascii_case(&row.stops_at) {
                FAILED
            } else {
                PENDING_VALIDATION
            };
            (name.as_str(), status)
        })
        .collect();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected the failed verdict on {} alone, with every name as \
         requested, got {reported:?}",
        row.stops_at
    );

    for entry in &entries {
        let name = entry["DomainName"].as_str().unwrap_or_default();
        // RULE 3, per entry: a mail suffix carries no wildcard label; a method that mails
        // nothing reduces nothing.
        let expected_domain = if row.mails {
            name.strip_prefix("*.").unwrap_or(name)
        } else {
            name
        };
        assert_eq!(
            entry["ValidationDomain"].as_str(),
            Some(expected_domain),
            "case {case:?}: expected ValidationDomain {expected_domain} for {name}, got {entry}"
        );

        // RULE 4: ACM mailed nothing for this request, so it names no address.
        let addresses = entry["ValidationEmails"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        assert!(
            addresses.is_empty(),
            "case {case:?}: ACM sent no validation mail for {name}, so it names no \
             address, got {entry}"
        );
    }
}

/// RULES 1-5: every row settles into the terminal failure on the clock alone, while the
/// one name the account holds a public hosted zone for is still waiting for its proof.
#[tokio::test]
async fn an_unvalidatable_requested_certificate_settles_on_the_clock_alone() {
    let router = emulator();
    let rows = rows();

    // The delegated name, whose holder is this account because Route 53 holds a PUBLIC
    // zone for it: the counterpart that must NOT be refused up front, under a registry
    // suffix the service reserves so that the delegation is the only thing separating it
    // from the rows below.
    let delegated_case = "a name the account holds a public hosted zone for";
    let delegated_apex = "acct-zone.example.com";
    create_public_zone(&router, delegated_case, delegated_apex).await;
    let delegated_name = format!("www.{delegated_apex}");

    // Taken before the first request, so the budget below starts no later than the
    // instant the service dates the first certificate from.
    let started = Instant::now();
    let delegated_arn = request_certificate(
        &router,
        delegated_case,
        &json!({ "DomainName": delegated_name, "ValidationMethod": "DNS" }),
    )
    .await;
    let mut requested: Vec<(&Row, String)> = Vec::new();
    for row in &rows {
        let arn = request_certificate(&router, &row.case, &row.body).await;
        requested.push((row, arn));
    }

    for (row, arn) in &requested {
        let detail = settled(&router, &row.case, arn, started).await;
        assert_settled_failure(row, &detail);

        // RULE 1, terminal: reading again neither moves the status on nor re-opens the
        // validation.
        let again = describe(&router, &row.case, arn).await;
        assert_eq!(
            again, detail,
            "case {:?}: a settled certificate must read the same every time, got {again}",
            row.case
        );
    }

    // RULE 5: every request made beside it has settled, so a request that was going to be
    // refused up front has been — and this one is still waiting for proof instead.
    let waiting = describe(&router, delegated_case, &delegated_arn).await;
    assert_eq!(
        waiting["Status"].as_str(),
        Some(PENDING_VALIDATION),
        "case {delegated_case:?}: {delegated_name} is delegated to this account, so its \
         request waits for proof, got {waiting}"
    );
    assert!(
        waiting["FailureReason"].is_null(),
        "case {delegated_case:?}: nothing has failed for {delegated_name}, got {waiting}"
    );
    for entry in validations(delegated_case, &waiting) {
        assert_eq!(
            entry["ValidationStatus"].as_str(),
            Some(PENDING_VALIDATION),
            "case {delegated_case:?}: an unproven domain of {delegated_name} is still \
             waiting, got {entry}"
        );
    }

    // The other reader agrees, so nothing here passed on a response the store does not
    // back: every row is reported FAILED, and the delegated request alone is pending.
    let failed = arns_in_status(&router, "the whole table", FAILED).await;
    for (row, arn) in &requested {
        assert!(
            failed.contains(arn),
            "case {:?}: expected ListCertificates to report {arn} as FAILED, got {failed:?}",
            row.case
        );
    }
    assert_eq!(
        failed.len(),
        requested.len(),
        "expected exactly the table's certificates to be FAILED, got {failed:?}"
    );
    let pending = arns_in_status(&router, delegated_case, PENDING_VALIDATION).await;
    assert_eq!(
        pending,
        vec![delegated_arn.clone()],
        "case {delegated_case:?}: expected ListCertificates to report {delegated_arn} \
         alone as PENDING_VALIDATION, got {pending:?}"
    );
}
