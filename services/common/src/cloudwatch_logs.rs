// SPDX-License-Identifier: CC0-1.0

//! What a caller-supplied CloudWatch Logs LOG GROUP NAME has to be.
//!
//! Several emulated services take a log group name as an input member and then WRITE
//! into that group — an access-log subscription, a delivery destination, a job's log
//! configuration. The value is therefore an instruction to create or write a log group,
//! so it has to be a name CloudWatch Logs can actually hold: the empty string, a name
//! past CloudWatch's ceiling, and a name carrying a character outside the published set
//! each name no group anything could ever be written to.
//!
//! CloudWatch Logs publishes one rule for every such name — 1 to
//! [`MAX_LOG_GROUP_NAME_CHARS`] characters, each of them a letter, a digit or one of the
//! characters [`log_group_name_requirement`] lists — and the services that bind such a
//! member typically declare it as a bare string with no `@length` or `@pattern`, so the
//! framework's constraint walk cannot refuse any of it. The rule lives here, once, so no
//! two services disagree about which names CloudWatch Logs holds; each service keeps only
//! the envelope its own API refuses with (the status, the error shape and which member the
//! refusal is keyed by).
//!
//! Beside the NAME, this module holds the three things a service that WRITES into a group
//! needs: the reading of a log group's own ARN, which is how a caller names the destination
//! it is pointing a service at ([`log_group_of_arn`]), whether CloudWatch Logs holds the
//! group a destination names at all ([`log_group_is_absent`]), and the vended-logs write
//! itself ([`delivered`]) — the one path a service delivering records on the account's
//! behalf puts them in the group through.
//!
//! The write comes in the two forms a destination comes in. A group the delivering service
//! publishes under its OWN name (an API Gateway stage's execution log group) is one it
//! provisions, so [`delivered`] creates it; a destination the service was merely POINTED AT
//! (a stage's access-log `destinationArn`) is a group of the account's, so
//! [`delivered_into_held_group`] writes into the group CloudWatch Logs already holds and
//! creates none — a destination that is not there loses the records and stays absent.
//!
//! A group of the first kind is PUBLISHED rather than merely written to, so it exists from the
//! moment the configuration that turns its log on is accepted, before any record is written
//! into it: [`provisioned`] is that creation on its own, and a client reads such a group — or
//! attaches a retention policy or a subscription filter to it — without waiting for the first
//! delivery.
//!
//! Whichever form it takes, the write goes in through CloudWatch Logs' OWN ingest path
//! (`PutLogEvents`) rather than into that service's store: a delivered record is an
//! ingestion into the destination group, so it is subject to every consumer that group is
//! configured with — its metric filters, its subscription filters, its data protection
//! policy, the Insights rules collecting from it and its own ingestion metrics — exactly as
//! a record a client put there is. A deliverer whose records already carry the instants they
//! report delivers them at those instants ([`delivered_stamped_over`]).
//!
//! `service-com-amazonaws-mediapackage-mediapackage` still carries its own copy of this
//! reading (`state::is_log_group_name`), inlined before this module existed and pinned by
//! that service's own refusal suite; it is to be MIGRATED onto this one rather than copied
//! again, exactly as the claim-rule copies this crate lists are. The same holds for the two
//! copies of the EXISTENCE reading [`log_group_is_absent`] now states: `log_group_is_absent`
//! in `service-com-amazonaws-ivschat-amazoninteractivevideoservicechat/src/state/log_destinations.rs`
//! and the variant in
//! `service-com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice/src/ops/set_log_delivery_configuration/log_destinations.rs`,
//! each to be migrated in the change that owns its service — and for the two copies of the
//! BATCH SPLIT [`delivered_stamped_over`] now states, in
//! `service-com-amazonaws-rum-rum/src/state/cw_log_delivery.rs` and
//! `service-com-amazonaws-fis-faultinjectionsimulator/src/state/experiment_logs.rs`, each of
//! which splits its own delivery against its own copy of `PutLogEvents`' published bounds.
//! Nothing new should restate any of it — call this module.

