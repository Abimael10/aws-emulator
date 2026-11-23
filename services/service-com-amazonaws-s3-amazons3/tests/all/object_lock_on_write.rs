// SPDX-License-Identifier: CC0-1.0

//! The Object Lock an object WRITE asks for, recorded on the version it writes.
//!
//! Every assertion drives a real HTTP request through the router (the harness the
//! bucket-lifecycle, object and Object Lock suites in this crate already own), so what is
//! pinned is the wire behaviour: the bound headers a read reports, the status a delete of the
//! locked version answers, and what the bucket holds afterwards.
//!
//! The rules under test, each asserted over a SET of inputs rather than one request:
//!
//! 1. "The Object Lock mode that you want to apply to this object" with "the date and time
//!    when you want this object's Object Lock to expire"
//!    (`PutObjectRequest$ObjectLockMode` and `$ObjectLockRetainUntilDate`), and "specifies
//!    whether a legal hold will be applied to this object"
//!    (`$ObjectLockLegalHoldStatus`) — so the write stores that lock ON the version it
//!    records, a later read reports it, and a delete of that version is judged against it:
//!    "if you try to delete objects protected by … retention and the retention period has not
//!    expired, the operation fails" (Amazon S3 User Guide, *S3 Object Lock*), while
//!    `x-amz-bypass-governance-retention` reaches a GOVERNANCE-mode period.
//! 2. Every write that declares those members records the same lock from the same values: the
//!    one-shot `PutObject`, the `CopyObject` whose members are "the Object Lock mode that you
//!    want to apply to the copied object", and the `CreateMultipartUpload` whose initiation
//!    "specif[ies] the options you want on the finished object", which the completion stores.
//! 3. A lock the service cannot record is refused and nothing is written: a retention mode
//!    without the date it runs to (and a date without a mode) with 400 `InvalidRequest`,
//!    because "a retention runs in a mode to a date", and any of the three members addressed
//!    to a bucket with no Object Lock configuration with 400 `InvalidRequest` and the
//!    sentence S3 publishes for that condition.
//! 4. An EVENT HOLD is a member of that same retention: "the event hold status for the
//!    object" with "the event hold duration for the object"
//!    (`ObjectLockRetention$EventHold` / `$EventHoldDuration`), which a write names through
//!    `x-amz-object-lock-event-hold` and one of
//!    `x-amz-object-lock-event-hold-duration-days` / `-years`
//!    (`PutObjectRequest$ObjectLockEventHold` and the two duration members). A write that
//!    names them stores them on the version it writes, both readers of that version report
//!    them back — the duration "only … when the event hold is enabled"
//!    (`GetObjectOutput$ObjectLockEventHoldDurationDays`) — as does the retention
//!    subresource, and `PutObjectRetention` changes the hold while the mode and the period it
//!    is a member of stay as they are.
//! 5. The pieces of a hold have to agree, and a request whose pieces do not is refused with
//!    nothing written: a hold with no mode for its retention to run in, and an enabled hold
//!    that does not name exactly one duration, are 400 `InvalidRequest`; a retain-until date
//!    that does not reach the end of the hold is 400 `InvalidArgument` naming that header and
//!    the instant it carried.
//!
//! The rows differ along what the rule generalises over: which member the write names and how
//! many at once, the retention MODE, how far the retain-until date runs, the legal hold
//! status, the event hold status and the unit and count of its duration, and which operation
//! carries the members.

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use service_com_amazonaws_s3_amazons3::state::{
    ACCESS_DENIED_CODE, COMPLIANCE_RETENTION_MODE, EVENT_HOLD_OFF_STATUS,
    EVENT_HOLD_ON_REQUIRES_ONE_DURATION_MESSAGE, EVENT_HOLD_ON_STATUS,
    EVENT_HOLD_REQUIRES_MODE_MESSAGE, GOVERNANCE_RETENTION_MODE, INVALID_ARGUMENT_CODE,
    INVALID_REQUEST_CODE, INVALID_REQUEST_MESSAGE, LEGAL_HOLD_OFF_STATUS, LEGAL_HOLD_ON_STATUS,
    OBJECT_LOCK_ACCESS_DENIED_MESSAGE, OBJECT_LOCK_CONFIGURATION_MISSING_ON_WRITE_MESSAGE,
    OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, RETAIN_UNTIL_DATE_UNDER_EVENT_HOLD_FLOOR_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_legal_hold::lock_enabled_bucket;
use super::object_retention::{get_object_retention, put_object_retention};

/// The bytes every write in this suite stores, so a read-back shows the lock did not disturb
/// the payload.
const PAYLOAD: &[u8] = b"locked-payload";

/// How far from now a retention period that is still RUNNING ends, in seconds — an hour,
/// which is long enough that no row's delete can race its expiry.
const ONE_HOUR: i64 = 60 * 60;

/// `HEAD /<bucket>/<key>?versionId=<id>` — `HeadObject` of one stored version.
async fn head_version(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    version_id: &str,
) -> Answer {
    send(
        router,
        "HEAD",
        &format!("/{name}/{key}?versionId={version_id}"),
        &[],
        "",
    )
    .await
}

/// `DELETE /<bucket>/<key>?versionId=<id>` — the PERMANENT delete of one stored version, the
/// request an Object Lock is enforced against, carrying the governance bypass when asked for.
async fn delete_version(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    version_id: &str,
    bypass: bool,
) -> Answer {
    let headers: Vec<(&str, &str)> = if bypass {
        vec![("x-amz-bypass-governance-retention", "true")]
    } else {
        Vec::new()
    };
    send_bytes(
        router,
        "DELETE",
        &format!("/{name}/{key}?versionId={version_id}"),
        &headers,
        b"",
    )
    .await
}

/// `PUT /<bucket>/<key>` carrying `x-amz-copy-source` — `CopyObject`.
async fn copy_object(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    copy_source: &str,
    headers: &[(&str, &str)],
) -> Answer {
    let mut all: Vec<(&str, &str)> = vec![("x-amz-copy-source", copy_source)];
    all.extend_from_slice(headers);
    send_bytes(router, "PUT", &format!("/{name}/{key}"), &all, b"").await
}

/// An instant `seconds` from now, in the spelling the framework's timestamp binding reads a
/// value in — which is what the retain-until-date member carries.
fn timestamp_in(seconds: i64) -> String {
    timestamp_at(now_seconds() + seconds)
}

/// Now, in epoch seconds.
fn now_seconds() -> i64 {
    DateTime::from(std::time::SystemTime::now()).secs()
}

/// `epoch` in that same spelling, so a row that ASSERTS on the instant it sent keeps the one
/// number rather than reading the clock a second time.
fn timestamp_at(epoch: i64) -> String {
    DateTime::from_secs(epoch)
        .fmt(Format::DateTime)
        .expect("an instant renders as a timestamp")
}

/// The epoch seconds a reported timestamp names, read back through the same `date-time`
/// format it is carried in — so a row compares INSTANTS rather than two renderings of one.
fn epoch_seconds(rendered: &str) -> i64 {
    DateTime::from_str(rendered, Format::DateTime)
        .unwrap_or_else(|error| {
            panic!("the reported date {rendered:?} must be an ISO8601 date-time, got {error}")
        })
        .secs()
}

/// Which write carries the Object Lock members a row names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Write {
    /// `PutObject` — the one-shot write.
    Put,
    /// `CopyObject` — the write whose bytes come from another object.
    Copy,
    /// `CreateMultipartUpload` followed by `CompleteMultipartUpload`: the initiation declares
    /// the members and the completion stores them on the assembled version.
    Multipart,
}

