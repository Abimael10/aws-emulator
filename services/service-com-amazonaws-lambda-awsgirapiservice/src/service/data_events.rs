// SPDX-License-Identifier: CC0-1.0

//! This front door's DATA leg: the calls that RUN a function, which CloudTrail records as
//! events of the `Data` category and delivers into the bucket of every trail whose event
//! selectors admit them.
//!
//! CloudTrail classes an account's API calls in two, and the two reach different readers. The
//! ADMINISTRATION of a function — creating it, publishing a version, reading or changing its
//! configuration, listing what the account holds — is a management event, recorded in event
//! history where `LookupEvents` reports it. A call that INVOKES a function acts on the
//! contents of that resource instead, so it is a DATA event:
//!
//! * it is NOT event history ("data events are not logged by default when you create a
//!   trail", and a lookup serves management and Insights events), so however many times a
//!   client invokes a function, none of those calls appears in a lookup;
//! * it is NOT published onto the default event bus, which carries management events; and
//! * it is delivered to a trail's BUCKET only while one of that trail's event selectors
//!   admits it — which such a selector states by naming the `Data` category and the TYPE of
//!   the resource the call acted on ([`FUNCTION_RESOURCE_TYPE`], one of the three types a
//!   BASIC selector's `DataResource$Type` names as well), and may narrow further by the
//!   function's ARN, by the operation or by the call's read/write class.
//!
//! This module is the three things that are THIS service's: WHICH of its operations are data
//! events and what each one's read/write class is ([`DATA_EVENT_OPERATIONS`]), WHICH function
//! each call acted on ([`data_event_resources`]), and the delivery of one such record into
//! the account's trails ([`FunctionDataEvents`]).
//!
//! The record itself is built by the shared recorder from the TYPED call
//! ([`::service_common::audited_operation`]), which is handed the deserialized input before
//! the handler runs and the handler's own `Result` after — so a data event of this service
//! reports the parameters, the identity and the outcome of the call, and a call the service
//! REFUSED is recorded with the code and the message its caller was answered with. That is
//! what makes a refused invocation auditable: an audit pipeline can tell a denied `Invoke`
//! from one that never happened.
//!
//! What is withheld is everything but the function the call named ([`OMITTED_MEMBERS`]): a
//! delivered record reports the function an invocation ran — by its own ARN, whichever
//! documented form the request addressed it in ([`this_services_record`]) — with the version
//! it addressed beside the parameters ([`FUNCTION_VERSION_MEMBER`]). CloudTrail logs the call
//! made on a function, not the event document that call carried.
//!
//! Two invocation surfaces this service serves are NOT recorded here, and neither is an
//! omission. `InvokeWithResponseStream` implements no [`Operation`] at all — its output binds
//! a `@streaming` union, so it is served by its own hand-written entry
//! ([`crate::ops::invoke_with_response_stream`]) and there is no typed call for the shared
//! recorder to build a record from; and a request to a FUNCTION URL names no modelled
//! operation of this model either ([`super::function_url`]). Recording either needs the
//! record to be built where that request is served rather than from a typed input.

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
/// AWS Lambda event (`smithy_query(query="service_info")` ->
/// `awsService.cloudTrailEventSource: lambda.amazonaws.com`).
pub const EVENT_SOURCE: &str = "lambda.amazonaws.com";

/// The type a FUNCTION is named by in a delivered record's `resources` and in an advanced
/// event selector's `resources.type` field — and the one a basic selector's
/// `DataResource$Type` names this service's invocation data events by
/// ("`AWS::Lambda::Function`", one of the three basic types that member lists).
pub const FUNCTION_RESOURCE_TYPE: &str = "AWS::Lambda::Function";

/// The members a record of this service carries the redaction literal in place of the value
/// of: none.
///
/// The ONE member of the two inputs below that this model declares `@sensitive` is the
/// invocation's own payload (`smithy_query(query="operation",operation="Invoke")` marks
/// `Payload`), and a record does not carry it at all rather than carrying a placeholder for
/// it — see [`OMITTED_MEMBERS`].
const SENSITIVE_MEMBERS: &[&str] = &[];

