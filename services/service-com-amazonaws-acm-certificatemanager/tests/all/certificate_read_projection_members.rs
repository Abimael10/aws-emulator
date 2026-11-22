// SPDX-License-Identifier: CC0-1.0

//! WHICH MEMBERS the ACM certificate read projections carry, end to end over the wire
//! (`POST /`, `X-Amz-Target: CertificateManager.<Op>`, awsJson1_1).
//!
//! RULES under test, each asserted over a table that spans what it generalises over:
//!
//! 1. `ListCertificateDomainValidations` reports the evidence a domain's validation
//!    stands under, read off the same stored record the certificate detail projects: the
//!    DNS arm carries that name's CNAME `ResourceRecord`, the record
//!    `CertificateDetail$DomainValidationOptions[*].ResourceRecord` reports for the same
//!    name, and the email arm's `ValidationDomain` is the covered name the summary is
//!    about, while the mail SUFFIX ACM writes to for that name is the detail's
//!    `[*].ValidationDomain`.
//! 2. `ValidationEmails` is "a list of email addresses that ACM used to send domain
//!    validation emails" (`smithy_query(query="doc",
//!    shape="com.amazonaws.acm#DomainValidation", member="ValidationEmails")`), so an
//!    email-validated name reports the EMPTY list until a dispatch has used an address
//!    and those addresses afterwards, through both projections, and a name ACM mails
//!    nothing for reports no such member at all.
//! 3. `CertificateDetail$DomainValidationOptions` carries one entry per name the
//!    certificate covers; for a certificate whose material came from outside ACM the
//!    entry carries that name alone, since ACM validated nothing for it.
//! 4. `CreatedAt` is the instant the certificate record came into being and is reported
//!    by every projection that models the member — the detail, the listing summary and
//!    the search metadata the operation's default `SortBy` of `CREATED_AT` orders a
//!    page by — for a certificate ACM was asked to issue and for an imported one alike.
//! 5. The transparency-logging preference a certificate that asked for none reports
//!    follows what ACM did with it: a certificate ACM issues is logged, and one it did
//!    not issue is not.
//! 6. `ListCertificateDomainValidations` reports its summaries in DOMAIN-NAME order —
//!    the total order it cuts and resumes its pages in — while the certificate's detail
//!    reports the same names in the order the request named them.
//!
//! Every assertion goes through `MultiServiceRouter::dispatch` on the shared harness in
//! [`crate::certificate_lifecycle`] and reads each claim back through an operation, so
//! nothing here can pass on a response the store does not back. Rows that must observe
//! one side of a clock-owed transition move the due instant the emulator itself stores
//! rather than racing the wall clock.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::certificate_domain_validations::summaries;
use crate::certificate_import::{generate, import, import_body, Blueprint};
use crate::certificate_lifecycle::{
    call, describe, dispatch_validation_mail_now, hold_pending_validations, request_certificate,
    router, router_with_engine, KEY_ALGORITHMS,
};

/// The mailbox local-parts ACM's dispatch writes to: "ACM sends email to the the
/// following five addresses: admin@subdomain.example.com
/// administrator@subdomain.example.com hostmaster@subdomain.example.com
/// postmaster@subdomain.example.com webmaster@subdomain.example.com"
/// (`smithy_query(query="doc", shape="com.amazonaws.acm#ResendValidationEmailRequest",
/// member="ValidationDomain")`). Written out here rather than read from the service, so
/// the catalog is not asserted against itself.
const VALIDATION_LOCAL_PARTS: &[&str] = &[
    "admin",
    "administrator",
    "hostmaster",
    "postmaster",
    "webmaster",
];

/// The `CertificateTransparencyLoggingPreference` values the model declares
/// (`smithy_query(query="shape",
/// shape="com.amazonaws.acm#CertificateTransparencyLoggingPreference")`).
const TRANSPARENCY_LOGGING_VALUES: &[&str] = &["ENABLED", "DISABLED"];

/// One mailbox row: what it exercises, the `RequestCertificate` body, and the
/// `name -> mail suffix` pairs the dispatched mail must be reported at. An empty suffix
/// is a name ACM mails nothing for.
type MailboxCase<'a> = (&'a str, Value, &'a [(&'a str, &'a str)]);