/// The shortest log group name CloudWatch Logs holds — so the empty string names no group.
pub const MIN_LOG_GROUP_NAME_CHARS: usize = 1;

/// The longest log group name CloudWatch Logs holds; see [`MIN_LOG_GROUP_NAME_CHARS`].
///
/// This is the ceiling of the service that WOULD hold the group, not any one emulated
/// service's generic string bound: a longer name is one CloudWatch Logs would never
/// accept, whatever memory it would occupy in a store here.
pub const MAX_LOG_GROUP_NAME_CHARS: usize = 512;

/// The non-alphanumeric characters a log group name may carry, as a refusal names them.
const LOG_GROUP_PUNCTUATION_CHARACTERS: &str = ". - _ / #";

/// Whether `c` is a character a CloudWatch Logs log group name may carry.
///
/// CloudWatch Logs' published set: letters, digits and the characters
/// [`LOG_GROUP_PUNCTUATION_CHARACTERS`] names. The letters-and-digits half is ASCII-ONLY
/// — unlike AWS's tagging restrictions, which say "Unicode letters, digits" — because the
/// log-group rule publishes its set as `a-z`, `A-Z` and `0-9`, so an accented or
/// non-Latin name is not one CloudWatch Logs holds.
///
/// Private: a caller judges a whole NAME ([`is_log_group_name`]), which is the unit the
/// rule is about; nothing outside this module needs the per-character half.
fn is_log_group_name_character(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | '#')
}

/// Whether `value` is a name CloudWatch Logs can hold: between
/// [`MIN_LOG_GROUP_NAME_CHARS`] and [`MAX_LOG_GROUP_NAME_CHARS`] characters, every one of
/// them a letter, a digit or one of the characters
/// [`log_group_name_requirement`] lists.
///
/// The length is counted in CHARACTERS, not bytes, so a name is judged by what the caller
/// wrote rather than by how it happened to encode.
pub fn is_log_group_name(value: &str) -> bool {
    let length = value.chars().count();
    (MIN_LOG_GROUP_NAME_CHARS..=MAX_LOG_GROUP_NAME_CHARS).contains(&length)
        && value.chars().all(is_log_group_name_character)
}

/// The requirement a refused log group name failed: the member's WHOLE accepted domain, in
/// one sentence, so a name refused for its length and one refused for a character it
/// carries are answered with the same rule.
///
/// Built here rather than written at each call site, so the sentence and the bounds it
/// states cannot disagree and a suite asserting it reads the rule's own words.
pub fn log_group_name_requirement() -> String {
    format!(
        "must be {MIN_LOG_GROUP_NAME_CHARS} to {MAX_LOG_GROUP_NAME_CHARS} characters of \
         letters, numbers and `{LOG_GROUP_PUNCTUATION_CHARACTERS}`"
    )
}

/// The log group name prefix CloudWatch Logs keeps for groups of its own: "Log group names
/// can't start with the string aws/" (`CreateLogGroup`'s own documentation), so a name in it
/// is not a caller's to create.
pub const RESERVED_LOG_GROUP_NAME_PREFIX: &str = "aws/";

/// Whether `value` is a name in [`RESERVED_LOG_GROUP_NAME_PREFIX`], which CloudWatch Logs
/// creates no group under however well-formed the rest of it is.
pub fn is_reserved_log_group_name(value: &str) -> bool {
    value.starts_with(RESERVED_LOG_GROUP_NAME_PREFIX)
}

/// What CloudWatch Logs states for a create whose name is in that prefix: the member and the
/// string it may not start with.
///
/// The sentence lives here, once, so the service that owns log groups and any other service
/// that creates one on a caller's behalf cannot word the same refusal differently.
pub fn reserved_log_group_name_message() -> String {
    format!("logGroupName must not start with the string '{RESERVED_LOG_GROUP_NAME_PREFIX}'")
}

