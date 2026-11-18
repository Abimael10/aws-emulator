// SPDX-License-Identifier: CC0-1.0

//! This front door's MANAGEMENT leg: every call it serves that ADMINISTERS the account's
//! functions is a management event of `lambda.amazonaws.com`, recorded in the account's
//! event history where CloudTrail's `LookupEvents` reads it.
//!
//! Every control-plane API call of an account is a management event — "event history"
//! holds the management calls of every service whether or not a trail exists — which makes
//! the recording a property of SERVING a call and not of any one operation. So it is not
//! written per handler: the shared wrapper ([`service_common::audited_operation`]) is
//! handed the deserialized input before the handler runs, the handler's own `Result` after,
//! and the rendered response last, and this module is only what THIS service contributes to
//! each record:
//!
//! * its `eventSource` (the modelled [`EVENT_SOURCE`], shared with the DATA leg);
//! * the `eventName` CloudTrail PUBLISHES the operation's calls under, which for this
//!   service is not the operation's shape name ([`published_event_name`]);
//! * the Region and the account the call was served for;
//! * the members whose values its records WITHHOLD ([`OMITTED_MEMBERS`]); and
//! * which operations REPORT state and which CHANGE it — the one fact a record cannot read
//!   off a request, so it is stated per operation in [`MANAGEMENT_EVENT_OPERATIONS`].
//!
//! Every modelled operation this service serves reaches the table this module's recorder
//! is wired into ([`recording_table`], filled where `EmulatedService::new` lists the whole
//! operation surface), because that table is what `Service::override_operation` answers
//! with — so no control-plane request can be served by a path that records nothing. The
//! INVOCATIONS are the exception, and not an omission: a call that runs a function acts on
//! that function's contents, which CloudTrail records as a DATA event and never as event
//! history (see [`super::data_events`], which files those two rows itself).
//!
//! A call this emulator has not implemented answers 501 and is NOT recorded: the wrapper
//! drops it, because event history is the record of the calls an account's APIs actually
//! served.

use std::sync::Arc;

use aws_smithy_runtime_api::http::Response as RtResponse;
use service_common::audited_operation::{AuditedService, AuditedTable, CallKind};
use service_common::management_events::{RecordedSession, ServedApiCall};
use smithy_server_runtime::OperationContext;

/// The `eventSource` every record of this service carries, stated once beside the DATA leg
/// that shares it: the modelled `awsService.cloudTrailEventSource`.
pub use super::data_events::EVENT_SOURCE;

/// The members a record of a control-plane call carries the redaction literal in place of
/// the value of: none. What this service withholds, it withholds WHOLE
/// ([`OMITTED_MEMBERS`]).
const SENSITIVE_MEMBERS: &[&str] = &[];

/// The members a record of this service leaves OUT entirely — no key for them in
/// `requestParameters` / `responseElements` at all, as against a key carrying CloudTrail's
/// redaction literal. The set is by member NAME, so it holds for every operation that
/// carries a member of that name: the environment a create declares and the one a read
/// reports are both left out.
///
/// * `Variables` is a function's environment (`Environment$Variables` /
///   `EnvironmentResponse$Variables`, whose own documentation says "Omitted from CloudTrail
///   logs"), a map this model declares `@sensitive` key and value alike.
/// * `ZipFile` is the deployment package a create or an update uploaded, and the archive a
///   layer version published (`FunctionCode$ZipFile`, `LayerVersionContentInput$ZipFile`,
///   both `@sensitive`): CloudTrail records the CALL made on a resource rather than the
///   artifact the call carried, which is also what keeps one record's footprint independent
///   of the package's size.
pub const OMITTED_MEMBERS: &[&str] = &["Variables", "ZipFile"];

/// The marker CloudTrail appends to the published name of an operation whose REST binding
/// is the SECOND revision of that operation at the same API date.
///
/// AWS Lambda revised a handful of its original `2015-03-31` operations in place rather
/// than under a new API date, and CloudTrail publishes those calls under the revised name:
/// `GetFunction20150331v2`, not `GetFunction20150331`. The marker is stated once here and
/// the operations that carry it in [`REVISED_OPERATIONS`].
const REVISION_MARKER: &str = "v2";

