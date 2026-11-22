// SPDX-License-Identifier: CC0-1.0

//! This front door's DATA leg: the calls on the CONTENTS of an object, which CloudTrail
//! records as events of the `Data` category and delivers into the bucket of every trail
//! whose event selectors admit them.
//!
//! CloudTrail classes an account's API calls in two, and the two reach different readers. The
//! ADMINISTRATION of a bucket — creating it, reading and writing its configuration, listing
//! what it holds — is a management event, recorded in event history where `LookupEvents`
//! reports it. A call on ONE OBJECT is a call on the contents of a resource instead, so it is
//! a DATA event:
//!
//! * it is NOT event history ("data events are not logged by default when you create a
//!   trail", and a lookup serves management and Insights events), so however many objects a
//!   client reads and writes, none of those calls appears in a lookup;
//! * it is NOT published onto the default event bus, which carries management events; and
//! * it is delivered to a trail's BUCKET only while one of that trail's event selectors
//!   admits it — which such a selector states by naming the `Data` category and the TYPE of
//!   the resource the call acted on ([`OBJECT_RESOURCE_TYPE`], one of the three types a BASIC
//!   selector's `DataResource$Type` names as well), and may narrow further by the object's
//!   ARN or by the call's read/write class.
//!
//! This module is the three things that are THIS service's: WHICH of its operations are data
//! events and what each one's read/write class is ([`DATA_EVENT_OPERATIONS`]), WHICH object
//! each call acted on ([`data_event_resources`]), and the delivery of one such record into
//! the account's trails ([`ObjectDataEvents`]).
//!
//! The record itself is built by the shared recorder from the TYPED call
//! ([`::service_common::audited_operation`]), which is handed the deserialized input before
//! the handler runs and the handler's own `Result` after — so a data event of this service
//! reports the same parameters, identity and outcome a management event would, and a call
//! the service REFUSED is recorded with the code and the message its caller was answered
//! with. That is what makes a refused read auditable: an audit pipeline can tell a denied
//! `GetObject` from one that never happened.
//!
//! What is withheld is the object's own bytes ([`OMITTED_MEMBERS`]): CloudTrail logs the call
//! made on an object's contents, not those contents.

use std::sync::Arc;
use std::time::SystemTime;

use aws_smithy_runtime_api::http::Response as RtResponse;
use service_common::audited_operation::{AuditedOperation, AuditedService, CallKind};
use service_common::data_events::{DataEventDelivery, DeliveringDataEvents};
use service_common::management_events::{
    recorded_member_name, AuditedResource, ManagementEventLog, RecordedSession, ServedApiCall,
    SessionAuditDirectory,
};
use service_common::trail_log_files::DeliveredLogFile;
use smithy_server_runtime::{
    ErasedOperation, Operation, OperationContext, OperationRoutingTable, PreDispatchProtocol,
};

/// The `eventSource` every record of this service carries: the one CloudTrail stamps on an
/// Amazon S3 event (`smithy_query(query="service_info")` ->
/// `awsService.cloudTrailEventSource: s3.amazonaws.com`).
pub const EVENT_SOURCE: &str = "s3.amazonaws.com";

/// The type an OBJECT is named by in a delivered record's `resources` and in an advanced
/// event selector's `resources.type` field — and the one a basic selector's
/// `DataResource$Type` names this service's object data events by ("`AWS::S3::Object`", one of
/// the three basic types that member lists).
pub const OBJECT_RESOURCE_TYPE: &str = "AWS::S3::Object";

/// The members a record of this service does NOT carry the value of, replaced by the
/// redaction literal: the ones this model declares `@sensitive`
/// (`smithy_query(query="operation",operation="PutObject")` marks `SSECustomerKey`,
/// `SSEKMSKeyId` and `SSEKMSEncryptionContext`; the copy operations carry the source
/// customer key under its own name).
const SENSITIVE_MEMBERS: &[&str] = &[
    "SSECustomerKey",
    "SSEKMSKeyId",
    "SSEKMSEncryptionContext",
    "CopySourceSSECustomerKey",
];

/// The members a record leaves OUT entirely: the object's own bytes.
///
/// `Body` is the `@streaming` payload of a write (`PutObjectRequest$Body`,
/// `UploadPartRequest$Body`), and CloudTrail records the CALL made on an object's contents
/// rather than the contents themselves — so the payload is never read into a record, which is
/// also what keeps one record's footprint independent of the object's size.
const OMITTED_MEMBERS: &[&str] = &["Body"];

