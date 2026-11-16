// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY a channel with a general purpose Amazon S3 destination performs: the records
//! its source streams hold, written into that bucket as objects.
//!
//! A channel "reads records from the specified stream and writes them to ... a general purpose
//! Amazon S3 bucket" (`CreateChannel`), so a channel that is `ACTIVE` is a RUNNING reader of
//! its source streams: nothing a client calls delivers a record, and the delivery is therefore
//! driven by a task of this process ([`drive_delivery_clock`]) rather than by a request. The
//! task is claimed ONCE per store, so exactly one reader of a channel's streams exists and no
//! record is delivered twice.
//!
//! What one delivery does:
//!
//! * it reads each source stream's shards from where this channel last got to — the delivery
//!   CURSOR, one sequence number per shard ([`Channel::delivered`]) — so each record is read
//!   once, and a record that arrived before the channel was created is not delivered at all:
//!   the channel was not reading the stream then;
//! * it writes what it read as ONE object per flush, holding those records' own bytes (the
//!   blob "is both opaque and immutable to Kinesis Data Streams, which does not inspect,
//!   interpret, or change the data in the blob in any way", `Record$Data`), one record per
//!   line, compressed as the destination's `CompressionType` asks;
//! * the object is written into the destination bucket THROUGH Amazon S3's own data plane,
//!   over the emulator's in-process transport, so a delivered object is an object that
//!   service holds and `GetObject` / `ListObjectsV2` read it back like any other;
//! * the cursor advances only once Amazon S3 has TAKEN the object, so a destination that
//!   cannot take it (a bucket that does not exist, a bucket owned by another account) leaves
//!   the records undelivered for a later attempt instead of losing them.
//!
//! `DataFreshnessInSeconds` is "the maximum age, in seconds, of undelivered data before the
//! channel delivers it to the destination" — an upper bound on how long a record may wait, not
//! a wait: this delivery flushes what it finds at each wake-up, which is inside that bound for
//! every value the member admits.
//!
//! Nothing here leaves the process, and nothing here is synthesised: the bytes written are the
//! producer's own, and the object is recorded by the service that owns objects.
//!
//! LOCK ORDER: the channel store is taken and RELEASED before Amazon S3 is called, and taken
//! again to advance the cursor — no guard is held across an await.

use ::std::io::Write;
use ::std::sync::Arc;
use ::std::time::{Duration, SystemTime};

use super::{
    sequence_number_le, Channel, ComAmazonawsKinesisKinesis20131202State, RegionalName,
    CHANNEL_ACTIVE,
};

/// How often an active channel's source streams are polled for records to deliver.
///
/// The span is this emulator's own: AWS states only the upper bound a record may wait
/// (`S3DestinationConfiguration$DataFreshnessInSeconds`), so the delivery is made promptly and
/// well inside it. It is never zero, so a wake-up costs a sleep rather than a spin.
pub const DELIVERY_POLL_SPAN: Duration = Duration::from_millis(250);

/// How long a channel whose destination did not take a delivery waits before trying again.
///
/// The emulator's own back-off, for the same reason the poll span is one: a destination that
/// cannot take an object (it does not exist yet, or it belongs to another account) must not be
/// written to at the poll rate, and the records stay undelivered either way.
pub const DELIVERY_RETRY_SPAN: Duration = Duration::from_secs(2);

/// How many records ONE delivery may carry. The emulator's own ceiling: it bounds the object
/// one flush builds in memory, and what is left over is delivered by the next wake-up.
const MAX_RECORDS_PER_DELIVERY: usize = 500;

/// How many bytes of record data ONE delivery may carry, bounding the same object.
const MAX_BYTES_PER_DELIVERY: usize = 4 * 1024 * 1024;

/// How many wake-ups one delivery task serves before handing over to a fresh one, so no task
/// runs unboundedly long.
const MAX_DELIVERY_WAKE_UPS: usize = 4_000;

/// The media type a delivered object is stored with: the channel writes the records' own
/// opaque bytes, so the object carries the type of an opaque byte stream, and what those bytes
/// were encoded with is stated by the object's content coding instead.
const DELIVERED_CONTENT_TYPE: &str = "application/octet-stream";

/// What separates two records inside one delivered object, so an object holding a flush of
/// several records is readable as one record per line.
const RECORD_DELIMITER: &[u8] = b"\n";

/// The template a destination that names no `OutputKeyTemplate` delivers under: "If not
/// specified, a default template is used" (`S3StorageConfiguration$OutputKeyTemplate`), whose
/// text AWS does not state — so this is the emulator's own, built from the variables below: the
/// channel, the calendar fields of the delivery's instant, and the first record it carries,
/// which is what keeps two deliveries of one channel at two keys.
pub const DEFAULT_OUTPUT_KEY_TEMPLATE: &str =
    "!{channel-name}/!{yyyy}/!{MM}/!{dd}/!{HH}/!{mm}/!{ss}/!{sequence-number}!{extension}";