/// The members a record leaves OUT entirely: everything an invocation carries except the
/// function it names.
///
/// A delivered invocation record reports the function and nothing else —
/// `requestParameters` is `{"functionName": <the function>}` — so the invocation's own
/// EVENT DOCUMENT and the way the client asked for the run are both left out:
///
/// * `Payload` is the body of the invocation (`InvocationRequest$Payload`, "the JSON that you
///   want to provide to your Lambda function as input") and `InvokeArgs` the `@streaming`
///   payload of the deprecated asynchronous one (`InvokeAsyncRequest$InvokeArgs`): CloudTrail
///   records the CALL made on a function rather than the data that call handed it, so neither
///   is ever read into a record — which is also what keeps one record's footprint independent
///   of the payload's size;
/// * `ClientContext` is more caller-supplied data the function is handed;
/// * `InvocationType` and `LogType` are how the client asked for the run rather than what the
///   call acted on, and `TenantId` and `DurableExecutionName` name work inside the run;
/// * `Qualifier` names the VERSION the invocation addressed, which the record reports beside
///   the parameters as the qualified function ([`FUNCTION_VERSION_MEMBER`]) rather than as a
///   parameter of its own.
const OMITTED_MEMBERS: &[&str] = &[
    "ClientContext",
    "DurableExecutionName",
    "InvocationType",
    "InvokeArgs",
    "LogType",
    "Payload",
    "Qualifier",
    "TenantId",
];

/// The member a record reports the function VERSION an invocation addressed under, beside the
/// parameters (`additionalEventData`).
///
/// It is reported there and not as a resource because the resource a data event names is the
/// FUNCTION, whose ARN carries no qualifier — which is what a trail's selector pins on
/// `resources.ARN` — while which version of it ran is a fact about the call. The value is the
/// QUALIFIED form of that ARN, carrying the version even for the unpublished one.
pub const FUNCTION_VERSION_MEMBER: &str = "functionVersion";

/// The envelope a pre-dispatch failure of one of these operations renders in: this service
/// speaks `restJson1` (`smithy_query(query="service_info")` -> `protocols: ["restJson1"]`),
/// whose errors carry `application/json` and name their shape in `x-amzn-errortype` — the
/// same values the generated per-operation shim passes, so a refusal raised ahead of a
/// handler reads exactly as it did before.
///
/// Shared with the MANAGEMENT leg ([`super::management_events`]): one protocol, one
/// envelope, whichever class of event the served call is recorded as.
pub(super) const JSON_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Json {
    content_type: "application/json",
    errortype_header: true,
};

/// The operations of this service CloudTrail records as DATA events, each with the read/write
/// class of its calls.
///
/// The set is the documented one: "data events provide visibility into the resource
/// operations performed on or within a resource", and for this service that is the function
/// INVOCATION — the call that runs the function and hands the run's own answer back to its
/// caller. Everything else this service serves administers the account's functions, versions,
/// aliases, layers, mappings and configurations, which is a management event and is not here.
///
/// The CLASS per row is the operation's own semantics, which is what the event's `readOnly`
/// carries and what a trail's `ReadWriteType` (and an advanced selector's `readOnly` field)
/// narrows a trail's data events by: an invocation RUNS the function, so it is a write
/// whichever invocation type it asks for.
///
/// Public, so this crate's own tests read the catalog the routing table is built from rather
/// than a second copy of it.
pub const DATA_EVENT_OPERATIONS: &[(&str, CallKind)] = &[
    ("Invoke", CallKind::Mutating),
    ("InvokeAsync", CallKind::Mutating),
];

/// The read/write class CloudTrail records the calls of `operation` as a data event with, or
/// `None` for an operation whose calls are management events.
#[must_use]
pub fn recorded_class(operation: &str) -> Option<CallKind> {
    DATA_EVENT_OPERATIONS
        .iter()
        .find(|(named, _)| *named == operation)
        .map(|(_, kind)| *kind)
}

/// Whether CloudTrail records the calls of `operation` as DATA events.
#[must_use]
pub fn is_data_event(operation: &str) -> bool {
    recorded_class(operation).is_some()
}