/// The envelope a pre-dispatch failure of one of this service's operations renders in: this
/// service speaks `restXml` with the error document UNWRAPPED
/// (`smithy_query(query="service_info")` -> `protocol_config.restXml.noErrorWrapping: true`)
/// and carries its own `@xmlNamespace` — the same two values the generated per-operation shim
/// passes, so a refusal raised ahead of a handler reads exactly as it did before.
///
/// Shared with the MANAGEMENT leg ([`super::management_events`]), which renders the
/// pre-dispatch failure of a bucket-level call in the same envelope: one service, one
/// protocol.
pub(super) const RESTXML_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::RestXml {
    xml_namespace: Some(super::wire::S3_XML_NAMESPACE),
    no_error_wrapping: true,
};

/// How a request names the object (or objects) a data call acts on.
///
/// The MODELLED member names are the ones stated here, with the record's own spelling of them
/// derived through [`recorded_member_name`] rather than transcribed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressedObjects {
    /// The `Bucket` and `Key` labels of the request's own address, which is how every
    /// single-object operation of this model names the object it acts on.
    AddressedKey,
    /// A DOCUMENT listing the keys of a multi-object delete: `Delete` -> `Objects` ->
    /// `Key`, beside the `Bucket` label (`DeleteObjects`).
    KeysInDocument,
}

