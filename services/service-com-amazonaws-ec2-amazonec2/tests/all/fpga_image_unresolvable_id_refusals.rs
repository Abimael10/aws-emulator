// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for what each operation that takes an Amazon FPGA Image id answers when
//! that id resolves to NO AFI, driven over the wire the service actually speaks — `ec2Query`,
//! through the crate's shared [`super::ec2_query`] harness.
//!
//! The rules under test:
//!
//! 1. A value OUTSIDE the form EC2 issues an AFI id in is refused by every one of those
//!    operations the same way — 400 `InvalidFpgaImageID.Malformed` with the FPGA-image id
//!    kind's `Image ID '<id>' is invalid.`, naming the value the REQUEST sent — before
//!    anything is looked up, and nothing is written by the refusal.
//! 2. A value in the PRINTED id form that this service never ISSUED is answered by the
//!    OPERATION's own refusal, because EC2 words a refusal per operation rather than per id
//!    kind:
//!    * `DescribeFpgaImages` makes the SAME answer as for a value that is no AFI id at all:
//!      it reports on "the AFIs available to you", so it judges the id it was given rather
//!      than resolving one resource of the account's own Region;
//!    * `CopyFpgaImage` answers `InvalidFpgaImageID.NotFound` with `Image ID '<id>' not
//!      found.`: its source is named by a REGION and an id together, so the id is read for
//!      its printed FORM and then resolved, and a value in that form it cannot resolve is a
//!      source that is missing rather than one it cannot read;
//!    * `DeleteFpgaImage` and `ResetFpgaImageAttribute`, which ACT on ONE named AFI OF THEIR
//!      OWN REGION, answer the unreadable-id refusal instead: EC2's long id form is an
//!      obfuscated identifier the service decodes before the lookup, so a value no issue of
//!      this kind ever produced is one they cannot read — their `InvalidFpgaImageID.NotFound`
//!      with `Image ID '<id>' not found.` is reserved for an identifier this service DID
//!      issue, which rule 3 drives;
//!    * `DescribeFpgaImageAttribute` answers the not-found code with EC2's common missing-id
//!      sentence instead, naming the `fpga-image` resource type.
//! 3. The two readings are about the ID and not about the operation's subject: the id of a
//!    STORED AFI resolves through every one of them, and the same id draws that operation's
//!    own unresolved answer again once a delete has removed the AFI.
//!
//! Each table row is a distinct input along what the rules generalise over — the WAY a value
//! falls outside the issued form (the prefix alone, no prefix, another kind's prefix, a
//! suffix of the wrong length or alphabet, the null id), which of the two published id FORMS
//! (8 and 17 hexadecimal digits) an unresolved id is written in, and which OPERATION resolves
//! it — so no row is the specification. The expected codes and sentences are written out
//! here, independently of the service, so the suite pins the refusal and not the code that
//! composes it.

use ec2::state::{
    ComAmazonawsEc2Amazonec2State as Ec2State, INVALID_FPGA_IMAGE_ID_MALFORMED,
    INVALID_FPGA_IMAGE_ID_NOT_FOUND,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{emulator, error_code, error_message, indexed};
use super::fpga_image_tasks_and_volume_io::seed_fpga_image;
use super::volume_lifecycle::{p, REGION};

/// One request's form parameters, as the ec2Query harness takes them.
type Params = Vec<(String, String)>;

/// The HTTP status EC2 answers a refusal about the request with.
const REFUSAL_STATUS: u16 = 400;

/// How ONE operation answers an id inside the issued form that resolves to no AFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unresolved {
    /// The answer the operations that JUDGE the id they were given make, which is the one
    /// they answer a value that is no AFI id at all with as well.
    Unreadable,
    /// `Image ID '<id>' not found.` — the sentence the operations that act on one named AFI
    /// are recorded answering.
    NotFoundImageId,
    /// EC2's common missing-id sentence, naming the `fpga-image` resource type — the
    /// sentence the attribute describe is recorded answering.
    NotFoundResourceType,
}

impl Unresolved {
    /// The error code this answer carries.
    fn code(self) -> &'static str {
        match self {
            Self::Unreadable => INVALID_FPGA_IMAGE_ID_MALFORMED,
            Self::NotFoundImageId | Self::NotFoundResourceType => INVALID_FPGA_IMAGE_ID_NOT_FOUND,
        }
    }

    /// The sentence this answer carries for `fpga_image_id`.
    fn message(self, fpga_image_id: &str) -> String {
        match self {
            Self::Unreadable => unreadable_message(fpga_image_id),
            Self::NotFoundImageId => format!("Image ID '{fpga_image_id}' not found."),
            Self::NotFoundResourceType => {
                format!("The fpga-image ID '{fpga_image_id}' does not exist")
            }
        }
    }
}

/// The sentence every operation answers a value that is no AFI id at all with.
fn unreadable_message(fpga_image_id: &str) -> String {
    format!("Image ID '{fpga_image_id}' is invalid.")
}