/// One row: the case name, the write that carries the members, the retention the write asks
/// for (mode and how far from now its date runs, which is NEGATIVE for a period that has
/// already lapsed), the legal hold status it asks for, and whether a delete of the version
/// without a bypass must be refused.
struct Row {
    case: &'static str,
    write: Write,
    retention: Option<(&'static str, i64)>,
    legal_hold: Option<&'static str>,
    delete_refused: bool,
}

/// Write `PAYLOAD` at `key` through `write`, carrying `headers`, and report the version the
/// write recorded it as.
async fn write_object(
    router: &MultiServiceRouter,
    name: &str,
    key: &str,
    write: Write,
    headers: &[(&str, &str)],
    case: &str,
) -> Answer {
    match write {
        Write::Put => put_object(router, name, key, PAYLOAD, headers).await,
        Write::Copy => {
            let source = "sources/to-copy.bin";
            let stored = put_object(router, name, source, PAYLOAD, &[]).await;
            assert_eq!(
                stored.status, 200,
                "case {case:?}: the copy's source must be written first, got {}",
                stored.status
            );
            copy_object(router, name, key, &format!("{name}/{source}"), headers).await
        }
        Write::Multipart => {
            let initiated = send(router, "POST", &format!("/{name}/{key}?uploads"), headers, "")
                .await;
            if initiated.status != 200 {
                return initiated;
            }
            let upload_id = initiated
                .element("UploadId")
                .expect("an initiation must report an UploadId");
            let uploaded = send_bytes(
                router,
                "PUT",
                &format!(
                    "/{name}/{key}?partNumber=1&uploadId={upload_id}&x-id=UploadPart"
                ),
                &[],
                PAYLOAD,
            )
            .await;
            assert_eq!(
                uploaded.status,
                200,
                "case {case:?}: the part must be uploaded, got {} with body {:?}",
                uploaded.status,
                text(&uploaded)
            );
            let etag = uploaded
                .header("etag")
                .expect("a part upload reports the part's ETag");
            send(
                router,
                "POST",
                &format!("/{name}/{key}?uploadId={upload_id}"),
                &[],
                &format!(
                    "<CompleteMultipartUpload><Part><PartNumber>1</PartNumber>\
                     <ETag>{etag}</ETag></Part></CompleteMultipartUpload>"
                ),
            )
            .await
        }
    }
}

#[tokio::test]
async fn a_write_that_names_an_object_lock_stores_it_on_the_version_it_writes() {
    let router = router();
    let name = "lock-on-write-bucket";
    lock_enabled_bucket(&router, name).await;

    let rows: Vec<Row> = vec![
        Row {
            case: "a governance retention still running, on a one-shot write",
            write: Write::Put,
            retention: Some((GOVERNANCE_RETENTION_MODE, ONE_HOUR)),
            legal_hold: None,
            delete_refused: true,
        },
        Row {
            case: "a compliance retention still running, on a one-shot write",
            write: Write::Put,
            retention: Some((COMPLIANCE_RETENTION_MODE, ONE_HOUR)),
            legal_hold: None,
            delete_refused: true,
        },
        Row {
            case: "a legal hold ON, on a one-shot write",
            write: Write::Put,
            retention: None,
            legal_hold: Some(LEGAL_HOLD_ON_STATUS),
            delete_refused: true,
        },
        Row {
            case: "a legal hold OFF, which protects nothing",
            write: Write::Put,
            retention: None,
            legal_hold: Some(LEGAL_HOLD_OFF_STATUS),
            delete_refused: false,
        },
        Row {
            case: "a retention and a legal hold at once",
            write: Write::Put,
            retention: Some((GOVERNANCE_RETENTION_MODE, ONE_HOUR)),
            legal_hold: Some(LEGAL_HOLD_ON_STATUS),
            delete_refused: true,
        },
        Row {
            case: "a governance retention on a copy",
            write: Write::Copy,
            retention: Some((GOVERNANCE_RETENTION_MODE, ONE_HOUR)),
            legal_hold: None,
            delete_refused: true,
        },
        Row {
            case: "a legal hold on a copy",
            write: Write::Copy,
            retention: None,
            legal_hold: Some(LEGAL_HOLD_ON_STATUS),
            delete_refused: true,
        },
        Row {
            case: "a compliance retention a multipart initiation named",
            write: Write::Multipart,
            retention: Some((COMPLIANCE_RETENTION_MODE, ONE_HOUR)),
            legal_hold: None,
            delete_refused: true,
        },
        Row {
            case: "a legal hold a multipart initiation named",
            write: Write::Multipart,
            retention: None,
            legal_hold: Some(LEGAL_HOLD_ON_STATUS),
            delete_refused: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let key = format!("locked/row-{index}.bin");
        let until = row
            .retention
            .map(|(_, offset)| timestamp_in(offset))
            .unwrap_or_default();
        let mut headers: Vec<(&str, &str)> = Vec::new();
        if let Some((mode, _)) = row.retention {
            headers.push(("x-amz-object-lock-mode", mode));
            headers.push(("x-amz-object-lock-retain-until-date", until.as_str()));
        }
        if let Some(status) = row.legal_hold {
            headers.push(("x-amz-object-lock-legal-hold", status));
        }

        let written = write_object(&router, name, &key, row.write, &headers, case).await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the write must be served, got {} with body {:?}",
            written.status,
            text(&written)
        );
        let version_id = written
            .header("x-amz-version-id")
            .unwrap_or_else(|| panic!("case {case:?}: the write must mint a version id"));

        // What a read of that version reports about its lock: the mode and the date the
        // retention runs to, and the hold status — through `GetObject` and `HeadObject`
        // alike, because both report the same stored version.
        let read = get_object(&router, name, &key, &format!("?versionId={version_id}"), &[]).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: the locked version must read back, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            PAYLOAD,
            "case {case:?}: the lock must leave the payload alone"
        );
        let head = head_version(&router, name, &key, &version_id).await;
        for (reader, answer) in [("GetObject", &read), ("HeadObject", &head)] {
            assert_eq!(
                answer.header("x-amz-object-lock-mode"),
                row.retention.map(|(mode, _)| mode.to_string()),
                "case {case:?}: {reader} must report the mode the write named, got {:?}",
                answer.header("x-amz-object-lock-mode")
            );
            assert_eq!(
                answer.header("x-amz-object-lock-legal-hold"),
                row.legal_hold.map(str::to_string),
                "case {case:?}: {reader} must report the hold the write named, got {:?}",
                answer.header("x-amz-object-lock-legal-hold")
            );
            let reported_until = answer.header("x-amz-object-lock-retain-until-date");
            assert_eq!(
                reported_until.is_some(),
                row.retention.is_some(),
                "case {case:?}: {reader} must report a retain-until date exactly when the \
                 write named a retention, got {reported_until:?}"
            );
        }

        // And what the lock DOES: a permanent delete of the protected version is refused
        // with the published 403 and the version stays, while the same delete of a version
        // nothing protects is carried out.
        let deleted = delete_version(&router, name, &key, &version_id, false).await;
        if row.delete_refused {
            assert_eq!(
                deleted.status,
                403,
                "case {case:?}: a delete of the protected version must be refused, got {} \
                 with body {:?}",
                deleted.status,
                text(&deleted)
            );
            assert_eq!(
                deleted.error_code(),
                ACCESS_DENIED_CODE,
                "case {case:?}: expected the published code, got {:?}",
                deleted.error_code()
            );
            assert_eq!(
                deleted.error_message(),
                OBJECT_LOCK_ACCESS_DENIED_MESSAGE,
                "case {case:?}: expected the published sentence, got {:?}",
                deleted.error_message()
            );
            let survived =
                get_object(&router, name, &key, &format!("?versionId={version_id}"), &[]).await;
            assert_eq!(
                survived.status, 200,
                "case {case:?}: the refused delete must leave the version stored, got {}",
                survived.status
            );
        } else {
            assert_eq!(
                deleted.status,
                204,
                "case {case:?}: a delete of a version nothing protects must be carried out, \
                 got {} with body {:?}",
                deleted.status,
                text(&deleted)
            );
        }

        // The governance bypass reaches a GOVERNANCE period and nothing else: a compliance
        // period and a legal hold are judged independently of it.
        if row.delete_refused {
            let bypassed = delete_version(&router, name, &key, &version_id, true).await;
            let reachable = row.legal_hold != Some(LEGAL_HOLD_ON_STATUS)
                && row
                    .retention
                    .is_none_or(|(mode, _)| mode == GOVERNANCE_RETENTION_MODE);
            let expected = if reachable { 204 } else { 403 };
            assert_eq!(
                bypassed.status, expected,
                "case {case:?}: expected HTTP {expected} for the bypassed delete, got {} \
                 with body {:?}",
                bypassed.status,
                text(&bypassed)
            );
        }
    }
}

