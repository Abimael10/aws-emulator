// SPDX-License-Identifier: CC0-1.0

//! The DATA CAPTURE an endpoint performs on the invocations it serves: what its endpoint
//! configuration asks to be recorded, the record one invocation produces, and the object that
//! record is written as under the configured Amazon S3 destination.
//!
//! # What capture IS
//!
//! `DataCaptureConfig` is "configuration to control how SageMaker AI captures inference data",
//! and its members say exactly what the endpoint does with an invocation it served:
//! `EnableCapture` — "whether data capture should be enabled or disabled (defaults to
//! enabled)"; `InitialSamplingPercentage` — "the percentage of requests SageMaker AI will
//! capture"; `DestinationS3Uri` — "the Amazon S3 location used to capture the data";
//! `CaptureOptions` — "whether to collect only input, only output, or both". So capture is a
//! BY-PRODUCT of serving: the endpoint writes what it was asked and what it answered into the
//! caller's own bucket, as newline-delimited JSON records.
//!
//! # Capture is BEST EFFORT
//!
//! The inference is what the operation answers; the capture object is not. A destination this
//! emulator cannot write to — a location the deployment's execution role is not granted
//! `s3:PutObject` on, a URI naming no bucket, an Amazon S3 that refused the write — leaves the
//! invocation answered exactly as it was, with no object behind it. Nothing about a capture
//! write reaches the caller, and nothing about it changes the inference.
//!
//! The write is made AS the deployment's execution role, like every other Amazon S3 call a
//! SageMaker resource's work makes ([`super::s3_access`]): the decision is IAM's, asked through
//! the shared reading of an object write made as a role
//! ([`super::s3_access::object_write_is_unauthorized`]) — the destination bucket's own policy
//! refuses what it EXPLICITLY DENIES, and otherwise the grant the role itself carries decides —
//! and a write IAM withholds from the role is one the endpoint does not perform.

use super::*;

/// `CaptureMode`'s member for the request the endpoint was sent
/// (`smithy_query(query="shape",shape="CaptureMode")`).
pub const CAPTURE_MODE_INPUT: &str = "Input";

/// `CaptureMode`'s member for the inference the endpoint answered.
pub const CAPTURE_MODE_OUTPUT: &str = "Output";

/// `CaptureMode`'s member for both halves of the exchange.
pub const CAPTURE_MODE_INPUT_AND_OUTPUT: &str = "InputAndOutput";

/// How a capture record names the request half of the exchange.
const CAPTURED_INPUT_MODE: &str = "INPUT";

/// How a capture record names the answer half of the exchange.
const CAPTURED_OUTPUT_MODE: &str = "OUTPUT";

/// The schema version a capture record declares.
pub const CAPTURE_EVENT_VERSION: &str = "0";

/// The suffix a capture object carries: one JSON record per line.
pub const CAPTURE_OBJECT_SUFFIX: &str = "jsonl";

/// How a captured payload of comma-separated values is recorded: as the text it was, which is
/// what a CSV content type carries.
const CAPTURE_ENCODING_CSV: &str = "CSV";

/// How a captured JSON payload is recorded: as the text it was.
const CAPTURE_ENCODING_JSON: &str = "JSON";

/// How a captured payload of any other content type is recorded — "if no headers are specified
/// SageMaker AI will by default base64 encode when capturing the data"
/// (`DataCaptureConfig$CaptureContentTypeHeader`) — so bytes that are not text still round-trip.
const CAPTURE_ENCODING_BASE64: &str = "BASE64";

/// The content type a payload with none declared is recorded as: the media type SageMaker's
/// built-in algorithms take records in, which is what an invocation carrying no `Content-Type`
/// is read as ([`super::model_serving::CSV_MEDIA_TYPE`]).
const CAPTURE_DEFAULT_CONTENT_TYPE: &str = model_serving::CSV_MEDIA_TYPE;

/// The content types treated as comma-separated values when the configuration names none
/// itself, and the ones treated as JSON. A capture record of a CSV or a JSON invocation
/// carries the payload AS TEXT with the encoding naming its form, which is what the capture
/// file of such an invocation holds.
const DEFAULT_CSV_CONTENT_TYPES: [&str; 1] = [model_serving::CSV_MEDIA_TYPE];
const DEFAULT_JSON_CONTENT_TYPES: [&str; 1] = [model_serving::JSON_MEDIA_TYPE];