/// The modelled member every invocation names the function in, bound as the request's own
/// path label (`smithy_query(query="operation",operation="Invoke")` ->
/// `InvocationRequest$FunctionName`, required).
const FUNCTION_NAME_MEMBER: &str = "FunctionName";

/// The modelled member a synchronous invocation may name a version or an alias in beside the
/// name (`InvocationRequest$Qualifier`).
const QUALIFIER_MEMBER: &str = "Qualifier";

/// The resources a call of `operation` acted on, read off the parameters its record carries —
/// the function the request addressed, by that function's ARN.
///
/// A record names the resource a data event acted on because that is what a trail's selector
/// SELECTS the event by: `resources.type` names the class of data events a trail logs and
/// `resources.ARN` narrows it to one function, so a record naming no resource is admitted by
/// no such selector. A MANAGEMENT call of this service names none, so this answers a resource
/// only for the operations in the catalog above.
///
/// The ARN is this service's own ([`crate::state::function_arn`]), built from the function the
/// request addressed — whichever of the documented `FunctionName` forms it used, since all of
/// them name the same resource ([`crate::state::parse_function_reference`]) — and carries NO
/// qualifier: the resource the call acted on is the function, and which version of it ran is
/// reported beside the parameters instead ([`data_event_additional_data`]). `region` is the
/// Region the call was served in, which is the Region of every function this endpoint holds; a
/// reference that NAMES one (the full-ARN form is the only documented form that does) keeps
/// its own, so a record of a call that addressed another Region's function names the resource
/// the caller addressed. A `FunctionName` in none of the documented forms names no function
/// rather than one built from a value this service does not recognise.
#[must_use]
pub fn data_event_resources(
    operation: &str,
    request_parameters: &::serde_json::Value,
    region: &str,
) -> Vec<AuditedResource> {
    addressed_function(operation, request_parameters, region)
        .map(|(arn, _)| AuditedResource::new(FUNCTION_RESOURCE_TYPE, arn))
        .into_iter()
        .collect()
}

/// The QUALIFIED function an invocation addressed, as the record reports it beside the
/// parameters: `{"functionVersion": "<function arn>:<version>"}`.
///
/// The version is the one the request named — in the `FunctionName` itself or in the
/// `Qualifier` parameter — and the unpublished version ([`crate::state::VERSION_LATEST`])
/// where it named none, so the value carries a version whichever form the request used. A
/// call of an operation that is no data event, or one naming no function this service can
/// read, reports nothing.
#[must_use]
pub fn data_event_additional_data(
    operation: &str,
    request_parameters: &::serde_json::Value,
    region: &str,
) -> Option<::serde_json::Value> {
    let (arn, version) = addressed_function(operation, request_parameters, region)?;
    Some(::serde_json::json!({ FUNCTION_VERSION_MEMBER: format!("{arn}:{version}") }))
}

/// The function a call of `operation` addressed: its unqualified ARN and the version the
/// request named, or `None` for a call that is no data event or names no function.
fn addressed_function(
    operation: &str,
    request_parameters: &::serde_json::Value,
    region: &str,
) -> Option<(String, String)> {
    if !is_data_event(operation) {
        return None;
    }
    let function_name = text_member(request_parameters, FUNCTION_NAME_MEMBER)?;
    let reference = crate::state::parse_function_reference(function_name)?;
    let from_query = text_member(request_parameters, QUALIFIER_MEMBER);
    let version = reference
        .qualifier
        .as_deref()
        .or(from_query)
        .unwrap_or(crate::state::VERSION_LATEST)
        .to_string();
    let region = reference
        .region
        .as_deref()
        .filter(|named| !named.is_empty())
        .unwrap_or(region);
    Some((crate::state::function_arn(region, &reference.name), version))
}

/// The non-empty text a record's `request_parameters` holds at the modelled member `member`.
///
/// The document names its members the way a record spells them
/// ([`recorded_member_name`]), which is the one place that spelling is decided.
fn text_member<'a>(request_parameters: &'a ::serde_json::Value, member: &str) -> Option<&'a str> {
    request_parameters
        .get(recorded_member_name(member))
        .and_then(::serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
}

