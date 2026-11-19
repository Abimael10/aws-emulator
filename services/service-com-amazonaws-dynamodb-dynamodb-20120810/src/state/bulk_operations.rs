// SPDX-License-Identifier: CC0-1.0

//! The bulk-operation catalogue: the records `StartBulkPutItems` writes and the
//! bulk-operation readers report.
//!
//! A bulk operation is a RESOURCE OF ITS OWN rather than part of a table — it is addressed
//! by its own ARN and it outlives the writes it performed — so it lives beside [`super`]
//! rather than inside it, like the import and export catalogues next door; `super`
//! re-exports everything here, so every caller still names it `crate::state::…`.
//!
//! WHERE THE SHAPE OF THIS COMES FROM. The service's Smithy model carried no
//! bulk-operation shape when this was written, so the members a record holds are the ones
//! the recorded calls name: the operation's own ARN, its status, the table it acts on and
//! the instants it began and ended, which is what a listing reports for each of them. The
//! model this crate is built from now declares those shapes (`BulkOperationSummary`), and
//! this is the state they are reported from.

use super::*;

/// The `BulkOperationStatus` a bulk operation is in while it is still running: a bulk put
/// is asynchronous — the call answers an operation to watch rather than a result — so a
/// caller that reads straight after the request still sees it running.
pub const BULK_OPERATION_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";

/// The `BulkOperationStatus` a bulk operation reports once its span has elapsed.
pub const BULK_OPERATION_STATUS_COMPLETED: &str = "COMPLETED";

/// The `BulkOperationStatus` of a bulk operation whose source could not be read at all,
/// which it reports once its span has elapsed rather than a completion it did not achieve.
pub const BULK_OPERATION_STATUS_FAILED: &str = "FAILED";

/// How long a `ClientToken` stands for the bulk operation it first started.
///
/// The window every other idempotent start of this service measures a repeated token over
/// ([`IMPORT_CLIENT_TOKEN_WINDOW`]): a token is "valid for 8 hours after the first request
/// that uses it is completed. After 8 hours, any request with the same client token is
/// treated as a new request" (`ImportTable$ClientToken`, the one window this service's
/// model states for a client token). Measured on every read from the instant the operation
/// was requested, so nothing sleeps and no read writes.
pub const BULK_OPERATION_CLIENT_TOKEN_WINDOW: Duration = IMPORT_CLIENT_TOKEN_WINDOW;

/// The most bulk operations this emulator will hold IN ONE REGION.
///
/// This emulator's own ceiling, like the import catalogue's
/// ([`MAX_IMPORTS_PER_REGION`]): it keeps the catalogue of one region bounded by the number
/// of tables that region may hold, so a client cannot grow it without end. Reaching it
/// answers the modelled `LimitExceededException`.
pub const MAX_BULK_OPERATIONS_PER_REGION: usize = MAX_IMPORTS_PER_REGION;

/// A new bulk operation's ARN:
/// `arn:aws:dynamodb:<region>:<account>:bulkoperation/<millis>-<id>`, built through the
/// same core ARN helper every other identifier here is minted with.
///
/// ACCOUNT-SCOPED rather than nested under the table it acts on: a bulk operation is a
/// resource of the account's own bulk-operation namespace — the listing that reports them
/// reaches every table's, and the table each one acts on is a member of the record rather
/// than a segment of its name.
///
/// The last segment is the instant the operation was requested, in zero-padded epoch
/// milliseconds, followed by a unique suffix: the instant alone does not identify an
/// operation, because two calls can land in the same millisecond. Minted ONCE, when the
/// operation is recorded, and stored on it — and because the fixed-width instant sits
/// inside it, ascending ARN order IS ascending request-instant order, which is the order a
/// listing pages through.
pub fn new_bulk_operation_arn(region: &str, requested_at: SystemTime) -> String {
    let millis = requested_at
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis());
    let unique = ::uuid::Uuid::new_v4().simple().to_string();
    ::engine::emulator_arn(
        "dynamodb",
        region,
        &format!(
            "bulk-operation/{millis:014}-{}",
            unique.get(..8).unwrap_or(unique.as_str())
        ),
    )
}

