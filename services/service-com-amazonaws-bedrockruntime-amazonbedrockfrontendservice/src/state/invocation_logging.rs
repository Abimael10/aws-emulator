// SPDX-License-Identifier: CC0-1.0

//! MODEL INVOCATION LOGGING: the record a SERVED inference delivers to the
//! destination the account configured for it.
//!
//! # The rule this module implements
//!
//! Model invocation logging is configured in the Bedrock CONTROL plane
//! (`PutModelInvocationLoggingConfiguration`), and the whole point of it is that
//! the records show up where the configuration pointed them: while a
//! configuration is in force, EVERY invocation this data plane serves writes one
//! record to EVERY destination that configuration names, and the
//! record carries what the call did — which model was invoked, by which
//! operation, the bodies the call carried, and the token counts the API reported
//! inline. Removing the configuration stops the records: an invocation served
//! after the delete writes nothing.
//!
//! A configuration names its destinations independently — `s3Config`, the object
//! destination, and `cloudWatchConfig`, the log group — so a record goes to each
//! one the configuration carries, and the presence of one destination neither
//! supplies nor suppresses the other: a configuration naming only the log group is
//! as much a destination as one naming only the bucket, and one naming both gets
//! the same record in each form.
//!
//! So "logging is configured" and "logging works" are two different observable
//! states, which is what an audit pipeline, a prompt/completion review and a
//! token-accounting consumer read.
//!
//! # Where it lives
//!
//! The destination sits in THIS service's store, keyed on this service's state
//! type like every other part of it, and the control plane installs it here
//! ([`install`]) on the call that stores the configuration — the same direction
//! [`super::enforced_guardrail`] is installed in, and the same direction
//! [`super::invocation_metrics`] publishes an invocation's metric data into the
//! CloudWatch emulator's store. The engine keys state by type, so what the
//! inference path has to read has exactly one home and the two planes cannot
//! drift into disagreeing about where records go.
//!
//! # What is written
//!
//! One RECORD per served invocation ([`record`]), in the form each destination is
//! read in.
//!
//! Into an object destination: one object, the form a reader of a bucket of vended
//! logs fetches — gzipped newline-delimited JSON under the `AWSLogs` layout
//! ([`record_key`]), below whatever `keyPrefix` the configuration named. That is
//! what the object IS rather than a transport encoding — a reader lists the
//! prefix, fetches an object and decompresses it.
//!
//! Into a log group destination: one log event whose `message` is that same record
//! as a JSON document, put in the stream [`LOG_STREAM_NAME`] of the configured
//! group through the one vended-logs write this tree delivers log events with
//! ([`::service_common::cloudwatch_logs::delivered_into_held_group`]) — so the
//! record is in the very store `FilterLogEvents` and `GetLogEvents` read, and a
//! metric or subscription filter over that group sees it. The group is the
//! ACCOUNT's, created by whoever owns it and resolved by the control plane before
//! the configuration was admitted, so the delivery writes into the group
//! CloudWatch Logs already holds and creates none: a group deleted after the
//! configuration was installed loses the records rather than being brought back
//! underneath its owner.
//!
//! It is the same vended-logs FORM that `::service_common::trail_log_files`
//! delivers a trail's events in, and a different layout: an object here holds one
//! INVOCATION rather than a batch under a `Records` envelope, it is partitioned by
//! the hour rather than by the day, and the lines are newline-delimited documents
//! rather than one JSON array. The two therefore state their own layouts, and
//! neither reads the other's.
//!
//! Every value in a record comes off the call that was served. A record names the
//! call by the id the CALLER was answered under and names the principal that made
//! it, so the two facts a consumer correlates a record by — the response a client
//! holds and the identity that invoked the model — are the call's own
//! ([`with_invocation_request_id_header`], [`record`]). Of the
//! per-datatype delivery switches, the one this emulator's records are gated by is
//! `textDataDeliveryEnabled`: the bodies it delivers carry a conversation and a
//! completion, which is text data, so a configuration that turned text data
//! delivery off still gets a record for the invocation — it is still evidence that
//! the call happened, with its model, its operation and its token counts — and the
//! prompt and completion bodies are withheld. The image and embedding switches are
//! carried on the installed destination, because the configuration carries them,
//! but nothing is gated on them: no image or embedding payload is generated here,
//! so there is none for them to withhold.
//!
//! # Best effort
//!
//! Delivery happens after the inference has been served, so nothing about the
//! answer the caller gets back depends on it: an emulator with no object store or
//! no CloudWatch Logs mounted, a bucket or log group the account has since deleted
//! and a write the owning service refuses lose the record and are traced, exactly
//! as every other vended-logs delivery in this tree is. The object store's guard is
//! taken, and the log event written, with NO guard of this service held.
//!
//! One record is bounded by the one call it is of — the bodies it carries are the
//! request this service already bounded ([`super::context_window`]) and the answer
//! it served, so a record cannot be larger than the call that produced it — and the
//! owning store's own ceilings refuse a write past what it holds, which is what
//! bounds a long-running emulator's accumulated records.