/// What opens a VARIABLE of a key template, and what closes one: a template states a variable
/// as `!{<variable>}`, which is the form the refusal of an invalid one spells it in
/// ([`invalid_output_key_template_variable_message`]). Text outside a variable is the key as
/// written.
const VARIABLE_OPEN: &str = "!{";
const VARIABLE_CLOSE: char = '}';

/// The variable standing for the name of the channel making the delivery.
const CHANNEL_NAME_VARIABLE: &str = "channel-name";

/// The variable standing for the id of that channel.
const CHANNEL_ID_VARIABLE: &str = "channel-id";

/// The variables standing for the calendar fields of the delivery's instant, in UTC, each
/// named by the pattern letters that field is written with and rendered zero-padded to that
/// many digits.
const YEAR_VARIABLE: &str = "yyyy";
const MONTH_VARIABLE: &str = "MM";
const DAY_VARIABLE: &str = "dd";
const HOUR_VARIABLE: &str = "HH";
const MINUTE_VARIABLE: &str = "mm";
const SECOND_VARIABLE: &str = "ss";

/// The variable standing for the sequence number of the first record the object holds. Every
/// delivery of one channel starts at a record no earlier delivery carried, so this is what
/// tells two deliveries made inside one second apart.
const SEQUENCE_NUMBER_VARIABLE: &str = "sequence-number";

/// The variable standing for the delivered object's file extension: the suffix the coding its
/// bytes carry is named by, and nothing at all for an object delivered uncompressed
/// ([`object_extension`]).
const EXTENSION_VARIABLE: &str = "extension";

/// The variables a key template may name.
///
/// The model states no set of them, so the set a template may name is the set this service
/// EXPANDS: a template naming one it does not expand would render the variable's own text into
/// the key instead of a value, which is not the key the caller asked for — so it is refused
/// rather than delivered under ([`invalid_output_key_template_variable_message`]).
const OUTPUT_KEY_TEMPLATE_VARIABLES: &[&str] = &[
    CHANNEL_NAME_VARIABLE,
    CHANNEL_ID_VARIABLE,
    YEAR_VARIABLE,
    MONTH_VARIABLE,
    DAY_VARIABLE,
    HOUR_VARIABLE,
    MINUTE_VARIABLE,
    SECOND_VARIABLE,
    SEQUENCE_NUMBER_VARIABLE,
    EXTENSION_VARIABLE,
];

/// The `CompressionType` values a destination's objects are encoded under, and the content
/// coding each one names: "NONE - No compression", "GZIP - gzip compression", "ZSTD -
/// Zstandard compression" (`S3StorageConfiguration$CompressionType`).
const COMPRESSION_NONE: &str = "NONE";
const COMPRESSION_GZIP: &str = "GZIP";
const COMPRESSION_ZSTD: &str = "ZSTD";

/// Where this channel last got to in one shard of one source stream, as a key of
/// [`Channel::delivered`]: the stream's ARN and the shard's id, which together name the shard
/// a sequence number belongs to.
pub fn cursor_key(stream_arn: &str, shard_id: &str) -> String {
    format!("{stream_arn}#{shard_id}")
}

/// The ARN vendor (service namespace) of a bucket, which a destination's `BucketARN` has to
/// name: `^arn:aws[-a-z0-9]*:s3:::[a-z0-9._-]{3,63}$` (`S3StorageConfiguration$BucketARN`).
const S3_ARN_VENDOR: &str = "s3";

/// The bucket a `BucketARN` names: a bucket ARN names the bucket as its whole resource field
/// (an account-less, Region-less resource), so it is what the tree's ONE reading of an ARN
/// reports as that field (`service_common::arn`). `None` for a value that is not a
/// well-formed Amazon S3 resource ARN.
pub fn bucket_name(bucket_arn: &str) -> Option<&str> {
    let arn = ::service_common::arn::parse_of(bucket_arn, S3_ARN_VENDOR)?;
    (!arn.resource.is_empty()).then_some(arn.resource)
}

/// How `CreateChannel` refuses an `OutputKeyTemplate` whose text does not name a key inside
/// the key space it states: FIXED text, which is also the statement of that rule.
pub const OUTPUT_KEY_TEMPLATE_INVALID_MESSAGE: &str =
    "OutputKeyTemplate must not contain consecutive periods, period-only path segments, or \
     start with /";

/// How it refuses one whose separators name no segment between them: FIXED text too.
pub const OUTPUT_KEY_TEMPLATE_CONSECUTIVE_SLASHES_MESSAGE: &str =
    "OutputKeyTemplate must not contain consecutive slashes (//)";

/// How it refuses one whose variable is opened and never closed: FIXED text too.
pub const OUTPUT_KEY_TEMPLATE_UNCLOSED_VARIABLE_MESSAGE: &str =
    "OutputKeyTemplate contains an unclosed variable placeholder";

