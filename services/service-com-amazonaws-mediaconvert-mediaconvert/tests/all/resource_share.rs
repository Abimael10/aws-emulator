// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of `CreateResourceShare`, and of the two `Job` members that
//! report what it recorded (`ShareStatus` and `LastShareDetails`).
//!
//! Every case drives the real restJson1 wire path — the router picks the service from
//! the request's SigV4 scope, the framework deserializes and validates, the handler
//! runs, and the response is serialized — so what is asserted is the bytes a client
//! would see, never an internal call.
//!
//! The rule under test, which every case below is one row of: `CreateResourceShare`
//! records a share attempt against the job its reference resolves to — in either form a
//! MediaConvert resource reference comes in — moving that job's `ShareStatus` to the
//! non-terminal `INITIATED` and its `LastShareDetails` to the attempt it persisted; the
//! clock then completes that attempt to `SHARED`. A job nothing has ever asked to share
//! is `NOT_SHARED` with no details, a reference that resolves to no stored job is the
//! modelled `NotFoundException`, a repeat against the same support case changes nothing,
//! and a request naming another support case records a new attempt that supersedes the
//! one before it — the job reports "the most recent share attempt". A `SupportCaseId`
//! that is not of the form AWS Support case identifiers take identifies no case, so it is
//! the modelled `BadRequestException` naming the member and that form, and records
//! nothing.
//!
//! A case that asserts the NON-TERMINAL status pins the completion span to
//! [`HELD_IN_INITIATED`] and a case that asserts the COMPLETION pins it to
//! [`SHORT_SHARE`] and then looks for the status it waits on ([`share_view_at`]), so no
//! assertion here races the machine in either direction.

use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_mediaconvert_mediaconvert::state::{
    not_found_message, ShareAttempt, SHARE_STATUS_INITIATED, SHARE_STATUS_NOT_SHARED,
    SHARE_STATUS_SHARED,
};
use service_com_amazonaws_mediaconvert_mediaconvert::ComAmazonawsMediaconvertMediaconvertState;

use super::job_lifecycle::{arn, settings, Emulator, Reply, ROLE};

/// A completion span short enough that a case waiting for the completion costs almost no
/// wall-clock time. Only a case that asserts the COMPLETED attempt pins it.
const SHORT_SHARE: Duration = Duration::from_millis(20);

/// How long a case waits for the CLOCK to complete a share attempt before it gives up.
///
/// Generous: a deadline for a machine under load, not a span anything is expected to take
/// (the span a case pins is [`SHORT_SHARE`]). What a case waits for is the CONDITION — the
/// status the job reports — never a fixed time.
const SHARE_DEADLINE: Duration = Duration::from_secs(60);

/// Longer than any case takes to run, so a share attempt stays `INITIATED` for the whole
/// of a case that is about the non-terminal status.
const HELD_IN_INITIATED: Duration = Duration::from_secs(3_600);

/// Pin how long after `CreateResourceShare` a share attempt is due to complete.
///
/// The span is state, as the job settle span is, so no case depends on how fast the
/// machine is. Nothing on a request path writes it.
fn pin_share_span(emulator: &Emulator, span: Duration) {
    emulator
        .engine
        .state::<ComAmazonawsMediaconvertMediaconvertState>()
        .lock()
        .share_completes_after = span;
}

/// An emulator whose jobs stay `SUBMITTED` and whose share attempts come due after
/// `span`.
fn emulator_with_share_span(span: Duration) -> Emulator {
    let emulator = Emulator::new();
    pin_share_span(&emulator, span);
    emulator
}