/// The whole of [`SamplingPercentage`]'s range: `InitialSamplingPercentage` is `range 0..100`
/// (`smithy_query(query="shape",shape="DataCaptureConfig")`), so this is the denominator a
/// sampling decision is taken against.
const SAMPLING_DENOMINATOR: u64 = 100;

/// What an endpoint's deployment asks to be captured, read off the configuration that is
/// serving.
#[derive(Debug, Clone)]
pub struct CaptureSettings {
    /// `DataCaptureConfig$DestinationS3Uri`, as the configuration wrote it.
    pub destination_s3_uri: String,
    /// `DataCaptureConfig$InitialSamplingPercentage`.
    pub sampling_percentage: i32,
    /// Whether `CaptureOptions` asks for the request.
    pub capture_input: bool,
    /// Whether `CaptureOptions` asks for the answer.
    pub capture_output: bool,
    /// `CaptureContentTypeHeader$CsvContentTypes`, when the configuration names any.
    pub csv_content_types: Vec<String>,
    /// `CaptureContentTypeHeader$JsonContentTypes`, when the configuration names any.
    pub json_content_types: Vec<String>,
    /// The role the capture write is made as: the deployed model's own execution role ("the
    /// IAM role that SageMaker can assume to access model artifacts and docker image for
    /// deployment", `CreateModel$ExecutionRoleArn`), and otherwise the configuration's.
    pub role_arn: Option<String>,
}

impl CaptureSettings {
    /// Whether the invocation whose request payload is `payload` is one of the sampled share.
    ///
    /// `InitialSamplingPercentage` is "the percentage of requests SageMaker AI will capture":
    /// the whole share captures every invocation and a zero share none, and in between the
    /// decision is taken from the REQUEST itself — a digest of the payload against the
    /// percentage — so it is stable for one request instead of depending on how many callers
    /// came before it.
    #[must_use]
    pub fn captures(&self, payload: &[u8]) -> bool {
        let percentage = u64::try_from(self.sampling_percentage).unwrap_or_default();
        if percentage >= SAMPLING_DENOMINATOR {
            return true;
        }
        if percentage == 0 {
            return false;
        }
        let mut digest = <::sha2::Sha256 as ::sha2::Digest>::new();
        ::sha2::Digest::update(&mut digest, payload);
        let hashed = ::sha2::Digest::finalize(digest);
        let mut head = [0_u8; 8];
        head.copy_from_slice(&hashed[..8]);
        u64::from_le_bytes(head) % SAMPLING_DENOMINATOR < percentage
    }

    /// How the payload of content type `content_type` is recorded: as the text it was for a
    /// content type the configuration treats as CSV or JSON, and base64 otherwise.
    fn encoding_of(&self, content_type: &str) -> &'static str {
        let named = |configured: &[String], defaults: &[&str]| {
            if configured.is_empty() {
                defaults.iter().any(|known| content_type.contains(known))
            } else {
                configured
                    .iter()
                    .any(|known| content_type.contains(known.as_str()))
            }
        };
        if named(&self.csv_content_types, &DEFAULT_CSV_CONTENT_TYPES) {
            return CAPTURE_ENCODING_CSV;
        }
        if named(&self.json_content_types, &DEFAULT_JSON_CONTENT_TYPES) {
            return CAPTURE_ENCODING_JSON;
        }
        CAPTURE_ENCODING_BASE64
    }
}

/// What an endpoint's deployment asks to be captured, or `None` when it captures nothing: no
/// `DataCaptureConfig` at all, capture turned off, or a configuration asking for neither half
/// of the exchange.
///
/// Read off the ENDPOINT's own snapshot of its configuration — the one the deployment that is
/// serving was created from — so a later `CreateEndpointConfig` of the same name does not
/// change what this endpoint captures.
#[must_use]
pub fn endpoint_capture_settings(
    store: &ComAmazonawsSagemakerSagemakerState,
    endpoint_name: &str,
    variant_name: &str,
) -> Option<CaptureSettings> {
    let endpoint = store.endpoints.get(endpoint_name)?;
    let config = endpoint.data_capture_config.as_ref()?;
    // "Whether data capture should be enabled or disabled (defaults to enabled)"
    // (`DataCaptureConfig$EnableCapture`), so only an explicit `false` turns it off.
    if config.enable_capture == Some(false) {
        return None;
    }
    let asked = |mode: &str| {
        config
            .capture_options
            .iter()
            .any(|option| option.capture_mode == mode || option.capture_mode == CAPTURE_MODE_INPUT_AND_OUTPUT)
    };
    let settings = CaptureSettings {
        destination_s3_uri: config.destination_s3_uri.clone(),
        sampling_percentage: config.initial_sampling_percentage,
        capture_input: asked(CAPTURE_MODE_INPUT),
        capture_output: asked(CAPTURE_MODE_OUTPUT),
        csv_content_types: config
            .capture_content_type_header
            .as_ref()
            .and_then(|header| header.csv_content_types.clone())
            .unwrap_or_default(),
        json_content_types: config
            .capture_content_type_header
            .as_ref()
            .and_then(|header| header.json_content_types.clone())
            .unwrap_or_default(),
        role_arn: capture_role(store, endpoint_name, variant_name),
    };
    (settings.capture_input || settings.capture_output).then_some(settings)
}