/// How it refuses one naming a variable the service does not expand
/// ([`OUTPUT_KEY_TEMPLATE_VARIABLES`]): built FROM the template, so the variable at fault
/// names it, in the `!{<variable>}` form a template states one in.
pub fn invalid_output_key_template_variable_message(variable: &str) -> String {
    format!("OutputKeyTemplate contains invalid variable: {VARIABLE_OPEN}{variable}{VARIABLE_CLOSE}")
}

/// Why a caller's `OutputKeyTemplate` is not a template for an object key, if it is not.
///
/// The template "is used to construct the Amazon S3 object key for delivered objects"
/// (`S3StorageConfiguration$OutputKeyTemplate`), so every key it renders has to lie inside the
/// key space the template itself states, and every part of it has to render something. What is
/// refused, wherever in the template it appears:
///
/// * CONSECUTIVE PERIODS, which are how a key escapes the prefix the rest of the template
///   states — the condition a client relies on the destination staying inside — and a
///   PERIOD-ONLY path segment, which names the position it sits in rather than an object under
///   it, and a leading `/`, which names no first segment at all
///   ([`OUTPUT_KEY_TEMPLATE_INVALID_MESSAGE`]);
/// * CONSECUTIVE SLASHES, which name no segment between them
///   ([`OUTPUT_KEY_TEMPLATE_CONSECUTIVE_SLASHES_MESSAGE`]);
/// * a VARIABLE that is opened and never closed, which renders no value at all
///   ([`OUTPUT_KEY_TEMPLATE_UNCLOSED_VARIABLE_MESSAGE`]);
/// * a VARIABLE this service does not expand, which would put its own text in the key instead
///   of a value ([`invalid_output_key_template_variable_message`]).
///
/// The sentence alone is answered, so the caller's operation refuses it with the error ITS
/// model declares for an input the service validates before it provisions anything.
pub fn output_key_template_refusal(template: &str) -> Option<String> {
    let names_an_object_key = !template.contains("..")
        && !template.starts_with('/')
        && !template
            .split('/')
            .any(|segment| !segment.is_empty() && segment.chars().all(|period| period == '.'));
    if !names_an_object_key {
        return Some(OUTPUT_KEY_TEMPLATE_INVALID_MESSAGE.to_string());
    }
    if template.contains("//") {
        return Some(OUTPUT_KEY_TEMPLATE_CONSECUTIVE_SLASHES_MESSAGE.to_string());
    }
    for variable in template.split(VARIABLE_OPEN).skip(1) {
        let Some((name, _)) = variable.split_once(VARIABLE_CLOSE) else {
            return Some(OUTPUT_KEY_TEMPLATE_UNCLOSED_VARIABLE_MESSAGE.to_string());
        };
        if !OUTPUT_KEY_TEMPLATE_VARIABLES.contains(&name) {
            return Some(invalid_output_key_template_variable_message(name));
        }
    }
    None
}

/// What one delivery's key is rendered FROM: the channel making it, the instant it is made at,
/// and the first record the object holds. One value per variable of
/// [`OUTPUT_KEY_TEMPLATE_VARIABLES`], so the catalog and the rendering cannot disagree about
/// which variables exist.
#[derive(Debug, Clone, Copy)]
struct KeyParts<'delivery> {
    /// `ChannelName`, which "is unique within your Amazon Web Services account and Amazon Web
    /// Services Region" (`CreateChannelInput$ChannelName`).
    channel_name: &'delivery str,
    /// `ChannelId`, the id the create minted for the channel.
    channel_id: &'delivery str,
    /// The sequence number of the first record the delivered object holds.
    sequence_number: &'delivery str,
    /// The file extension the delivered object's own bytes earn ([`object_extension`]).
    extension: &'delivery str,
    /// The instant the delivery is made at.
    at: SystemTime,
}

impl KeyParts<'_> {
    /// What `variable` stands for in this delivery, or `None` for one this service does not
    /// expand — which a template cannot carry, because the create naming it was refused.
    fn value_of(self, variable: &str) -> Option<String> {
        match variable {
            CHANNEL_NAME_VARIABLE => Some(self.channel_name.to_string()),
            CHANNEL_ID_VARIABLE => Some(self.channel_id.to_string()),
            SEQUENCE_NUMBER_VARIABLE => Some(self.sequence_number.to_string()),
            EXTENSION_VARIABLE => Some(self.extension.to_string()),
            _ => calendar_field(self.at, variable),
        }
    }
}

/// The key one delivery is written under: [`rendered_key`], made UNIQUE to this delivery.
///
/// Two deliveries of one channel must never write one key: the second would replace the
/// records of the first, which were already counted as delivered and would then be readable
/// nowhere. The sequence number of the first record an object holds is unique inside the
/// channel — every delivery starts past the cursor the last one advanced — so a template that
/// names `!{sequence-number}` already renders a key of its own, and one that does not has that
/// sequence number appended to what it rendered.
fn delivery_key(template: &str, parts: KeyParts<'_>) -> String {
    let rendered = rendered_key(template, parts);
    if template.contains(&format!(
        "{VARIABLE_OPEN}{SEQUENCE_NUMBER_VARIABLE}{VARIABLE_CLOSE}"
    )) {
        return rendered;
    }
    format!("{rendered}-{}", parts.sequence_number)
}