/// The suffix the ARN of a log group carries when it names the group TOGETHER with
/// everything under it, which is the form `DescribeLogGroups` reports a group's `arn` in.
const EVERYTHING_UNDER_THE_GROUP: &str = ":*";

/// The resource type a log group's ARN names
/// (`arn:<partition>:logs:<region>:<account>:log-group:<name>`).
const LOG_GROUP_RESOURCE_TYPE: &str = "log-group";

/// The NAME of the log group `arn` names, or `None` when it names anything else.
///
/// Several services take the ARN of a log group as the destination they are to write into
/// — an access-log setting, a vended-logs destination, a web ACL's logging configuration —
/// and both forms of that ARN name the same group: the group on its own, and the group
/// with everything under it ([`EVERYTHING_UNDER_THE_GROUP`], which is how a client reads
/// an ARN off `DescribeLogGroups`). What is left has to be a name CloudWatch Logs can hold
/// ([`is_log_group_name`]), so a destination of another kind (an Amazon S3 bucket, a
/// Firehose delivery stream) resolves no group at all and its caller reports the delivery
/// as one it does not make rather than writing somewhere nobody named.
#[must_use]
pub fn log_group_of_arn(arn: &str) -> Option<&str> {
    let resource = crate::arn::resource_id_of_type(arn, LOG_GROUP_RESOURCE_TYPE)?;
    let name = resource
        .strip_suffix(EVERYTHING_UNDER_THE_GROUP)
        .unwrap_or(resource);
    is_log_group_name(name).then_some(name)
}

/// Whether CloudWatch Logs ANSWERED that `region` holds no log group under `name`.
///
/// A service POINTED at a log group it is to deliver into has to resolve that group where
/// CloudWatch Logs holds it before it stores the destination, or it leaves an operator
/// believing a trail exists where no record is ever written. The question is put with
/// `DescribeLogGroups`, whose `logGroupIdentifiers` names groups "by name or ARN": an answer
/// carrying no group of exactly that name is CloudWatch Logs saying the group is not there.
///
/// `false` — establish nothing — for every answer that is NOT that one: an emulator
/// assembled with no CloudWatch Logs to ask, and an answer that could not be read. A
/// refusal has to be grounded in what the owning service said, which is the rule every
/// cross-service pre-condition in this tree follows.
///
/// The hop is the in-process callback into this same emulator ([`logs_client`]), so nothing
/// here opens a socket or leaves the process. Call it with NO state guard held — it awaits.
pub async fn log_group_is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> bool {
    match logs_client(ctx.callback(), region)
        .describe_log_groups()
        .log_group_identifiers(name)
        .send()
        .await
    {
        Ok(answer) => !answer
            .log_groups()
            .iter()
            .any(|group| group.log_group_name() == Some(name)),
        Err(error) => {
            // Not CloudWatch Logs reporting what it holds, so this establishes nothing about
            // the group — and it is worth a warning, because an emulator with no CloudWatch
            // Logs mounted to ask is one where the pre-condition cannot be decided. The
            // caller's own value is rendered in its escaped `Debug` form, so a name carrying
            // a newline cannot forge a log line.
            ::tracing::warn!(
                log_group = ?name,
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the log group a destination names could not be resolved in this emulator's \
                 CloudWatch Logs"
            );
            false
        }
    }
}

/// Write one log event per entry of `records` into `log_group`, in the stream `log_stream`.
///
/// The VENDED-LOGS write: a service delivering records on the account's behalf — an API
/// Gateway stage's execution log, the record a WAFv2 web ACL logs about a request it decided —
/// writes them here, once, so no two of them deliver differently. The group and the stream
/// are created first, because the group is one the delivering service publishes itself, and a
/// group or stream already there is the one it has been writing to; each record is one
/// log event stamped with the instant it is delivered, which is what keeps it inside
/// CloudWatch Logs' ingestion window. A destination the service was POINTED AT instead is
/// delivered to by [`delivered_into_held_group`], which creates no group.
///
/// The write goes over the in-process transport into the CloudWatch Logs this emulator
/// mounts, so a delivered record and `GetLogEvents` / `FilterLogEvents` cannot disagree
/// about it.
///
/// Delivery is BEST EFFORT, as it is for every vended-logs destination: a group the account
/// has since deleted, or a write CloudWatch Logs refuses, loses the record and is reported
/// in tracing rather than failing the request the caller has already answered. `true` once
/// the records are in the group, `false` when they could not be written. Call it with NO
/// state guard held — it awaits.
pub async fn delivered(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group: &str,
    log_stream: &str,
    records: &[String],
) -> bool {
    delivered_over(ctx.callback(), region, log_group, log_stream, records).await
}

