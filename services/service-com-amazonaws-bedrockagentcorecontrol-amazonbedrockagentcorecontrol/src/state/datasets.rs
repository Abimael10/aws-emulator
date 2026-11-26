// SPDX-License-Identifier: CC0-1.0

//! The evaluation DATASET's state: one [`DatasetRecord`] per `datasetId`,
//! holding the configuration the create persisted, the examples its source
//! yielded into the DRAFT working copy, every version published from that copy,
//! and the transition out of `CREATING` the clock owes it.
//!
//! The examples live on the record rather than in a store of their own, because
//! an example is addressed only THROUGH the dataset that holds it
//! (`/datasets/{datasetId}/examples`) and is gone when the dataset is — the same
//! reason a gateway target lives inside its gateway. A published version
//! ([`DatasetVersionRecord`]) is held the same way, and for the same reason.
//!
//! The ingestion of a source lives here too ([`ingest_dataset_source`]): an
//! inline source carries its examples in the request, and an Amazon S3 source
//! names a JSONL file this emulator READS out of its own object storage, so the
//! number of examples a dataset reports is the number it really holds either
//! way. An Amazon S3 source is RESOLVED before anything is persisted, so a URI
//! object storage holds nothing readable at is the modelled validation refusal
//! ([`s3_source_not_accessible`]) rather than a dataset that could never be
//! populated; only what the file's CONTENT turns out to be is reported
//! asynchronously. The SCHEMA check a batch of examples has to pass before any
//! of it is persisted is here as well ([`validate_examples`]), so the create
//! and any later write of examples cannot disagree about what an example must
//! carry ([`validate_examples`]).
//!
//! Every example in the working copy carries the id it was assigned when it
//! entered ([`DatasetExampleRecord`]), because an example is addressed by that
//! id once it is stored: the add answers the ids it minted, and a later update
//! or delete names them.
//!
//! A DELETION is held here too, as the instant the clock owes it rather than as
//! a removal the call performs: `DeleteDataset` is asynchronous, so the dataset
//! reports `DELETING` for one span ([`DATASET_REMOVAL_DELAY`]) and what the
//! request named — the whole record, or the one version it carried — is gone
//! from the instant that span passes
//! ([`super::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_datasets`]).
//! Nothing sleeps for it, and what a caller observes in that window is the
//! point: a dataset still readable and reporting `DELETING`, the writes that
//! status blocks refused as conflicts, and the modelled 404 only once the
//! removal is due — because that is how a client tells "busy, retry later" from
//! "gone, stop".

use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, SystemTime};

use ::aws_smithy_types::Document;
use ::serde_json::Value;

use crate::ops::types::{
    AddDatasetExamplesOutput, ConflictException, CreateDatasetInput, CreateDatasetOutput,
    CreateDatasetVersionOutput, DataSourceType, DatasetSummary, DatasetVersionSummary,
    DeleteDatasetExamplesOutput, DeleteDatasetOutput, GetDatasetOutput, ResourceNotFoundException,
    UpdateDatasetOutput, ValidationException,
};

use super::{sorted_map, timestamp, VALIDATION_REASON_FIELD_VALIDATION_FAILED};

/// The ARN resource type a dataset carries — `DatasetArn` is
/// `…:bedrock-agentcore:{region}:{account}:dataset/{datasetId}` (`smithy_query
/// shape=DatasetArn`).
const ARN_TYPE_DATASET: &str = "dataset";

/// The resource noun a missing VERSION of a dataset is reported by through the
/// service-wide [`super::resource_not_found`] sentence: a request that names a
/// version this dataset never published failed on the `datasetVersion` it
/// carried, not on the `datasetId`, so the dataset's own sentence
/// ([`dataset_not_found`]) — which tells the caller to verify the `datasetId` —
/// would misreport it.
pub const RESOURCE_DATASET_VERSION: &str = "dataset version";

/// The modelled not-found error for a `datasetId` this account holds no dataset
/// for ("The specified dataset does not exist. Please verify the datasetId and
/// try again.").
///
/// The dataset resource words this refusal in its OWN fixed sentence rather than
/// through the service-wide [`super::resource_not_found`] template: the text is
/// entirely fixed and names the request member the caller has to check. Nothing
/// is interpolated — the identifier is the caller's own value and the remedy does
/// not depend on it — so every dataset route that resolves a `datasetId` reports
/// a missing dataset in exactly these words.
pub fn dataset_not_found() -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(
            "The specified dataset does not exist. Please verify the datasetId and try again."
                .to_string(),
        ),
    }
}

/// The modelled not-found error for an `ExampleId` the dataset's DRAFT working
/// copy holds no example under ("The specified example does not exist in DRAFT:
/// <exampleId>. Please verify the exampleIds and try again.").
///
/// Worded in the dataset family's own sentence, like [`dataset_not_found`]: what
/// is missing and the request member the caller has to check. The one variable
/// part is the offending id, taken from the request — unlike the dataset's own
/// refusal this one names it, because a batch addressing many examples is
/// rejected WHOLE and the caller cannot otherwise tell which id was wrong.
pub fn dataset_example_not_found(example_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!(
            "The specified example does not exist in {DATASET_VERSION_DRAFT}: {example_id}. \
             Please verify the exampleIds and try again."
        )),
    }
}

/// The modelled validation error for an Amazon S3 source this emulator cannot
/// resolve to a readable object ("The specified S3 URI does not exist or is not
/// accessible: <s3Uri>").
///
/// The sentence is fixed and its one variable part is the URI the request's
/// `source` carried, verbatim. The member the request got wrong is the URI
/// itself, so `reason` says a field failed validation; the sentence already
/// quotes the offending value, so there is no `fieldList` beside it.
pub fn s3_source_not_accessible(uri: &str) -> ValidationException {
    ValidationException {
        message: format!("The specified S3 URI does not exist or is not accessible: {uri}"),
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: None,
    }
}

/// `DatasetStatus` members this service drives. The enum's full value list is
/// `CREATING | UPDATING | DELETING | ACTIVE | CREATE_FAILED | UPDATE_FAILED |
/// DELETE_FAILED` (`smithy_query shape=DatasetStatus`), where `CREATING` is
/// "CreateDataset async ingestion in progress", `UPDATING` is "An async example
/// mutation or CreateDatasetVersion is in progress" — in both of which "All
/// writes are blocked" — and `ACTIVE` is "Dataset is stable".
pub const DATASET_STATUS_CREATING: &str = "CREATING";
pub const DATASET_STATUS_ACTIVE: &str = "ACTIVE";
pub const DATASET_STATUS_CREATE_FAILED: &str = "CREATE_FAILED";
pub const DATASET_STATUS_UPDATING: &str = "UPDATING";
pub const DATASET_STATUS_DELETING: &str = "DELETING";
/// `UPDATE_FAILED` is "Last example mutation or CreateDatasetVersion failed.
/// DRAFT may be partially modified", which is where a dataset lands when the
/// source an example mutation named yielded nothing to write.
pub const DATASET_STATUS_UPDATE_FAILED: &str = "UPDATE_FAILED";

/// `DraftStatus` members a dataset reports. The enum's full value list is
/// `MODIFIED | UNMODIFIED` (`smithy_query shape=DraftStatus`): `MODIFIED` is
/// what a DRAFT with "changes not yet reflected in any published version, or no
/// versions … published yet" reports, and `UNMODIFIED` what one whose "DRAFT
/// content matches the latest published version exactly" reports.
pub const DRAFT_STATUS_MODIFIED: &str = "MODIFIED";
pub const DRAFT_STATUS_UNMODIFIED: &str = "UNMODIFIED";