/// The operations of this service CloudTrail records as DATA events, each with the read/write
/// class of its calls and the way its request names the objects it acts on.
///
/// The set is the documented one: "data events provide visibility into the resource
/// operations performed on or within a resource", and for this service those are the
/// OBJECT-LEVEL API operations (Amazon S3 User Guide, *Logging data events*) — the reads and
/// writes of one key, the multipart upload that writes one in parts, the per-object ACL,
/// tagging, retention and legal-hold surfaces, and the multi-object delete. Everything else
/// this service serves administers a BUCKET (its own configuration, its listings, its
/// policies), which is a management event and is not here.
///
/// Two object-level surfaces this service serves are NOT modelled operations of it, so no
/// typed call exists for the shared recorder to build a record from, and neither is here: the
/// S3 SELECT data plane, whose output is a `@streaming` event-stream union the scaffold typed
/// none of ([`super::select_object_content`]), and the BROWSER-FORM upload, which no route of
/// the model is bound to at all ([`super::post_object`]). Recording either needs the record to
/// be built where that request is served rather than from a typed input.
///
/// The CLASS per row is the operation's own documented semantics, which is what the event's
/// `readOnly` carries and what a trail's `ReadWriteType` (and an advanced selector's
/// `readOnly` field) narrows a trail's data events by: a read of an object or of one of its
/// subresources reports state, and everything that writes, copies, deletes or restores one
/// changes it.
///
/// Public, so this crate's own tests read the catalog the routing table is built from rather
/// than a second copy of it.
pub const DATA_EVENT_OPERATIONS: &[(&str, CallKind, AddressedObjects)] = &[
    (
        "AbortMultipartUpload",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "CompleteMultipartUpload",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "CopyObject",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "CreateMultipartUpload",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "DeleteObject",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "DeleteObjectTagging",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "DeleteObjects",
        CallKind::Mutating,
        AddressedObjects::KeysInDocument,
    ),
    (
        "GetObject",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "GetObjectAcl",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "GetObjectAttributes",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "GetObjectLegalHold",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "GetObjectRetention",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "GetObjectTagging",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "GetObjectTorrent",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "HeadObject",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "ListParts",
        CallKind::Reporting,
        AddressedObjects::AddressedKey,
    ),
    (
        "PutObject",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "PutObjectAcl",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "PutObjectLegalHold",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "PutObjectRetention",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "PutObjectTagging",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "RestoreObject",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "UploadPart",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
    (
        "UploadPartCopy",
        CallKind::Mutating,
        AddressedObjects::AddressedKey,
    ),
];

/// The read/write class CloudTrail records the calls of `operation` as a data event with, or
/// `None` for an operation whose calls are management events.
#[must_use]
pub fn recorded_class(operation: &str) -> Option<CallKind> {
    cataloged(operation).map(|(kind, _)| kind)
}

/// The catalog row of `operation`.
fn cataloged(operation: &str) -> Option<(CallKind, AddressedObjects)> {
    DATA_EVENT_OPERATIONS
        .iter()
        .find(|(named, _, _)| *named == operation)
        .map(|(_, kind, addressing)| (*kind, *addressing))
}

/// The modelled member carrying the bucket a request addresses, which every operation of this
/// model binds as a path label.
const BUCKET_MEMBER: &str = "Bucket";

/// The modelled member carrying the key a request addresses.
const KEY_MEMBER: &str = "Key";

/// The modelled members a multi-object delete names its keys under: the request document, the
/// list inside it, and each element's own key.
const DELETE_MEMBER: &str = "Delete";
const OBJECTS_MEMBER: &str = "Objects";

/// The resources a call of `operation` acted on, read off the parameters its record carries —
/// each object the request addressed, by that object's ARN.
///
/// A record names the resource a data event acted on because that is what a trail's selector
/// SELECTS the event by: `resources.type` names the class of data events a trail logs and
/// `resources.ARN` narrows it to one object (or to a key prefix), so a record naming no
/// resource is admitted by no such selector. A MANAGEMENT call of this service names none, so
/// this answers a resource only for the operations in the catalog above.
///
/// The ARN is this service's own ([`crate::state::object_arn`]), built from the bucket and the
/// key the request addressed — "`arn:aws:s3:::<bucket>/<key>`", which carries no Region and no
/// account because a bucket name is globally unique. A request that named no bucket, or no
/// key, names no object rather than one built from half an address.
///
/// The walk is BOUNDED at [`MAX_RECORDED_OBJECTS`] keys and de-duplicates through a set, so
/// what one request can make this cost is linear in that bound and not in the length of the
/// document it sent: the record is built from the request's own parameters BEFORE the handler
/// that refuses an over-long `Delete` document has run.
#[must_use]
pub fn data_event_resources(
    operation: &str,
    request_parameters: &::serde_json::Value,
) -> Vec<AuditedResource> {
    let Some((_, addressing)) = cataloged(operation) else {
        return Vec::new();
    };
    let Some(bucket) = addressed_bucket(request_parameters) else {
        return Vec::new();
    };
    let mut resources: Vec<AuditedResource> = Vec::new();
    // One resource per object, however many of a delete's entries addressed it: `resources`
    // names the resources the call acted on, not its entries. The ARNs already named are held
    // in a SET, so the walk costs one lookup per entry rather than a scan of what it has
    // built.
    let mut named: ::std::collections::BTreeSet<String> = ::std::collections::BTreeSet::new();
    for key in addressed_keys(addressing, request_parameters) {
        let arn = crate::state::object_arn(bucket, &key);
        if named.insert(arn.clone()) {
            resources.push(AuditedResource::new(OBJECT_RESOURCE_TYPE, arn));
        }
    }
    resources
}

/// The bucket and the key a data call of `operation` addressed, as the delivery reads them off
/// the record's own parameters — or `None` for a call that addressed no single object.
///
/// What the recursion guard asks: the delivery of a trail's log file is itself a write of an
/// object, and this is how the service serving that write recognises it
/// ([`ObjectDataEvents::delivered`]).
fn addressed_object(
    operation: &str,
    request_parameters: &::serde_json::Value,
) -> Option<(String, String)> {
    let (_, addressing) = cataloged(operation)?;
    let bucket = addressed_bucket(request_parameters)?;
    let mut keys = addressed_keys(addressing, request_parameters).into_iter();
    let key = keys.next()?;
    keys.next().is_none().then(|| (bucket.to_string(), key))
}

/// How many objects one record names at most, and so how many key-naming values a record's
/// parameters are walked for.
///
/// The published bound of the one operation that names several: "the request can contain a
/// list of up to 1,000 keys that you want to delete"
/// ([`crate::ops::delete_objects::MAX_KEYS_PER_DELETE`]). The record is rendered from the
/// request's parameters BEFORE the handler that refuses a longer list has run, so the walk is
/// bounded here as well — what a caller sends cannot make the audit path do unbounded work,
/// and no record of a request the service refuses names more objects than the service would
/// have acted on.
const MAX_RECORDED_OBJECTS: usize = crate::ops::delete_objects::MAX_KEYS_PER_DELETE;

/// Every key-naming value a record's `request_parameters` carries, as the request wrote it —
/// at most [`MAX_RECORDED_OBJECTS`] of them.
fn addressed_keys(
    addressing: AddressedObjects,
    request_parameters: &::serde_json::Value,
) -> Vec<String> {
    match addressing {
        AddressedObjects::AddressedKey => text_member(request_parameters, KEY_MEMBER)
            .map(|key| vec![key.to_string()])
            .unwrap_or_default(),
        AddressedObjects::KeysInDocument => request_parameters
            .get(recorded_member_name(DELETE_MEMBER))
            .and_then(|document| document.get(recorded_member_name(OBJECTS_MEMBER)))
            .and_then(::serde_json::Value::as_array)
            .map(|objects| {
                objects
                    .iter()
                    .take(MAX_RECORDED_OBJECTS)
                    .filter_map(|object| text_member(object, KEY_MEMBER))
                    .map(ToString::to_string)
                    .collect()
            })
            .unwrap_or_default(),
    }
}

/// The non-empty text `document` holds at the modelled member `member`, named as a record
/// names it.
fn text_member<'a>(document: &'a ::serde_json::Value, member: &str) -> Option<&'a str> {
    document
        .get(recorded_member_name(member))
        .and_then(::serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

/// The bucket a record's `request_parameters` name, in either spelling: the one this service's
/// own records carry ([`RECORDED_BUCKET_MEMBER`]) and the modelled member's own
/// ([`recorded_member_name`]), so a reader of the document answers the same whether or not it
/// has already been spelled this service's way ([`this_services_record`]).
fn addressed_bucket(request_parameters: &::serde_json::Value) -> Option<&str> {
    request_parameters
        .get(RECORDED_BUCKET_MEMBER)
        .and_then(::serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .or_else(|| text_member(request_parameters, BUCKET_MEMBER))
}

/// The name a record of THIS service spells the addressed bucket with.
///
/// CloudTrail names the members of a recorded call as the SERVING API spells them, which for
/// almost every service is the modelled member name with a lower-case first letter
/// ([`recorded_member_name`]) — and for Amazon S3 is `bucketName` for the bucket a request
/// addressed, beside the `key` the modelled name already gives. So a reader of the account's
/// trail finds the bucket under the member every S3 record of it carries.
const RECORDED_BUCKET_MEMBER: &str = "bucketName";

/// `call` as a record of THIS service: the two things about the document that are Amazon S3's
/// own rather than the shared recorder's.
///
/// 1. The BUCKET member is named as S3's records name it ([`RECORDED_BUCKET_MEMBER`]).
/// 2. The `errorMessage` is the sentence the CALLER was answered with. Every error shape of
///    this model declares no `message` member of its own (see
///    [`crate::scaffolded::DOCUMENTED_ERROR_MESSAGES`], the catalog this service's error
///    channel writes the `<Message>` of such a refusal from), so the error channel alone
///    carries no sentence for one — the record reads it from that same catalog, which is what
///    makes a delivered record's message the one the client received. A refusal that words its
///    OWN sentence (an authorization denial naming the principal, the action and the resource)
///    keeps it.
pub(super) fn this_services_record(mut call: ServedApiCall) -> ServedApiCall {
    if let Some(parameters) = call.request_parameters.as_object_mut() {
        if let Some(bucket) = parameters.remove(&recorded_member_name(BUCKET_MEMBER)) {
            parameters.insert(RECORDED_BUCKET_MEMBER.to_string(), bucket);
        }
    }
    if let Some(code) = call.error_code.as_deref() {
        // The error channel renders a shape that carries no message as the code alone, which
        // is the record saying nothing about why the call failed.
        if call.error_message.as_deref() == Some(code) {
            call.error_message =
                crate::scaffolded::documented_error_message(code).map(ToString::to_string);
        }
    }
    call
}

/// What this service contributes to the record of one of its data events, and to the delivery
/// of it: the log files the account's trails deliver it as.
///
/// Holds the shared engine — the trail projection the delivery is decided by lives in the log
/// that engine carries, the one CloudTrail recomputes whenever a trail of the account is
/// created, reconfigured, started or stopped — so it is generic over the engine type exactly
/// as the service is.
pub struct ObjectDataEvents<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> ObjectDataEvents<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for ObjectDataEvents<Eng> {
    /// Records NOTHING, because no call served through this recorder is a management event.
    ///
    /// Every row this recorder is wired into is filed behind the DATA-event delivery
    /// ([`DeliveringDataEvents`]), which never calls this — the layer that does
    /// ([`::service_common::audited_operation::RecordedCall`]) is the management-event one, and
    /// a data event is not event history ("data events are not logged by default when you
    /// create a trail"; a lookup serves management and Insights events). So there is nothing
    /// here to record: entering one of this service's object calls in the log would put it in
    /// the very event history the whole class must stay out of.
    fn record(
        &self,
        call: ServedApiCall,
        answered_as: &str,
    ) -> Option<::service_common::api_call_events::DeliveredApiCall> {
        let _ = (call, answered_as);
        None
    }

    fn event_source(&self) -> &'static str {
        EVENT_SOURCE
    }

    fn region(&self, ctx: &OperationContext<'_>) -> String {
        // The Region the call was served in, read off the request the way every other seam of
        // this service reads it ([`crate::state::request_region`]).
        crate::state::request_region(ctx.parts())
    }

    fn account_id(&self) -> String {
        ::engine::emulator_account_id().to_string()
    }

    fn resources(
        &self,
        operation: &str,
        request_parameters: &::serde_json::Value,
        region: &str,
    ) -> Vec<AuditedResource> {
        // An S3 ARN carries no Region: "`arn:aws:s3:::<bucket>`" (see
        // [`crate::state::bucket_arn`]), because a bucket name is globally unique.
        let _ = region;
        data_event_resources(operation, request_parameters)
    }

    fn sensitive_members(&self) -> &'static [&'static str] {
        SENSITIVE_MEMBERS
    }

    fn omitted_members(&self) -> &'static [&'static str] {
        OMITTED_MEMBERS
    }

    fn recorded_session(&self, access_key_id: &str) -> Option<RecordedSession> {
        // The account's own session directory, so a call made with a role session is recorded
        // as that session — and as the role that issued it: the shared store the minting
        // service wrote it to.
        session_audit_directory(&*self.engine)
            .and_then(|directory| directory.lock().session(access_key_id).cloned())
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        stated_request_id(resp)
    }
}

