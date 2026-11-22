// SPDX-License-Identifier: CC0-1.0

//! The whole chain a client walks to put a mutual-TLS REGIONAL custom domain name in front of
//! a stage with a certificate Certificate Manager ISSUED for it: request a DNS-validated
//! public certificate, publish the challenge CNAME the certificate handed out in the
//! account's own PUBLIC hosted zone, wait for the certificate to reach `ISSUED`, and build
//! the custom domain on the ARN it answered.
//!
//! RULE under test: **a public certificate whose DNS challenge is published in a public
//! hosted zone of the account leaves `PENDING_VALIDATION` for `ISSUED` — with every
//! `DomainValidation.ValidationStatus` `SUCCESS` — on the clock alone, and the ARN it answered
//! is then a certificate reference like any other: a REGIONAL custom domain name created with
//! it as its `regionalCertificateArn` reaches `AVAILABLE`, publishes an address a client can
//! dial, and enforces its `mutualTlsAuthentication` truststore there — a client certificate
//! the truststore trusts is served the stage its base path mapping names, one signed by
//! another CA is refused `403` `Forbidden`, and one presenting no certificate at all is
//! refused at the TLS layer with no HTTP answer.**
//!
//! Three crates have to agree for that to hold — Route 53 publishes the challenge,
//! Certificate Manager reads it and issues, API Gateway fronts the stage under the issued
//! ARN — so every one of them is mounted on ONE engine behind ONE router
//! ([`gateway_with_acm_s3_and_dns`]) and addressed over its own wire: Route 53's restXml
//! `/2013-04-01/…`, Certificate Manager's awsJson1_1 `POST /`, this service's restJson1.
//!
//! Nothing is staged in a store: the hosted zone is created through Route 53's own
//! `CreateHostedZone`, the challenge through its own `ChangeResourceRecordSets`, the
//! certificate through Certificate Manager's own `RequestCertificate` (so the ARN under test
//! is the one that service minted), the truststore through Amazon S3's own `PutObject`, and
//! every client certificate is minted by `rcgen` — something other than the code under test.
//! The issuance is reached by POLLING the real clock, exactly as a client's waiter does:
//! nothing here pushes a stored instant, so a transition that needed a call to start it would
//! leave the poll spinning.
//!
//! The rows span what the rule generalises over: how the certificate's name sits against the
//! zone it is validated in (a label under the apex, the apex itself, a wildcard covering the
//! custom domain name), and the base path the mapping answers under (`(none)` and a named
//! base path).

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::base_path_mapping::create_mapping;
use crate::domain_name_collection::created_domain;
use crate::regional_custom_domain_front_door::{
    assert_forbidden, authority, client_identity, deployed_api, is_reachable_authority,
    stage_truststore, tls_exchange, STAGE_BODY, TRUSTSTORE_BUCKET, TRUSTSTORE_KEY,
};
use crate::rest_api_lifecycle::{gateway_with_acm_s3_and_dns, string_member, Gateway};

/// The Region every call of this suite is signed for, and so the Region the truststore object
/// is read from. The same Region the sibling front-door suite uses, because the harness this
/// suite shares with it addresses that one.
const REGION: &str = "us-east-1";

/// How long a wait for the issuance the published challenge owes may take before this suite
/// calls it a failure, and how long it waits between two reads.
///
/// The budget is far above the span Certificate Manager owes that transition, so an expired
/// one is a real defect rather than a loaded host; the interval is far below it, so the
/// transition is OBSERVED rather than slept through.
const ISSUANCE_BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The `domainNameStatus` a custom domain name is usable in.
const AVAILABLE: &str = "AVAILABLE";

/// `CertificateStatus` values this suite reads.
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";
const ISSUED: &str = "ISSUED";

/// `DomainValidation$ValidationStatus` of a name whose control has been proven.
const VALIDATION_SUCCESS: &str = "SUCCESS";

// ---------------------------------------------------------------------------
// The other two services' wires
// ---------------------------------------------------------------------------

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is the
/// signal the router resolves the service from. The signature itself is never checked: this
/// harness runs with request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0"
    )
}

