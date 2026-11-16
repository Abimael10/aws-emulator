// SPDX-License-Identifier: CC0-1.0

//! Publishing destinations: the export target a detector publishes its findings
//! to, and the verification status the clock owes it.

use super::*;

/// One publishing destination: the resource a detector exports its findings to,
/// plus the KMS key those findings are encrypted with.
///
/// The destination properties are stored exactly as the create accepted them —
/// nothing is published, so the ARNs are only ever reported back.
#[derive(Debug, Clone)]
pub struct PublishingDestinationRecord {
    pub id: String,
    /// `DestinationType`: a member of [`DESTINATION_TYPES`].
    pub destination_type: String,
    /// "The ARN of the resource to publish to"
    /// (`smithy_query(shape="DestinationProperties")`).
    pub destination_arn: String,
    /// "The ARN of the KMS key to use for encryption"
    /// (`smithy_query(shape="DestinationProperties")`).
    pub kms_key_arn: String,
    /// The expected owner of the destination bucket. "This field appears in the
    /// response only if it was provided when the publishing destination was
    /// created or updated"
    /// (`smithy_query(query="doc",shape="DestinationProperties",member="ExpectedBucketOwner")`),
    /// so it stays absent until a request supplies it.
    pub expected_bucket_owner: Option<String>,
    /// `PublishingStatus`: a member of [`PUBLISHING_STATUSES`].
    pub status: String,
    /// The instant the verification this destination is waiting on comes due, in
    /// epoch milliseconds; `None` once [`Self::settle`] has materialised it.
    pub verification_due_millis: Option<i64>,
    /// "The time, in epoch millisecond format, at which GuardDuty was first unable
    /// to publish findings to the destination"
    /// (`smithy_query(query="doc",shape="DescribePublishingDestinationResponse",member="PublishingFailureStartTimestamp")`).
    /// Zero while publishing to this destination has never failed, which is every
    /// destination this emulator holds: nothing is published, so nothing fails.
    pub publishing_failure_start_millis: i64,
    pub tags: BTreeMap<String, String>,
    /// The `ClientToken` the create that minted this destination carried, if any —
    /// the `@idempotencyToken` index, kept on the record so it cannot outlive the
    /// destination it names. Read it through
    /// [`Detector::publishing_destination_for_token`].
    pub client_token: Option<String>,
    pub created_at_millis: i64,
    pub updated_at_millis: i64,
}

impl PublishingDestinationRecord {
    /// Materialise the verification the clock already owes: a destination created
    /// at `t` reports [`PENDING_VERIFICATION`] until `t +
    /// VERIFICATION_DELAY_MILLIS` and [`PUBLISHING`] from then on.
    ///
    /// Only the clock moves the status here, and it moves it at an instant fixed
    /// by the create, so a read that settles it shows exactly what a read at the
    /// same instant would have shown had nobody called in between. A status a
    /// request moved away from `PENDING_VERIFICATION` is left alone.
    pub fn settle(&mut self, now: i64) {
        let Some(due) = self.verification_due_millis else {
            return;
        };
        if now < due {
            return;
        }
        self.verification_due_millis = None;
        if self.status == PENDING_VERIFICATION {
            self.status = PUBLISHING.to_string();
        }
    }

    /// Put the destination back in [`PENDING_VERIFICATION`] and owe the move to
    /// [`PUBLISHING`] again, as of `now`.
    ///
    /// The destination properties say WHERE GuardDuty publishes and with which key,
    /// so a destination whose properties changed is one GuardDuty has not verified
    /// it can write to — the same state a create leaves behind, reached again by the
    /// same clock-owed span. This is what makes the status a destination reports
    /// follow its current properties: a destination whose properties were fixed is
    /// verified again rather than keeping the verdict reached for the properties it
    /// no longer has.
    pub fn begin_verification(&mut self, now: i64) {
        self.status = PENDING_VERIFICATION.to_string();
        self.verification_due_millis = Some(now.saturating_add(VERIFICATION_DELAY_MILLIS));
    }
}