use ::std::cell::RefCell;
use ::std::io::Write;

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::service_com_amazonaws_s3_amazons3 as s3;

use crate::ops::types::{ContentBlock, Message, TokenUsage};

/// `schemaType` — what kind of record a delivered line is, stated by the record
/// itself so a reader of a bucket can tell one vended-log form from another.
pub const SCHEMA_TYPE: &str = "ModelInvocationLog";

/// `schemaVersion` — the version of that form this emulator writes.
pub const SCHEMA_VERSION: &str = "1.0";

/// The first segment of every delivered record's key, under whatever prefix the
/// configuration carries: the prefix an audit pipeline reads a bucket of vended
/// logs under.
pub const LOG_FILE_ROOT_SEGMENT: &str = "AWSLogs";

/// The segment naming WHOSE logs a prefix holds.
pub const LOG_FILE_SERVICE_SEGMENT: &str = "BedrockModelInvocationLogs";

/// What a delivered object's name ends with: the records, gzipped.
pub const LOG_FILE_SUFFIX: &str = ".json.gz";

/// The media type of the object a delivered log file is: a gzip stream.
const GZIP_MEDIA_TYPE: &str = "application/x-gzip";

/// The input member that travels in the request URI rather than in the body, so
/// it is not part of the body a record carries — the model is named by the
/// record's own `modelId` instead.
const MODEL_ID_MEMBER: &str = "modelId";

/// The log stream of the configured log group that model invocation log events
/// are put in: Amazon Bedrock writes the records of a CloudWatch Logs destination
/// into the one `aws/bedrock/modelinvocations` stream of the group the
/// configuration named, so every invocation's event is read back from one place.
pub const LOG_STREAM_NAME: &str = "aws/bedrock/modelinvocations";

/// `identity` — the block a record names the principal that made the invocation
/// in, carrying that principal's `arn`.
const IDENTITY_MEMBER: &str = "identity";

/// `operation` — the API operation a record was written for. One constant per
/// inference surface, so the four surfaces cannot spell the same operation two
/// ways.
pub const OPERATION_CONVERSE: &str = "Converse";
pub const OPERATION_CONVERSE_STREAM: &str = "ConverseStream";
pub const OPERATION_INVOKE_MODEL: &str = "InvokeModel";
pub const OPERATION_INVOKE_MODEL_WITH_RESPONSE_STREAM: &str = "InvokeModelWithResponseStream";

/// Where this account's invocation records are delivered, as the control plane's
/// admitted `PutModelInvocationLoggingConfiguration` installed it.
///
/// This is the DESTINATION rather than the stored configuration: the control
/// plane keeps the configuration its own `Get` echoes back, and what the
/// inference path needs is where each record goes and which data the
/// configuration allows to travel.
///
/// A configuration can name either destination or both, and each one it names is
/// delivered to independently (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct InvocationLogDelivery {
    /// `s3Config` — the bucket the records are written into as objects, or `None`
    /// for a configuration that named no object destination.
    pub object_destination: Option<ObjectDestination>,
    /// `cloudWatchConfig` — the log group the records are put in as log events,
    /// or `None` for a configuration that named no log group destination.
    pub log_group_destination: Option<LogGroupDestination>,
    /// `textDataDeliveryEnabled` — whether the prompt and completion bodies
    /// travel in the record.
    pub text_data_delivery_enabled: bool,
    /// `imageDataDeliveryEnabled`, as the configuration carried it. Nothing is
    /// gated on it: no image payload is generated here, so a record carries none
    /// for it to withhold (see the module docs).
    pub image_data_delivery_enabled: bool,
    /// `embeddingDataDeliveryEnabled`, as the configuration carried it. Nothing
    /// is gated on it, for the reason above.
    pub embedding_data_delivery_enabled: bool,
}

/// The OBJECT destination a configuration names (`S3Config`): where a record is
/// written as a vended-log object.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ObjectDestination {
    /// `s3Config.bucketName` — the bucket the records are written into.
    pub bucket: String,
    /// `s3Config.keyPrefix` — what the whole layout is written under, or `None`
    /// for a configuration that named none (the records then land at the
    /// bucket's root).
    pub key_prefix: Option<String>,
}