/// One row of the refusal table: the case name, the bucket the write is addressed to, the
/// Object Lock headers it carries, and the `<Message>` the refusal must carry (empty where
/// the row pins the code and status alone).
type RefusalRow<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>, &'a str);

#[tokio::test]
async fn a_lock_the_service_cannot_record_is_refused_and_nothing_is_written() {
    let router = router();
    let locked = "lock-refusal-bucket";
    let unlocked = "lock-refusal-plain-bucket";
    lock_enabled_bucket(&router, locked).await;
    bucket(&router, unlocked).await;
    let until = timestamp_in(ONE_HOUR);

    // (case, bucket, headers, the message the refusal must carry). Every row is the same code
    // and status — `InvalidRequest`, 400 — because each names a lock no version can carry:
    // half a retention, or a lock on a bucket that locks nothing.
    let rows: Vec<RefusalRow<'_>> = vec![
        (
            "a mode without the date it runs to",
            locked,
            vec![("x-amz-object-lock-mode", GOVERNANCE_RETENTION_MODE)],
            "",
        ),
        (
            "a date without the mode it runs in",
            locked,
            vec![("x-amz-object-lock-retain-until-date", until.as_str())],
            "",
        ),
        (
            "a retention on a bucket with no Object Lock configuration",
            unlocked,
            vec![
                ("x-amz-object-lock-mode", COMPLIANCE_RETENTION_MODE),
                ("x-amz-object-lock-retain-until-date", until.as_str()),
            ],
            OBJECT_LOCK_CONFIGURATION_MISSING_ON_WRITE_MESSAGE,
        ),
        (
            "a legal hold on a bucket with no Object Lock configuration",
            unlocked,
            vec![("x-amz-object-lock-legal-hold", LEGAL_HOLD_ON_STATUS)],
            OBJECT_LOCK_CONFIGURATION_MISSING_ON_WRITE_MESSAGE,
        ),
    ];

    for (index, (case, name, headers, message)) in rows.iter().enumerate() {
        let key = format!("refused/row-{index}.bin");
        for write in [Write::Put, Write::Copy, Write::Multipart] {
            let key = format!("{key}.{write:?}");
            let refused = write_object(&router, name, &key, write, headers, case).await;
            assert_eq!(
                refused.status,
                400,
                "case {case:?} through {write:?}: expected HTTP 400, got {} with body {:?}",
                refused.status,
                text(&refused)
            );
            assert_eq!(
                refused.error_code(),
                INVALID_REQUEST_CODE,
                "case {case:?} through {write:?}: expected the published code, got {:?}",
                refused.error_code()
            );
            if !message.is_empty() {
                assert_eq!(
                    refused.error_message(),
                    *message,
                    "case {case:?} through {write:?}: expected the published sentence, got \
                     {:?}",
                    refused.error_message()
                );
            }
            // Nothing was written: the key holds nothing at all afterwards.
            let absent = get_object(&router, name, &key, "", &[]).await;
            assert_eq!(
                absent.status, 404,
                "case {case:?} through {write:?}: a refused write must store nothing, got \
                 {}",
                absent.status
            );
        }
    }
}