/// [`delivered`] for a deliverer that holds the in-process transport ITSELF rather than a
/// request's context: a data plane served on a core-owned socket (an API Gateway WebSocket
/// api's own endpoint) has no `OperationContext` at all, and the handle is the one piece the
/// write needs.
///
/// The same write, so a record delivered from a socket and one delivered from an operation
/// cannot land differently.
pub async fn delivered_over(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
    log_stream: &str,
    records: &[String],
) -> bool {
    write(callback, region, log_group, log_stream, records, true).await
}

/// [`delivered_over`] for records that each carry the instant it OBSERVED, rather than the
/// instant they are handed over.
///
/// A deliverer whose records are already stamped — the history events of a workflow
/// execution, each written as its own step happened — delivers them at those instants, so a
/// consumer of the group reads each record in the period it belongs to instead of in the
/// period the delivery happened to be made in. Everything else is the same write
/// ([`delivered_over`]): the group and the stream are created, and the events go in through
/// CloudWatch Logs' own ingest path.
///
/// `events` are `(instant in epoch milliseconds, record)` pairs, in any order: they are
/// delivered oldest first, because "the log events in the batch must be in chronological
/// order by their timestamp" (`PutLogEvents`).
pub async fn delivered_stamped_over(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
    log_stream: &str,
    events: &[(i64, String)],
) -> bool {
    let stamped: Vec<(i64, &str)> = events
        .iter()
        .map(|(at, record)| (*at, record.as_str()))
        .collect();
    write_events(callback, region, log_group, log_stream, &stamped, true).await
}

/// [`delivered`] into a destination the service delivering is POINTED AT rather than one it
/// publishes itself: the records go into the log group CloudWatch Logs already holds under
/// that name, and no group is created.
///
/// A destination named by a caller — an API Gateway stage's `accessLogSettings`
/// `destinationArn` — is a group of the ACCOUNT's, created by whoever owns it, so a
/// delivering service writes into it and does not bring it into existence: a destination
/// naming a group that is not there loses the records and leaves the account holding no such
/// group, which is what a client reading the destination back sees. The stream inside an
/// existing group is still created, because the stream is the delivering service's own
/// ([`delivered`]).
///
/// `true` once the records are in the group, `false` when they could not be written —
/// including the absent destination, which never fails the request the caller has already
/// been answered. Call it with NO state guard held — it awaits.
pub async fn delivered_into_held_group(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group: &str,
    log_stream: &str,
    records: &[String],
) -> bool {
    delivered_into_held_group_over(ctx.callback(), region, log_group, log_stream, records).await
}

/// [`delivered_into_held_group`] for a deliverer that holds the in-process transport ITSELF
/// rather than a request's context, as [`delivered_over`] is for [`delivered`].
pub async fn delivered_into_held_group_over(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
    log_stream: &str,
    records: &[String],
) -> bool {
    write(callback, region, log_group, log_stream, records, false).await
}

