// SPDX-License-Identifier: CC0-1.0

//! The data transformation a destination's `ProcessingConfiguration` asks for by naming a
//! Lambda function, and what the function's verdict does to each record.
//!
//! "Enables or disables data processing" (`ProcessingConfiguration$Enabled`) with a
//! processor whose `Type` is `Lambda` and whose `LambdaArn` parameter names the function
//! ([`crate::state::LAMBDA_ARN_PARAMETER`]): the records of a buffer are handed to that
//! function BEFORE anything is written, and what it answers for each record is what the
//! destination receives. Nothing about a record is taken on through unchanged once such a
//! processor is in force — that is the whole point of the processor, and a destination that
//! delivered the producer's own bytes instead would make every transformation silently a
//! pass-through.
//!
//! ## One invocation
//!
//! The function is handed the invocation's own id, the stream the records came from (and the
//! Kinesis data stream feeding it, for a stream that has one), the Region the delivery is
//! made in, and one entry per record carrying that record's `recordId`, the instant it
//! arrived and its data base64-encoded. It answers one entry per record, naming that record's
//! id, the `result` it reached for it and — for a record it transformed — the `data` the
//! destination is to receive:
//!
//! * `Ok` — the record is delivered, carrying the data the function returned rather than the
//!   data it was handed.
//! * `Dropped` — the record is delivered NOWHERE, and no error output is written for it: the
//!   function's verdict is that it should not be delivered at all.
//! * `ProcessingFailed` — the record is not delivered either; it goes to the destination's
//!   error output instead ([`super::error_output_prefix`]), under the code that says the
//!   function marked it so, carrying the record's own SOURCE bytes.
//!
//! A record the answer places in none of those ways is a record the transformation did not
//! place: an id the answer never mentions, an id it mentions twice, a `result` that is not
//! one of the three, and an `Ok` carrying no readable data each send their record to the
//! error output under the class that states which of those it was, rather than being
//! delivered as the bytes the function was asked to replace.
//!
//! ## How the invocation is made, and what a failed one costs
//!
//! The call is the AWS SDK's own `Invoke`, synchronous ("RequestResponse"), driven over the
//! IN-PROCESS transport of the emulator this delivery runs within ([`super::DeliveryHop`]),
//! exactly as every other hop a delivery makes: the functions this emulator can invoke are
//! the ones it serves itself, so a transformation opens no socket and reaches nothing outside
//! this process.
//!
//! * An invocation that could not be ANSWERED at all leaves the whole buffer owed again,
//!   exactly like a destination that did not answer, so nothing a put accepted is lost and
//!   the records keep their documented retention.
//! * One the function answered with an error result, and one whose answer is not the response
//!   document at all, is attempted again for as many further attempts as the processor's
//!   `NumberOfRetries` parameter allows ("3 for `NumberOfRetries`" —
//!   `ProcessorParameter$ParameterName`) and its records are then written to the error
//!   output, so a function that never answers properly cannot wedge a stream's buffer for
//!   ever.
//!
//! One request carries the records of one object's worth of the buffer, which is the whole
//! buffer for every destination but one with dynamic partitioning in force, where the records
//! of each partition are transformed by a request of their own.

use super::*;

/// The ARN namespace of AWS Lambda, which is what the Region a `LambdaArn` names is read out
/// of.
const LAMBDA_ARN_VENDOR: &str = "lambda";

/// The member of one request entry naming the record it carries, which the answer names that
/// record's verdict under.
const RECORD_ID_MEMBER: &str = "recordId";

/// The member of one request entry carrying the record's data, base64-encoded — and the
/// member of an answer entry carrying what the function made of it.
const DATA_MEMBER: &str = "data";

/// The member of one request entry stating when the record was ingested, as epoch
/// milliseconds.
const ARRIVAL_TIMESTAMP_MEMBER: &str = "approximateArrivalTimestamp";