/// The key `template` renders for this delivery: each VARIABLE this service expands replaced
/// by what it stands for ([`KeyParts`]). Text outside a variable is the key as written, and a
/// template naming a variable this service does not expand never reaches here: the create that
/// named it was refused ([`output_key_template_refusal`]).
fn rendered_key(template: &str, parts: KeyParts<'_>) -> String {
    let mut key = template.to_string();
    for variable in OUTPUT_KEY_TEMPLATE_VARIABLES {
        let Some(value) = parts.value_of(variable) else {
            continue;
        };
        key = key.replace(&format!("{VARIABLE_OPEN}{variable}{VARIABLE_CLOSE}"), &value);
    }
    key
}

/// The file extension an object delivered under `compression` carries: the suffix that
/// coding's own format is named by, and NOTHING for an object delivered uncompressed, whose
/// bytes are the records' own and carry no container of their own.
fn object_extension(compression: &str) -> &'static str {
    match compression {
        COMPRESSION_GZIP => ".gz",
        COMPRESSION_ZSTD => ".zst",
        _ => "",
    }
}

/// The calendar field `variable` names, read off `at` in UTC and zero-padded to the width the
/// variable's own pattern letters state; `None` when `variable` names no calendar field, and
/// for an instant the calendar cannot place (`SystemTime` reaches further than the date library
/// does, so such an instant is answered with no field instead of panicking inside the
/// conversion).
fn calendar_field(at: SystemTime, variable: &str) -> Option<String> {
    use ::chrono::{Datelike, Timelike};

    let since_epoch = at.duration_since(SystemTime::UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(since_epoch.as_secs()).ok()?;
    let placed = ::chrono::DateTime::from_timestamp(seconds, 0)?;
    Some(match variable {
        YEAR_VARIABLE => format!("{:04}", placed.year()),
        MONTH_VARIABLE => format!("{:02}", placed.month()),
        DAY_VARIABLE => format!("{:02}", placed.day()),
        HOUR_VARIABLE => format!("{:02}", placed.hour()),
        MINUTE_VARIABLE => format!("{:02}", placed.minute()),
        SECOND_VARIABLE => format!("{:02}", placed.second()),
        _ => return None,
    })
}

/// `body` encoded as `compression` asks, beside the content coding the encoded object carries.
/// `None` for a value no coding is known for, which delivers nothing rather than an object
/// whose bytes do not match what the destination says they are.
fn compressed(body: Vec<u8>, compression: &str) -> Option<(Vec<u8>, Option<String>)> {
    match compression {
        COMPRESSION_NONE => Some((body, None)),
        COMPRESSION_GZIP => {
            let mut encoder =
                ::flate2::write::GzEncoder::new(Vec::new(), ::flate2::Compression::default());
            encoder.write_all(&body).ok()?;
            Some((
                encoder.finish().ok()?,
                Some(COMPRESSION_GZIP.to_ascii_lowercase()),
            ))
        }
        COMPRESSION_ZSTD => Some((
            ::ruzstd::encoding::compress_to_vec(
                body.as_slice(),
                ::ruzstd::encoding::CompressionLevel::Fastest,
            ),
            Some(COMPRESSION_ZSTD.to_ascii_lowercase()),
        )),
        _ => None,
    }
}

/// ONE object a channel owes its destination, with everything the write needs and the cursor
/// it advances once the write is taken.
#[derive(Debug)]
struct PendingDelivery {
    /// The channel the delivery belongs to, as its record is keyed.
    channel: RegionalName,
    /// The destination bucket, its expected owner, and the key the object is written under.
    bucket: String,
    expected_bucket_owner: String,
    key: String,
    /// The object's bytes, already encoded, and the coding they carry.
    body: Vec<u8>,
    content_encoding: Option<String>,
    /// "The Amazon S3 storage class for delivered objects"
    /// (`S3StorageConfiguration$StorageClass`), when the destination names one.
    storage_class: Option<String>,
    /// Where each shard's cursor stands once this object is taken.
    cursors: Vec<(String, String)>,
}

/// Every object the store owes its destinations at `now`, read from the channels and the
/// streams as they stand. Writes nothing: the cursor advances only once the destination has
/// taken the object ([`deliver_due`]).
fn take_due(
    store: &ComAmazonawsKinesisKinesis20131202State,
    now: SystemTime,
) -> Vec<PendingDelivery> {
    let mut pending = Vec::new();
    for (key, channel) in &store.channels {
        // "After provisioning is complete, Amazon Kinesis Data Streams sets the state to
        // ACTIVE" (`CreateChannel`): a channel that is not active yet is not reading its
        // streams, and one whose last delivery was refused waits out its back-off.
        if channel.status(now) != CHANNEL_ACTIVE
            || channel.delivery_retry_at.is_some_and(|at| now < at)
        {
            continue;
        }
        let Some(destination) = channel.s3_destination.as_ref() else {
            continue;
        };
        let storage = &destination.storage_configuration;
        let Some(bucket) = bucket_name(&storage.bucket_arn) else {
            continue;
        };
        let (body, cursors, first) = undelivered(store, channel, now);
        let Some(first) = first else {
            continue;
        };
        let Some((body, content_encoding)) = compressed(body, &storage.compression_type) else {
            ::tracing::warn!(
                channel = %channel.name,
                compression_type = ?storage.compression_type,
                "no content coding is known for this destination's compression type, so \
                 nothing was delivered for it"
            );
            continue;
        };
        pending.push(PendingDelivery {
            channel: key.clone(),
            bucket: bucket.to_string(),
            expected_bucket_owner: storage.expected_bucket_owner.clone(),
            key: delivery_key(
                storage
                    .output_key_template
                    .as_deref()
                    .unwrap_or(DEFAULT_OUTPUT_KEY_TEMPLATE),
                KeyParts {
                    channel_name: &channel.name,
                    channel_id: &channel.id,
                    sequence_number: &first,
                    extension: object_extension(&storage.compression_type),
                    at: now,
                },
            ),
            body,
            content_encoding,
            storage_class: storage.storage_class.clone(),
            cursors,
        });
    }
    pending
}

/// What `channel` has not delivered yet: the bytes of those records, where each shard's cursor
/// would then stand, and the sequence number of the FIRST of them (`None` when there is
/// nothing to deliver).
fn undelivered(
    store: &ComAmazonawsKinesisKinesis20131202State,
    channel: &Channel,
    now: SystemTime,
) -> (Vec<u8>, Vec<(String, String)>, Option<String>) {
    let mut body = Vec::new();
    let mut cursors = Vec::new();
    let mut first = None;
    let mut records = 0usize;
    for source in &channel.streams {
        // A source stream this account and region no longer hold has nothing to read: a
        // stream that was deleted takes its records with it.
        let Some(stream) = store.stream_by_arn(now, &channel.region, &source.stream_arn) else {
            continue;
        };
        let trim_horizon = stream.trim_horizon(now);
        for shard in &stream.shards {
            let cursor = cursor_key(&source.stream_arn, &shard.shard_id);
            let delivered = channel.delivered.get(&cursor);
            let mut last = None;
            for record in shard.retained(trim_horizon) {
                if records >= MAX_RECORDS_PER_DELIVERY || body.len() >= MAX_BYTES_PER_DELIVERY {
                    break;
                }
                // The channel reads its streams from the instant it was created: a record
                // that arrived before that was not written into a channel's source.
                if record.arrived_at < channel.created_at {
                    continue;
                }
                if delivered
                    .is_some_and(|seen| sequence_number_le(&record.sequence_number, seen))
                {
                    continue;
                }
                if first.is_none() {
                    first = Some(record.sequence_number.clone());
                }
                body.extend_from_slice(&record.data);
                body.extend_from_slice(RECORD_DELIMITER);
                records += 1;
                last = Some(record.sequence_number.clone());
            }
            if let Some(last) = last {
                cursors.push((cursor, last));
            }
        }
    }
    (body, cursors, first)
}

/// Write every object the store owes at `now` into its destination bucket, and advance the
/// cursor of each channel whose object Amazon S3 took.
pub async fn deliver_due<Eng: Send + Sync + 'static>(
    engine: &Eng,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    now: SystemTime,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    // The guard is taken to read what is owed and RELEASED before Amazon S3 is called.
    let pending = take_due(&state.lock(), now);
    for mut delivery in pending {
        let region = delivery.channel.region.clone();
        // The body is MOVED into the request, so the object's bytes are held once.
        let body = ::std::mem::take(&mut delivery.body);
        // The ONE in-process Amazon S3 client an emulated service writes an object through
        // (`service_common::s3_client`): the bucket a channel delivers to is this emulator's
        // own, so the delivery travels over the emulator's own transport and never leaves the
        // process.
        let written = ::service_common::s3_client::s3_client(callback, &region)
            .put_object()
            .bucket(&delivery.bucket)
            .key(&delivery.key)
            .body(::aws_sdk_s3::primitives::ByteStream::from(body))
            .content_type(DELIVERED_CONTENT_TYPE)
            .set_content_encoding(delivery.content_encoding.take())
            .set_storage_class(
                delivery
                    .storage_class
                    .as_deref()
                    .map(::aws_sdk_s3::types::StorageClass::from),
            )
            // "This value helps prevent delivery to an unintended bucket if ownership
            // changes" (`S3StorageConfiguration$ExpectedBucketOwner`), so the write states
            // it and Amazon S3 is what checks it.
            .expected_bucket_owner(&delivery.expected_bucket_owner)
            .send()
            .await;
        let mut store = state.lock();
        let Some(channel) = store.channels.get_mut(&delivery.channel) else {
            // The channel was deleted while its object was being written; there is no cursor
            // left to advance.
            continue;
        };
        match written {
            Ok(_) => {
                // The destination has the object, so these records are delivered and no
                // later delivery reads them again.
                for (cursor, sequence_number) in delivery.cursors {
                    channel.delivered.insert(cursor, sequence_number);
                }
                channel.delivery_retry_at = None;
            }
            Err(error) => {
                channel.delivery_retry_at = Some(now + DELIVERY_RETRY_SPAN);
                ::tracing::warn!(
                    channel = %channel.name,
                    bucket = %delivery.bucket,
                    key = %delivery.key,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the destination bucket did not take this channel's delivery, so its \
                     records stay undelivered"
                );
            }
        }
    }
}