/// `call` as a record of THIS service: the one thing about the document that is AWS Lambda's
/// own rather than the shared recorder's.
///
/// The FUNCTION member carries the function's own ARN. A caller addresses a function in any of
/// the documented `FunctionName` forms — a bare name, a name with a qualifier, a partial ARN,
/// a full ARN — and they all name one resource, so a record of a call on a function names that
/// function the one way it can be named outside the request: unqualified, as the record's
/// `resources` entry names it, with the version the call addressed reported beside it
/// ([`FUNCTION_VERSION_MEMBER`]). An audit pipeline reading the delivered record therefore
/// finds the same function whichever form the client happened to write, and the three places a
/// record names it cannot disagree.
fn this_services_record(mut call: ServedApiCall) -> ServedApiCall {
    naming_the_function(
        &call.event_name,
        &call.aws_region,
        &mut call.request_parameters,
    );
    call
}

/// Rewrite the FUNCTION member of `request_parameters` to the ARN of the function the call
/// addressed, leaving a document that names no function this service can read exactly as it
/// stands. The one write [`this_services_record`] makes, stated apart from the record so it is
/// read on the document alone.
fn naming_the_function(operation: &str, region: &str, request_parameters: &mut ::serde_json::Value) {
    let Some((arn, _)) = addressed_function(operation, request_parameters, region) else {
        return;
    };
    if let Some(parameters) = request_parameters.as_object_mut() {
        parameters.insert(
            recorded_member_name(FUNCTION_NAME_MEMBER),
            ::serde_json::Value::String(arn),
        );
    }
}

/// What this service contributes to the record of one of its data events, and to the delivery
/// of it: the log files the account's trails deliver it as.
///
/// Holds the shared engine — the trail projection the delivery is decided by lives in the log
/// that engine carries, the one CloudTrail recomputes whenever a trail of the account is
/// created, reconfigured, started or stopped — so it is generic over the engine type exactly
/// as the service is.
pub struct FunctionDataEvents<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> FunctionDataEvents<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for FunctionDataEvents<Eng> {
    /// Records NOTHING, because no call served through this recorder is a management event.
    ///
    /// Every row this recorder is wired into is filed behind the DATA-event delivery
    /// ([`DeliveringDataEvents`]), which never calls this — the layer that does
    /// ([`::service_common::audited_operation::RecordedCall`]) is the management-event one,
    /// and a data event is not event history ("data events are not logged by default when you
    /// create a trail"; a lookup serves management and Insights events). So there is nothing
    /// here to record: entering an invocation in the log would put it in the very event
    /// history the whole class must stay out of.
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
        // The Region the call was served in, read off the request the way every handler of
        // this service reads it ([`crate::state::request_region`]).
        crate::state::request_region(ctx)
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
        data_event_resources(operation, request_parameters, region)
    }

    fn additional_event_data(
        &self,
        operation: &str,
        request_parameters: &::serde_json::Value,
        region: &str,
    ) -> Option<::serde_json::Value> {
        data_event_additional_data(operation, request_parameters, region)
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
        // Every answer of this service already names the request it answers, in the header an
        // SDK reads its response metadata's `RequestId` from — and for an INVOCATION that id
        // is the invocation's own (see [`super::request_id_header`]). So the id is READ out of
        // that header and the response is handed back untouched: the record names the request
        // the CALLER was answered under, which for an invocation is the run a client can find
        // in its logs.
        ::service_common::query_error_envelope::named_request_as(
            resp,
            crate::state::mint_request_id,
        )
    }
}