/// The role the endpoint's capture write is made as: the execution role of the model deployed
/// behind the variant that served the invocation, and otherwise the endpoint configuration's
/// own (`CreateEndpointConfig$ExecutionRoleArn`).
fn capture_role(
    store: &ComAmazonawsSagemakerSagemakerState,
    endpoint_name: &str,
    variant_name: &str,
) -> Option<String> {
    let endpoint = store.endpoints.get(endpoint_name)?;
    let config = store.endpoint_configs.get(&endpoint.config_name)?;
    let model_role = config
        .production_variants
        .iter()
        .find(|variant| variant.variant_name == variant_name)
        .and_then(|variant| variant.model_name.as_ref())
        .and_then(|model_name| store.models.get(model_name))
        .and_then(|model| model.execution_role_arn.clone());
    model_role.or_else(|| config.execution_role_arn.clone())
}

/// One half of the exchange one capture record holds: the bytes and the content type they were
/// carried under.
#[derive(Debug, Clone, Copy)]
pub struct CapturedPayload<'payload> {
    pub data: &'payload [u8],
    pub content_type: Option<&'payload str>,
}

/// The invocation one capture record is about.
#[derive(Debug, Clone, Copy)]
pub struct CapturedInvocation<'invocation> {
    pub endpoint_name: &'invocation str,
    /// The production variant that served it, which the capture object's key is laid out
    /// under.
    pub variant_name: &'invocation str,
    /// `InvokeEndpoint$InferenceId`: "if you provide a value, it is added to the captured
    /// data".
    pub inference_id: Option<&'invocation str>,
    pub input: CapturedPayload<'invocation>,
    pub output: CapturedPayload<'invocation>,
    /// When the endpoint served it, which the record reports and the object's key is laid out
    /// under.
    pub at: SystemTime,
}

/// Capture one served invocation: write the record it produces under the configured
/// destination, as the deployment's execution role.
///
/// Answers whether an object was written, for the caller's own log. BEST EFFORT in every
/// outcome: nothing here fails the invocation it is about (see the module docs), and a
/// destination that cannot be written leaves no object and no error.
pub async fn capture_invocation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    settings: &CaptureSettings,
    invocation: &CapturedInvocation<'_>,
) -> bool {
    if !settings.captures(invocation.input.data) {
        return false;
    }
    let Some(destination) = S3Location::parse(&settings.destination_s3_uri) else {
        ::tracing::debug!(
            destination = ?settings.destination_s3_uri,
            "the data-capture destination names no Amazon S3 bucket, so nothing was captured"
        );
        return false;
    };
    let event_id = capture_event_id();
    let object = S3Location {
        bucket: destination.bucket.clone(),
        key: capture_object_key(&destination.key, invocation, &event_id),
    };
    let role_arn = settings.role_arn.clone().unwrap_or_default();
    let partition = arn_partition(&role_arn);
    let client = s3_client(ctx, region);
    // The ONE question this emulator asks about an Amazon S3 object a SageMaker resource's
    // work writes AS its execution role ([`super::s3_access::object_write_is_unauthorized`]):
    // the destination bucket's own policy refuses what it EXPLICITLY DENIES, and otherwise the
    // grant the role itself carries decides. Asked here rather than restated, so a capture
    // delivery and a job's output write cannot disagree about what IAM allows the same role.
    if object_write_is_unauthorized(ctx, &client, region, &role_arn, &partition, &object).await {
        ::tracing::debug!(
            endpoint = ?invocation.endpoint_name,
            destination = ?object.uri(),
            "the endpoint's execution role may not write the data-capture destination, so \
             nothing was captured"
        );
        return false;
    }
    let record = capture_record(settings, invocation, &event_id);
    let Ok(mut document) = ::serde_json::to_vec(&record) else {
        return false;
    };
    // One JSON record per line, so a reader takes the objects of the destination apart by
    // lines however many invocations each of them holds.
    document.push(b'\n');
    match write_object(&client, &object, document).await {
        Ok(()) => true,
        Err(detail) => {
            ::tracing::debug!(
                endpoint = ?invocation.endpoint_name,
                destination = ?object.uri(),
                detail = ?detail,
                "the data-capture object could not be written, so nothing was captured"
            );
            false
        }
    }
}

