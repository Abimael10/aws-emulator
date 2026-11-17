// SPDX-License-Identifier: CC0-1.0

//! What each phase of a running task execution DOES, and what its work established.
//!
//! A task execution is not a timer. `PREPARING` resolves both of the task's locations
//! and finds what they hold, `TRANSFERRING` moves the objects the task's `Options`
//! select into the destination, and `VERIFYING` compares the two sides — and what each
//! phase found is exactly what `DescribeTaskExecution` reports: its per-phase
//! `PhaseStatus`, the file and byte counters, and the `ErrorCode` of a phase that
//! failed.
//!
//! Both sides of a transfer between Amazon S3 locations are resources of THIS
//! emulator's own object store, and the bucket access role each location names is a
//! resource of its own IAM, so every phase here is grounded in them rather than in a
//! value this module makes up. Both are reached the way any emulated service reaches
//! another — an AWS SDK client over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), which opens no socket and
//! reaches nothing outside this process — through the one shared construction of each
//! hop ([`service_common::s3_client`], [`service_common::iam_role`]).
//!
//! Nothing here sleeps or runs in the background. A phase's work runs when the clock has
//! already made that phase due (its span is behind the execution), which is the one
//! write a reporting operation is allowed to materialise: the same work was owed at the
//! same instant whether or not anybody called.
//!
//! A phase runs only once every phase before it is RECORDED, so the steps of one
//! execution always happen in order — a later step can never report an outcome reached
//! before an earlier one did its work, however many callers are advancing the same
//! execution at the same time. Each phase's work is also repeatable: it is derived from
//! what the two locations hold when it runs and SETS its own counters rather than adding
//! to them, so a pass interrupted before it recorded anything leaves the phase to the
//! next pass, and two passes that run one phase at once reach the same record.
//!
//! What the object store or IAM did not ANSWER establishes nothing: a location whose bucket
//! this emulator holds nothing under, and a role its IAM holds no record of, leave the
//! execution transferring nothing rather than failing it, because neither is evidence
//! about the transfer.

use std::collections::BTreeMap;
use std::time::SystemTime;

use ::smithy_server_runtime::EmbeddedEmulator;

use super::{
    ComAmazonawsDatasyncFmrsserviceState, ExecutionCounters, PhaseStatus, DEFAULT_REGION,
};
use crate::ops::types::{FilterRule, Options};

/// The phase that resolves the locations and sizes up the work.
const PREPARE_PHASE: u8 = 0;

/// The phase that moves the objects.
const TRANSFER_PHASE: u8 = 1;

/// The most objects one phase enumerates on one side of a transfer.
///
/// A RESOURCE GUARD, not an AWS-published quota: every object a phase enumerates is held
/// in memory while that phase runs, so a location holding more than this is transferred
/// up to this many objects rather than without bound.
const MAX_OBJECTS_PER_SIDE: usize = 1_000;

/// The `ObjectMetadata` and `ObjectTags` value that carries none of the source object's
/// metadata across. Both members are documented with the same pair of values: `PRESERVE`,
/// which is the default behaviour, and this one.
const PRESERVE_NONE: &str = "NONE";

/// The user-metadata entry a transfer into an object store carries whatever the task's
/// `ObjectMetadata` says: "even if you specify NONE, metadata for the last modified time
/// is still preserved" (`Options$ObjectMetadata`).
///
/// An object store takes no last-modified time on a write — the destination object's own
/// is when the transfer wrote it — so the SOURCE object's modification time is carried as
/// the user-metadata entry a client reads it back from, `x-amz-meta-last-modified`. Its
/// value is that modification time in the unit a modification time is KEPT in, seconds
/// since the epoch ([`last_modified_metadata_value`]), which is what makes the entry the
/// preserved time of the source rather than a rendering of it.
const LAST_MODIFIED_METADATA_KEY: &str = "last-modified";

/// The S3 action DataSync performs under a location's bucket access role before it reads
/// or writes anything there: it lists the bucket, which is the permission
/// `CreateLocationS3`'s own access test and every execution's `PREPARING` step need.
pub const BUCKET_LIST_ACTION: &str = "s3:ListBucket";

/// The `ErrorCode` an execution reports when it could not reach one of its task's
/// locations: the bucket access role the location names is not allowed to list the
/// bucket, so the step that needs the listing cannot run.
const LOCATION_ACCESS_ERROR_CODE: &str = "FsS3UnableToConnectToEndpoint";

/// The `ErrorCode` an execution reports when it transferred what it was configured to
/// transfer and verification then found the destination does not match the source — what
/// `OverwriteMode NEVER` produces whenever the destination already holds different
/// content.
const VERIFICATION_ERROR_CODE: &str = "CompleteWithErrorNoLogsWithVerificationError";

