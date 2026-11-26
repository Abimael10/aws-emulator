// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an imported certificate's VALIDITY WINDOW decides: whether
//! the import is admitted at all, and — once it is — the bounds the record reports.
//!
//! RULE under test: `ImportCertificate` admits material only while its validity window is
//! OPEN at the instant of the call — "the current time must be between the Not Before and
//! Not After certificate fields" (the operation's own guidelines) — and the sentence of the
//! refusal names which side of the window the material fell on, and whether it was the
//! certificate or its chain. Every such refusal is `ValidationException` (HTTP 400) and
//! stores nothing. The window of an ADMITTED certificate is read out of its DER and
//! reported on the record as `NotBefore`/`NotAfter`, which is where a caller reads how much
//! of it is left.
//!
//! Every certificate here is built by `rcgen` — something other than the code under
//! test — and every call goes through the same router the emulator serves production
//! traffic on, with each admitted import read back through `DescribeCertificate` and
//! `GetCertificate`. The tables span what the rule generalises over: where the window
//! sits relative to the instant a test runs at (closed decades ago, closed more
//! recently, open, not yet opened), how long the window is, the curve of the key
//! carrying it, and — for the chain — which member of it is outside its window and in
//! which position.

use serde_json::{json, Value};

use crate::certificate_import::{blob, generate, import, import_body, Blueprint};
use crate::certificate_lifecycle::{call, describe, error_code, error_message, list_all_arns, router};

/// The sentence for a `Certificate` whose window has NOT OPENED yet — one of the two sides
/// of the window an import is refused on, as recorded against the real service. Written out
/// here rather than imported from the service, so no assertion is made against the template
/// it is asserting.
const CERTIFICATE_NOT_YET_VALID: &str = "The certificate is not valid yet. You can Import a \
     certificate only during its validity period.";

/// The sentence for a `Certificate` whose window has CLOSED: the other side of the same
/// bound, in the service's own words for it.
const CERTIFICATE_EXPIRED: &str =
    "The certificate is expired. You can Import a certificate only during its validity \
     period.";

/// The sentence for a `CertificateChain` carrying a certificate whose window has not
/// opened yet: it names the chain, and identifies no member of it.
const CHAIN_CERTIFICATE_NOT_YET_VALID: &str = "A certificate in the chain is not valid yet. You \
     can Import a certificate only during its validity period.";

/// And for a chain member whose window has closed, named the same way.
const CHAIN_CERTIFICATE_EXPIRED: &str = "A certificate in the chain is expired. You can Import \
     a certificate only during its validity period.";

/// One row of the window table: what it exercises, the `(not_before, not_after)` years
/// to generate the certificate with, the signature algorithm, the subject common name —
/// which an admitted row's record must report as its `DomainName` — and the sentence the
/// import must be refused under, or `None` for a window that is open.
type WindowCase<'a> = (
    &'a str,
    (i32, i32),
    &'static rcgen::SignatureAlgorithm,
    &'a str,
    Option<&'a str>,
);

/// The rows the window table runs over.
///
/// The years are absolute, so each row keeps the same relation to the clock whenever the
/// suite runs: 1998-1999, 2000-2001 and 2015-2016 have closed, 2020-2100 and 2024-2099 are
/// open, and 2990-2999 has not opened. The closed rows differ by up to seventeen years and
/// by curve, and the open rows by window length, so a rule that happened to admit one
/// window length rather than another cannot pass the table — and each side of the bound
/// earns its own sentence.
fn window_cases() -> Vec<WindowCase<'static>> {
    vec![
        (
            "a window that closed decades ago",
            (2000, 2001),
            &rcgen::PKCS_ECDSA_P256_SHA256,
            "long-closed.import.example.com",
            Some(CERTIFICATE_EXPIRED),
        ),
        (
            "a window that closed more recently, on another curve",
            (2015, 2016),
            &rcgen::PKCS_ECDSA_P384_SHA384,
            "recently-closed.import.example.com",
            Some(CERTIFICATE_EXPIRED),
        ),
        (
            "a single-year window far in the past",
            (1998, 1999),
            &rcgen::PKCS_ECDSA_P256_SHA256,
            "oldest.import.example.com",
            Some(CERTIFICATE_EXPIRED),
        ),
        (
            "a window that has not opened yet",
            (2990, 2999),
            &rcgen::PKCS_ECDSA_P384_SHA384,
            "not-yet-open.import.example.com",
            Some(CERTIFICATE_NOT_YET_VALID),
        ),
        (
            "a window open across any instant this runs at",
            (2020, 2100),
            &rcgen::PKCS_ECDSA_P256_SHA256,
            "open.import.example.com",
            None,
        ),
        (
            "a shorter window, open on another curve",
            (2024, 2099),
            &rcgen::PKCS_ECDSA_P384_SHA384,
            "also-open.import.example.com",
            None,
        ),
    ]
}