/// The id the CALLER was answered under, read off the response this service already stated
/// it in.
///
/// Every response of this service names the request it answers, in the header S3 names it in
/// ("`x-amz-request-id`: a value created by Amazon S3 that uniquely identifies the request")
/// — see [`super::wire`]. So the id is READ and the response is handed back untouched: the
/// shared reader reports the `x-amzn-requestid` almost every other service spells its ids in,
/// and writing that header onto an S3 response would put a second id on the wire.
///
/// Both legs of this service's auditing join their records to the caller's own response this
/// way ([`super::management_events`]), so a client holding a response finds the record of
/// that very call whichever class it was recorded as.
pub(super) fn stated_request_id(resp: RtResponse) -> (RtResponse, String) {
    let stated = resp
        .headers()
        .get(super::wire::REQUEST_ID_HEADER)
        .unwrap_or_default()
        .to_string();
    (resp, stated)
}

impl<Eng: Send + Sync + 'static> DataEventDelivery for ObjectDataEvents<Eng> {
    fn delivered(
        &self,
        call: ServedApiCall,
        served_at: SystemTime,
        answered_as: &str,
    ) -> Vec<DeliveredLogFile> {
        // A write CloudTrail is making RIGHT NOW to deliver a trail's own log file is not a
        // call on the account's data: it is the delivery of another call's record. Asking the
        // trails to deliver a record of it would, for a trail whose selectors admit the bucket
        // it delivers into, deliver a log file for the log file without end — so such a write
        // is left out of the account's data events, as the one call this service makes for
        // CloudTrail rather than for a client.
        if let Some((bucket, key)) = addressed_object(&call.event_name, &call.request_parameters) {
            if ::service_common::trail_log_files::is_delivering(&bucket, &key) {
                return Vec::new();
            }
        }
        let call = this_services_record(call);
        let Some(log) = management_event_log(&*self.engine) else {
            // Not silent: an undelivered data event leaves no trace in the account's trail by
            // definition, so it leaves one here instead.
            ::tracing::warn!(
                operation = call.event_name,
                "the engine carries no management-event log: the object call was served with \
                 no trail able to deliver it"
            );
            return Vec::new();
        };
        // Nothing is RECORDED: a data event is not event history, so the log is only asked
        // which of the account's trails deliver this record. The guard is BOUND rather than
        // used as a temporary: a local is released before the handle it borrows, which a
        // temporary at the end of this block would not be.
        let delivering = log.lock();
        delivering.data_event_log_files(call, served_at, answered_as)
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        // The same reading the record is joined to its caller's response by, stated once
        // ([`AuditedService::names_the_request`]).
        AuditedService::names_the_request(self, resp)
    }
}