/// That error's `ErrorDetail`, which names what a client can do to see WHICH files
/// mismatched: the per-file list is a task log, and this task publishes none — which is
/// also what the `NoLogs` in the code above says.
const VERIFICATION_ERROR_DETAIL: &str =
    "Transfer and verification completed. Verification detected mismatches. To see a list of \
     files with mismatches in CloudWatch logs, provide a log group when creating or editing a \
     task.";

/// The `TransferMode` that copies everything without comparing the two sides.
const TRANSFER_MODE_ALL: &str = "ALL";

/// The `OverwriteMode` that leaves data already at the destination alone.
const OVERWRITE_MODE_NEVER: &str = "NEVER";

/// The `PreserveDeletedFiles` value that deletes destination data the source no longer
/// holds.
const PRESERVE_DELETED_FILES_REMOVE: &str = "REMOVE";

/// The `VerifyMode` that performs no verification at all.
const VERIFY_MODE_NONE: &str = "NONE";

/// The `VerifyMode` that verifies only what this execution transferred.
const VERIFY_MODE_ONLY_FILES_TRANSFERRED: &str = "ONLY_FILES_TRANSFERRED";

/// The `TaskMode` whose counters include `FilesPrepared`.
const TASK_MODE_ENHANCED: &str = "ENHANCED";

/// The only `FilterType` the model declares, and so the only one a filter rule's
/// patterns are read under.
const SIMPLE_PATTERN: &str = "SIMPLE_PATTERN";

/// Run every phase of every execution the clock has already made due.
///
/// Called by each operation that reports or acts on a task or an execution, with that
/// handler's own engine handle and the in-process callback the phases reach the object
/// store and IAM through. One phase runs per pass of the loop, and the phase that runs
/// is always the first one of its execution that has not been recorded yet, so the
/// steps of an execution happen in order whoever drives them.
pub async fn advance_transfers<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &EmbeddedEmulator,
    now: SystemTime,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    loop {
        let next = {
            let mut store = state.lock();
            store.advance_clock(now);
            store.next_due_phase(now)
        };
        let Some(due) = next else { return };
        let outcome = run_phase(callback, &due).await;
        let mut store = state.lock();
        store.record_phase(&due, outcome);
        store.advance_clock(now);
    }
}

/// Whether IAM ANSWERED that the role `role_arn` names may not list the bucket
/// `bucket_arn`.
///
/// The one question both `CreateLocationS3`'s access test and an execution's
/// `PREPARING` step put, so a location that was created and an execution that runs
/// against it agree about what the role allows. `false` for everything that is not such
/// an answer — a value that names no role of this account, no IAM to ask, a role this
/// account's IAM holds no record of — because none of those is evidence that the role
/// withholds anything ([`service_common::iam_role::role_withholds_action_through`]).
pub async fn bucket_listing_withheld(
    callback: &EmbeddedEmulator,
    region: &str,
    role_arn: &str,
    bucket_arn: &str,
) -> bool {
    ::service_common::iam_role::role_withholds_action_through(
        callback,
        region,
        role_arn,
        BUCKET_LIST_ACTION,
        bucket_arn,
    )
    .await
}

/// Whether Amazon S3 ANSWERED that it holds no bucket under the name `bucket_arn`
/// addresses.
///
/// The other half of the pre-flight `CreateLocationS3` makes before it stores a location:
/// the location is a transfer endpoint on a bucket, so a bucket the object store reports
/// is not there is one no transfer could ever read or write, and the create is refused.
/// It is asked with `HeadBucket`, the operation Amazon S3 documents for exactly this
/// ("You can use this operation to determine if a bucket exists"), through the one shared
/// construction of the hop ([`service_common::s3_client`]).
///
/// Asked only of an `S3BucketArn` that names a BUCKET (`arn:<partition>:s3:::<bucket>`):
/// the member's other two forms address an S3 access point, which is a resource of its
/// own that this reading says nothing about. `false` — refuse nothing — for every answer
/// that is NOT the object store reporting the bucket absent, for the reason the module
/// docs give: an answer this emulator could not read, and any failure of the hop at all,
/// including an emulator with no object store mounted.
pub async fn bucket_is_absent(
    callback: &EmbeddedEmulator,
    region: &str,
    bucket_arn: &str,
) -> bool {
    let Some(bucket) = super::s3_plain_bucket_name(bucket_arn) else {
        return false;
    };
    let answer = ::service_common::s3_client::s3_client(callback, region)
        .head_bucket()
        .bucket(bucket)
        .send()
        .await;
    match answer {
        Ok(_) => false,
        Err(error) => error.as_service_error().is_some_and(
            ::aws_sdk_s3::operation::head_bucket::HeadBucketError::is_not_found,
        ),
    }
}