impl<Eng: Send + Sync + 'static> DataEventDelivery for FunctionDataEvents<Eng> {
    fn delivered(
        &self,
        call: ServedApiCall,
        served_at: SystemTime,
        answered_as: &str,
    ) -> Vec<DeliveredLogFile> {
        let call = this_services_record(call);
        let Some(log) = management_event_log(&*self.engine) else {
            // Not silent: an undelivered data event leaves no trace in the account's trail by
            // definition, so it leaves one here instead.
            ::tracing::warn!(
                operation = call.event_name,
                "the engine carries no management-event log: the invocation was served with \
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
///
/// Read by both legs of this front door ([`super::management_events`] too), so the session a
/// data event and a management event of one caller are recorded as cannot differ.
pub(super) fn session_audit_directory<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<SessionAuditDirectory>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<SessionAuditDirectory>)
}

/// `operation` served as the DATA event of the function it runs, as a call of `kind`.
///
/// The three layers, outermost first: the DELIVERY of the record
/// ([`DeliveringDataEvents`]), which needs the rendered response because the record names the
/// request the CALLER was answered under; this service's own per-operation wrapper
/// ([`super::StatedPerOperation`]), which every row of this service's table carries; and the
/// RECORDER, which is innermost because the record reports the operation's own typed input and
/// outcome.
fn served_as_data_event<Eng, O>(
    recorder: &Arc<FunctionDataEvents<Eng>>,
    operation: O,
    kind: CallKind,
) -> Box<dyn ErasedOperation>
where
    Eng: Send + Sync + 'static,
    O: Operation + ErasedOperation + 'static,
{
    let audited = AuditedOperation::data_event(operation, Arc::clone(recorder), kind, JSON_ENVELOPE);
    Box::new(DeliveringDataEvents::new(
        super::StatedPerOperation::new(audited),
        Arc::clone(recorder),
    ))
}

/// Serve every invocation of this service as the data event of the function it runs, under the
/// modelled name the protocol resolves for it.
///
/// The rows are the catalog's ([`DATA_EVENT_OPERATIONS`]), which is where the read/write class
/// is stated once: an operation the catalog does not hold is served by the table's own row, so
/// a row here and the catalog cannot disagree about which calls are data events.
pub fn serve_invocation_data_events<Eng: Send + Sync + 'static>(
    table: &mut OperationRoutingTable,
    engine: &Arc<Eng>,
) {
    let recorder = Arc::new(FunctionDataEvents::new(Arc::clone(engine)));
    macro_rules! data_plane {
        ( $( $module:ident :: $operation:ident ),* $(,)? ) => {{
            $(
                let name = <crate::ops::$module::$operation<Eng> as Operation>::NAME;
                if let Some(kind) = recorded_class(name) {
                    table.add_operation(
                        name,
                        served_as_data_event(
                            &recorder,
                            crate::ops::$module::$operation::new(Arc::clone(engine)),
                            kind,
                        ),
                    );
                }
            )*
        }};
    }
    data_plane!(invoke::Invoke, invoke_async::InvokeAsync);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Region the records below are served in.
    const REGION: &str = "us-west-2";

    /// The resources as the `<type> <arn>` pairs they name, in order.
    fn named(resources: &[AuditedResource]) -> Vec<String> {
        resources
            .iter()
            .map(|resource| format!("{} {}", resource.resource_type, resource.resource_name))
            .collect()
    }

    /// One expected pair: the function resource type and `arn`.
    fn function(arn: &str) -> Vec<String> {
        vec![format!("{FUNCTION_RESOURCE_TYPE} {arn}")]
    }

    /// RULE: a data event's record names the FUNCTION the request addressed, by that
    /// function's UNQUALIFIED ARN and under the function data-event resource type; a call of
    /// any other operation of this service names no resource.
    ///
    /// The rows differ along the documented FORM the request named the function in (a bare
    /// name, a name with an alias, the `function:` prefix, a partial ARN, a full ARN), along
    /// whether a `Qualifier` parameter named the version instead, along the operation, and
    /// along what a member of the wrong type or no member at all leaves.
    #[test]
    fn a_data_events_record_names_the_function_the_request_addressed() {
        let unqualified = format!("arn:aws:lambda:{REGION}:000000000000:function:fn");
        let rows: Vec<(&str, &str, ::serde_json::Value, Vec<String>)> = vec![
            (
                "a bare function name",
                "Invoke",
                ::serde_json::json!({"functionName": "fn"}),
                function(&unqualified),
            ),
            (
                "a name carrying an alias",
                "Invoke",
                ::serde_json::json!({"functionName": "fn:live"}),
                function(&unqualified),
            ),
            (
                "a name whose version comes from the Qualifier parameter",
                "Invoke",
                ::serde_json::json!({"functionName": "fn", "qualifier": "7"}),
                function(&unqualified),
            ),
            (
                "the unpublished version named explicitly",
                "Invoke",
                ::serde_json::json!({"functionName": "fn", "qualifier": "$LATEST"}),
                function(&unqualified),
            ),
            (
                "the bare resource-type prefix form",
                "Invoke",
                ::serde_json::json!({"functionName": "function:fn"}),
                function(&unqualified),
            ),
            (
                "a partial ARN",
                "Invoke",
                ::serde_json::json!({"functionName": "000000000000:function:fn"}),
                function(&unqualified),
            ),
            (
                "a full ARN of this Region",
                "Invoke",
                ::serde_json::json!({"functionName": unqualified.clone()}),
                function(&unqualified),
            ),
            (
                "a full ARN of ANOTHER Region, which names its own",
                "Invoke",
                ::serde_json::json!({
                    "functionName": "arn:aws:lambda:eu-west-1:000000000000:function:fn",
                }),
                function("arn:aws:lambda:eu-west-1:000000000000:function:fn"),
            ),
            (
                "the deprecated asynchronous invocation",
                "InvokeAsync",
                ::serde_json::json!({"functionName": "fn"}),
                function(&unqualified),
            ),
            (
                "a read of the same function, which is administration",
                "GetFunction",
                ::serde_json::json!({"functionName": "fn"}),
                Vec::new(),
            ),
            (
                "a create of a function, which is administration",
                "CreateFunction",
                ::serde_json::json!({"functionName": "fn"}),
                Vec::new(),
            ),
            (
                "an invocation whose member carries no text at all",
                "Invoke",
                ::serde_json::json!({"functionName": ""}),
                Vec::new(),
            ),
            (
                "an invocation whose member carries a value of another type",
                "Invoke",
                ::serde_json::json!({"functionName": 7}),
                Vec::new(),
            ),
            (
                "an invocation carrying no such member",
                "Invoke",
                ::serde_json::json!({"logType": "Tail"}),
                Vec::new(),
            ),
            (
                "an invocation naming a value in none of the documented forms",
                "Invoke",
                ::serde_json::json!({"functionName": "arn:aws:s3:::bucket"}),
                Vec::new(),
            ),
        ];
        for (case, operation, request_parameters, expected) in rows {
            let actual = named(&data_event_resources(
                operation,
                &request_parameters,
                REGION,
            ));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the record to name {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: a data event's record reports the function VERSION the invocation addressed
    /// beside its parameters, as the QUALIFIED form of the function's ARN — carrying a version
    /// whichever form the request named the function in, the unpublished one included.
    ///
    /// The rows differ along where the version came from (the name's own qualifier, the
    /// `Qualifier` parameter, neither), along its form (a number, an alias, the unpublished
    /// version) and along the operation.
    #[test]
    fn a_data_events_record_reports_the_version_the_invocation_addressed() {
        let unqualified = format!("arn:aws:lambda:{REGION}:000000000000:function:fn");
        let rows: Vec<(&str, &str, ::serde_json::Value, Option<String>)> = vec![
            (
                "a name naming no version at all",
                "Invoke",
                ::serde_json::json!({"functionName": "fn"}),
                Some(format!("{unqualified}:$LATEST")),
            ),
            (
                "a name carrying a published version",
                "Invoke",
                ::serde_json::json!({"functionName": "fn:7"}),
                Some(format!("{unqualified}:7")),
            ),
            (
                "a name carrying an alias",
                "Invoke",
                ::serde_json::json!({"functionName": "fn:live"}),
                Some(format!("{unqualified}:live")),
            ),
            (
                "a version named by the Qualifier parameter",
                "Invoke",
                ::serde_json::json!({"functionName": "fn", "qualifier": "7"}),
                Some(format!("{unqualified}:7")),
            ),
            (
                "the unpublished version named explicitly",
                "Invoke",
                ::serde_json::json!({"functionName": "fn", "qualifier": "$LATEST"}),
                Some(format!("{unqualified}:$LATEST")),
            ),
            (
                "the deprecated asynchronous invocation, which names no version",
                "InvokeAsync",
                ::serde_json::json!({"functionName": "fn"}),
                Some(format!("{unqualified}:$LATEST")),
            ),
            (
                "a control-plane call of the same function",
                "GetFunction",
                ::serde_json::json!({"functionName": "fn", "qualifier": "7"}),
                None,
            ),
            (
                "an invocation naming no function this service can read",
                "Invoke",
                ::serde_json::json!({"functionName": "arn:aws:s3:::bucket"}),
                None,
            ),
        ];
        for (case, operation, request_parameters, expected) in rows {
            let reported = data_event_additional_data(operation, &request_parameters, REGION);
            let actual = reported
                .as_ref()
                .and_then(|data| data.get(FUNCTION_VERSION_MEMBER))
                .and_then(::serde_json::Value::as_str)
                .map(ToString::to_string);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {FUNCTION_VERSION_MEMBER} {expected:?}, got \
                 {actual:?} in {reported:?}"
            );
        }
    }

    /// RULE: a delivered record names the function by its own ARN, whichever documented form
    /// the request addressed it in — so the parameters, the resource and the reported version
    /// all name one function — and leaves a document naming no function this service can read
    /// exactly as it stands.
    ///
    /// The rows differ along the form the request named the function in, along whether it also
    /// named a version, and along what a value this service cannot read leaves.
    #[test]
    fn a_delivered_record_names_the_function_by_its_own_arn() {
        let unqualified = format!("arn:aws:lambda:{REGION}:000000000000:function:fn");
        let rows: Vec<(&str, &str, ::serde_json::Value, ::serde_json::Value)> = vec![
            (
                "a bare function name",
                "Invoke",
                ::serde_json::json!({"functionName": "fn"}),
                ::serde_json::json!({"functionName": unqualified.clone()}),
            ),
            (
                "a partial ARN",
                "Invoke",
                ::serde_json::json!({"functionName": "000000000000:function:fn"}),
                ::serde_json::json!({"functionName": unqualified.clone()}),
            ),
            (
                "a name carrying a published version",
                "Invoke",
                ::serde_json::json!({"functionName": "fn:7"}),
                ::serde_json::json!({"functionName": unqualified.clone()}),
            ),
            (
                "the function's own ARN, which is left as it is",
                "Invoke",
                ::serde_json::json!({"functionName": unqualified.clone()}),
                ::serde_json::json!({"functionName": unqualified.clone()}),
            ),
            (
                "a value naming no function this service can read",
                "Invoke",
                ::serde_json::json!({"functionName": "arn:aws:s3:::bucket"}),
                ::serde_json::json!({"functionName": "arn:aws:s3:::bucket"}),
            ),
            (
                "a control-plane call, which names no function data event",
                "GetFunction",
                ::serde_json::json!({"functionName": "fn"}),
                ::serde_json::json!({"functionName": "fn"}),
            ),
        ];
        for (case, operation, mut request_parameters, expected) in rows {
            naming_the_function(operation, REGION, &mut request_parameters);
            assert_eq!(
                request_parameters, expected,
                "case {case:?}: expected {expected}, got {request_parameters}"
            );
        }
    }

    /// RULE: the operations recorded as data events are exactly the catalog, and every one of
    /// them is a WRITE — an invocation runs the function.
    #[test]
    fn only_the_cataloged_invocations_are_data_events() {
        for (operation, kind) in DATA_EVENT_OPERATIONS {
            assert!(
                is_data_event(operation),
                "case {operation:?}: a cataloged operation is a data event"
            );
            assert_eq!(
                *kind,
                CallKind::Mutating,
                "case {operation:?}: an invocation runs the function, so it is a write"
            );
        }
        for operation in [
            "CreateFunction",
            "GetFunction",
            "ListFunctions",
            "UpdateFunctionCode",
            "PublishVersion",
        ] {
            assert!(
                !is_data_event(operation),
                "case {operation:?}: a control-plane call is no data event"
            );
        }
    }
}