/// A described certificate's `DomainValidationOptions` entry for `domain`.
fn described(case: &str, detail: &Value, domain: &str) -> Value {
    detail["DomainValidationOptions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: no DomainValidationOptions in {detail}"))
        .iter()
        .find(|entry| {
            entry["DomainName"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(domain))
        })
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: no validation for {domain} in {detail}"))
}

/// One `ListCertificateDomainValidations` summary's requested configuration for
/// `domain`.
async fn configuration(router: &MultiServiceRouter, case: &str, arn: &str, domain: &str) -> Value {
    let (list, _next) = summaries(router, &json!({ "CertificateArn": arn })).await;
    list.iter()
        .find(|summary| {
            summary["DomainName"]
                .as_str()
                .is_some_and(|name| name.eq_ignore_ascii_case(domain))
        })
        .map(|summary| summary["RequestedValidationConfiguration"].clone())
        .unwrap_or_else(|| panic!("case {case}: no summary for {domain} in {list:?}"))
}

/// The `ListCertificates` summary for `arn`, requiring success.
///
/// The request names every modelled key algorithm in `Includes.keyTypes`, because
/// "default filtering returns only RSA_1024 and RSA_2048 certificates"
/// (`ListCertificatesRequest$Includes` doc) and these rows cover other algorithms too.
async fn summary(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, response) = call(
        router,
        "ListCertificates",
        &json!({ "MaxItems": 100, "Includes": { "keyTypes": KEY_ALGORITHMS } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: ListCertificates must succeed, got {status} {response}"
    );
    response["CertificateSummaryList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: no CertificateSummaryList in {response}"))
        .iter()
        .find(|entry| entry["CertificateArn"].as_str() == Some(arn))
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: {arn} is not listed in {response}"))
}

/// The `SearchCertificates` result metadata for `arn`, requiring success.
async fn search_metadata(router: &MultiServiceRouter, case: &str, arn: &str) -> Value {
    let (status, response) = call(router, "SearchCertificates", &json!({})).await;
    assert_eq!(
        status, 200,
        "case {case}: SearchCertificates must succeed, got {status} {response}"
    );
    response["Results"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case}: no Results in {response}"))
        .iter()
        .find(|result| result["CertificateArn"].as_str() == Some(arn))
        .map(|result| result["CertificateMetadata"]["AcmCertificateMetadata"].clone())
        .unwrap_or_else(|| panic!("case {case}: {arn} is not a search result in {response}"))
}

/// The five addresses at `suffix`, sorted, as a validation must report them once the
/// dispatch has used them.
fn mailboxes_at(suffix: &str) -> Vec<String> {
    let mut mailboxes: Vec<String> = VALIDATION_LOCAL_PARTS
        .iter()
        .map(|local_part| format!("{local_part}@{}", suffix.to_ascii_lowercase()))
        .collect();
    mailboxes.sort();
    mailboxes
}

/// The addresses a validation or a challenge reports, sorted, or `None` when the member
/// is absent.
fn reported_mailboxes(carrier: &Value) -> Option<Vec<String>> {
    let mut mailboxes: Vec<String> = carrier["ValidationEmails"]
        .as_array()?
        .iter()
        .map(|address| address.as_str().unwrap_or_default().to_ascii_lowercase())
        .collect();
    mailboxes.sort();
    Some(mailboxes)
}

