// SPDX-License-Identifier: CC0-1.0

//! Which members `AmendBenefitApplication` may write, end to end over the wire.
//!
//! Every assertion goes through the real request path — the shared `Harness` of
//! `tests/all/benefit_application_lifecycle.rs` (`MultiServiceRouter::dispatch` with
//! `awsJson1_0` bodies, `POST /` and `X-Amz-Target:
//! PartnerCentralBenefitsService.<Operation>`) — and every effect is read back through
//! `GetBenefitApplication`.
//!
//! The rule under test: an amendment may write every member of the application a CALLER
//! supplies and `UpdateBenefitApplication` also writes — `Name`, `Description`,
//! `BenefitApplicationDetails`, `PartnerContacts` and `FileDetails` — and only those; a
//! path naming a member the service owns is refused with the modelled
//! `ValidationException`. The two list members hold structures, so the path below one
//! addresses a single element (its 0-based position) and the member of that element to
//! write, and the `NewValue` must satisfy the constraints the model puts on THAT member.
//!
//! The tests are tables. Their rows differ along what the rule generalises over: which
//! member a `FieldPath` names (every member of a `Contact` and both members of a
//! `FileInput`), how the element position is written (a bracket subscript, a `.`-separated
//! segment, JSON-path form), how many amendments one request carries, and each published
//! length bound of the member the path names plus one past it — including this service's
//! own bound where the model constrains the member not at all.
//!
//! The last two tests cover what a stored attachment's SERVICE-OWNED members do once an
//! amendment wrote the caller-supplied content they follow: `FileName` and `FileType`
//! follow the `FileURI`, while `CreatedBy` and `CreatedAt` report "the identifier of the
//! user who uploaded the file" and "the timestamp when the file was uploaded"
//! (`smithy_query(query="shape",shape="FileDetail")`) and so belong to the FILE rather than
//! to the request that last wrote the list or to the position the file holds in it — the
//! rule `tests/all/file_upload_facts.rs` drives on the create and the update paths, driven
//! here on the amendment. No assertion on an instant depends on how fast the host runs: an
//! instant a call recorded is pinned to a window bounded below by an instant already stored
//! and above by the record's own `UpdatedAt`, and a carried instant is compared for
//! equality with the one the read before the amendment reported.

use serde_json::{json, Value};
use service_com_amazonaws_partnercentralbenefits_partnercentralbenefitsservice::state::{
    MAX_DETAILS_VALUE_LEN, STATUS_IN_REVIEW,
};

use crate::amend_benefit_application::{amend_body, amendment, clause, message_of};
use crate::application_status_gates::reach;
use crate::benefit_application_lifecycle::{
    error_code, instant, text, Harness, ACCESS_KEY_ID, CATALOG,
};

/// The modelled `@pattern` of `ContactEmail`, without the `^`/`$` anchors this service
/// states a pattern requirement without — from
/// `smithy_query(query="shape",shape="Contact")`, member `Email`.
const EMAIL_PATTERN: &str = "[a-z0-9!#$%&'*+/=?^_`{|}~-]+(?:\\.[a-z0-9!#$%&'*+/=?^_`{|}~-]+)*@(?:[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\\.)+[a-z0-9](?:[a-z0-9-]*[a-z0-9])?";

/// The modelled `@pattern` of `FileURI`, likewise unanchored — from
/// `smithy_query(query="shape",shape="FileInput")`, member `FileURI`.
const FILE_URI_PATTERN: &str = "(s3://|https://).*";

/// The modelled `@length` of every `Contact` string member
/// (`ContactEmail`/`ContactFirstName`/`ContactLastName` and `BusinessTitle`: 0..80).
const CONTACT_MAX_LEN: usize = 80;

/// The modelled `@length` maximum of `FileURI` (1..2000).
const FILE_URI_MAX_LEN: usize = 2000;

/// The requirement a `FieldPath` that names no amendable field at all fails, as the
/// handler states it.
const UNAMENDABLE: &str = "Member must name one of the amendable fields (Name, Description, \
                           BenefitApplicationDetails, PartnerContacts, FileDetails), a \
                           `.`-separated path into BenefitApplicationDetails, or a member \
                           of one element of PartnerContacts or FileDetails";

/// The requirement a `FieldPath` fails when it does not address one element of a list
/// member and one member of that element, as the handler states it.
fn element_path_requirement(field: &str) -> String {
    format!(
        "Member must name a member of one element of {field}, as that element's 0-based \
         position followed by the member name"
    )
}

/// The requirement a `FieldPath` fails when it addresses an element the list does not
/// hold.
fn element_count_requirement(field: &str, count: usize) -> String {
    format!("Member must name a 0-based element position within the {count} elements {field} holds")
}

/// The requirement a `FieldPath` fails when it names a member of a list element the
/// service owns rather than one a caller supplies.
fn element_member_requirement(field: &str, members: &[&str]) -> String {
    format!(
        "Member must name one of the amendable members of a {field} element ({})",
        members.join(", ")
    )
}