/// One side of a transfer between Amazon S3 locations, as a phase addresses it.
#[derive(Debug, Clone)]
pub struct S3Endpoint {
    /// The bucket or access point name the location's `S3BucketArn` addresses.
    pub bucket: String,
    /// That ARN itself — what an IAM policy for this bucket names.
    pub bucket_arn: String,
    /// The location's subdirectory as a key prefix: empty, or ending in `/`.
    pub prefix: String,
    /// `S3Config.BucketAccessRoleArn`: the role DataSync acts under here.
    pub role_arn: String,
}

/// One phase of one execution that is due to run, with everything its work needs read
/// out of the store under one lock.
#[derive(Debug)]
pub struct DuePhase {
    execution_arn: String,
    /// Which phase this is: [`PREPARE_PHASE`], [`TRANSFER_PHASE`] or the verify phase.
    phase: u8,
    /// The Region this execution's hops into the object store and IAM are addressed to:
    /// the one its task's ARN carries.
    region: String,
    /// The options in effect for this run.
    options: Options,
    task_mode: String,
    includes: Vec<FilterRule>,
    excludes: Vec<FilterRule>,
    source: Option<S3Endpoint>,
    destination: Option<S3Endpoint>,
    /// The counters the phases before this one established.
    counters: ExecutionCounters,
}

/// What one phase's work established.
#[derive(Debug)]
pub struct PhaseOutcome {
    status: PhaseStatus,
    error_code: Option<String>,
    error_detail: Option<String>,
    counters: ExecutionCounters,
}

impl PhaseOutcome {
    /// The phase did its work.
    fn succeeded(counters: ExecutionCounters) -> Self {
        Self {
            status: PhaseStatus::Success,
            error_code: None,
            error_detail: None,
            counters,
        }
    }

    /// The phase failed, which ends the execution: the code and detail are what
    /// `Result.ErrorCode`/`Result.ErrorDetail` then report.
    fn failed(counters: ExecutionCounters, error_code: &str, error_detail: String) -> Self {
        Self {
            status: PhaseStatus::Error,
            error_code: Some(error_code.to_string()),
            error_detail: Some(error_detail),
            counters,
        }
    }
}

impl ComAmazonawsDatasyncFmrsserviceState {
    /// The next phase that is due to run, if any execution has one.
    ///
    /// The phase is the first one of its execution whose outcome is not recorded yet, and
    /// it is due only once the clock has carried the execution past the end of its span.
    /// An execution whose task or locations this service no longer holds still runs its
    /// phases — they have no work to do, and an execution that could never finish would
    /// never leave its task's `RUNNING` status.
    fn next_due_phase(&self, now: SystemTime) -> Option<DuePhase> {
        let execution = self.task_executions.values().find(|execution| {
            execution.progress.failed_phase().is_none()
                && execution.progress.settled() < execution.phases_due(now)
        })?;
        let task = self.tasks.get(&execution.task_arn);
        let region = ::service_common::arn::region_of(&execution.task_arn, "datasync")
            .unwrap_or(DEFAULT_REGION)
            .to_string();
        Some(DuePhase {
            execution_arn: execution.arn.clone(),
            phase: execution.progress.settled(),
            region,
            options: execution.options.clone(),
            task_mode: execution.task_mode.clone(),
            includes: execution.includes.clone(),
            excludes: execution.excludes.clone(),
            source: task.and_then(|task| self.s3_endpoint(&task.source_location_arn)),
            destination: task.and_then(|task| self.s3_endpoint(&task.destination_location_arn)),
            counters: execution.counters,
        })
    }

    /// Record what a phase established, unless another pass of that same phase already
    /// did — the two reach the same record, so the first one to land settles it.
    fn record_phase(&mut self, due: &DuePhase, outcome: PhaseOutcome) {
        let Some(execution) = self.task_executions.get_mut(&due.execution_arn) else {
            return;
        };
        if execution.progress.settled() != due.phase {
            return;
        }
        execution.progress.record(due.phase, outcome.status);
        execution.counters = outcome.counters;
        if outcome.status == PhaseStatus::Error {
            execution.progress.error_code = outcome.error_code;
            execution.progress.error_detail = outcome.error_detail;
        }
    }

    /// The Amazon S3 side of a transfer the location `location_arn` names, when it names
    /// an Amazon S3 location this service holds.
    fn s3_endpoint(&self, location_arn: &str) -> Option<S3Endpoint> {
        let (location, config) = self.s3_location(location_arn).ok()?;
        let prefix = location
            .subdirectory
            .as_deref()
            .unwrap_or_default()
            .trim_matches('/');
        Some(S3Endpoint {
            bucket: super::s3_bucket_name(&config.bucket_arn),
            bucket_arn: config.bucket_arn.clone(),
            prefix: if prefix.is_empty() {
                String::new()
            } else {
                format!("{prefix}/")
            },
            role_arn: config.s3_config.bucket_access_role_arn.clone(),
        })
    }
}