/// RULE 1: for every DNS-validated name, the summary's challenge is the DNS arm carrying
/// that name's CNAME, the very record the detail of the same certificate reports.
#[tokio::test]
async fn the_dns_challenge_carries_the_cname_the_detail_reports() {
    // Rows differ along the number of names covered (one, an apex with a `www.` name,
    // three) and the form of each name (apex, subdomain, wildcard).
    let cases: &[(&str, Value, &[&str])] = &[
        (
            "one name",
            json!({ "DomainName": "solo.example.com", "ValidationMethod": "DNS" }),
            &["solo.example.com"],
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
            "a wildcard beside two names",
            json!({
                "DomainName": "*.wild.example.org",
                "SubjectAlternativeNames": ["wild.example.org", "api.wild.example.org"],
                "ValidationMethod": "DNS",
            }),
            &["*.wild.example.org", "wild.example.org", "api.wild.example.org"],
        ),
    ];

    for (case, body, names) in cases {
        // A store per row, so one row's certificate cannot explain another's.
        let (router, engine) = router_with_engine();
        let arn = request_certificate(&router, body).await;
        // The row is about the record the request created, so the transitions the clock
        // owes it are held out.
        hold_pending_validations(&engine);
        let detail = describe(&router, &arn).await;

        for name in *names {
            let row = format!("{case}/{name}");
            let configuration = configuration(&router, &row, &arn, name).await;
            let challenge = &configuration["ValidationChallenge"]["DnsValidationChallenge"];
            assert!(
                challenge.is_object(),
                "case {row}: a DNS-validated name carries the DNS challenge arm, got {configuration}"
            );
            assert_eq!(
                challenge.as_object().map(|arm| arm.len()),
                Some(1),
                "case {row}: the DNS challenge carries exactly its ResourceRecord, got {challenge}"
            );

            // The record is a real CNAME under the name being proven, and it is the same
            // record the certificate's detail reports for that name.
            let entry = described(&row, &detail, name);
            let record = &entry["ResourceRecord"];
            assert_eq!(
                &challenge["ResourceRecord"],
                record,
                "case {row}: both readers of one stored record report one CNAME, got {challenge}"
            );
            assert_eq!(
                record["Type"].as_str(),
                Some("CNAME"),
                "case {row}: the CNAME to publish is reported, got {entry}"
            );
            let proven = name.strip_prefix("*.").unwrap_or(name);
            let record_name = record["Name"].as_str().unwrap_or_default();
            assert!(
                record_name.starts_with('_') && record_name.ends_with(&format!(".{proven}.")),
                "case {row}: expected a validation label under {proven}, got {record_name:?}"
            );
        }
    }
}

/// RULE 2: both projections report the addresses ACM has USED — the empty list for an
/// email-validated name until a dispatch has used one, the five role-account addresses
/// at that name's own mail suffix afterwards, and no member at all for a name ACM mails
/// nothing for. The challenge names the domain the summary is ABOUT, while the mail
/// suffix ACM writes to is the certificate's detail to report.
#[tokio::test]
async fn a_mailbox_list_is_reported_only_for_a_dispatch_that_happened() {
    // Rows differ along the validation method (named and defaulted), the number of
    // covered names, the form of each name and the mail suffix each is written at (the
    // name itself, a superdomain the request chose, the domain a wildcard covers). An
    // empty suffix is a name ACM mails nothing for.
    let cases: &[MailboxCase<'_>] = &[
        (
            "one email-validated name, mailed at itself",
            json!({ "DomainName": "solo.example.com", "ValidationMethod": "EMAIL" }),
            &[("solo.example.com", "solo.example.com")],
        ),
        (
            "no method named, so ACM validates by email",
            json!({ "DomainName": "defaulted.example.net" }),
            &[("defaulted.example.net", "defaulted.example.net")],
        ),
        (
            "three names, one mailed at a suffix the request chose",
            json!({
                "DomainName": "a.deep.example.io",
                "SubjectAlternativeNames": ["b.deep.example.io", "*.wild.example.io"],
                "ValidationMethod": "EMAIL",
                "DomainValidationOptions": [
                    { "DomainName": "a.deep.example.io", "ValidationDomain": "example.io" }
                ],
            }),
            &[
                ("a.deep.example.io", "example.io"),
                ("b.deep.example.io", "b.deep.example.io"),
                ("*.wild.example.io", "wild.example.io"),
            ],
        ),
        (
            "DNS validation, which mails nothing",
            json!({ "DomainName": "dns.example.com", "ValidationMethod": "DNS" }),
            &[("dns.example.com", "")],
        ),
        (
            "HTTP validation, which mails nothing",
            json!({ "DomainName": "http.example.com", "ValidationMethod": "HTTP" }),
            &[("http.example.com", "")],
        ),
    ];

    for (case, body, names) in cases {
        let (router, engine) = router_with_engine();
        let arn = request_certificate(&router, body).await;
        // Both transitions the clock owes the request are held out, so the row observes
        // the state before any dispatch whatever the wall clock does.
        hold_pending_validations(&engine);

        let unmailed = describe(&router, &arn).await;
        for (name, suffix) in *names {
            let row = format!("{case}/{name}/before the dispatch");
            let mails = !suffix.is_empty();
            // An email-validated name reports the list ACM has used, which is empty;
            // a name ACM mails nothing for reports no list at all.
            assert_eq!(
                reported_mailboxes(&described(&row, &unmailed, name)),
                mails.then(Vec::new),
                "case {row}: ACM has used no address yet, got {unmailed}"
            );
            let configuration = configuration(&router, &row, &arn, name).await;
            let challenge = &configuration["ValidationChallenge"]["EmailValidationChallenge"];
            assert_eq!(
                reported_mailboxes(challenge),
                mails.then(Vec::new),
                "case {row}: the challenge reports the same used addresses, got {configuration}"
            );
            if mails {
                // The challenge is about THIS name; the suffix ACM mails is the
                // certificate's own record, which the detail is what reports.
                assert_eq!(
                    challenge["ValidationDomain"].as_str(),
                    Some(*name),
                    "case {row}: the challenge names the domain it is about, got {configuration}"
                );
                assert_eq!(
                    described(&row, &unmailed, name)["ValidationDomain"].as_str(),
                    Some(*suffix),
                    "case {row}: the detail reports the suffix ACM mails, got {unmailed}"
                );
            }
        }

        // The dispatch the clock owed then falls due: every name ACM mails reports the
        // five addresses at its own suffix, through both readers, and a name ACM mails
        // nothing for still reports none.
        dispatch_validation_mail_now(&engine);
        let mailed = describe(&router, &arn).await;
        for (name, suffix) in *names {
            let row = format!("{case}/{name}/after the dispatch");
            let expected = (!suffix.is_empty()).then(|| mailboxes_at(suffix));
            let entry = described(&row, &mailed, name);
            assert_eq!(
                reported_mailboxes(&entry),
                expected,
                "case {row}: expected {expected:?}, got {entry}"
            );
            let configuration = configuration(&router, &row, &arn, name).await;
            let challenge = &configuration["ValidationChallenge"]["EmailValidationChallenge"];
            assert_eq!(
                reported_mailboxes(challenge),
                expected,
                "case {row}: the two readers of one record must agree, got {configuration}"
            );
        }
    }
}