/// The member of the request and of the answer carrying the records themselves.
const RECORDS_MEMBER: &str = "records";

/// The member of the request naming the invocation, which is what a function that writes its
/// own log of a transformation identifies the batch by.
const INVOCATION_ID_MEMBER: &str = "invocationId";

/// The member of the request naming the Firehose stream whose buffer is being transformed.
const DELIVERY_STREAM_ARN_MEMBER: &str = "deliveryStreamArn";

/// The member of the request naming the Kinesis data stream feeding that Firehose stream,
/// carried only by a stream that HAS one — a direct-put stream is fed by no other stream, so
/// its request names none.
const SOURCE_KINESIS_STREAM_ARN_MEMBER: &str = "sourceKinesisStreamArn";

/// The member of the request naming the Region the delivery is made in.
const REGION_MEMBER: &str = "region";

/// The member of one answer entry carrying the function's verdict for that record.
const RESULT_MEMBER: &str = "result";

/// The verdict of a record the function transformed: deliver it, carrying the data the answer
/// returned.
const OK_RESULT: &str = "Ok";

/// The verdict of a record the function asks to be delivered nowhere.
const DROPPED_RESULT: &str = "Dropped";

/// The verdict of a record the function could not process, which goes to the destination's
/// error output.
const PROCESSING_FAILED_RESULT: &str = "ProcessingFailed";

/// The prefix of every `errorCode` the TRANSFORMATION decides, which is what makes the
/// error-output document of such a record name the function that decided it.
const LAMBDA_ERROR_CODE_PREFIX: &str = "Lambda.";

/// What the `firehose:error-output-type` namespace expands to for a record the destination's
/// data processing did not place.
///
/// Read off [`PROCESSING_FAILED`], the class an enabled processor of any other kind writes its
/// failed records under: the records a destination's data processing could not place go to ONE
/// error output whichever processor of it judged them, so the two cannot drift apart.
const PROCESSING_FAILED_OUTPUT_TYPE: &str = PROCESSING_FAILED.output_type;

/// The failure of a record the function itself marked `ProcessingFailed`.
const LAMBDA_PROCESSING_FAILED: DeliveryFailure = DeliveryFailure {
    output_type: PROCESSING_FAILED_OUTPUT_TYPE,
    error_code: "Lambda.ProcessingFailedStatus",
    error_message: "ProcessingFailed status set for record",
};

/// The failure of a record the function's answer never mentioned, so nothing was returned for
/// it to deliver.
const LAMBDA_MISSING_RECORD_ID: DeliveryFailure = DeliveryFailure {
    output_type: PROCESSING_FAILED_OUTPUT_TYPE,
    error_code: "Lambda.MissingRecordId",
    error_message: "One or more record Ids were not returned. Please check your function and \
                    make sure the processed records contain valid record Ids.",
};

/// The failure of a record the function's answer carried more than one entry for, so which of
/// them is its verdict is not stated.
const LAMBDA_DUPLICATED_RECORD_ID: DeliveryFailure = DeliveryFailure {
    output_type: PROCESSING_FAILED_OUTPUT_TYPE,
    error_code: "Lambda.DuplicatedRecordId",
    error_message: "Multiple records with the same record Id were returned. Please check your \
                    function and make sure the processed records have unique record Ids.",
};

/// The failure of a record whose entry — or whose whole answer — is not the response document
/// the transformation reads: a `result` that is none of the three, an `Ok` carrying no readable
/// data, or an answer that is not a document of records at all.
const LAMBDA_JSON_PROCESSING: DeliveryFailure = DeliveryFailure {
    output_type: PROCESSING_FAILED_OUTPUT_TYPE,
    error_code: "Lambda.JsonProcessingException",
    error_message: "There was an error parsing returned records from the Lambda function \
                    response. Please make sure the processed records contain valid result \
                    status of Dropped, Ok, or ProcessingFailed.",
};

