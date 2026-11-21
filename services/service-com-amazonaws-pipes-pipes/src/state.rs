// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-pipes-pipes service.
//!
//! An EventBridge Pipes pipe is a record here: the control plane (the record, its ARN,
//! its lifecycle states and its timestamps) is real and persisted in this module, and
//! what a RUNNING pipe DOES with no client action — poll the source, hand the batch to
//! the enrichment, deliver each event to the target — is [`data_plane`].
//!
//! Two modules serve the stages that data plane runs: [`execution_role`], the decision of
//! whether the pipe's own `RoleArn` lets a stage make the call it is about to make — and,
//! before any stage runs, whether this service can assume that role at all, which is what
//! the lifecycle calls that store a role settle the pipe's terminal failure state from —
//! and [`metrics`], the CloudWatch series the running pipe publishes about its executions —
//! the only signal that distinguishes a pipe whose stage was refused from a pipe that
//! never ran.
//!
//! [`request_authorization`] is the other identity decision, and not to be confused with
//! the first: whether the principal that SIGNED a control-plane call may perform it on the
//! pipe it addresses, which every operation asks before it reads or writes a record.
//!
//! [`bring_up`] is the lifecycle's own reading of those: what a pipe could not poll its
//! source without, which is where a creation, a start or a reconfiguration that cannot be
//! completed settles.
//!
//! [`kms_key`] is the one resource a pipe names that is resolved BEFORE the write rather
//! than settled after it: the customer managed key its data is to be encrypted under, which
//! KMS owns and which a write naming a key KMS holds none for is refused on.

pub mod bring_up;
pub mod data_plane;
pub mod execution_role;
pub mod kms_key;
pub mod metrics;
pub mod request_authorization;

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime};

use crate::ops::types::{
    ConflictException, FilterCriteria, NotFoundException, Pipe, PipeEnrichmentParameters,
    PipeLogConfiguration, PipeSourceParameters, PipeTargetParameters, ValidationException,
    ValidationExceptionField,
};

/// The `PipeState` members this slice reports. Source: the `PipeState` enum of
/// `smithy-model/com-amazonaws-pipes-pipes.json` (see `PIPE_STATE_VALUES`).
pub mod pipe_state {
    pub const CREATING: &str = "CREATING";
    pub const RUNNING: &str = "RUNNING";
    pub const STARTING: &str = "STARTING";
    pub const STOPPED: &str = "STOPPED";
    pub const STOPPING: &str = "STOPPING";
    pub const DELETING: &str = "DELETING";
    pub const UPDATING: &str = "UPDATING";
    /// Where a pipe rests when the creation `CREATING` began could not be completed.
    pub const CREATE_FAILED: &str = "CREATE_FAILED";
    /// Where a pipe rests when the start `STARTING` began could not be completed.
    pub const START_FAILED: &str = "START_FAILED";
    /// Where a pipe rests when the reconfiguration `UPDATING` began could not be
    /// completed.
    pub const UPDATE_FAILED: &str = "UPDATE_FAILED";
}

/// The `PipeState` values that mean "a move this pipe is already making has not landed
/// yet" — every state above that is not one a pipe rests in (`RUNNING`, `STOPPED`).
///
/// A pipe in one of these is mid-transition, so a call that would start ANOTHER
/// transition on it is refused rather than interleaved with the one in flight.
pub const TRANSIENT_PIPE_STATES: &[&str] = &[
    pipe_state::CREATING,
    pipe_state::STARTING,
    pipe_state::STOPPING,
    pipe_state::DELETING,
    pipe_state::UPDATING,
];

/// The `PipeState` values that mean "the move this pipe was making could not be completed"
/// — the terminal states a failed move settles in, each named by the move that failed.
///
/// A pipe in one of these is not running and is not on its way to running, so nothing polls
/// its source, and what it reports about itself is the sentence saying what stopped it.
pub const FAILED_PIPE_STATES: &[&str] = &[
    pipe_state::CREATE_FAILED,
    pipe_state::START_FAILED,
    pipe_state::UPDATE_FAILED,
];

/// The states a caller may ask a pipe to be in (`RequestedPipeState`), plus the
/// `DELETED` that only a response reports (`RequestedPipeStateDescribeResponse`).
/// Source: those two enums in `smithy-model/com-amazonaws-pipes-pipes.json`.
///
/// `DELETED` is the one no write installs: a delete answers with the run state the pipe is
/// wound down to (see `ops/delete_pipe.rs`), and the token stays here because it is the
/// modelled value of the member either way.
pub mod requested_state {
    pub const RUNNING: &str = "RUNNING";
    pub const STOPPED: &str = "STOPPED";
    pub const DELETED: &str = "DELETED";
}

/// The `resourceType` every error of this service that carries one names: the pipe
/// resource, the only resource type this service models.
///
/// It is FIXED text — the name of the resource type, not a value out of the request — and
/// is spelled the way the API names the resource itself, with a leading capital, so the
/// token is the same whichever error (`ConflictException`, `ServiceQuotaExceededException`)
/// reports it and whichever operation raised it.
pub const PIPE_RESOURCE_TYPE: &str = "Pipe";

/// The scheme of the one non-ARN `Source` this service accepts: a self-managed Apache
/// Kafka cluster. Source: the `^smk://…|arn:…$` alternation of `ArnOrUrl`'s pattern in
/// `smithy-model/com-amazonaws-pipes-pipes.json`.
pub const SELF_MANAGED_KAFKA_SCHEME: &str = "smk://";

/// The source type [`PipeRecord::source_type`] reports for a `Source` carrying
/// [`SELF_MANAGED_KAFKA_SCHEME`], which has no ARN service namespace to be named by.
pub const SELF_MANAGED_KAFKA: &str = "selfmanagedkafka";

/// The most pipe records this store holds, so a caller cannot grow the emulator's heap
/// without limit by creating distinct names.
///
/// This is the EMULATOR's bound, not an AWS number (AWS's own soft quota is larger): each
/// record is already size-bounded by the modelled `@length` constraints, so capping the
/// count bounds the store. `CreatePipe` refuses past it with the quota error its model
/// declares for exactly this case, `ServiceQuotaExceededException` (402).
pub const MAX_PIPES: usize = 100;

/// The most tags one pipe carries.
///
/// Source: the `TagResource` documentation in `smithy-model/com-amazonaws-pipes-pipes.json`
/// — "You can associate as many as 50 tags with a pipe." — the same bound as `TagMap`'s
/// `@length(max: 50)`. A single request past the bound is already refused by the boundary
/// validator; `TagResource` re-checks the bound against the tags ALREADY on the pipe, which
/// no per-request constraint can express.
pub const MAX_TAGS_PER_PIPE: usize = 50;

/// The length a tag key must be within: `@length(min: 1, max: 128)` on `TagKey`. Source:
/// the `TagMap` key member of `smithy-model/com-amazonaws-pipes-pipes.json`.
///
/// A constraint on a MAP KEY is not one the boundary validator checks (it walks the
/// collection's own `@length` and its values), so the writes that install tags enforce it
/// themselves — and refuse a key outside it with the `ValidationException` their models
/// declare, exactly as the boundary would have.
pub const TAG_KEY_LENGTH: std::ops::RangeInclusive<usize> = 1..=128;

/// The length a tag value must be within: `@length(min: 0, max: 256)` on `TagValue`.
/// Source: the `TagMap` value member of the same model. The empty string is therefore a
/// value a tag can hold, not a way of removing one.
pub const TAG_VALUE_LENGTH: std::ops::RangeInclusive<usize> = 0..=256;

/// The tag-key prefix AWS reserves for its own tags: "You cannot edit or delete tag keys
/// or values with this prefix. Tags with this prefix do not count against your tags per
/// resource limit." (AWS General Reference, "Tag naming limits and requirements"), so a
/// caller-supplied key carrying it is refused rather than stored. Matched
/// case-insensitively, the way the reservation is stated.
pub const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// The sentence a key using [`RESERVED_TAG_KEY_PREFIX`] is refused with: FIXED, documented
/// text whose parenthetical states the rule itself, so it interpolates nothing — neither
/// the key that used the prefix nor the resource it was meant for reaches the caller.
pub const RESERVED_TAG_KEY_PREFIX_MESSAGE: &str =
    "Invalid tag key. Customers cannot set tag keys starting with aws: (case insensitive)";

/// The special characters a tag may carry besides letters, numbers and spaces. Source:
/// the documented tag rule [`TAG_CHARACTER_SET_MESSAGE`] states verbatim.
const TAG_SPECIAL_CHARACTERS: &[char] = &['_', '.', ':', '/', '=', '+', '-', '@'];