/// The operations whose published name carries [`REVISION_MARKER`].
///
/// A hand-maintained set, because the revision is CloudTrail's own fact and no trait of the
/// model records it: these are the `2015-03-31` operations AWS reports in event history
/// with the `v2` marker (as the record of a function read, `GetFunction20150331v2`, shows).
/// Every other operation of this service publishes its name with its API date alone.
const REVISED_OPERATIONS: &[&str] = &[
    "AddPermission",
    "GetFunction",
    "GetFunctionConfiguration",
    "GetPolicy",
    "RemovePermission",
    "UpdateFunctionCode",
    "UpdateFunctionConfiguration",
];

/// Every operation of this service CloudTrail records as a MANAGEMENT event: its modelled
/// name, the read/write class of its calls, and the API DATE its published name carries.
///
/// The CLASS is the operation's own semantics, which is what the event's `readOnly` carries
/// and what a trail's `ReadWriteType` narrows a trail's management events by: `Reporting`
/// for the `Get`/`List` readers, which report state and change none of it (each of them
/// `@readonly` in the model), `Mutating` for every call that creates, changes, publishes,
/// tags or removes something.
///
/// The DATE is the API version the operation's own HTTP binding is under, with the dashes
/// dropped: `GetFunction` is bound to `GET /2015-03-31/functions/{FunctionName}`, so its
/// records are published under `20150331` (plus the revision marker, see
/// [`REVISED_OPERATIONS`]). It is in the MODEL, which is why
/// `tests/all/management_event_recording.rs` holds every row of this table against that
/// model's own `@http` URI rather than against a second hand-written list.
///
/// The three INVOCATION operations are absent on purpose: `Invoke` and `InvokeAsync` are
/// data events of the function they run (see [`super::data_events`]) and
/// `InvokeWithResponseStream` implements no `smithy_server_runtime::Operation` at all, so
/// there is no typed call to build a record from.
///
/// Public, so this crate's own tests read the catalog the routing table is built from
/// rather than a second copy of it.
pub const MANAGEMENT_EVENT_OPERATIONS: &[(&str, CallKind, &str)] = &[
    ("AddLayerVersionPermission", CallKind::Mutating, "20181031"),
    ("AddPermission", CallKind::Mutating, "20150331"),
    ("CheckpointDurableExecution", CallKind::Mutating, "20251201"),
    ("CreateAlias", CallKind::Mutating, "20150331"),
    ("CreateCapacityProvider", CallKind::Mutating, "20251130"),
    ("CreateCodeSigningConfig", CallKind::Mutating, "20200422"),
    ("CreateEventSourceMapping", CallKind::Mutating, "20150331"),
    ("CreateFunction", CallKind::Mutating, "20150331"),
    ("CreateFunctionUrlConfig", CallKind::Mutating, "20211031"),
    ("DeleteAlias", CallKind::Mutating, "20150331"),
    ("DeleteCapacityProvider", CallKind::Mutating, "20251130"),
    ("DeleteCodeSigningConfig", CallKind::Mutating, "20200422"),
    ("DeleteEventSourceMapping", CallKind::Mutating, "20150331"),
    ("DeleteFunction", CallKind::Mutating, "20150331"),
    (
        "DeleteFunctionCodeSigningConfig",
        CallKind::Mutating,
        "20200630",
    ),
    ("DeleteFunctionConcurrency", CallKind::Mutating, "20171031"),
    (
        "DeleteFunctionEventInvokeConfig",
        CallKind::Mutating,
        "20190925",
    ),
    ("DeleteFunctionUrlConfig", CallKind::Mutating, "20211031"),
    ("DeleteLayerVersion", CallKind::Mutating, "20181031"),
    (
        "DeleteProvisionedConcurrencyConfig",
        CallKind::Mutating,
        "20190930",
    ),
    ("DeleteResourcePolicy", CallKind::Mutating, "20260709"),
    ("GetAccountSettings", CallKind::Reporting, "20160819"),
    ("GetAlias", CallKind::Reporting, "20150331"),
    ("GetCapacityProvider", CallKind::Reporting, "20251130"),
    ("GetCodeSigningConfig", CallKind::Reporting, "20200422"),
    ("GetDurableExecution", CallKind::Reporting, "20251201"),
    ("GetDurableExecutionHistory", CallKind::Reporting, "20251201"),
    ("GetDurableExecutionState", CallKind::Reporting, "20251201"),
    ("GetEventSourceMapping", CallKind::Reporting, "20150331"),
    ("GetFunction", CallKind::Reporting, "20150331"),
    (
        "GetFunctionCodeSigningConfig",
        CallKind::Reporting,
        "20200630",
    ),
    ("GetFunctionConcurrency", CallKind::Reporting, "20190930"),
    ("GetFunctionConfiguration", CallKind::Reporting, "20150331"),
    (
        "GetFunctionEventInvokeConfig",
        CallKind::Reporting,
        "20190925",
    ),
    ("GetFunctionRecursionConfig", CallKind::Reporting, "20240831"),
    ("GetFunctionScalingConfig", CallKind::Reporting, "20251130"),
    ("GetFunctionUrlConfig", CallKind::Reporting, "20211031"),
    ("GetLayerVersion", CallKind::Reporting, "20181031"),
    ("GetLayerVersionByArn", CallKind::Reporting, "20181031"),
    ("GetLayerVersionPolicy", CallKind::Reporting, "20181031"),
    ("GetPolicy", CallKind::Reporting, "20150331"),
    (
        "GetProvisionedConcurrencyConfig",
        CallKind::Reporting,
        "20190930",
    ),
    ("GetResourcePolicy", CallKind::Reporting, "20260709"),
    ("GetRuntimeManagementConfig", CallKind::Reporting, "20210720"),
    ("ListAliases", CallKind::Reporting, "20150331"),
    ("ListCapacityProviders", CallKind::Reporting, "20251130"),
    ("ListCodeSigningConfigs", CallKind::Reporting, "20200422"),
    (
        "ListDurableExecutionsByFunction",
        CallKind::Reporting,
        "20251201",
    ),
    ("ListEventSourceMappings", CallKind::Reporting, "20150331"),
    (
        "ListFunctionEventInvokeConfigs",
        CallKind::Reporting,
        "20190925",
    ),
    ("ListFunctionUrlConfigs", CallKind::Reporting, "20211031"),
    (
        "ListFunctionVersionsByCapacityProvider",
        CallKind::Reporting,
        "20251130",
    ),
    ("ListFunctions", CallKind::Reporting, "20150331"),
    (
        "ListFunctionsByCodeSigningConfig",
        CallKind::Reporting,
        "20200422",
    ),
    ("ListLayerVersions", CallKind::Reporting, "20181031"),
    ("ListLayers", CallKind::Reporting, "20181031"),
    (
        "ListProvisionedConcurrencyConfigs",
        CallKind::Reporting,
        "20190930",
    ),
    ("ListTags", CallKind::Reporting, "20170331"),
    ("ListVersionsByFunction", CallKind::Reporting, "20150331"),
    ("PublishLayerVersion", CallKind::Mutating, "20181031"),
    ("PublishVersion", CallKind::Mutating, "20150331"),
    (
        "PutFunctionCodeSigningConfig",
        CallKind::Mutating,
        "20200630",
    ),
    ("PutFunctionConcurrency", CallKind::Mutating, "20171031"),
    (
        "PutFunctionEventInvokeConfig",
        CallKind::Mutating,
        "20190925",
    ),
    ("PutFunctionRecursionConfig", CallKind::Mutating, "20240831"),
    ("PutFunctionScalingConfig", CallKind::Mutating, "20251130"),
    (
        "PutProvisionedConcurrencyConfig",
        CallKind::Mutating,
        "20190930",
    ),
    ("PutResourcePolicy", CallKind::Mutating, "20260709"),
    ("PutRuntimeManagementConfig", CallKind::Mutating, "20210720"),
    (
        "RemoveLayerVersionPermission",
        CallKind::Mutating,
        "20181031",
    ),
    ("RemovePermission", CallKind::Mutating, "20150331"),
    (
        "SendDurableExecutionCallbackFailure",
        CallKind::Mutating,
        "20251201",
    ),
    (
        "SendDurableExecutionCallbackHeartbeat",
        CallKind::Mutating,
        "20251201",
    ),
    (
        "SendDurableExecutionCallbackSuccess",
        CallKind::Mutating,
        "20251201",
    ),
    ("StopDurableExecution", CallKind::Mutating, "20251201"),
    ("TagResource", CallKind::Mutating, "20170331"),
    ("UntagResource", CallKind::Mutating, "20170331"),
    ("UpdateAlias", CallKind::Mutating, "20150331"),
    ("UpdateCapacityProvider", CallKind::Mutating, "20251130"),
    ("UpdateCodeSigningConfig", CallKind::Mutating, "20200422"),
    ("UpdateEventSourceMapping", CallKind::Mutating, "20150331"),
    ("UpdateFunctionCode", CallKind::Mutating, "20150331"),
    ("UpdateFunctionConfiguration", CallKind::Mutating, "20150331"),
    (
        "UpdateFunctionEventInvokeConfig",
        CallKind::Mutating,
        "20190925",
    ),
    ("UpdateFunctionUrlConfig", CallKind::Mutating, "20211031"),
];