/// The failure of a buffer whose invocation RAN and ended in an error: the function was
/// reached and answered that its own run failed.
const LAMBDA_FUNCTION_ERROR: DeliveryFailure = DeliveryFailure {
    output_type: PROCESSING_FAILED_OUTPUT_TYPE,
    error_code: "Lambda.FunctionError",
    error_message: "The Lambda function was successfully invoked but it returned an error \
                    result.",
};

/// Whether `failure` is one the TRANSFORMATION decided, whose error-output document therefore
/// names the function that decided it beside the failure itself.
pub(super) fn reports_the_function(failure: DeliveryFailure) -> bool {
    failure.error_code.starts_with(LAMBDA_ERROR_CODE_PREFIX)
}

/// What a destination's enabled `Lambda` processor transforms this location's records with.
#[derive(Debug, Clone)]
pub struct LambdaTransform {
    /// The `LambdaArn` parameter's value, verbatim: the function the records are handed to.
    function_arn: String,
    /// The `NumberOfRetries` parameter's value: how many FURTHER attempts an invocation that
    /// answered an error is given before its records are written to the error output.
    retries: u32,
}

impl LambdaTransform {
    /// The transformation `processing` asks for, and `None` where it asks for none: data
    /// processing that is not enabled, a configuration with no processor of the `Lambda` type,
    /// and one whose processor names no function.
    pub(super) fn of(processing: Option<&ProcessingConfiguration>) -> Option<Self> {
        let processing = processing?;
        if processing.enabled != Some(true) {
            return None;
        }
        let processor = processing
            .processors
            .iter()
            .flatten()
            .find(|processor| processor.r#type == crate::state::LAMBDA_PROCESSOR_TYPE)?;
        let parameter = |name: &str| {
            processor
                .parameters
                .iter()
                .flatten()
                .find(|parameter| parameter.parameter_name == name)
                .map(|parameter| parameter.parameter_value.as_str())
        };
        let function_arn = parameter(crate::state::LAMBDA_ARN_PARAMETER)?;
        if function_arn.is_empty() {
            return None;
        }
        Some(Self {
            function_arn: function_arn.to_string(),
            retries: parameter(crate::state::NUMBER_OF_RETRIES_PARAMETER)
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(default_retries),
        })
    }

    /// The function this transformation invokes, which the error-output document of a record
    /// it did not place names.
    pub(super) fn function_arn(&self) -> &str {
        &self.function_arn
    }
}

/// `NumberOfRetries` for a processor carrying none — which only a configuration written before
/// this service normalises its processors can be, since that normalisation fills the parameter
/// in. The SAME documented default, read off it so the two cannot drift.
fn default_retries() -> u32 {
    crate::state::DEFAULT_NUMBER_OF_RETRIES
        .parse()
        .unwrap_or_default()
}

/// What the transformation left of one object's worth of a buffer.
pub(super) struct Transformed<'r> {
    /// The records to deliver, in arrival order, each beside the data the function returned
    /// for it.
    pub records: Vec<(&'r StoredRecord, Vec<u8>)>,
    /// The records the transformation did not place, each beside the failure that says why.
    /// The record is the SOURCE record, so its error-output document carries the bytes the
    /// producer sent.
    pub failed: Vec<(&'r StoredRecord, DeliveryFailure)>,
}

/// Hand `processed` to the function `transform` names and answer what its verdicts left.
///
/// `None` when the whole buffer is owed again rather than written: there was no emulator to
/// invoke through, the function could not be reached at all, or an invocation that answered an
/// error still has attempts left ([`LambdaTransform::retries`]). Nothing is written and
/// nothing is dropped in that case — the caller puts the records back where they came from.
pub(super) async fn transform<'r>(
    hop: &DeliveryHop,
    delivery: &PendingDelivery,
    transform: &LambdaTransform,
    processed: Vec<(&'r StoredRecord, Vec<u8>)>,
) -> Option<Transformed<'r>> {
    if processed.is_empty() {
        return Some(Transformed {
            records: Vec::new(),
            failed: Vec::new(),
        });
    }
    let Some(emulator) = hop.emulator() else {
        ::tracing::warn!(
            stream = %delivery.stream,
            "there is no emulator to invoke the destination's data transformation through, \
             so this delivery was left for a later one"
        );
        return None;
    };
    // The Region the function lives in, which its own ARN names, and the Region of the stream
    // whose buffer this is for a parameter that names no Region at all.
    let region = ::service_common::arn::region_of(&transform.function_arn, LAMBDA_ARN_VENDOR)
        .or_else(|| delivery_region(&delivery.source_arn))
        .unwrap_or_default();
    if region.is_empty() {
        ::tracing::warn!(
            stream = %delivery.stream,
            "the destination's data transformation names no Region to invoke its function \
             in, so this delivery was left for a later one"
        );
        return None;
    }
    let body = ::serde_json::to_vec(&request_payload(delivery, region, &processed)).ok()?;
    let answer = lambda_client(emulator, region)
        .invoke()
        .function_name(&transform.function_arn)
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse)
        .payload(::aws_sdk_lambda::primitives::Blob::new(body))
        .send()
        .await;
    let output = match answer {
        Ok(output) => output,
        // The function was not reached, so nothing was judged: the buffer is owed again
        // exactly as it is for a destination that did not answer.
        Err(error) => {
            ::tracing::warn!(
                stream = %delivery.stream,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the destination's data transformation could not be invoked, so this \
                 delivery was left for a later one"
            );
            return None;
        }
    };
    // "FunctionError: if present, indicates that an error occurred during function
    // execution": the function ran and its run failed, so no record of the buffer was
    // transformed.
    if output.function_error().is_some() {
        return spent_or_owed(processed, LAMBDA_FUNCTION_ERROR, transform);
    }
    let payload = output
        .payload()
        .map(|payload| payload.as_ref().to_vec())
        .unwrap_or_default();
    let answered = ::serde_json::from_slice::<::serde_json::Value>(&payload).ok();
    let Some(entries) = answered
        .as_ref()
        .and_then(|answered| answered.get(RECORDS_MEMBER))
        .and_then(::serde_json::Value::as_array)
    else {
        return spent_or_owed(processed, LAMBDA_JSON_PROCESSING, transform);
    };
    Some(verdicts(processed, entries))
}