/// The LOG GROUP destination a configuration names (`CloudWatchConfig`): where a
/// record is put as a log event.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LogGroupDestination {
    /// `cloudWatchConfig.logGroupName` — the group the log events are put in.
    pub log_group: String,
    /// `cloudWatchConfig.roleArn` — the role Amazon Bedrock writes the events
    /// under, as the admitted configuration named it. Whether that role may be
    /// passed, and whether the pair it describes resolves at all, is settled by
    /// the control plane before the destination is installed, so the delivery
    /// carries the value rather than deciding on it a second time: nothing in
    /// this crate reads it, and a reader must not take it for a permission this
    /// plane enforces.
    pub role_arn: String,
}

/// Put `delivery` in force for this account's invocations, or withdraw whatever
/// stood before it with `None` — which the control plane's delete of the
/// configuration does.
///
/// Answers whether this data plane's store was reached: an emulator assembled
/// without the Bedrock Runtime data plane delivers nothing, and the control
/// plane's own record of the configuration is unaffected either way.
///
/// Call it with NO store of the calling service held, so two services' stores are
/// never locked at once.
pub fn install<Eng: Send + Sync + 'static>(
    engine: &Eng,
    delivery: Option<InvocationLogDelivery>,
) -> bool {
    let Some(state) = super::service_state(engine) else {
        return false;
    };
    state.lock().invocation_log_delivery = delivery;
    true
}

thread_local! {
    /// The id the caller of the invocation this thread is serving is answered under,
    /// put here by the delivery that carried it in the record ([`deliver`]) and taken
    /// by the report that states it on the response
    /// ([`with_invocation_request_id_header`]) — so the record of a call and the
    /// response to that call name one id, which is what lets a consumer of the records
    /// find the record of the very call a response it holds is of.
    ///
    /// # The invariant this rests on
    ///
    /// The two sides of it are ONE poll apart, which is what makes a thread-local the
    /// right home: the framework's sequence serializes the handler's result with
    /// nothing awaited after `Operation::handle`
    /// ([`::smithy_server_runtime::run_operation_http`]), and the layer that reports
    /// the id is the one that awaited that sequence — so nothing else can run on this
    /// thread between the id being put here and it being taken. The delivery therefore
    /// puts it here as the LAST thing it does, after both destinations have been
    /// written.
    ///
    /// Nothing is left to be quietly wrong if that ever stops holding: the id is TAKEN
    /// rather than read, so it is reported for at most one response, and an empty slot
    /// means the response is named by a freshly minted id exactly as the response of a
    /// call that delivered no record is.
    static ANSWERED_AS: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Hold `request_id` as the id the caller of this invocation is answered under.
fn answer_as(request_id: &str) {
    ANSWERED_AS.with(|slot| *slot.borrow_mut() = Some(request_id.to_string()));
}

/// The id this thread is holding, taken out of the slot so it is read once.
fn taken_answer() -> Option<String> {
    ANSWERED_AS.with(|slot| slot.borrow_mut().take())
}

/// How this service names the request every one of its responses answers: under the
/// id the invocation's own record was delivered with, where this call delivered one,
/// and under a freshly minted id everywhere else.
///
/// The report (`::service_common::query_error_envelope::Report`) this service hands
/// both its operation wrappers and its definition wrapper, so the response an
/// operation produced and the response no operation produced name their request the
/// same way. It is the one place the two readings of an invocation are held to one
/// value: the delivered record carries the id the caller was answered under because
/// the caller is answered under the id the record carries.
#[must_use]
pub fn with_invocation_request_id_header(resp: RtResponse) -> RtResponse {
    match taken_answer() {
        Some(answered_as) => {
            ::service_common::query_error_envelope::with_stated_request_id_header(
                resp,
                &answered_as,
            )
        }
        None => ::service_common::query_error_envelope::with_request_id_header(resp),
    }
}

/// The ARN of the IAM principal an invocation was made by, as the record's `identity`
/// names them — `None` for a request this emulator can place no principal for.
///
/// Read off the credential the request was signed with, through the one rule every
/// recorder of a served call in this tree reads a caller by
/// (`::service_common::management_events::recorded_caller`): a call made with a
/// temporary credential names the session the account's directory holds for it, a
/// call made with the emulator's long-term credential names the IAM user that
/// credential belongs to, and a request that carried no credential at all names
/// nobody rather than a made-up principal.
fn caller_identity_arn<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &::http::request::Parts,
) -> Option<String> {
    let caller = ::service_common::management_events::recorded_caller(parts);
    let access_key_id = caller.access_key_id.as_deref()?;
    let session = ::service_common::on_behalf_of_calls::session_audit_directory(engine).and_then(
        |directory| {
            directory
                .lock()
                .session(access_key_id)
                .map(|held| held.arn.clone())
        },
    );
    match session {
        Some(arn) => Some(arn),
        None => (caller.principal_type
            == ::service_common::management_events::PRINCIPAL_TYPE_IAM_USER)
            .then(::service_common::caller_principal::ambient_user_arn),
    }
}

