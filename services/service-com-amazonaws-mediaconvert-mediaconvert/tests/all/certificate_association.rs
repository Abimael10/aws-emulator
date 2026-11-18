// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of the account's ACM certificate associations:
//! `AssociateCertificate` and `DisassociateCertificate`.
//!
//! The rule these cases pin: the ARNs of the ACM certificates an account has associated
//! are a SET in the account's persisted state. `AssociateCertificate` adds the ARN it is
//! given to that set and `DisassociateCertificate` removes exactly the ARN it addresses,
//! leaving every other member in place. Either door asked for the state the account is
//! ALREADY in changes nothing and answers its own success status: a repeated association
//! answers the 201 again, and a removal of an association the account does not hold
//! answers the 202 again. A value that is not an ACM certificate ARN never reaches the
//! set: both doors answer it with the modelled `BadRequestException` (400). An ARN that IS
//! one but names no certificate the account holds is the modelled `NotFoundException`
//! (404) on the association door, which resolves the certificate against the service that
//! owns it.
//!
//! What the set is keyed by is the WHOLE ARN the request carried — `Arn` targets the
//! model's bare `__string` and nothing takes it apart — so two ARNs that differ anywhere
//! are two certificates, and an accepted association answers the modelled 201 with no
//! member at all (its response shape has none).
//!
//! The API has no operation that READS the set, and neither door's answer reveals whether
//! the account held the association, so a case asserts the STATUS a client sees at every
//! door and reads the SET itself out of the store the handlers write ([`holds`]). That is
//! what makes these cases fail against a handler that stores nothing.
//!
//! The wire harness is the one `job_lifecycle` builds: a single shared engine behind the
//! real multi-service router, so every case asserts on the bytes a client would see.
//!
//! WHETHER the certificate an ARN names exists is not this service's fact: the certificate
//! is Certificate Manager's resource, so the handler asks that service over the in-process
//! callback. A case that is about THAT answer mounts Certificate Manager beside this
//! service and gives it a certificate to hold
//! (`an_arn_that_names_no_certificate_the_account_holds_is_the_modelled_not_found`); the
//! other cases run with no Certificate Manager mounted, where the owning service answers
//! nothing and so establishes nothing — a silence that is never read as a refusal — and
//! what decides is the ARN's form alone.

use serde_json::{json, Value};
use service_com_amazonaws_mediaconvert_mediaconvert::state::MAX_CERTIFICATES;
use service_com_amazonaws_mediaconvert_mediaconvert::ComAmazonawsMediaconvertMediaconvertState;

use super::job_lifecycle::{Emulator, Reply};
// The one path-label encoder this test binary has: the tagging suite addresses its
// resource by ARN exactly as `DisassociateCertificate` does, so the encoding rule is
// shared rather than written again here.
use super::resource_tagging::encode_label;

/// The certificate collection's URI — the `http` binding of both operations
/// (`smithy_query(query="operation",operation="AssociateCertificate,DisassociateCertificate")`:
/// `POST /2017-08-29/certificates` and `DELETE /2017-08-29/certificates/{Arn}`).
const CERTIFICATES_URI: &str = "/2017-08-29/certificates";

async fn associate(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .send("POST", CERTIFICATES_URI, Some(json!({ "arn": arn })))
        .await
}

async fn disassociate(emulator: &Emulator, arn: &str) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!("{CERTIFICATES_URI}/{}", encode_label(arn)),
            None,
        )
        .await
}

/// How this service words the refusal of a member that carries no characters.
fn blank_refusal(member: &str) -> String {
    format!("{member} must not be empty.")
}

/// How this service words the refusal of a value that is not an ACM certificate ARN.
fn arn_refusal(arn: &str) -> String {
    format!("Arn {arn} is not the ARN of an ACM certificate.")
}

/// How this service words a request that addresses something the store does not hold.
fn not_found_refusal(member: &str) -> String {
    format!("The resource you specified in {member} was not found.")
}

/// Assert `reply` is the modelled bad request carrying `message`.
fn assert_bad_request(case: &str, message: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 400,
        "case {case:?}: expected HTTP 400, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some("BadRequestException"),
        "case {case:?}: expected BadRequestException, got {:?} with body {}",
        reply.error_type,
        reply.body
    );
    assert_eq!(
        reply.string("/message"),
        message,
        "case {case:?}: expected the refusal to read {message:?}, got body {}",
        reply.body
    );
}