/// The one vended-logs write both forms above are: the records as log events in
/// `log_stream` of `log_group`, each stamped with the instant it is delivered.
///
/// `provisions_group` is the ONE difference between them — whether the group itself is
/// created when CloudWatch Logs does not hold it — so a destination the delivering service
/// publishes under its own name and one it was merely pointed at cannot otherwise be
/// delivered to differently.
async fn write(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
    log_stream: &str,
    records: &[String],
    provisions_group: bool,
) -> bool {
    let at = epoch_millis(::std::time::SystemTime::now());
    let events: Vec<(i64, &str)> = records
        .iter()
        .map(|record| (at, record.as_str()))
        .collect();
    write_events(callback, region, log_group, log_stream, &events, provisions_group).await
}

/// The most log events ONE `PutLogEvents` request may carry. Source: that operation's own
/// documentation — "the maximum number of log events in a batch is 10,000".
const MAX_BATCH_EVENTS: usize = 10_000;

/// The most one `PutLogEvents` request may weigh. Source: "the maximum batch size is
/// 1,048,576 bytes. This size is calculated as the sum of all event messages in UTF-8, plus
/// 26 bytes for each log event".
const MAX_BATCH_BYTES: usize = 1_048_576;

/// The per-event overhead that accounting rule charges; see [`MAX_BATCH_BYTES`].
const EVENT_OVERHEAD_BYTES: usize = 26;

/// The longest span one `PutLogEvents` request's events may cover. Source: "the time span
/// in a single batch cannot exceed 24 hours. Otherwise, the operation fails".
const MAX_BATCH_SPAN_MS: i64 = 24 * 60 * 60 * 1_000;

/// Bring the log group a delivering service publishes under its OWN name into existence,
/// with no record delivered into it.
///
/// A group of the delivering service's own is one it PUBLISHES, so it exists from the moment
/// the configuration that turns its log on is accepted rather than from the first record
/// written into it: a client reads such a group to confirm the log is on, and attaches a
/// retention policy or a subscription filter to it, before anything has been logged. The
/// records themselves go in through [`delivered`], which creates the group too, so a group
/// provisioned here and one created by the first delivery are the same group.
///
/// `true` once CloudWatch Logs holds the group — including a group it already held, which is
/// the group this service has been writing to — and `false` when it could not be created.
/// Creating it is best effort, as every vended-logs write is: the configuration the caller is
/// being answered for is not failed by it. Call it with NO state guard held — it awaits.
pub async fn provisioned(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group: &str,
) -> bool {
    provisioned_over(ctx.callback(), region, log_group).await
}

/// [`provisioned`] for a deliverer that holds the in-process transport ITSELF rather than a
/// request's context, as [`delivered_over`] is for [`delivered`].
pub async fn provisioned_over(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
) -> bool {
    created_group(&logs_client(callback, region), log_group).await
}

/// Keep the records of `log_group` for `retention_in_days`, the retention the delivering
/// service's own configuration states for the group it publishes.
///
/// A service whose log configuration carries a retention period applies that period to the
/// GROUP it publishes, so a client reading the group back sees it: the period is CloudWatch
/// Logs' own property of a group (`DescribeLogGroups` reports it as `retentionInDays`), set
/// through that service's own ingest path (`PutRetentionPolicy`) rather than kept beside the
/// group in the delivering service's store, where no reader of the group would find it.
///
/// Call it on a group CloudWatch Logs already holds ([`provisioned`] or [`delivered`] put it
/// there): a period cannot be attached to a group that is not there. Best effort, as every
/// vended-logs write is — `true` once the period is on the group, `false` when it could not be
/// set, and the configuration the caller is being answered for is not failed by it. Call it
/// with NO state guard held: it awaits.
pub async fn retention_set(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    log_group: &str,
    retention_in_days: i32,
) -> bool {
    retention_set_over(ctx.callback(), region, log_group, retention_in_days).await
}

/// [`retention_set`] for a deliverer that holds the in-process transport ITSELF rather than a
/// request's context, as [`delivered_over`] is for [`delivered`].
pub async fn retention_set_over(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
    retention_in_days: i32,
) -> bool {
    match logs_client(callback, region)
        .put_retention_policy()
        .log_group_name(log_group)
        .retention_in_days(retention_in_days)
        .send()
        .await
    {
        Ok(_) => true,
        Err(error) => {
            ::tracing::warn!(
                log_group = %log_group,
                retention_in_days,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the retention this service's log configuration states could not be set on the \
                 log group it publishes"
            );
            false
        }
    }
}