/// The record one captured invocation is: what the endpoint was sent, what it answered, and
/// the event the exchange was.
fn capture_record(
    settings: &CaptureSettings,
    invocation: &CapturedInvocation<'_>,
    event_id: &str,
) -> ::serde_json::Value {
    let mut capture_data = ::serde_json::Map::new();
    if settings.capture_input {
        capture_data.insert(
            "endpointInput".to_string(),
            captured_half(settings, &invocation.input, CAPTURED_INPUT_MODE),
        );
    }
    if settings.capture_output {
        capture_data.insert(
            "endpointOutput".to_string(),
            captured_half(settings, &invocation.output, CAPTURED_OUTPUT_MODE),
        );
    }
    let mut metadata = ::serde_json::Map::new();
    metadata.insert("eventId".to_string(), ::serde_json::json!(event_id));
    if let Some(inference_id) = invocation.inference_id {
        metadata.insert("inferenceId".to_string(), ::serde_json::json!(inference_id));
    }
    metadata.insert(
        "inferenceTime".to_string(),
        ::serde_json::json!(capture_instant(invocation.at)),
    );
    ::serde_json::json!({
        "captureData": ::serde_json::Value::Object(capture_data),
        "eventMetadata": ::serde_json::Value::Object(metadata),
        "eventVersion": CAPTURE_EVENT_VERSION,
    })
}

/// One half of a capture record: the content type the payload was observed under, which half
/// of the exchange it is, the data and how the data is encoded.
fn captured_half(
    settings: &CaptureSettings,
    payload: &CapturedPayload<'_>,
    mode: &str,
) -> ::serde_json::Value {
    let content_type = payload
        .content_type
        .unwrap_or(CAPTURE_DEFAULT_CONTENT_TYPE)
        .to_string();
    let encoding = settings.encoding_of(&content_type);
    let data = if encoding == CAPTURE_ENCODING_BASE64 {
        ::base64::Engine::encode(&::base64::engine::general_purpose::STANDARD, payload.data)
    } else {
        String::from_utf8_lossy(payload.data).to_string()
    };
    ::serde_json::json!({
        "observedContentType": content_type,
        "mode": mode,
        "data": data,
        "encoding": encoding,
    })
}

/// Where one capture record is written: under the destination prefix, in the endpoint's and the
/// variant's own folder, then the hour of the invocation, then the object the record is in.
///
/// Laid out by the instant the invocation was served, so the objects of a destination are
/// readable by endpoint, by variant and by hour, and two invocations of the same hour never
/// land on one key (the event id is the object's own).
fn capture_object_key(
    prefix: &str,
    invocation: &CapturedInvocation<'_>,
    event_id: &str,
) -> String {
    let at = ::chrono::DateTime::<::chrono::Utc>::from(invocation.at);
    let folder = at.format("%Y/%m/%d/%H");
    let object = at.format("%M-%S-%3f");
    let prefix = prefix.trim_end_matches('/');
    let path = format!(
        "{}/{}/{folder}/{object}-{event_id}.{CAPTURE_OBJECT_SUFFIX}",
        invocation.endpoint_name, invocation.variant_name
    );
    if prefix.is_empty() {
        path
    } else {
        format!("{prefix}/{path}")
    }
}