/// One served invocation, as a record states it.
///
/// Everything here is a fact of the call that was served: the operation that
/// served it, the model it resolved to, the Region it was signed for, the bodies
/// it carried and the token counts it reported inline.
#[derive(Debug, Clone)]
pub struct LoggedInvocation {
    /// `operation` — one of the four `OPERATION_*` constants above.
    pub operation: &'static str,
    /// `modelId` — the model this emulator resolved the request's identifier to.
    pub model_id: String,
    /// The Region the call was signed for, which is where the records are written
    /// from.
    pub region: String,
    /// The request body, or `None` where it could not be rendered.
    pub input_body: Option<::serde_json::Value>,
    /// The response body the same call answered with, or `None` where it could
    /// not be rendered.
    pub output_body: Option<::serde_json::Value>,
    /// `inputTokenCount` — the tokens the call reported sending to the model.
    pub input_tokens: u32,
    /// `outputTokenCount` — the tokens it reported the model generating.
    pub output_tokens: u32,
    /// Epoch milliseconds of the served call, which dates both the record and the
    /// object's key.
    pub at_millis: i64,
}

/// One shape rendered as the JSON its own schema lays it out as, or `None` where
/// the codec could not render it.
///
/// The record carries the BODIES of the call rather than a second description of
/// them, so they are rendered by the very codec the restJson wire protocol
/// serializes this service's shapes with: a member's wire name, its nesting and
/// its omission when absent are the protocol's, not this module's, and they
/// cannot drift from what the caller actually sent and got back.
#[must_use]
pub fn shape_json(
    schema: &::aws_smithy_schema::Schema,
    value: &dyn ::aws_smithy_schema::serde::SerializableStruct,
) -> Option<::serde_json::Value> {
    use ::aws_smithy_schema::codec::Codec;
    use ::aws_smithy_schema::serde::ShapeSerializer;

    let mut serializer =
        ::aws_smithy_schema_server::json::JsonCodec::rest_json().create_serializer();
    if let Err(error) = serializer.write_struct(schema, value) {
        ::tracing::warn!(
            %error,
            "an invocation's body could not be rendered, so its record carries none"
        );
        return None;
    }
    match ::serde_json::from_str(&serializer.into_string()) {
        Ok(body) => Some(body),
        Err(error) => {
            ::tracing::warn!(
                %error,
                "an invocation's rendered body is not readable JSON, so its record carries none"
            );
            None
        }
    }
}

/// The BODY of a Converse-API request, as the record carries it: the request's own
/// input shape, minus the member that travels in the URI rather than in the body.
#[must_use]
pub fn converse_request_body(
    schema: &::aws_smithy_schema::Schema,
    value: &dyn ::aws_smithy_schema::serde::SerializableStruct,
) -> Option<::serde_json::Value> {
    let mut body = shape_json(schema, value)?;
    if let Some(members) = body.as_object_mut() {
        members.remove(MODEL_ID_MEMBER);
    }
    Some(body)
}

/// The BODY of the answer a Converse-API call serves: the assistant message, the
/// stop reason and the token usage, as the response envelope carries them.
///
/// Both Converse surfaces answer from here — the unary one and the streaming one,
/// whose events are the increments of this very message — so a record cannot
/// report the two differently for the same turn.
#[must_use]
pub fn converse_answer_body(
    content: Vec<ContentBlock>,
    stop_reason: &str,
    usage: &TokenUsage,
) -> Option<::serde_json::Value> {
    let message = Message {
        role: "assistant".to_string(),
        content,
    };
    let message = shape_json(&Message::SCHEMA, &message)?;
    let usage = shape_json(&TokenUsage::SCHEMA, usage)?;
    Some(::serde_json::json!({
        "output": { "message": message },
        "stopReason": stop_reason,
        "usage": usage,
    }))
}