/// The version a dataset read resolves to when the request names none: "Use the
/// `datasetVersion` query parameter to retrieve a specific version's metadata.
/// If absent, defaults to DRAFT" (`smithy_query doc shape=GetDataset`).
/// `DatasetVersion` is `^(DRAFT|[0-9]+)$`, so every other spelling is a
/// published version number.
pub const DATASET_VERSION_DRAFT: &str = "DRAFT";

/// How long a freshly created dataset stays `CREATING` before the clock owes it
/// the status its ingestion earned, and how long one stays `UPDATING` after a
/// version is published from it.
///
/// The create is asynchronous — "Returns immediately with status CREATING. Poll
/// GetDataset until status transitions to ACTIVE or CREATE_FAILED"
/// (`smithy_query doc shape=CreateDataset`) — and AWS documents no duration for
/// the transition, so the span is chosen: long enough that a caller polling
/// straight after the call still observes the transient status, short enough to
/// keep a client script quick.
pub const DATASET_SETTLE_DELAY: Duration = Duration::from_secs(3);

/// How long an ACCEPTED dataset deletion stays under way: the span the dataset
/// reports `DELETING` for before the clock owes the removal of what the request
/// named — the whole dataset, or the one version it asked for.
///
/// `DeleteDataset` answers 202 carrying `status`, so the deletion is accepted
/// rather than carried out by the call: "Deletes a dataset version or an entire
/// dataset asynchronously" (`smithy_query doc shape=DeleteDataset`). AWS
/// documents no duration for it, so the span is the one every other transition
/// of this resource takes ([`DATASET_SETTLE_DELAY`]), which is long enough that
/// a caller reading the dataset straight after the delete still observes
/// `DELETING` — where "read operations are still allowed"
/// (`smithy_query shape=DatasetStatus`) — and the writes it blocks.
pub const DATASET_REMOVAL_DELAY: Duration = DATASET_SETTLE_DELAY;

/// How many datasets this emulator will hold.
///
/// `CreateDataset` models `ServiceQuotaExceededException` (HTTP 402) for
/// exceeding the real service's quota; this number is not a fidelity claim about
/// that quota but the emulator's own ceiling, set generously so no realistic
/// workflow meets it, so a caller looping on create cannot grow this process's
/// memory without bound.
pub const MAX_DATASETS: usize = 1_000;

/// How many examples one ingestion will yield into a dataset.
///
/// An INLINE source is already bounded by the model — `DatasetExampleList` is
/// `length 1..1000` (`smithy_query shape=InlineExamplesSource`) — and the same
/// ceiling is applied to the examples read out of a JSONL file, so neither
/// source can grow this process's memory without bound.
pub const MAX_DATASET_EXAMPLES: usize = 1_000;

/// How many bytes of a caller's JSONL file this emulator reads, beside
/// [`MAX_DATASET_EXAMPLES`] and for the same reason.
pub const MAX_DATASET_FILE_BYTES: i64 = 32 * 1024 * 1024;

/// How deeply nested one example read out of a JSONL file may be.
///
/// The bound is this module's own: it keeps the conversion below from being
/// driven as deep as a caller's file asks. It is measured with an explicit
/// stack rather than by recursion, so measuring cannot itself overflow.
pub const MAX_EXAMPLE_DEPTH: usize = 64;

/// How many published versions this emulator will hold for one dataset.
///
/// `CreateDatasetVersion` models `ServiceQuotaExceededException` (HTTP 402) for
/// exceeding the real service's quota; this number is the emulator's own
/// ceiling, so a caller looping on publish cannot grow this process's memory
/// without bound. It is lower than the ceilings on the resources beside it
/// because every version SNAPSHOTS the whole working copy
/// ([`MAX_DATASET_EXAMPLES`]), so the memory one dataset can hold is the product
/// of the two.
pub const MAX_VERSIONS_PER_DATASET: usize = 100;

/// `arn:aws:bedrock-agentcore:{region}:{account}:dataset/{datasetId}` —
/// `DatasetArn`'s pattern.
pub fn dataset_arn(region: &str, dataset_id: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!("{ARN_TYPE_DATASET}/{dataset_id}"),
    )
}

/// The field every example of an AgentCore evaluation dataset is keyed by: both
/// AgentCore schema types describe an example as a scenario — "each example is a
/// scenario used to generate full conversations" for the simulated schema, and
/// "pre-written inputs per conversation turn" for the predefined one, each turn
/// belonging to a scenario (`smithy_query doc shape=DatasetSchemaType
/// member=AGENTCORE_EVALUATION_SIMULATED_V1` /
/// `member=AGENTCORE_EVALUATION_PREDEFINED_V1`) — so an example that names no
/// scenario belongs to none.
const EXAMPLE_FIELD_SCENARIO_ID: &str = "scenario_id";

/// The field every example of a third-party evaluation dataset carries: that
/// schema "supports single-turn (string input) and multi-turn (message list
/// input) across third-party evaluation frameworks" (`smithy_query doc
/// shape=DatasetSchemaType member=THIRD_PARTY_EVALUATION_V1`), so the input is
/// the example.
const EXAMPLE_FIELD_INPUT: &str = "input";

/// `DatasetSchemaType` members, each of which "identifies both the source format
/// and the version of that format's schema"
/// (`crate::ops::types::DATASET_SCHEMA_TYPE_VALUES`).
pub const DATASET_SCHEMA_AGENTCORE_EVALUATION_PREDEFINED_V1: &str =
    "AGENTCORE_EVALUATION_PREDEFINED_V1";
pub const DATASET_SCHEMA_AGENTCORE_EVALUATION_SIMULATED_V1: &str =
    "AGENTCORE_EVALUATION_SIMULATED_V1";
pub const DATASET_SCHEMA_THIRD_PARTY_EVALUATION_V1: &str = "THIRD_PARTY_EVALUATION_V1";

/// The fields an example of `schema_type` must carry.
///
/// The schema type "identifies both the source format and the version of that
/// format's schema", so which fields an example needs is a property of the
/// schema and not of any one request.
pub fn required_example_fields(schema_type: &str) -> &'static [&'static str] {
    match schema_type {
        DATASET_SCHEMA_THIRD_PARTY_EVALUATION_V1 => &[EXAMPLE_FIELD_INPUT],
        // Both AgentCore evaluation schemas address an example as a scenario.
        _ => &[EXAMPLE_FIELD_SCENARIO_ID],
    }
}

/// The modelled validation error for an example that does not satisfy the
/// dataset's schema: which example in the submitted batch failed, and the field
/// of the schema it omits.
///
/// `index` is the zero-based position of the offending example in the batch the
/// request carried and `field` the schema field it does not carry, so the
/// sentence around them is fixed and both values come from the request.
pub fn example_schema_violation(index: usize, field: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "Example at index {index} failed schema validation: Required field '{field}' is \
             missing from example content"
        ),
        reason: VALIDATION_REASON_FIELD_VALIDATION_FAILED.to_string(),
        field_list: None,
    }
}

/// Check every example of a submitted BATCH against the dataset's schema,
/// answering the modelled refusal of the FIRST example that does not satisfy it.
///
/// Whole-batch, before anything is persisted: a batch is one commit, so one
/// example that fails the schema refuses the whole call and leaves no dataset
/// holding the examples that would have passed — "all examples are validated
/// against the dataset's schema type before any writes occur. If any example
/// fails validation, the entire batch is rejected" (`smithy_query doc
/// shape=AddDatasetExamples`). Which batch it is does not change the rule, so
/// the create and every later write of examples check theirs here.
pub fn validate_examples(
    schema_type: &str,
    examples: &[Document],
) -> Result<(), ValidationException> {
    let required = required_example_fields(schema_type);
    for (index, example) in examples.iter().enumerate() {
        // An example is "a free-form JSON document whose structure is defined by
        // the dataset's schema type": anything that is not a JSON object carries
        // no field of that structure at all, so it is missing the first one the
        // schema requires.
        let members = match example {
            Document::Object(members) => Some(members),
            _ => None,
        };
        for field in required {
            let present = members.is_some_and(|members| members.contains_key(*field));
            if !present {
                return Err(example_schema_violation(index, field));
            }
        }
    }
    Ok(())
}