/// An email address of exactly `length` characters that the modelled pattern admits, so
/// the published `@length` bound and one past it can both be sent.
fn email_of_length(length: usize) -> String {
    let domain = "@example.com";
    format!("{}{domain}", "a".repeat(length - domain.len()))
}

/// An `s3://` URI of exactly `length` characters, for the same reason.
fn file_uri_of_length(length: usize) -> String {
    let prefix = "s3://bucket/";
    format!("{prefix}{}", "u".repeat(length - prefix.len()))
}

/// The value a `.`-separated path names in a read application: a numeric segment indexes a
/// list, so a probe path reads like the `FieldPath` that wrote it.
fn at<'a>(body: &'a Value, path: &str) -> &'a Value {
    path.split('.').fold(body, |value, step| {
        match step.parse::<usize>() {
            Ok(index) => &value[index],
            Err(_) => &value[step],
        }
    })
}

/// Create one application carrying a partner contact and two attachments, submit it, and
/// return its id — so every amendment below is made on a SUBMITTED application, the state
/// in which `UpdateBenefitApplication` is refused on status.
async fn submitted(harness: &Harness, case: &str, token: &str) -> String {
    let benefit_id = text(case, &harness.active_benefit_of(CATALOG).await, "Id");
    let created = harness
        .create(
            case,
            &json!({
                "Catalog": CATALOG,
                "ClientToken": token,
                "BenefitIdentifier": benefit_id,
                "Name": "contacts-and-attachments",
                "Description": "the application whose contacts and files are amended",
                "BenefitApplicationDetails": { "CampaignName": "seeded" },
                "PartnerContacts": [{ "Email": "ada@example.com", "FirstName": "Ada" }],
                "FileDetails": [
                    { "FileURI": "s3://bucket/first.csv", "BusinessUseCase": "the first attachment" },
                    { "FileURI": "https://example.com/second.pdf" },
                ],
            }),
        )
        .await;
    let id = text(case, &created, "Id");
    reach(harness, case, CATALOG, &id, STATUS_IN_REVIEW).await;
    id
}

// ---------------------------------------------------------------------------
// Applying
// ---------------------------------------------------------------------------