/// The message GuardDuty refuses a `destinationArn` whose resource does not exist
/// with: AWS's own published wording for that parameter, naming the member in the
/// words AWS uses for it and quoting neither the ARN nor the resource back.
///
/// "Before you start exporting the findings, the destination resource must exist"
/// (`smithy_query(query="doc",shape="CreatePublishingDestination")`), so a request
/// naming a resource that is not there describes an export that could never happen
/// and is refused at configuration time rather than recorded.
pub const DESTINATION_RESOURCE_ABSENT_MESSAGE: &str =
    "The request failed because the resource specified in the destinationArn parameter does not exist.";

/// The Amazon S3 BUCKET the destination ARN `destination_arn` publishes into, or
/// `None` when it names no bucket at all.
///
/// "To specify an S3 bucket folder use the following format:
/// `arn:aws:s3:::DOC-EXAMPLE-BUCKET/myFolder/`"
/// (`smithy_query(query="doc",shape="DestinationProperties",member="DestinationArn")`),
/// so the resource field opens with the bucket and anything after the first `/` is
/// a folder inside it: the resource whose EXISTENCE decides the request is the
/// bucket. The ARN is split by the shared ARN reader rather than by hand, and the
/// NAME itself is left to Amazon S3 to judge — what this service needs to know is
/// whether the store holds a bucket under it, which is the store's answer and not a
/// reading of the spelling.
#[must_use]
pub fn destination_bucket(destination_arn: &str) -> Option<&str> {
    let arn = ::service_common::arn::parse_of(destination_arn, S3_SERVICE)?;
    let bucket = arn.resource.split('/').next().unwrap_or(arn.resource);
    (!bucket.is_empty()).then_some(bucket)
}

/// Whether the resource `destination_arn` names is one Amazon S3 ANSWERED that it
/// does not hold — the question
/// [`DESTINATION_RESOURCE_ABSENT_MESSAGE`] is the refusal for.
///
/// A publishing destination's resource belongs to another service, so this service
/// resolves it where that service holds it: the bucket is asked for with
/// `HeadBucket` ("you can use this operation to determine if a bucket exists"),
/// over the in-process callback every call this emulator makes to another emulated
/// service goes through ([`smithy_server_runtime::OperationContext::callback`]) and
/// the shared client that hop is configured by
/// ([`service_common::s3_client::s3_client`]) — nothing here opens a socket, and
/// the only caller-supplied value that reaches the client's address is the signing
/// Region, narrowed to a Region label before it gets there.
///
/// `false` for every answer that is not the object store reporting the bucket
/// absent: an emulator assembled with no object store to ask establishes nothing
/// about the bucket, and a refusal has to be grounded in what Amazon S3 answered
/// rather than assumed. A value that names no bucket to ask about is refused by the
/// FORM check the same request already passes through
/// ([`validate_destination_property_forms`]), so it is not this refusal either.
pub async fn destination_resource_is_absent(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    destination_arn: &str,
) -> bool {
    let Some(bucket) = destination_bucket(destination_arn) else {
        return false;
    };
    // The Region decides nothing about which store answers — the emulator serves one
    // — so a scope carrying no Region label falls back to the default rather than
    // putting caller text in the client's authority.
    let region = ::service_common::arn::region_label(region).unwrap_or(DEFAULT_REGION);
    let answer = ::service_common::s3_client::s3_client(ctx.callback(), region)
        .head_bucket()
        .bucket(bucket)
        .send()
        .await;
    match answer {
        // Amazon S3 holds the bucket.
        Ok(_) => false,
        Err(error) => {
            // What the object store's refusal ESTABLISHED, read by the one shared
            // reading of such an answer: only the store itself saying it holds
            // nothing there is the absent resource this refuses on.
            let read = ::service_common::s3_object_read::answered_refusal(&error);
            read == ::service_common::s3_object_read::ObjectRead::Absent
        }
    }
}