/// What two `StartBulkPutItems` requests have to agree on for a replayed `ClientToken` to
/// answer the operation it already started: EVERY member the request carries except the
/// token itself — the table, the source it reads, the location it reports into, the
/// write-unit ceiling and the performance tier.
///
/// One rendering of those members, so the comparison is member for member however deeply a
/// member nests, and a request that changes any of them is a conflicting replay rather than
/// a second operation. The members are compared as the request STATED them, except the two
/// that RESOLVE — the source's format and its compression, each resolved to the value its
/// member defaults to, so a request naming `NONE` and one naming no compression ask for the
/// same operation.
pub fn bulk_put_request_signature(
    table_name: &str,
    input: &BulkPutInputS3Configuration,
    output: Option<&BulkPutOutputS3Configuration>,
    max_write_request_units: Option<i64>,
    performance_tier: Option<&str>,
) -> String {
    format!(
        "{:?}",
        (
            table_name,
            &input.s3_bucket,
            &input.s3_bucket_owner,
            &input.s3_key_prefix,
            input
                .input_format
                .as_deref()
                .unwrap_or(INPUT_FORMAT_DYNAMODB_JSON),
            input
                .input_compression_type
                .as_deref()
                .unwrap_or(DEFAULT_INPUT_COMPRESSION_TYPE),
            output.map(|output| {
                (
                    &output.s3_bucket,
                    &output.s3_bucket_owner,
                    &output.s3_key_prefix,
                    &output.output_type,
                )
            }),
            max_write_request_units,
            performance_tier,
        )
    )
}

/// One recorded bulk operation: what it wrote, into which table, and how far it got.///
/// The status a reader REPORTS is derived at every read from the stored instants
/// ([`Self::status`]), the way an import's and a table's are, so the `IN_PROGRESS` ->
/// terminal transition the clock owes needs no write.
#[derive(Debug, Clone)]
pub struct BulkOperation {
    /// The ARN minted for this operation when it was recorded
    /// ([`new_bulk_operation_arn`]), which is also the key the store holds it under.
    pub arn: String,
    /// The region this operation lives in — the one the table it wrote into lives in,
    /// which is the scope every bulk-operation call resolves inside.
    pub region: String,
    /// The name of the table this operation wrote into. The `TableArn` a reader reports is
    /// minted from it and this region ([`table_arn`]), so one stored table identity cannot
    /// drift from the ARN the table reports for itself.
    pub table_name: String,
    /// The `ClientToken` the call that started it carried, or `None` where it carried
    /// none: what makes a repeated call answer this operation instead of starting a second
    /// one ([`BULK_OPERATION_CLIENT_TOKEN_WINDOW`]).
    pub client_token: Option<String>,
    /// The S3 location the call named as the source of its items, as it named it.
    pub input_s3_configuration: BulkPutInputS3Configuration,
    /// The S3 location the call named for the operation's own outcome report, where it
    /// named one.
    pub output_s3_configuration: Option<BulkPutOutputS3Configuration>,
    /// The write-unit ceiling the call asked the operation to run under, where it asked for
    /// one.
    pub max_write_request_units: Option<i64>,
    /// The performance tier the call asked the operation to run in, where it asked for one.
    pub performance_tier: Option<String>,
    /// How many records the task READ, the ones it could not decode included — the figure
    /// the audit record of the call states.
    pub processed_item_count: i64,
    /// How many items LANDED in the table.
    pub written_item_count: i64,
    /// How many records the task read and could not write.
    pub error_count: i64,
    /// The instant the operation began, which a reader reports as its `StartTime`.
    pub started_at: SystemTime,
    /// The instant it is due to end, which is one transition span after it began
    /// ([`STATUS_TRANSITION`]). Before it the operation reports `IN_PROGRESS` and no end
    /// time; from it the status it settled into, and this instant as its end.
    pub ended_at: SystemTime,
    /// The status it SETTLES INTO once that instant has passed: one of the
    /// `BulkOperationStatus` values the model declares.
    pub status: String,
}

impl BulkOperation {
    /// The `BulkOperationStatus` at `now`: a bulk operation runs for a span, so a caller
    /// that reads straight after the request still sees it running, and only then the
    /// status it settled into.
    pub fn status(&self, now: SystemTime) -> &str {
        if now >= self.ended_at {
            self.status.as_str()
        } else {
            BULK_OPERATION_STATUS_IN_PROGRESS
        }
    }