/// The record one served invocation is, as the line a delivered object carries.
///
/// `request_id` names the CALL: it is the id the caller of that invocation was
/// answered under, which is also what tells two objects of the same minute apart —
/// so a client holding a response finds the record of that very invocation rather
/// than guessing which record of an operation is its own (see
/// [`with_invocation_request_id_header`] for how the two are held to one value).
///
/// `identity_arn` is the IAM principal the invocation was made BY, as the record's
/// `identity` block names them; a request whose principal this emulator cannot place
/// carries no such block rather than a made-up one.
///
/// The bodies travel only where `delivery` allows that data to: a configuration
/// that turned text data delivery off still gets the record, with its model, its
/// operation and its token counts, and without the prompt and the completion.
#[must_use]
pub fn record(
    delivery: &InvocationLogDelivery,
    invocation: &LoggedInvocation,
    request_id: &str,
    identity_arn: Option<&str>,
) -> ::serde_json::Value {
    let mut input = ::serde_json::Map::new();
    input.insert(
        "inputContentType".to_string(),
        ::serde_json::json!(super::JSON_MIME),
    );
    input.insert(
        "inputTokenCount".to_string(),
        ::serde_json::json!(invocation.input_tokens),
    );
    let mut output = ::serde_json::Map::new();
    output.insert(
        "outputContentType".to_string(),
        ::serde_json::json!(super::JSON_MIME),
    );
    output.insert(
        "outputTokenCount".to_string(),
        ::serde_json::json!(invocation.output_tokens),
    );
    if delivery.text_data_delivery_enabled {
        if let Some(body) = invocation.input_body.clone() {
            input.insert("inputBodyJson".to_string(), body);
        }
        if let Some(body) = invocation.output_body.clone() {
            output.insert("outputBodyJson".to_string(), body);
        }
    }
    let mut written = ::serde_json::json!({
        "schemaType": SCHEMA_TYPE,
        "schemaVersion": SCHEMA_VERSION,
        "timestamp": super::datetime_of_millis(invocation.at_millis)
            .fmt(::aws_smithy_types::date_time::Format::DateTime)
            .unwrap_or_default(),
        "accountId": ::engine::emulator_account_id(),
        "region": invocation.region,
        "requestId": request_id,
        "operation": invocation.operation,
        "modelId": invocation.model_id,
        "input": ::serde_json::Value::Object(input),
        "output": ::serde_json::Value::Object(output),
    });
    if let (Some(members), Some(arn)) = (written.as_object_mut(), identity_arn) {
        members.insert(
            IDENTITY_MEMBER.to_string(),
            ::serde_json::json!({ "arn": arn }),
        );
    }
    written
}

/// The prefix every record of one account's invocations lands under:
/// `<key-prefix>/AWSLogs/<account-id>/BedrockModelInvocationLogs/<region>/`, with
/// a trailing separator.
///
/// `key_prefix` is what the configuration was installed with
/// (`S3Config$keyPrefix`), which goes ahead of the whole layout; a configuration
/// that carries none delivers at the bucket's root. The prefix's own separators
/// are not doubled: a value written with a leading or trailing `/` names the same
/// prefix as one without.
///
/// The one place this layout is written, for both its readers: the delivery that
/// WRITES the keys ([`record_key`]) and the suites that assert what a destination
/// bucket holds.
#[must_use]
pub fn record_key_prefix(key_prefix: Option<&str>, account_id: &str, region: &str) -> String {
    let prefix = key_prefix
        .map(|prefix| prefix.trim_matches('/'))
        .filter(|prefix| !prefix.is_empty())
        .map(|prefix| format!("{prefix}/"))
        .unwrap_or_default();
    format!("{prefix}{LOG_FILE_ROOT_SEGMENT}/{account_id}/{LOG_FILE_SERVICE_SEGMENT}/{region}/")
}

/// The key one delivered object takes: [`record_key_prefix`], then the HOUR the
/// record belongs to as a path, then the object's own name —
/// `<account-id>_BedrockModelInvocationLogs_<region>_<YYYYMMDDTHHmmZ>_<unique>.json.gz`.
///
/// `at_millis` is the instant the invocation was served at, read as UTC calendar
/// fields with the maintained calendar crate rather than by slicing a rendered
/// stamp; `unique` is what tells two objects of the same minute apart, for which
/// the delivery passes the id of the invocation the record is of.
#[must_use]
pub fn record_key(
    key_prefix: Option<&str>,
    account_id: &str,
    region: &str,
    at_millis: i64,
    unique: &str,
) -> String {
    let at = ::chrono::DateTime::<::chrono::Utc>::from_timestamp_millis(at_millis)
        .unwrap_or(::chrono::DateTime::<::chrono::Utc>::UNIX_EPOCH)
        .naive_utc();
    let hour_path = at.format("%Y/%m/%d/%H");
    let minute = at.format("%Y%m%dT%H%MZ");
    let prefix = record_key_prefix(key_prefix, account_id, region);
    format!(
        "{prefix}{hour_path}/\
         {account_id}_{LOG_FILE_SERVICE_SEGMENT}_{region}_{minute}_{unique}{LOG_FILE_SUFFIX}"
    )
}