/// Create `log_group` where CloudWatch Logs does not hold it, which is what a group the
/// delivering service publishes under its own name needs before anything is written to it.
///
/// A group already there is the one this service has been writing to, so that answer is this
/// one: `true` means CloudWatch Logs holds the group now.
async fn created_group(client: &::aws_sdk_cloudwatchlogs::Client, log_group: &str) -> bool {
    match client
        .create_log_group()
        .log_group_name(log_group)
        .send()
        .await
    {
        Ok(_) => true,
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_already_exists_exception()) =>
        {
            true
        }
        Err(error) => {
            ::tracing::warn!(
                log_group = %log_group,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the vended-logs log group this service publishes could not be created"
            );
            false
        }
    }
}

/// [`write`] for records that carry their own instants, which is what both forms end in.
///
/// The events are delivered in CHRONOLOGICAL order and in requests inside the three bounds
/// `PutLogEvents` publishes for one batch ([`MAX_BATCH_EVENTS`], [`MAX_BATCH_BYTES`],
/// [`MAX_BATCH_SPAN_MS`]), so a deliverer handing over more than one request's worth of
/// records delivers all of them rather than having the whole batch refused for its size.
async fn write_events(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    log_group: &str,
    log_stream: &str,
    events: &[(i64, &str)],
    provisions_group: bool,
) -> bool {
    if events.is_empty() {
        return true;
    }
    let client = logs_client(callback, region);
    if provisions_group && !created_group(&client, log_group).await {
        // The group this service publishes is not there and could not be created, so these
        // records have nowhere to go.
        ::tracing::warn!(
            log_group = %log_group,
            "the vended-logs destination could not be created, so these records are not \
             delivered"
        );
        return false;
    }
    match client
        .create_log_stream()
        .log_group_name(log_group)
        .log_stream_name(log_stream)
        .send()
        .await
    {
        Ok(_) => {}
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_already_exists_exception()) => {}
        Err(error)
            if error
                .as_service_error()
                .is_some_and(|service| service.is_resource_not_found_exception()) =>
        {
            // CloudWatch Logs holds no group of that name, and this delivery creates none:
            // the destination it was pointed at is not there, so the records are lost and
            // the account still holds no such group.
            ::tracing::warn!(
                log_group = %log_group,
                log_stream = %log_stream,
                "the vended-logs destination log group does not exist, so these records are \
                 not delivered"
            );
            return false;
        }
        Err(error) => {
            ::tracing::warn!(
                log_group = %log_group,
                log_stream = %log_stream,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the vended-logs stream could not be created, so these records are not \
                 delivered"
            );
            return false;
        }
    }
    // Oldest first, keeping the order the deliverer handed records of one instant over in:
    // a batch out of chronological order is refused whole.
    let mut ordered: Vec<(i64, &str)> = events.to_vec();
    ordered.sort_by_key(|(at, _)| *at);
    let mut delivered = true;
    for batch in batches(&ordered) {
        let inputs: Vec<::aws_sdk_cloudwatchlogs::types::InputLogEvent> = batch
            .iter()
            .filter_map(|(at, record)| {
                ::aws_sdk_cloudwatchlogs::types::InputLogEvent::builder()
                    .timestamp(*at)
                    .message(*record)
                    .build()
                    .ok()
            })
            .collect();
        let count = inputs.len();
        if let Err(error) = client
            .put_log_events()
            .log_group_name(log_group)
            .log_stream_name(log_stream)
            .set_log_events(Some(inputs))
            .send()
            .await
        {
            ::tracing::warn!(
                log_group = %log_group,
                log_stream = %log_stream,
                records = count,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "these records could not be delivered to CloudWatch Logs"
            );
            delivered = false;
        }
    }
    delivered
}