/// The sentence a tag carrying a character outside the documented set is refused with.
/// It is a FIXED, documented rule rather than a message built from the request, so it
/// names neither the key nor the value that broke it.
pub const TAG_CHARACTER_SET_MESSAGE: &str =
    "Tags can only contain letters, numbers, spaces, and the following special \
     characters: _ . : / = + - @";

/// Whether `text` is within the documented tag character set: letters, numbers, spaces
/// and [`TAG_SPECIAL_CHARACTERS`]. "Letters" and "numbers" are NOT ASCII-only — a tag
/// written in another script is letters and round-trips — so both are judged by unicode
/// category rather than by byte range.
fn within_tag_character_set(text: &str) -> bool {
    text.chars().all(|character| {
        character.is_alphanumeric()
            || character == ' '
            || TAG_SPECIAL_CHARACTERS.contains(&character)
    })
}

/// The most characters of a tag KEY a refusal echoes back.
///
/// A key reaches a handler UNBOUNDED — the boundary validator walks a map's entry count
/// and its values but not its keys — so a message that interpolated one whole would let a
/// request be amplified into an arbitrarily long response. The same bound the framework's
/// own validation messages elide an echoed value at. A tag VALUE needs no such bound:
/// `TagValue`'s own `@length` is one the validator does walk.
const MAX_ECHOED_KEY: usize = 256;

/// `key` as a refusal echoes it: itself, or its first [`MAX_ECHOED_KEY`] characters with
/// an ellipsis. Cut on a CHARACTER boundary, so a multi-byte key stays valid text.
fn echoed_key(key: &str) -> String {
    if key.chars().count() <= MAX_ECHOED_KEY {
        return key.to_string();
    }
    let kept: String = key.chars().take(MAX_ECHOED_KEY).collect();
    format!("{kept}...")
}

/// Why a tag the request carries is not one this service accepts.
///
/// Every one of these is a rule of the resource-tagging layer that fronts the pipes API,
/// which refuses a tag breaking one with `BadRequestException` (400) rather than with the
/// modelled `ValidationException` the service states its own refusals as — see
/// [`tag_refusal`] for the sentence each is worded with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagViolation {
    /// The key is outside `TagKey`'s `@length` bound ([`TAG_KEY_LENGTH`]).
    KeyLength,
    /// The value is outside `TagValue`'s `@length` bound ([`TAG_VALUE_LENGTH`]).
    ValueLength,
    /// The key uses the prefix AWS reserves for its own tags
    /// ([`RESERVED_TAG_KEY_PREFIX`]).
    ReservedKeyPrefix,
    /// The key or the value carries a character outside the documented tag set
    /// ([`TAG_CHARACTER_SET_MESSAGE`]).
    CharacterSet,
}

/// Which rule a tag breaks, or `None` when it breaks none.
///
/// `@length` counts CHARACTERS, not bytes (the framework's own constraint checker counts
/// `chars()`), so a unicode key or value is measured the same way AWS measures it.
///
/// The key is judged as a CALLER's: every public write that takes tags is a caller's
/// request. The one write that is not is the in-process create another emulated service
/// makes for a resource it provisions ([`tag_violation_from`]).
pub fn tag_violation(key: &str, value: &str) -> Option<TagViolation> {
    tag_violation_from(key, value, TagKeyOrigin::Caller)
}

/// Who is naming the tag keys one request carries, which is what decides whether a key in
/// the reserved `aws:` namespace may be written ([`tag_violation_from`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagKeyOrigin {
    /// A request a CLIENT made. A key in the reserved namespace is AWS's own, so such a
    /// request may not write one.
    Caller,
    /// A request another emulated service made in-process on behalf of a resource it
    /// provisions — the path the tags AWS sets from inside itself arrive on, which no
    /// client's request can be. A key in the reserved namespace travels on it.
    Service,
}

/// Where the tag keys this request names came from.
///
/// A tag in the reserved namespace is AWS's to set: the refusal
/// ([`RESERVED_TAG_KEY_PREFIX_MESSAGE`]) is the answer to an API CALLER, while the services
/// that own such a tag set it from inside AWS, through no public API — the
/// `aws:cloudformation:*` tags a stack's resources carry are exactly that. In this emulator
/// one service reaches another ONLY through the in-process callback
/// ([`::smithy_server_runtime::OperationContext::callback`]), so that callback is the inside
/// path, and the signal is the [`::smithy_server_runtime::CallbackRecursionDepth`] request
/// EXTENSION the framework's callback transport stamps on every in-process call. An
/// extension never travels over the wire: nothing a client can send — on any socket, signed
/// with any credential — sets it, so a client's reserved key is refused exactly as before.
pub fn tag_key_origin(ctx: &::smithy_server_runtime::OperationContext<'_>) -> TagKeyOrigin {
    match ctx
        .parts()
        .extensions
        .get::<::smithy_server_runtime::CallbackRecursionDepth>()
    {
        Some(_) => TagKeyOrigin::Service,
        None => TagKeyOrigin::Caller,
    }
}

/// [`tag_violation`] for a write whose tag keys may be AWS's own: the reserved-namespace
/// rule is the one rule that depends on WHO named the key ([`TagKeyOrigin`]), and every
/// other rule is the tag's own and holds whoever wrote it.
pub fn tag_violation_from(
    key: &str,
    value: &str,
    origin: TagKeyOrigin,
) -> Option<TagViolation> {
    if !TAG_KEY_LENGTH.contains(&key.chars().count()) {
        return Some(TagViolation::KeyLength);
    }
    if origin == TagKeyOrigin::Caller
        && key
            .get(..RESERVED_TAG_KEY_PREFIX.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_TAG_KEY_PREFIX))
    {
        return Some(TagViolation::ReservedKeyPrefix);
    }
    if !TAG_VALUE_LENGTH.contains(&value.chars().count()) {
        return Some(TagViolation::ValueLength);
    }
    if !within_tag_character_set(key) || !within_tag_character_set(value) {
        return Some(TagViolation::CharacterSet);
    }
    None
}

/// The member a refused tag is reported at by `TagResource`, whose request-validation
/// name for the tags it merges is not the wire member's. Source: the recorded refusal of
/// an over-long tag key, which names this member.
pub const ADDED_TAGS_MEMBER: &str = "addedTags";

/// The member a refused tag KEY is reported at by `UntagResource`, from the same source.
pub const REMOVED_TAG_KEYS_MEMBER: &str = "removedTagKeys";

/// The member a refused tag is reported at by `CreatePipe`. No recorded refusal names it,
/// so it is the model's own member in the casing a validation path uses.
pub const CREATE_TAGS_MEMBER: &str = "tags";

/// Both published bounds of a `@length`-constrained member, in the order and the wording
/// request validation states them in — the maximum first, each as one `Member must …`
/// clause. The bounds come from the model (the constants above), never from a request.
fn length_requirements(bound: &std::ops::RangeInclusive<usize>) -> String {
    format!(
        "Member must have length less than or equal to {}, \
         Member must have length greater than or equal to {}",
        bound.end(),
        bound.start(),
    )
}

/// One refused request as request validation states it: how many clauses, then the
/// clause. The writes below refuse one tag at a time, so one clause is stated.
///
/// The sentence travels as the JSON DOCUMENT this service's tagging layer words a
/// constraint-validation refusal as — the sentence under a `message` member — which is what
/// a client unescapes and parses out of the error's own message member. Recorded behaviour
/// for this error class.
fn one_validation_error(clause: String) -> String {
    validation_document(&format!("1 validation error detected: {clause}"))
}

/// `sentence` as the refusal DOCUMENT carries it: inside `{"message": "…"}`, rendered as a
/// JSON string so every character a JSON string escapes — the backslash, the quote and the
/// control characters a tag key may carry — is escaped rather than reaching the document raw,
/// and then with the apostrophes the sentence quotes a value and a member name with escaped
/// too. That last escape is recorded behaviour of this error class rather than JSON's own, so
/// a client unescapes it before parsing the document.
fn validation_document(sentence: &str) -> String {
    let quoted = ::serde_json::Value::String(sentence.to_string()).to_string();
    format!("{{\"message\": {}}}", quoted.replace('\'', "\\'"))
}

/// The tags a request carried, as a validation message echoes the member's value: every
/// entry in key order, `{k1=v1, k2=v2}`. What such a message echoes is the value of the
/// MEMBER, not only the entry that broke the rule.
///
/// Each KEY is elided to what a refusal may echo (a key arrives unbounded); each value is
/// interpolated as given, being already within `TagValue`'s own bound — or, when it is
/// what broke that bound, already rendered by the validator that refused it.
pub fn echoed_tags<'a>(tags: impl IntoIterator<Item = (&'a str, &'a str)>) -> String {
    let entries: Vec<String> = tags
        .into_iter()
        .map(|(key, value)| format!("{}={value}", echoed_key(key)))
        .collect();
    format!("{{{}}}", entries.join(", "))
}