/// One object a phase found on one side of the transfer.
#[derive(Debug, Clone)]
struct ListedObject {
    /// The key relative to the location's own prefix — what the two sides are matched
    /// on, since each side keeps its objects under its own subdirectory.
    relative_key: String,
    /// The object's key in its bucket.
    key: String,
    size: i64,
    /// The entity tag the object store derived from the object's bytes, so comparing it
    /// compares CONTENT rather than metadata.
    etag: String,
    /// When the side holding it reports it was last modified, as the value a transfer
    /// carries across in the destination object's [`LAST_MODIFIED_METADATA_KEY`] entry —
    /// or `None` when the listing reported no time for it.
    last_modified: Option<String>,
}

impl ListedObject {
    /// Whether `other` holds the same content as this object.
    fn matches(&self, other: &ListedObject) -> bool {
        self.size == other.size && self.etag == other.etag
    }
}

/// What one side of the transfer holds, by key relative to that side's prefix.
type Listing = BTreeMap<String, ListedObject>;

/// Run the phase `due` names.
async fn run_phase(callback: &EmbeddedEmulator, due: &DuePhase) -> PhaseOutcome {
    match due.phase {
        PREPARE_PHASE => prepare(callback, due).await,
        TRANSFER_PHASE => transfer(callback, due).await,
        _ => verify(callback, due).await,
    }
}

/// `PREPARING`: resolve the two locations and size up the work.
///
/// DataSync acts under each location's bucket access role, so a role this account's IAM
/// withholds the bucket listing from fails this step and the transfer never starts.
/// Otherwise the step reports what it expects to move: the objects the task's filters
/// admit and its `TransferMode` selects, less the ones its `OverwriteMode` will leave
/// alone, and the destination objects a task configured to remove them will delete.
async fn prepare(callback: &EmbeddedEmulator, due: &DuePhase) -> PhaseOutcome {
    let mut counters = due.counters;
    for endpoint in [due.source.as_ref(), due.destination.as_ref()]
        .into_iter()
        .flatten()
    {
        if bucket_listing_withheld(
            callback,
            &due.region,
            &endpoint.role_arn,
            &endpoint.bucket_arn,
        )
        .await
        {
            return PhaseOutcome::failed(
                counters,
                LOCATION_ACCESS_ERROR_CODE,
                format!(
                    "Could not perform {BUCKET_LIST_ACTION} on bucket {bucket}. Access denied. \
                     Ensure that the bucket access role {role} is allowed to list the bucket.",
                    bucket = endpoint.bucket,
                    role = endpoint.role_arn,
                ),
            );
        }
    }

    let (source, destination) = listings(callback, due).await;
    let admitted = admitted(due, &source);
    let selected = selected(due, &admitted, &destination);
    let writable = writable_by_overwrite_mode(due, &selected, &destination);
    counters.estimated_files_to_transfer = writable.len() as i64;
    counters.estimated_bytes_to_transfer = writable.iter().map(|object| object.size).sum();
    counters.estimated_files_to_delete = deletable(due, &source, &destination).len() as i64;
    // "The number of files or objects that DataSync will attempt to transfer after
    // comparing your source and destination locations. Applies only to Enhanced mode
    // tasks" (`FilesPrepared`), and not applicable at all when the task transfers
    // everything without comparing.
    counters.files_prepared = if due.task_mode == TASK_MODE_ENHANCED
        && transfer_mode(due) != TRANSFER_MODE_ALL
    {
        writable.len() as i64
    } else {
        0
    };
    PhaseOutcome::succeeded(counters)
}