/// Start the ONE task that delivers this store's channels, if none runs already.
///
/// A channel delivers for as long as it exists, because its data arrives at its source streams
/// and not through a call of this service — so the task runs while the store holds a channel
/// with a general purpose bucket destination, and stands down as soon as it holds none. Called
/// by the operations that can make a delivery owed; a call that finds a task running starts
/// nothing.
pub fn drive_delivery_clock<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
) {
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        ::tracing::warn!("no async runtime is available to deliver this channel's records on");
        return;
    };
    let Some(generation) = start_pump(&mut state.lock()) else {
        return;
    };
    let engine = Arc::clone(engine);
    // The callback is an owned handle into this same emulator, so the task outlives the
    // request that started it.
    let callback = callback.clone();
    let driven = ::engine::ServiceState::clone(&state);
    let handle = runtime.spawn(async move {
        // `true` only if the wake-up cap is what ended the loop; a task that ran out of work
        // has already released its claim inside the one critical section that observed it.
        let mut capped = true;
        for _ in 0..MAX_DELIVERY_WAKE_UPS {
            if !delivers_anything_or_release_pump(&mut state.lock(), generation) {
                capped = false;
                break;
            }
            // Every wake-up waits, which is what keeps the delivery off a CPU-bound spin and
            // hands the runtime back between wake-ups.
            ::tokio::time::sleep(DELIVERY_POLL_SPAN).await;
            deliver_due(&*engine, &callback, SystemTime::now()).await;
        }
        if capped {
            let still_owed = {
                let mut store = state.lock();
                forget_pump(&mut store, generation);
                delivers_anything(&store)
            };
            // A channel is delivered for as long as it exists and no further call of this
            // service is coming, so the cap HANDS OVER rather than standing down on work that
            // is still owed: a fresh task takes the claim this one released.
            if still_owed {
                drive_delivery_clock(&engine, &callback);
            }
        }
    });
    // The handle is kept so the task it replaces is aborted rather than merely forgotten.
    set_pump(&mut driven.lock(), generation, handle);
}

