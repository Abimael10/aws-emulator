// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what an imported certificate's VALIDITY WINDOW is: a fact the
//! record reports, never a reason to refuse the import.
//!
//! RULE under test: `ImportCertificate` admits the material a request carries on what
//! this service can read and hold out of it — one PEM certificate, PEM certificates for
//! the chain, an unencrypted private key matching the certificate's public key, a key
//! type `KeyAlgorithm` declares, a non-empty `Issuer`, at least one name — and the
//! certificate's own `Not Before`/`Not After` bounds are REPORTED as `NotBefore` and
//! `NotAfter` rather than compared with the serving host's clock. A comparison between
//! this host's clock and bounds pinned inside the request's own material would make one
//! and the same import succeed on one machine and fail on another, which is not a
//! property of the request: so a certificate whose window closed long ago, one whose
//! window closed recently, one wide open and one that has not opened yet are all
//! accepted, and each is reported with its own bounds.
//!
//! Every certificate here is built by `rcgen` — something other than the code under
//! test — and every call goes through the same router the emulator serves production
//! traffic on, with each import read back through `DescribeCertificate` and
//! `GetCertificate`. The table spans what the rule generalises over: where the window
//! sits relative to any instant a test can run at (closed decades ago, closed recently,
//! open, not yet opened), how long it is, and the curve of the key carrying it.

use serde_json::json;

use crate::certificate_import::{blob, generate, import, import_body, Blueprint};
use crate::certificate_lifecycle::{call, describe, router};

/// One row of the window table: what it exercises, the `(not_before, not_after)` years
/// to generate the certificate with, the signature algorithm, and the subject common
/// name — which is what the stored record must report as its `DomainName`.
type WindowCase<'a> = (&'a str, (i32, i32), &'static rcgen::SignatureAlgorithm, &'a str);

/// The rows the window table runs over.
///
/// The years are absolute, so each row keeps the same relation to the clock whenever the
/// suite runs: 2000-2001 and 2015-2016 are closed, 2020-2100 is open, and 2990-2999 has
/// not opened. The two closed rows differ by fifteen years and by curve, so a rule that
/// happened to admit one window length rather than another cannot pass the table.
fn window_cases() -> Vec<WindowCase<'static>> {
    vec![
        (
            "a window that closed decades ago",
            (2000, 2001),
            &rcgen::PKCS_ECDSA_P256_SHA256,
            "long-closed.import.example.com",
        ),
        (
            "a window that closed more recently, on another curve",
            (2015, 2016),
            &rcgen::PKCS_ECDSA_P384_SHA384,
            "recently-closed.import.example.com",
        ),
        (
            "a window open across any instant this runs at",
            (2020, 2100),
            &rcgen::PKCS_ECDSA_P256_SHA256,
            "open.import.example.com",
        ),
        (
            "a window that has not opened yet",
            (2990, 2999),
            &rcgen::PKCS_ECDSA_P384_SHA384,
            "not-yet-open.import.example.com",
        ),
        (
            "a single-year window far in the past",
            (1998, 1999),
            &rcgen::PKCS_ECDSA_P256_SHA256,
            "oldest.import.example.com",
        ),
    ]
}

/// RULE: every certificate in the table is imported and reported with its OWN validity
/// window, whatever that window is relative to the clock serving the call.
#[tokio::test]
async fn an_import_is_admitted_whatever_its_validity_window_and_reports_that_window() {
    let router = router();

    for (case, validity, algorithm, common_name) in window_cases() {
        let generated = generate(&Blueprint {
            common_name: Some(common_name),
            algorithm,
            validity,
            ..Blueprint::default()
        });

        // The import itself: a window outside the serving clock's instant is no refusal,
        // so every row answers 200 with the ARN its record is stored under.
        let arn = import(&router, case, &import_body(&generated)).await;

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
}

/// RULE: a chain member's own window is no refusal either — the chain is material stored
/// to be served back, so a chain whose certificates' windows have closed is accepted and
/// handed back as it was sent, while the record keeps reporting the leaf.
#[tokio::test]
async fn a_chain_is_admitted_whatever_its_members_windows_are() {
    let router = router();

    let leaf = generate(&Blueprint {
        common_name: Some("chain-leaf.import.example.com"),
        ..Blueprint::default()
    });
    // Authorities whose own windows closed before any instant this suite can run at, and
    // one that is open: a chain is admitted whichever of them it carries, in whichever
    // order.
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

    let chains: &[(&str, String)] = &[
        ("one certificate whose window has closed", closed_root.certificate.clone()),
        (
            "a closed certificate below an open one",
            format!("{}{}", open_root.certificate, closed_root.certificate),
        ),
        (
            "a closed certificate above an open one",
            format!("{}{}", closed_root.certificate, open_root.certificate),
        ),
    ];

    for (case, chain) in chains {
        let mut body = import_body(&leaf);
        body["CertificateChain"] = blob(chain);
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
}