/// The message a write that installs tags refuses `violation` with, for the `member` the
/// tags arrived on (the operations name it differently, so the caller passes its own).
/// `tags_shown` is the member's value as [`echoed_tags`] renders it.
///
/// A violated published BOUND is stated as request validation states one — the member,
/// its value, and the bounds that value had to satisfy, all from this request and the
/// model — and that sentence is the whole message ([`one_validation_error`]). The
/// character-set and reserved-prefix rules are not modelled constraints and no
/// validation sentence states them: each is the documented rule itself, stated bare.
pub fn tag_refusal(member: &str, violation: TagViolation, tags_shown: &str) -> String {
    match violation {
        TagViolation::KeyLength => one_validation_error(format!(
            "Value '{tags_shown}' at '{member}' failed to satisfy constraint: \
             Map keys must satisfy constraint: [{}]",
            length_requirements(&TAG_KEY_LENGTH),
        )),
        TagViolation::ValueLength => one_validation_error(format!(
            "Value '{tags_shown}' at '{member}' failed to satisfy constraint: \
             Map value must satisfy constraint: [{}]",
            length_requirements(&TAG_VALUE_LENGTH),
        )),
        TagViolation::ReservedKeyPrefix => RESERVED_TAG_KEY_PREFIX_MESSAGE.to_string(),
        TagViolation::CharacterSet => TAG_CHARACTER_SET_MESSAGE.to_string(),
    }
}

/// The message the tagging layer refuses a tag KEY named for REMOVAL with when it is
/// outside `TagKey`'s bound: the same sentence as [`tag_refusal`], with the key list as
/// the value request validation echoes for a list member.
///
/// `key` is interpolated as given: a key named for removal travels as a list ELEMENT,
/// which the boundary validator does walk, so it reaches this wording already bounded by
/// that validator's own rendering.
pub fn tag_key_refusal(member: &str, key: &str) -> String {
    one_validation_error(format!(
        "Value '[{key}]' at '{member}' failed to satisfy constraint: \
         Member must satisfy constraint: [{}]",
        length_requirements(&TAG_KEY_LENGTH),
    ))
}

/// One entry of a [`validation_error`]'s list: the request field that was refused and why.
pub fn validation_field(
    name: impl Into<String>,
    message: impl Into<String>,
) -> ValidationExceptionField {
    ValidationExceptionField {
        name: name.into(),
        message: message.into(),
    }
}

/// The reason clause [`invalid_parameter`] states for a parameter value this service cannot
/// interpret AT ALL — one it can neither read nor act on, as against a value it read and
/// found outside a modelled constraint, which the boundary validator reports against that
/// constraint.
///
/// Recorded behaviour for the one such parameter this service has, the opaque continuation
/// token: the reason is this fixed phrase for the error class rather than anything derived
/// from the value, which the service by definition could not read.
pub const MALFORMED_REQUEST_REASON: &str = "Malformed request";

/// The `ValidationException` a single uninterpretable request PARAMETER is refused with:
/// ONE sentence naming the parameter and the reason it was refused for, with the per-field
/// detail in the modelled `fieldList` where a client reads per-field detail.
///
/// Fixed text (`Parameter `, ` is not valid. Reason: ` and the closing stop) around two
/// variable parts: `member`, the name of the rejected parameter as the request spells it,
/// and `reason`, the fixed phrase for the class of refusal.
pub fn invalid_parameter(
    member: &str,
    reason: &str,
    detail: impl Into<String>,
) -> ValidationException {
    ValidationException {
        message: Some(format!("Parameter {member} is not valid. Reason: {reason}.")),
        field_list: Some(vec![validation_field(member, detail)]),
    }
}

