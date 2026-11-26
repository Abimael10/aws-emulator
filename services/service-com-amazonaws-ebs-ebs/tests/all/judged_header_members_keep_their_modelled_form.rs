// SPDX-License-Identifier: CC0-1.0

//! A header member an operation takes OFF the request boundary keeps the FORM its model
//! declares for it.
//!
//! RULE: a member whose modelled constraint traits `skip_validation` takes off the
//! generated member schema is still judged against those traits — the `@length` and
//! `@pattern` of the shape it targets — through the framework's own constraint walk, and
//! that judgement comes BEFORE the service judges what the value means. So a value outside
//! the member's declared form earns the constraint report (the clause naming the member,
//! the requirement it failed and the value, in the envelope the request boundary would
//! have used), and only a value the model admits reaches the question the service answers
//! for itself — here, which checksum algorithms it supports.
//!
//! Why it matters: `ChecksumAlgorithm` is `@length` 0..32 with `@pattern ^[A-Za-z0-9]+$`
//! (`smithy_query(query="shape",shape="com.amazonaws.ebs#ChecksumAlgorithm")`), and both
//! operations that carry it take it off the boundary so they can answer an unsupported
//! algorithm with the modelled `ValidationException` a client branches on. A member whose
//! traits are then never re-applied reports a malformed value as an unsupported ALGORITHM
//! — telling a caller to pick another algorithm when what it sent is not an algorithm name
//! at all — and words that refusal around a value no declared bound has held.
//!
//! Rows differ along what the rule generalises over: which modelled constraint the value
//! breaks (the pattern, the length maximum, both at once), which character class puts it
//! outside the pattern, how far past the length maximum it is (one character, far past the
//! ceiling a clause echoes), which operation carries the member, and — for the in-form
//! rows — which unsupported algorithm was asked for.
//!
//! A row sends an EMPTY header, which the pattern admits no value of; a request that omits
//! the header altogether is a different class and belongs to the suite that owns it
//! (`crate::absent_required_members`, which asserts the `Member must not be null` report
//! for `checksumAlgorithm`), because the member's `@required` is judged before any
//! constraint is: the guard in `crate::service` refuses the request on the absent header.

use crate::declared_block_inputs::assert_validation;
// The clause, envelope and echo of the framework's constraint report, and the modelled
// forms of the sibling members a row below breaks alongside the algorithm, are the ones
// `response_bodies_and_refusals` already states for the rest of this service's
// handler-judged members: one encoding of that format per test binary, so the two suites
// cannot drift apart on what the report looks like.
use crate::response_bodies_and_refusals::{
    clause, constraint_report, echoed, max_length_requirement, min_value_requirement,
    pattern_requirement, CHANGED_BLOCKS_COUNT_MIN, CHECKSUM_PATTERN, CONSTRAINT_REFUSAL_REASON,
    MAX_ECHOED_VALUE, SUPPORTED_ALGORITHM,
};
use crate::snapshot_block_loop::{
    block_bytes, checksum, complete_snapshot_with_algorithm, member, put_block,
    put_block_with_algorithm, router, start_snapshot, Reply, BLOCK_SIZE,
};

/// The modelled form of `com.amazonaws.ebs#ChecksumAlgorithm`, the shape both
/// `PutSnapshotBlockRequest$ChecksumAlgorithm` and `CompleteSnapshotRequest$ChecksumAlgorithm`
/// target (`smithy_query(query="shape",shape="com.amazonaws.ebs#ChecksumAlgorithm")`:
/// `@length` 0..32, this `@pattern`, and `SHA256` — [`SUPPORTED_ALGORITHM`] — as the one
/// value its enum declares).
const ALGORITHM_PATTERN: &str = "^[A-Za-z0-9]+$";
const ALGORITHM_MAX_LENGTH: usize = 32;