/// Whether the store holds a channel that delivers to a general purpose bucket at all.
fn delivers_anything(store: &ComAmazonawsKinesisKinesis20131202State) -> bool {
    store
        .channels
        .values()
        .any(|channel| channel.s3_destination.is_some())
}

/// [`delivers_anything`], RELEASING the claim `generation` holds when the answer is no.
///
/// Observing "nothing is owed" and standing down happen in ONE critical section, which is what
/// makes the hand-over race-free: a create holds this same lock to insert its channel BEFORE
/// it tries to claim the task, so either this call sees that channel and the running task keeps
/// it, or the create's claim finds the flag cleared and starts a task of its own.
fn delivers_anything_or_release_pump(
    store: &mut ComAmazonawsKinesisKinesis20131202State,
    generation: u64,
) -> bool {
    if delivers_anything(store) {
        return true;
    }
    release_pump(store, generation);
    false
}

/// Claim the single delivery task for this store: the claim's GENERATION when the caller may
/// start one, `None` when one already runs.
fn start_pump(store: &mut ComAmazonawsKinesisKinesis20131202State) -> Option<u64> {
    if store.delivery_task_running {
        return None;
    }
    store.delivery_task_running = true;
    store.delivery_task_generation = store.delivery_task_generation.wrapping_add(1);
    Some(store.delivery_task_generation)
}

/// Release the claim `generation` took — and ONLY that one, so a task that ended on the
/// wake-up cap cannot clear a claim a task started since is holding.
fn release_pump(store: &mut ComAmazonawsKinesisKinesis20131202State, generation: u64) {
    if store.delivery_task_generation == generation {
        store.delivery_task_running = false;
    }
}