/// The `ValidationException` this service refuses a request with: the fields that failed
/// validation reported BOTH as the modelled `fieldList` and, in the exception's `message`,
/// as the JSON document this service words a refusal as — a `ValidationErrorFields` array
/// whose entries carry each field's `Name` and `Message`. Both are built from the one list,
/// so a client that parses the document and one that reads `fieldList` are told the same
/// thing.
pub fn validation_error(fields: Vec<ValidationExceptionField>) -> ValidationException {
    // Written out rather than serialized from a map so the two keys keep the order this
    // service reports them in; the values go through `serde_json` so any character they
    // carry is escaped as JSON requires.
    let entries = fields
        .iter()
        .map(|field| {
            format!(
                r#"{{"Name":{},"Message":{}}}"#,
                ::serde_json::Value::String(field.name.clone()),
                ::serde_json::Value::String(field.message.clone()),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    ValidationException {
        message: Some(format!(r#"{{"ValidationErrorFields":[{entries}]}}"#)),
        field_list: Some(fields),
    }
}

/// The `ConflictException` a lifecycle call is refused with when a move the pipe is
/// already making has not landed yet.
///
/// ONE sentence, built from the request — the pipe the caller named interpolated into
/// fixed text — and the same sentence for every such refusal: which operation was called,
/// and which of its own states the pipe is passing through, are not the caller's business
/// and do not change what the caller must do (wait for the move to land and ask again).
/// So `StartPipe`, `StopPipe` and `UpdatePipe` all read alike here.
pub fn pipe_conflict(pipe: &str) -> ConflictException {
    ConflictException {
        message: format!("Pipe {pipe} is updating concurrently."),
        resource_id: pipe.to_string(),
        resource_type: PIPE_RESOURCE_TYPE.to_string(),
    }
}

/// The `NotFoundException` the NAME-addressed operations of this service answer with for
/// a pipe this account does not hold.
///
/// The message is built from the request — the pipe the caller named interpolated into ONE
/// fixed sentence — so an absent pipe reads the same whichever of those operations was
/// asked for it. The ARN-addressed tagging operations answer [`resource_not_found`]
/// instead.
pub fn pipe_not_found(pipe: &str) -> NotFoundException {
    NotFoundException {
        message: Some(format!("Pipe {pipe} does not exist.")),
    }
}

/// The sentence the ARN-addressed tagging operations report an unresolvable
/// `resourceArn` with.
pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The `NotFoundException` the ARN-addressed tagging operations
/// (`TagResource`/`UntagResource`/`ListTagsForResource`) answer with for a `resourceArn`
/// no stored pipe carries.
///
/// A CONSTANT, resource-agnostic sentence: those operations are served by the generic
/// resource-tagging layer in front of this service, which resolved no pipe and so has no
/// name to interpolate — and echoes no part of the ARN it was given back to the caller.
pub fn resource_not_found() -> NotFoundException {
    NotFoundException {
        message: Some(RESOURCE_NOT_FOUND_MESSAGE.to_string()),
    }
}

/// The `Description` to STORE for the one a request carried.
///
/// `PipeDescription` is `@length(min: 0)`, so the empty string is a value a request may
/// carry — but a pipe holds a description or holds none, and the empty string is how a
/// caller says "none": recorded behaviour is that `Description=""` is stored exactly as an
/// omitted `Description` is, so a later read reports the member for neither. The same
/// normalization the other "empty string clears it" members of this service get
/// (`Enrichment`, `KmsKeyIdentifier`), which is why it is stated once here rather than in
/// each write.
pub fn stored_description(requested: Option<String>) -> Option<String> {
    requested.filter(|text| !text.is_empty())
}

/// This service's own ARN namespace: `arn:<partition>:pipes:…`.
pub const PIPES_SERVICE: &str = "pipes";

/// The sentence the tagging layer refuses a `resourceArn` belonging to ANOTHER service
/// with, or `None` when the ARN names this service's own vendor.
///
/// The generic resource-tagging layer resolves the ARN's vendor before it looks for a
/// resource: an ARN of another service — and text that is not a resource ARN at all — can
/// never name a pipe, however this account's pipes change, so it is a BAD REQUEST rather
/// than a missing resource. An ARN of THIS vendor that resolves to no pipe (a resource that
/// is not a pipe, a pipe this account does not hold) is the 404 those operations model.
///
/// Which ARNs are of this vendor is `service_common::arn`'s reading, the one parse of an
/// ARN's fields in this tree, and the sentence is that layer's own
/// (`service_common::validation_message::expected_vendor`), which names the vendor it
/// expected and echoes no part of the ARN it was handed — as [`RESOURCE_NOT_FOUND_MESSAGE`]
/// echoes none either.
pub fn foreign_resource_arn(arn: &str) -> Option<String> {
    (!::service_common::arn::is_resource_arn_of(arn, PIPES_SERVICE))
        .then(|| ::service_common::validation_message::expected_vendor(PIPES_SERVICE))
}

/// The largest batch a pipe may read for a target, by the target's ARN service namespace.
///
/// A pipe's batch size is bounded by what the TARGET takes in ONE delivery, not by the
/// source: each entry is the maximum number of entries that target service's own batch API
/// accepts per call — Amazon SQS `SendMessageBatch` and EventBridge `PutEvents` 10 entries,
/// Kinesis Data Streams `PutRecords` and Amazon Data Firehose `PutRecordBatch` 500 records,
/// CloudWatch Logs `PutLogEvents` 10,000 events.
///
/// A target whose delivery is ONE call per batch (Lambda, Step Functions, ECS, Batch,
/// SageMaker, an API destination, Timestream, Redshift Data) is deliberately absent: it has
/// no per-call entry limit to cap the batch at, so only the source's own modelled `@range`
/// bounds it.
const TARGET_BATCH_SIZE_CAPS: &[(&str, i32)] = &[
    ("events", 10),
    ("firehose", 500),
    ("kinesis", 500),
    ("logs", 10_000),
    (PIPES_SERVICE_SQS, 10),
];

/// The ARN service namespace of Amazon SQS, named once: it is both a batch cap above and
/// the service whose FIFO queues need a message group id.
const PIPES_SERVICE_SQS: &str = "sqs";

/// The suffix the name of an Amazon SQS FIFO queue carries — a queue whose name does not
/// end in it is a standard queue. Source: Amazon SQS requires the name of a FIFO queue to
/// end with `.fifo`.
const FIFO_QUEUE_SUFFIX: &str = ".fifo";

/// The vendor the resource `target` names and the largest batch it accepts per delivery, or
/// `None` when that target has no per-call entry limit ([`TARGET_BATCH_SIZE_CAPS`]) and when
/// `target` is not a resource ARN at all.
///
/// The vendor travels WITH the cap because the refusal states both, and the one that proved
/// the cap is the one the sentence has to name.
pub fn target_batch_size_cap(target: &str) -> Option<(&str, i32)> {
    let vendor = ::service_common::arn::vendor(target)?;
    TARGET_BATCH_SIZE_CAPS
        .iter()
        .find(|(namespace, _)| *namespace == vendor)
        .map(|(_, cap)| (vendor, *cap))
}

/// The `BatchSize` a pipe polls its source with when that source's parameters name none,
/// as a read of the pipe reports it and as the poller in [`data_plane`] reads the source
/// with.
///
/// Not one of the `BatchSize` members declares a `@default` (each declares only its
/// `@range`), so the value in force where a configuration names none is the service's own,
/// and it is ONE value whatever the source is: a queue source and a stream source both poll
/// ten records per batch. A pipe that wants another number says so.
pub const DEFAULT_BATCH_SIZE: i32 = 10;

/// The batch size a pipe's source parameters configure, whichever source type they are
/// for: every `PipeSource*Parameters` shape that can batch carries the same `BatchSize`
/// member, and a pipe reads from exactly one source.
pub fn configured_batch_size(parameters: &PipeSourceParameters) -> Option<i32> {
    let from_each = [
        parameters.sqs_queue_parameters.as_ref().and_then(|p| p.batch_size),
        parameters.kinesis_stream_parameters.as_ref().and_then(|p| p.batch_size),
        parameters.dynamo_db_stream_parameters.as_ref().and_then(|p| p.batch_size),
        parameters.active_mq_broker_parameters.as_ref().and_then(|p| p.batch_size),
        parameters.rabbit_mq_broker_parameters.as_ref().and_then(|p| p.batch_size),
        parameters
            .managed_streaming_kafka_parameters
            .as_ref()
            .and_then(|p| p.batch_size),
        parameters
            .self_managed_kafka_parameters
            .as_ref()
            .and_then(|p| p.batch_size),
    ];
    from_each.into_iter().flatten().next()
}

/// The field a `FilterCriteria` this service cannot accept is refused as, or `None` when
/// every pattern it carries is one.
///
/// A pipe's filter patterns are EventBridge EVENT PATTERNS: each is a JSON object whose
/// members are clauses matched against the event, so text that is not a JSON object is not
/// a pattern at all, and neither is an object whose clauses are not clauses
/// ([`event_pattern_reason`]). The model types `Filter.Pattern` as a plain string, which no
/// boundary constraint can express, so the write that stores one compiles it. An absent or
/// empty pattern carries no event pattern and is left alone — `FilterCriteria` with an
/// empty `Filters` array is how a caller REMOVES filtering.
fn filter_criteria_field(criteria: &FilterCriteria) -> Option<ValidationExceptionField> {
    let patterns = criteria.filters.iter().flat_map(|filters| filters.iter());
    for filter in patterns {
        let pattern = match filter.pattern.as_deref() {
            None | Some("") => continue,
            Some(pattern) => pattern,
        };
        if let Some(reason) = event_pattern_reason(pattern) {
            return Some(validation_field(
                PATTERN_FIELD,
                format!("Invalid Event Pattern. Reason: {reason}."),
            ));
        }
    }
    None
}

/// Why `pattern` is not an event pattern, as the parser that read it reports it — or
/// `None` when it is one.
///
/// The reason is the PARSER's own diagnostic, read through
/// [`::service_common::json_parse_fault`] — the one place this tree states where a
/// submitted JSON document stopped parsing and what was expected there — so two services
/// handed the same text cannot disagree about it. Three readings are this service's own:
///
/// * a text that PARSES but is not an object is not a filter, which is the one fault with
///   no parse failure to report ([`NOT_AN_OBJECT_REASON`]);
/// * a text that parses to an object is read on: an object whose CLAUSES are not clauses
///   is not a pattern either, which the one reading of the pattern language in this tree
///   answers ([`::service_common::event_pattern::clause_form_fault`]);
/// * the parser this service hands a pattern to reads the filter's MEMBERS, so its first
///   expectation is a member name. Where the text does not open the filter object, the
///   parse never begins and stops on its very first character — so that parse is the one
///   RUN (the text read as the object's body, the opening brace the pattern never had
///   prepended), rather than a guess about which fault the text carries: the character and
///   the expectation still come from the parse itself.
fn event_pattern_reason(pattern: &str) -> Option<String> {
    let error = match ::serde_json::from_str::<::serde_json::Value>(pattern) {
        Ok(filter @ ::serde_json::Value::Object(_)) => {
            return ::service_common::event_pattern::clause_form_fault(&filter);
        }
        Ok(_) => return Some(NOT_AN_OBJECT_REASON.to_string()),
        Err(error) => error,
    };
    if !pattern.trim_start().starts_with(FILTER_OBJECT_OPENING) {
        let body = format!("{FILTER_OBJECT_OPENING}{pattern}");
        if let Err(body_error) = ::serde_json::from_str::<::serde_json::Value>(&body) {
            return Some(::service_common::json_parse_fault::unexpected_input(
                &body,
                &body_error,
            ));
        }
    }
    Some(::service_common::json_parse_fault::unexpected_input(
        pattern, &error,
    ))
}

/// The character a filter object opens with, which an event pattern has to.
const FILTER_OBJECT_OPENING: char = '{';

/// The member a refused event pattern is reported at: the one that carried it.
const PATTERN_FIELD: &str = "Pattern";

/// What this service reports for a pattern that parses but is not an object — the one
/// fault with no parse failure behind it, so no parser states it.
const NOT_AN_OBJECT_REASON: &str = "Filter is not an object";

/// The field a batch size the target cannot take is reported at: the member that carried
/// the value the target cannot take.
const BATCH_SIZE_FIELD: &str = "BatchSize";

/// The request members that carry a pipe's source and target configuration, which a member
/// nested inside one is reported under (`SourceParameters.<Member>`,
/// `TargetParameters.<Member>`).
const SOURCE_PARAMETERS_MEMBER: &str = "SourceParameters";
const TARGET_PARAMETERS_MEMBER: &str = "TargetParameters";

/// The `PipeTargetParameters` member that configures an Amazon SQS queue target, as the
/// model names it — the member a requirement of such a target is reported at.
const SQS_QUEUE_PARAMETERS_MEMBER: &str = "SqsQueueParameters";

/// The member a template that spells no JSON is reported at: the one that carried it, as
/// both `PipeTargetParameters` and `PipeEnrichmentParameters` name it — which is why the
/// refusal reads alike whichever of the two carried the template.
const INPUT_TEMPLATE_MEMBER: &str = "InputTemplate";

/// What this service reports for an `InputTemplate` that is JSON text and spells no JSON
/// document: ONE fixed sentence for the class, the offending member's name being the only
/// part of the refusal that varies ([`input_template_violation`]).
const MALFORMED_INPUT_TEMPLATE_MESSAGE: &str = "Invalid JSON format for Input Template.";

/// What this service reports for a member the request had to carry and did not, and what it
/// reports for a TARGET parameter the target resource requires that the configuration does
/// not supply.
///
/// TWO sentences, deliberately: the service words the target side as its own refusal, naming
/// the target, so neither constant is the other spelled differently and unifying them would
/// change what a client reads.
const MISSING_PARAMETER_MESSAGE: &str = "Missing required parameter.";
const MISSING_TARGET_PARAMETER_MESSAGE: &str = "Missing required target parameter.";

/// One `SourceParameters` member that belongs to a source TYPE.
pub struct SourceTypeMember {
    /// The member, as the model names it.
    pub member: &'static str,
    /// The source type it belongs to, as [`source_type_of`] reports one.
    pub source_type: &'static str,
    /// Whether a source of that type has a legal configuration that OMITS this member.
    /// `false` where the member's own shape declares a `@required` member, so no
    /// configuration of such a source can leave the member out.
    pub omittable: bool,
    /// Whether a source configuration carries this member. The ONE place the member's
    /// name and the field that holds it are paired, so a rule about the member reads it
    /// off the table rather than naming the field again.
    pub carried: fn(&PipeSourceParameters) -> bool,
    /// Whether a source of this type is read PER SHARD, which is the scope the model gives
    /// `ParallelizationFactor` — "the number of batches to process concurrently from each
    /// shard" — and which only the two stream parameter shapes carry. A pipe over such a
    /// source reports what its poller made of its last pass as the reason it is running
    /// ([`PipeRecord::reported_state_reason`]).
    pub per_shard: bool,
    /// Report the `BatchSize` a source of this type POLLS WITH onto this member of a
    /// configuration a read is about to report, so a reader sees the value in force rather
    /// than nothing where the request named none ([`PipeRecord::reported_source_parameters`]).
    ///
    /// A no-op where the configuration already names one, and where this emulator polls no
    /// source of that type: there is no value in force to report then, so the stored
    /// configuration is reported exactly as it was written. Every type that IS polled
    /// reports the default its poller uses ([`DEFAULT_BATCH_SIZE`]), and only the one member
    /// whose shape can be synthesized (`omittable`) is added to a configuration that lacks
    /// it — the others declare a `@required` member this has no value for.
    pub report_effective_batch_size: fn(&mut PipeSourceParameters),
}

/// Every `SourceParameters` member that belongs to a source type.
///
/// Source: the members of `PipeSourceParameters` in
/// `smithy-model/com-amazonaws-pipes-pipes.json` (`UpdatePipeSourceParameters` names them
/// identically) — each is the parameters of exactly ONE source type — paired with the
/// service namespace of that type's resource ARN and with whether that type can be
/// configured without the member. `FilterCriteria`, the one member that belongs to no
/// source type, is deliberately absent: it applies to a pipe whatever its source is.
///
/// `omittable` is read off the member's own shape in that model:
/// `PipeSourceSqsQueueParameters` declares no `@required` member, so an Amazon SQS source
/// needs no parameters at all; the two stream shapes declare `StartingPosition`, the two
/// Kafka shapes `TopicName` and the two broker shapes `QueueName` and `Credentials`, so a
/// source of one of those types has no legal configuration with the member absent.
///
/// One source type — an Amazon MQ broker, which is `arn:…:mq:…` whichever engine it runs
/// — has TWO members that configure it, and the `Source` does not say which engine the
/// broker runs. A configuration carrying neither is still illegal, so the refusal names
/// both candidates rather than guessing one ([`missing_source_parameters_fields`]).
pub const SOURCE_TYPE_MEMBERS: &[SourceTypeMember] = &[
    SourceTypeMember {
        member: "KinesisStreamParameters",
        source_type: "kinesis",
        omittable: false,
        per_shard: true,
        carried: |parameters| parameters.kinesis_stream_parameters.is_some(),
        report_effective_batch_size: |parameters| {
            if let Some(stream) = parameters.kinesis_stream_parameters.as_mut() {
                stream.batch_size.get_or_insert(DEFAULT_BATCH_SIZE);
            }
        },
    },
    SourceTypeMember {
        member: "DynamoDBStreamParameters",
        source_type: "dynamodb",
        omittable: false,
        per_shard: true,
        carried: |parameters| parameters.dynamo_db_stream_parameters.is_some(),
        report_effective_batch_size: |parameters| {
            if let Some(stream) = parameters.dynamo_db_stream_parameters.as_mut() {
                stream.batch_size.get_or_insert(DEFAULT_BATCH_SIZE);
            }
        },
    },
    SourceTypeMember {
        member: "SqsQueueParameters",
        source_type: PIPES_SERVICE_SQS,
        omittable: true,
        per_shard: false,
        carried: |parameters| parameters.sqs_queue_parameters.is_some(),
        report_effective_batch_size: |parameters| {
            parameters
                .sqs_queue_parameters
                .get_or_insert_with(Default::default)
                .batch_size
                .get_or_insert(DEFAULT_BATCH_SIZE);
        },
    },
    SourceTypeMember {
        member: "ActiveMQBrokerParameters",
        source_type: "mq",
        omittable: false,
        per_shard: false,
        carried: |parameters| parameters.active_mq_broker_parameters.is_some(),
        report_effective_batch_size: |_| {},
    },
    SourceTypeMember {
        member: "RabbitMQBrokerParameters",
        source_type: "mq",
        omittable: false,
        per_shard: false,
        carried: |parameters| parameters.rabbit_mq_broker_parameters.is_some(),
        report_effective_batch_size: |_| {},
    },
    SourceTypeMember {
        member: "ManagedStreamingKafkaParameters",
        source_type: "kafka",
        omittable: false,
        per_shard: false,
        carried: |parameters| parameters.managed_streaming_kafka_parameters.is_some(),
        report_effective_batch_size: |_| {},
    },
    SourceTypeMember {
        member: "SelfManagedKafkaParameters",
        source_type: SELF_MANAGED_KAFKA,
        omittable: false,
        per_shard: false,
        carried: |parameters| parameters.self_managed_kafka_parameters.is_some(),
        report_effective_batch_size: |_| {},
    },
];

/// The source type `member` belongs to, per [`SOURCE_TYPE_MEMBERS`], or the empty string
/// for a member that belongs to none.
pub fn member_source_type(member: &str) -> &'static str {
    SOURCE_TYPE_MEMBERS
        .iter()
        .find(|entry| entry.member == member)
        .map_or("", |entry| entry.source_type)
}

/// The source type a `Source` names, as the token this service names a source by: the
/// vendor of its ARN as `service_common::arn` reads one — the one parse of an ARN's fields
/// in this tree — or [`SELF_MANAGED_KAFKA`] for the one non-ARN form `ArnOrUrl` admits.
///
/// Read off the `Source` rather than off a record, because both the write that creates a
/// pipe (which has no record yet) and the reads of a stored one resolve the same token.
pub fn source_type_of(source: &str) -> String {
    if source
        .get(..SELF_MANAGED_KAFKA_SCHEME.len())
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case(SELF_MANAGED_KAFKA_SCHEME))
    {
        return SELF_MANAGED_KAFKA.to_string();
    }
    ::service_common::arn::vendor(source)
        .unwrap_or_default()
        .to_string()
}

/// The fields a configuration whose `Source` names a kind of resource that cannot be
/// configured without its own `SourceParameters` member is refused at — empty when the
/// configuration carries one of them, or when the source's type needs none.
///
/// This is the requirement the SERVICE derives from the resource the `Source` ARN names:
/// the model makes every member of `PipeSourceParameters` optional, so no boundary
/// constraint can express it, but the parameters shape of a source type may itself declare
/// a `@required` member ([`SOURCE_TYPE_MEMBERS`]), and then no legal configuration of such
/// a source leaves that member out.
///
/// A source type configured by SEVERAL such members — an Amazon MQ broker, whose ARN does
/// not say which engine it runs — needs one of them, so carrying any one satisfies the
/// rule and carrying none is refused naming every candidate: which member the request had
/// to carry is the caller's to choose, and the refusal states the choice rather than
/// inventing it.
fn missing_source_parameters_fields(
    source: &str,
    source_parameters: Option<&PipeSourceParameters>,
) -> Vec<ValidationExceptionField> {
    let source_type = source_type_of(source);
    let required: Vec<&SourceTypeMember> = SOURCE_TYPE_MEMBERS
        .iter()
        .filter(|entry| !entry.omittable && entry.source_type == source_type)
        .collect();
    let carried = required.iter().any(|entry| {
        source_parameters.is_some_and(|parameters| (entry.carried)(parameters))
    });
    if carried {
        return Vec::new();
    }
    required
        .into_iter()
        .map(|entry| {
            validation_field(
                format!("{SOURCE_PARAMETERS_MEMBER}.{}", entry.member),
                MISSING_PARAMETER_MESSAGE,
            )
        })
        .collect()
}

/// The `ValidationException` a pipe configuration is refused with when it breaks a rule
/// about the RESOURCES the pipe names rather than about the shape of the request, or `None`
/// when it breaks none.
///
/// These are the rules no modelled constraint can express, because each spans two members:
/// the `SourceParameters` member a source needs is the one for the kind of resource its
/// `Source` names ([`missing_source_parameters_fields`]), the batch size the source reads is
/// bounded by what the TARGET accepts per delivery ([`target_batch_size_cap`]), a FIFO
/// queue target needs the message group id its sends are ordered by, and a filter pattern
/// has to be an event pattern ([`filter_criteria_field`]). Checked by both writes that can
/// configure a pipe, against the configuration the pipe would END UP with, so neither a
/// create nor an update can leave a pipe AWS would never have accepted.
pub fn cross_resource_violation(
    source: &str,
    source_parameters: Option<&PipeSourceParameters>,
    target: &str,
    target_parameters: Option<&PipeTargetParameters>,
) -> Option<ValidationException> {
    // First, because a source configuration that is not there at all can carry no batch
    // size and no filter either.
    let missing = missing_source_parameters_fields(source, source_parameters);
    if !missing.is_empty() {
        return Some(validation_error(missing));
    }
    let field = cross_resource_field(source_parameters, target, target_parameters)?;
    Some(validation_error(vec![field]))
}

/// The `ValidationException` a pipe configuration is refused with when an `InputTemplate`
/// it carries does not spell the valid JSON text that member is, or `None` when every
/// template it carries does.
///
/// A template is "Valid JSON text passed to the target" (and the same sentence for the
/// enrichment's own), with its placeholders standing where a JSON value or a string's
/// characters stand, so whether it spells that JSON is decided from the template's own text
/// ([`data_plane::spells_malformed_json`]) and decided at AUTHORING time: a write carrying
/// one that spells no JSON stores nothing, rather than leaving a broken template to be
/// discovered by a delivery. Asked of the configuration the pipe would END UP with by both
/// writes that can configure one, and of BOTH members that carry a template, which are
/// reported alike — the member's own name is the only part of this refusal that varies.
pub fn input_template_violation(
    enrichment_parameters: Option<&PipeEnrichmentParameters>,
    target_parameters: Option<&PipeTargetParameters>,
) -> Option<ValidationException> {
    let templates = [
        enrichment_parameters.and_then(|parameters| parameters.input_template.as_deref()),
        target_parameters.and_then(|parameters| parameters.input_template.as_deref()),
    ];
    templates
        .into_iter()
        .flatten()
        .any(data_plane::spells_malformed_json)
        .then(|| {
            validation_error(vec![validation_field(
                INPUT_TEMPLATE_MEMBER,
                MALFORMED_INPUT_TEMPLATE_MESSAGE,
            )])
        })
}

/// The one field [`cross_resource_violation`] reports for the rules that name exactly one,
/// in the order they are stated.
fn cross_resource_field(
    source_parameters: Option<&PipeSourceParameters>,
    target: &str,
    target_parameters: Option<&PipeTargetParameters>,
) -> Option<ValidationExceptionField> {
    if let Some(criteria) = source_parameters.and_then(|p| p.filter_criteria.as_ref()) {
        if let Some(field) = filter_criteria_field(criteria) {
            return Some(field);
        }
    }

    if let (Some(batch_size), Some((target_type, cap))) = (
        source_parameters.and_then(configured_batch_size),
        target_batch_size_cap(target),
    ) {
        if batch_size > cap {
            return Some(validation_field(
                BATCH_SIZE_FIELD,
                format!(
                    "The configured batch size {batch_size} is greater than the max \
                     supported batch size {cap} for target {target_type}."
                ),
            ));
        }
    }

    // A FIFO queue orders its messages by message group, and `SendMessage` to one requires
    // the group to send in — which for a pipe is the target parameter, whether it carries
    // the group literally or a path to read it from each event. A FIFO queue is one whose
    // NAME carries the suffix, which is the resource field of its ARN
    // (`service_common::arn` reads both fields).
    //
    // A target parameter the target RESOURCE requires and the configuration does not supply
    // is reported at the `TargetParameters` member that configures that kind of target —
    // the parameters the pipe needed and has not got, not the leaf inside them — with the
    // one sentence that states a target parameter is missing
    // ([`MISSING_TARGET_PARAMETER_MESSAGE`]), because what the caller has to supply is that
    // member's configuration whether the member is absent altogether or merely incomplete.
    let fifo_target = ::service_common::arn::parse_of(target, PIPES_SERVICE_SQS)
        .is_some_and(|queue| queue.resource.ends_with(FIFO_QUEUE_SUFFIX));
    let message_group_id = target_parameters
        .and_then(|p| p.sqs_queue_parameters.as_ref())
        .and_then(|p| p.message_group_id.as_deref())
        .filter(|id| !id.is_empty());
    if fifo_target && message_group_id.is_none() {
        return Some(validation_field(
            format!("{TARGET_PARAMETERS_MEMBER}.{SQS_QUEUE_PARAMETERS_MEMBER}"),
            MISSING_TARGET_PARAMETER_MESSAGE,
        ));
    }

    None
}

/// The region an ARN carries when the request names none.
const DEFAULT_REGION: &str = "us-east-1";

/// How long after a lifecycle call a pipe settles out of its transient state
/// (`CREATING`, `STARTING`, `STOPPING`, `UPDATING`) into the state the caller asked for.
///
/// AWS does not publish this duration, so it is CHOSEN, not cited: long enough that a
/// caller acting straight after the call still observes the transient state, short
/// enough to be waited out. It is never zero and nothing ever sleeps for it — the due
/// instant is stored and the move is materialised by whichever later call reads the
/// pipe (see [`ComAmazonawsPipesPipesState::settle_due_transitions`]).
pub const STATE_SETTLE_DELAY: Duration = Duration::from_millis(500);

/// The `StateReason` of a pipe whose current state is one its CALLER drove it into — the
/// state a lifecycle call (a creation with a `DesiredState`, a start, a stop, an update)
/// asked the pipe to be in, as against one this service moved it to on its own.
///
/// Recorded behaviour: a pipe resting in the state its caller asked for reports this
/// FIXED token. It says HOW the pipe reached its state, so it is neither read from the
/// request nor minted per call. A pipe that has a READER of its source reports that reader's
/// reading instead, which is what such a pipe says about itself
/// ([`PipeRecord::reported_state_reason`]).
pub const USER_INITIATED_STATE_REASON: &str = "USER_INITIATED";

/// The `StateReason` a pipe whose source reader has processed no record of that source
/// reports: that reader's reading of what it has processed, which is nothing.
///
/// Recorded behaviour, as a FIXED sentence with no variable part: it is what a pipe over a
/// source read PER SHARD reports from its creation — such a pipe is created with a reader of
/// that source — until a pass of its poller has processed records. A pipe created with no
/// such reader reports the lifecycle reason throughout
/// ([`PipeRecord::reported_state_reason`]).
pub const NO_RECORDS_PROCESSED_STATE_REASON: &str = "No records processed";

/// The same reader's reading once a pass HAS processed the records it polled: the result of
/// that attempt.
///
/// NOT recorded for this service: what IS recorded is the reading above, the one a reader
/// that has processed nothing reports. This is the word this emulator's other reader of the
/// same kind uses for a pass that delivered its batch (the event-source-mapping surface of
/// `services/service-com-amazonaws-lambda-awsgirapiservice`), and it is what the reading
/// moves to when a pass of this pipe's poller has delivered its batch — so what a read of the
/// pipe says follows what its reader did rather than standing at one sentence for ever.
pub const RECORDS_PROCESSED_STATE_REASON: &str = "OK";

/// A state move the clock owes a pipe: at `due_at` its `CurrentState` becomes `to`, and
/// its `StateReason` becomes `reason` — why the pipe is in the state the move left it in.
#[derive(Debug, Clone)]
pub struct PendingTransition {
    /// The monotonic instant the move became (or becomes) due.
    pub due_at: Instant,
    /// The `PipeState` the pipe settles into.
    pub to: String,
    /// The `StateReason` the pipe carries once it has settled, if the move has one.
    pub reason: Option<String>,
}

impl PendingTransition {
    /// Whether this move lands in a state a FAILED move settles in
    /// ([`FAILED_PIPE_STATES`]): the lifecycle call that owes it was accepted but could not
    /// be completed, so nothing it would have brought up is there.
    pub fn lands_in_failure(&self) -> bool {
        FAILED_PIPE_STATES.contains(&self.to.as_str())
    }
}

/// The reading a pipe over `source` is created with, or `None` for a pipe that is created
/// with no reader of its source.
///
/// A pipe whose source is read PER SHARD is created WITH a reader of that source — which is
/// why the position such a source is read from is fixed at creation and no update shape can
/// move it — and that reader has processed no record yet, which is what the pipe reports
/// until a pass of its poller moves the reading on
/// ([`PipeRecord::reported_state_reason`]).
pub fn initial_processing_result(source: &str) -> Option<String> {
    source_is_read_per_shard(source).then(|| NO_RECORDS_PROCESSED_STATE_REASON.to_string())
}

/// Whether a source of the type `source` names is read PER SHARD, per the one table that
/// pairs a source type with its parameters ([`SourceTypeMember::per_shard`]).
pub fn source_is_read_per_shard(source: &str) -> bool {
    let source_type = source_type_of(source);
    SOURCE_TYPE_MEMBERS
        .iter()
        .any(|entry| entry.source_type == source_type && entry.per_shard)
}

/// The move owed to the clock when a lifecycle call starts a transition into `to`.
///
/// The move is one the CALLER asked for, so the state it lands in is reported as
/// [`USER_INITIATED_STATE_REASON`] — the same reason the transient state on the way there
/// already carries ([`PipeRecord::note_user_initiated`]).
pub fn pending_transition(to: &str) -> PendingTransition {
    PendingTransition {
        due_at: Instant::now() + STATE_SETTLE_DELAY,
        to: to.to_string(),
        reason: Some(USER_INITIATED_STATE_REASON.to_string()),
    }
}

/// The move owed to the clock when a lifecycle call cannot be completed: the pipe settles
/// in the terminal failure state `to` of the move that was in flight, carrying `reason` as
/// its `StateReason`.
///
/// The failure is settled rather than answered, because the call itself was accepted — the
/// state pair (`DesiredState` the caller asked for, `CurrentState` the pipe reached) plus
/// `StateReason` is where the pipe reports it.
pub fn failed_transition(to: &str, reason: String) -> PendingTransition {
    PendingTransition {
        due_at: Instant::now() + STATE_SETTLE_DELAY,
        to: to.to_string(),
        reason: Some(reason),
    }
}

/// One persisted pipe. Every member `CreatePipe` accepts is stored here and read back
/// by `DescribePipe`, so the record — not the request — is what every response is
/// computed from.
#[derive(Debug, Clone)]
pub struct PipeRecord {
    pub arn: String,
    pub name: String,
    pub description: Option<String>,
    pub desired_state: String,
    pub current_state: String,
    pub state_reason: Option<String>,
    pub source: String,
    pub source_parameters: Option<PipeSourceParameters>,
    pub enrichment: Option<String>,
    pub enrichment_parameters: Option<PipeEnrichmentParameters>,
    pub target: String,
    pub target_parameters: Option<PipeTargetParameters>,
    pub role_arn: String,
    /// Tags of this pipe, keyed by tag key. They live on the record (reachable from the
    /// pipe ARN) so the later `TagResource`/`UntagResource`/`ListTagsForResource` slice
    /// reads the same copy.
    pub tags: BTreeMap<String, String>,
    pub creation_time: SystemTime,
    pub last_modified_time: SystemTime,
    pub log_configuration: Option<PipeLogConfiguration>,
    pub kms_key_identifier: Option<String>,
    /// The move the clock owes this pipe, if any. See [`STATE_SETTLE_DELAY`].
    pub pending: Option<PendingTransition>,
    /// What the READER of this pipe's source has made of it, for a pipe that has one: the
    /// reading a read of the pipe reports as the reason it is in the state it is in
    /// ([`Self::reported_state_reason`]). A pipe over a source read per shard is created with
    /// a reader that has processed nothing ([`initial_processing_result`]); the passes its
    /// poller makes move the reading on, and a move that could not be completed leaves it
    /// with no reader at all. Nothing a request carries reaches it.
    pub processing_result: Option<String>,
}

impl PipeRecord {
    /// Record that the state this pipe is in now is one its caller asked for: its
    /// `StateReason` becomes [`USER_INITIATED_STATE_REASON`].
    ///
    /// Every lifecycle write a REQUEST drives goes through here, so a pipe reports why it
    /// is where it is from the moment the call lands — whether it is still mid-move or
    /// already resting — and no earlier failure's reason outlives the state it described.
    pub fn note_user_initiated(&mut self) {
        self.state_reason = Some(USER_INITIATED_STATE_REASON.to_string());
    }

    /// Record what the READER of this pipe's source made of a pass over it, so a read of the
    /// pipe reports that reading rather than the lifecycle reason that put the pipe in the
    /// state it is in ([`Self::reported_state_reason`]).
    ///
    /// Only a pipe that HAS such a reader has a reading to move on: a pipe whose source is
    /// not read per shard reports the reason its own last move carries whatever its poller
    /// did with that source.
    pub fn note_processing_result(&mut self, result: &str) {
        if let Some(reading) = self.processing_result.as_mut() {
            *reading = result.to_string();
        }
    }

    /// Forget that reading: the pipe has no reader of its source any more, so it reports the
    /// reason its own last move carries. What tears a reader down is a move that could not be
    /// completed ([`Self::settle`]), which brought nothing up in the first place.
    fn forget_processing_result(&mut self) {
        self.processing_result = None;
    }

    /// Whether this pipe is mid-move towards `RUNNING`: in a transient state its caller asked
    /// to end there. The one reading of that question, so the pass the poller decides and
    /// what a read of the pipe reports cannot drift apart.
    pub fn moves_towards_running(&self) -> bool {
        matches!(
            self.current_state.as_str(),
            pipe_state::CREATING | pipe_state::STARTING | pipe_state::UPDATING
        ) && self.desired_state == requested_state::RUNNING
    }

    /// The reason this pipe reports for being in the state it is in NOW.
    ///
    /// A pipe that HAS a reader of its source reports what that reader has made of it
    /// ([`Self::processing_result`]): a pipe over a source read PER SHARD is created with
    /// one — which is why the position such a source is read from is fixed at creation and no
    /// update can move it — so from then on what the pipe says about itself is what has been
    /// processed, and the passes its poller makes are what move that reading on. Every other
    /// pipe reports the reason its own last move carries: one over a source that is not read
    /// per shard, and one whose move could not be completed, which brought no reader up at
    /// all.
    ///
    /// The reading is NOT conditioned on the pipe polling at this instant: recorded behaviour
    /// reports it for a pipe that is not reading its source then — a pipe its caller asked
    /// for a run state it is resting in, with nothing having come off the stream — and the
    /// member states the result of the reader's LAST attempt, which a later move does not
    /// undo.
    ///
    /// The ONE reading of the member, so `DescribePipe` and the summary `ListPipes` returns
    /// cannot report one pipe's reason two ways.
    pub fn reported_state_reason(&self) -> Option<String> {
        self.processing_result
            .clone()
            .or_else(|| self.state_reason.clone())
    }

    /// Put the pipe into `transient` now and owe it the move to `settled`.
    ///
    /// A move that brings the pipe UP stands its reader of its source back up where an
    /// earlier failed move tore it down; a pipe that already has one keeps the reading it
    /// has, because what that reader processed is not undone by a later move.
    pub fn begin_transition(&mut self, transient: &str, settled: &str) {
        self.current_state = transient.to_string();
        self.note_user_initiated();
        if settled == pipe_state::RUNNING && self.processing_result.is_none() {
            self.processing_result = initial_processing_result(&self.source);
        }
        self.pending = Some(pending_transition(settled));
    }

    /// Put the pipe into `transient` now and owe it the FAILURE of that move: at the same
    /// span [`begin_transition`](Self::begin_transition) uses it settles in `failed`,
    /// carrying `reason`.
    pub fn begin_failed_transition(&mut self, transient: &str, failed: &str, reason: String) {
        self.current_state = transient.to_string();
        // The transient state is still one the caller asked for; the failure's own reason
        // replaces this one when the move it describes lands.
        self.note_user_initiated();
        self.pending = Some(failed_transition(failed, reason));
    }

    /// Materialise the move the clock has ALREADY made due, if it has. A no-op while the
    /// move is still in the future, so a caller acting immediately still sees the
    /// transient state.
    ///
    /// The `StateReason` is the settled move's own: a move that failed installs the
    /// sentence saying why, and one the caller drove installs
    /// [`USER_INITIATED_STATE_REASON`], because the reason describes how the pipe came to
    /// be in the state it is in now and not how it came to be in an earlier one. A move that
    /// failed brought nothing up, so the pipe is left with no reader of its source either and
    /// the sentence saying what stopped it is what it reports
    /// ([`Self::reported_state_reason`]).
    pub fn settle(&mut self, now: Instant) {
        let settled = match &self.pending {
            Some(pending) if now >= pending.due_at => pending.clone(),
            _ => return,
        };
        if settled.lands_in_failure() {
            self.forget_processing_result();
        }
        self.current_state = settled.to;
        self.state_reason = settled.reason;
        self.pending = None;
    }

    /// The transient state this pipe is in, or `None` when it is resting in a settled
    /// one. Call [`ComAmazonawsPipesPipesState::settle_due_transitions`] first, so a move
    /// the clock has already made due is not mistaken for one still in flight.
    pub fn transient_state(&self) -> Option<&str> {
        TRANSIENT_PIPE_STATES
            .iter()
            .find(|state| **state == self.current_state)
            .copied()
    }

    /// Whether this pipe RESTS in `RUNNING`, which is the one state its data plane reads
    /// its source in ([`data_plane`]). Call
    /// [`ComAmazonawsPipesPipesState::settle_due_transitions`] first, so a pipe the clock
    /// has already moved into `RUNNING` is seen as the running pipe it became.
    ///
    /// The one place that question is answered, so the pass the poller decides and the
    /// gate each of its source reads is taken under cannot drift apart.
    pub fn rests_in_running(&self) -> bool {
        self.current_state == pipe_state::RUNNING
    }

    /// The source type this pipe reads from, as the token this service names a source by.
    ///
    /// It is resolved from the pipe's `Source`, which is fixed when the pipe is created
    /// (no update shape carries a `Source`), through the one reading of that token
    /// ([`source_type_of`]).
    pub fn source_type(&self) -> String {
        source_type_of(&self.source)
    }

    /// This record as the `Pipe` summary `ListPipes` returns.
    pub fn summary(&self) -> Pipe {
        Pipe {
            name: Some(self.name.clone()),
            arn: Some(self.arn.clone()),
            desired_state: Some(self.desired_state.clone()),
            current_state: Some(self.current_state.clone()),
            state_reason: self.reported_state_reason(),
            creation_time: Some(timestamp(self.creation_time)),
            last_modified_time: Some(timestamp(self.last_modified_time)),
            source: Some(self.source.clone()),
            target: Some(self.target.clone()),
            enrichment: self.enrichment.clone(),
        }
    }

    /// The pipe's SOURCE configuration as a read of the pipe reports it: what the pipe
    /// actually polls its source with.
    ///
    /// Two things make the reported configuration more than the stored one. The container
    /// is always there — a read reports the pipe's configuration, not a record of which
    /// members its create request happened to mention, and `PipeSourceParameters` holds
    /// only optional members, so the empty one is a legal value of the shape. And where the
    /// service supplies a value the request left out, the read reports the value IN FORCE:
    /// the `BatchSize` the pipe's source type polls with when its parameters name none
    /// ([`SOURCE_TYPE_MEMBERS`]).
    pub fn reported_source_parameters(&self) -> PipeSourceParameters {
        let mut reported = self.source_parameters.clone().unwrap_or_default();
        let source_type = self.source_type();
        for entry in SOURCE_TYPE_MEMBERS
            .iter()
            .filter(|entry| entry.source_type == source_type)
        {
            (entry.report_effective_batch_size)(&mut reported);
        }
        reported
    }

    /// The pipe's ENRICHMENT configuration as a read reports it: the stored one, or the
    /// empty shape for a pipe configured without enrichment parameters. The container rule
    /// of [`Self::reported_source_parameters`].
    pub fn reported_enrichment_parameters(&self) -> PipeEnrichmentParameters {
        self.enrichment_parameters.clone().unwrap_or_default()
    }

    /// The pipe's TARGET configuration as a read reports it: the stored one, or the empty
    /// shape for a pipe configured without target parameters. The container rule of
    /// [`Self::reported_source_parameters`].
    pub fn reported_target_parameters(&self) -> PipeTargetParameters {
        self.target_parameters.clone().unwrap_or_default()
    }

    /// The stored tags as the map every read ALWAYS reports, empty when the pipe carries no
    /// tag.
    ///
    /// The container rule of [`Self::reported_source_parameters`] again:
    /// `ListTagsForResource` and `DescribePipe` answer with their tag member present for a
    /// pipe with no tags, so a caller can read it without first asking whether it is there.
    /// `TagMap`'s `@length(min: 1)` bounds what a request may SEND, which is what the
    /// boundary validator enforces it on.
    pub fn tags_reported(&self) -> std::collections::HashMap<String, String> {
        self.tags
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    }
}

/// Per-service state for com-amazonaws-pipes-pipes. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsPipesPipesState {
    /// Every pipe this account holds, keyed by name. A `BTreeMap` so `ListPipes` and its
    /// `NextToken` paging enumerate in a single deterministic order.
    pub pipes: BTreeMap<String, PipeRecord>,
    /// The pipes a data-plane poller is currently running for, by name. One poller per
    /// pipe: a pipe is polled once however many lifecycle calls asked it to run (see
    /// [`data_plane`]).
    pollers: BTreeSet<String>,
}

impl ComAmazonawsPipesPipesState {
    /// The stored pipe whose ARN is `arn`, if this account holds one.
    ///
    /// The tagging operations address a pipe by its `PipeArn` rather than by name, so the
    /// lookup is over the ARN each record minted at create time. A pipe that was never
    /// created — or that `DeletePipe` has removed — is simply absent here, which is the
    /// 404 those operations model.
    pub fn pipe_by_arn(&self, arn: &str) -> Option<&PipeRecord> {
        self.pipes.values().find(|pipe| pipe.arn == arn)
    }

    /// [`Self::pipe_by_arn`] for the operations that write the record's tags.
    pub fn pipe_by_arn_mut(&mut self, arn: &str) -> Option<&mut PipeRecord> {
        self.pipes.values_mut().find(|pipe| pipe.arn == arn)
    }

    /// Claim the one data-plane poller of the pipe named `pipe`, answering whether this
    /// caller is the one that got it. `false` means a poller is already running for that
    /// pipe, so the caller starts none.
    pub fn claim_poller(&mut self, pipe: &str) -> bool {
        self.pollers.insert(pipe.to_string())
    }

    /// Release the claim of [`Self::claim_poller`], so a later lifecycle call can start a
    /// poller for this pipe again. Called by the poller itself as it ends.
    pub fn release_poller(&mut self, pipe: &str) {
        self.pollers.remove(pipe);
    }

    /// Apply every state move the clock has already made due. Call this after locking,
    /// before reading any pipe's `CurrentState`: it writes only what was owed at this
    /// instant even if nobody had called, so a reporting operation may do it too.
    pub fn settle_due_transitions(&mut self) {
        let now = Instant::now();
        for pipe in self.pipes.values_mut() {
            pipe.settle(now);
        }
    }
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsPipesPipesState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsPipesPipesState>)
}

/// The region this request is signed for, for the ARNs a handler mints.
///
/// Read from the SigV4 credential scope of the `Authorization` header (the same scope
/// the router already routes on); an unsigned request falls back to [`DEFAULT_REGION`].
pub fn request_region(parts: &::http::request::Parts) -> String {
    ::emulator_server::sigv4_credential_scope(&parts.headers)
        .map(|scope| scope.region)
        .unwrap_or_else(|| DEFAULT_REGION.to_string())
}

/// The ARN of the pipe named `name`: `arn:aws:pipes:<region>:<account>:pipe/<name>`.
pub fn pipe_arn(region: &str, name: &str) -> String {
    ::engine::emulator_arn("pipes", region, &format!("pipe/{name}"))
}

/// A stored instant as the modelled `Timestamp`.
pub fn timestamp(at: SystemTime) -> ::aws_smithy_types::DateTime {
    let millis = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX));
    ::aws_smithy_types::DateTime::from_millis(millis)
}

/// Encode the exclusive start key of the next `ListPipes` page as an opaque `NextToken`.
///
/// Hex rather than the bare name: `NextToken` is an opaque continuation token to its
/// caller, who must pass it back unchanged rather than build one.
pub fn encode_next_token(last_returned_name: &str) -> String {
    ::hex::encode(last_returned_name.as_bytes())
}

/// The exclusive start key a `NextToken` carries, or `None` when the token was not
/// produced by [`encode_next_token`].
pub fn decode_next_token(token: &str) -> Option<String> {
    let bytes = ::hex::decode(token).ok()?;
    String::from_utf8(bytes).ok()
}