/// The bound header each piece of an EVENT HOLD arrives in
/// (`smithy_query(query="operation",operation="PutObject")`).
const EVENT_HOLD_HEADER: &str = "x-amz-object-lock-event-hold";
/// See [`EVENT_HOLD_HEADER`].
const EVENT_HOLD_DAYS_HEADER: &str = "x-amz-object-lock-event-hold-duration-days";
/// See [`EVENT_HOLD_HEADER`].
const EVENT_HOLD_YEARS_HEADER: &str = "x-amz-object-lock-event-hold-duration-years";
/// The header the mode of the retention a hold lives in arrives in.
const OBJECT_LOCK_MODE_HEADER: &str = "x-amz-object-lock-mode";
/// How many seconds a day runs for — what an event-hold row's retain-until offset is written
/// in terms of.
const ONE_DAY: i64 = 24 * 60 * 60;

/// The unit a row states its event hold duration in — the two the API states one in, "you
/// cannot specify a duration in both days and years"
/// (`PutObjectRequest$ObjectLockEventHoldDurationDays`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unit {
    /// `x-amz-object-lock-event-hold-duration-days`, reported as `EventHoldDuration$Days`.
    Days,
    /// `x-amz-object-lock-event-hold-duration-years`, reported as `EventHoldDuration$Years`.
    Years,
}

impl Unit {
    /// The request header this unit's count arrives in.
    fn header(self) -> &'static str {
        match self {
            Self::Days => EVENT_HOLD_DAYS_HEADER,
            Self::Years => EVENT_HOLD_YEARS_HEADER,
        }
    }

    /// The other unit's header, which the same hold must NOT report a count in.
    fn other_header(self) -> &'static str {
        match self {
            Self::Days => EVENT_HOLD_YEARS_HEADER,
            Self::Years => EVENT_HOLD_DAYS_HEADER,
        }
    }

    /// The member of `EventHoldDuration` the retention subresource carries this unit in.
    fn member(self) -> &'static str {
        match self {
            Self::Days => "Days",
            Self::Years => "Years",
        }
    }

    /// The other unit, whose member the same hold must not be reported in.
    fn other_unit(self) -> Self {
        match self {
            Self::Days => Self::Years,
            Self::Years => Self::Days,
        }
    }
}

/// One row of the event-hold round-trip table: the case name, the write that carries the
/// members, the retention mode and how far from now its date runs, the hold status the row
/// asks for, and the duration it states (`None` for a row that states none).
struct HoldRow {
    case: &'static str,
    write: Write,
    mode: &'static str,
    retain_until_seconds: i64,
    status: &'static str,
    duration: Option<(Unit, i32)>,
}

/// The `x-amz-object-lock-*` headers one [`HoldRow`] carries, as owned pairs.
fn hold_headers(row: &HoldRow, until: &str, count: &str) -> Vec<(&'static str, String)> {
    let mut headers = vec![
        (OBJECT_LOCK_MODE_HEADER, row.mode.to_string()),
        (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, until.to_string()),
        (EVENT_HOLD_HEADER, row.status.to_string()),
    ];
    if let Some((unit, _)) = row.duration {
        headers.push((unit.header(), count.to_string()));
    }
    headers
}

/// `headers` as the borrowed pairs the request helpers take.
fn borrowed<'a>(headers: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