/// The object's bytes: the records as newline-delimited JSON, gzipped — or `None`
/// when the framing itself failed.
///
/// Writing into a `Vec` cannot fail for want of space, so a failure here would be
/// the encoder's own; it answers `None` rather than a truncated stream, because no
/// object at all is more honest than one a reader cannot decompress.
fn gzipped(records: &[::serde_json::Value]) -> Option<Vec<u8>> {
    let mut encoder =
        ::flate2::write::GzEncoder::new(Vec::new(), ::flate2::Compression::default());
    for record in records {
        encoder.write_all(record.to_string().as_bytes()).ok()?;
        encoder.write_all(b"\n").ok()?;
    }
    encoder.finish().ok()
}

/// The records one DELIVERED object carries: its bytes read back as what [`gzipped`]
/// wrote them, one JSON document per line — `None` for bytes that are not such an
/// object.
///
/// The reading counterpart of the framing above, stated here beside it for the reason
/// [`record_key_prefix`] is: a reader of a destination bucket — an audit consumer, and
/// the suites that assert what one holds — fetches an object and decompresses it, so
/// the form it decodes cannot drift from the form the delivery wrote.
#[must_use]
pub fn delivered_records(object: &[u8]) -> Option<Vec<::serde_json::Value>> {
    use ::std::io::Read;

    let mut decoded = String::new();
    ::flate2::read::GzDecoder::new(object)
        .read_to_string(&mut decoded)
        .ok()?;
    decoded
        .lines()
        .map(|line| ::serde_json::from_str(line).ok())
        .collect()
}

/// Deliver `invocation`'s record to EVERY destination this account configured, if
/// it configured any.
///
/// Nothing at all happens while no configuration is in force, which is what makes
/// an account that enabled logging distinguishable from one that did not. ONE
/// record is composed for the call and then delivered in each destination's own
/// form — an object for the bucket, a log event for the log group — so the two
/// readings of one invocation cannot differ.
///
/// The record is composed, framed and written with NO guard of this service's
/// store held, and a write a destination's own service refuses is traced rather
/// than failing the call that was already served (see the module docs).
///
/// The record names the call by the id the CALLER is answered under: the id is
/// minted here, carried in the record, and then held for the report that states it
/// on the response this call is answered with
/// ([`with_invocation_request_id_header`]) — which is why holding it is the last
/// thing this does, after every destination has been written.
pub async fn deliver<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    invocation: &LoggedInvocation,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let delivery = state.lock().invocation_log_delivery.clone();
    let Some(delivery) = delivery else {
        return;
    };
    // The id this call is answered under, which the record names the call by and
    // which also tells two objects of the same minute apart. Drawn fresh from the
    // maintained generator the rest of this tree mints request ids with, never
    // derived from the request.
    let request_id = ::service_common::query_error_envelope::new_request_id();
    let record = record(
        &delivery,
        invocation,
        &request_id,
        caller_identity_arn(engine, ctx.parts()).as_deref(),
    );
    if let Some(destination) = delivery.object_destination.as_ref() {
        deliver_object(engine, destination, invocation, &record, &request_id);
    }
    if let Some(destination) = delivery.log_group_destination.as_ref() {
        deliver_log_event(ctx, destination, invocation, &record).await;
    }
    // Last, with nothing awaited after it: the report that answers this call is one
    // poll away on this very thread (see [`ANSWERED_AS`]).
    answer_as(&request_id);
}

/// Write one invocation's `record` into the object destination, as the vended-log
/// object that destination's reader fetches ([`record_key`]).
fn deliver_object<Eng: Send + Sync + 'static>(
    engine: &Eng,
    destination: &ObjectDestination,
    invocation: &LoggedInvocation,
    record: &::serde_json::Value,
    request_id: &str,
) {
    let key = record_key(
        destination.key_prefix.as_deref(),
        ::engine::emulator_account_id(),
        &invocation.region,
        invocation.at_millis,
        request_id,
    );
    let Some(body) = gzipped(::std::slice::from_ref(record)) else {
        ::tracing::warn!(
            bucket = ?destination.bucket,
            key = ?key,
            "an invocation's log record could not be framed, so no object was delivered"
        );
        return;
    };
    let Some(store) = s3::state::service_state(engine) else {
        ::tracing::warn!(
            bucket = ?destination.bucket,
            "no Amazon S3 is mounted beside this service, so an invocation's log record could \
             not be delivered"
        );
        return;
    };
    let object = s3::state::Object::stored(body, Some(GZIP_MEDIA_TYPE.to_string()));
    let written = {
        let mut objects = store.lock();
        objects.put_object(&destination.bucket, &key, object)
    };
    match written {
        Some(s3::state::ObjectWrite::Created | s3::state::ObjectWrite::Replaced) => {}
        refused => ::tracing::warn!(
            bucket = ?destination.bucket,
            key = ?key,
            outcome = ?refused,
            "an invocation's log record could not be written to this emulator's Amazon S3"
        ),
    }
}