/// The account's management-event log in the shared engine.
///
/// A SHARED type ([`service_common::management_events::ManagementEventLog`]) and not a field
/// of this service's own state: it is where CloudTrail projects which trails of the account
/// are logging and what each one delivers, and the service that served a call is the only one
/// that knows it happened — so deliverer and projector reach one log rather than two copies.
pub fn management_event_log<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ManagementEventLog>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ManagementEventLog>)
}

/// The account's SESSION directory in that same shared engine: the sessions the token service
/// minted, each keyed by the access key id it was minted under, which is what lets the record
/// of a call made with one name the session it was made by.
pub fn session_audit_directory<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<SessionAuditDirectory>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<SessionAuditDirectory>)
}

/// `operation` served as the DATA event of the object it acts on, with `wrap` applied between
/// the record and the delivery — the wrappers this service already serves that operation
/// behind ([`super::request_metrics::Metered`], a front door), so one operation is served
/// once rather than twice.
///
/// The three layers, outermost first: the DELIVERY of the record
/// ([`DeliveringDataEvents`]), which needs the rendered response because the record names the
/// request the CALLER was answered under; this service's own wrappers; and the RECORDER, which
/// is innermost because the record reports the operation's own typed input and outcome.
pub fn served_as_data_event<Eng, O, W>(
    recorder: &Arc<ObjectDataEvents<Eng>>,
    operation: O,
    kind: CallKind,
    wrap: impl FnOnce(AuditedOperation<O, ObjectDataEvents<Eng>>) -> W,
) -> Box<dyn ErasedOperation>
where
    Eng: Send + Sync + 'static,
    O: Operation + ErasedOperation,
    W: ErasedOperation + 'static,
{
    let audited = AuditedOperation::data_event(
        operation,
        Arc::clone(recorder),
        kind,
        RESTXML_ENVELOPE,
    );
    Box::new(DeliveringDataEvents::new(
        wrap(audited),
        Arc::clone(recorder),
    ))
}