/// The `ListCertificates` filter every store read here carries: that operation reports
/// RSA_1024 and RSA_2048 certificates unless `Includes` names other key types, and the
/// certificates these tables import are the two elliptic curves their rows generate on.
fn elliptic_curve_certificates() -> Value {
    json!({ "Includes": { "keyTypes": ["EC_prime256v1", "EC_secp384r1"] } })
}

/// RULE: an import is admitted only while its window is open; material on either side of
/// it is refused in that side's own words, and the record of an admitted one reports its
/// window.
#[tokio::test]
async fn an_import_reports_its_own_window_and_is_refused_outside_it() {
    let router = router();

    for (case, validity, algorithm, common_name, refusal) in window_cases() {
        let generated = generate(&Blueprint {
            common_name: Some(common_name),
            algorithm,
            validity,
            ..Blueprint::default()
        });
        let body = import_body(&generated);

        if let Some(expected) = refusal {
            let (status, response) = call(&router, "ImportCertificate", &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a certificate outside its validity window must be refused \
                 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {response}"
            );
            assert_eq!(
                error_message(&response),
                expected,
                "case {case:?}: expected the message {expected:?}, got {}",
                error_message(&response)
            );
            continue;
        }

        // The import itself: material inside its window answers 200 with the ARN its
        // record is stored under.
        let arn = import(&router, case, &body).await;

        // And the record reports the certificate's own bounds, read out of its DER —
        // which is what a client inspects the window through.
        let detail = describe(&router, &arn).await;
        assert_eq!(
            detail["NotBefore"].as_f64(),
            Some(generated.not_before),
            "case {case:?}: expected NotBefore {} from the certificate's own DER, got {detail}",
            generated.not_before
        );
        assert_eq!(
            detail["NotAfter"].as_f64(),
            Some(generated.not_after),
            "case {case:?}: expected NotAfter {} from the certificate's own DER, got {detail}",
            generated.not_after
        );
        assert_eq!(
            detail["Serial"].as_str(),
            Some(generated.serial.as_str()),
            "case {case:?}: expected the imported certificate's own serial, got {detail}"
        );
        assert_eq!(
            detail["DomainName"].as_str(),
            Some(common_name),
            "case {case:?}: expected the certificate's own subject name, got {detail}"
        );
        assert_eq!(
            detail["Type"].as_str(),
            Some("IMPORTED"),
            "case {case:?}: an imported certificate is typed IMPORTED, got {detail}"
        );

        // The material itself is held, so the bodies the import carried are served back.
        let (status, served) = call(
            &router,
            "GetCertificate",
            &json!({ "CertificateArn": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetCertificate must answer 200, got {status} {served}"
        );
        assert_eq!(
            served["Certificate"].as_str(),
            Some(generated.certificate.as_str()),
            "case {case:?}: expected the imported body back byte for byte, got {served}"
        );
    }

    // ...and the refused rows left nothing behind: the store holds exactly the records
    // the admitted rows stored. The key types are named because `ListCertificates`
    // reports RSA_1024 and RSA_2048 certificates unless `Includes` says otherwise, and
    // every certificate here is an elliptic-curve one.
    let listed = list_all_arns(&router, &elliptic_curve_certificates()).await;
    let admitted = window_cases()
        .into_iter()
        .filter(|(_, _, _, _, refusal)| refusal.is_none())
        .count();
    assert_eq!(
        listed.len(),
        admitted,
        "expected only the {admitted} admitted import(s) to be stored, got {listed:?}"
    );
}

/// RULE: the same reading covers the CHAIN — a member outside its window refuses the
/// import, in that side's own words and whichever position the member holds in the body,
/// and every other chain is stored and served back as it was sent.
#[tokio::test]
async fn a_chain_is_refused_for_a_member_outside_its_window() {
    let router = router();

    let leaf = generate(&Blueprint {
        common_name: Some("chain-leaf.import.example.com"),
        ..Blueprint::default()
    });
    // Authorities whose own windows closed before any instant this suite can run at, one
    // that has not opened yet, and two that are open: which member of the chain is
    // outside its window, and where in the body it sits, is what the rows vary.
    let closed_root = generate(&Blueprint {
        common_name: Some("closed-root.import.example.com"),
        validity: (2000, 2001),
        ..Blueprint::default()
    });
    let open_root = generate(&Blueprint {
        common_name: Some("open-root.import.example.com"),
        validity: (2020, 2100),
        ..Blueprint::default()
    });
    let open_intermediate = generate(&Blueprint {
        common_name: Some("open-intermediate.import.example.com"),
        validity: (2024, 2099),
        ..Blueprint::default()
    });
    let future_root = generate(&Blueprint {
        common_name: Some("future-root.import.example.com"),
        validity: (2990, 2999),
        ..Blueprint::default()
    });

    let chains: &[(&str, String, Option<&str>)] = &[
        (
            "one certificate whose window has closed",
            closed_root.certificate.clone(),
            Some(CHAIN_CERTIFICATE_EXPIRED),
        ),
        (
            "a closed certificate below an open one",
            format!("{}{}", open_root.certificate, closed_root.certificate),
            Some(CHAIN_CERTIFICATE_EXPIRED),
        ),
        (
            "a closed certificate above an open one",
            format!("{}{}", closed_root.certificate, open_root.certificate),
            Some(CHAIN_CERTIFICATE_EXPIRED),
        ),
        (
            "a certificate whose window has not opened yet",
            future_root.certificate.clone(),
            Some(CHAIN_CERTIFICATE_NOT_YET_VALID),
        ),
        ("one open certificate", open_root.certificate.clone(), None),
        (
            "two open certificates",
            format!(
                "{}{}",
                open_intermediate.certificate, open_root.certificate
            ),
            None,
        ),
    ];

    for (case, chain, refusal) in chains {
        let mut body = import_body(&leaf);
        body["CertificateChain"] = blob(chain);

        if let Some(expected) = refusal {
            let (status, response) = call(&router, "ImportCertificate", &body).await;
            assert_eq!(
                status, 400,
                "case {case:?}: a chain member outside its validity window must be refused \
                 400, got {status} {response}"
            );
            assert_eq!(
                error_code(&response),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {response}"
            );
            assert_eq!(
                error_message(&response),
                *expected,
                "case {case:?}: expected the message {expected:?}, got {}",
                error_message(&response)
            );
            continue;
        }

        let arn = import(&router, case, &body).await;
        let detail = describe(&router, &arn).await;
        assert_eq!(
            detail["Serial"].as_str(),
            Some(leaf.serial.as_str()),
            "case {case:?}: the stored certificate must still be the leaf, got {detail}"
        );

        let (status, served) = call(
            &router,
            "GetCertificate",
            &json!({ "CertificateArn": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetCertificate must answer 200, got {status} {served}"
        );
        assert_eq!(
            served["CertificateChain"].as_str(),
            Some(chain.as_str()),
            "case {case:?}: expected the chain back as sent, got {served}"
        );
    }

    // The refused chains stored nothing: only the rows whose members were all open are
    // in the store ([`elliptic_curve_certificates`] for why the key types are named).
    let listed = list_all_arns(&router, &elliptic_curve_certificates()).await;
    let admitted = chains
        .iter()
        .filter(|(_, _, refusal)| refusal.is_none())
        .count();
    assert_eq!(
        listed.len(),
        admitted,
        "expected only the {admitted} admitted chain row(s) to be stored, got {listed:?}"
    );
}

/// The one assertion the chain rows make about a REFUSED import beyond its message: a
/// refusal is the whole answer, so the response carries no `CertificateArn` for the
/// caller to go on with.
#[tokio::test]
async fn a_refused_import_answers_no_certificate_arn() {
    let router = router();
    let not_yet_valid = generate(&Blueprint {
        common_name: Some("no-arn.import.example.com"),
        validity: (2990, 2999),
        ..Blueprint::default()
    });
    let (status, response): (u16, Value) =
        call(&router, "ImportCertificate", &import_body(&not_yet_valid)).await;
    assert_eq!(
        status, 400,
        "case \"a certificate whose window has not opened\": expected 400, got {status} \
         {response}"
    );
    assert!(
        response.get("CertificateArn").is_none(),
        "case \"a certificate whose window has not opened\": a refusal mints no ARN, got \
         {response}"
    );
}