/// Whether the account's stored set of associated certificates holds `arn`.
///
/// The set is read out of the store the handlers write, because the API has no operation
/// that reports it and neither door's answer reveals it: both answer their own success
/// status whether or not the account held the association. What a CLIENT sees is asserted
/// at the door in every case; this is how a case tells what was STORED.
fn holds(emulator: &Emulator, arn: &str) -> bool {
    emulator
        .engine
        .state::<ComAmazonawsMediaconvertMediaconvertState>()
        .lock()
        .certificates
        .contains(arn)
}

/// Assert the account's certificate set holds `arn` exactly when `expected`.
fn assert_holds(case: &str, emulator: &Emulator, arn: &str, expected: bool) {
    let held = holds(emulator, arn);
    assert_eq!(
        held, expected,
        "case {case:?}: expected the account's certificate set to hold {arn:?} to be \
         {expected}, got {held}"
    );
}

/// Assert `reply` is the empty 202 a removal answers with — the removal of a held
/// association and of one the account does not hold alike.
fn assert_disassociated(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 202,
        "case {case:?}: expected HTTP 202, got {} with body {}",
        reply.status, reply.body
    );
    assert!(
        reply.at("/message").is_null(),
        "case {case:?}: a removed association answers an empty body, got {}",
        reply.body
    );
}

/// Assert `reply` is the 201 an association answers with.
///
/// The association's response shape carries NO members
/// (`smithy_query(query="operation",operation="AssociateCertificate")`: `output {}`, `http`
/// code 201), so an accepted association answers that status and nothing else — no error
/// type, and no member invented for the body.
fn assert_associated(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 201,
        "case {case:?}: expected HTTP 201, got {} with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        None,
        "case {case:?}: an accepted association carries no error type, got {:?}",
        reply.error_type
    );
    assert!(
        reply.body.is_null() || reply.body.as_object().is_some_and(|body| body.is_empty()),
        "case {case:?}: the association's response shape has no members, got body {}",
        reply.body
    );
}

/// Call one Certificate Manager operation the way a client does: `awsJson1_1`, `POST /`
/// with `X-Amz-Target: CertificateManager.<Op>` and an `acm`-scoped credential.
async fn acm(emulator: &Emulator, operation: &str, body: Value) -> (u16, Value) {
    emulator
        .rpc("acm", "CertificateManager", "1.1", operation, body)
        .await
}

/// A certificate the ACCOUNT really holds: requested through Certificate Manager itself,
/// which answers the ARN it issued for it.
async fn given_certificate(emulator: &Emulator, case: &str, domain: &str) -> String {
    let (status, body) = acm(
        emulator,
        "RequestCertificate",
        json!({ "DomainName": domain, "ValidationMethod": "DNS" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RequestCertificate answered {status} {body}"
    );
    body["CertificateArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no CertificateArn in {body}"))
        .to_string()
}

/// The same ARN with its certificate id altered in its last character: an ARN of exactly
/// the right form that names a certificate the account does not hold.
fn near_miss(arn: &str) -> String {
    let mut altered = arn.to_string();
    let last = altered.pop().unwrap_or('0');
    altered.push(if last == 'a' { 'b' } else { 'a' });
    altered
}

/// Assert `reply` is the modelled 404 for an `Arn` that names no certificate the account
/// holds — the same status, error shape and sentence a lookup that resolves nothing gets
/// anywhere in this service, naming the request member the lookup read.
fn assert_no_such_certificate(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 404,
        "case {case:?}: expected HTTP 404 for an ARN that names no certificate, got {} \
         with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.error_type.as_deref(),
        Some("NotFoundException"),
        "case {case:?}: expected NotFoundException, got {:?} with body {}",
        reply.error_type,
        reply.body
    );
    assert_eq!(
        reply.string("/message"),
        not_found_refusal("arn"),
        "case {case:?}: expected the member-naming lookup refusal, got body {}",
        reply.body
    );
}