/// Serve every data-event operation of this service that this service wraps in NOTHING else,
/// each as the data event of the object it acts on and under the modelled name the protocol
/// resolves for it.
///
/// The object operations that are also COUNTED into a bucket's request metrics, or served
/// behind a front door, are wired where those wrappers are built
/// ([`super::request_metrics::serve_metered`], [`super::front_door`]) so that one operation is
/// not served twice; everything else in the catalog is served here.
pub fn serve_object_data_events<Eng: Send + Sync + 'static>(
    table: &mut OperationRoutingTable,
    engine: &Arc<Eng>,
    recorder: &Arc<ObjectDataEvents<Eng>>,
) {
    /// Serve one operation as its own data event, wrapped in nothing else.
    ///
    /// The read/write class is the catalog's ([`DATA_EVENT_OPERATIONS`]), which is where it is
    /// stated once: an operation the catalog does not hold is left to the generated table, so a
    /// row here and the catalog cannot disagree about which calls are data events.
    macro_rules! data_plane {
        ( $( $module:ident :: $operation:ident ),* $(,)? ) => {{
            $(
                let name = <crate::ops::$module::$operation<Eng> as Operation>::NAME;
                if let Some(kind) = recorded_class(name) {
                    table.add_operation(
                        name,
                        served_as_data_event(
                            recorder,
                            crate::ops::$module::$operation::new(Arc::clone(engine)),
                            kind,
                            |audited| audited,
                        ),
                    );
                }
            )*
        }};
    }
    data_plane!(
        delete_object_tagging::DeleteObjectTagging,
        delete_objects::DeleteObjects,
        get_object_acl::GetObjectAcl,
        get_object_attributes::GetObjectAttributes,
        get_object_legal_hold::GetObjectLegalHold,
        get_object_retention::GetObjectRetention,
        get_object_tagging::GetObjectTagging,
        get_object_torrent::GetObjectTorrent,
        list_parts::ListParts,
        put_object_acl::PutObjectAcl,
        put_object_legal_hold::PutObjectLegalHold,
        put_object_retention::PutObjectRetention,
        put_object_tagging::PutObjectTagging,
        restore_object::RestoreObject,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One object ARN as this service builds it.
    fn object(bucket: &str, key: &str) -> String {
        crate::state::object_arn(bucket, key)
    }

    /// The resources as the `<type> <arn>` pairs they name, in order.
    fn named(resources: &[AuditedResource]) -> Vec<String> {
        resources
            .iter()
            .map(|resource| format!("{} {}", resource.resource_type, resource.resource_name))
            .collect()
    }

    /// RULE: a data event's record names each OBJECT the request addressed, by that object's
    /// ARN and under the object data-event resource type; a call of this service that
    /// administers a bucket names no resource.
    ///
    /// The rows differ along the FORM the request names its objects in (the address, a delete
    /// document), along how many objects one call addresses, along the operation's class, and
    /// along what a half-addressed or wrongly typed parameter leaves.
    #[test]
    fn a_data_events_record_names_the_objects_the_request_addressed() {
        let rows: Vec<(&str, &str, ::serde_json::Value, Vec<String>)> = vec![
            (
                "a read of one key",
                "GetObject",
                ::serde_json::json!({"bucket": "ledger", "key": "reports/held.csv"}),
                vec![format!("{OBJECT_RESOURCE_TYPE} {}", object("ledger", "reports/held.csv"))],
            ),
            (
                "a write of one key",
                "PutObject",
                ::serde_json::json!({"bucket": "ledger", "key": "inbox/new.txt"}),
                vec![format!("{OBJECT_RESOURCE_TYPE} {}", object("ledger", "inbox/new.txt"))],
            ),
            (
                "a part of a multipart upload",
                "UploadPart",
                ::serde_json::json!({"bucket": "ledger", "key": "big.bin", "partNumber": 2}),
                vec![format!("{OBJECT_RESOURCE_TYPE} {}", object("ledger", "big.bin"))],
            ),
            (
                "a per-object subresource read",
                "GetObjectTagging",
                ::serde_json::json!({"bucket": "ledger", "key": "tagged.txt"}),
                vec![format!("{OBJECT_RESOURCE_TYPE} {}", object("ledger", "tagged.txt"))],
            ),
            (
                "a multi-object delete naming two keys, one of them twice",
                "DeleteObjects",
                ::serde_json::json!({"bucket": "ledger", "delete": {"objects": [
                    {"key": "a.txt"},
                    {"key": "b.txt"},
                    {"key": "a.txt"},
                ]}}),
                vec![
                    format!("{OBJECT_RESOURCE_TYPE} {}", object("ledger", "a.txt")),
                    format!("{OBJECT_RESOURCE_TYPE} {}", object("ledger", "b.txt")),
                ],
            ),
            (
                "a multi-object delete naming no key at all",
                "DeleteObjects",
                ::serde_json::json!({"bucket": "ledger", "delete": {"objects": []}}),
                Vec::new(),
            ),
            (
                "a bucket's own configuration",
                "PutBucketPolicy",
                ::serde_json::json!({"bucket": "ledger", "policy": "{}"}),
                Vec::new(),
            ),
            (
                "a listing of what a bucket holds",
                "ListObjectsV2",
                ::serde_json::json!({"bucket": "ledger", "prefix": "reports/"}),
                Vec::new(),
            ),
            (
                "a read carrying no bucket",
                "GetObject",
                ::serde_json::json!({"key": "reports/held.csv"}),
                Vec::new(),
            ),
            (
                "a read carrying no key",
                "GetObject",
                ::serde_json::json!({"bucket": "ledger"}),
                Vec::new(),
            ),
            (
                "a read whose key carries no text",
                "GetObject",
                ::serde_json::json!({"bucket": "ledger", "key": ""}),
                Vec::new(),
            ),
            (
                "a read whose bucket is a value of another type",
                "GetObject",
                ::serde_json::json!({"bucket": 7, "key": "reports/held.csv"}),
                Vec::new(),
            ),
        ];
        for (case, operation, request_parameters, expected) in rows {
            let actual = data_event_resources(operation, &request_parameters);
            assert_eq!(
                named(&actual),
                expected,
                "case {case:?}: expected the record to name {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the operations recorded as data events are exactly the catalog, each with the
    /// read/write class its own semantics give it; every other operation of this service is a
    /// management event.
    #[test]
    fn only_the_cataloged_object_operations_are_data_events() {
        for (operation, kind, _) in DATA_EVENT_OPERATIONS {
            assert_eq!(
                recorded_class(operation),
                Some(*kind),
                "case {operation:?}: a cataloged operation is a data event of its own class"
            );
        }
        // One row per kind of bucket-level call: the bucket's lifecycle, its listings, its
        // configuration, its policy and its tags.
        for operation in [
            "CreateBucket",
            "DeleteBucket",
            "HeadBucket",
            "ListBuckets",
            "ListObjects",
            "ListObjectsV2",
            "ListObjectVersions",
            "ListMultipartUploads",
            "GetBucketPolicy",
            "PutBucketPolicy",
            "GetBucketTagging",
            "PutBucketTagging",
            "GetBucketVersioning",
            "PutBucketNotificationConfiguration",
        ] {
            assert_eq!(
                recorded_class(operation),
                None,
                "case {operation:?}: a bucket-level call is no data event"
            );
        }
    }

    /// RULE: the record of one of this service's calls spells the bucket the member S3's own
    /// records name it by, and carries as its `errorMessage` the sentence the CALLER was
    /// answered with.
    ///
    /// The rows differ along the outcome (served, a refusal whose shape carries no message of
    /// its own, a refusal that words its own sentence) and along what the parameters hold.
    #[test]
    fn a_record_of_this_service_names_its_bucket_and_the_message_the_caller_got() {
        /// One row: the case, the code and message the error channel reported, the parameters
        /// the recorder rendered, and the document and message the record carries.
        type Row = (
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
            ::serde_json::Value,
            ::serde_json::Value,
            Option<&'static str>,
        );
        let rows: Vec<Row> = vec![
            (
                "a served read",
                None,
                None,
                ::serde_json::json!({"bucket": "ledger", "key": "held.csv"}),
                ::serde_json::json!({"bucketName": "ledger", "key": "held.csv"}),
                None,
            ),
            (
                "a read of a key the bucket does not hold",
                Some("NoSuchKey"),
                Some("NoSuchKey"),
                ::serde_json::json!({"bucket": "ledger", "key": "absent.csv"}),
                ::serde_json::json!({"bucketName": "ledger", "key": "absent.csv"}),
                Some("The specified key does not exist."),
            ),
            (
                "a read refused by a resource policy, which words its own sentence",
                Some("AccessDenied"),
                Some(
                    "User: arn:aws:sts::000000000000:assumed-role/reader/session is not \
                     authorized to perform: s3:GetObject on resource: \
                     \"arn:aws:s3:::ledger/held.csv\" with an explicit deny in a \
                     resource-based policy",
                ),
                ::serde_json::json!({"bucket": "ledger", "key": "held.csv"}),
                ::serde_json::json!({"bucketName": "ledger", "key": "held.csv"}),
                Some(
                    "User: arn:aws:sts::000000000000:assumed-role/reader/session is not \
                     authorized to perform: s3:GetObject on resource: \
                     \"arn:aws:s3:::ledger/held.csv\" with an explicit deny in a \
                     resource-based policy",
                ),
            ),
            (
                "a refusal whose code this service publishes no sentence for",
                Some("SomethingUnpublished"),
                Some("SomethingUnpublished"),
                ::serde_json::json!({"bucket": "ledger"}),
                ::serde_json::json!({"bucketName": "ledger"}),
                None,
            ),
            (
                "a call whose parameters were not recorded at all",
                None,
                None,
                ::serde_json::Value::Null,
                ::serde_json::Value::Null,
                None,
            ),
        ];
        for (case, code, message, rendered, expected, expected_message) in rows {
            let recorded = this_services_record(ServedApiCall {
                event_name: "GetObject".to_string(),
                event_source: EVENT_SOURCE.to_string(),
                aws_region: "us-east-1".to_string(),
                read_only: true,
                access_key_id: None,
                user_name: None,
                principal_type: "Unknown".to_string(),
                principal_id: None,
                identity_provider: None,
                invoked_by: None,
                session: None,
                account_id: ::engine::emulator_account_id().to_string(),
                user_agent: None,
                resources: Vec::new(),
                request_parameters: rendered,
                response_elements: None,
                additional_event_data: None,
                error_code: code.map(ToString::to_string),
                error_message: message.map(ToString::to_string),
            });
            assert_eq!(
                recorded.request_parameters, expected,
                "case {case:?}: expected the parameters spelled {expected}, got {}",
                recorded.request_parameters
            );
            assert_eq!(
                recorded.error_message.as_deref(),
                expected_message,
                "case {case:?}: expected the message the caller got, got {:?}",
                recorded.error_message
            );
        }
    }

    /// RULE: the walk over a record's key-naming values is bounded at
    /// [`MAX_RECORDED_OBJECTS`] — the published bound of the one operation that names several
    /// objects — so a document longer than the service would act on cannot make the audit path
    /// do unbounded work.
    #[test]
    fn a_record_names_at_most_the_published_number_of_objects() {
        for (case, keys) in [
            ("one key under the bound", 1_usize),
            ("exactly the bound", MAX_RECORDED_OBJECTS),
            ("twice the bound", MAX_RECORDED_OBJECTS * 2),
        ] {
            let objects: Vec<::serde_json::Value> = (0..keys)
                .map(|index| ::serde_json::json!({"key": format!("object-{index}")}))
                .collect();
            let actual = data_event_resources(
                "DeleteObjects",
                &::serde_json::json!({"bucket": "ledger", "delete": {"objects": objects}}),
            );
            let expected = keys.min(MAX_RECORDED_OBJECTS);
            assert_eq!(
                actual.len(),
                expected,
                "case {case:?}: expected {expected} resource(s), got {}",
                actual.len()
            );
        }
    }

    /// RULE: the object a delivery guard asks about is the ONE object a call addressed — a
    /// call that addressed several, or none, is no single-object write.
    #[test]
    fn the_addressed_object_is_the_one_object_a_call_named() {
        /// One row: the case, the operation called, the parameters its record carries, and the
        /// one object the call addressed.
        type ObjectRow = (
            &'static str,
            &'static str,
            ::serde_json::Value,
            Option<(String, String)>,
        );
        let rows: Vec<ObjectRow> = vec![
            (
                "a write of one key",
                "PutObject",
                ::serde_json::json!({"bucket": "audit-logs", "key": "AWSLogs/file.json.gz"}),
                Some((
                    "audit-logs".to_string(),
                    "AWSLogs/file.json.gz".to_string(),
                )),
            ),
            (
                "a delete naming two keys",
                "DeleteObjects",
                ::serde_json::json!({"bucket": "audit-logs", "delete": {"objects": [
                    {"key": "a"},
                    {"key": "b"},
                ]}}),
                None,
            ),
            (
                "a call that addressed no object",
                "GetObject",
                ::serde_json::json!({"bucket": "audit-logs"}),
                None,
            ),
            (
                "a bucket-level call",
                "PutBucketPolicy",
                ::serde_json::json!({"bucket": "audit-logs", "policy": "{}"}),
                None,
            ),
        ];
        for (case, operation, request_parameters, expected) in rows {
            let actual = addressed_object(operation, &request_parameters);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
