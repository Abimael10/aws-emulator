// SPDX-License-Identifier: CC0-1.0

//! Object Lock keeps a bucket VERSIONED: end-to-end cover for what
//! `PutBucketVersioning` does to a bucket that carries an Object Lock
//! configuration.
//!
//! Every assertion drives a real HTTP request through the router (the harness is
//! the one [`super::bucket_lifecycle`] owns), so what is pinned is the wire
//! behaviour — status, error code, message — rather than any handler's
//! internals.
//!
//! The rule under test: "Object Lock works only in versioned buckets" (Amazon S3
//! User Guide, *Using S3 Object Lock*) and a lock is placed for the life of the
//! bucket, so a bucket carrying an Object Lock configuration cannot leave the
//! `Enabled` versioning state: a `PutBucketVersioning` that would move it out of
//! that state is refused HTTP 409 `InvalidBucketState` with that condition's
//! fixed sentence and changes nothing, while the same request against a bucket
//! with no lock is served and a request naming the state the locked bucket is
//! already in changes nothing and is served.
//!
//! The rows differ along what the rule generalises over: how the bucket came to
//! carry its configuration (the create's own `x-amz-bucket-object-lock-enabled`
//! header, and `PutObjectLockConfiguration` placed on a versioned bucket, with
//! and without a default retention rule), which status the put names (both values
//! `BucketVersioningStatus` publishes), and whether the configuration is there at
//! all.

use service_com_amazonaws_s3_amazons3::ops::types::BUCKET_VERSIONING_STATUS_VALUES;
use service_com_amazonaws_s3_amazons3::state::{
    COMPLIANCE_RETENTION_MODE, INVALID_BUCKET_STATE_CODE, OBJECT_LOCK_ENABLED_STATUS,
    OBJECT_LOCK_VERSIONING_IMMUTABLE_MESSAGE, VERSIONING_ENABLED_STATUS,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send, Answer};
use super::object_versioning::set_versioning;

/// The `BucketVersioningStatus` value that takes a bucket OUT of the versioning
/// state Object Lock requires — the other of the two the model publishes
/// (asserted against that catalog below).
const VERSIONING_SUSPENDED_STATUS: &str = "Suspended";

/// How a bucket came to carry its Object Lock configuration, or that it carries
/// none.
#[derive(Debug, Clone, Copy)]
enum Lock {
    /// No Object Lock configuration at all.
    None,
    /// `CreateBucket` carrying `x-amz-bucket-object-lock-enabled` — "specifies
    /// whether you want S3 Object Lock to be enabled for the new bucket".
    AtCreate,
    /// `PutObjectLockConfiguration` on a versioning-enabled bucket, naming no
    /// default retention rule.
    PutWithoutRule,
    /// The same put, naming a default retention rule.
    PutWithRule,
}

/// One row: the case name, how the bucket is locked, the status the put names,
/// and whether the put is served.
struct Row {
    case: &'static str,
    lock: Lock,
    status: &'static str,
    served: bool,
}

/// The table every row of which the rule decides.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bucket with no lock, suspended",
            lock: Lock::None,
            status: VERSIONING_SUSPENDED_STATUS,
            served: true,
        },
        Row {
            case: "a bucket with no lock, enabled",
            lock: Lock::None,
            status: VERSIONING_ENABLED_STATUS,
            served: true,
        },
        Row {
            case: "a bucket locked at create, suspended",
            lock: Lock::AtCreate,
            status: VERSIONING_SUSPENDED_STATUS,
            served: false,
        },
        Row {
            case: "a bucket locked at create, enabled again",
            lock: Lock::AtCreate,
            status: VERSIONING_ENABLED_STATUS,
            served: true,
        },
        Row {
            case: "a bucket whose configuration names no rule, suspended",
            lock: Lock::PutWithoutRule,
            status: VERSIONING_SUSPENDED_STATUS,
            served: false,
        },
        Row {
            case: "a bucket whose configuration names a rule, suspended",
            lock: Lock::PutWithRule,
            status: VERSIONING_SUSPENDED_STATUS,
            served: false,
        },
        Row {
            case: "a bucket whose configuration names a rule, enabled again",
            lock: Lock::PutWithRule,
            status: VERSIONING_ENABLED_STATUS,
            served: true,
        },
    ]
}

/// The body of the response as text, for an assertion message.
fn text(answer: &Answer) -> String {
    answer.element("Message").unwrap_or_default()
}