/// Release the claim `generation` took AND drop the handle stored for it: what a task handing
/// over to a fresh one calls, since the task it starts would otherwise abort it as the handle
/// it replaces ([`set_pump`]). Dropping a handle detaches the task rather than cancelling it.
fn forget_pump(store: &mut ComAmazonawsKinesisKinesis20131202State, generation: u64) {
    if store.delivery_task_generation != generation {
        return;
    }
    release_pump(store, generation);
    store.delivery_task = None;
}

/// Keep the running task's handle, aborting the one it replaces — and only while `generation`
/// is still the CURRENT claim, so a starter whose own task has already stood down installs
/// nothing and aborts nothing.
fn set_pump(
    store: &mut ComAmazonawsKinesisKinesis20131202State,
    generation: u64,
    handle: ::tokio::task::JoinHandle<()>,
) {
    if store.delivery_task_generation != generation {
        return;
    }
    if let Some(previous) = store.delivery_task.replace(handle) {
        previous.abort();
    }
}

#[cfg(test)]
mod tests {
    //! The two rules this module owns that need no destination to state: which key templates
    //! name an object key, and what one renders to. The delivery itself is driven end to end,
    //! over the wire, in `core/router/tests/all/kinesis_channel_s3_delivery.rs`.

    use super::*;