/// Whether a dataset in `status` can serve a WRITE.
///
/// Which statuses block one is the model's own account of them: `CREATING` is
/// "CreateDataset async ingestion in progress. All writes are blocked",
/// `UPDATING` is "An async example mutation or CreateDatasetVersion is in
/// progress. All writes are blocked", and for a `DELETING` dataset on its way
/// out "Read operations are still allowed" — writes are not. A dataset is
/// stable in `ACTIVE`, where "All operations are allowed per operation-specific
/// guards" (`smithy_query shape=DatasetStatus`).
///
/// Shared by the handlers rather than written per operation, so no write path
/// can disagree about when a dataset is busy.
pub fn dataset_blocks_writes(status: &str) -> bool {
    status == DATASET_STATUS_CREATING
        || status == DATASET_STATUS_UPDATING
        || status == DATASET_STATUS_DELETING
}

/// The modelled conflict a dataset write gets while the dataset is in a status
/// that blocks it ("Dataset is not in a state that allows this operation:
/// <status>").
///
/// The sentence is fixed and its one variable part is the dataset's current
/// status, spelled as the wire's enum token: the dataset resource reports a
/// blocked write by the state it is in rather than by the call that was refused,
/// which the caller already knows. Shared by the handlers so one refusal cannot
/// be worded two ways.
pub fn dataset_status_conflict(status: &str) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "Dataset is not in a state that allows this operation: {status}"
        )),
    }
}

/// What ingesting a dataset's source established.
#[derive(Debug, Clone)]
pub enum DatasetIngestion {
    /// The examples the source yielded, in the order the source lists them.
    Ingested(Vec<Document>),
    /// Nothing could be ingested. The sentence is what the dataset reports as
    /// its `failureReason` once the clock owes it `CREATE_FAILED`.
    Failed(String),
}

/// The examples the `source` of a create yields, read where they live.
///
/// An inline source carries them in the request. An Amazon S3 source names a
/// JSONL file in the caller's own bucket, which is READ out of this emulator's
/// own object storage over the in-process callback, so the examples a dataset
/// holds are the ones the file really holds.
///
/// A source is RESOLVED before it is ingested: an address object storage holds
/// no readable object at is the modelled refusal ([`s3_source_not_accessible`]),
/// answered to the caller instead of a resource nothing could ever populate.
/// What the file's CONTENT turns out to be is a different matter — a file that
/// is not a JSONL batch of examples resolved fine and ingested nothing, which is
/// the asynchronous "Initial ingestion failed. DRAFT record exists but contains
/// no examples" (`smithy_query shape=DatasetStatus`).
pub async fn ingest_dataset_source(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    source: &DataSourceType,
) -> Result<DatasetIngestion, ValidationException> {
    match source {
        DataSourceType::inlineExamples(inline) => {
            if inline.examples.len() > MAX_DATASET_EXAMPLES {
                return Ok(DatasetIngestion::Failed(too_many_examples_reason()));
            }
            Ok(DatasetIngestion::Ingested(inline.examples.clone()))
        }
        DataSourceType::s3Source(s3) => ingest_s3_file(ctx, region, &s3.s3_uri).await,
    }
}

/// The examples the JSONL file at `uri` holds, or the refusal of a `uri` that
/// resolves to no readable object.
async fn ingest_s3_file(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    uri: &str,
) -> Result<DatasetIngestion, ValidationException> {
    let Some((bucket, key)) = object_address(uri) else {
        return Err(s3_source_not_accessible(uri));
    };
    let answer = super::s3_client(ctx, region)
        .get_object()
        .bucket(&bucket)
        .key(&key)
        .send()
        .await;
    let object = match answer {
        Ok(object) => {
            // The size of the file, bounded BEFORE its bytes are collected: what
            // object storage reports for the object is what reading it would
            // allocate. The object itself resolved, so this is an ingestion that
            // yielded nothing rather than an address naming nothing.
            if object
                .content_length()
                .is_some_and(|length| length > MAX_DATASET_FILE_BYTES)
            {
                return Ok(DatasetIngestion::Failed(oversized_reason(uri)));
            }
            object
        }
        Err(error) => {
            if !names_nothing(&error) {
                // The answer says nothing about the caller's object — an
                // emulator assembled without object storage answers the
                // router's own `UnknownService`, and a transport failure answers
                // nothing at all — so it is worth a warning. The caller's own
                // values are rendered in their escaped `Debug` form, so a key
                // carrying a newline cannot forge a log line.
                ::tracing::warn!(
                    bucket = ?bucket,
                    key = ?key,
                    detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the dataset file could not be read from this emulator's object storage, so \
                     the source was refused as naming no readable object",
                );
            }
            // Either way the address resolved to nothing this caller can read,
            // which the create refuses rather than reporting a dataset for.
            return Err(s3_source_not_accessible(uri));
        }
    };
    let bytes = match object.body.collect().await {
        Ok(body) => {
            let bytes = body.into_bytes();
            if i64::try_from(bytes.len()).unwrap_or(i64::MAX) > MAX_DATASET_FILE_BYTES {
                return Ok(DatasetIngestion::Failed(oversized_reason(uri)));
            }
            bytes
        }
        Err(error) => {
            ::tracing::warn!(
                bucket = ?bucket,
                key = ?key,
                detail = %error,
                "the dataset file could not be read from this emulator's object storage, so the \
                 source was refused as naming no readable object",
            );
            return Err(s3_source_not_accessible(uri));
        }
    };
    let Ok(text) = ::std::str::from_utf8(&bytes) else {
        return Ok(DatasetIngestion::Failed(not_jsonl_reason(uri)));
    };
    Ok(parse_jsonl_examples(uri, text))
}

/// The examples a JSONL file holds: one example per non-blank line, in the order
/// the file lists them, each line a JSON value — `DatasetExampleList`'s member
/// is a `document` (`smithy_query shape=DatasetExampleList`), so an example is
/// whatever JSON the schema type gives it.
fn parse_jsonl_examples(uri: &str, text: &str) -> DatasetIngestion {
    let mut examples: Vec<Document> = Vec::new();
    for (index, line) in text
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
    {
        // The line NUMBER a reader counts, so the sentence names the line of the
        // file.
        let number = index + 1;
        if examples.len() >= MAX_DATASET_EXAMPLES {
            return DatasetIngestion::Failed(too_many_examples_reason());
        }
        let Ok(value) = ::serde_json::from_str::<Value>(line) else {
            return DatasetIngestion::Failed(format!(
                "Line {number} of the dataset file at {uri} is not JSON. Each example must be a \
                 JSON value on its own line."
            ));
        };
        if depth_exceeded(&value) {
            return DatasetIngestion::Failed(format!(
                "Line {number} of the dataset file at {uri} is nested deeper than the \
                 {MAX_EXAMPLE_DEPTH} levels an ingestion reads. Flatten the example and create \
                 the dataset again."
            ));
        }
        examples.push(json_to_document(&value));
    }
    if examples.is_empty() {
        return DatasetIngestion::Failed(format!(
            "The dataset file at {uri} holds no examples. Each example must be a JSON value on \
             its own line."
        ));
    }
    DatasetIngestion::Ingested(examples)
}