#[tokio::test]
async fn a_write_that_names_an_event_hold_stores_it_on_the_version_it_writes() {
    let router = router();
    let name = "event-hold-write-bucket";
    lock_enabled_bucket(&router, name).await;

    let rows: Vec<HoldRow> = vec![
        HoldRow {
            case: "a governance retention holding for one day, on a one-shot write",
            write: Write::Put,
            mode: GOVERNANCE_RETENTION_MODE,
            retain_until_seconds: 2 * ONE_DAY,
            status: EVENT_HOLD_ON_STATUS,
            duration: Some((Unit::Days, 1)),
        },
        HoldRow {
            case: "a compliance retention holding for seven days",
            write: Write::Put,
            mode: COMPLIANCE_RETENTION_MODE,
            retain_until_seconds: 30 * ONE_DAY,
            status: EVENT_HOLD_ON_STATUS,
            duration: Some((Unit::Days, 7)),
        },
        HoldRow {
            case: "a hold stated in YEARS rather than days",
            write: Write::Put,
            mode: GOVERNANCE_RETENTION_MODE,
            retain_until_seconds: 2 * 365 * ONE_DAY,
            status: EVENT_HOLD_ON_STATUS,
            duration: Some((Unit::Years, 1)),
        },
        HoldRow {
            case: "a hold the write asks for OFF, which runs for no duration at all",
            write: Write::Put,
            mode: GOVERNANCE_RETENTION_MODE,
            retain_until_seconds: ONE_HOUR,
            status: EVENT_HOLD_OFF_STATUS,
            duration: None,
        },
        HoldRow {
            case: "a hold a COPY asks for",
            write: Write::Copy,
            mode: GOVERNANCE_RETENTION_MODE,
            retain_until_seconds: 10 * ONE_DAY,
            status: EVENT_HOLD_ON_STATUS,
            duration: Some((Unit::Days, 3)),
        },
        HoldRow {
            case: "a hold a multipart INITIATION asks for, stored by the completion",
            write: Write::Multipart,
            mode: COMPLIANCE_RETENTION_MODE,
            retain_until_seconds: 10 * ONE_DAY,
            status: EVENT_HOLD_ON_STATUS,
            duration: Some((Unit::Days, 2)),
        },
        HoldRow {
            case: "a hold a multipart initiation asks for in years",
            write: Write::Multipart,
            mode: GOVERNANCE_RETENTION_MODE,
            retain_until_seconds: 3 * 365 * ONE_DAY,
            status: EVENT_HOLD_ON_STATUS,
            duration: Some((Unit::Years, 2)),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let key = format!("held/row-{index}.bin");
        let until_seconds = now_seconds() + row.retain_until_seconds;
        let until = timestamp_at(until_seconds);
        let count = row
            .duration
            .map(|(_, count)| count.to_string())
            .unwrap_or_default();
        let owned = hold_headers(row, &until, &count);
        let headers = borrowed(&owned);

        let written = write_object(&router, name, &key, row.write, &headers, case).await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the write must be served, got {} with body {:?}",
            written.status,
            text(&written)
        );
        let version_id = written
            .header("x-amz-version-id")
            .unwrap_or_else(|| panic!("case {case:?}: the write must mint a version id"));

        // What a read of that version reports: the retention it is locked under AND the hold
        // inside it, through `GetObject` and `HeadObject` alike, because both report the same
        // stored version.
        let read = get_object(&router, name, &key, &format!("?versionId={version_id}"), &[]).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: the held version must read back, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.body.as_ref(),
            PAYLOAD,
            "case {case:?}: the hold must leave the payload alone"
        );
        let head = head_version(&router, name, &key, &version_id).await;
        // The duration is reported in the ONE unit it was stated in, and only while the hold
        // is enabled.
        let reported_count = row
            .duration
            .filter(|_| row.status == EVENT_HOLD_ON_STATUS)
            .map(|(_, count)| count.to_string());
        let unit = row.duration.map_or(Unit::Days, |(unit, _)| unit);
        for (reader, answer) in [("GetObject", &read), ("HeadObject", &head)] {
            assert_eq!(
                answer.header(OBJECT_LOCK_MODE_HEADER).as_deref(),
                Some(row.mode),
                "case {case:?}: {reader} must report the mode the write named, got {:?}",
                answer.header(OBJECT_LOCK_MODE_HEADER)
            );
            assert_eq!(
                answer
                    .header(OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER)
                    .map(|reported| epoch_seconds(&reported)),
                Some(until_seconds),
                "case {case:?}: {reader} must report the instant the write named, got {:?}",
                answer.header(OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER)
            );
            assert_eq!(
                answer.header(EVENT_HOLD_HEADER).as_deref(),
                Some(row.status),
                "case {case:?}: {reader} must report the hold status the write named, got \
                 {:?}",
                answer.header(EVENT_HOLD_HEADER)
            );
            assert_eq!(
                answer.header(unit.header()),
                reported_count,
                "case {case:?}: {reader} must report the duration the write named, got {:?}",
                answer.header(unit.header())
            );
            assert_eq!(
                answer.header(unit.other_header()),
                None,
                "case {case:?}: {reader} must report no duration in the other unit, got {:?}",
                answer.header(unit.other_header())
            );
        }

        // And the retention subresource reports the same hold, because it is the same record.
        let retention = get_object_retention(
            &router,
            name,
            &key,
            &format!("&versionId={version_id}"),
            &[],
        )
        .await;
        assert_eq!(
            retention.status,
            200,
            "case {case:?}: the retention must read back, got {} with body {:?}",
            retention.status,
            text(&retention)
        );
        assert_eq!(
            retention.element("Mode").as_deref(),
            Some(row.mode),
            "case {case:?}: the retention must report the mode, got {:?}",
            retention.element("Mode")
        );
        assert_eq!(
            retention.element("EventHold").as_deref(),
            Some(row.status),
            "case {case:?}: the retention must report the hold status, got {:?}",
            retention.element("EventHold")
        );
        assert_eq!(
            retention.element(unit.member()),
            reported_count,
            "case {case:?}: the retention must report the duration in {:?}, got {:?}",
            unit,
            retention.element(unit.member())
        );
        assert_eq!(
            retention.element(unit.other_unit().member()),
            None,
            "case {case:?}: the retention must report no duration in the other unit, got \
             {:?}",
            retention.element(unit.other_unit().member())
        );
    }
}