/// An `Arn` of the right form that names NO certificate the account holds is the modelled
/// `NotFoundException`, and nothing is associated for it; the certificate the account DOES
/// hold is associated and reads back.
///
/// Certificate Manager is mounted beside this service on the same engine, so the question
/// "does the account hold a certificate under this ARN?" is answered by the service that
/// owns certificates, over the same in-process callback the production router gives it.
/// The refused rows span the ways an ARN of the right form can name nothing: a near miss
/// on the certificate id of one the account holds, another account's, another Region's,
/// another partition's, an id nothing ever issued, and a certificate the account held
/// until it was deleted — which also shows the answer is asked for afresh each time
/// rather than remembered.
#[tokio::test]
async fn an_arn_that_names_no_certificate_the_account_holds_is_the_modelled_not_found() {
    let emulator =
        Emulator::beside(service_com_amazonaws_acm_certificatemanager::EmulatedService::new);

    // One certificate the account really holds, and one it holds only until it is deleted.
    let held = given_certificate(&emulator, "the certificate ACM issued", "media.example.com").await;
    let deleted = given_certificate(&emulator, "a certificate about to be deleted", "old.example.com").await;
    let (status, body) = acm(
        &emulator,
        "DeleteCertificate",
        json!({ "CertificateArn": deleted }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DeleteCertificate answered {status} {body}"
    );

    // The certificate the account holds is associated, and the association is persisted:
    // the store holds it, and its removal takes it back out.
    assert_associated(
        "the certificate the account holds",
        &associate(&emulator, &held).await,
    );
    assert_holds("the certificate the account holds", &emulator, &held, true);
    assert_disassociated(
        "the certificate the account holds",
        &disassociate(&emulator, &held).await,
    );
    assert_holds(
        "the certificate the account holds, after its removal",
        &emulator,
        &held,
        false,
    );

    for (case, arn) in [
        ("a near miss on a held certificate's id", near_miss(&held)),
        (
            "another account's certificate",
            "arn:aws:acm:us-west-2:210987654321:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7b"
                .to_string(),
        ),
        (
            "another Region's certificate",
            "arn:aws:acm:eu-central-1:123456789012:certificate/0b1d7e8a-5c3f-4a2e-9d6b-7f8e9a0b1c2d"
                .to_string(),
        ),
        (
            "another partition's certificate",
            "arn:aws-us-gov:acm:us-gov-west-1:123456789012:certificate/4f0b2c1d-8e7a-4b3c-9d2e-1a0b9c8d7e6f"
                .to_string(),
        ),
        (
            "an id nothing ever issued",
            "arn:aws:acm:us-west-2:123456789012:certificate/00000000-0000-0000-0000-000000000000"
                .to_string(),
        ),
        ("a certificate that was deleted", deleted.clone()),
    ] {
        assert_no_such_certificate(case, &associate(&emulator, &arn).await);
        // Nothing was stored for it: the account holds no association under that ARN.
        assert_holds(&format!("{case}, after the refusal"), &emulator, &arn, false);
    }

    // And the certificate the account holds is still associable, so the refusals above
    // were about the ARN and not about the door.
    assert_associated(
        "the certificate the account holds, again",
        &associate(&emulator, &held).await,
    );
}

/// The ACM certificate ARNs the table below is driven through.
///
/// Both members are documented as "the ARN of the ACM certificate" that is associated with
/// (or disassociated from) the MediaConvert resource, and target the model's bare
/// `__string` — no pattern and no length
/// (`smithy_query(query="operation",operation="AssociateCertificate,DisassociateCertificate")`),
/// so what makes a value acceptable is that it is an ACM certificate ARN,
/// `arn:<partition>:acm:<region>:<account>:certificate/<id>`. The rows differ along every
/// field of that form: the partition, the region, the account and the shape of the
/// certificate id — including an id that is not a UUID, which the model's `__string`
/// admits.
fn certificates() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "the standard partition",
            "arn:aws:acm:us-west-2:123456789012:certificate/12345678-1234-1234-1234-123456789012",
        ),
        (
            "another region",
            "arn:aws:acm:eu-central-1:123456789012:certificate/0b1d7e8a-5c3f-4a2e-9d6b-7f8e9a0b1c2d",
        ),
        (
            "another account",
            "arn:aws:acm:us-west-2:210987654321:certificate/aaaabbbb-cccc-dddd-eeee-ffff00001111",
        ),
        (
            "the GovCloud partition",
            "arn:aws-us-gov:acm:us-gov-west-1:123456789012:certificate/4f0b2c1d-8e7a-4b3c-9d2e-1a0b9c8d7e6f",
        ),
        (
            "the China partition",
            "arn:aws-cn:acm:cn-north-1:123456789012:certificate/9c8b7a65-4321-4fed-cba9-876543210fed",
        ),
        (
            "a certificate id that is not a UUID",
            "arn:aws:acm:us-west-2:123456789012:certificate/legacy-media-cert-01",
        ),
    ]
}

