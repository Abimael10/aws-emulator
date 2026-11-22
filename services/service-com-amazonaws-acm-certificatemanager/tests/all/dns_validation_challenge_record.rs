// SPDX-License-Identifier: CC0-1.0

//! The CNAME a `ListCertificateDomainValidations` summary hands a client, over the wire
//! (`POST /`, `X-Amz-Target: CertificateManager.<Op>`, awsJson1_1).
//!
//! RULE under test: a domain validation summary's `ValidationChallenge` carries the
//! evidence the domain's current validation method asks for, read off the record the
//! service minted for that domain — so for a DNS-validated name the `DNS` arm carries a
//! fully populated `ResourceRecord{Name, Type: CNAME, Value}`, the very record
//! `CertificateDetail$DomainValidationOptions[*].ResourceRecord` reports for the same
//! name, which is what retrieving "the CNAME records required for DNS validation"
//! through this operation means.
//!
//! The tables span what the rule generalises over: the form of each covered name (apex,
//! subdomain, deep label, wildcard), how many names one certificate covers, the page the
//! summary falls on, how the method was chosen (the top-level member, `Options`, a later
//! `UpdateCertificateOptions` migration onto DNS and away from it), and whether the
//! request needs out-of-band proof at all (a public request against a private-CA one).
//!
//! Every record read here is cross-checked against `DescribeCertificate`'s projection of
//! the same stored validation, read back twice so a reporting call cannot be what mints
//! it, and compared across two certificates for one name so the record cannot be a
//! constant.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::certificate_domain_validations::{
    assert_resource_record, described_validations, summaries, PRIVATE_CA_ARN,
};
use crate::certificate_lifecycle::{
    call, hold_pending_validations, request_certificate, router_with_engine,
};
use crate::certificate_validation_state_machine::make_validation_due_now;

/// The zone ACM publishes DNS-validation CNAME targets under, as the
/// `ResourceRecord$Value` doc shows (`_3639ac533e.acm-validations.aws.`).
const ACM_VALIDATIONS_ZONE: &str = "acm-validations.aws";

/// The status a validation that has not resolved stands at
/// (`com.amazonaws.acm#DomainStatus`).
const PENDING_VALIDATION: &str = "PENDING_VALIDATION";

/// One row of the projection table: what it is, the `RequestCertificate` body, and the
/// names the certificate covers.
type RecordCase<'a> = (&'a str, Value, &'a [&'a str]);

/// Every summary of one certificate, walked one page at a time, so a challenge that is
/// right only on the first element of the first page cannot pass.
async fn paged_summaries(
    router: &smithy_server_runtime::MultiServiceRouter,
    arn: &str,
    max_items: usize,
) -> Vec<Value> {
    let mut walked: Vec<Value> = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let mut body = json!({ "CertificateArn": arn, "MaxItems": max_items });
        if let Some(token) = next.as_ref() {
            body["NextToken"] = json!(token);
        }
        let (page, token) = summaries(router, &body).await;
        walked.extend(page);
        next = token;
        if next.is_none() {
            return walked;
        }
        assert!(
            walked.len() <= 64,
            "paging did not terminate after {} summaries",
            walked.len()
        );
    }
}

/// The DNS challenge's resource record of the summary for `domain`.
fn challenge_record(case: &str, list: &[Value], domain: &str) -> Value {
    let summary = list
        .iter()
        .find(|summary| {
            summary["DomainName"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(domain))
        })
        .unwrap_or_else(|| panic!("case {case}: no summary for {domain} in {list:?}"));
    let configuration = &summary["RequestedValidationConfiguration"];
    let challenge = &configuration["ValidationChallenge"]["DnsValidationChallenge"];
    assert!(
        challenge.is_object(),
        "case {case}: a DNS-validated name carries the DNS challenge arm, got {configuration}"
    );
    assert_resource_record(case, &challenge["ResourceRecord"], &summary["DomainName"]);
    // The CNAME target is a name under the zone ACM publishes its challenges in, so a
    // client that follows the record reaches ACM rather than a name of its own.
    let value = challenge["ResourceRecord"]["Value"]
        .as_str()
        .unwrap_or_default();
    assert!(
        value.starts_with('_') && value.ends_with(&format!(".{ACM_VALIDATIONS_ZONE}.")),
        "case {case}: expected a CNAME target in {ACM_VALIDATIONS_ZONE}, got {value:?}"
    );
    challenge["ResourceRecord"].clone()
}