/// A stored job to share, created through the real `CreateJob` path. Returns its `Id`.
async fn fixture_job(emulator: &Emulator, input_file: &str) -> String {
    let created = emulator
        .create_job(json!({
            "role": ROLE,
            "settings": settings(input_file, "_out")
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "the fixture job must be created: got {} with body {}",
        created.status, created.body
    );
    let id = created.string("/job/id");
    assert!(
        !id.is_empty(),
        "the fixture job must carry an id, got {}",
        created.body
    );
    id
}

/// `CreateResourceShare` for `job_reference` and `support_case_id`.
async fn create_resource_share(
    emulator: &Emulator,
    job_reference: &str,
    support_case_id: &str,
) -> Reply {
    emulator
        .send(
            "POST",
            "/2017-08-29/resourceShares",
            Some(json!({
                "jobId": job_reference,
                "supportCaseId": support_case_id
            })),
        )
        .await
}

/// The `ShareStatus` and `LastShareDetails` a `GetJob` reports for `id`.
async fn share_view(emulator: &Emulator, id: &str) -> (String, Value) {
    let read = emulator.get_job(id).await;
    assert_eq!(
        read.status, 200,
        "job {id} must read back: got {} with body {}",
        read.status, read.body
    );
    (
        read.string("/job/shareStatus"),
        read.at("/job/lastShareDetails").clone(),
    )
}

/// The `ShareStatus` and `LastShareDetails` a `GetJob` reports for `id` once the status is
/// `expected`, looking again until it is or [`SHARE_DEADLINE`] passes.
///
/// The CONDITION, not a duration: the completion of an attempt is the clock's business (it
/// is owed the pinned span after the call that started it), so a case that needs the
/// completed attempt polls the job the way a client does rather than waiting a fixed time
/// and hoping. A status that is already `expected` returns on the first look, so a case
/// about the non-terminal status pays nothing for it.
async fn share_view_at(
    emulator: &Emulator,
    case: &str,
    id: &str,
    expected: &str,
) -> (String, Value) {
    let started = ::std::time::Instant::now();
    loop {
        let (status, details) = share_view(emulator, id).await;
        if status == expected {
            return (status, details);
        }
        assert!(
            started.elapsed() < SHARE_DEADLINE,
            "case {case:?}: the clock never moved the share of job {id} to {expected}, it \
             reports {status:?} with details {details}"
        );
        ::tokio::task::yield_now().await;
    }
}

/// What `LastShareDetails` must read for an attempt against `support_case_id` in
/// `status`, built through the emulator's OWN rendering of a stored attempt, so the
/// expectation is not a second hand-written format. One case below pins that rendering
/// literally, so a silent change to it is still caught.
fn expected_details(support_case_id: &str, status: &str) -> Value {
    json!(ShareAttempt {
        support_case_id: support_case_id.to_string(),
        status: status.to_string(),
        // The instant takes no part in the rendering; it is the attempt's entry in the
        // store's pending index.
        completes_at: None,
    }
    .details())
}

/// One row of the reference-and-case table: how the job is named, and the support case
/// the share is requested against.
struct ShareRow {
    case: &'static str,
    /// The `JobId` form: built from the stored job id.
    reference: fn(&str) -> String,
    /// The `SupportCaseId` the share names.
    support_case_id: &'static str,
}

/// The job's own id, the first form `CreateResourceShare$JobId` accepts ("Specify
/// MediaConvert Job ID or ARN to share").
fn by_id(id: &str) -> String {
    id.to_string()
}

/// The job's ARN, the second form that member accepts.
fn by_arn(id: &str) -> String {
    arn(&format!("jobs/{id}"))
}

/// The rows every reference-and-case table below spans: both forms `JobId` accepts,
/// crossed with support case identifiers of the form the service enforces for
/// `SupportCaseId` — 15 decimal digits, the form
/// `state::SUPPORT_CASE_ID_FORMAT` states — differing along what remains free in it: a
/// leading-zero id, a trailing-zero one, one digit repeated, the lowest and the highest
/// id the form admits. (What a value of ANOTHER form earns is
/// `a_support_case_id_of_another_form_is_refused` below.)
fn share_rows() -> [ShareRow; 5] {
    [
        ShareRow {
            case: "job id, an id padded with leading zeros",
            reference: by_id,
            support_case_id: "000000000042424",
        },
        ShareRow {
            case: "job ARN, an id ending in zeros",
            reference: by_arn,
            support_case_id: "987654321000000",
        },
        ShareRow {
            case: "job id, one digit repeated",
            reference: by_id,
            support_case_id: "777777777777777",
        },
        ShareRow {
            case: "job ARN, the lowest id the form admits",
            reference: by_arn,
            support_case_id: "000000000000000",
        },
        ShareRow {
            case: "job id, the highest id the form admits",
            reference: by_id,
            support_case_id: "999999999999999",
        },
    ]
}

/// A share attempt is recorded against the job the request names, in either reference
/// form, and the job reports it as `INITIATED` until the clock completes it.
#[tokio::test]
async fn a_resource_share_is_recorded_against_the_job_it_names() {
    for row in share_rows() {
        let emulator = emulator_with_share_span(HELD_IN_INITIATED);
        let id = fixture_job(&emulator, "s3://example-bucket/in/a.mp4").await;

        // Before any share: the job reports NOT_SHARED and no share details at all.
        let (status, details) = share_view(&emulator, &id).await;
        assert_eq!(
            status, SHARE_STATUS_NOT_SHARED,
            "case {:?}: an unshared job must report {SHARE_STATUS_NOT_SHARED}, got {status:?}",
            row.case
        );
        assert_eq!(
            details,
            Value::Null,
            "case {:?}: an unshared job must report no share details, got {details}",
            row.case
        );

        let reference = (row.reference)(&id);
        let shared = create_resource_share(&emulator, &reference, row.support_case_id).await;
        assert_eq!(
            shared.status, 202,
            "case {:?}: CreateResourceShare must answer HTTP 202, got {} with body {}",
            row.case, shared.status, shared.body
        );
        assert_eq!(
            shared.body,
            json!({}),
            "case {:?}: CreateResourceShare models no output member, so its body must \
             carry none, got {}",
            row.case,
            shared.body
        );

        // The attempt is in flight: the status is the non-terminal INITIATED and the
        // details name the support case the request asked for.
        let (status, details) = share_view(&emulator, &id).await;
        assert_eq!(
            status, SHARE_STATUS_INITIATED,
            "case {:?}: a recorded share must report {SHARE_STATUS_INITIATED}, got {status:?}",
            row.case
        );
        assert_eq!(
            details,
            expected_details(row.support_case_id, SHARE_STATUS_INITIATED),
            "case {:?}: expected the details of the attempt just recorded, got {details}",
            row.case
        );

        // Reading the job does not complete the share: the status is the clock's to move.
        let (polled, polled_details) = share_view(&emulator, &id).await;
        assert_eq!(
            polled, SHARE_STATUS_INITIATED,
            "case {:?}: polling the job must not move the share status, got {polled:?}",
            row.case
        );
        assert_eq!(
            polled_details, details,
            "case {:?}: polling the job must not rewrite its share details, got \
             {polled_details}",
            row.case
        );
    }
}

/// Past the instant the clock owes it, the recorded attempt reports `SHARED` — against
/// the same support case it was recorded for.
#[tokio::test]
async fn a_recorded_share_completes_when_the_clock_makes_it_due() {
    for row in share_rows() {
        let emulator = emulator_with_share_span(SHORT_SHARE);
        let id = fixture_job(&emulator, "s3://example-bucket/in/b.mp4").await;
        let reference = (row.reference)(&id);
        let shared = create_resource_share(&emulator, &reference, row.support_case_id).await;
        assert_eq!(
            shared.status, 202,
            "case {:?}: CreateResourceShare must answer HTTP 202, got {} with body {}",
            row.case, shared.status, shared.body
        );

        let (status, details) = share_view_at(&emulator, row.case, &id, SHARE_STATUS_SHARED).await;
        assert_eq!(
            status, SHARE_STATUS_SHARED,
            "case {:?}: past the completion instant the share must report \
             {SHARE_STATUS_SHARED}, got {status:?}",
            row.case
        );
        assert_eq!(
            details,
            expected_details(row.support_case_id, SHARE_STATUS_SHARED),
            "case {:?}: expected the details of the completed attempt, got {details}",
            row.case
        );
    }
}

/// The rendering `Job$LastShareDetails` carries, pinned LITERALLY in both statuses.
///
/// The tables elsewhere in this suite build their expectation through the emulator's own
/// renderer, which would follow a silent change to the format; this case would not.
#[tokio::test]
async fn the_share_details_name_the_support_case_and_the_status() {
    let held = emulator_with_share_span(HELD_IN_INITIATED);
    let held_job = fixture_job(&held, "s3://example-bucket/in/c.mp4").await;
    let shared = create_resource_share(&held, &held_job, "123456789012345").await;
    assert_eq!(
        shared.status, 202,
        "the share must answer HTTP 202, got {} with body {}",
        shared.status, shared.body
    );
    let (_, details) = share_view(&held, &held_job).await;
    assert_eq!(
        details,
        json!("supportCaseId=123456789012345, shareStatus=INITIATED"),
        "case \"in flight\": expected the rendered details of the recorded attempt, got \
         {details}"
    );

    let completing = emulator_with_share_span(SHORT_SHARE);
    let completing_job = fixture_job(&completing, "s3://example-bucket/in/c.mp4").await;
    let shared = create_resource_share(&completing, &completing_job, "123456789012345").await;
    assert_eq!(
        shared.status, 202,
        "the share must answer HTTP 202, got {} with body {}",
        shared.status, shared.body
    );
    let (_, details) =
        share_view_at(&completing, "completed", &completing_job, SHARE_STATUS_SHARED).await;
    assert_eq!(
        details,
        json!("supportCaseId=123456789012345, shareStatus=SHARED"),
        "case \"completed\": expected the rendered details of the completed attempt, got \
         {details}"
    );
}

/// A repeat against the SAME support case asks for the state the job is already in, so it
/// answers the same empty 202 and leaves the recorded attempt exactly as it was.
///
/// Both rows of the class: the repeat while the attempt is still in flight, and the repeat
/// after it completed.
#[tokio::test]
async fn a_repeat_for_the_same_support_case_changes_nothing() {
    for (case, span, expected_status) in [
        (
            "repeat while the attempt is in flight",
            HELD_IN_INITIATED,
            SHARE_STATUS_INITIATED,
        ),
        (
            "repeat after the attempt completed",
            SHORT_SHARE,
            SHARE_STATUS_SHARED,
        ),
    ] {
        let emulator = emulator_with_share_span(span);
        let id = fixture_job(&emulator, "s3://example-bucket/in/d.mp4").await;
        let first = create_resource_share(&emulator, &id, "000000000000077").await;
        assert_eq!(
            first.status, 202,
            "case {case:?}: the first share must answer HTTP 202, got {} with body {}",
            first.status, first.body
        );
        // The row's own condition: the attempt is still in flight, or the clock has
        // completed it. Looked for, never waited out.
        let before = share_view_at(&emulator, case, &id, expected_status).await;

        // The repeat names the job the other way round, so the idempotent repeat is not a
        // property of the reference form either.
        let repeat = create_resource_share(&emulator, &by_arn(&id), "000000000000077").await;
        assert_eq!(
            repeat.status, 202,
            "case {case:?}: the repeat must answer HTTP 202, got {} with body {}",
            repeat.status, repeat.body
        );

        let after = share_view(&emulator, &id).await;
        assert_eq!(
            after.0, expected_status,
            "case {case:?}: expected the attempt to stay {expected_status}, got {:?}",
            after.0
        );
        assert_eq!(
            after, before,
            "case {case:?}: the repeat must leave the recorded attempt as it was, \
             expected {before:?}, got {after:?}"
        );
    }
}

/// A request naming ANOTHER support case records a new attempt that supersedes the one
/// before it: `LastShareDetails` reports "the most recent share attempt for the job".
///
/// The rows differ in what the superseded attempt was: one still in flight, and one the
/// clock had already completed.
#[tokio::test]
async fn another_support_case_supersedes_the_previous_attempt() {
    for (case, span, completed_first) in [
        ("supersede an attempt in flight", HELD_IN_INITIATED, false),
        ("supersede a completed attempt", SHORT_SHARE, true),
    ] {
        let emulator = emulator_with_share_span(span);
        let id = fixture_job(&emulator, "s3://example-bucket/in/e.mp4").await;
        let first = create_resource_share(&emulator, &id, "111111111111111").await;
        assert_eq!(
            first.status, 202,
            "case {case:?}: the first share must answer HTTP 202, got {} with body {}",
            first.status, first.body
        );
        if completed_first {
            // The row's own condition: the attempt this one supersedes has completed.
            // Looked for, never waited out — and the attempts that follow are then held in
            // flight, so what is asserted below is the superseding and not how fast the
            // machine ran.
            share_view_at(&emulator, case, &id, SHARE_STATUS_SHARED).await;
            pin_share_span(&emulator, HELD_IN_INITIATED);
        }

        // Each later case supersedes the one before it, named either way round.
        for (step, (reference, support_case_id)) in [
            (by_id(&id), "222222222222222"),
            (by_arn(&id), "333333333333333"),
        ]
        .into_iter()
        .enumerate()
        {
            let next = create_resource_share(&emulator, &reference, support_case_id).await;
            assert_eq!(
                next.status, 202,
                "case {case:?} step {step}: a new support case must answer HTTP 202, got \
                 {} with body {}",
                next.status, next.body
            );
            let (status, details) = share_view(&emulator, &id).await;
            assert_eq!(
                status, SHARE_STATUS_INITIATED,
                "case {case:?} step {step}: the new attempt must report \
                 {SHARE_STATUS_INITIATED}, got {status:?}"
            );
            assert_eq!(
                details,
                expected_details(support_case_id, SHARE_STATUS_INITIATED),
                "case {case:?} step {step}: the details must name the most recent \
                 attempt, got {details}"
            );
        }
    }
}

/// The attempt that completes is the one the job holds: a superseded attempt's due instant
/// completes nothing, and the surviving attempt reports its OWN support case as `SHARED`.
#[tokio::test]
async fn the_attempt_that_completes_is_the_most_recent_one() {
    let emulator = emulator_with_share_span(SHORT_SHARE);
    let id = fixture_job(&emulator, "s3://example-bucket/in/f.mp4").await;
    for support_case_id in ["100000000000001", "200000000000002", "300000000000003"] {
        let shared = create_resource_share(&emulator, &id, support_case_id).await;
        assert_eq!(
            shared.status, 202,
            "case {support_case_id:?}: the share must answer HTTP 202, got {} with body {}",
            shared.status, shared.body
        );
    }

    let (status, details) = share_view_at(
        &emulator,
        "the most recent attempt",
        &id,
        SHARE_STATUS_SHARED,
    )
    .await;
    assert_eq!(
        status, SHARE_STATUS_SHARED,
        "the surviving attempt must complete to {SHARE_STATUS_SHARED}, got {status:?}"
    );
    assert_eq!(
        details,
        expected_details("300000000000003", SHARE_STATUS_SHARED),
        "the completed attempt must be the most recent one, got {details}"
    );
}

/// A `JobId` that CARRIES CHARACTERS but resolves to no stored job is the modelled
/// `NotFoundException`, worded the way this service words a lookup that resolves nothing —
/// naming the request member. (A `JobId` carrying no characters resolves nothing because it
/// names nothing at all, which is the bad request `tests/all/blank_resource_name.rs` pins.)
///
/// The table spans the class: an id nothing holds, the ARN of such an id, the ARN of
/// another resource type, and a stored id padded with whitespace — the reference is not
/// normalised, so the padded spelling of a stored id names no job either.
#[tokio::test]
async fn sharing_a_job_that_is_not_stored_is_the_modelled_not_found() {
    let emulator = emulator_with_share_span(HELD_IN_INITIATED);
    // One stored job, so the refusals below are about the reference and not about an
    // empty store.
    let stored = fixture_job(&emulator, "s3://example-bucket/in/g.mp4").await;

    for (case, reference) in [
        ("an id nothing holds", "1700000000000-abcdef".to_string()),
        (
            "the ARN of an id nothing holds",
            by_arn("1700000000000-abcdef"),
        ),
        ("the ARN of another resource type", arn("queues/Default")),
        (
            "a stored id padded with whitespace",
            format!("  {stored}  "),
        ),
    ] {
        let reply = create_resource_share(&emulator, &reference, "404000000000404").await;
        assert_eq!(
            reply.status, 404,
            "case {case:?}: expected HTTP 404, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some("NotFoundException"),
            "case {case:?}: expected NotFoundException, got {:?}",
            reply.error_type
        );
        assert_eq!(
            reply.string("/message"),
            not_found_message("jobId"),
            "case {case:?}: expected the member-naming refusal, got {}",
            reply.body
        );
    }

    // The stored job is untouched by any of those refusals.
    let (status, details) = share_view(&emulator, &stored).await;
    assert_eq!(
        status, SHARE_STATUS_NOT_SHARED,
        "a refused share must leave the stored job {SHARE_STATUS_NOT_SHARED}, got {status:?}"
    );
    assert_eq!(
        details,
        Value::Null,
        "a refused share must record no details on any job, got {details}"
    );
}

/// A `SupportCaseId` that names no support case is a problem in the request: the modelled
/// `BadRequestException`, with nothing recorded against the job.
///
/// The table spans the whitespace-only forms the member's bare `__string` target lets
/// through, since the model constrains neither its length nor its pattern.
#[tokio::test]
async fn a_support_case_id_that_names_no_case_is_refused() {
    let emulator = emulator_with_share_span(HELD_IN_INITIATED);
    let id = fixture_job(&emulator, "s3://example-bucket/in/h.mp4").await;

    for (case, support_case_id) in [
        ("empty", ""),
        ("one space", " "),
        ("spaces", "    "),
        ("a tab", "\t"),
        ("a newline", "\n"),
    ] {
        let reply = create_resource_share(&emulator, &id, support_case_id).await;
        assert_eq!(
            reply.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            reply.status, reply.body
        );
        assert_eq!(
            reply.error_type.as_deref(),
            Some("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {:?}",
            reply.error_type
        );
        assert!(
            reply.string("/message").contains("supportCaseId"),
            "case {case:?}: the refusal must name the member it is in, got {}",
            reply.body
        );

        let (status, details) = share_view(&emulator, &id).await;
        assert_eq!(
            status, SHARE_STATUS_NOT_SHARED,
            "case {case:?}: a refused share must leave the job \
             {SHARE_STATUS_NOT_SHARED}, got {status:?}"
        );
        assert_eq!(
            details,
            Value::Null,
            "case {case:?}: a refused share must record no details, got {details}"
        );
    }

    // A real case id against the same job still works, so the refusals above blocked the
    // request and not the job.
    let accepted = create_resource_share(&emulator, &id, "123456789012345").await;
    assert_eq!(
        accepted.status, 202,
        "a well-formed share must still be accepted: got {} with body {}",
        accepted.status, accepted.body
    );
    let (status, _) = share_view(&emulator, &id).await;
    assert_eq!(
        status, SHARE_STATUS_INITIATED,
        "the accepted share must move the job to {SHARE_STATUS_INITIATED}, got {status:?}"
    );
}

/// The sentence MediaConvert answers a `SupportCaseId` that is not of the form the service
/// enforces for it, spelled out here rather than imported from the crate so a wrong
/// constant in the implementation cannot make this case pass.
const INVALID_SUPPORT_CASE: &str = "You specified an invalid value for supportCaseId. The \
     resource ID must match the format '^\\d{15}$'.";

/// A `SupportCaseId` of ANOTHER FORM identifies no AWS Support case, so it is the modelled
/// `BadRequestException` naming the member and the form — and nothing is recorded against
/// the job.
///
/// "AWS Support case identifier": the identifier of a resource of another system, which
/// that system publishes in one form — 15 decimal digits, the form MediaConvert states in
/// this very refusal. The refused rows span the ways a value can fail to be one: one digit
/// short and one digit long (the boundary either side of the form), far shorter and far
/// longer, a prefixed id, a separator, a space, a sign, a letter, a decimal point and
/// digits that are not ASCII. The accepted rows are the edges of the form itself.
#[tokio::test]
async fn a_support_case_id_of_another_form_is_refused() {
    let emulator = emulator_with_share_span(HELD_IN_INITIATED);
    let id = fixture_job(&emulator, "s3://example-bucket/in/i.mp4").await;

    for (case, value) in [
        ("one digit short of the form", "12345678901234"),
        ("one digit past the form", "1234567890123456"),
        ("a single digit", "7"),
        ("far longer than the form", "12345678901234567890123456789012345"),
        ("a prefixed case id", "case-123456789012345"),
        ("a separator among the digits", "12345-678901234"),
        ("a space among the digits", "12345678 901234"),
        ("a signed id", "+12345678901234"),
        ("a letter among the digits", "12345678901234a"),
        ("a decimal point", "1234567890123.5"),
        ("digits that are not ASCII", "１２３４５６７８９０１２３４５"),
    ] {
        let refused = create_resource_share(&emulator, &id, value).await;
        assert_eq!(
            refused.status, 400,
            "case {case:?}: expected HTTP 400, got {} with body {}",
            refused.status, refused.body
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("BadRequestException"),
            "case {case:?}: expected BadRequestException, got {:?} with body {}",
            refused.error_type,
            refused.body
        );
        assert_eq!(
            refused.string("/message"),
            INVALID_SUPPORT_CASE,
            "case {case:?}: expected the refusal to name the member and the form, got {}",
            refused.body
        );

        let (status, details) = share_view(&emulator, &id).await;
        assert_eq!(
            status, SHARE_STATUS_NOT_SHARED,
            "case {case:?}: a refused share must leave the job \
             {SHARE_STATUS_NOT_SHARED}, got {status:?}"
        );
        assert_eq!(
            details,
            Value::Null,
            "case {case:?}: a refused share must record no details, got {details}"
        );
    }

    // The other side of the rule: a value OF the form is accepted and recorded, at each
    // edge of the form and in between.
    for (case, value) in [
        ("the lowest id the form admits", "000000000000000"),
        ("an ordinary case number", "472819003651884"),
        ("the highest id the form admits", "999999999999999"),
    ] {
        let accepted = create_resource_share(&emulator, &id, value).await;
        assert_eq!(
            accepted.status, 202,
            "case {case:?}: expected HTTP 202, got {} with body {}",
            accepted.status, accepted.body
        );
        let (status, details) = share_view(&emulator, &id).await;
        assert_eq!(
            status, SHARE_STATUS_INITIATED,
            "case {case:?}: an accepted share must move the job to \
             {SHARE_STATUS_INITIATED}, got {status:?}"
        );
        assert_eq!(
            details,
            expected_details(value, SHARE_STATUS_INITIATED),
            "case {case:?}: expected the details of the attempt just recorded, got {details}"
        );
    }
}