/// Every operation that resolves an AFI id, with the answer it makes for a value in the
/// printed id form that this service never ISSUED.
const RESOLVERS: [(&str, Unresolved); 5] = [
    ("CopyFpgaImage", Unresolved::NotFoundImageId),
    ("DescribeFpgaImages", Unresolved::Unreadable),
    ("DeleteFpgaImage", Unresolved::Unreadable),
    ("ResetFpgaImageAttribute", Unresolved::Unreadable),
    ("DescribeFpgaImageAttribute", Unresolved::NotFoundResourceType),
];

/// The same table for an identifier this service DID issue that names no stored AFI — the id
/// of an AFI a delete has removed: the operations that resolve one named AFI, the copy
/// included, word that cause `Image ID '<id>' not found.`.
const RESOLVERS_OF_AN_ISSUED_ID: [(&str, Unresolved); 5] = [
    ("CopyFpgaImage", Unresolved::NotFoundImageId),
    ("DescribeFpgaImages", Unresolved::Unreadable),
    ("DeleteFpgaImage", Unresolved::NotFoundImageId),
    ("ResetFpgaImageAttribute", Unresolved::NotFoundImageId),
    ("DescribeFpgaImageAttribute", Unresolved::NotFoundResourceType),
];

/// The parameters `action` names `fpga_image_id` through: the member each operation takes an
/// AFI id in, plus whatever else that operation requires.
fn resolver_params(action: &str, fpga_image_id: &str) -> Params {
    match action {
        "CopyFpgaImage" => vec![
            p("SourceFpgaImageId", fpga_image_id),
            p("SourceRegion", REGION),
        ],
        "DescribeFpgaImages" => indexed("FpgaImageId", &[fpga_image_id]),
        "ResetFpgaImageAttribute" | "DescribeFpgaImageAttribute" => vec![
            p("FpgaImageId", fpga_image_id),
            p("Attribute", "loadPermission"),
        ],
        _ => vec![p("FpgaImageId", fpga_image_id)],
    }
}

/// One `action` call signed for the Region the suite works in.
async fn op(router: &MultiServiceRouter, action: &str, params: &Params) -> (u16, String) {
    super::ec2_query::call(router, REGION, action, params).await
}

/// How many AFIs the account holds right now.
fn stored_afi_count(engine: &engine::InMemoryEngine) -> usize {
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    guard.fpga_images.len()
}

/// `(case, the value)`: every row is OUTSIDE the form EC2 issues an AFI id in, each for a
/// different reason.
fn unreadable_rows() -> Vec<(&'static str, String)> {
    vec![
        ("the prefix alone", "afi-".to_string()),
        ("no prefix at all", "0123456789abcdef0".to_string()),
        (
            "another resource kind's prefix",
            "ami-0123456789abcdef0".to_string(),
        ),
        (
            "a suffix one digit short of the long form",
            "afi-0123456789abcdef".to_string(),
        ),
        (
            "a suffix outside the hexadecimal alphabet",
            "afi-0123456789abcdefg".to_string(),
        ),
        ("an upper-case suffix", "afi-0123456789ABCDEF0".to_string()),
        (
            // The placeholder every client reaches for: inside the long form's length and
            // alphabet, outside the ids the mint can produce.
            "the null id in the long form",
            format!("afi-{}", "0".repeat(17)),
        ),
    ]
}

/// `(case, the value)`: every row is in the PRINTED form EC2 issues an AFI id in, and this
/// service never issued an AFI id as any of them.
fn unresolved_rows() -> Vec<(&'static str, String)> {
    vec![
        (
            "the long id form, which nothing was ever issued under",
            "afi-0123456789abcdef0".to_string(),
        ),
        (
            "the short id form, which nothing was ever issued under",
            "afi-01234567".to_string(),
        ),
        (
            // A leading digit above zero puts an INSTANCE id outside the form EC2 issues;
            // that clause is the instance id's own, so an AFI id carrying one is inside this
            // kind's form.
            "a long-form id whose leading digit is above zero",
            "afi-1234567890abcdef0".to_string(),
        ),
    ]
}

/// RULE 1: a value outside the issued form is refused 400 `InvalidFpgaImageID.Malformed`
/// with the id kind's unreadable sentence, by every operation that resolves an AFI id, and
/// the refusal writes nothing.
#[tokio::test]
async fn a_value_outside_the_issued_afi_id_form_is_refused_the_same_way_everywhere() {
    let (router, engine) = emulator();

    // A stored AFI, so a refusal has something it could have removed or changed had the id
    // been read as a lookup instead of as a form.
    let (stored_id, _) = seed_fpga_image(&engine, "unreadable-id-form");
    let stored_before = stored_afi_count(&engine);

    for (case, fpga_image_id) in unreadable_rows() {
        let expected_message = unreadable_message(&fpga_image_id);
        for (action, _) in RESOLVERS {
            let params = resolver_params(action, &fpga_image_id);
            let (status, body) = op(&router, action, &params).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {case:?} on {action}: expected HTTP {REFUSAL_STATUS}, got {status} with \
                 body {body}"
            );
            let answered_code = error_code(&body);
            assert_eq!(
                answered_code, INVALID_FPGA_IMAGE_ID_MALFORMED,
                "case {case:?} on {action}: expected the code \
                 {INVALID_FPGA_IMAGE_ID_MALFORMED:?}, got {answered_code:?} in {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected_message,
                "case {case:?} on {action}: expected the message {expected_message:?}, got \
                 {message:?}"
            );
        }
    }

    assert_eq!(
        stored_afi_count(&engine),
        stored_before,
        "a refused call writes nothing: expected {stored_before} stored, got {}",
        stored_afi_count(&engine)
    );
    let store = engine.state::<Ec2State>();
    let guard = store.lock();
    assert!(
        guard.fpga_images.contains_key(&stored_id),
        "the stored AFI {stored_id:?} survives every refusal, got {:?}",
        guard.fpga_images.keys().collect::<Vec<_>>()
    );
}