/// A certificate is associated with the account until it is disassociated, and the
/// association is per certificate: removing one leaves every other one in place.
///
/// Every row runs against ONE account in sequence, so each certificate is associated while
/// the others already are. What proves the set is persisted is the order of facts for a
/// single ARN: not held before it is associated (nothing is pre-seeded), held once it is,
/// and not held again after it has been removed — while every removal answers the same
/// 202, including the removal of an association the account does not hold.
#[tokio::test]
async fn a_certificate_is_associated_with_the_account_until_it_is_disassociated() {
    let emulator = Emulator::new();
    let rows = certificates();

    // Nothing is associated on a fresh account: every row addresses no association yet,
    // and removing one that is not there is the same success and changes nothing.
    for (case, arn) in &rows {
        let case = format!("{case}, before any association");
        assert_holds(&case, &emulator, arn, false);
        assert_disassociated(&case, &disassociate(&emulator, arn).await);
        assert_holds(&case, &emulator, arn, false);
    }

    // Associate them all, then remove them one at a time: after each removal the one just
    // removed is gone and every one still associated is still there.
    for (case, arn) in &rows {
        assert_associated(case, &associate(&emulator, arn).await);
        assert_holds(case, &emulator, arn, true);
    }

    for (index, (case, arn)) in rows.iter().enumerate() {
        assert_disassociated(case, &disassociate(&emulator, arn).await);
        assert_holds(case, &emulator, arn, false);
        // The same ARN a second time: the association is gone, so there is nothing left to
        // remove under it and the answer is the same 202.
        assert_disassociated(
            &format!("{case}, disassociated twice"),
            &disassociate(&emulator, arn).await,
        );
        assert_holds(&format!("{case}, disassociated twice"), &emulator, arn, false);

        // A disassociation removes exactly the ARN it names: every row already addressed
        // stays removed, and every row not yet addressed is still associated.
        for (other_case, other_arn) in rows.iter().take(index) {
            assert_holds(
                &format!("{other_case}, after {case} was disassociated"),
                &emulator,
                other_arn,
                false,
            );
        }
        for (other_case, other_arn) in rows.iter().skip(index + 1) {
            assert_holds(
                &format!("{other_case}, while {case} was disassociated"),
                &emulator,
                other_arn,
                true,
            );
        }
    }
}

/// Associating a certificate the account already has associated asks for the state the
/// account is in: the repeat answers the same 201 and the account still holds that ONE
/// association, so a single disassociation removes it.
///
/// The rows differ in how many times the association is repeated, so the set cannot be
/// storing one entry per call.
#[tokio::test]
async fn associating_the_same_certificate_again_leaves_one_association() {
    for (case, arn) in certificates() {
        for repeats in [1_usize, 2, 5] {
            let emulator = Emulator::new();
            for attempt in 1..=repeats {
                assert_associated(
                    &format!("{case}, association #{attempt} of {repeats}"),
                    &associate(&emulator, arn).await,
                );
            }
            assert_holds(
                &format!("{case}, after {repeats} association(s)"),
                &emulator,
                arn,
                true,
            );

            assert_disassociated(
                &format!("{case}, after {repeats} association(s)"),
                &disassociate(&emulator, arn).await,
            );
            // One removal is enough however many times it was associated: the account
            // holds a SET, not one entry per call.
            assert_holds(
                &format!("{case}, after one removal of {repeats} association(s)"),
                &emulator,
                arn,
                false,
            );
        }
    }
}