/// `TRANSFERRING`: move the objects the task selects into the destination.
///
/// A transfer READS each selected object off the source side and WRITES it into the
/// destination — it is not an object-store copy of the source's record — so the
/// destination object is a write of its own: it carries the destination bucket's
/// encryption, its own modification time and the checksum the object store computes for a
/// write that names no algorithm, never the source object's integrity record. What it
/// carries ACROSS is what a transfer moves, "the data (including metadata)"
/// (`Options$TransferMode`): the source object's content type and `x-amz-meta-*` user
/// metadata unless the task's `ObjectMetadata` is `NONE`, its tag set unless `ObjectTags`
/// is `NONE`, and a [`LAST_MODIFIED_METADATA_KEY`] entry either way. Every byte counter is
/// the number of bytes that moved. An admitted file this transfer did not have to move —
/// one the destination already holds identically, or one the task's `OverwriteMode`
/// protects — is counted as skipped and not written, and a task configured not to preserve
/// deleted files deletes the destination objects its source no longer holds.
async fn transfer(callback: &EmbeddedEmulator, due: &DuePhase) -> PhaseOutcome {
    let mut counters = due.counters;
    let (source, destination) = listings(callback, due).await;
    let (Some(source_endpoint), Some(destination_endpoint)) =
        (due.source.as_ref(), due.destination.as_ref())
    else {
        return PhaseOutcome::succeeded(counters);
    };
    let admitted = admitted(due, &source);
    let selected = selected(due, &admitted, &destination);
    let writable = writable_by_overwrite_mode(due, &selected, &destination);

    // Counted here and ASSIGNED below, so what this phase records is the work of this
    // pass over the two locations rather than a running total.
    let mut files_transferred = 0;
    let mut bytes_transferred = 0;
    let mut bytes_written = 0;
    let mut files_deleted = 0;

    let client = ::service_common::s3_client::s3_client(callback, &due.region);
    for object in &writable {
        let Some(read) = source_object(&client, due, source_endpoint, object).await else {
            // An object the source would not answer about is one this transfer did not
            // move, and nothing here reports bytes it did not move.
            continue;
        };
        let SourceObject {
            body,
            content_type,
            user_metadata,
            tags,
        } = read;
        let key = format!("{}{}", destination_endpoint.prefix, object.relative_key);
        let moved = i64::try_from(body.len()).unwrap_or(i64::MAX);
        // The media type this write DECLARES: the source object's where the task preserves
        // object metadata, and none at all where it ignores it — a write that declares none
        // leaves the object store to record its own default for the destination object.
        let declared_content_type =
            preserves_object_metadata(due).then_some(content_type).flatten();
        let metadata = destination_metadata(due, object, &user_metadata);
        let declares_a_content_type = declared_content_type.is_some();
        // The one request that moves the object: a WRITE of the bytes this transfer read,
        // stating in full the metadata it carries across — the content type and the user
        // metadata where the task preserves object metadata, the last-modified entry
        // whatever it says, and the tag set only where the task preserves object tags.
        let written = client
            .put_object()
            .bucket(&destination_endpoint.bucket)
            .key(&key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(body))
            .set_content_type(declared_content_type)
            .set_metadata(Some(metadata))
            .set_tagging(tagging_header(&tags))
            .customize()
            // The transport would otherwise declare a media type of its own for the bytes
            // of any write carrying a payload, so a write that declares NONE has to send
            // no `Content-Type` at all for the object store to record its own default.
            .mutate_request(move |request| {
                if !declares_a_content_type {
                    request.headers_mut().remove("content-type");
                }
            })
            .send()
            .await;
        if let Err(error) = written {
            ::tracing::debug!(
                error = ?error,
                bucket = %destination_endpoint.bucket,
                "a transfer could not write an object to the destination location"
            );
            continue;
        }
        files_transferred += 1;
        bytes_transferred += moved;
        bytes_written += moved;
    }

    for object in deletable(due, &source, &destination) {
        let removed = client
            .delete_object()
            .bucket(&destination_endpoint.bucket)
            .key(&object.key)
            .send()
            .await;
        if removed.is_ok() {
            files_deleted += 1;
        }
    }

    // Every file the task admits that this transfer did not have to move is one it
    // SKIPPED: the two reasons a transfer leaves an admitted file where it is are that
    // the destination already holds it identically (what `TransferMode CHANGED` compares
    // for) and that the task's `OverwriteMode` protects the copy already there. Both are
    // the same thing to an observer — a file this run did not move and did not need to.
    // A file it DID have to move is therefore not skipped, whether or not the move
    // succeeded: an object the source would not answer about is reported as neither
    // transferred nor skipped, for the reason the module docs give — an answer this
    // emulator did not get establishes nothing about that file.
    counters.files_skipped = admitted.len().saturating_sub(writable.len()) as i64;
    counters.files_transferred = files_transferred;
    counters.bytes_transferred = bytes_transferred;
    counters.bytes_written = bytes_written;
    counters.files_deleted = files_deleted;
    PhaseOutcome::succeeded(counters)
}