/// Why a file larger than this emulator reads ingested nothing.
fn oversized_reason(uri: &str) -> String {
    format!(
        "The dataset file at {uri} is larger than the {MAX_DATASET_FILE_BYTES} bytes an \
         ingestion reads. Split the file and create the dataset again."
    )
}

/// Why a file that is not JSONL text ingested nothing.
fn not_jsonl_reason(uri: &str) -> String {
    format!(
        "The dataset file at {uri} is not a JSONL file: it is not UTF-8 text. Each example must \
         be a JSON value on its own line."
    )
}

/// Why a source carrying more examples than one ingestion yields ingested
/// nothing.
fn too_many_examples_reason() -> String {
    format!(
        "A dataset holds at most {MAX_DATASET_EXAMPLES} examples. Split the examples across \
         datasets and create the dataset again."
    )
}

/// The bucket and key an `S3Uri` names. The member's own pattern is
/// `^s3://[a-z0-9][a-z0-9.\-]{1,61}[a-z0-9]/.{1,1024}$` (`smithy_query
/// shape=S3Source`), so a value that reached a handler names a bucket and a
/// non-empty key under it; a value that does not is read as naming no object.
fn object_address(uri: &str) -> Option<(String, String)> {
    let (bucket, key) = uri.strip_prefix("s3://")?.split_once('/')?;
    (!bucket.is_empty() && !key.is_empty()).then(|| (bucket.to_string(), key.to_string()))
}