/// The values that are not ACM certificate ARNs, which neither door accepts.
///
/// The rows span the ways a value can fail to be one: not an ARN at all, an ARN of another
/// service, an ACM ARN naming another kind of resource, one with no resource id, one whose
/// certificate id is empty, and one missing an ARN field altogether.
fn not_certificate_arns() -> Vec<(&'static str, &'static str)> {
    vec![
        ("not an ARN at all", "certificate/12345678-1234-1234-1234-123456789012"),
        ("a bare certificate id", "12345678-1234-1234-1234-123456789012"),
        (
            "an ARN of another service",
            "arn:aws:mediaconvert:us-west-2:123456789012:queues/Default",
        ),
        (
            "an ACM ARN naming another kind of resource",
            "arn:aws:acm:us-west-2:123456789012:certificate-authority/12345678-1234-1234-1234-123456789012",
        ),
        (
            "an ACM ARN with no resource id",
            "arn:aws:acm:us-west-2:123456789012:certificate",
        ),
        (
            "an ACM ARN whose certificate id is empty",
            "arn:aws:acm:us-west-2:123456789012:certificate/",
        ),
        (
            "an ARN missing the account field",
            "arn:aws:acm:us-west-2:certificate/12345678-1234-1234-1234-123456789012",
        ),
    ]
}

/// A value that is not an ACM certificate ARN is refused by BOTH doors with the modelled
/// bad request, and changes nothing: the certificate the account does hold is still
/// associated afterwards, and the refused value never becomes an association of its own.
#[tokio::test]
async fn a_value_that_is_not_an_acm_certificate_arn_is_refused_by_both_doors() {
    let emulator = Emulator::new();
    // One association the refusals must leave exactly as it is.
    let (held_case, held) = certificates()[0];
    assert_associated(held_case, &associate(&emulator, held).await);

    for (case, value) in not_certificate_arns() {
        assert_bad_request(
            &format!("AssociateCertificate with {case}"),
            &arn_refusal(value),
            &associate(&emulator, value).await,
        );
        assert_bad_request(
            &format!("DisassociateCertificate with {case}"),
            &arn_refusal(value),
            &disassociate(&emulator, value).await,
        );
    }

    // An `Arn` that carries no characters is refused naming the member instead: it is not
    // a mis-shaped ARN, it is an absent value. The zero-length form is sent to the
    // associate door only — it is no path segment at all, so it addresses no operation.
    assert_bad_request(
        "AssociateCertificate with no characters at all",
        &blank_refusal("Arn"),
        &associate(&emulator, "").await,
    );
    for blank in [" ", "   ", "\t", "\n"] {
        assert_bad_request(
            &format!("AssociateCertificate with the blank value {blank:?}"),
            &blank_refusal("Arn"),
            &associate(&emulator, blank).await,
        );
        assert_bad_request(
            &format!("DisassociateCertificate with the blank value {blank:?}"),
            &blank_refusal("Arn"),
            &disassociate(&emulator, blank).await,
        );
    }

    // The one real association survived every refusal above, and nothing else was stored.
    assert_holds(held_case, &emulator, held, true);
    assert_disassociated(held_case, &disassociate(&emulator, held).await);
    assert_holds(held_case, &emulator, held, false);
}

/// Caller-driven growth is bounded: the account associates certificates up to its capacity
/// and then answers the modelled quota error instead of growing further. A certificate
/// already associated is still a no-op success at capacity, and disassociating one makes
/// room for exactly one more.
#[tokio::test]
async fn associating_past_the_account_capacity_is_a_quota_refusal() {
    let emulator = Emulator::new();
    let arn_at = |index: usize| {
        format!("arn:aws:acm:us-west-2:123456789012:certificate/bulk-{index:04}")
    };

    for index in 0..MAX_CERTIFICATES {
        assert_associated(
            &format!("certificate #{index} of {MAX_CERTIFICATES}"),
            &associate(&emulator, &arn_at(index)).await,
        );
    }

    // Past the cap: the modelled quota error, for every further certificate.
    for index in [MAX_CERTIFICATES, MAX_CERTIFICATES + 1] {
        let arn = arn_at(index);
        let refused = associate(&emulator, &arn).await;
        assert_eq!(
            refused.status, 402,
            "case certificate #{index} at capacity: expected HTTP 402, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ServiceQuotaExceededException"),
            "case certificate #{index} at capacity: expected ServiceQuotaExceededException, \
             got {:?} with body {}",
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.string("/message"),
            format!("An account associates at most {MAX_CERTIFICATES} ACM certificates."),
            "case certificate #{index} at capacity: expected the refusal to name the bound, \
             got body {}",
            refused.body
        );
        // Nothing was stored for it: the account holds no association under that ARN.
        assert_holds(
            &format!("certificate #{index} after the refusal"),
            &emulator,
            &arn,
            false,
        );
    }

    // A certificate the account already holds adds nothing, so it is accepted at capacity.
    assert_associated(
        "a certificate already associated, at capacity",
        &associate(&emulator, &arn_at(0)).await,
    );

    // Room for exactly one more once one is removed.
    assert_disassociated("the first certificate", &disassociate(&emulator, &arn_at(0)).await);
    assert_associated(
        "one more after a removal",
        &associate(&emulator, &arn_at(MAX_CERTIFICATES)).await,
    );
    let refused = associate(&emulator, &arn_at(MAX_CERTIFICATES + 1)).await;
    assert_eq!(
        refused.status, 402,
        "case back at capacity: expected HTTP 402, got {} with body {}",
        refused.status, refused.body
    );
}