/// Call one Certificate Manager operation over its awsJson1_1 wire and answer the parsed
/// body, asserting the 200 every operation here must answer.
async fn acm(gateway: &Gateway, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("acm.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("CertificateManager.{operation}"))
        .header("authorization", authorization("acm"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(&response.into_body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "case {case}: {operation} must answer 200, got {status} with {answered}"
    );
    answered
}

/// Call one Amazon Route 53 operation over its restXml wire and answer `(status, body)`.
async fn route53(gateway: &Gateway, method: &str, uri: &str, body: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "route53.amazonaws.com")
        .header("content-type", "application/xml")
        .header("authorization", authorization("route53"))
        .body(bytes::Bytes::from(body.to_owned()))
        .expect("the request is well formed");
    let response = gateway.send(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(&response.into_body()).to_string();
    (status, payload)
}

/// The text inside the first `<tag>…</tag>` element of `body`.
fn element(body: &str, tag: &str) -> String {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body
        .find(&open)
        .unwrap_or_else(|| panic!("the reply must carry a {tag} element, got {body}"))
        + open.len();
    let rest = &body[start..];
    let end = rest
        .find(&close)
        .unwrap_or_else(|| panic!("the {tag} element must be closed, got {body}"));
    rest[..end].to_string()
}

/// Create a PUBLIC hosted zone for `apex` through Route 53's own API and answer its bare
/// (`Z…`) id — the account really being authoritative for the name is what lets the
/// certificate below be validated there at all.
async fn create_public_zone(gateway: &Gateway, case: &str, apex: &str) -> String {
    let body = format!(
        "<CreateHostedZoneRequest xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\">\
         <Name>{apex}</Name><CallerReference>{case}/{apex}</CallerReference>\
         </CreateHostedZoneRequest>"
    );
    let (status, reply) = route53(gateway, "POST", "/2013-04-01/hostedzone", &body).await;
    assert_eq!(
        status, 201,
        "case {case}: CreateHostedZone for {apex} must answer 201, got {status} with {reply}"
    );
    element(&reply, "Id")
        .trim_start_matches("/hostedzone/")
        .to_string()
}

/// Publish `record` in `zone` through Route 53's own `ChangeResourceRecordSets`.
async fn publish_record(gateway: &Gateway, case: &str, zone: &str, record: &Value) {
    let (name, kind, value) = (
        record["Name"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the challenge carries no Name: {record}")),
        record["Type"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the challenge carries no Type: {record}")),
        record["Value"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case}: the challenge carries no Value: {record}")),
    );
    let body = format!(
        "<ChangeResourceRecordSetsRequest \
         xmlns=\"https://route53.amazonaws.com/doc/2013-04-01/\"><ChangeBatch><Changes><Change>\
         <Action>CREATE</Action><ResourceRecordSet><Name>{name}</Name><Type>{kind}</Type>\
         <TTL>60</TTL><ResourceRecords><ResourceRecord><Value>{value}</Value></ResourceRecord>\
         </ResourceRecords></ResourceRecordSet></Change></Changes></ChangeBatch>\
         </ChangeResourceRecordSetsRequest>"
    );
    let (status, reply) = route53(
        gateway,
        "POST",
        &format!("/2013-04-01/hostedzone/{zone}/rrset"),
        &body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: publishing {name} must be accepted, got {status} with {reply}"
    );
}

// ---------------------------------------------------------------------------
// The certificate half of the chain
// ---------------------------------------------------------------------------

/// Request a DNS-validated public certificate covering `domain_name`, publish every challenge
/// it hands out in `zone`, and answer the ARN once the certificate reports `ISSUED`.
///
/// Every assertion on the way is part of the rule: the request waits in `PENDING_VALIDATION`
/// with each covered name unproven and a CNAME to publish, and the issuance the published
/// challenge owes is reached by polling the real clock.
async fn issued_certificate(
    gateway: &Gateway,
    case: &str,
    zone: &str,
    domain_name: &str,
) -> String {
    let requested = acm(
        gateway,
        case,
        "RequestCertificate",
        &json!({ "DomainName": domain_name, "ValidationMethod": "DNS" }),
    )
    .await;
    let arn = string_member(&requested, "CertificateArn", case);

    let pending = describe_certificate(gateway, case, &arn).await;
    assert_eq!(
        pending["Status"].as_str(),
        Some(PENDING_VALIDATION),
        "case {case}: a requested certificate waits for proof of domain control, got {pending}"
    );
    let validations = pending["DomainValidationOptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: no DomainValidationOptions in {pending}"))
        .clone();
    assert!(
        !validations.is_empty(),
        "case {case}: expected one validation per covered name, got {pending}"
    );
    for validation in &validations {
        assert_eq!(
            validation["ValidationStatus"].as_str(),
            Some(PENDING_VALIDATION),
            "case {case}: an unproven name is PENDING_VALIDATION, got {validation}"
        );
        let record = &validation["ResourceRecord"];
        assert_eq!(
            record["Type"].as_str(),
            Some("CNAME"),
            "case {case}: DNS validation publishes a CNAME, got {validation}"
        );
        publish_record(gateway, case, zone, record).await;
    }

    // The issuance the proof owes falls due on its own: this is an ordinary poll, and a
    // transition that needed something other than the clock would never arrive.
    let started = Instant::now();
    loop {
        let detail = describe_certificate(gateway, case, &arn).await;
        if detail["Status"].as_str() == Some(ISSUED) {
            for validation in detail["DomainValidationOptions"]
                .as_array()
                .unwrap_or(&Vec::new())
            {
                assert_eq!(
                    validation["ValidationStatus"].as_str(),
                    Some(VALIDATION_SUCCESS),
                    "case {case}: a proven name reports SUCCESS, got {validation}"
                );
            }
            assert!(
                detail["FailureReason"].is_null(),
                "case {case}: nothing failed for an issued certificate, got {detail}"
            );
            return arn;
        }
        assert_eq!(
            detail["Status"].as_str(),
            Some(PENDING_VALIDATION),
            "case {case}: a published challenge takes the request to {ISSUED}, never to \
             another terminal status, got {detail}"
        );
        assert!(
            started.elapsed() < ISSUANCE_BUDGET,
            "case {case}: polling for {ISSUED} gave up after {:?}: still {detail}",
            started.elapsed()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `DescribeCertificate`'s `Certificate` member for `arn`.
async fn describe_certificate(gateway: &Gateway, case: &str, arn: &str) -> Value {
    let answered = acm(
        gateway,
        case,
        "DescribeCertificate",
        &json!({ "CertificateArn": arn }),
    )
    .await;
    answered["Certificate"].clone()
}

// ---------------------------------------------------------------------------
// The cases
// ---------------------------------------------------------------------------

/// One row: the zone the certificate is validated in, the name it covers, the custom domain
/// name built on it and the base path its mapping answers under.
struct Row {
    case: &'static str,
    apex: &'static str,
    /// The `DomainName` the certificate is requested for.
    covered: &'static str,
    /// The custom domain name created with that certificate.
    domain_name: &'static str,
    /// The `basePath` of the mapping, and the path a client then asks for.
    base_path: &'static str,
    path: &'static str,
}

/// RULE: the whole chain — a DNS-validated certificate reaches `ISSUED` once its challenge is
/// published, and the custom domain name built on the ARN it answered enforces its
/// mutual-TLS truststore on the address it publishes.
#[tokio::test]
async fn an_issued_certificate_fronts_a_mutual_tls_custom_domain() {
    let rows = [
        Row {
            case: "a label under the apex, mapped under (none)",
            apex: "issued-chain.example.com",
            covered: "shop.issued-chain.example.com",
            domain_name: "shop.issued-chain.example.com",
            base_path: "(none)",
            path: "/pets",
        },
        Row {
            case: "the apex itself, mapped under a base path",
            apex: "apex-chain.example.net",
            covered: "apex-chain.example.net",
            domain_name: "apex-chain.example.net",
            base_path: "shop",
            path: "/shop/pets",
        },
        Row {
            case: "a wildcard covering the custom domain name",
            apex: "wild-chain.example.org",
            covered: "*.wild-chain.example.org",
            domain_name: "api.wild-chain.example.org",
            base_path: "(none)",
            path: "/pets",
        },
    ];

    for row in rows {
        // A fresh emulator per row, so one row's zone, certificate or truststore cannot
        // explain another's.
        let gateway = gateway_with_acm_s3_and_dns();
        let case = row.case;

        let zone = create_public_zone(&gateway, case, row.apex).await;
        let certificate_arn = issued_certificate(&gateway, case, &zone, row.covered).await;

        let trusted_ca = authority("trusted-ca.issued-chain.example.com");
        let other_ca = authority("other-ca.issued-chain.example.net");
        let trusted_client = client_identity(&trusted_ca, "client.issued-chain.example.com");
        let untrusted_client = client_identity(&other_ca, "client.issued-chain.example.com");
        let truststore_version =
            stage_truststore(&gateway, &trusted_ca.certificate.pem()).await;
        let (api, stage) = deployed_api(&gateway, "issued-chain").await;

        let mut authentication = json!({
            "truststoreUri": format!("s3://{TRUSTSTORE_BUCKET}/{TRUSTSTORE_KEY}"),
        });
        if let Some(version) = truststore_version.as_deref() {
            authentication["truststoreVersion"] = json!(version);
        }
        let created = created_domain(
            &gateway,
            row.domain_name,
            json!({
                "endpointConfiguration": { "types": ["REGIONAL"] },
                "regionalCertificateArn": certificate_arn,
                "securityPolicy": "TLS_1_2",
                "mutualTlsAuthentication": authentication,
            }),
        )
        .await;
        assert_eq!(
            created["domainNameStatus"].as_str(),
            Some(AVAILABLE),
            "case {case}: expected the record to be {AVAILABLE}, got {created}"
        );
        assert_eq!(
            created["regionalCertificateArn"].as_str(),
            Some(certificate_arn.as_str()),
            "case {case}: expected the record to report the issued certificate's own ARN, \
             got {created}"
        );
        let published = string_member(&created, "regionalDomainName", case);
        assert!(
            is_reachable_authority(&published),
            "case {case}: expected an address a client can dial, got {published:?}"
        );

        let answer = create_mapping(
            &gateway,
            row.domain_name,
            json!({ "basePath": row.base_path, "restApiId": api, "stage": stage }),
        )
        .await;
        assert_eq!(
            answer.status, 201,
            "case {case}: expected 201 from CreateBasePathMapping, got {} with {:?}",
            answer.status, answer.body
        );

        // The three mutual-TLS outcomes on the address the record published.
        let trusted = tls_exchange(
            case,
            &published,
            row.domain_name,
            Some(&trusted_client),
            row.path,
        )
        .await;
        assert!(
            trusted.leaf.is_some(),
            "case {case}: a certificate has to be presented for a client certificate to be \
             asked for at all"
        );
        let answered = trusted.answered(case);
        assert!(
            answered.starts_with("http/1.1 200") && answered.contains(&STAGE_BODY.to_lowercase()),
            "case {case}: expected the stage's own answer for a trusted client certificate, \
             got {answered:?}"
        );

        let untrusted = tls_exchange(
            case,
            &published,
            row.domain_name,
            Some(&untrusted_client),
            row.path,
        )
        .await;
        assert_forbidden(&untrusted.answered(case), case);

        let anonymous = tls_exchange(case, &published, row.domain_name, None, row.path).await;
        assert!(
            anonymous.answer.is_none(),
            "case {case}: a client presenting no certificate is refused at the TLS layer, \
             got {:?}",
            anonymous.answer
        );

        // And the trusted client is served again afterwards: a refusal of another client
        // changes nothing about this one.
        let again = tls_exchange(
            case,
            &published,
            row.domain_name,
            Some(&trusted_client),
            row.path,
        )
        .await;
        let answered = again.answered(case);
        assert!(
            answered.starts_with("http/1.1 200") && answered.contains(&STAGE_BODY.to_lowercase()),
            "case {case}: expected the trusted client to be served again, got {answered:?}"
        );
    }
}