/// Whether object storage's answer is object storage ITSELF saying it holds
/// nothing at the address the read named: the status and the codes it sends for
/// a missing bucket, key or version.
fn names_nothing<E>(
    error: &::aws_sdk_s3::error::SdkError<
        E,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool
where
    E: ::aws_smithy_types::error::metadata::ProvideErrorMetadata,
{
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    error
        .raw_response()
        .map(|response| response.status().as_u16())
        == Some(super::OBJECT_ABSENT_STATUS)
        && super::ABSENT_OBJECT_CODES.contains(&error.code().unwrap_or_default())
}

/// Whether `value` nests deeper than [`MAX_EXAMPLE_DEPTH`], walked with an
/// explicit stack so the measurement itself cannot overflow.
fn depth_exceeded(value: &Value) -> bool {
    let mut pending: Vec<(&Value, usize)> = vec![(value, 1)];
    while let Some((current, depth)) = pending.pop() {
        if depth > MAX_EXAMPLE_DEPTH {
            return true;
        }
        match current {
            Value::Array(items) => pending.extend(items.iter().map(|item| (item, depth + 1))),
            Value::Object(members) => {
                pending.extend(members.values().map(|member| (member, depth + 1)));
            }
            _ => {}
        }
    }
    false
}

/// One JSON value as the `document` an example is modelled as. The caller has
/// already established that the value nests no deeper than
/// [`MAX_EXAMPLE_DEPTH`], which bounds this walk.
fn json_to_document(value: &Value) -> Document {
    match value {
        Value::Null => Document::Null,
        Value::Bool(flag) => Document::Bool(*flag),
        Value::Number(number) => Document::Number(json_number(number)),
        Value::String(text) => Document::String(text.clone()),
        Value::Array(items) => Document::Array(items.iter().map(json_to_document).collect()),
        Value::Object(members) => Document::Object(
            members
                .iter()
                .map(|(name, member)| (name.clone(), json_to_document(member)))
                .collect(),
        ),
    }
}

/// One JSON number as the document number form that holds it without loss: an
/// unsigned integer, a signed one, or a float for everything else.
fn json_number(number: &::serde_json::Number) -> ::aws_smithy_types::Number {
    use ::aws_smithy_types::Number;

    if let Some(value) = number.as_u64() {
        Number::PosInt(value)
    } else if let Some(value) = number.as_i64() {
        Number::NegInt(value)
    } else {
        Number::Float(number.as_f64().unwrap_or_default())
    }
}

/// One example of a dataset: the free-form JSON document the source carried,
/// under the id it was assigned when it entered the dataset.
///
/// An example is addressed by that id for the rest of its life — "each example
/// is assigned an auto-generated UUID" (`smithy_query shape=InlineExamplesSource
/// member=examples`), which is what an add answers and what a later update or
/// delete names (`ExampleId`, `^[a-zA-Z0-9_.:-]+$`) — so the id is stored beside
/// the content rather than derived from the example's position in the working
/// copy, which every later write would change.
#[derive(Debug, Clone)]
pub struct DatasetExampleRecord {
    /// The `ExampleId` this example is addressed by, unique within the dataset.
    pub example_id: String,
    /// The example itself: "a free-form JSON document whose structure is
    /// defined by the dataset's schema type".
    pub content: Document,
    /// Which example of this dataset it is: the sequence it entered under,
    /// ascending in entry order and never re-used. A listing pages over the
    /// examples by this number rather than by their position in the working
    /// copy, which a delete of an earlier example changes, so a cursor handed
    /// out by one page still resumes in the right place after one.
    seq: u64,
}

/// The member a listing carries an example's own id under: "each element is a
/// JSON object containing at least an `exampleId` field plus the
/// schema-specific content fields" (`smithy_query shape=ListDatasetExamplesResponse
/// member=examples`).
const LISTED_EXAMPLE_ID_FIELD: &str = "exampleId";

impl DatasetExampleRecord {
    /// This example as a listing reports it: the content document the source
    /// carried, carrying the id the example is addressed by.
    ///
    /// The id is MERGED into the content rather than reported beside it because
    /// the list's member is the example document itself
    /// (`DatasetExampleList`), and the model says each element carries at least
    /// that field. An example that is not a JSON object carries no field at all
    /// — which the schema check refuses before anything is stored
    /// ([`validate_examples`]) — so such content is reported exactly as it is
    /// rather than wrapped in a shape the model does not declare.
    pub fn listed(&self) -> Document {
        let Document::Object(members) = &self.content else {
            return self.content.clone();
        };
        let mut listed = members.clone();
        listed.insert(
            LISTED_EXAMPLE_ID_FIELD.to_string(),
            Document::String(self.example_id.clone()),
        );
        Document::Object(listed)
    }

    /// The key a listing pages this example by: its entry sequence, zero-padded
    /// so the ascending-key order every listing of this service pages by IS the
    /// order the examples entered the dataset.
    pub fn page_key(&self) -> String {
        format!("{:020}", self.seq)
    }
}

/// One published version of a dataset: the numbered snapshot
/// `CreateDatasetVersion` publishes the working copy as.
///
/// "The DRAFT is preserved and remains editable after publishing", so a version
/// holds the examples the draft carried at the instant it was published rather
/// than a reference to the draft.
#[derive(Debug, Clone)]
pub struct DatasetVersionRecord {
    /// The version number, as `DatasetVersion` spells it: "a non-negative
    /// integer string representing a published version number".
    pub version: u32,
    /// The examples the draft held when this version was published, each under
    /// the id it is addressed by in the draft.
    pub examples: Vec<DatasetExampleRecord>,
    pub created_at: SystemTime,
}

impl DatasetVersionRecord {
    /// How many examples this version holds, as the `Long` the responses
    /// declare.
    pub fn example_count(&self) -> i64 {
        i64::try_from(self.examples.len()).unwrap_or(i64::MAX)
    }

    /// This version as `ListDatasetVersions` reports it: the three members
    /// `DatasetVersionSummary` declares, off the snapshot itself.
    pub fn summary(&self) -> DatasetVersionSummary {
        DatasetVersionSummary {
            dataset_version: self.version.to_string(),
            example_count: self.example_count(),
            created_at: timestamp(self.created_at),
        }
    }
}

/// One dataset: the configuration `CreateDataset` persisted, the examples its
/// source yielded into the DRAFT working copy, every version published from that
/// copy, and the transition out of `CREATING` the clock owes it.
///
/// Every read of the dataset reports this record, so a create and a later read
/// cannot disagree about its name, its configuration, how many examples it holds
/// or its status.
#[derive(Debug, Clone)]
pub struct DatasetRecord {
    pub dataset_id: String,
    pub dataset_arn: String,
    pub dataset_name: String,
    pub description: Option<String>,
    /// Immutable after creation, as the model documents the member.
    pub schema_type: String,
    pub kms_key_arn: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// The DRAFT working copy's examples, in the order they entered it, each
    /// under the id it is addressed by. Grown only through
    /// [`Self::push_examples`], which caps it at [`MAX_DATASET_EXAMPLES`], so no
    /// write path can grow it past that ceiling — the same reason
    /// [`Self::versions`] is private.
    draft_examples: Vec<DatasetExampleRecord>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status the dataset reports until [`Self::settles_at`].
    pub transient_status: String,
    /// The status it reports from then on: what its ingestion earned.
    pub settled_status: String,
    /// When the clock owes the move from one to the other. Stored, never slept
    /// on: a read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
    /// Why the settled status is a failure state, for a dataset whose ingestion
    /// failed; `None` for one that ingested its source.
    pub failure_reason: Option<String>,
    /// When the clock owes the removal of this dataset RECORD, for a dataset
    /// whose whole deletion has been accepted; `None` for one no delete has been
    /// asked of. Stored, never slept on, exactly as [`Self::settles_at`] is.
    pub removal_due_at: Option<SystemTime>,
    /// The versions a delete has been accepted for, each under the instant the
    /// clock owes its removal: the `DatasetVersion` the request named, so
    /// `DRAFT` (the working copy, whose EXAMPLES go) and a published version
    /// number are recorded the same way. A version in here is still reported
    /// until that instant — a deletion under way is not a deletion done.
    deleting_versions: BTreeMap<String, SystemTime>,
    /// Every version published from the working copy, in the order they were
    /// published. Grown only through [`Self::publish_version`], which caps it at
    /// [`MAX_VERSIONS_PER_DATASET`].
    versions: Vec<DatasetVersionRecord>,
    /// The number the next published version carries. Counted rather than
    /// derived from how many versions the dataset still holds, so a version
    /// `DeleteDataset` removed does not let the next publication re-use its
    /// number: a `datasetVersion` addresses one snapshot for good.
    next_version: u32,
    /// Whether the working copy holds changes no published version reflects,
    /// which is what `draftStatus` reports. True for a dataset no version has
    /// been published from, false from the instant one is.
    draft_modified: bool,
    /// How many examples have entered this dataset, counting those already
    /// removed from the working copy: the sequence the next `ExampleId` is
    /// derived from, so an id is never reused after a delete.
    next_example_seq: u64,
}

impl DatasetRecord {
    /// The dataset a create stores for the request `requested` in `region`,
    /// under the id that create minted, holding what `ingestion` yielded.
    ///
    /// A new dataset is `CREATING`, not yet settled: the transition to the
    /// status its ingestion earned — `ACTIVE`, or `CREATE_FAILED` for a source
    /// that yielded nothing — is owed by the clock rather than by this call. No
    /// version has been published from it yet, so its working copy holds changes
    /// no version reflects.
    pub fn created(
        region: &str,
        dataset_id: String,
        requested: CreateDatasetInput,
        ingestion: DatasetIngestion,
        now: SystemTime,
    ) -> Self {
        let (examples, settled_status, failure_reason) = match ingestion {
            DatasetIngestion::Ingested(examples) => {
                (examples, DATASET_STATUS_ACTIVE.to_string(), None)
            }
            DatasetIngestion::Failed(reason) => (
                // "DRAFT record exists but contains no examples."
                Vec::new(),
                DATASET_STATUS_CREATE_FAILED.to_string(),
                Some(reason),
            ),
        };
        let mut record = Self {
            dataset_arn: dataset_arn(region, &dataset_id),
            dataset_id,
            dataset_name: requested.dataset_name,
            description: requested.description,
            schema_type: requested.schema_type,
            kms_key_arn: requested.kms_key_arn,
            tags: sorted_map(requested.tags),
            draft_examples: Vec::new(),
            created_at: now,
            updated_at: now,
            transient_status: DATASET_STATUS_CREATING.to_string(),
            settled_status,
            settles_at: now + DATASET_SETTLE_DELAY,
            failure_reason,
            removal_due_at: None,
            deleting_versions: BTreeMap::new(),
            versions: Vec::new(),
            next_version: 1,
            draft_modified: true,
            next_example_seq: 0,
        };
        // "Each example is assigned an auto-generated UUID": an example gets
        // the id it is addressed by where it ENTERS the working copy, whichever
        // call put it there, so the create and a later add mint ids the same
        // way. A source yielding more examples than the working copy holds is
        // refused by that append, which leaves the draft empty: the dataset
        // settles in the failure state its source earned, as "DRAFT record
        // exists but contains no examples" describes.
        if record.push_examples(examples, now).is_none() {
            record.settled_status = DATASET_STATUS_CREATE_FAILED.to_string();
            record.failure_reason = Some(too_many_examples_reason());
        }
        record
    }

    /// The status at `now`: the transient one until the clock owes the
    /// transition, the settled one from then on. No read performs it — it is due
    /// at that instant whether or not anybody looks.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// Why the dataset is in its current status, reported only once that status
    /// IS the failure state the clock owed it: a dataset still `CREATING` has
    /// nothing to report yet.
    pub fn failure_reason_at(&self, now: SystemTime) -> Option<String> {
        if now < self.settles_at {
            return None;
        }
        self.failure_reason.clone()
    }

    /// How many examples the DRAFT holds, as the `Long` the responses declare.
    pub fn example_count(&self) -> i64 {
        i64::try_from(self.draft_examples.len()).unwrap_or(i64::MAX)
    }

    /// What `draftStatus` reports: whether the working copy holds changes no
    /// published version reflects.
    pub fn draft_status(&self) -> &'static str {
        if self.draft_modified {
            DRAFT_STATUS_MODIFIED
        } else {
            DRAFT_STATUS_UNMODIFIED
        }
    }

    /// Every version published from this dataset, in the order they were
    /// published.
    pub fn versions(&self) -> &[DatasetVersionRecord] {
        &self.versions
    }

    /// The DRAFT working copy's examples, in the order they entered it.
    ///
    /// Read-only, like [`Self::versions`]: an example enters the working copy
    /// only through [`Self::add_examples`], which is what keeps it inside
    /// [`MAX_DATASET_EXAMPLES`].
    pub fn draft_examples(&self) -> &[DatasetExampleRecord] {
        &self.draft_examples
    }

    /// The version a `datasetVersion` names, or `None` when this dataset holds
    /// no such published version.
    pub fn version(&self, version: u32) -> Option<&DatasetVersionRecord> {
        self.versions
            .iter()
            .find(|published| published.version == version)
    }

    /// Publish the working copy as this dataset's next numbered version,
    /// answering the number it was published as — or `None` when the dataset
    /// already holds [`MAX_VERSIONS_PER_DATASET`] of them — whereupon the caller
    /// answers the operation's modelled quota error rather than growing this
    /// process's memory. The ceiling lives here, at the ONE place a version is
    /// appended (`versions` is private for exactly that reason), so no publish
    /// path can skip it.
    ///
    /// "The DRAFT is preserved and remains editable after publishing", so the
    /// version is a snapshot of the examples the draft holds now and the draft
    /// itself is left in place — matching the version it was published as until
    /// something changes it again. The dataset reports `UPDATING` until the
    /// clock owes it `ACTIVE`: "Always UPDATING immediately after this call".
    pub fn publish_version(&mut self, now: SystemTime) -> Option<u32> {
        if self.versions.len() >= MAX_VERSIONS_PER_DATASET {
            return None;
        }
        // Versions are numbered from one, in publication order, and a number is
        // never re-used after the version carrying it is deleted.
        let version = self.next_version;
        self.next_version = self.next_version.saturating_add(1);
        self.versions.push(DatasetVersionRecord {
            version,
            examples: self.draft_examples.clone(),
            created_at: now,
        });
        self.draft_modified = false;
        self.updated_at = now;
        self.transient_status = DATASET_STATUS_UPDATING.to_string();
        self.settled_status = DATASET_STATUS_ACTIVE.to_string();
        self.settles_at = now + DATASET_SETTLE_DELAY;
        // The publication succeeded, so nothing of an earlier failure is owed.
        self.failure_reason = None;
        Some(version)
    }

    /// Whether this dataset's own deletion has been accepted and is still under
    /// way: it reports `DELETING`, and the clock owes the removal of the record.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Accept the deletion of the WHOLE dataset at `now`: "if `datasetVersion`
    /// is absent, deletes all versions and the dataset record itself"
    /// (`smithy_query doc shape=DeleteDataset`).
    ///
    /// The deletion is asynchronous, so this call only accepts it: the dataset
    /// reports `DELETING` from this instant on — where "read operations are
    /// still allowed" — and the clock owes the removal of the record, with every
    /// version published from it, one span later
    /// ([`DATASET_REMOVAL_DELAY`]). Nothing sleeps for it: the instant is stored
    /// and a later call materialises what it made due
    /// ([`super::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_datasets`]).
    ///
    /// `DeleteDataset` is `@idempotent`, so a repeat while the deletion is still
    /// under way leaves the instant it is owed alone rather than pushing the
    /// removal further out.
    pub fn accept_deletion(&mut self, now: SystemTime) {
        if self.is_deleting() {
            return;
        }
        self.transient_status = DATASET_STATUS_DELETING.to_string();
        self.settled_status = DATASET_STATUS_DELETING.to_string();
        self.settles_at = now;
        self.removal_due_at = Some(now + DATASET_REMOVAL_DELAY);
        // The deletion was accepted, so nothing of an earlier failure is owed.
        self.failure_reason = None;
        self.updated_at = now;
    }

    /// Whether the deletion of the version `label` names has already been
    /// accepted and is still under way.
    pub fn version_deletion_accepted(&self, label: &str) -> bool {
        self.deleting_versions.contains_key(label)
    }

    /// Accept the deletion of ONE version at `now`: "if provided, deletes only
    /// that specific version" — the dataset record and every other version it
    /// holds stay.
    ///
    /// Asynchronous like the whole-dataset deletion and owed by the same clock:
    /// the dataset reports `DELETING` until the span passes, the version it
    /// named is still readable for exactly that long, and the status it was
    /// already settled in is what it reports from then on — a version deleted
    /// out of a dataset whose own ingestion or last mutation FAILED leaves that
    /// failure, and the reason it carries, exactly as they were.
    ///
    /// Idempotent for the same reason [`Self::accept_deletion`] is: a repeat
    /// naming the version a deletion is already under way for does not re-arm
    /// the clock.
    pub fn accept_version_deletion(&mut self, label: &str, now: SystemTime) {
        if self.version_deletion_accepted(label) {
            return;
        }
        let due_at = now + DATASET_REMOVAL_DELAY;
        self.deleting_versions.insert(label.to_string(), due_at);
        self.transient_status = DATASET_STATUS_DELETING.to_string();
        self.settles_at = due_at;
        self.updated_at = now;
    }

    /// Carry out every version deletion of this dataset the clock has already
    /// made due, which was owed at that instant whether or not anybody called.
    ///
    /// A published version is gone from the dataset's history. The `DRAFT` is
    /// the working copy, which exists for as long as the dataset record does
    /// ("if absent, defaults to DRAFT" is how every dataset route addresses it),
    /// so what a delete naming it removes is the examples it holds — leaving a
    /// working copy whose content no published version reflects.
    pub fn expire_deleted_versions(&mut self, now: SystemTime) {
        let due: Vec<String> = self
            .deleting_versions
            .iter()
            .filter(|(_, due_at)| now >= **due_at)
            .map(|(label, _)| label.clone())
            .collect();
        for label in due {
            self.deleting_versions.remove(&label);
            if label == DATASET_VERSION_DRAFT {
                let held_examples = !self.draft_examples.is_empty();
                self.draft_examples.clear();
                self.draft_modified = self.draft_modified || held_examples;
                continue;
            }
            // `DatasetVersion` is `^(DRAFT|[0-9]+)$`, so every other label a
            // request can carry is a published version number.
            if let Ok(version) = label.parse::<u32>() {
                self.versions
                    .retain(|published| published.version != version);
            }
        }
    }

    /// What the delete of `version` of this dataset reports: the five members
    /// `DeleteDatasetResponse` declares, every one of them off the record this
    /// delete just wrote, so the status it answers is the one a read of the
    /// dataset reports and `updatedAt` is "the timestamp when the delete was
    /// initiated".
    pub fn delete_output(&self, version: &str, now: SystemTime) -> DeleteDatasetOutput {
        DeleteDatasetOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            status: self.status_at(now).to_string(),
            dataset_version: version.to_string(),
            updated_at: timestamp(self.updated_at),
        }
    }

    /// Append `examples` to the DRAFT working copy, answering the `ExampleId`
    /// minted for each one in the order they were added — or `None` when the
    /// batch would take the working copy past [`MAX_DATASET_EXAMPLES`], where
    /// the caller answers the operation's modelled quota error rather than
    /// growing this process's memory. Nothing of a refused batch is written.
    ///
    /// "Adds examples to the dataset's DRAFT": the working copy grows and no
    /// published version is touched, so the draft now holds changes no version
    /// reflects. The mutation is ASYNCHRONOUS, so the dataset reports `UPDATING`
    /// — in which "an async example mutation … is in progress" and "all writes
    /// are blocked" — until the clock owes it `ACTIVE`.
    pub fn add_examples(&mut self, examples: Vec<Document>, now: SystemTime) -> Option<Vec<String>> {
        let minted = self.push_examples(examples, now)?;
        self.draft_modified = true;
        self.updated_at = now;
        self.transient_status = DATASET_STATUS_UPDATING.to_string();
        self.settled_status = DATASET_STATUS_ACTIVE.to_string();
        self.settles_at = now + DATASET_SETTLE_DELAY;
        // The mutation succeeded, so nothing of an earlier failure is owed.
        self.failure_reason = None;
        Some(minted)
    }

    /// Record that an example mutation of this dataset wrote NOTHING, because
    /// the source it named yielded no examples.
    ///
    /// The mutation is asynchronous either way, so the dataset reports
    /// `UPDATING` until the clock owes it `UPDATE_FAILED` — where "the last
    /// example mutation … failed" — and `reason` is the `failureReason` it
    /// reports from then on. The working copy is left exactly as it was.
    pub fn fail_example_mutation(&mut self, reason: String, now: SystemTime) {
        self.updated_at = now;
        self.transient_status = DATASET_STATUS_UPDATING.to_string();
        self.settled_status = DATASET_STATUS_UPDATE_FAILED.to_string();
        self.settles_at = now + DATASET_SETTLE_DELAY;
        self.failure_reason = Some(reason);
    }

    /// What an add of examples to this dataset reports: the six members
    /// `AddDatasetExamplesResponse` declares, every one of them off the record
    /// just written, so the `updatedAt` it answers is the one a later read
    /// reports and `addedCount` is the number of ids it hands back.
    pub fn add_examples_output(
        &self,
        example_ids: Vec<String>,
        now: SystemTime,
    ) -> AddDatasetExamplesOutput {
        AddDatasetExamplesOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            status: self.status_at(now).to_string(),
            added_count: i64::try_from(example_ids.len()).unwrap_or(i64::MAX),
            updated_at: timestamp(self.updated_at),
            example_ids,
        }
    }

    /// The first id of `example_ids` the DRAFT working copy holds no example
    /// under, or `None` when it holds one for every id in the batch.
    ///
    /// The whole batch is checked BEFORE anything is removed — "all example IDs
    /// are validated before any deletes occur. If any ID does not exist in
    /// DRAFT, the entire batch is rejected (all-or-nothing semantics)"
    /// (`smithy_query doc shape=DeleteDatasetExamples`) — so a caller naming one
    /// id the draft does not hold is refused with the draft exactly as it was,
    /// rather than part-way through the batch.
    pub fn first_example_not_held(&self, example_ids: &[String]) -> Option<String> {
        let held: HashSet<&str> = self
            .draft_examples
            .iter()
            .map(|example| example.example_id.as_str())
            .collect();
        example_ids
            .iter()
            .find(|example_id| !held.contains(example_id.as_str()))
            .cloned()
    }

    /// Remove every example `example_ids` names from the DRAFT working copy,
    /// answering the ids that were really removed in the order the working copy
    /// held them.
    ///
    /// "Deletes specific examples by ID from DRAFT": only the working copy is
    /// touched, so every published version keeps the examples it snapshotted and
    /// the draft now holds changes no version reflects. An id repeated in one
    /// batch addresses the one example it names, which goes once, so the count
    /// this answers is the number of examples that left the draft.
    ///
    /// The mutation is ASYNCHRONOUS like every other example mutation, so the
    /// dataset reports `UPDATING` — in which "an async example mutation … is in
    /// progress" and "all writes are blocked" — until the clock owes it
    /// `ACTIVE`.
    pub fn delete_examples(&mut self, example_ids: &[String], now: SystemTime) -> Vec<String> {
        let requested: HashSet<&str> = example_ids.iter().map(String::as_str).collect();
        let mut deleted = Vec::new();
        self.draft_examples.retain(|example| {
            if requested.contains(example.example_id.as_str()) {
                deleted.push(example.example_id.clone());
                return false;
            }
            true
        });
        self.draft_modified = true;
        self.updated_at = now;
        self.transient_status = DATASET_STATUS_UPDATING.to_string();
        self.settled_status = DATASET_STATUS_ACTIVE.to_string();
        self.settles_at = now + DATASET_SETTLE_DELAY;
        // The mutation succeeded, so nothing of an earlier failure is owed.
        self.failure_reason = None;
        deleted
    }

    /// What a delete of examples from this dataset reports: the five members
    /// `DeleteDatasetExamplesResponse` declares, every one of them off the
    /// record just written, so the status it answers is the one a read of the
    /// dataset reports and `deletedCount` is the number of examples that left
    /// the working copy.
    pub fn delete_examples_output(
        &self,
        deleted_count: usize,
        now: SystemTime,
    ) -> DeleteDatasetExamplesOutput {
        DeleteDatasetExamplesOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            status: self.status_at(now).to_string(),
            deleted_count: i64::try_from(deleted_count).unwrap_or(i64::MAX),
            updated_at: timestamp(self.updated_at),
        }
    }

    /// Put `examples` into the working copy under freshly minted ids, answering
    /// those ids in the order the examples were appended — or `None` when the
    /// batch would take the working copy past [`MAX_DATASET_EXAMPLES`], in which
    /// case not one of them is appended.
    ///
    /// The ONE place an example enters a dataset, which is why the ceiling lives
    /// here (`draft_examples` is private for exactly that reason): so every
    /// example the dataset holds carries an id, no two of them carry the same
    /// one, and no write path can grow the working copy without bound.
    fn push_examples(&mut self, examples: Vec<Document>, now: SystemTime) -> Option<Vec<String>> {
        if self.draft_examples.len().saturating_add(examples.len()) > MAX_DATASET_EXAMPLES {
            return None;
        }
        let mut held: HashSet<String> = self
            .draft_examples
            .iter()
            .map(|example| example.example_id.clone())
            .collect();
        let mut minted = Vec::with_capacity(examples.len());
        for content in examples {
            let example_id = self.next_example_id(&mut held, now);
            minted.push(example_id.clone());
            self.draft_examples.push(DatasetExampleRecord {
                example_id,
                content,
                seq: self.next_example_seq,
            });
        }
        Some(minted)
    }

    /// An `ExampleId` no example of this dataset holds: the UUID form the model
    /// documents ("an auto-generated UUID"), derived from this dataset's id and
    /// the next example sequence, which `ExampleId`'s own pattern
    /// `^[a-zA-Z0-9_.:-]+$` admits.
    fn next_example_id(&mut self, held: &mut HashSet<String>, at: SystemTime) -> String {
        loop {
            self.next_example_seq = self.next_example_seq.wrapping_add(1);
            let candidate = super::uuid_shaped(&self.dataset_id, self.next_example_seq, at);
            if held.insert(candidate.clone()) {
                return candidate;
            }
        }
    }

    /// The tags as a response reports them: the map the create carried, or no
    /// member at all for a dataset carrying none, so an untagged dataset is not
    /// reported as one holding an empty tag set.
    fn reported_tags(&self) -> Option<HashMap<String, String>> {
        if self.tags.is_empty() {
            return None;
        }
        Some(
            self.tags
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    }

    /// Apply an update of this dataset's METADATA: "Only provided fields are
    /// updated; omitted fields remain unchanged", so a member the request does
    /// not carry is left exactly as it was rather than cleared.
    ///
    /// The update is SYNCHRONOUS, so the dataset keeps the status it is already
    /// in and the clock owes it no transition. It touches no example — "To
    /// modify dataset content, use AddDatasetExamples, UpdateDatasetExamples, or
    /// DeleteDatasetExamples" — so the working copy, every published version and
    /// the DRAFT's publish synchronization state are all left alone.
    pub fn apply_metadata_update(&mut self, description: Option<String>, now: SystemTime) {
        if description.is_some() {
            self.description = description;
        }
        self.updated_at = now;
    }

    /// What an update of this dataset reports: the three members
    /// `UpdateDatasetResponse` declares, every one of them off the record just
    /// written, so the `updatedAt` it answers is the one a later read reports.
    pub fn update_output(&self) -> UpdateDatasetOutput {
        UpdateDatasetOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            updated_at: timestamp(self.updated_at),
        }
    }

    /// What the create of this dataset reports: the four members
    /// `CreateDatasetResponse` declares, every one of them off the record just
    /// written.
    pub fn create_output(&self, now: SystemTime) -> CreateDatasetOutput {
        CreateDatasetOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            status: self.status_at(now).to_string(),
            created_at: timestamp(self.created_at),
        }
    }

    /// What the publication of `version` from this dataset reports.
    pub fn create_version_output(
        &self,
        version: &DatasetVersionRecord,
        now: SystemTime,
    ) -> CreateDatasetVersionOutput {
        CreateDatasetVersionOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            status: self.status_at(now).to_string(),
            dataset_version: version.version.to_string(),
            created_at: timestamp(version.created_at),
        }
    }

    /// What a read of this dataset reports, for the version the request resolved
    /// to: the DRAFT working copy, or one version published from it.
    ///
    /// Only the version's own members differ between the two — the identifier it
    /// is addressed by and the examples it holds; everything else is a property
    /// of the dataset.
    pub fn get_output(
        &self,
        version: Option<&DatasetVersionRecord>,
        now: SystemTime,
    ) -> GetDatasetOutput {
        GetDatasetOutput {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            dataset_version: version.map_or_else(
                || DATASET_VERSION_DRAFT.to_string(),
                |published| published.version.to_string(),
            ),
            dataset_name: self.dataset_name.clone(),
            description: self.description.clone(),
            status: self.status_at(now).to_string(),
            // The DRAFT's publish synchronization state, which is a property of
            // the working copy whichever version was read.
            draft_status: Some(self.draft_status().to_string()),
            failure_reason: self.failure_reason_at(now),
            schema_type: self.schema_type.clone(),
            kms_key_arn: self.kms_key_arn.clone(),
            example_count: version
                .map_or_else(|| self.example_count(), DatasetVersionRecord::example_count),
            // "Presigned Amazon S3 URL to download the consolidated dataset
            // file for the resolved version. … Omitted if the file does not yet
            // exist": this emulator consolidates no file into object storage, so
            // there is no URL to publish and none is invented.
            download_url: None,
            download_url_expires_at: None,
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
            tags: self.reported_tags(),
        }
    }

    /// This dataset as `ListDatasets` reports it: the members `DatasetSummary`
    /// declares, off the same record a read answers from.
    pub fn summary(&self, now: SystemTime) -> DatasetSummary {
        DatasetSummary {
            dataset_arn: self.dataset_arn.clone(),
            dataset_id: self.dataset_id.clone(),
            dataset_name: self.dataset_name.clone(),
            description: self.description.clone(),
            status: self.status_at(now).to_string(),
            draft_status: Some(self.draft_status().to_string()),
            schema_type: self.schema_type.clone(),
            example_count: self.example_count(),
            created_at: timestamp(self.created_at),
            updated_at: timestamp(self.updated_at),
        }
    }
}