/// RULE 6: `ListCertificateDomainValidations` reports the covered names in DOMAIN-NAME
/// order — the total order it pages in — whichever name the request made the primary
/// one, while the certificate's detail reports them in the order the request named.
#[tokio::test]
async fn the_summaries_are_reported_in_domain_name_order() {
    // Rows differ along where the stored order and the name order disagree: a primary
    // name that sorts last, one that sorts first, a wildcard (whose '*' sorts before
    // every letter) and a set of three names in reverse order.
    let cases: &[(&str, Value, &[&str], &[&str])] = &[
        (
            "a primary name that sorts after its SAN",
            json!({
                "DomainName": "www.order.example.com",
                "SubjectAlternativeNames": ["order.example.com"],
                "ValidationMethod": "DNS",
            }),
            &["www.order.example.com", "order.example.com"],
            &["order.example.com", "www.order.example.com"],
        ),
        (
            "a primary name that already sorts first",
            json!({
                "DomainName": "api.order.example.net",
                "SubjectAlternativeNames": ["www.order.example.net"],
                "ValidationMethod": "EMAIL",
            }),
            &["api.order.example.net", "www.order.example.net"],
            &["api.order.example.net", "www.order.example.net"],
        ),
        (
            "three names named in reverse order, one of them a wildcard",
            json!({
                "DomainName": "zed.order.example.org",
                "SubjectAlternativeNames": ["mid.order.example.org", "*.order.example.org"],
                "ValidationMethod": "DNS",
            }),
            &[
                "zed.order.example.org",
                "mid.order.example.org",
                "*.order.example.org",
            ],
            &[
                "*.order.example.org",
                "mid.order.example.org",
                "zed.order.example.org",
            ],
        ),
    ];

    for (case, body, requested, sorted) in cases {
        let (router, engine) = router_with_engine();
        let arn = request_certificate(&router, body).await;
        hold_pending_validations(&engine);

        // The whole list, and then one page at a time: a name order that only holds on
        // the first page would leave the paged walk in another order.
        let (list, next) = summaries(&router, &json!({ "CertificateArn": arn })).await;
        assert!(
            next.is_none(),
            "case {case}: every name fit in one page, got {next:?}"
        );
        let reported: Vec<String> = list
            .iter()
            .map(|summary| summary["DomainName"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            reported,
            sorted.iter().map(|name| (*name).to_string()).collect::<Vec<_>>(),
            "case {case}: expected the summaries in domain-name order, got {reported:?}"
        );

        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let mut page_body = json!({ "CertificateArn": arn, "MaxItems": 1 });
            if let Some(resume) = token.as_ref() {
                page_body["NextToken"] = json!(resume);
            }
            let (page, next) = summaries(&router, &page_body).await;
            walked.extend(
                page.iter()
                    .map(|summary| summary["DomainName"].as_str().unwrap_or_default().to_string()),
            );
            token = next;
            if token.is_none() {
                break;
            }
            assert!(
                walked.len() <= requested.len(),
                "case {case}: paging did not terminate, got {walked:?}"
            );
        }
        assert_eq!(
            walked, reported,
            "case {case}: the paged walk must report the same order, got {walked:?}"
        );

        // The certificate's own detail keeps the order the request named the names in.
        let detail = describe(&router, &arn).await;
        let described: Vec<String> = detail["DomainValidationOptions"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no DomainValidationOptions in {detail}"))
            .iter()
            .map(|entry| entry["DomainName"].as_str().unwrap_or_default().to_string())
            .collect();
        assert_eq!(
            described,
            requested.iter().map(|name| (*name).to_string()).collect::<Vec<_>>(),
            "case {case}: the detail reports the names as the request named them, got {described:?}"
        );
    }
}

/// RULES 3, 4 and 5 for a certificate whose material came from outside ACM: its names
/// are reported as validation entries carrying nothing but the name, its creation
/// instant is the instant it was imported and is reported by every projection, and the
/// transparency-logging preference it reports is the one of a certificate ACM did not
/// issue.
#[tokio::test]
async fn an_imported_certificate_reports_its_names_and_its_creation_instant() {
    // Rows differ along the number of names the imported certificate asserts, where it
    // asserts them (the subject, the extension, both) and their form (plain, wildcard).
    let cases: &[(&str, Blueprint<'_>, &[&str])] = &[
        (
            "one name in the subject",
            Blueprint {
                common_name: Some("solo.example.com"),
                ..Blueprint::default()
            },
            &["solo.example.com"],
        ),
        (
            "a subject name beside three extension names",
            Blueprint {
                common_name: Some("one.example.net"),
                dns_names: &["two.example.net", "three.example.net", "four.example.net"],
                ..Blueprint::default()
            },
            &[
                "one.example.net",
                "two.example.net",
                "three.example.net",
                "four.example.net",
            ],
        ),
        (
            "no subject name: the extension's first name is the primary one",
            Blueprint {
                common_name: None,
                organization: Some("Example Org"),
                dns_names: &["only.example.org", "second.example.org"],
                ..Blueprint::default()
            },
            &["only.example.org", "second.example.org"],
        ),
        (
            "a wildcard name",
            Blueprint {
                common_name: Some("*.wild.example.io"),
                dns_names: &["*.wild.example.io"],
                ..Blueprint::default()
            },
            &["*.wild.example.io"],
        ),
    ];

    for (case, blueprint, names) in cases {
        // A store per row, so one row's certificate cannot explain another's.
        let router = router();
        let arn = import(&router, case, &import_body(&generate(blueprint))).await;
        let detail = describe(&router, &arn).await;

        // One entry per covered name, in the order the certificate's names are stored,
        // each carrying the name alone.
        let entries = detail["DomainValidationOptions"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case}: no DomainValidationOptions in {detail}"));
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry["DomainName"].as_str().unwrap_or_default().to_string())
                .collect::<Vec<_>>(),
            names.iter().map(|name| (*name).to_string()).collect::<Vec<_>>(),
            "case {case}: expected one entry per covered name, got {detail}"
        );
        for entry in entries {
            assert_eq!(
                entry.as_object().map(|members| members.len()),
                Some(1),
                "case {case}: ACM validated nothing for an imported certificate, so the \
                 entry carries its name alone, got {entry}"
            );
        }

        // The creation instant: the one the import recorded, reported by every
        // projection that models the member.
        let created_at = detail["CreatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case}: expected a numeric CreatedAt, got {detail}")
        });
        assert_eq!(
            detail["CreatedAt"], detail["ImportedAt"],
            "case {case}: an imported certificate was created when it was imported, got {detail}"
        );
        let listed = summary(&router, case, &arn).await;
        assert_eq!(
            listed["CreatedAt"].as_f64(),
            Some(created_at),
            "case {case}: the listing summary reports the same instant, got {listed}"
        );
        let metadata = search_metadata(&router, case, &arn).await;
        assert_eq!(
            metadata["CreatedAt"].as_f64(),
            Some(created_at),
            "case {case}: the search metadata reports the same instant, got {metadata}"
        );

        // A certificate ACM did not issue is not logged by ACM.
        assert_eq!(
            detail["Options"]["CertificateTransparencyLoggingPreference"].as_str(),
            Some("DISABLED"),
            "case {case}: ACM logs no certificate it did not issue, got {detail}"
        );
        assert!(
            TRANSPARENCY_LOGGING_VALUES.contains(&"DISABLED"),
            "the expected preference must be a modelled value"
        );
    }
}