/// Put one invocation's `record` in the log group destination, as the one log event
/// a reader of that group filters for.
///
/// The event's `message` is the record as a JSON document, written in the stream
/// [`LOG_STREAM_NAME`] of the group the configuration named, through the one
/// vended-logs write this tree puts log events with — into the group CloudWatch
/// Logs already holds, creating none. Call it with no state guard held: it awaits
/// the write.
async fn deliver_log_event(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    destination: &LogGroupDestination,
    invocation: &LoggedInvocation,
    record: &::serde_json::Value,
) {
    let message = record.to_string();
    let written = ::service_common::cloudwatch_logs::delivered_into_held_group(
        ctx,
        &invocation.region,
        &destination.log_group,
        LOG_STREAM_NAME,
        ::std::slice::from_ref(&message),
    )
    .await;
    if !written {
        ::tracing::warn!(
            log_group = ?destination.log_group,
            "the log group this account's invocation logging names did not take an invocation's \
             log event"
        );
    }
}

#[cfg(test)]
mod tests {
    use ::std::io::Read;

    use super::*;

    /// An instant with a known UTC calendar rendering: 2026-11-02T10:23:43Z.
    fn at_millis() -> i64 {
        ::chrono::DateTime::parse_from_rfc3339("2026-11-02T10:23:43Z")
            .expect("the fixture instant is a well-formed timestamp")
            .timestamp_millis()
    }

    /// A destination with text data delivery on, under `key_prefix`.
    fn delivery(key_prefix: Option<&str>) -> InvocationLogDelivery {
        InvocationLogDelivery {
            object_destination: Some(ObjectDestination {
                bucket: "audit".to_string(),
                key_prefix: key_prefix.map(str::to_string),
            }),
            log_group_destination: None,
            text_data_delivery_enabled: true,
            image_data_delivery_enabled: true,
            embedding_data_delivery_enabled: true,
        }
    }

    /// One served invocation to write a record for.
    fn invocation() -> LoggedInvocation {
        LoggedInvocation {
            operation: OPERATION_CONVERSE,
            model_id: "amazon.nova-lite-v1:0".to_string(),
            region: "us-west-2".to_string(),
            input_body: Some(::serde_json::json!({"messages": [{"role": "user"}]})),
            output_body: Some(::serde_json::json!({"stopReason": "end_turn"})),
            input_tokens: 13,
            output_tokens: 11,
            at_millis: at_millis(),
        }
    }

    /// RULE: the key is the vended-logs layout — the configuration's own prefix, the
    /// `AWSLogs` root, the account, this service's segment, the Region, the HOUR the
    /// record belongs to, and a name carrying the account, the Region and the minute.
    #[test]
    fn a_delivered_record_key_carries_the_vended_logs_layout() {
        // (case, the configured key prefix, the key)
        let rows = [
            (
                "no key prefix, so the layout starts at the bucket root",
                None,
                "AWSLogs/000000000000/BedrockModelInvocationLogs/us-west-2/2026/11/02/10/\
                 000000000000_BedrockModelInvocationLogs_us-west-2_20261102T1023Z_r1.json.gz",
            ),
            (
                "a one-segment prefix",
                Some("sim2real"),
                "sim2real/AWSLogs/000000000000/BedrockModelInvocationLogs/us-west-2/2026/11/02/\
                 10/000000000000_BedrockModelInvocationLogs_us-west-2_20261102T1023Z_r1.json.gz",
            ),
            (
                "a nested prefix written with separators around it",
                Some("/audit/bedrock/"),
                "audit/bedrock/AWSLogs/000000000000/BedrockModelInvocationLogs/us-west-2/2026/\
                 11/02/10/\
                 000000000000_BedrockModelInvocationLogs_us-west-2_20261102T1023Z_r1.json.gz",
            ),
            (
                "an empty prefix, which names none",
                Some(""),
                "AWSLogs/000000000000/BedrockModelInvocationLogs/us-west-2/2026/11/02/10/\
                 000000000000_BedrockModelInvocationLogs_us-west-2_20261102T1023Z_r1.json.gz",
            ),
        ];
        for (case, key_prefix, expected) in rows {
            let actual = record_key(key_prefix, "000000000000", "us-west-2", at_millis(), "r1");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
            assert!(
                actual.starts_with(&record_key_prefix(
                    key_prefix,
                    "000000000000",
                    "us-west-2"
                )),
                "case {case:?}: expected the key under the record prefix, got {actual:?}"
            );
        }
    }