impl super::ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Mint a `datasetId` for `name` that no stored dataset holds: the name +
    /// `-` + 10 alphanumerics, which `DatasetId`'s pattern
    /// `^[a-zA-Z0-9_-]{1,110}$` admits for every `DatasetName`
    /// (`^[a-zA-Z][a-zA-Z0-9_]{0,47}$`) a create can carry.
    pub fn mint_dataset_id(&mut self, name: &str, at: SystemTime) -> String {
        loop {
            self.mint_counter = self.mint_counter.wrapping_add(1);
            let candidate = format!("{name}-{}", super::suffix_for(name, self.mint_counter, at));
            if !self.datasets.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// Carry out every dataset deletion the clock has already made due: the
    /// datasets whose whole record is owed removal — with the name each of them
    /// reserved, which is free again from that instant — and, on the datasets
    /// that remain, the versions whose own deletion is owed
    /// ([`DatasetRecord::expire_deleted_versions`]).
    ///
    /// `DeleteDataset` ACCEPTS the deletion (202) and reports the dataset
    /// `DELETING`; what it asked to remove goes when the span it owes passes, so
    /// a caller polling the dataset sees `DELETING` and then the modelled 404.
    /// The removal is owed by the CLOCK — it was due at this instant whether or
    /// not anybody called — so every operation that reaches the dataset plane
    /// materialises it before reading, and nothing ever sleeps for it.
    pub fn expire_deleted_datasets(&mut self, now: SystemTime) {
        let due: Vec<String> = self
            .datasets
            .iter()
            .filter(|(_, record)| record.removal_due_at.is_some_and(|at| now >= at))
            .map(|(dataset_id, _)| dataset_id.clone())
            .collect();
        for dataset_id in due {
            if let Some(record) = self.datasets.remove(&dataset_id) {
                self.dataset_names.remove(&record.dataset_name);
            }
        }
        for record in self.datasets.values_mut() {
            record.expire_deleted_versions(now);
        }
    }

    /// The dataset a previous request to `operation` carrying this `clientToken`
    /// produced, if the record is still held and the dataset still exists.
    pub fn replayed_dataset_result(
        &self,
        operation: &str,
        client_token: Option<&str>,
    ) -> Option<String> {
        let dataset_id = self
            .dataset_idempotency
            .get(&super::token_key(operation, client_token?))?;
        self.datasets
            .contains_key(dataset_id)
            .then(|| dataset_id.clone())
    }

    /// Record what a `clientToken` produced for `operation`, evicting the oldest
    /// record once `MAX_REMEMBERED_CLIENT_TOKENS` are held. An evicted token is
    /// indistinguishable from one whose idempotency window has passed: the next
    /// request carrying it is served as a fresh one.
    pub fn remember_dataset_client_token(
        &mut self,
        operation: &str,
        client_token: Option<String>,
        dataset_id: &str,
    ) {
        let Some(token) = client_token else {
            return;
        };
        super::remember_token(
            &mut self.dataset_idempotency,
            &mut self.dataset_idempotency_order,
            super::token_key(operation, &token),
            dataset_id.to_string(),
        );
    }

    /// The example ids a previous request to `operation` carrying this
    /// `clientToken` added to THIS dataset, if the record is still held and the
    /// dataset still exists.
    ///
    /// Answered only for the dataset the record names: a token replayed against
    /// a different dataset is a different request, so it is served as a fresh
    /// one rather than answering ids the dataset the request named does not
    /// hold.
    ///
    /// Separate from the records above because what an example mutation
    /// produced is a set of examples rather than a resource of its own: a
    /// replayed add answers the ids it minted the first time instead of
    /// appending the batch twice.
    pub fn replayed_dataset_examples(
        &self,
        operation: &str,
        client_token: Option<&str>,
        dataset_id: &str,
    ) -> Option<Vec<String>> {
        let (remembered, example_ids) = self
            .dataset_examples_idempotency
            .get(&super::token_key(operation, client_token?))?;
        (remembered == dataset_id && self.datasets.contains_key(dataset_id))
            .then(|| example_ids.clone())
    }

    /// Record which examples a `clientToken` added for `operation`, evicting the
    /// oldest record once `MAX_REMEMBERED_CLIENT_TOKENS` are held, on the same
    /// rule as every other idempotency record of this service.
    pub fn remember_dataset_examples_client_token(
        &mut self,
        operation: &str,
        client_token: Option<String>,
        dataset_id: &str,
        example_ids: &[String],
    ) {
        let Some(token) = client_token else {
            return;
        };
        super::remember_token(
            &mut self.dataset_examples_idempotency,
            &mut self.dataset_examples_idempotency_order,
            super::token_key(operation, &token),
            (dataset_id.to_string(), example_ids.to_vec()),
        );
    }

    /// The (dataset, version number) a previous request to `operation` carrying
    /// this `clientToken` published, if the record is still held and the version
    /// it names still exists.
    pub fn replayed_dataset_version(
        &self,
        operation: &str,
        client_token: Option<&str>,
    ) -> Option<(String, u32)> {
        let (dataset_id, version) = self
            .dataset_version_idempotency
            .get(&super::token_key(operation, client_token?))?;
        self.datasets
            .get(dataset_id)
            .and_then(|record| record.version(*version))
            .map(|_| (dataset_id.clone(), *version))
    }

    /// Record which version a `clientToken` published for `operation`, evicting
    /// the oldest record once `MAX_REMEMBERED_CLIENT_TOKENS` are held, on the
    /// same rule as every other idempotency record of this service.
    pub fn remember_dataset_version_client_token(
        &mut self,
        operation: &str,
        client_token: Option<String>,
        dataset_id: &str,
        version: u32,
    ) {
        let Some(token) = client_token else {
            return;
        };
        super::remember_token(
            &mut self.dataset_version_idempotency,
            &mut self.dataset_version_idempotency_order,
            super::token_key(operation, &token),
            (dataset_id.to_string(), version),
        );
    }
}