/// The read/write class CloudTrail records the calls of `operation` as a management event
/// with, or `None` for an operation whose calls are no management event of this service.
#[must_use]
pub fn recorded_class(operation: &str) -> Option<CallKind> {
    row(operation).map(|(_, kind, _)| *kind)
}

/// The `eventName` CloudTrail publishes the calls of `operation` under: the operation's own
/// name, its API date, and the revision marker when that operation's binding carries one —
/// so a client filtering a lookup by `EventName` names the same string AWS does.
///
/// An operation this service records no management event for keeps its modelled name: there
/// is no published management-event name for a call that is not one.
#[must_use]
pub fn published_event_name(operation: &str) -> String {
    let Some((_, _, api_date)) = row(operation) else {
        return operation.to_string();
    };
    let marker = if REVISED_OPERATIONS.contains(&operation) {
        REVISION_MARKER
    } else {
        ""
    };
    format!("{operation}{api_date}{marker}")
}

/// The catalog row of `operation`, or `None` when it holds none.
fn row(operation: &str) -> Option<&'static (&'static str, CallKind, &'static str)> {
    MANAGEMENT_EVENT_OPERATIONS
        .iter()
        .find(|(named, _, _)| *named == operation)
}

/// What this service contributes to the record of each control-plane call it serves.
///
/// Holds the shared engine — the record goes into the log that engine carries, the one
/// CloudTrail's `LookupEvents` reads — so it is generic over the engine type exactly as the
/// service is.
pub struct FunctionManagementEvents<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> FunctionManagementEvents<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for FunctionManagementEvents<Eng> {
    fn record(
        &self,
        call: ServedApiCall,
        answered_as: &str,
    ) -> Option<::service_common::api_call_events::DeliveredApiCall> {
        let Some(log) = super::data_events::management_event_log(&*self.engine) else {
            // Not silent: an unrecorded call leaves no trace in event history by
            // definition, so it leaves one here instead.
            ::tracing::warn!(
                operation = call.event_name,
                "the engine carries no management-event log: the call was served unrecorded"
            );
            return None;
        };
        // The record, and the event the account's CloudTrail fans out for it while a trail
        // of the Region is logging calls of its kind; publishing it is the wrapper's. The
        // guard is BOUND rather than used as a temporary: a local is released before the
        // handle it borrows, which a temporary at the end of this block would not be.
        let mut recording = log.lock();
        recording.record_delivered(
            call,
            ::std::time::SystemTime::now(),
            answered_as.to_string(),
        )
    }