/// Every record of the buffer under `failure` once the attempts the processor allows have been
/// spent, and `None` — the buffer owed again — while they have not.
///
/// The attempts a record has already been left by are the ones every other delivery failure
/// counts ([`StoredRecord::delivery_attempts`]), so an invocation that keeps answering an error
/// costs the records the retries the processor asked for and no more.
fn spent_or_owed<'r>(
    processed: Vec<(&'r StoredRecord, Vec<u8>)>,
    failure: DeliveryFailure,
    transform: &LambdaTransform,
) -> Option<Transformed<'r>> {
    let attempts = processed
        .iter()
        .map(|(record, _)| record.delivery_attempts)
        .min()
        .unwrap_or_default();
    if attempts < transform.retries {
        return None;
    }
    Some(Transformed {
        records: Vec::new(),
        failed: processed
            .into_iter()
            .map(|(record, _)| (record, failure))
            .collect(),
    })
}

/// Apply the answer's verdicts to the records it was asked about, in arrival order.
fn verdicts<'r>(
    processed: Vec<(&'r StoredRecord, Vec<u8>)>,
    entries: &[::serde_json::Value],
) -> Transformed<'r> {
    // By record id, so a record the answer mentions twice is seen as such rather than taking
    // whichever entry came first.
    let mut answered: BTreeMap<&str, Vec<&::serde_json::Value>> = BTreeMap::new();
    for entry in entries {
        let Some(record_id) = entry
            .get(RECORD_ID_MEMBER)
            .and_then(::serde_json::Value::as_str)
        else {
            continue;
        };
        answered.entry(record_id).or_default().push(entry);
    }
    let mut records = Vec::with_capacity(processed.len());
    let mut failed = Vec::new();
    for (record, _) in processed {
        match answered
            .get(record.record_id.as_str())
            .map(::std::vec::Vec::as_slice)
        {
            Some([entry]) => match verdict(entry) {
                Verdict::Delivered(data) => records.push((record, data)),
                Verdict::Dropped => {}
                Verdict::Failed(failure) => failed.push((record, failure)),
            },
            Some([_, _, ..]) => failed.push((record, LAMBDA_DUPLICATED_RECORD_ID)),
            None | Some([]) => failed.push((record, LAMBDA_MISSING_RECORD_ID)),
        }
    }
    Transformed { records, failed }
}