    /// The instant this operation ENDED, or `None` while it is still running: no reader
    /// reports an end for work that has not finished.
    pub fn ended_at(&self, now: SystemTime) -> Option<SystemTime> {
        Some(self.ended_at).filter(|end| now >= *end)
    }

    /// Whether a repeated `ClientToken` still stands for this operation at `now`
    /// ([`BULK_OPERATION_CLIENT_TOKEN_WINDOW`]), measured from the instant it was
    /// requested.
    pub fn within_idempotency_window(&self, now: SystemTime) -> bool {
        now.duration_since(self.started_at)
            .is_ok_and(|elapsed| elapsed <= BULK_OPERATION_CLIENT_TOKEN_WINDOW)
    }

    /// What this operation was REQUESTED with, in the one form two requests are compared in
    /// ([`bulk_put_request_signature`]).
    pub fn request_signature(&self) -> String {
        bulk_put_request_signature(
            &self.table_name,
            &self.input_s3_configuration,
            self.output_s3_configuration.as_ref(),
            self.max_write_request_units,
            self.performance_tier.as_deref(),
        )
    }

    /// This operation as `ListBulkOperations` reports it, as it stands at `now`: every
    /// member read from the record.
    pub fn summary(&self, now: SystemTime) -> BulkOperationSummary {
        BulkOperationSummary {
            bulk_operation_arn: Some(self.arn.clone()),
            bulk_operation_status: Some(self.status(now).to_string()),
            table_arn: Some(table_arn(&self.region, &self.table_name)),
            start_time: Some(super::reported_date(self.started_at)),
            end_time: self.ended_at(now).map(super::reported_date),
        }
    }

    /// This operation as the call that started it — and every call that replays its token —
    /// reports it, as it stands at `now`.
    ///
    /// The figures a FINISHED task reports are reported only once the task has finished, the
    /// way an import's are: while it runs the counters stand at zero, because nothing has
    /// been counted yet.
    pub fn describe(&self, now: SystemTime) -> BulkOperationDescription {
        let finished = self.ended_at(now).is_some();
        BulkOperationDescription {
            bulk_operation_arn: Some(self.arn.clone()),
            bulk_operation_status: Some(self.status(now).to_string()),
            table_arn: Some(table_arn(&self.region, &self.table_name)),
            client_token: self.client_token.clone(),
            input_s3_configuration: Some(self.input_s3_configuration.clone()),
            output_s3_configuration: self.output_s3_configuration.clone(),
            max_write_request_units: self.max_write_request_units,
            performance_tier: self.performance_tier.clone(),
            start_time: Some(super::reported_date(self.started_at)),
            end_time: self.ended_at(now).map(super::reported_date),
            processed_item_count: Some(if finished {
                self.processed_item_count
            } else {
                0
            }),
            error_count: Some(if finished { self.error_count } else { 0 }),
        }
    }
}

impl ComAmazonawsDynamodbDynamodb20120810State {
    /// Every bulk operation of one region, in ascending ARN order, which is the scope a
    /// listing reports and the order it pages through.
    pub fn bulk_operations_in<'s>(
        &'s self,
        region: &'s str,
    ) -> impl Iterator<Item = &'s BulkOperation> + 's {
        self.bulk_operations
            .values()
            .filter(move |operation| operation.region == region)
    }

    /// How many bulk operations that region already holds — what
    /// [`MAX_BULK_OPERATIONS_PER_REGION`] is measured against.
    pub fn bulk_operation_count_in(&self, region: &str) -> usize {
        self.bulk_operations_in(region).count()
    }

    /// Record one bulk operation under the ARN minted for it.
    pub fn insert_bulk_operation(&mut self, operation: BulkOperation) {
        self.bulk_operations
            .insert(operation.arn.clone(), operation);
    }

    /// The bulk operation a `ClientToken` still stands for at `now` in that region, if any
    /// — what makes a repeated call idempotent rather than a second operation.
    ///
    /// Scoped to the region the request reached, like every other bulk-operation lookup: a
    /// token names an operation of the endpoint it was first used against.
    pub fn bulk_operation_for_client_token(
        &self,
        region: &str,
        client_token: &str,
        now: SystemTime,
    ) -> Option<&BulkOperation> {
        self.bulk_operations.values().rev().find(|operation| {
            operation.region == region
                && operation.client_token.as_deref() == Some(client_token)
                && operation.within_idempotency_window(now)
        })
    }
}