/// Call `UpdateCertificateOptions` with one `Options` body, requiring success.
async fn migrate_to(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    arn: &str,
    method: &str,
) {
    let (status, response) = call(
        router,
        "UpdateCertificateOptions",
        &json!({ "CertificateArn": arn, "Options": { "ValidationMethod": method } }),
    )
    .await;
    assert_eq!(
        (status, &response),
        (200, &json!({})),
        "case {case}: migrating onto {method} must succeed, got {status} {response}"
    );
}

/// RULE: every DNS-validated name's challenge carries that name's minted CNAME, the same
/// record the certificate's detail reports, on every page and however the name is
/// spelled.
#[tokio::test]
async fn every_dns_challenge_carries_the_cname_the_detail_reports() {
    // Rows differ along the form of each covered name, how many names are covered, and
    // whether ACM has to prove control of them at all (public against private CA).
    let cases: &[RecordCase<'_>] = &[
        (
            "one apex name",
            json!({ "DomainName": "example.com", "ValidationMethod": "DNS" }),
            &["example.com"],
        ),
        (
            "an apex and its www name",
            json!({
                "DomainName": "apex.example.net",
                "SubjectAlternativeNames": ["www.apex.example.net"],
                "ValidationMethod": "DNS",
            }),
            &["apex.example.net", "www.apex.example.net"],
        ),
        (
            "a wildcard beside two names, one of them deep",
            json!({
                "DomainName": "*.wild.example.org",
                "SubjectAlternativeNames": ["wild.example.org", "a.b.wild.example.org"],
                "ValidationMethod": "DNS",
            }),
            &["*.wild.example.org", "wild.example.org", "a.b.wild.example.org"],
        ),
        (
            "the method named through Options rather than the top-level member",
            json!({
                "DomainName": "options.example.io",
                "Options": { "ValidationMethod": "DNS" },
            }),
            &["options.example.io"],
        ),
        (
            "a private certificate authority, which needs no proof of control",
            json!({
                "DomainName": "private.example.co",
                "ValidationMethod": "DNS",
                "CertificateAuthorityArn": PRIVATE_CA_ARN,
            }),
            &["private.example.co"],
        ),
    ];

    for (case, body, names) in cases {
        // A fresh store per row, so one row's certificate cannot explain another's.
        let (router, engine) = router_with_engine();
        let arn = request_certificate(&router, body).await;
        // The row is about the record the request minted, so the outcome the clock owes
        // the request is held out rather than raced.
        hold_pending_validations(&engine);

        // Read whole and one summary per page: the record must be there either way.
        let (whole, _next) = summaries(&router, &json!({ "CertificateArn": arn })).await;
        let paged = paged_summaries(&router, &arn, 1).await;
        let described = described_validations(&router, &arn).await;

        let mut records: BTreeSet<String> = BTreeSet::new();
        for name in *names {
            let row = format!("{case}/{name}");
            let record = challenge_record(&row, &whole, name);
            assert_eq!(
                challenge_record(&format!("{row}/paged"), &paged, name),
                record,
                "case {row}: the page the summary falls on cannot change its record"
            );

            let entry = described
                .get(*name)
                .unwrap_or_else(|| panic!("case {row}: {name} is not a described validation"));
            assert_eq!(
                &record,
                &entry["ResourceRecord"],
                "case {row}: the challenge must carry the stored CNAME, got {entry}"
            );
            assert_eq!(
                entry["ValidationStatus"].as_str(),
                Some(PENDING_VALIDATION),
                "case {row}: the row holds the validation pending, got {entry}"
            );
            records.insert(record["Name"].as_str().unwrap_or_default().to_string());
        }
        assert_eq!(
            records.len(),
            names.len(),
            "case {case}: each covered name is proven by a record of its own, got {records:?}"
        );

        // A second read reports the same record: reading the challenges mints nothing.
        let (again, _next) = summaries(&router, &json!({ "CertificateArn": arn })).await;
        assert_eq!(
            again, whole,
            "case {case}: reading the challenges again must report the same records"
        );
    }
}