    /// RULE: the object is a gzip stream of the records, one JSON document per line.
    #[test]
    fn a_delivered_object_is_gzipped_newline_delimited_json() {
        for count in [0_usize, 1, 3] {
            let records: Vec<::serde_json::Value> = (0..count)
                .map(|index| ::serde_json::json!({ "requestId": format!("r{index}") }))
                .collect();
            let body = gzipped(&records)
                .unwrap_or_else(|| panic!("case {count} records: the object must have a body"));
            let mut decoded = String::new();
            ::flate2::read::GzDecoder::new(body.as_slice())
                .read_to_string(&mut decoded)
                .unwrap_or_else(|error| panic!("case {count} records: not a gzip stream: {error}"));
            let lines: Vec<&str> = decoded.lines().collect();
            assert_eq!(
                lines.len(),
                count,
                "case {count} records: expected one line each, got {decoded:?}"
            );
            for (index, line) in lines.iter().enumerate() {
                let document: ::serde_json::Value = ::serde_json::from_str(line)
                    .unwrap_or_else(|error| {
                        panic!("case {count} records: line {index} is not JSON: {error}")
                    });
                assert_eq!(
                    document["requestId"],
                    ::serde_json::json!(format!("r{index}")),
                    "case {count} records: expected line {index} to be that record, got {line:?}"
                );
            }
        }
    }

    /// RULE: a record always states what the call did — its operation, its model and
    /// the token counts the API reported inline — and the prompt and completion bodies
    /// travel only where the configuration allows text data to.
    #[test]
    fn the_text_delivery_switch_decides_only_whether_the_bodies_travel() {
        // (case, text data delivery, the bodies travel)
        let rows = [
            ("text data delivery enabled", true, true),
            ("text data delivery disabled", false, false),
        ];
        for (case, enabled, bodies) in rows {
            let destination = InvocationLogDelivery {
                text_data_delivery_enabled: enabled,
                ..delivery(Some("sim2real"))
            };
            let invocation = invocation();
            let written = record(
                &destination,
                &invocation,
                "r1",
                Some("arn:aws:iam::000000000000:user/emulator"),
            );
            assert_eq!(
                (
                    written["operation"].as_str(),
                    written["modelId"].as_str(),
                    written["requestId"].as_str(),
                ),
                (
                    Some(OPERATION_CONVERSE),
                    Some("amazon.nova-lite-v1:0"),
                    Some("r1")
                ),
                "case {case:?}: the record must state the call it is of, got {written}"
            );
            assert_eq!(
                (
                    written["input"]["inputTokenCount"].as_u64(),
                    written["output"]["outputTokenCount"].as_u64(),
                ),
                (Some(13), Some(11)),
                "case {case:?}: the record must carry the counts reported inline, got {written}"
            );
            assert_eq!(
                (
                    !written["input"]["inputBodyJson"].is_null(),
                    !written["output"]["outputBodyJson"].is_null(),
                ),
                (bodies, bodies),
                "case {case:?}: expected the bodies present={bodies}, got {written}"
            );
        }
    }

    /// RULE: a record names the principal that made the invocation, and names none at
    /// all for a call this emulator can place no principal for — never a made-up one.
    #[test]
    fn a_record_names_the_principal_that_made_the_invocation() {
        // (case, the principal the call was signed by, the `identity.arn` stated)
        let rows = [
            (
                "a call made by the account's IAM user",
                Some("arn:aws:iam::000000000000:user/emulator"),
                Some("arn:aws:iam::000000000000:user/emulator"),
            ),
            (
                "a call made with a role session",
                Some("arn:aws:sts::000000000000:assumed-role/auditor/session-1"),
                Some("arn:aws:sts::000000000000:assumed-role/auditor/session-1"),
            ),
            ("a call no principal could be placed for", None, None),
        ];
        for (case, identity_arn, expected) in rows {
            let written = record(&delivery(None), &invocation(), "r1", identity_arn);
            assert_eq!(
                written[IDENTITY_MEMBER]["arn"].as_str(),
                expected,
                "case {case:?}: expected {expected:?} as the record's identity, got {written}"
            );
        }
    }
}