/// `VERIFYING`: check the integrity of the data at the end of the transfer, the way the
/// task's `VerifyMode` asks for.
///
/// `NONE` verifies nothing. `ONLY_FILES_TRANSFERRED` compares the data this execution
/// transferred, which this engine wrote from the bytes it read in the same process, so
/// those files are verified by construction. `POINT_IN_TIME_CONSISTENT` "checks the
/// entire source and destination to verify that both locations are fully synchronized" —
/// so an object the transfer left behind, which is what `OverwriteMode NEVER` does
/// whenever the destination already holds different content, is a mismatch verification
/// legitimately reports, and the execution ends in error.
async fn verify(callback: &EmbeddedEmulator, due: &DuePhase) -> PhaseOutcome {
    let mut counters = due.counters;
    let verify_mode = due.options.verify_mode.as_deref().unwrap_or_default();
    if verify_mode == VERIFY_MODE_NONE {
        counters.files_verified = 0;
        return PhaseOutcome::succeeded(counters);
    }
    if verify_mode == VERIFY_MODE_ONLY_FILES_TRANSFERRED {
        counters.files_verified = counters.files_transferred;
        return PhaseOutcome::succeeded(counters);
    }

    let (source, destination) = listings(callback, due).await;
    let admitted = admitted(due, &source);
    counters.files_verified = admitted.len() as i64;
    let mismatched = admitted
        .iter()
        .filter(|object| {
            destination
                .get(&object.relative_key)
                .is_none_or(|held| !held.matches(object))
        })
        .count();
    if mismatched > 0 {
        return PhaseOutcome::failed(
            counters,
            VERIFICATION_ERROR_CODE,
            VERIFICATION_ERROR_DETAIL.to_string(),
        );
    }
    PhaseOutcome::succeeded(counters)
}

/// What each side of the transfer holds right now.
async fn listings(callback: &EmbeddedEmulator, due: &DuePhase) -> (Listing, Listing) {
    let source = match due.source.as_ref() {
        Some(endpoint) => list_location(callback, &due.region, endpoint).await,
        None => Listing::new(),
    };
    let destination = match due.destination.as_ref() {
        Some(endpoint) => list_location(callback, &due.region, endpoint).await,
        None => Listing::new(),
    };
    (source, destination)
}

/// The objects one side of the transfer holds under its own prefix.
///
/// An answer the object store did not give establishes nothing, so a bucket it holds
/// nothing under — a location whose bucket was provisioned out of band, or never existed
/// — reads as a side holding no objects rather than as a failure of the transfer.
async fn list_location(
    callback: &EmbeddedEmulator,
    region: &str,
    endpoint: &S3Endpoint,
) -> Listing {
    let client = ::service_common::s3_client::s3_client(callback, region);
    let mut found = Listing::new();
    let mut continuation: Option<String> = None;
    loop {
        let mut listing = client.list_objects_v2().bucket(&endpoint.bucket);
        if !endpoint.prefix.is_empty() {
            listing = listing.prefix(&endpoint.prefix);
        }
        if let Some(token) = continuation.as_deref() {
            listing = listing.continuation_token(token);
        }
        let page = match listing.send().await {
            Ok(page) => page,
            Err(error) => {
                ::tracing::debug!(
                    error = ?error,
                    bucket = %endpoint.bucket,
                    "a transfer location's bucket could not be listed"
                );
                return found;
            }
        };
        for object in page.contents() {
            let Some(key) = object.key() else { continue };
            let Some(relative) = key.strip_prefix(endpoint.prefix.as_str()) else {
                continue;
            };
            // A key that is the prefix itself, or ends in the delimiter, names a folder
            // rather than an object with bytes to move.
            if relative.is_empty() || relative.ends_with('/') {
                continue;
            }
            found.insert(
                relative.to_string(),
                ListedObject {
                    relative_key: relative.to_string(),
                    key: key.to_string(),
                    size: object.size().unwrap_or_default(),
                    etag: object.e_tag().unwrap_or_default().trim_matches('"').to_string(),
                    last_modified: object.last_modified().map(last_modified_metadata_value),
                },
            );
            if found.len() >= MAX_OBJECTS_PER_SIDE {
                return found;
            }
        }
        continuation = page.next_continuation_token().map(ToString::to_string);
        if continuation.is_none() {
            return found;
        }
    }
}

/// What the transfer read off ONE source object: the bytes it moves, and the metadata it
/// carries with them.
#[derive(Debug, Default)]
struct SourceObject {
    /// The object's bytes, as the source side served them.
    body: Vec<u8>,
    /// The object's content type.
    content_type: Option<String>,
    /// Its user metadata, which an object store reports as `x-amz-meta-*`.
    user_metadata: BTreeMap<String, String>,
    /// Its tag set where the task carries tags across, and no tags at all where it does
    /// not: a task whose `ObjectTags` is `NONE` reads none, so none is written.
    tags: Vec<(String, String)>,
}