/// `ordered` split into the runs one `PutLogEvents` request carries: at most
/// [`MAX_BATCH_EVENTS`] events, weighing at most [`MAX_BATCH_BYTES`] between them and
/// covering at most [`MAX_BATCH_SPAN_MS`].
///
/// A single record past the batch weight is its own batch rather than being dropped here:
/// what CloudWatch Logs does with an event over its own per-event limit is that service's to
/// answer, and this is only the split.
fn batches<'r>(ordered: &[(i64, &'r str)]) -> Vec<Vec<(i64, &'r str)>> {
    let mut batches: Vec<Vec<(i64, &str)>> = Vec::new();
    let mut batch: Vec<(i64, &str)> = Vec::new();
    let mut bytes: usize = 0;
    let mut opened_at: i64 = 0;
    for event in ordered {
        let weight = event.1.len().saturating_add(EVENT_OVERHEAD_BYTES);
        let full = !batch.is_empty()
            && (batch.len() >= MAX_BATCH_EVENTS
                || bytes.saturating_add(weight) > MAX_BATCH_BYTES
                || event.0.saturating_sub(opened_at) > MAX_BATCH_SPAN_MS);
        if full {
            batches.push(::std::mem::take(&mut batch));
            bytes = 0;
        }
        if batch.is_empty() {
            opened_at = event.0;
        }
        batch.push(*event);
        bytes = bytes.saturating_add(weight);
    }
    if !batch.is_empty() {
        batches.push(batch);
    }
    batches
}

