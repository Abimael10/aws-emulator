// SPDX-License-Identifier: CC0-1.0

//! WHICH absence a certificate read or deregistration reports, driven end to end over HTTP.
//!
//! Every case goes through the same front door a client uses: `POST /` with
//! `X-Amz-Target: DirectoryService_20150416.<Operation>` and an `awsJson1_1` body
//! (`crate::harness`), so nothing here reaches into the state struct and an answer that is
//! not computed from persisted state cannot pass.
//!
//! The rule under test: the CERTIFICATE is the subject of these two calls, so the absence
//! they report is the certificate's — a `CertificateId` no registration of the account
//! answers to is refused with `CertificateDoesNotExistException` whatever the `DirectoryId`
//! the request looked for it under turns out to be — and the directory-level absence
//! (`DirectoryDoesNotExistException`) is reported only for a request whose certificate the
//! account DOES hold, where the directory it was looked for under is the thing that is
//! wrong. A caller branching on the class is then told which of the two identifiers to act
//! on, and an idempotent "remove it if it is there" client reads the same answer a describe
//! does.
//!
//! It is asserted over a TABLE spanning the pairs the rule distinguishes — a certificate id
//! nothing registered against a directory the account does not have, the same id against a
//! directory it does, a registration of ANOTHER directory looked for under an absent
//! directory, and a registration looked for under the directory that holds it — against BOTH
//! operations that resolve a registration, with the stage refusal of a directory still being
//! built as the one answer that outranks both.

use serde_json::json;

use crate::certificates::{certificate_pem, directory_in_service, register, TYPE_CLIENT_LDAPS};
use crate::harness::{call, ok, service};
use crate::microsoft_ad::minimal_microsoft_ad;

/// The operations whose subject is a registered certificate, and which therefore resolve one
/// before anything else about the request
/// (`smithy_query(query="operation",operation="DescribeCertificate,DeregisterCertificate")`
/// — both model `CertificateDoesNotExistException` and `DirectoryDoesNotExistException`).
const LOOKUPS: [&str; 2] = ["DescribeCertificate", "DeregisterCertificate"];

/// Well-formed `DirectoryId`s (`^d-[0-9a-f]{10}$`) this account never creates, varied along
/// their digits so no row can pass by matching one recorded id.
const ABSENT_DIRECTORY_IDS: [&str; 3] = ["d-90000ff0ab", "d-0000000000", "d-ffffffffff"];

/// Well-formed `CertificateId`s (`^c-[0-9a-f]{10}$`) no registration here ever mints.
const ABSENT_CERTIFICATE_IDS: [&str; 3] = ["c-00112233aa", "c-0000000000", "c-ffffffffff"];

/// RULE: a certificate id no registration of the account answers to is refused as the
/// CERTIFICATE's absence — through either operation, and whether or not the directory the
/// request named exists.
#[tokio::test]
async fn an_unregistered_certificate_is_refused_as_the_certificates_absence() {
    let router = service();
    let held = directory_in_service(&router, "cert-absence.example.com", "the held one").await;

    // (case, the directory the lookup names) — the rows differ along what the DirectoryId
    // resolves to, which the rule says does not change the class.
    let mut directories: Vec<(String, String)> = ABSENT_DIRECTORY_IDS
        .iter()
        .map(|absent| {
            (
                format!("against the absent directory {absent}"),
                (*absent).to_string(),
            )
        })
        .collect();
    directories.push((
        "against a directory the account holds".to_string(),
        held.clone(),
    ));

    for operation in LOOKUPS {
        for (clause, directory_id) in &directories {
            for certificate_id in ABSENT_CERTIFICATE_IDS {
                let case = format!("{operation} {clause}, certificate {certificate_id}");
                let (status, parsed) = call(
                    &router,
                    operation,
                    json!({ "DirectoryId": directory_id, "CertificateId": certificate_id }),
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the lookup to be refused, got {status} with \
                     {parsed}"
                );
                assert_eq!(
                    parsed["__type"],
                    json!("CertificateDoesNotExistException"),
                    "case {case:?}: expected the certificate's absence, got {parsed}"
                );
                let request_id = parsed["RequestId"].as_str().unwrap_or_default();
                assert!(
                    !request_id.is_empty(),
                    "case {case:?}: expected the refusal to report a RequestId, got {parsed}"
                );
                let message = parsed["Message"].as_str().unwrap_or_default();
                assert!(
                    message.contains(certificate_id) && message.ends_with(request_id),
                    "case {case:?}: expected a message naming the certificate and closing \
                     with the call's request id, got {message:?}"
                );
            }
        }
    }
}