    fn event_source(&self) -> &'static str {
        EVENT_SOURCE
    }

    fn event_name(&self, operation: &'static str) -> String {
        // The name CloudTrail publishes this operation's calls under, which for this
        // service is the operation name plus its API version (see
        // [`published_event_name`]).
        published_event_name(operation)
    }

    fn region(&self, ctx: &OperationContext<'_>) -> String {
        // The Region the call was served in, read off the request the way every handler of
        // this service reads it.
        crate::state::request_region(ctx)
    }

    fn account_id(&self) -> String {
        ::engine::emulator_account_id().to_string()
    }

    // `resources` is the shared recorder's default: none. The pairs CloudTrail reports in
    // an event's `Resources` are the DATA leg's own concern here — a data event is selected
    // by the resource it acted on (see [`super::data_events`]) — while a management record
    // of this service carries the call, its parameters and its outcome.

    fn sensitive_members(&self) -> &'static [&'static str] {
        SENSITIVE_MEMBERS
    }

    fn omitted_members(&self) -> &'static [&'static str] {
        OMITTED_MEMBERS
    }

    fn recorded_session(&self, access_key_id: &str) -> Option<RecordedSession> {
        // The account's own session directory, so a call made with a role session is
        // recorded as that session — and as the role that issued it: the shared store the
        // minting service wrote it to, read the one way this service reads it.
        super::data_events::session_audit_directory(&*self.engine)
            .and_then(|directory| directory.lock().session(access_key_id).cloned())
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        // Every answer of this service already names the request it answers, in the header
        // an SDK reads its response metadata's `RequestId` from (see
        // [`super::request_id_header`]), so the id is READ out of that header and the
        // response handed back untouched: the record names the request the CALLER was
        // answered under.
        ::service_common::query_error_envelope::named_request_as(resp, crate::state::mint_request_id)
    }
}