/// One ACM certificate ARN, and the ARNs that differ from it in exactly one field.
///
/// `Arn` targets the model's bare `__string` with no pattern, so the member carries the
/// whole ARN and nothing takes it apart: two ARNs are the same certificate only when they
/// are the same string. The rows differ along every field an ARN has — the partition, the
/// region, the account and the certificate id — including two ways an id can be a
/// near-miss: one character apart, and one extending the other.
fn near_miss_pairs() -> Vec<(&'static str, &'static str, &'static str)> {
    // The ARN every row's variant is built from: a certificate id of the shape ACM issues.
    const BASE: &str =
        "arn:aws:acm:us-west-2:123456789012:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7b";
    vec![
        (
            "certificate ids one character apart",
            BASE,
            "arn:aws:acm:us-west-2:123456789012:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7c",
        ),
        (
            "one certificate id extending the other",
            BASE,
            "arn:aws:acm:us-west-2:123456789012:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7b-replica",
        ),
        (
            "the same certificate id in another case",
            BASE,
            "arn:aws:acm:us-west-2:123456789012:certificate/3F5C1B7A-9D2E-4C6F-8A1B-2C3D4E5F6A7B",
        ),
        (
            "another region",
            BASE,
            "arn:aws:acm:us-east-1:123456789012:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7b",
        ),
        (
            "another account",
            BASE,
            "arn:aws:acm:us-west-2:210987654321:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7b",
        ),
        (
            "another partition",
            BASE,
            "arn:aws-us-gov:acm:us-west-2:123456789012:certificate/3f5c1b7a-9d2e-4c6f-8a1b-2c3d4e5f6a7b",
        ),
    ]
}

/// What `AssociateCertificate` adds is the exact ARN the request carried: two ARNs that
/// differ anywhere are two certificates, so associating one never associates the other and
/// never replaces it.
///
/// Each pair runs on its own account, so what is observed is only the two ARNs: the second
/// one is not associated after the first one is, both are held once both are associated,
/// and each removal addresses exactly its own ARN.
#[tokio::test]
async fn an_association_is_keyed_by_the_whole_arn() {
    for (case, left, right) in near_miss_pairs() {
        let emulator = Emulator::new();

        assert_associated(&format!("{case}, the first ARN"), &associate(&emulator, left).await);
        // The near miss is a different certificate, so the account does not hold it yet.
        assert_holds(
            &format!("{case}, the second ARN before it is associated"),
            &emulator,
            right,
            false,
        );

        assert_associated(&format!("{case}, the second ARN"), &associate(&emulator, right).await);
        // Associating the second one did not replace the first: both are held, and each
        // removal takes only the ARN it addresses.
        assert_holds(&format!("{case}, both ARNs associated"), &emulator, left, true);
        assert_holds(&format!("{case}, both ARNs associated"), &emulator, right, true);
        assert_disassociated(
            &format!("{case}, removing the first ARN"),
            &disassociate(&emulator, left).await,
        );
        assert_holds(
            &format!("{case}, the first ARN after its removal"),
            &emulator,
            left,
            false,
        );
        assert_holds(
            &format!("{case}, the second ARN after the first was removed"),
            &emulator,
            right,
            true,
        );
        assert_disassociated(
            &format!("{case}, removing the second ARN"),
            &disassociate(&emulator, right).await,
        );
        assert_holds(
            &format!("{case}, the second ARN after its removal"),
            &emulator,
            right,
            false,
        );
    }
}