/// The source object at `object`, read off the source side, or `None` when the object
/// store would not ANSWER about it.
///
/// Such an object is one this transfer does not move: what the store did not answer
/// establishes nothing about the object, so nothing is written for it and no counter
/// reports it (the module docs state the rule). ONE read carries the bytes and the metadata
/// the destination write needs, since a read of an object reports both.
async fn source_object(
    client: &::aws_sdk_s3::Client,
    due: &DuePhase,
    endpoint: &S3Endpoint,
    object: &ListedObject,
) -> Option<SourceObject> {
    let read = client
        .get_object()
        .bucket(&endpoint.bucket)
        .key(&object.key)
        .send()
        .await;
    let read = match read {
        Ok(read) => read,
        Err(error) => {
            ::tracing::debug!(
                error = ?error,
                key = %object.key,
                "a transfer could not read a source object"
            );
            return None;
        }
    };
    let content_type = read.content_type().map(ToString::to_string);
    let user_metadata: BTreeMap<String, String> = read
        .metadata()
        .map(|metadata| {
            metadata
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();
    // The bytes themselves, held only for as long as the one write below takes: a phase
    // moves one object at a time, so what this holds is bounded by the largest object the
    // object store accepts rather than by what a location holds.
    let body = match read.body.collect().await {
        Ok(collected) => collected.into_bytes().to_vec(),
        Err(error) => {
            ::tracing::debug!(
                error = ?error,
                key = %object.key,
                "a transfer could not read a source object's bytes"
            );
            return None;
        }
    };
    let tags = match preserves_object_tags(due) {
        true => source_tags(client, endpoint, object).await?,
        false => Vec::new(),
    };
    Some(SourceObject {
        body,
        content_type,
        user_metadata,
        tags,
    })
}

/// The tag set the source object at `object` carries, or `None` when the object store would
/// not ANSWER about it.
///
/// A tag read that did not answer establishes nothing about the object's tags, so the object
/// is one this transfer does not move — the same exit a source object it could not read at
/// all takes, rather than a destination written without the tag set its source carries.
async fn source_tags(
    client: &::aws_sdk_s3::Client,
    endpoint: &S3Endpoint,
    object: &ListedObject,
) -> Option<Vec<(String, String)>> {
    let read = client
        .get_object_tagging()
        .bucket(&endpoint.bucket)
        .key(&object.key)
        .send()
        .await;
    match read {
        Ok(tagged) => Some(
            tagged
                .tag_set()
                .iter()
                .map(|tag| (tag.key().to_string(), tag.value().to_string()))
                .collect(),
        ),
        Err(error) => {
            ::tracing::debug!(
                error = ?error,
                key = %object.key,
                "a transfer could not read a source object's tag set"
            );
            None
        }
    }
}

/// The `x-amz-tagging` a write delivers `tags` in, or `None` for a write that carries no
/// tag set at all.
///
/// "The tag-set for the object. The tag-set must be encoded as URL Query parameters. (For
/// example, "Key1=Value1")" (`PutObjectRequest$Tagging`) — so each pair is one tag with
/// both halves percent-encoded over the set RFC 3986 reserves, which is what makes a tag
/// carrying a `&`, an `=` or a space arrive as the tag it IS.
fn tagging_header(tags: &[(String, String)]) -> Option<String> {
    if tags.is_empty() {
        return None;
    }
    let encoded = tags
        .iter()
        .map(|(key, value)| format!("{}={}", url_encoded(key), url_encoded(value)))
        .collect::<Vec<String>>()
        .join("&");
    Some(encoded)
}

/// `text` as one URL-encoded query-string half.
fn url_encoded(text: &str) -> String {
    ::percent_encoding::utf8_percent_encode(text, ::percent_encoding::NON_ALPHANUMERIC)
        .to_string()
}

/// The user metadata the destination object is written with.
///
/// The source object's own entries where the task preserves object metadata, carried
/// across verbatim, and none of them where the task ignores it. Either way the
/// destination ends up with a [`LAST_MODIFIED_METADATA_KEY`] entry: the source object's
/// own where it carries one, and otherwise the modification time the source side reports
/// for the object it listed.
fn destination_metadata(
    due: &DuePhase,
    object: &ListedObject,
    user_metadata: &BTreeMap<String, String>,
) -> ::std::collections::HashMap<String, String> {
    let mut metadata: ::std::collections::HashMap<String, String> =
        if preserves_object_metadata(due) {
            user_metadata
                .iter()
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect()
        } else {
            ::std::collections::HashMap::new()
        };
    if let Some(last_modified) = object.last_modified.clone() {
        metadata
            .entry(LAST_MODIFIED_METADATA_KEY.to_string())
            .or_insert(last_modified);
    }
    metadata
}

/// The modification time `at` as the destination object's [`LAST_MODIFIED_METADATA_KEY`]
/// entry states it: whole seconds since the epoch, the unit a file's or an object's
/// modification time is kept in, so a client reads the entry as the TIME it preserves and
/// not as a rendering of it in one of the several forms an API states an instant in.
///
/// A modification time has one-second resolution, so the value is the seconds and nothing
/// finer. Nothing in the model states this form — no DataSync shape describes what a
/// transfer preserves in an object's user metadata — and it is written only for a source
/// object that carries no entry of this name itself: one that does has its own value travel
/// verbatim ([`destination_metadata`]).
fn last_modified_metadata_value(at: &::aws_smithy_types::DateTime) -> String {
    at.secs().to_string()
}

/// Whether this task carries the source object's metadata across: "specifies whether you
/// want DataSync to `PRESERVE` object metadata (default behavior) when transferring
/// between object storage systems. If you want your DataSync task to ignore object
/// metadata, specify the `NONE` value" (`Options$ObjectMetadata`).
fn preserves_object_metadata(due: &DuePhase) -> bool {
    due.options.object_metadata.as_deref() != Some(PRESERVE_NONE)
}

/// Whether it carries the source object's tag set across, which `Options$ObjectTags`
/// documents with the same pair of values and the same default.
fn preserves_object_tags(due: &DuePhase) -> bool {
    due.options.object_tags.as_deref() != Some(PRESERVE_NONE)
}

/// The task's effective `TransferMode`.
fn transfer_mode(due: &DuePhase) -> &str {
    due.options.transfer_mode.as_deref().unwrap_or_default()
}

/// The source objects this execution's filters admit, in key order.
///
/// A filter rule carries its patterns delimited by `|` ("A single filter string that
/// consists of the patterns to include or exclude. The patterns are delimited by '|'"),
/// matched against each object's path under the source location. An object is admitted
/// when the include rules admit it — a request that stated none includes everything —
/// and no exclude rule matches it. A pattern matches a path it names outright, and
/// everything under a folder it names.
fn admitted<'a>(due: &DuePhase, source: &'a Listing) -> Vec<&'a ListedObject> {
    source
        .values()
        .filter(|object| {
            let path = format!("/{}", object.relative_key);
            let included = due.includes.is_empty() || matches_any(&due.includes, &path);
            included && !matches_any(&due.excludes, &path)
        })
        .collect()
}