    /// The parts every rendering case below renders from: one channel, one instant, one
    /// sequence number.
    fn parts() -> KeyParts<'static> {
        KeyParts {
            channel_name: "a-channel",
            channel_id: "ch4nn3lid",
            sequence_number: "49610000000000001",
            extension: ".gz",
            at: SystemTime::UNIX_EPOCH + Duration::from_millis(1_767_225_600_123),
        }
    }

    /// A template that renders a key inside the key space it states is accepted, and one that
    /// does not is refused with the sentence its own fault earns — over a SET of forms, not
    /// only the recorded one.
    #[test]
    fn a_key_template_that_cannot_render_a_key_is_refused() {
        // (the template, the refusal it earns, or `None` when it names an object key). Every
        // accepted row is inside the modelled `@pattern` `^[0-9A-Za-z!\-_'.*()\/=:{}]+$`
        // (`smithy_query(query="shape", shape="com.amazonaws.kinesis#S3StorageConfiguration")`).
        let invalid = OUTPUT_KEY_TEMPLATE_INVALID_MESSAGE.to_string();
        let slashes = OUTPUT_KEY_TEMPLATE_CONSECUTIVE_SLASHES_MESSAGE.to_string();
        let unclosed = OUTPUT_KEY_TEMPLATE_UNCLOSED_VARIABLE_MESSAGE.to_string();
        let rows: [(&str, Option<String>); 19] = [
            ("records", None),
            ("records/!{yyyy}/!{MM}/!{dd}", None),
            (
                "a/b/c/!{channel-name}/!{HH}!{mm}!{ss}/!{sequence-number}",
                None,
            ),
            ("a.b-c_d'e!f(g)h=i:j", None),
            // A brace that opens no variable is text, not a variable: a variable opens with
            // `!{`.
            ("records/{yyyy}", None),
            ("a/", None),
            ("a/../b", Some(invalid.clone())),
            ("../b", Some(invalid.clone())),
            ("a/..", Some(invalid.clone())),
            ("a..b", Some(invalid.clone())),
            ("a/./b", Some(invalid.clone())),
            (".", Some(invalid.clone())),
            ("a/.", Some(invalid.clone())),
            ("/a", Some(invalid)),
            ("a//b", Some(slashes.clone())),
            ("a/!{yyyy}//b", Some(slashes)),
            ("a/!{yyyy", Some(unclosed.clone())),
            ("!{", Some(unclosed)),
            (
                "records/!{partition-id}",
                Some(invalid_output_key_template_variable_message("partition-id")),
            ),
        ];
        for (template, refusal) in rows {
            let answered = output_key_template_refusal(template);
            assert_eq!(
                answered, refusal,
                "case {template:?}: expected {refusal:?}, got {answered:?}"
            );
        }
    }

    /// Every variable this service expands is one a template may name, and the other way
    /// round: the catalog the renderer reads and the one validation checks are ONE.
    #[test]
    fn every_variable_a_template_may_name_is_one_the_renderer_expands() {
        for variable in OUTPUT_KEY_TEMPLATE_VARIABLES {
            let template = format!("r/{VARIABLE_OPEN}{variable}{VARIABLE_CLOSE}");
            assert_eq!(
                output_key_template_refusal(&template),
                None,
                "case {variable:?}: a variable the renderer expands is admitted"
            );
            let rendered = rendered_key(&template, parts());
            assert!(
                !rendered.contains(VARIABLE_OPEN),
                "case {variable:?}: a variable a template may name is expanded, got \
                 {rendered:?}"
            );
        }
        assert_eq!(
            output_key_template_refusal(DEFAULT_OUTPUT_KEY_TEMPLATE),
            None,
            "the default template is one a caller could have named"
        );
    }

    /// The rendered key replaces the variables this service expands and keeps everything else
    /// as written.
    #[test]
    fn a_rendered_key_replaces_the_variables_this_service_expands() {
        // (the template, the key it renders from the parts above). The instant is
        // 2026-01-01T00:00:00.123Z.
        let rows = [
            (
                "records/!{yyyy}/!{MM}/!{dd}/!{HH}/!{mm}/!{ss}",
                "records/2026/01/01/00/00/00".to_string(),
            ),
            ("!{sequence-number}", "49610000000000001".to_string()),
            (
                "!{channel-name}/!{channel-id}",
                "a-channel/ch4nn3lid".to_string(),
            ),
            (
                "r/!{yyyy}!{MM}!{dd}/!{sequence-number}!{extension}",
                "r/20260101/49610000000000001.gz".to_string(),
            ),
            ("r/{yyyy}", "r/{yyyy}".to_string()),
            ("records", "records".to_string()),
        ];
        for (template, expected) in rows {
            let rendered = rendered_key(template, parts());
            assert_eq!(
                rendered, expected,
                "case {template:?}: expected {expected:?}, got {rendered:?}"
            );
        }
    }

    /// No two deliveries of one channel write one key, whatever the template names: a
    /// template that states no varying variable has the first record's sequence number
    /// appended, so the records of an earlier delivery are never replaced.
    #[test]
    fn two_deliveries_of_one_channel_never_write_one_key() {
        // (case, the template, the key each of two deliveries writes). The two deliveries
        // differ only in the record they start at, which is what makes them two deliveries.
        let first = parts();
        let second = KeyParts {
            sequence_number: "49610000000000002",
            ..first
        };
        let rows = [
            ("a constant template", "records"),
            ("a template of calendar fields alone", "r/!{yyyy}/!{MM}"),
            ("a template naming the channel", "!{channel-name}/!{channel-id}"),
            ("a template naming the record", "r/!{sequence-number}"),
            ("the default template", DEFAULT_OUTPUT_KEY_TEMPLATE),
        ];
        for (case, template) in rows {
            let one = delivery_key(template, first);
            let two = delivery_key(template, second);
            assert_ne!(
                one, two,
                "case {case:?}: expected two deliveries at two keys, both wrote {one:?}"
            );
            assert!(
                one.contains(first.sequence_number) && two.contains(second.sequence_number),
                "case {case:?}: each key names the record its object starts at, got {one:?} \
                 and {two:?}"
            );
        }
    }

    /// A delivered object's bytes are encoded as the destination's `CompressionType` asks, and
    /// carry the content coding that names.
    #[test]
    fn a_body_is_encoded_as_the_compression_type_asks() {
        let body = b"one\ntwo\n".to_vec();
        let (plain, coding) =
            compressed(body.clone(), COMPRESSION_NONE).expect("NONE names a coding");
        assert_eq!(plain, body, "case NONE: the bytes are delivered as they are");
        assert_eq!(coding, None, "case NONE: no content coding is stated");

        let (gzipped, coding) =
            compressed(body.clone(), COMPRESSION_GZIP).expect("GZIP names a coding");
        assert_eq!(
            gzipped.get(..2),
            Some([0x1f, 0x8b].as_slice()),
            "case GZIP: expected a gzip stream, got {:?}",
            gzipped.get(..2)
        );
        assert_eq!(coding.as_deref(), Some("gzip"), "case GZIP: the coding");

        let (zstd, coding) = compressed(body, COMPRESSION_ZSTD).expect("ZSTD names a coding");
        assert_eq!(
            zstd.get(..4),
            Some([0x28, 0xb5, 0x2f, 0xfd].as_slice()),
            "case ZSTD: expected a Zstandard frame, got {:?}",
            zstd.get(..4)
        );
        assert_eq!(coding.as_deref(), Some("zstd"), "case ZSTD: the coding");

        assert!(
            compressed(Vec::new(), "BROTLI").is_none(),
            "case a compression type with no known coding: nothing is delivered"
        );
    }

    /// The extension a delivered object's key carries is the one its coding is named by.
    #[test]
    fn a_delivered_objects_extension_names_its_coding() {
        for (compression, extension) in [
            (COMPRESSION_NONE, ""),
            (COMPRESSION_GZIP, ".gz"),
            (COMPRESSION_ZSTD, ".zst"),
        ] {
            let named = object_extension(compression);
            assert_eq!(
                named, extension,
                "case {compression:?}: expected the extension {extension:?}, got {named:?}"
            );
        }
    }

    /// The bucket a destination's `BucketARN` names.
    #[test]
    fn a_bucket_arn_names_its_bucket() {
        for (arn, bucket) in [
            ("arn:aws:s3:::channel-bucket", Some("channel-bucket")),
            ("arn:aws-cn:s3:::a.b-c", Some("a.b-c")),
            ("arn:aws:s3:::", None),
            ("channel-bucket", None),
        ] {
            assert_eq!(
                bucket_name(arn),
                bucket,
                "case {arn:?}: expected the bucket {bucket:?}"
            );
        }
    }
}