/// RULE 2: a value in the printed id form that this service never ISSUED is answered by the
/// OPERATION's own refusal — the unreadable-id refusal from the ones that judge or decode the
/// id they were given, the not-found code with its own sentence from the copy, which resolves
/// the value it read, and from the attribute describe.
#[tokio::test]
async fn an_unresolved_afi_id_is_answered_by_each_operations_own_refusal() {
    let (router, engine) = emulator();

    // A stored AFI the account DOES hold, so every row below is unresolved because of the id
    // it names rather than because the store is empty.
    let (stored_id, _) = seed_fpga_image(&engine, "unresolved-id");

    for (case, fpga_image_id) in unresolved_rows() {
        assert_ne!(
            fpga_image_id, stored_id,
            "case {case:?}: the row must name an id the account holds no AFI under, got the \
             stored {stored_id:?}"
        );
        for (action, unresolved) in RESOLVERS {
            let params = resolver_params(action, &fpga_image_id);
            let (status, body) = op(&router, action, &params).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {case:?} on {action}: expected HTTP {REFUSAL_STATUS}, got {status} with \
                 body {body}"
            );
            let expected_code = unresolved.code();
            let answered_code = error_code(&body);
            assert_eq!(
                answered_code, expected_code,
                "case {case:?} on {action}: expected the code {expected_code:?}, got \
                 {answered_code:?} in {body}"
            );
            let expected_message = unresolved.message(&fpga_image_id);
            let message = error_message(&body);
            assert_eq!(
                message, expected_message,
                "case {case:?} on {action}: expected the message {expected_message:?}, got \
                 {message:?}"
            );
        }
    }
}

/// RULE 3: the readings are about the ID: a stored AFI's own id resolves through every one of
/// these operations, and once the delete has removed that AFI the same id draws each
/// operation's own unresolved answer.
#[tokio::test]
async fn a_stored_afi_id_resolves_until_the_delete_removes_it() {
    let (router, engine) = emulator();
    let (fpga_image_id, _) = seed_fpga_image(&engine, "resolves-until-deleted");

    // Every reader of a stored AFI answers for it, and so does the copy that names it as its
    // source: the id a mint produces is one all of them resolve.
    for action in [
        "DescribeFpgaImageAttribute",
        "DescribeFpgaImages",
        "CopyFpgaImage",
        "ResetFpgaImageAttribute",
    ] {
        let params = resolver_params(action, &fpga_image_id);
        let (status, body) = op(&router, action, &params).await;
        assert_eq!(
            status, 200,
            "case {fpga_image_id:?} on {action}: expected HTTP 200 for the stored AFI, got \
             {status} with body {body}"
        );
    }

    let params = resolver_params("DeleteFpgaImage", &fpga_image_id);
    let (status, body) = op(&router, "DeleteFpgaImage", &params).await;
    assert_eq!(
        status, 200,
        "case {fpga_image_id:?}: expected HTTP 200 from DeleteFpgaImage, got {status} with \
         body {body}"
    );
    {
        let store = engine.state::<Ec2State>();
        let guard = store.lock();
        assert!(
            !guard.fpga_images.contains_key(&fpga_image_id),
            "the delete takes the AFI out of the store, got {:?}",
            guard.fpga_images.keys().collect::<Vec<_>>()
        );
    }

    // The id is one this service ISSUED, so what changed is the AFI: each operation answers
    // its own refusal for an identifier that named a resource and no longer does, naming the
    // id the request carried.
    for (action, unresolved) in RESOLVERS_OF_AN_ISSUED_ID {
        let params = resolver_params(action, &fpga_image_id);
        let (status, body) = op(&router, action, &params).await;
        assert_eq!(
            status, REFUSAL_STATUS,
            "case {fpga_image_id:?} on {action} after the delete: expected HTTP \
             {REFUSAL_STATUS}, got {status} with body {body}"
        );
        let expected_code = unresolved.code();
        let answered_code = error_code(&body);
        assert_eq!(
            answered_code, expected_code,
            "case {fpga_image_id:?} on {action} after the delete: expected the code \
             {expected_code:?}, got {answered_code:?} in {body}"
        );
        let expected_message = unresolved.message(&fpga_image_id);
        let message = error_message(&body);
        assert_eq!(
            message, expected_message,
            "case {fpga_image_id:?} on {action} after the delete: expected the message \
             {expected_message:?}, got {message:?}"
        );
    }
}