/// Whether any pattern of `rules` matches `path`.
fn matches_any(rules: &[FilterRule], path: &str) -> bool {
    rules
        .iter()
        .filter(|rule| {
            rule.filter_type
                .as_deref()
                .is_none_or(|filter_type| filter_type == SIMPLE_PATTERN)
        })
        .filter_map(|rule| rule.value.as_deref())
        .flat_map(|value| value.split('|'))
        .map(str::trim)
        .filter(|pattern| !pattern.is_empty())
        .any(|pattern| matches_pattern(pattern, path))
}

/// Whether one filter pattern matches one object path.
///
/// The pattern's `*` is the tree's one wildcard match
/// ([`service_common::iam_authorization::wildcard_match`]), compared case-sensitively
/// the way object keys are, and a pattern that names a folder covers everything under
/// it.
fn matches_pattern(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    ::service_common::iam_authorization::wildcard_match(pattern, path, true)
        || ::service_common::iam_authorization::wildcard_match(
            &format!("{pattern}/*"),
            path,
            true,
        )
}

/// The admitted source objects the task's `TransferMode` selects for transfer.
///
/// `ALL` "copies everything in the source to the destination without comparing
/// differences between the locations"; `CHANGED` "copies only the data and metadata that
/// differs between the source and destination location", which is an object the
/// destination does not hold or holds with different content.
fn selected<'a>(
    due: &DuePhase,
    admitted: &[&'a ListedObject],
    destination: &Listing,
) -> Vec<&'a ListedObject> {
    let all = transfer_mode(due) == TRANSFER_MODE_ALL;
    admitted
        .iter()
        .copied()
        .filter(|object| {
            all || destination
                .get(&object.relative_key)
                .is_none_or(|held| !held.matches(object))
        })
        .collect()
}

/// The selected objects the task's `OverwriteMode` allows this run to write.
///
/// `NEVER` "doesn't overwrite data in the destination location even if the source data
/// has changed", so an object the destination already holds is left where it is rather
/// than written — which is also why it is not among the files the execution expects to
/// transfer, and why the transfer step counts it as skipped.
fn writable_by_overwrite_mode<'a>(
    due: &DuePhase,
    selected: &[&'a ListedObject],
    destination: &Listing,
) -> Vec<&'a ListedObject> {
    let never = due.options.overwrite_mode.as_deref() == Some(OVERWRITE_MODE_NEVER);
    selected
        .iter()
        .copied()
        .filter(|object| !(never && destination.contains_key(&object.relative_key)))
        .collect()
}

/// The destination objects a task configured not to preserve deleted files removes:
/// "Deletes destination files that aren't present in the source"
/// (`PreserveDeletedFiles REMOVE`). A task that preserves them deletes nothing.
fn deletable<'a>(
    due: &DuePhase,
    source: &Listing,
    destination: &'a Listing,
) -> Vec<&'a ListedObject> {
    if due.options.preserve_deleted_files.as_deref() != Some(PRESERVE_DELETED_FILES_REMOVE) {
        return Vec::new();
    }
    destination
        .values()
        .filter(|object| !source.contains_key(&object.relative_key))
        .collect()
}