/// Every member of a partner contact and of an attachment that a caller supplies can be
/// amended on a submitted application, whichever form the path is written in; the next read
/// reports exactly that, the service-owned members that FOLLOW an amended value are derived
/// again, and the revision moves once per request.
#[tokio::test]
async fn every_caller_supplied_member_of_a_contact_or_an_attachment_can_be_amended() {
    let harness = Harness::new();
    let id = submitted(&harness, "seed", "tok-supplied-seed").await;

    /// One row: the amendments a request carries, and the `(path, expected JSON)` pairs the
    /// read must then report.
    type Row = (&'static str, Value, Vec<(&'static str, Value)>);
    let rows: Vec<Row> = vec![
        (
            "a contact email, the position subscripted",
            json!([amendment("PartnerContacts[0].Email", "grace@example.com")]),
            vec![("PartnerContacts.0.Email", json!("grace@example.com"))],
        ),
        (
            "a contact first name, the position a segment of its own",
            json!([amendment("PartnerContacts.0.FirstName", "Grace")]),
            vec![("PartnerContacts.0.FirstName", json!("Grace"))],
        ),
        (
            "a contact last name, in JSON-path form",
            json!([amendment("$.PartnerContacts[0].LastName", "Hopper")]),
            vec![("PartnerContacts.0.LastName", json!("Hopper"))],
        ),
        (
            "a contact business title",
            json!([amendment("PartnerContacts[0].BusinessTitle", "Rear Admiral")]),
            vec![("PartnerContacts.0.BusinessTitle", json!("Rear Admiral"))],
        ),
        (
            "a contact phone",
            json!([amendment("PartnerContacts[0].Phone", "+1-555-0100")]),
            vec![("PartnerContacts.0.Phone", json!("+1-555-0100"))],
        ),
        (
            "a phone at this service's bound on a member the model leaves unconstrained",
            json!([amendment(
                "PartnerContacts[0].Phone",
                &"5".repeat(MAX_DETAILS_VALUE_LEN)
            )]),
            vec![(
                "PartnerContacts.0.Phone",
                json!("5".repeat(MAX_DETAILS_VALUE_LEN)),
            )],
        ),
        (
            "an email at the published length bound",
            json!([amendment(
                "PartnerContacts[0].Email",
                &email_of_length(CONTACT_MAX_LEN)
            )]),
            vec![(
                "PartnerContacts.0.Email",
                json!(email_of_length(CONTACT_MAX_LEN)),
            )],
        ),
        (
            "an attachment URI, which the members derived from it follow",
            json!([amendment("FileDetails[0].FileURI", "s3://bucket/corrected.pdf")]),
            vec![
                ("FileDetails.0.FileURI", json!("s3://bucket/corrected.pdf")),
                ("FileDetails.0.FileName", json!("corrected.pdf")),
                ("FileDetails.0.FileType", json!("application/pdf")),
                // The attachment's own use case is untouched by a write to its URI.
                (
                    "FileDetails.0.BusinessUseCase",
                    json!("the first attachment"),
                ),
            ],
        ),
        (
            "an attachment business use case, on the second attachment",
            json!([amendment(
                "FileDetails.1.BusinessUseCase",
                "the corrected second attachment"
            )]),
            vec![(
                "FileDetails.1.BusinessUseCase",
                json!("the corrected second attachment"),
            )],
        ),
        (
            "an https attachment URI",
            json!([amendment(
                "FileDetails[1].FileURI",
                "https://example.com/third.png"
            )]),
            vec![
                (
                    "FileDetails.1.FileURI",
                    json!("https://example.com/third.png"),
                ),
                ("FileDetails.1.FileName", json!("third.png")),
                ("FileDetails.1.FileType", json!("image/png")),
            ],
        ),
        (
            "an attachment URI at the published length bound",
            json!([amendment(
                "FileDetails[0].FileURI",
                &file_uri_of_length(FILE_URI_MAX_LEN)
            )]),
            vec![
                (
                    "FileDetails.0.FileURI",
                    json!(file_uri_of_length(FILE_URI_MAX_LEN)),
                ),
                // The last path segment is longer than the modelled `FileName`, so the
                // service reports no file name for it rather than one the model refuses.
                ("FileDetails.0.FileName", Value::Null),
            ],
        ),
        (
            "a contact member and an attachment member in one request",
            json!([
                amendment("PartnerContacts[0].Email", "ada@example.org"),
                amendment("FileDetails[0].FileURI", "s3://bucket/both.csv"),
            ]),
            vec![
                ("PartnerContacts.0.Email", json!("ada@example.org")),
                ("FileDetails.0.FileURI", json!("s3://bucket/both.csv")),
                ("FileDetails.0.FileName", json!("both.csv")),
                ("FileDetails.0.FileType", json!("text/csv")),
            ],
        ),
        (
            // A subscript is a list index on the FIELD NAME only: a key of the details
            // document is a name the caller chose, so it is addressed exactly as written.
            "a details key that contains a bracket",
            json!([amendment("BenefitApplicationDetails.a[b]", "verbatim")]),
            vec![("BenefitApplicationDetails.a[b]", json!("verbatim"))],
        ),
    ];

    let mut before = harness.get("seed", &id).await;
    for (index, (case, amendments, expected)) in rows.iter().enumerate() {
        let revision = text(case, &before, "Revision");
        harness
            .ok(
                case,
                "AmendBenefitApplication",
                &amend_body(
                    &id,
                    &revision,
                    &format!("tok-supplied-{index}"),
                    amendments.clone(),
                ),
            )
            .await;

        let after = harness.get(case, &id).await;
        for (path, value) in expected {
            assert_eq!(
                at(&after, path),
                value,
                "case {case:?}: expected {path} to be {value:?}, got {after:?}"
            );
        }
        assert_ne!(
            text(case, &after, "Revision"),
            revision,
            "case {case:?}: an amendment that changed the record must move the revision, \
             got {after:?}"
        );
        assert_eq!(
            after["Status"],
            json!(STATUS_IN_REVIEW),
            "case {case:?}: an amendment must not move the status, got {after:?}"
        );
        for untouched in ["Id", "Arn", "Catalog", "BenefitId", "CreatedAt"] {
            assert_eq!(
                after[untouched], before[untouched],
                "case {case:?}: an amendment must not change {untouched}, got {after:?}"
            );
        }
        // Who uploaded a stored file and when belongs to the FILE, so an entry whose
        // `FileURI` this amendment left keeps the uploader and upload time recorded for it
        // exactly as they were, while an entry the amendment pointed at another file
        // carries that other file's upload facts: this caller, at the instant this
        // amendment wrote the record. Every write here is signed by the one caller, so
        // what the second case reports is pinned exactly — against a DIFFERENT calling
        // principal — by `an_amendment_records_the_upload_facts_of_the_file_it_attached`
        // below.
        for file in ["0", "1"] {
            let uri = format!("FileDetails.{file}.FileURI");
            let uploader = format!("FileDetails.{file}.CreatedBy");
            let uploaded = format!("FileDetails.{file}.CreatedAt");
            assert_eq!(
                at(&after, &uploader),
                at(&before, &uploader),
                "case {case:?}: every attachment of this record was uploaded by the one \
                 calling principal, so {uploader} must not change, got {after:?}"
            );
            if at(&after, &uri) == at(&before, &uri) {
                assert_eq!(
                    at(&after, &uploaded),
                    at(&before, &uploaded),
                    "case {case:?}: the file at position {file} stayed attached, so \
                     {uploaded} must not change, got {after:?}"
                );
            } else {
                let element = format!("FileDetails.{file}");
                let now = instant(case, at(&after, &element), "CreatedAt");
                let replaced = instant(case, at(&before, &element), "CreatedAt");
                let wrote = instant(case, &after, "UpdatedAt");
                assert!(
                    (replaced..=wrote).contains(&now),
                    "case {case:?}: position {file} was pointed at a file the application \
                     did not carry, so {uploaded} is the instant this amendment recorded, \
                     between {replaced} and {wrote}, got {now} in {after:?}"
                );
            }
        }
        before = after;
    }
}