/// RULES 4 and 5 for a certificate ACM was asked to issue: its creation instant is
/// reported by every projection, and the preference it reports when it asked for none
/// is the logged one — the value a request that DID ask is reported under being its
/// own.
#[tokio::test]
async fn a_requested_certificate_reports_its_creation_instant_and_its_logging_preference() {
    // Rows differ along the validation method, the number of covered names, whether the
    // request names a private certificate authority, and whether it asks for a
    // transparency-logging preference at all (each modelled value, and none).
    let mut cases: Vec<(String, Value, &str)> = vec![
        (
            "DNS validation, one name, no preference asked for".to_string(),
            json!({ "DomainName": "dns.example.com", "ValidationMethod": "DNS" }),
            "ENABLED",
        ),
        (
            "EMAIL validation, two names, no preference asked for".to_string(),
            json!({
                "DomainName": "mail.example.net",
                "SubjectAlternativeNames": ["smtp.mail.example.net"],
                "ValidationMethod": "EMAIL",
            }),
            "ENABLED",
        ),
        (
            "a private certificate, which needs no domain validation".to_string(),
            json!({
                "DomainName": "private.example.org",
                "CertificateAuthorityArn":
                    crate::certificate_domain_validations::PRIVATE_CA_ARN,
            }),
            "ENABLED",
        ),
    ];
    for preference in TRANSPARENCY_LOGGING_VALUES {
        cases.push((
            format!("a request that asked for {preference}"),
            json!({
                "DomainName": format!("asked-{}.example.com", preference.to_lowercase()),
                "ValidationMethod": "DNS",
                "Options": { "CertificateTransparencyLoggingPreference": preference },
            }),
            preference,
        ));
    }

    for (case, body, expected_preference) in &cases {
        let (router, engine) = router_with_engine();
        let arn = request_certificate(&router, body).await;
        hold_pending_validations(&engine);
        let detail = describe(&router, &arn).await;

        let created_at = detail["CreatedAt"].as_f64().unwrap_or_else(|| {
            panic!("case {case}: expected a numeric CreatedAt, got {detail}")
        });
        let listed = summary(&router, case, &arn).await;
        assert_eq!(
            listed["CreatedAt"].as_f64(),
            Some(created_at),
            "case {case}: the listing summary reports the same instant, got {listed}"
        );
        let metadata = search_metadata(&router, case, &arn).await;
        assert_eq!(
            metadata["CreatedAt"].as_f64(),
            Some(created_at),
            "case {case}: the search metadata reports the same instant, got {metadata}"
        );
        assert_eq!(
            detail["Options"]["CertificateTransparencyLoggingPreference"].as_str(),
            Some(*expected_preference),
            "case {case}: expected the preference {expected_preference}, got {detail}"
        );
    }
}