/// The `@length` maximum of `ErrorMessage`, the shape every modelled error's `Message`
/// member targets (`smithy_query(query="shape",shape="com.amazonaws.ebs#ErrorMessage")`
/// reports `@length` 0..256). A Smithy `@length` on a string counts characters, which is
/// how the assertion below measures.
const ERROR_MESSAGE_MAX_LENGTH: usize = 256;

/// The sentence this service words for an algorithm it does not support: the refusal a
/// value INSIDE the member's modelled form earns.
fn unsupported_message(algorithm: &str) -> String {
    format!("Unsupported Checksum Algorithm '{algorithm}', supported algorithms are {SUPPORTED_ALGORITHM}")
}

/// A pending snapshot of one GiB, which takes a write and can be completed.
async fn pending_snapshot(router: &smithy_server_runtime::MultiServiceRouter) -> String {
    let started = start_snapshot(router, &serde_json::json!({ "VolumeSize": 1 })).await;
    assert_eq!(
        started.status, 201,
        "the fixture snapshot must start, got {} with body {}",
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    member(&started, "SnapshotId")
}

/// `PutSnapshotBlock` carrying `algorithm`, with a full block and the checksum of the bytes
/// it sends, so nothing but the algorithm can be refused.
async fn put_with_algorithm(
    router: &smithy_server_runtime::MultiServiceRouter,
    snapshot_id: &str,
    algorithm: &str,
) -> Reply {
    let data = block_bytes(17);
    let digest = checksum(&data);
    put_block_with_algorithm(
        router,
        snapshot_id,
        0,
        data,
        BLOCK_SIZE,
        &digest,
        algorithm,
    )
    .await
}

/// One row of the out-of-form table: the header value, and the clauses the report must
/// state for it.
struct OutsideTheForm {
    case: &'static str,
    algorithm: String,
    clauses: Vec<String>,
}

/// RULE: a `ChecksumAlgorithm` outside the form its member declares is refused by the
/// MODEL's constraints — the framework's constraint report in the service's own modelled
/// `ValidationException` — and not as an algorithm the service does not support. Asserted
/// on both operations that carry the member, since both take it off the boundary.
#[tokio::test]
async fn a_checksum_algorithm_outside_its_modelled_form_is_refused_by_the_modelled_constraints() {
    let router = router();
    let snapshot_id = pending_snapshot(&router).await;

    let one_past = "A".repeat(ALGORITHM_MAX_LENGTH + 1);
    let far_past = "A".repeat(MAX_ECHOED_VALUE + 44);
    let long_and_malformed = format!("{} ", "A".repeat(ALGORITHM_MAX_LENGTH));

    let rows: Vec<OutsideTheForm> = vec![
        OutsideTheForm {
            case: "an empty algorithm, which the modelled pattern admits no value of",
            algorithm: String::new(),
            clauses: vec![clause(
                "",
                "checksumAlgorithm",
                &pattern_requirement(ALGORITHM_PATTERN),
            )],
        },
        OutsideTheForm {
            case: "an algorithm carrying a space",
            algorithm: "SHA 256".to_string(),
            clauses: vec![clause(
                "SHA 256",
                "checksumAlgorithm",
                &pattern_requirement(ALGORITHM_PATTERN),
            )],
        },
        OutsideTheForm {
            case: "an algorithm carrying a hyphen",
            algorithm: "SHA-256".to_string(),
            clauses: vec![clause(
                "SHA-256",
                "checksumAlgorithm",
                &pattern_requirement(ALGORITHM_PATTERN),
            )],
        },
        OutsideTheForm {
            case: "an algorithm carrying punctuation",
            algorithm: "SHA256!".to_string(),
            clauses: vec![clause(
                "SHA256!",
                "checksumAlgorithm",
                &pattern_requirement(ALGORITHM_PATTERN),
            )],
        },
        OutsideTheForm {
            case: "an algorithm one character past the modelled maximum length",
            algorithm: one_past.clone(),
            clauses: vec![clause(
                &one_past,
                "checksumAlgorithm",
                &max_length_requirement(ALGORITHM_MAX_LENGTH),
            )],
        },
        OutsideTheForm {
            case: "an algorithm far past the modelled maximum length",
            algorithm: far_past.clone(),
            clauses: vec![clause(
                &echoed(&far_past),
                "checksumAlgorithm",
                &max_length_requirement(ALGORITHM_MAX_LENGTH),
            )],
        },
        OutsideTheForm {
            case: "an algorithm past the modelled maximum length AND outside the pattern",
            algorithm: long_and_malformed.clone(),
            clauses: vec![
                clause(
                    &long_and_malformed,
                    "checksumAlgorithm",
                    &max_length_requirement(ALGORITHM_MAX_LENGTH),
                ),
                clause(
                    &long_and_malformed,
                    "checksumAlgorithm",
                    &pattern_requirement(ALGORITHM_PATTERN),
                ),
            ],
        },
    ];

    for row in &rows {
        let report = constraint_report(&row.clauses);
        for (operation, reply) in [
            (
                "PutSnapshotBlock",
                put_with_algorithm(&router, &snapshot_id, &row.algorithm).await,
            ),
            (
                "CompleteSnapshot",
                complete_snapshot_with_algorithm(&router, &snapshot_id, 0, &row.algorithm).await,
            ),
        ] {
            let case = format!("{operation} with {}", row.case);
            assert_validation(&case, &reply, CONSTRAINT_REFUSAL_REASON, &report);
            let message = member(&reply, "Message");
            assert!(
                !message.starts_with("Unsupported Checksum Algorithm"),
                "case {case:?}: a value outside the member's modelled form must be refused by \
                 those constraints, not as an unsupported algorithm, got {message:?}"
            );
        }
    }

    // The controls that make the rows above mean something. The snapshot none of them
    // touched still takes the write and the completion, so every refusal was about the
    // value the row carried.
    let written = put_with_algorithm(&router, &snapshot_id, SUPPORTED_ALGORITHM).await;
    assert_eq!(
        written.status, 201,
        "case \"the supported algorithm\": PutSnapshotBlock must answer 201, got {} with body {}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );
    let sealed = complete_snapshot_with_algorithm(&router, &snapshot_id, 1, SUPPORTED_ALGORITHM).await;
    assert_eq!(
        sealed.status, 202,
        "case \"the supported algorithm\": CompleteSnapshot must answer 202, got {} with body {}",
        sealed.status,
        String::from_utf8_lossy(&sealed.body)
    );
    assert_eq!(
        member(&sealed, "Status"),
        "pending",
        "case \"the supported algorithm\": the 202 must report the status the completion was \
         accepted against, got {}",
        sealed.json()
    );
}

/// RULE, the other half: a `ChecksumAlgorithm` the model DOES admit is judged on what the
/// value means — the set of algorithms this service supports — and that refusal names the
/// rejected algorithm and the supported set inside the `@length` the `Message` member
/// carrying it declares.
///
/// Rows differ along which unsupported algorithm was asked for: shorter and longer names,
/// one differing from the supported value only in case, a digit-only value, and a value at
/// the member's modelled maximum length.
#[tokio::test]
async fn a_checksum_algorithm_inside_its_modelled_form_is_judged_on_the_supported_set() {
    let router = router();
    let snapshot_id = pending_snapshot(&router).await;

    let at_maximum = "A".repeat(ALGORITHM_MAX_LENGTH);
    let inside: Vec<String> = vec![
        "MD5".to_string(),
        "SHA1".to_string(),
        "CRC32C".to_string(),
        "sha256".to_string(),
        "0".to_string(),
        at_maximum,
    ];

    for algorithm in &inside {
        let expected = unsupported_message(algorithm);
        for (operation, reply) in [
            (
                "PutSnapshotBlock",
                put_with_algorithm(&router, &snapshot_id, algorithm).await,
            ),
            (
                "CompleteSnapshot",
                complete_snapshot_with_algorithm(&router, &snapshot_id, 0, algorithm).await,
            ),
        ] {
            let case = format!("{operation} with the unsupported algorithm {algorithm:?}");
            assert_validation(&case, &reply, CONSTRAINT_REFUSAL_REASON, &expected);
            let message = member(&reply, "Message");
            assert!(
                message.chars().count() <= ERROR_MESSAGE_MAX_LENGTH,
                "case {case:?}: expected a Message of at most {ERROR_MESSAGE_MAX_LENGTH} \
                 characters, got {} characters: {message:?}",
                message.chars().count()
            );
        }
    }

    // And the one value the modelled enum declares is accepted, so the refusals above are
    // about the algorithm asked for and not about the member.
    let written = put_with_algorithm(&router, &snapshot_id, SUPPORTED_ALGORITHM).await;
    assert_eq!(
        written.status, 201,
        "case \"the supported algorithm\": PutSnapshotBlock must answer 201, got {} with body {}",
        written.status,
        String::from_utf8_lossy(&written.body)
    );
    assert_eq!(
        written.header("x-amz-Checksum-Algorithm"),
        SUPPORTED_ALGORITHM,
        "case \"the supported algorithm\": the stored block must report the algorithm it was \
         checksummed with, got {:?}",
        written.header("x-amz-Checksum-Algorithm")
    );
}

/// RULE, applied where it is visible: the member joins the very walk its siblings are
/// judged in, so a request breaking its form AND another judged member's is reported in the
/// ONE envelope the boundary would have used, with a clause per rejected value in the order
/// the input shape declares its members.
#[tokio::test]
async fn a_request_outside_two_judged_members_reports_both_in_one_envelope() {
    let router = router();
    let snapshot_id = pending_snapshot(&router).await;

    let malformed_checksum = "not*a*checksum";
    let one_past = "A".repeat(ALGORITHM_MAX_LENGTH + 1);
    let written = put_block_with_algorithm(
        &router,
        &snapshot_id,
        0,
        block_bytes(19),
        BLOCK_SIZE,
        malformed_checksum,
        &one_past,
    )
    .await;
    assert_validation(
        "PutSnapshotBlock with a Checksum outside its pattern and an over-long algorithm",
        &written,
        CONSTRAINT_REFUSAL_REASON,
        &constraint_report(&[
            clause(
                malformed_checksum,
                "checksum",
                &pattern_requirement(CHECKSUM_PATTERN),
            ),
            clause(
                &one_past,
                "checksumAlgorithm",
                &max_length_requirement(ALGORITHM_MAX_LENGTH),
            ),
        ]),
    );

    let completed = complete_snapshot_with_algorithm(&router, &snapshot_id, -1, "SHA 256").await;
    assert_validation(
        "CompleteSnapshot with a ChangedBlocksCount below its minimum and a malformed algorithm",
        &completed,
        CONSTRAINT_REFUSAL_REASON,
        &constraint_report(&[
            clause(
                "-1",
                "changedBlocksCount",
                &min_value_requirement(CHANGED_BLOCKS_COUNT_MIN),
            ),
            clause(
                "SHA 256",
                "checksumAlgorithm",
                &pattern_requirement(ALGORITHM_PATTERN),
            ),
        ]),
    );

    // The control: with both members inside their form the same write is stored and the
    // same completion accepted, so neither refusal above touched the snapshot.
    let data = block_bytes(19);
    let digest = checksum(&data);
    let stored = put_block(&router, &snapshot_id, 0, data, BLOCK_SIZE, &digest).await;
    assert_eq!(
        stored.status, 201,
        "case \"both members inside their form\": PutSnapshotBlock must answer 201, got {} with \
         body {}",
        stored.status,
        String::from_utf8_lossy(&stored.body)
    );
    let sealed = complete_snapshot_with_algorithm(&router, &snapshot_id, 1, SUPPORTED_ALGORITHM).await;
    assert_eq!(
        sealed.status, 202,
        "case \"both members inside their form\": CompleteSnapshot must answer 202, got {} with \
         body {}",
        sealed.status,
        String::from_utf8_lossy(&sealed.body)
    );
}