/// RULE: the DIRECTORY's absence is what is reported for a certificate the account DOES hold
/// — the id is a real registration, looked for under a directory that is not there — so the
/// two classes stay the two different things a caller branches on.
#[tokio::test]
async fn a_held_certificate_under_an_absent_directory_reports_the_directorys_absence() {
    let router = service();
    let owner = directory_in_service(&router, "cert-owner.example.com", "the owner").await;
    let (pem, _) = certificate_pem(Some("cert-owner.example.com"), (2033, 8, 9));
    let registered = register(
        &router,
        json!({
            "DirectoryId": &owner,
            "CertificateData": pem,
            "Type": TYPE_CLIENT_LDAPS
        }),
        "the registration",
    )
    .await;
    // The registration is in the inventory from the register on — only the state it reports
    // is owed to the clock — so the rows below wait for nothing.

    for operation in LOOKUPS {
        for absent in ABSENT_DIRECTORY_IDS {
            let case = format!("{operation} for a held certificate under absent {absent}");
            let (status, parsed) = call(
                &router,
                operation,
                json!({ "DirectoryId": absent, "CertificateId": &registered }),
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the lookup to be refused, got {status} with {parsed}"
            );
            assert_eq!(
                parsed["__type"],
                json!("DirectoryDoesNotExistException"),
                "case {case:?}: expected the directory's absence, got {parsed}"
            );
            let message = parsed["Message"].as_str().unwrap_or_default();
            assert!(
                message.contains(absent),
                "case {case:?}: expected the message to name the directory the request \
                 asked for, got {message:?}"
            );
        }
    }

    // The positive control for both rules: the same registration, under the directory that
    // holds it, is READ rather than refused — so what refuses the rows above is the pair of
    // identifiers and not the operation.
    let answer = ok(
        &router,
        "DescribeCertificate",
        json!({ "DirectoryId": &owner, "CertificateId": &registered }),
        "the registration under its own directory",
    )
    .await;
    assert_eq!(
        answer["Certificate"]["CertificateId"],
        json!(&registered),
        "expected the registration to be read back, got {answer}"
    );
}

/// RULE: a directory that is STORED but not in service cannot serve a deregistration from
/// that stage at all, which is the answer a caller gets ahead of either absence — so the
/// certificate resolution above does not swallow the stage refusal a client waits on.
#[tokio::test]
async fn a_deregistration_during_a_build_is_still_refused_on_the_stage() {
    let router = service();
    let building = ok(
        &router,
        "CreateMicrosoftAD",
        minimal_microsoft_ad("still-building.example.com"),
        "a directory still being built",
    )
    .await;
    let directory_id = building["DirectoryId"].as_str().unwrap_or_default().to_string();

    let case = "a deregistration inside the build";
    let (status, parsed) = call(
        &router,
        "DeregisterCertificate",
        json!({ "DirectoryId": &directory_id, "CertificateId": ABSENT_CERTIFICATE_IDS[0] }),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected the write to be refused, got {status} with {parsed}"
    );
    assert_eq!(
        parsed["__type"],
        json!("DirectoryUnavailableException"),
        "case {case:?}: expected the stage refusal, got {parsed}"
    );
}