/// The instant a capture record reports an invocation at, in the form a timestamp is rendered
/// in on the wire.
fn capture_instant(at: SystemTime) -> String {
    timestamp(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

/// The id one captured event carries, in the grouped form a capture record's `eventId` takes.
///
/// Minted the way every server-generated id in this service is
/// ([`super::mint_opaque_resource_id`]), then grouped 8-4-4-4-12: an identifier of one event
/// inside this account's own store, never a secret.
fn capture_event_id() -> String {
    let id = mint_opaque_resource_id();
    let groups = [0_usize, 8, 12, 16, 20, 32];
    groups
        .windows(2)
        .filter_map(|bounds| id.get(bounds[0]..bounds[1]))
        .collect::<Vec<&str>>()
        .join("-")
}

#[cfg(test)]
mod tests {
    use super::{CaptureSettings, CAPTURE_OBJECT_SUFFIX, SAMPLING_DENOMINATOR};

    /// Settings asking for the whole exchange at `sampling_percentage`.
    fn settings(sampling_percentage: i32) -> CaptureSettings {
        CaptureSettings {
            destination_s3_uri: "s3://bucket/capture/".to_string(),
            sampling_percentage,
            capture_input: true,
            capture_output: true,
            csv_content_types: Vec::new(),
            json_content_types: Vec::new(),
            role_arn: None,
        }
    }

    /// RULE: `InitialSamplingPercentage` decides the share of invocations captured — the whole
    /// share captures every one of them and a zero share none — and the decision is stable for
    /// one request.
    ///
    /// The rows span the member's modelled range (`range 0..100`): both published bounds, the
    /// values either side of them and a share in between.
    #[test]
    fn the_sampling_percentage_decides_the_share_captured() {
        let payloads: Vec<Vec<u8>> = (0..200_u32)
            .map(|row| format!("{row},{},{}\n", row + 1, row + 2).into_bytes())
            .collect();
        // (case, the percentage, how many of the payloads above must be captured)
        let rows: [(&str, i32, Option<usize>); 5] = [
            ("the whole share", 100, Some(payloads.len())),
            ("past the whole share", 100 + 1, Some(payloads.len())),
            ("no share at all", 0, Some(0)),
            ("below no share", -1, Some(0)),
            ("a share in between", 50, None),
        ];
        for (case, percentage, expected) in rows {
            let settings = settings(percentage);
            let captured = payloads
                .iter()
                .filter(|payload| settings.captures(payload))
                .count();
            if let Some(expected) = expected {
                assert_eq!(
                    captured, expected,
                    "case {case:?}: expected {expected} of {} payloads captured, got {captured}",
                    payloads.len()
                );
            } else {
                assert!(
                    captured > 0 && captured < payloads.len(),
                    "case {case:?}: a share in between captures some and not all of {} payloads, \
                     got {captured}",
                    payloads.len()
                );
            }
            // The decision is the REQUEST's, so asking again answers the same.
            for payload in &payloads {
                assert_eq!(
                    settings.captures(payload),
                    settings.captures(payload),
                    "case {case:?}: one request is captured or not captured consistently"
                );
            }
        }
        assert_eq!(
            SAMPLING_DENOMINATOR, 100,
            "the percentage is taken against the member's own range"
        );
    }

    /// RULE: a capture object's key is laid out under the destination prefix by endpoint,
    /// variant and the hour the invocation was served, whatever form the prefix takes.
    #[test]
    fn a_capture_key_is_laid_out_under_the_destination_prefix() {
        let at = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        let invocation = super::CapturedInvocation {
            endpoint_name: "ep",
            variant_name: "AllTraffic",
            inference_id: None,
            input: super::CapturedPayload {
                data: b"1,2,3",
                content_type: Some("text/csv"),
            },
            output: super::CapturedPayload {
                data: b"0.5",
                content_type: Some("text/csv"),
            },
            at,
        };
        // (case, the destination's key, what the capture key must start with)
        let rows: [(&str, &str, &str); 4] = [
            ("a prefix ending in a separator", "capture/", "capture/ep/AllTraffic/"),
            ("a prefix without one", "capture", "capture/ep/AllTraffic/"),
            ("a nested prefix", "a/b/c/", "a/b/c/ep/AllTraffic/"),
            ("the bucket root", "", "ep/AllTraffic/"),
        ];
        for (case, prefix, expected) in rows {
            let key = super::capture_object_key(prefix, &invocation, "event-id");
            assert!(
                key.starts_with(expected),
                "case {case:?}: expected a key under {expected:?}, got {key:?}"
            );
            assert!(
                key.ends_with(&format!(".{CAPTURE_OBJECT_SUFFIX}")),
                "case {case:?}: a capture object holds JSON records, got {key:?}"
            );
            // 2023-11-14T22:13:20Z: the hour folder the instant above falls in.
            assert!(
                key.contains("/2023/11/14/22/"),
                "case {case:?}: the key is laid out under the hour served, got {key:?}"
            );
        }
    }
}