/// What one answer entry says becomes of its record.
enum Verdict {
    /// Deliver it, carrying these bytes.
    Delivered(Vec<u8>),
    /// Deliver it nowhere.
    Dropped,
    /// Write it to the error output under this failure.
    Failed(DeliveryFailure),
}

/// The verdict one answer entry states.
fn verdict(entry: &::serde_json::Value) -> Verdict {
    match entry
        .get(RESULT_MEMBER)
        .and_then(::serde_json::Value::as_str)
    {
        Some(OK_RESULT) => match entry
            .get(DATA_MEMBER)
            .and_then(::serde_json::Value::as_str)
            .and_then(|data| ::aws_smithy_types::base64::decode(data).ok())
        {
            Some(data) => Verdict::Delivered(data),
            // A transformed record with no readable data is nothing the destination can be
            // given: there is no record to deliver for that entry.
            None => Verdict::Failed(LAMBDA_JSON_PROCESSING),
        },
        Some(DROPPED_RESULT) => Verdict::Dropped,
        Some(PROCESSING_FAILED_RESULT) => Verdict::Failed(LAMBDA_PROCESSING_FAILED),
        _ => Verdict::Failed(LAMBDA_JSON_PROCESSING),
    }
}

/// The JSON document one invocation carries: the invocation's own id, the stream the records
/// came from and the Region, and one entry per record.
fn request_payload(
    delivery: &PendingDelivery,
    region: &str,
    processed: &[(&StoredRecord, Vec<u8>)],
) -> ::serde_json::Value {
    let records: Vec<::serde_json::Value> = processed
        .iter()
        .map(|(record, data)| {
            ::serde_json::json!({
                RECORD_ID_MEMBER: record.record_id,
                ARRIVAL_TIMESTAMP_MEMBER: epoch_millis(record.received_at),
                DATA_MEMBER: ::aws_smithy_types::base64::encode(data),
            })
        })
        .collect();
    let mut event = ::serde_json::json!({
        INVOCATION_ID_MEMBER: ::uuid::Uuid::new_v4().to_string(),
        DELIVERY_STREAM_ARN_MEMBER: delivery.source_arn,
        REGION_MEMBER: region,
        RECORDS_MEMBER: records,
    });
    if let (Some(members), Some(source)) = (
        event.as_object_mut(),
        delivery.kinesis_source_arn.as_deref(),
    ) {
        members.insert(
            SOURCE_KINESIS_STREAM_ARN_MEMBER.to_string(),
            source.into(),
        );
    }
    event
}

/// The Lambda client a transformation is invoked through: the AWS SDK driven over the
/// emulator's own in-process transport, exactly as this service reaches every other one.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a stalled
/// stream, and each of those would need a sleep implementation this client is not given. The
/// same goes for caching the identity: the credentials the emulator hands over are used
/// directly, and they only sign this in-process call.
fn lambda_client(
    emulator: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(emulator.http_client())
        .credentials_provider(emulator.aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://lambda.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}