/// The empty AUDIT table this service's operations are served through, recording into the
/// account's management-event log.
///
/// The ROWS are filed where the whole operation surface of this service is already
/// listed — `EmulatedService::new`, through its own `wrap_operations!` — because every row
/// carries this service's per-operation wrapper as well, and one list cannot drift from
/// itself. What lives here is the recorder each of those rows records through and the
/// envelope their pre-dispatch failures render in.
pub fn recording_table<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
) -> AuditedTable<FunctionManagementEvents<Eng>> {
    let recorder = Arc::new(FunctionManagementEvents::new(Arc::clone(engine)));
    AuditedTable::new(recorder, super::data_events::JSON_ENVELOPE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the published `eventName` of a management call is the operation's name, the
    /// API date of its own HTTP binding, and the revision marker for an operation that
    /// carries one; an operation this service records no management event for keeps its
    /// modelled name.
    ///
    /// The rows span the API versions this service binds and both halves of the rule — a
    /// revised operation and an unrevised one of the SAME date, the oldest and the newest
    /// date in the catalog, a read and a write — rather than the one read that motivated
    /// it. The dates themselves are held against the model where the whole catalog is
    /// (`tests/all/management_event_recording.rs`).
    #[test]
    fn a_published_event_name_carries_the_operations_api_version() {
        let rows = [
            ("a revised read of a function", "GetFunction", "GetFunction20150331v2"),
            (
                "a revised write on a function's policy",
                "AddPermission",
                "AddPermission20150331v2",
            ),
            (
                "an unrevised write of the same API date",
                "CreateFunction",
                "CreateFunction20150331",
            ),
            (
                "an unrevised read of the same API date",
                "ListFunctions",
                "ListFunctions20150331",
            ),
            (
                "an operation under an API date of its own",
                "GetAccountSettings",
                "GetAccountSettings20160819",
            ),
            (
                "a layer write",
                "PublishLayerVersion",
                "PublishLayerVersion20181031",
            ),
            ("a tagging write", "TagResource", "TagResource20170331"),
            (
                "the newest API date this service binds",
                "GetResourcePolicy",
                "GetResourcePolicy20260709",
            ),
            ("an invocation, which is a data event", "Invoke", "Invoke"),
            (
                "the asynchronous invocation",
                "InvokeAsync",
                "InvokeAsync",
            ),
        ];
        for (case, operation, expected) in rows {
            let actual = published_event_name(operation);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the published event name {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the operations recorded as management events are exactly the catalog, and the
    /// invocations are not among them — those are data events of the function they run.
    #[test]
    fn only_the_cataloged_operations_are_management_events() {
        for (operation, kind, _) in MANAGEMENT_EVENT_OPERATIONS {
            assert_eq!(
                recorded_class(operation),
                Some(*kind),
                "case {operation:?}: a cataloged operation is recorded with its own class"
            );
            assert!(
                !crate::service::data_events::is_data_event(operation),
                "case {operation:?}: a management event is no data event"
            );
        }
        for operation in ["Invoke", "InvokeAsync", "InvokeWithResponseStream"] {
            assert_eq!(
                recorded_class(operation),
                None,
                "case {operation:?}: an invocation is no management event of this service"
            );
        }
    }
}