/// The `Retention` container a release or a re-hold carries: the members the row names, and
/// nothing else.
fn retention_payload(
    mode: Option<&str>,
    retain_until_seconds: Option<i64>,
    event_hold: Option<&str>,
    duration: Option<(Unit, i32)>,
) -> String {
    let mut body = String::from("<Retention xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">");
    if let Some(mode) = mode {
        body.push_str(&format!("<Mode>{mode}</Mode>"));
    }
    if let Some(seconds) = retain_until_seconds {
        body.push_str(&format!(
            "<RetainUntilDate>{}</RetainUntilDate>",
            timestamp_in(seconds)
        ));
    }
    if let Some(status) = event_hold {
        body.push_str(&format!("<EventHold>{status}</EventHold>"));
    }
    if let Some((unit, count)) = duration {
        let member = unit.member();
        body.push_str(&format!(
            "<EventHoldDuration><{member}>{count}</{member}></EventHoldDuration>"
        ));
    }
    body.push_str("</Retention>");
    body
}

/// One row of the release table: the case name, the `Retention` container the retention write
/// carries, and the hold status and duration the version must report afterwards.
type ReleaseRow = (&'static str, String, &'static str, Option<(Unit, i32)>);

#[tokio::test]
async fn a_retention_write_changes_the_event_hold_and_keeps_the_period_it_is_a_member_of() {
    let router = router();
    let name = "event-hold-release-bucket";
    lock_enabled_bucket(&router, name).await;

    // Every row starts from the same written state — a GOVERNANCE retention running for a
    // week and holding for a day — so what differs between rows is only the container.
    let rows: Vec<ReleaseRow> = vec![
        (
            "a container naming only the hold, released",
            retention_payload(None, None, Some(EVENT_HOLD_OFF_STATUS), None),
            EVENT_HOLD_OFF_STATUS,
            None,
        ),
        (
            "a container restating the period and releasing the hold",
            retention_payload(
                Some(GOVERNANCE_RETENTION_MODE),
                Some(14 * ONE_DAY),
                Some(EVENT_HOLD_OFF_STATUS),
                None,
            ),
            EVENT_HOLD_OFF_STATUS,
            None,
        ),
        (
            "a container naming only a LONGER hold",
            retention_payload(
                None,
                None,
                Some(EVENT_HOLD_ON_STATUS),
                Some((Unit::Days, 5)),
            ),
            EVENT_HOLD_ON_STATUS,
            Some((Unit::Days, 5)),
        ),
        (
            "a container restating the period and stating the hold in years",
            retention_payload(
                Some(GOVERNANCE_RETENTION_MODE),
                Some(2 * 365 * ONE_DAY),
                Some(EVENT_HOLD_ON_STATUS),
                Some((Unit::Years, 1)),
            ),
            EVENT_HOLD_ON_STATUS,
            Some((Unit::Years, 1)),
        ),
    ];

    for (index, (case, payload, status, duration)) in rows.iter().enumerate() {
        let key = format!("released/row-{index}.bin");
        let until = timestamp_in(7 * ONE_DAY);
        let written = put_object(
            &router,
            name,
            &key,
            PAYLOAD,
            &[
                (OBJECT_LOCK_MODE_HEADER, GOVERNANCE_RETENTION_MODE),
                (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, until.as_str()),
                (EVENT_HOLD_HEADER, EVENT_HOLD_ON_STATUS),
                (EVENT_HOLD_DAYS_HEADER, "1"),
            ],
        )
        .await;
        assert_eq!(
            written.status,
            200,
            "case {case:?}: the held version must be written first, got {} with body {:?}",
            written.status,
            text(&written)
        );
        let version_id = written
            .header("x-amz-version-id")
            .unwrap_or_else(|| panic!("case {case:?}: the write must mint a version id"));

        let put = put_object_retention(
            &router,
            name,
            &key,
            &format!("&versionId={version_id}"),
            &[],
            payload,
        )
        .await;
        assert_eq!(
            put.status,
            200,
            "case {case:?}: the retention write must be served, got {} with body {:?}",
            put.status,
            text(&put)
        );

        let head = head_version(&router, name, &key, &version_id).await;
        // The MODE the version is locked in survives the call, whether the container restated
        // it or not: the hold is a member of that retention, not a replacement for it.
        assert_eq!(
            head.header(OBJECT_LOCK_MODE_HEADER).as_deref(),
            Some(GOVERNANCE_RETENTION_MODE),
            "case {case:?}: the mode must be kept, got {:?}",
            head.header(OBJECT_LOCK_MODE_HEADER)
        );
        assert_eq!(
            head.header(EVENT_HOLD_HEADER).as_deref(),
            Some(*status),
            "case {case:?}: expected the hold status {status:?}, got {:?}",
            head.header(EVENT_HOLD_HEADER)
        );
        let unit = duration.map_or(Unit::Days, |(unit, _)| unit);
        assert_eq!(
            head.header(unit.header()),
            duration.map(|(_, count)| count.to_string()),
            "case {case:?}: expected the duration {duration:?}, got {:?}",
            head.header(unit.header())
        );
        assert_eq!(
            head.header(unit.other_header()),
            None,
            "case {case:?}: no duration may be reported in the other unit, got {:?}",
            head.header(unit.other_header())
        );
        // A released hold leaves the retention itself standing, so the period still protects
        // the version: the retention subresource reports the mode and the date it runs to.
        let retention = get_object_retention(
            &router,
            name,
            &key,
            &format!("&versionId={version_id}"),
            &[],
        )
        .await;
        assert_eq!(
            retention.element("Mode").as_deref(),
            Some(GOVERNANCE_RETENTION_MODE),
            "case {case:?}: the retention must still name the mode, got {:?}",
            retention.element("Mode")
        );
        assert!(
            retention.element("RetainUntilDate").is_some(),
            "case {case:?}: the retention must still name the date it runs to, got {:?}",
            text(&retention)
        );
    }
}

/// One row of the event-hold refusal table: the case name, the headers the write carries, and
/// the error document it must answer with — the code, the `<Message>` where the refusal
/// carries a published sentence of its own (`None` where the row pins the code alone, because
/// the operations answer the cause with their own `InvalidRequest` and its shape's sentence),
/// and the `<ArgumentName>` where the refusal names one.
struct HoldRefusal {
    case: &'static str,
    headers: Vec<(&'static str, String)>,
    code: &'static str,
    message: Option<&'static str>,
    argument: Option<&'static str>,
}

#[tokio::test]
async fn an_event_hold_whose_pieces_do_not_agree_is_refused_and_nothing_is_written() {
    let router = router();
    let name = "event-hold-refusal-bucket";
    lock_enabled_bucket(&router, name).await;

    let covered = timestamp_in(30 * ONE_DAY);
    let under_the_floor = timestamp_in(ONE_DAY);
    let rows: Vec<HoldRefusal> = vec![
        HoldRefusal {
            case: "an enabled hold with no mode for its retention to run in",
            headers: vec![
                (EVENT_HOLD_HEADER, EVENT_HOLD_ON_STATUS.to_string()),
                (EVENT_HOLD_DAYS_HEADER, "1".to_string()),
            ],
            code: INVALID_REQUEST_CODE,
            message: Some(EVENT_HOLD_REQUIRES_MODE_MESSAGE),
            argument: None,
        },
        HoldRefusal {
            case: "a released hold with no mode either",
            headers: vec![(EVENT_HOLD_HEADER, EVENT_HOLD_OFF_STATUS.to_string())],
            code: INVALID_REQUEST_CODE,
            message: Some(EVENT_HOLD_REQUIRES_MODE_MESSAGE),
            argument: None,
        },
        HoldRefusal {
            // A duration is the span the hold it belongs to runs for, so one supplied with no
            // hold status beside it describes a span for a hold nobody asked for: a parameter
            // that is not valid for the request rather than a member to drop.
            case: "a duration in days with no hold status beside it",
            headers: vec![
                (
                    OBJECT_LOCK_MODE_HEADER,
                    GOVERNANCE_RETENTION_MODE.to_string(),
                ),
                (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, covered.clone()),
                (EVENT_HOLD_DAYS_HEADER, "5".to_string()),
            ],
            code: INVALID_REQUEST_CODE,
            message: None,
            argument: None,
        },
        HoldRefusal {
            case: "a duration in years with no hold status and no retention either",
            headers: vec![(EVENT_HOLD_YEARS_HEADER, "2".to_string())],
            code: INVALID_REQUEST_CODE,
            message: None,
            argument: None,
        },
        HoldRefusal {
            case: "an enabled hold with no duration at all",
            headers: vec![
                (
                    OBJECT_LOCK_MODE_HEADER,
                    GOVERNANCE_RETENTION_MODE.to_string(),
                ),
                (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, covered.clone()),
                (EVENT_HOLD_HEADER, EVENT_HOLD_ON_STATUS.to_string()),
            ],
            code: INVALID_REQUEST_CODE,
            message: Some(EVENT_HOLD_ON_REQUIRES_ONE_DURATION_MESSAGE),
            argument: None,
        },
        HoldRefusal {
            case: "an enabled hold with a duration in each unit at once",
            headers: vec![
                (
                    OBJECT_LOCK_MODE_HEADER,
                    COMPLIANCE_RETENTION_MODE.to_string(),
                ),
                (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, covered.clone()),
                (EVENT_HOLD_HEADER, EVENT_HOLD_ON_STATUS.to_string()),
                (EVENT_HOLD_DAYS_HEADER, "1".to_string()),
                (EVENT_HOLD_YEARS_HEADER, "1".to_string()),
            ],
            code: INVALID_REQUEST_CODE,
            message: Some(EVENT_HOLD_ON_REQUIRES_ONE_DURATION_MESSAGE),
            argument: None,
        },
        HoldRefusal {
            case: "a retain-until date a day away under a hold of seven days",
            headers: vec![
                (
                    OBJECT_LOCK_MODE_HEADER,
                    GOVERNANCE_RETENTION_MODE.to_string(),
                ),
                (
                    OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER,
                    under_the_floor.clone(),
                ),
                (EVENT_HOLD_HEADER, EVENT_HOLD_ON_STATUS.to_string()),
                (EVENT_HOLD_DAYS_HEADER, "7".to_string()),
            ],
            code: INVALID_ARGUMENT_CODE,
            message: Some(RETAIN_UNTIL_DATE_UNDER_EVENT_HOLD_FLOOR_MESSAGE),
            argument: Some(OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER),
        },
        HoldRefusal {
            case: "a retain-until date a month away under a hold of one year",
            headers: vec![
                (
                    OBJECT_LOCK_MODE_HEADER,
                    GOVERNANCE_RETENTION_MODE.to_string(),
                ),
                (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, covered.clone()),
                (EVENT_HOLD_HEADER, EVENT_HOLD_ON_STATUS.to_string()),
                (EVENT_HOLD_YEARS_HEADER, "1".to_string()),
            ],
            code: INVALID_ARGUMENT_CODE,
            message: Some(RETAIN_UNTIL_DATE_UNDER_EVENT_HOLD_FLOOR_MESSAGE),
            argument: Some(OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let headers = borrowed(&row.headers);
        for write in [Write::Put, Write::Copy, Write::Multipart] {
            let key = format!("hold-refused/row-{index}-{write:?}.bin");
            let refused = write_object(&router, name, &key, write, &headers, case).await;
            assert_eq!(
                refused.status,
                400,
                "case {case:?} through {write:?}: expected HTTP 400, got {} with body {:?}",
                refused.status,
                text(&refused)
            );
            assert_eq!(
                refused.error_code(),
                row.code,
                "case {case:?} through {write:?}: expected the code {:?}, got {:?}",
                row.code,
                refused.error_code()
            );
            if let Some(message) = row.message {
                assert_eq!(
                    refused.error_message(),
                    message,
                    "case {case:?} through {write:?}: expected the published sentence, got \
                     {:?}",
                    refused.error_message()
                );
            } else {
                assert!(
                    !refused.error_message().is_empty(),
                    "case {case:?} through {write:?}: every S3 error document carries a \
                     message, got {:?}",
                    refused.error_message()
                );
            }
            // The refused VALUE travels in the pair beside the sentence, as the INSTANT it
            // names rather than as anything of this test's own spelling.
            assert_eq!(
                refused.element("ArgumentName").as_deref(),
                row.argument,
                "case {case:?} through {write:?}: expected the argument {:?}, got {:?}",
                row.argument,
                refused.element("ArgumentName")
            );
            if row.argument.is_some() {
                let sent = row
                    .headers
                    .iter()
                    .find(|(name, _)| *name == OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER)
                    .map(|(_, value)| value.clone())
                    .expect("a floor row names a retain-until date");
                let echoed = refused.element("ArgumentValue").unwrap_or_default();
                assert_eq!(
                    epoch_seconds(&echoed),
                    epoch_seconds(&sent),
                    "case {case:?} through {write:?}: expected the refused instant {sent:?}, \
                     got {echoed:?}"
                );
                assert!(
                    echoed.len() == "2026-01-02T03:04:05Z".len()
                        && echoed.contains('T')
                        && echoed.ends_with('Z'),
                    "case {case:?} through {write:?}: the refused date must be the UTC \
                     calendar instant with whole seconds, got {echoed:?}"
                );
            }
            // Nothing was written: the key holds nothing at all afterwards.
            let absent = get_object(&router, name, &key, "", &[]).await;
            assert_eq!(
                absent.status, 404,
                "case {case:?} through {write:?}: a refused write must store nothing, got {}",
                absent.status
            );
        }
    }
}

#[tokio::test]
async fn a_retention_write_whose_hold_does_not_agree_with_its_period_is_refused() {
    let router = router();
    let name = "event-hold-retention-refusal-bucket";
    lock_enabled_bucket(&router, name).await;
    let key = "retention-refused.bin";
    let until = timestamp_in(2 * ONE_DAY);
    let written = put_object(
        &router,
        name,
        key,
        PAYLOAD,
        &[
            (OBJECT_LOCK_MODE_HEADER, GOVERNANCE_RETENTION_MODE),
            (OBJECT_LOCK_RETAIN_UNTIL_DATE_HEADER, until.as_str()),
        ],
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "the version must be written first, got {} with body {:?}",
        written.status,
        text(&written)
    );
    let version_id = written
        .header("x-amz-version-id")
        .expect("the write must mint a version id");

    // (case, the container, the code and the sentence the refusal must carry). Each row names
    // a hold the service cannot place, by the same rules the write path is judged by.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "an enabled hold with no duration",
            retention_payload(
                Some(GOVERNANCE_RETENTION_MODE),
                Some(2 * ONE_DAY),
                Some(EVENT_HOLD_ON_STATUS),
                None,
            ),
            INVALID_REQUEST_CODE,
            EVENT_HOLD_ON_REQUIRES_ONE_DURATION_MESSAGE,
        ),
        (
            "a period of two days under a hold of seven",
            retention_payload(
                Some(GOVERNANCE_RETENTION_MODE),
                Some(2 * ONE_DAY),
                Some(EVENT_HOLD_ON_STATUS),
                Some((Unit::Days, 7)),
            ),
            INVALID_ARGUMENT_CODE,
            RETAIN_UNTIL_DATE_UNDER_EVENT_HOLD_FLOOR_MESSAGE,
        ),
        (
            "a container naming only a hold the period in force cannot cover",
            retention_payload(None, None, Some(EVENT_HOLD_ON_STATUS), Some((Unit::Days, 7))),
            INVALID_ARGUMENT_CODE,
            RETAIN_UNTIL_DATE_UNDER_EVENT_HOLD_FLOOR_MESSAGE,
        ),
        (
            // A duration with no `EventHold` beside it is the span of a hold nobody asked
            // for: this operation declares no error shapes, so the code goes out through the
            // unmodelled channel carrying the sentence the modelled shape states for itself.
            "a container naming a duration with no hold status",
            retention_payload(
                Some(GOVERNANCE_RETENTION_MODE),
                Some(2 * ONE_DAY),
                None,
                Some((Unit::Days, 1)),
            ),
            INVALID_REQUEST_CODE,
            INVALID_REQUEST_MESSAGE,
        ),
    ];

    for (case, payload, code, message) in rows {
        let refused = put_object_retention(
            &router,
            name,
            key,
            &format!("&versionId={version_id}"),
            &[],
            &payload,
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            code,
            "case {case:?}: expected the code {code:?}, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            message,
            "case {case:?}: expected the published sentence, got {:?}",
            refused.error_message()
        );
        // And the version keeps the retention it carried: a refused call records nothing.
        let head = head_version(&router, name, key, &version_id).await;
        assert_eq!(
            head.header(OBJECT_LOCK_MODE_HEADER).as_deref(),
            Some(GOVERNANCE_RETENTION_MODE),
            "case {case:?}: the retention in force must be untouched, got {:?}",
            head.header(OBJECT_LOCK_MODE_HEADER)
        );
        assert_eq!(
            head.header(EVENT_HOLD_HEADER),
            None,
            "case {case:?}: no hold may be recorded by a refused call, got {:?}",
            head.header(EVENT_HOLD_HEADER)
        );
    }

    // A hold named for a version carrying NO retention has no retention to live in, so the
    // container describes a hold the service cannot place: refused with the sentence every
    // call that supplies a hold without the retention it runs in is refused with, and nothing
    // recorded — not answered 200 with the hold dropped, which no later read could tell from
    // a hold that was placed.
    let unretained = "retention-absent.bin";
    let written = put_object(&router, name, unretained, PAYLOAD, &[]).await;
    assert_eq!(
        written.status,
        200,
        "the unretained version must be written first, got {} with body {:?}",
        written.status,
        text(&written)
    );
    let unretained_version = written
        .header("x-amz-version-id")
        .expect("the write must mint a version id");
    for (case, payload) in [
        (
            "an enabled hold-only container on a version carrying no retention",
            retention_payload(None, None, Some(EVENT_HOLD_ON_STATUS), Some((Unit::Days, 1))),
        ),
        (
            "a released hold-only container on a version carrying no retention",
            retention_payload(None, None, Some(EVENT_HOLD_OFF_STATUS), None),
        ),
    ] {
        let refused = put_object_retention(
            &router,
            name,
            unretained,
            &format!("&versionId={unretained_version}"),
            &[],
            &payload,
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: expected HTTP 400, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            INVALID_REQUEST_CODE,
            "case {case:?}: expected the code {INVALID_REQUEST_CODE:?}, got {:?}",
            refused.error_code()
        );
        assert_eq!(
            refused.error_message(),
            EVENT_HOLD_REQUIRES_MODE_MESSAGE,
            "case {case:?}: expected the published sentence, got {:?}",
            refused.error_message()
        );
        // Nothing was recorded: the version still carries no Object Lock at all, and the
        // retention subresource still answers that it has none.
        let head = head_version(&router, name, unretained, &unretained_version).await;
        for header in [EVENT_HOLD_HEADER, OBJECT_LOCK_MODE_HEADER, EVENT_HOLD_DAYS_HEADER] {
            assert_eq!(
                head.header(header),
                None,
                "case {case:?}: a refused call must record nothing, got {header} {:?}",
                head.header(header)
            );
        }
        let retention = get_object_retention(
            &router,
            name,
            unretained,
            &format!("&versionId={unretained_version}"),
            &[],
        )
        .await;
        assert_eq!(
            retention.status, 404,
            "case {case:?}: a version with no retention must answer the \
             subresource-not-there 404, got {}",
            retention.status
        );
    }
}