/// `PUT /<bucket>?versioning` with `status`, whatever the answer.
async fn put_versioning(router: &MultiServiceRouter, bucket: &str, status: &str) -> Answer {
    let payload = format!(
        "<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
         <Status>{status}</Status></VersioningConfiguration>"
    );
    send(
        router,
        "PUT",
        &format!("/{bucket}?versioning"),
        &[],
        &payload,
    )
    .await
}

/// `GET /<bucket>?versioning` — the `Status` it reports, or `"<unset>"`.
async fn versioning_status(router: &MultiServiceRouter, bucket: &str) -> String {
    let read = send(router, "GET", &format!("/{bucket}?versioning"), &[], "").await;
    assert_eq!(
        read.status, 200,
        "GetBucketVersioning of {bucket:?} must answer 200, got {}",
        read.status
    );
    read.element("Status").unwrap_or_else(|| "<unset>".to_string())
}

/// Create the bucket a row needs, in the state its `Lock` describes.
async fn bucket_for(router: &MultiServiceRouter, case: &str, name: &str, lock: Lock) {
    let headers: &[(&str, &str)] = match lock {
        Lock::AtCreate => &[("x-amz-bucket-object-lock-enabled", "true")],
        _ => &[],
    };
    let created = send(router, "PUT", &format!("/{name}"), headers, "").await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: the bucket must be created, got {} with {:?}",
        created.status,
        text(&created)
    );
    let rule = match lock {
        Lock::None | Lock::AtCreate => return,
        Lock::PutWithoutRule => String::new(),
        Lock::PutWithRule => format!(
            "<Rule><DefaultRetention><Mode>{COMPLIANCE_RETENTION_MODE}</Mode><Days>30</Days>\
             </DefaultRetention></Rule>"
        ),
    };
    // "Object Lock works only in versioned buckets", so the put is placed on a
    // bucket that is already versioning-enabled.
    set_versioning(router, name, VERSIONING_ENABLED_STATUS).await;
    let placed = send(
        router,
        "PUT",
        &format!("/{name}?object-lock"),
        &[],
        &format!(
            "<ObjectLockConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <ObjectLockEnabled>{OBJECT_LOCK_ENABLED_STATUS}</ObjectLockEnabled>{rule}\
             </ObjectLockConfiguration>"
        ),
    )
    .await;
    assert_eq!(
        placed.status, 200,
        "case {case:?}: the Object Lock configuration must be placed, got {} with {:?}",
        placed.status,
        text(&placed)
    );
}

/// A bucket carrying an Object Lock configuration cannot be moved out of the
/// `Enabled` versioning state; one with no configuration takes either state.
#[tokio::test]
async fn an_object_lock_configuration_holds_the_bucket_in_the_enabled_versioning_state() {
    // The statuses the rows name are the whole catalog the model publishes, so
    // no third state is left untested.
    assert_eq!(
        BUCKET_VERSIONING_STATUS_VALUES,
        &[VERSIONING_ENABLED_STATUS, VERSIONING_SUSPENDED_STATUS],
        "the rows must span BucketVersioningStatus, got {BUCKET_VERSIONING_STATUS_VALUES:?}"
    );

    for (index, row) in rows().into_iter().enumerate() {
        let router = router();
        let bucket = format!("lock-versioning-{index}");
        bucket_for(&router, row.case, &bucket, row.lock).await;
        let before = versioning_status(&router, &bucket).await;

        let answered = put_versioning(&router, &bucket, row.status).await;
        if row.served {
            assert_eq!(
                answered.status, 200,
                "case {:?}: expected PutBucketVersioning 200, got {} with {:?}",
                row.case,
                answered.status,
                text(&answered)
            );
            assert_eq!(
                versioning_status(&router, &bucket).await,
                row.status.to_string(),
                "case {:?}: the served put must record the status it named",
                row.case
            );
            continue;
        }
        assert_eq!(
            (
                answered.status,
                answered.error_code(),
                answered.error_message(),
            ),
            (
                409,
                INVALID_BUCKET_STATE_CODE.to_string(),
                OBJECT_LOCK_VERSIONING_IMMUTABLE_MESSAGE.to_string(),
            ),
            "case {:?}: expected the Object Lock versioning refusal, got {} with {:?}",
            row.case,
            answered.status,
            answered.error_message()
        );
        // The refusal changed nothing: the bucket is still in the state Object
        // Lock requires.
        assert_eq!(
            versioning_status(&router, &bucket).await,
            before,
            "case {:?}: a refused put must leave the versioning state alone",
            row.case
        );
        assert_eq!(
            before,
            VERSIONING_ENABLED_STATUS.to_string(),
            "case {:?}: a locked bucket must report versioning enabled",
            row.case
        );
    }
}