/// A CloudWatch Logs client over the in-process callback into this same emulator.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// routes in process, so no network client is configured here. `region` is the Region the
/// delivering service's own records name, so a record lands where the destination's ARN says
/// it does.
fn logs_client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_cloudwatchlogs::Client {
    let config = ::aws_sdk_cloudwatchlogs::Config::builder()
        .behavior_version(::aws_sdk_cloudwatchlogs::config::BehaviorVersion::latest())
        .region(::aws_sdk_cloudwatchlogs::config::Region::new(
            region.to_string(),
        ))
        .endpoint_url(format!("https://logs.{region}.amazonaws.com"))
        .credentials_provider(callback.aws_credentials_provider())
        .http_client(callback.http_client())
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials are handed over
        // directly.
        .retry_config(::aws_sdk_cloudwatchlogs::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudwatchlogs::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudwatchlogs::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudwatchlogs::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudwatchlogs::Client::from_conf(config)
}

/// `at` in milliseconds since the Unix epoch, which is the stamp a log event carries.
///
/// An instant before the epoch, which no log event can carry, clamps to the epoch rather
/// than wrapping.
fn epoch_millis(at: ::std::time::SystemTime) -> i64 {
    at.duration_since(::std::time::SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_log_group_arn_resolves_in_both_published_forms_and_nothing_else_does() {
        for (case, arn, expected) in [
            (
                "a log group",
                "arn:aws:logs:us-east-1:000000000000:log-group:/vended/one",
                Some("/vended/one"),
            ),
            (
                "a log group with everything under it",
                "arn:aws:logs:us-east-1:000000000000:log-group:aws-waf-logs-guard:*",
                Some("aws-waf-logs-guard"),
            ),
            (
                "a bucket, which names no log group",
                "arn:aws:s3:::aws-waf-logs-guard",
                None,
            ),
            ("not an arn at all", "aws-waf-logs-guard", None),
            (
                "a log group whose name no group may carry",
                "arn:aws:logs:us-east-1:000000000000:log-group:has a space",
                None,
            ),
        ] {
            let got = log_group_of_arn(arn);
            assert_eq!(
                got, expected,
                "case {case:?}: expected {expected:?} for {arn:?}, got {got:?}"
            );
        }
    }

    #[test]
    fn the_published_domain_is_admitted_and_nothing_else_is() {
        for name in [
            "a",
            "7",
            "Z",
            "_",
            "-",
            ".",
            "/",
            "#",
            "/aws/vendedlogs/states/one",
            "UPPER/lower/MiXeD-0.9_#",
        ] {
            assert!(
                is_log_group_name(name),
                "case {name:?}: CloudWatch Logs holds this name, got rejected"
            );
        }
        for name in [
            "",
            "has a space",
            "group!",
            "group$",
            "group*",
            "group:name",
            "grüße",
            "日本語",
        ] {
            assert!(
                !is_log_group_name(name),
                "case {name:?}: CloudWatch Logs holds no such group, got accepted"
            );
        }
    }

    #[test]
    fn the_length_bound_holds_at_each_published_edge() {
        for (length, acceptable) in [
            (MIN_LOG_GROUP_NAME_CHARS - 1, false),
            (MIN_LOG_GROUP_NAME_CHARS, true),
            (MIN_LOG_GROUP_NAME_CHARS + 1, true),
            (MAX_LOG_GROUP_NAME_CHARS - 1, true),
            (MAX_LOG_GROUP_NAME_CHARS, true),
            (MAX_LOG_GROUP_NAME_CHARS + 1, false),
        ] {
            let name = "g".repeat(length);
            assert_eq!(
                is_log_group_name(&name),
                acceptable,
                "case {length} characters: expected acceptable={acceptable}, got {}",
                is_log_group_name(&name)
            );
        }
    }

    /// One row of the split table: what it is about, the records handed over, and how many
    /// requests the published bounds make of them.
    struct Split {
        case: &'static str,
        events: Vec<(i64, String)>,
        requests: usize,
    }

    #[test]
    fn a_delivery_is_split_into_requests_inside_both_published_batch_bounds() {
        let kilobyte = "m".repeat(1_024);
        let rows: Vec<Split> = vec![
            Split {
                case: "one record is one request",
                events: vec![(1, "one".to_string())],
                requests: 1,
            },
            Split {
                case: "a delivery inside both bounds is one request",
                events: (0..100).map(|index| (index, kilobyte.clone())).collect(),
                requests: 1,
            },
            Split {
                case: "a delivery past the event count is split by it",
                events: (0..(MAX_BATCH_EVENTS as i64 + 1))
                    .map(|index| (index, "m".to_string()))
                    .collect(),
                requests: 2,
            },
            Split {
                case: "a delivery past the batch weight is split by it",
                events: (0..2_048).map(|index| (index, kilobyte.clone())).collect(),
                requests: 3,
            },
            Split {
                case: "a record heavier than one batch is its own request",
                events: vec![
                    (1, "m".repeat(MAX_BATCH_BYTES + 1)),
                    (2, "after".to_string()),
                ],
                requests: 2,
            },
            Split {
                case: "a delivery spanning more than one batch's span is split by it",
                events: vec![
                    (0, "first".to_string()),
                    (MAX_BATCH_SPAN_MS, "at the bound".to_string()),
                    (MAX_BATCH_SPAN_MS + 1, "past it".to_string()),
                ],
                requests: 2,
            },
        ];
        for row in rows {
            let case = row.case;
            let expected = row.requests;
            let ordered: Vec<(i64, &str)> = row
                .events
                .iter()
                .map(|(at, record)| (*at, record.as_str()))
                .collect();
            let split = batches(&ordered);
            assert_eq!(
                split.len(),
                expected,
                "case {case:?}: expected {expected} request(s), got {}",
                split.len()
            );
            for batch in &split {
                assert!(
                    batch.len() <= MAX_BATCH_EVENTS,
                    "case {case:?}: a request carries at most {MAX_BATCH_EVENTS} events, got {}",
                    batch.len()
                );
            }
            // Every record is delivered exactly once, in the order it was handed over.
            let regrouped: Vec<(i64, &str)> = split
                .iter()
                .flat_map(|batch| batch.iter().copied())
                .collect();
            assert_eq!(
                regrouped, ordered,
                "case {case:?}: expected every record once and in order, got {} of {}",
                regrouped.len(),
                ordered.len()
            );
        }
    }
}