/// RULE: the challenge follows the method a domain's validation currently stands under,
/// so a domain MIGRATED onto DNS reports the CNAME minted for it then — and one migrated
/// off DNS reports its new method's challenge instead, with no record left behind.
#[tokio::test]
async fn a_domain_migrated_onto_dns_reports_its_new_cname() {
    // Rows differ along the method the certificate starts on (named, defaulted) and the
    // form and number of the names covered.
    let cases: &[RecordCase<'_>] = &[
        (
            "EMAIL validation, one name",
            json!({
                "DomainName": "mail.example.com",
                "ValidationMethod": "EMAIL",
                "CertificateAuthorityArn": PRIVATE_CA_ARN,
            }),
            &["mail.example.com"],
        ),
        (
            "HTTP validation, a wildcard beside a plain name",
            json!({
                "DomainName": "*.http.example.net",
                "SubjectAlternativeNames": ["http.example.net"],
                "ValidationMethod": "HTTP",
                "CertificateAuthorityArn": PRIVATE_CA_ARN,
            }),
            &["*.http.example.net", "http.example.net"],
        ),
        (
            "no method named, so the EMAIL default",
            json!({
                "DomainName": "defaulted.example.org",
                "SubjectAlternativeNames": ["alt.defaulted.example.org"],
                "CertificateAuthorityArn": PRIVATE_CA_ARN,
            }),
            &["defaulted.example.org", "alt.defaulted.example.org"],
        ),
    ];

    for (case, body, names) in cases {
        let (router, engine) = router_with_engine();
        let arn = request_certificate(&router, body).await;
        // Reconfiguring a certificate's options is for one ACM has issued, which a
        // private-CA request becomes once the outcome it is owed falls due.
        make_validation_due_now(&engine);

        // Before the migration the names carry no DNS challenge at all.
        let (before, _next) = summaries(&router, &json!({ "CertificateArn": arn })).await;
        for summary in &before {
            assert!(
                summary["RequestedValidationConfiguration"]["ValidationChallenge"]
                    ["DnsValidationChallenge"]
                    .is_null(),
                "case {case}: a name not validated by DNS carries no DNS challenge, got {summary}"
            );
        }

        migrate_to(&router, case, &arn, "DNS").await;
        let (migrated, _next) = summaries(&router, &json!({ "CertificateArn": arn })).await;
        let described = described_validations(&router, &arn).await;
        for name in *names {
            let row = format!("{case}/{name}/migrated onto DNS");
            let record = challenge_record(&row, &migrated, name);
            let entry = described
                .get(*name)
                .unwrap_or_else(|| panic!("case {row}: {name} is not a described validation"));
            assert_eq!(
                &record,
                &entry["ResourceRecord"],
                "case {row}: the challenge must carry the re-minted CNAME, got {entry}"
            );
        }

        // Migrated back onto a method that publishes no record, the challenge is that
        // method's and carries no CNAME.
        migrate_to(&router, case, &arn, "EMAIL").await;
        let (reverted, _next) = summaries(&router, &json!({ "CertificateArn": arn })).await;
        for summary in &reverted {
            let challenge = &summary["RequestedValidationConfiguration"]["ValidationChallenge"];
            assert!(
                challenge["EmailValidationChallenge"].is_object()
                    && challenge["DnsValidationChallenge"].is_null(),
                "case {case}: an email-validated name carries the email challenge alone, \
                 got {summary}"
            );
        }
    }
}

/// RULE: the record is this certificate's own — two certificates covering one name are
/// handed different CNAMEs, so no part of the challenge is a constant or an echo of the
/// request.
#[tokio::test]
async fn two_certificates_for_one_name_are_handed_different_records() {
    let domain = "shared.example.com";
    let (router, engine) = router_with_engine();
    let body = json!({ "DomainName": domain, "ValidationMethod": "DNS" });

    let first = request_certificate(&router, &body).await;
    // A distinct request for the same name: ACM's idempotency token is what makes it a
    // second certificate rather than the first one again.
    let second = request_certificate(
        &router,
        &json!({
            "DomainName": domain,
            "ValidationMethod": "DNS",
            "IdempotencyToken": "secondrequest",
        }),
    )
    .await;
    assert_ne!(
        first, second,
        "case two certificates: the fixture must create two certificates"
    );
    hold_pending_validations(&engine);

    let (first_list, _next) = summaries(&router, &json!({ "CertificateArn": first })).await;
    let (second_list, _next) = summaries(&router, &json!({ "CertificateArn": second })).await;
    let first_record = challenge_record("two certificates/first", &first_list, domain);
    let second_record = challenge_record("two certificates/second", &second_list, domain);
    assert_ne!(
        first_record, second_record,
        "case two certificates: each certificate proves the name with its own record, \
         got {first_record} twice"
    );
}