/// An amendment naming the value a caller-supplied member already holds is accepted and
/// leaves the record — and so the revision — exactly as it was, the rule every write of
/// this service applies.
#[tokio::test]
async fn an_amendment_writing_the_stored_value_moves_nothing() {
    let harness = Harness::new();
    let id = submitted(&harness, "seed", "tok-noop-seed").await;
    let before = harness.get("seed", &id).await;

    let rows: Vec<(&str, Value)> = vec![
        (
            "the contact email it already holds",
            json!([amendment("PartnerContacts[0].Email", "ada@example.com")]),
        ),
        (
            "the contact first name it already holds",
            json!([amendment("PartnerContacts.0.FirstName", "Ada")]),
        ),
        (
            "the attachment URI it already holds",
            json!([amendment("FileDetails[0].FileURI", "s3://bucket/first.csv")]),
        ),
        (
            "the attachment use case it already holds",
            json!([amendment(
                "FileDetails[0].BusinessUseCase",
                "the first attachment"
            )]),
        ),
    ];

    for (index, (case, amendments)) in rows.iter().enumerate() {
        let revision = text(case, &before, "Revision");
        harness
            .ok(
                case,
                "AmendBenefitApplication",
                &amend_body(
                    &id,
                    &revision,
                    &format!("tok-noop-{index}"),
                    amendments.clone(),
                ),
            )
            .await;
        let after = harness.get(case, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: an amendment writing the stored value must change nothing, \
             got {after:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Refusals
// ---------------------------------------------------------------------------

/// A `FieldPath` naming a member of a contact or an attachment that the SERVICE owns, a
/// path that does not address one element of a list member, or a `NewValue` the model does
/// not let that member hold, is refused 400 `ValidationException` — and the stored
/// application is left exactly as it was.
#[tokio::test]
async fn amend_refuses_a_service_owned_member_a_bad_element_path_or_a_value_the_model_refuses() {
    let harness = Harness::new();
    let id = submitted(&harness, "seed", "tok-refuse-seed").await;
    let before = harness.get("seed", &id).await;
    let revision = text("seed", &before, "Revision");

    // The members of a stored attachment a caller supplies, and the members of a contact,
    // as the refusals name them.
    let file_members = ["FileURI", "BusinessUseCase"];
    let contact_members = ["Email", "FirstName", "LastName", "BusinessTitle", "Phone"];

    let rows: Vec<(&str, Value, String)> = vec![
        (
            "the file name derived from the URI",
            json!([amendment("FileDetails[0].FileName", "renamed.csv")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_member_requirement("FileDetails", &file_members),
            ),
        ),
        (
            "the file type derived from the URI",
            json!([amendment("FileDetails[0].FileType", "text/csv")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_member_requirement("FileDetails", &file_members),
            ),
        ),
        (
            "who attached the file",
            json!([amendment("FileDetails.0.CreatedBy", "someone-else")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_member_requirement("FileDetails", &file_members),
            ),
        ),
        (
            "when the file was attached",
            json!([amendment("FileDetails[0].CreatedAt", "2020-01-01T00:00:00Z")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_member_requirement("FileDetails", &file_members),
            ),
        ),
        (
            "the status of the file",
            json!([amendment("FileDetails[0].FileStatus", "APPROVED")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_member_requirement("FileDetails", &file_members),
            ),
        ),
        (
            "a member no contact has",
            json!([amendment("PartnerContacts[0].Nickname", "Amazing Grace")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_member_requirement("PartnerContacts", &contact_members),
            ),
        ),
        (
            "the contacts list itself, with no element named",
            json!([amendment("PartnerContacts", "grace@example.com")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_path_requirement("PartnerContacts"),
            ),
        ),
        (
            "the files list itself, with no element named",
            json!([amendment("FileDetails", "s3://bucket/loose.csv")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_path_requirement("FileDetails"),
            ),
        ),
        (
            "a position that is not a number",
            json!([amendment("PartnerContacts[first].Email", "grace@example.com")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_path_requirement("PartnerContacts"),
            ),
        ),
        (
            "a position written with a leading zero",
            json!([amendment("PartnerContacts[00].Email", "grace@example.com")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_path_requirement("PartnerContacts"),
            ),
        ),
        (
            "a signed position",
            json!([amendment("FileDetails[+1].FileURI", "s3://bucket/signed.csv")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_path_requirement("FileDetails"),
            ),
        ),
        (
            "an unclosed subscript",
            json!([amendment("PartnerContacts[0.Email", "grace@example.com")]),
            clause("amendments.1.member.fieldPath", UNAMENDABLE),
        ),
        (
            // A subscript is an element position, so it is read on a LIST member's name
            // and nowhere else: on any other field name the brackets are part of the
            // name, which names no amendable field.
            "a subscript on a field that is not a list",
            json!([amendment("BenefitApplicationDetails[0]", "subscripted")]),
            clause("amendments.1.member.fieldPath", UNAMENDABLE),
        ),
        (
            "a subscript on a string field",
            json!([amendment("Name[0]", "subscripted")]),
            clause("amendments.1.member.fieldPath", UNAMENDABLE),
        ),
        (
            "a path below a member of a contact",
            json!([amendment(
                "PartnerContacts[0].Email.Domain",
                "example.com"
            )]),
            clause(
                "amendments.1.member.fieldPath",
                &element_path_requirement("PartnerContacts"),
            ),
        ),
        (
            "an element the contacts do not hold",
            json!([amendment("PartnerContacts[1].Email", "grace@example.com")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_count_requirement("PartnerContacts", 1),
            ),
        ),
        (
            "an element the files do not hold",
            json!([amendment("FileDetails[7].FileURI", "s3://bucket/eighth.csv")]),
            clause(
                "amendments.1.member.fieldPath",
                &element_count_requirement("FileDetails", 2),
            ),
        ),
        (
            "an email the modelled pattern refuses",
            json!([amendment("PartnerContacts[0].Email", "not-an-email")]),
            clause(
                "amendments.1.member.newValue",
                &format!("Member must satisfy regular expression pattern: {EMAIL_PATTERN}"),
            ),
        ),
        (
            "an email one character past the published bound",
            json!([amendment(
                "PartnerContacts[0].Email",
                &email_of_length(CONTACT_MAX_LEN + 1)
            )]),
            clause(
                "amendments.1.member.newValue",
                &format!("Member must have length less than or equal to {CONTACT_MAX_LEN}"),
            ),
        ),
        (
            "an attachment URI the modelled pattern refuses",
            json!([amendment("FileDetails[0].FileURI", "ftp://bucket/first.csv")]),
            clause(
                "amendments.1.member.newValue",
                &format!("Member must satisfy regular expression pattern: {FILE_URI_PATTERN}"),
            ),
        ),
        (
            "an attachment URI one character past the published bound",
            json!([amendment(
                "FileDetails[0].FileURI",
                &file_uri_of_length(FILE_URI_MAX_LEN + 1)
            )]),
            clause(
                "amendments.1.member.newValue",
                &format!("Member must have length less than or equal to {FILE_URI_MAX_LEN}"),
            ),
        ),
        (
            "a phone one character past this service's bound",
            json!([amendment(
                "PartnerContacts[0].Phone",
                &"5".repeat(MAX_DETAILS_VALUE_LEN + 1)
            )]),
            clause(
                "amendments.1.member.newValue",
                &format!("Member must have length less than or equal to {MAX_DETAILS_VALUE_LEN}"),
            ),
        ),
        (
            "a business use case one character past this service's bound",
            json!([amendment(
                "FileDetails[0].BusinessUseCase",
                &"u".repeat(MAX_DETAILS_VALUE_LEN + 1)
            )]),
            clause(
                "amendments.1.member.newValue",
                &format!("Member must have length less than or equal to {MAX_DETAILS_VALUE_LEN}"),
            ),
        ),
        (
            "one applicable amendment and one that is not",
            json!([
                amendment("PartnerContacts[0].FirstName", "would-apply"),
                amendment("FileDetails[0].CreatedBy", "would-not"),
            ]),
            // The refusal names the SECOND element of the list, 1-based.
            clause(
                "amendments.2.member.fieldPath",
                &element_member_requirement("FileDetails", &file_members),
            ),
        ),
    ];

    for (index, (case, amendments, expected)) in rows.iter().enumerate() {
        let (status, out) = harness
            .call(
                "AmendBenefitApplication",
                &amend_body(
                    &id,
                    &revision,
                    &format!("tok-refuse-{index}"),
                    amendments.clone(),
                ),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {out:?}"
        );
        assert_eq!(
            error_code(&out),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {out:?}"
        );
        let message = message_of(&out);
        assert_eq!(
            &message, expected,
            "case {case:?}: expected the refusal to state {expected:?}, got {out:?}"
        );
        // A refusal states the requirement, never the value it rejected.
        assert!(
            !message.contains("would-apply") && !message.contains("not-an-email"),
            "case {case:?}: a refusal must not echo the rejected value, got {message:?}"
        );
        let after = harness.get(case, &id).await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused amendment must change nothing, got {after:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The hole the rule closes
// ---------------------------------------------------------------------------

/// Once the application is submitted `UpdateBenefitApplication` is refused on status, and
/// the amendment is what changes the members that update would have written — so a
/// submitted application's contacts and attachments are reachable through this operation.
#[tokio::test]
async fn a_submitted_applications_contacts_and_attachments_are_amendable() {
    let harness = Harness::new();
    let id = submitted(&harness, "seed", "tok-hole-seed").await;
    let before = harness.get("seed", &id).await;
    let revision = text("seed", &before, "Revision");

    // The update is refused: it rewrites the partner's own draft, and this one is in
    // review.
    let (status, out) = harness
        .call(
            "UpdateBenefitApplication",
            &json!({
                "Catalog": CATALOG,
                "ClientToken": "tok-hole-update",
                "Identifier": id,
                "Revision": revision,
                "PartnerContacts": [{ "Email": "grace@example.com" }],
                "FileDetails": [{ "FileURI": "s3://bucket/updated.csv" }],
            }),
        )
        .await;
    assert_eq!(
        status, 409,
        "case \"the update of a submitted application\": expected 409, got {status} {out:?}"
    );
    assert_eq!(
        error_code(&out),
        "ConflictException",
        "case \"the update of a submitted application\": expected ConflictException, got {out:?}"
    );

    // The amendment reaches both of those members on the same record.
    harness
        .ok(
            "the amendment of a submitted application",
            "AmendBenefitApplication",
            &amend_body(
                &id,
                &revision,
                "tok-hole-amend",
                json!([
                    amendment("PartnerContacts[0].Email", "grace@example.com"),
                    amendment("FileDetails[0].FileURI", "s3://bucket/updated.csv"),
                ]),
            ),
        )
        .await;

    let after = harness.get("the amendment of a submitted application", &id).await;
    for (path, expected) in [
        ("PartnerContacts.0.Email", json!("grace@example.com")),
        ("FileDetails.0.FileURI", json!("s3://bucket/updated.csv")),
    ] {
        assert_eq!(
            at(&after, path),
            &expected,
            "case \"the amendment of a submitted application\": expected {path} to be \
             {expected:?}, got {after:?}"
        );
    }
    assert_eq!(
        after["Status"],
        json!(STATUS_IN_REVIEW),
        "case \"the amendment of a submitted application\": the status must not move, \
         got {after:?}"
    );
}

// ---------------------------------------------------------------------------
// The upload facts of an attachment an amendment pointed at another file
// ---------------------------------------------------------------------------

/// The access key id the amendments in the two tests below are signed with: a caller other
/// than the one that created the application, so an upload fact an amendment recorded is
/// observably different from the one the create recorded.
const AMENDER_KEY: &str = "AKIDAMENDER";

/// One attachment as a request names it: its `FileURI` and the `BusinessUseCase` beside it.
type SuppliedFile = (&'static str, &'static str);

/// The attachment `plan.pdf`.
const PLAN: SuppliedFile = ("s3://partner-bucket/proposals/plan.pdf", "funding request");

/// A second attachment, of the same file type.
const EXTRA: SuppliedFile = (
    "s3://partner-bucket/proposals/extra.pdf",
    "supporting detail",
);

/// A third attachment, of another file type.
const NOTES: SuppliedFile = ("s3://partner-bucket/proposals/notes.docx", "background");

/// A file no seed below attaches, so an amendment naming it points an attachment at a file
/// the application does not already carry.
const FRESH: SuppliedFile = ("s3://partner-bucket/proposals/budget.xlsx", "budget");

/// The `FileDetails` list a request carries for `files`, as the modelled `FileInput`
/// members.
fn file_inputs(files: &[SuppliedFile]) -> Value {
    Value::Array(
        files
            .iter()
            .map(|(uri, use_case)| json!({ "FileURI": uri, "BusinessUseCase": use_case }))
            .collect(),
    )
}

/// The last path segment of a `FileURI`, which is the `FileName` the service derives from
/// it.
fn last_segment(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or(uri)
}

/// The `FileDetails` one read reports, in the order the service reported them.
fn stored_files(case: &str, application: &Value) -> Vec<Value> {
    match &application["FileDetails"] {
        Value::Array(files) => files.clone(),
        Value::Null => Vec::new(),
        other => panic!("case {case:?}: expected a FileDetails list, got {other:?}"),
    }
}

/// Call one operation signed by `access_key_id`, asserting it answered 200.
async fn ok_as(
    harness: &Harness,
    case: &str,
    access_key_id: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, out) = harness.call_as(access_key_id, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected {operation} to answer 200, got {status} {out:?}"
    );
    out
}

/// Create one draft application carrying `files`, signed by `access_key_id`, and return
/// its id.
async fn seed_files(
    harness: &Harness,
    case: &str,
    access_key_id: &str,
    token: &str,
    files: &[SuppliedFile],
) -> String {
    let benefit_id = text(case, &harness.active_benefit().await, "Id");
    let created = ok_as(
        harness,
        case,
        access_key_id,
        "CreateBenefitApplication",
        &json!({
            "Catalog": CATALOG,
            "ClientToken": token,
            "BenefitIdentifier": benefit_id,
            "Name": "amended-upload-facts-subject",
            "FileDetails": file_inputs(files),
        }),
    )
    .await;
    text(case, &created, "Id")
}

/// The principal this service reports for the caller signing as `access_key_id`, observed
/// through a file that caller attached itself rather than restated here.
async fn principal_of(harness: &Harness, access_key_id: &str, token: &str) -> String {
    let case = format!("the principal of {access_key_id}");
    let id = seed_files(harness, &case, access_key_id, token, &[PLAN]).await;
    let application = ok_as(
        harness,
        &case,
        access_key_id,
        "GetBenefitApplication",
        &json!({ "Catalog": CATALOG, "Identifier": id }),
    )
    .await;
    text(&case, &stored_files(&case, &application)[0], "CreatedBy")
}

/// What one position of the amended list must report: its `FileURI`, and the attachment
/// whose recorded upload facts it carries — `Some(uri)` for a file the application already
/// carried under that URI, whose principal and instant stay exactly as they were, and
/// `None` for a file it did not carry, which this amendment uploaded.
type ExpectedFile = (&'static str, Option<&'static str>);

/// An amendment that points an attachment at a file the application does not already carry
/// records the amending caller and this call's instant on that entry, while every entry
/// naming a file the application already carries keeps the uploader and upload time
/// recorded for it.
///
/// Each row runs on its own freshly created application, created by one caller and amended
/// by another, so a rewritten `CreatedBy` is visible as well as a rewritten `CreatedAt`.
#[tokio::test]
async fn an_amendment_records_the_upload_facts_of_the_file_it_attached() {
    let harness = Harness::new();
    let creator = principal_of(&harness, ACCESS_KEY_ID, "tok-amend-facts-creator").await;
    let amender = principal_of(&harness, AMENDER_KEY, "tok-amend-facts-amender").await;
    assert_ne!(
        creator, amender,
        "the two callers must report different principals, or the rule is untestable"
    );

    /// One row: the case, the files the create attaches, the amended `FieldPath`, its
    /// `NewValue`, and what every position of the amended list must then report.
    type Row = (
        &'static str,
        Vec<SuppliedFile>,
        &'static str,
        &'static str,
        Vec<ExpectedFile>,
    );
    let rows: Vec<Row> = vec![
        (
            "the only attachment pointed at a file the application does not carry",
            vec![PLAN],
            "FileDetails.0.FileURI",
            FRESH.0,
            vec![(FRESH.0, None)],
        ),
        (
            "the same replacement with the position written as a subscript",
            vec![PLAN],
            "FileDetails[0].FileURI",
            FRESH.0,
            vec![(FRESH.0, None)],
        ),
        (
            "the second of two attachments replaced, in JSON-path form",
            vec![PLAN, EXTRA],
            "$.FileDetails[1].FileURI",
            FRESH.0,
            vec![(PLAN.0, Some(PLAN.0)), (FRESH.0, None)],
        ),
        (
            "the middle of three attachments replaced",
            vec![PLAN, EXTRA, NOTES],
            "FileDetails.1.FileURI",
            FRESH.0,
            vec![
                (PLAN.0, Some(PLAN.0)),
                (FRESH.0, None),
                (NOTES.0, Some(NOTES.0)),
            ],
        ),
        (
            "an attachment pointed at the file another position already carries",
            vec![PLAN, EXTRA],
            "FileDetails.0.FileURI",
            EXTRA.0,
            // The application carries that file once, so the first entry naming it keeps
            // its upload facts and the second records this call: the rule follows the
            // FILE, not the position.
            vec![(EXTRA.0, Some(EXTRA.0)), (EXTRA.0, None)],
        ),
        (
            "the attachment pointed at the file it already carries",
            vec![PLAN],
            "FileDetails.0.FileURI",
            PLAN.0,
            vec![(PLAN.0, Some(PLAN.0))],
        ),
        (
            "an attachment's business use case rewritten",
            vec![PLAN, EXTRA],
            "FileDetails.1.BusinessUseCase",
            "a corrected use case",
            vec![(PLAN.0, Some(PLAN.0)), (EXTRA.0, Some(EXTRA.0))],
        ),
        (
            "a member of the application outside the attachment list",
            vec![PLAN, EXTRA],
            "Name",
            "amended-name",
            vec![(PLAN.0, Some(PLAN.0)), (EXTRA.0, Some(EXTRA.0))],
        ),
    ];

    for (row, (case, seeded, field_path, new_value, expected)) in rows.iter().enumerate() {
        let id = seed_files(
            &harness,
            case,
            ACCESS_KEY_ID,
            &format!("tok-amend-facts-seed-{row}"),
            seeded,
        )
        .await;
        let before = harness.get(case, &id).await;
        let attached = stored_files(case, &before);
        // The instant the application came into existence, which no later write moves, so
        // it is the earliest instant any upload of this record can carry.
        let seeded_at = instant(case, &before, "CreatedAt");

        ok_as(
            &harness,
            case,
            AMENDER_KEY,
            "AmendBenefitApplication",
            &amend_body(
                &id,
                &text(case, &before, "Revision"),
                &format!("tok-amend-facts-{row}"),
                json!([amendment(field_path, new_value)]),
            ),
        )
        .await;

        let after = harness.get(case, &id).await;
        let after_updated = instant(case, &after, "UpdatedAt");
        let stored = stored_files(case, &after);
        assert_eq!(
            stored.len(),
            expected.len(),
            "case {case:?}: expected {} attachments after the amendment, got {stored:?}",
            expected.len()
        );

        for (index, (uri, carried)) in expected.iter().enumerate() {
            let file = &stored[index];
            assert_eq!(
                file["FileURI"],
                json!(uri),
                "case {case:?}: expected {uri:?} at position {index}, got {file:?}"
            );
            assert_eq!(
                file["FileName"],
                json!(last_segment(uri)),
                "case {case:?}: expected the file name {:?} at position {index}, got {file:?}",
                last_segment(uri)
            );

            let created_by = text(case, file, "CreatedBy");
            let created_at = instant(case, file, "CreatedAt");
            match carried {
                Some(source) => {
                    // The application already carried this file, so its upload facts are
                    // the ones the create recorded: the creating principal, and the very
                    // instant the read before the amendment reported for that file.
                    let was = attached
                        .iter()
                        .find(|stored| stored["FileURI"] == json!(source))
                        .unwrap_or_else(|| {
                            panic!(
                                "case {case:?}: {source:?} must have been attached, got \
                                 {attached:?}"
                            )
                        });
                    let uploaded_at = instant(case, was, "CreatedAt");
                    assert_eq!(
                        created_by, creator,
                        "case {case:?}: position {index} must keep the uploading principal \
                         {creator:?}, got {created_by:?} in {file:?}"
                    );
                    assert_eq!(
                        created_at, uploaded_at,
                        "case {case:?}: position {index} must keep the upload instant \
                         {uploaded_at}, got {created_at} in {file:?}"
                    );
                }
                None => {
                    // The application did not carry this file, so this amendment uploaded
                    // it: the amending principal, at the instant this call wrote the
                    // record — inside the window the record's own `UpdatedAt` marks out.
                    assert_eq!(
                        created_by, amender,
                        "case {case:?}: position {index} must record the uploading principal \
                         {amender:?}, got {created_by:?} in {file:?}"
                    );
                    assert!(
                        (seeded_at..=after_updated).contains(&created_at),
                        "case {case:?}: position {index} must record an upload instant this \
                         amendment made, between {seeded_at} and {after_updated}, got \
                         {created_at} in {file:?}"
                    );
                }
            }
        }
    }
}

/// A chain of amendments, each by a different caller, never rewrites the upload facts of a
/// file that stays attached: the entry for the file the create attached reports the same
/// `CreatedBy` and `CreatedAt` after every one of them, while the entry beside it is
/// pointed at one file after another and each of those reports the caller that attached it.
#[tokio::test]
async fn repeated_amendments_leave_an_attached_files_upload_facts_alone() {
    let harness = Harness::new();
    let case = "a file carried through a chain of amendments";
    let id = seed_files(
        &harness,
        case,
        ACCESS_KEY_ID,
        "tok-amend-chain-seed",
        &[PLAN, EXTRA],
    )
    .await;
    let first = harness.get(case, &id).await;
    let attached = stored_files(case, &first);
    let uploaded_by = text(case, &attached[0], "CreatedBy");
    let uploaded_at = instant(case, &attached[0], "CreatedAt");

    // (the caller signing the amendment, the file it points the second attachment at)
    let writes: [(&str, SuppliedFile); 3] = [
        ("AKIDAMENDERONE", NOTES),
        ("AKIDAMENDERTWO", FRESH),
        ("AKIDAMENDERTHREE", EXTRA),
    ];

    for (write, (caller, (uri, _))) in writes.iter().enumerate() {
        let before = harness.get(case, &id).await;
        ok_as(
            &harness,
            case,
            caller,
            "AmendBenefitApplication",
            &amend_body(
                &id,
                &text(case, &before, "Revision"),
                &format!("tok-amend-chain-{write}"),
                json!([amendment("FileDetails.1.FileURI", uri)]),
            ),
        )
        .await;

        let after = harness.get(case, &id).await;
        let after_updated = instant(case, &after, "UpdatedAt");
        let stored = stored_files(case, &after);
        assert_eq!(
            stored.len(),
            2,
            "case {case:?}: amendment {write} must leave two attachments, got {stored:?}"
        );

        let plan = &stored[0];
        assert_eq!(
            text(case, plan, "CreatedBy"),
            uploaded_by,
            "case {case:?}: amendment {write} must keep the uploading principal \
             {uploaded_by:?}, got {plan:?}"
        );
        assert_eq!(
            instant(case, plan, "CreatedAt"),
            uploaded_at,
            "case {case:?}: amendment {write} must keep the upload instant {uploaded_at}, \
             got {plan:?}"
        );

        let attached_now = &stored[1];
        assert_eq!(
            attached_now["FileURI"],
            json!(uri),
            "case {case:?}: amendment {write} must attach {uri:?}, got {attached_now:?}"
        );
        assert_ne!(
            text(case, attached_now, "CreatedBy"),
            uploaded_by,
            "case {case:?}: amendment {write} attached a file the application did not carry, \
             so that entry must not report the creating principal {uploaded_by:?}, got \
             {attached_now:?}"
        );
        let created_at = instant(case, attached_now, "CreatedAt");
        assert!(
            (uploaded_at..=after_updated).contains(&created_at),
            "case {case:?}: amendment {write} must record an upload instant it made, between \
             {uploaded_at} and {after_updated}, got {created_at} in {attached_now:?}"
        );
    }
}
