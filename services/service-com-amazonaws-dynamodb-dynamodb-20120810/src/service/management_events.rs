// SPDX-License-Identifier: CC0-1.0

//! This front door's AUDIT leg: every control-plane call it serves is a management event of
//! `dynamodb.amazonaws.com`, recorded in the account's event history where CloudTrail's
//! `LookupEvents` reads it.
//!
//! Every control-plane call of an account is a management event, which makes the recording
//! a property of SERVING a call and not of any one operation — so it is not written per
//! handler. The shared wrapper ([`service_common::audited_operation`]) is handed the
//! deserialized input before the handler runs and the handler's own `Result` after, and
//! this module is only what THIS service contributes to each record:
//!
//! * its `eventSource` ([`EVENT_SOURCE`]);
//! * the Region the call was served in and the account it was served for;
//! * the session a call's temporary credential belongs to, so a call made with a role
//!   session is attributed to that session and to the role that issued it;
//! * the FORM the id a record is joined to its caller's own response by is spelled in,
//!   which is this service's own ([`super::request_id`]);
//! * which operations REPORT state and which CHANGE it — the one fact a record cannot read
//!   off a request, so it is stated per operation in [`audited_table`];
//! * and which of its calls are DATA events rather than management events, stated in that
//!   same table ([`super::data_events`]).
//!
//! Every request this service serves reaches that table, because it is the routing table
//! this service's definition answers with
//! ([`service_common::audited_operation::AuditedDefinition`], nested inside this service's
//! authentication-refusal and request-id wrappers) — the same table this service's own
//! dispatch resolves each request through (`super::EmulatedService::override_operation`),
//! so no request can be served by a path that records nothing.
//!
//! Two kinds of answer are NOT records, both for the shared wrapper's own reason — event
//! history is the record of the calls an account's APIs actually SERVED:
//!
//! * a call the scaffold refused as not implemented (answered 501), which reached no
//!   handler;
//! * a request refused before any handler ran — a body that does not deserialize, a member
//!   outside a modelled constraint, an authentication refusal — which is answered without
//!   the typed call a record is built from ever happening.

use std::sync::Arc;

use aws_smithy_runtime_api::http::Response as RtResponse;
use service_common::audited_operation::{AuditedService, AuditedTable, CallKind};
use service_common::management_events::{
    AuditedResource, ManagementEventLog, RecordedSession, ServedApiCall, SessionAuditDirectory,
};
use service_common::quoted_refusals::QuotedRefusals;
use smithy_server_runtime::{OperationContext, OperationRoutingTable, PreDispatchProtocol};

/// This service's model-derived half, routing through the AUDITED operations — the shared
/// wrapper every recording service uses, re-exported under the name this crate holds it by.
pub use service_common::audited_operation::AuditedDefinition;

/// The `eventSource` every record of this service carries: this service's endpoint prefix
/// (`smithy_query(query="service_info",model_path="services/service-com-amazonaws-dynamodb-dynamodb-20120810/smithy-model/com-amazonaws-dynamodb-dynamodb-20120810.json")`
/// -> `endpointPrefix: dynamodb`) in the `<prefix>.amazonaws.com` form CloudTrail stamps on
/// every event.
pub const EVENT_SOURCE: &str = "dynamodb.amazonaws.com";

/// What this service contributes to the record of each call it serves.
///
/// Holds the shared engine — the record goes into the log that engine carries, the one
/// CloudTrail reads — so it is generic over the engine type exactly as the service is.
pub struct DynamoDbAudit<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> DynamoDbAudit<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The account's log in that engine, or `None` for an engine that carries none.
    fn log(&self) -> Option<::engine::ServiceState<ManagementEventLog>> {
        management_event_log(&*self.engine)
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for DynamoDbAudit<Eng> {
    fn record(
        &self,
        call: ServedApiCall,
        answered_as: &str,
    ) -> Option<::service_common::api_call_events::DeliveredApiCall> {
        let Some(log) = self.log() else {
            // Not silent: an unrecorded call leaves no trace in the log by definition, so
            // it leaves one here instead.
            ::tracing::warn!(
                operation = call.event_name,
                "the engine carries no management-event log: the call was served unrecorded"
            );
            return None;
        };
        // The record, and the event the account's CloudTrail fans out for it while a trail
        // of the Region is logging calls of its kind; publishing it is the wrapper's.
        // The guard is BOUND rather than used as a temporary: a local is released before
        // the handle it borrows, which a temporary at the end of this block would not be.
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

    fn region(&self, ctx: &OperationContext<'_>) -> String {
        // The Region the call was served in, read off the request the way every handler of
        // this service reads it: a DynamoDB endpoint serves one Region, and a table name is
        // unique within it, so a record belongs to the Region its call was signed for.
        crate::state::request_region(ctx.parts())
    }

    fn account_id(&self) -> String {
        ::engine::emulator_account_id().to_string()
    }

    fn recorded_session(&self, access_key_id: &str) -> Option<RecordedSession> {
        // The account's own session directory, so a call made with a role session is
        // recorded as that session — and as the role that issued it: the shared store the
        // minting service wrote it to.
        session_audit_directory(&*self.engine)
            .and_then(|directory| directory.lock().session(access_key_id).cloned())
    }

    fn resources(
        &self,
        operation: &str,
        request_parameters: &serde_json::Value,
        region: &str,
    ) -> Vec<AuditedResource> {
        // A MANAGEMENT call of this service names no resource: the pairs CloudTrail reports
        // in an event's `Resources` are what a lookup by `ResourceName` filters on, and a
        // record naming a resource nothing resolved would be reported as fact — so a
        // management record carries the call, its parameters (the table the request
        // addressed among them) and its outcome. A DATA event is the one record that MUST
        // name one, because a trail's selector is what selects it by resource type and ARN
        // ([`super::data_events`]).
        super::data_events::data_event_resources(operation, request_parameters, region)
    }

    fn sensitive_members(&self) -> &'static [&'static str] {
        // Nothing is withheld: this model declares no `@sensitive` member — no shape and no
        // member of it carries the trait — so no value of a call of this service is
        // replaced by the redaction literal.
        &[]
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        // This service spells its request ids its own way ([`super::request_id`]), so the
        // record is joined to the caller's response through THAT form's reader: the shared
        // one reports only the hyphenated-UUID form, and would leave a record of this
        // service's call naming a request no caller was ever answered under.
        super::request_id::named_request(resp)
    }
}

/// The account's management-event log in the shared engine.
///
/// A SHARED type ([`service_common::management_events::ManagementEventLog`]) and not a
/// field of this service's own state: CloudTrail reports the management calls of every
/// service, and the service that served a call is the only one that knows it happened, so
/// recorder and reporter reach one log rather than two copies of it.
pub fn management_event_log<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ManagementEventLog>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ManagementEventLog>)
}

/// The account's SESSION directory in that same shared engine: the sessions the token
/// service minted, each keyed by the access key id it was minted under, which is what lets
/// a record of a call made with one name the session it was made by.
///
/// A SHARED type for the same reason the log is: the service that MINTS a session and the
/// services that record the calls made WITH it are not the same service.
pub fn session_audit_directory<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<SessionAuditDirectory>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<SessionAuditDirectory>)
}

/// The envelope a pre-dispatch failure of one of this service's operations renders in:
/// this service speaks `awsJson1_0` (`smithy_query(query="digest")` reports the one
/// protocol `awsJson1_0`), whose errors carry the fault shape in the body's `__type`
/// rather than in a header — the same value the generated per-operation shim passes, so a
/// refusal raised ahead of a handler reads exactly as it did before.
pub const JSON_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Json {
    content_type: "application/x-amz-json-1.0",
    // The awsJson families convey the error type in the body's `__type`, not in a header.
    errortype_header: false,
};

/// Serve one operation of this service audited, as a call of the stated kind: `reports`
/// for an operation that only REPORTS state, `mutates` for one that CHANGES the account's
/// resources — which is the event's `readOnly`, and what a lookup can filter on.
///
/// The kind per row names the shared table's own method for it
/// ([`AuditedTable::reports`] / [`AuditedTable::mutates`]), so nothing about what a kind
/// means is restated here.
macro_rules! audited {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident => $kind:ident ),* $(,)?
    ) => {{
        $(
            $table.$kind(
                crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
            );
        )*
    }};
}

/// Serve one operation of this service as a DATA event of the table it acts on, as a call
/// of the stated kind — the record built from the typed call exactly as a management
/// event's is, and delivered to the account's trails instead of recorded in event history
/// ([`super::data_events`]).
macro_rules! data_plane {
    (
        $table:expr, $engine:expr, $delivery:expr,
        $( $module:ident :: $operation:ident => $kind:expr ),* $(,)?
    ) => {{
        $(
            $table.serves_data_event(
                crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                $kind,
                $delivery,
            );
        )*
    }};
}

/// Serve one operation audited AND behind the wrapper that restores the value half of a
/// constraint clause ([`QuotedRefusals`]), as a call of the stated kind.
///
/// WHICH operations: the ones whose whole selector is a member the model declares LONGER
/// than the ceiling the framework's own walk elides an echoed value at — an ARN
/// (`smithy_query(query="shape", shape="com.amazonaws.dynamodb#ResourceArnString")` gives
/// `@length` 1..1283, where that walk elides past 256 bytes). AWS's clause quotes the value
/// the request carried however long it was, so a refusal of such a member read back the
/// walk's `<prefix>... [<n> bytes]` instead of the ARN the caller has to correct. The shared
/// wrapper re-reads the request through the framework's OWN deserializer and walk and hands
/// the violations to this service's own wording, so only that value half changes.
///
/// The audited call stays INSIDE the wrapper ([`AuditedTable::serve_behind`]), so a record
/// still reports the operation's own input and outcome.
macro_rules! quoting_refused_values {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident => $kind:ident ),* $(,)?
    ) => {{
        $(
            $table.serve_behind(
                crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                CallKind::$kind,
                |audited, _| {
                    QuotedRefusals::over::<crate::ops::$module::$operation<Eng>>(audited)
                },
            );
        )*
    }};
}

/// Every modelled operation of this service, each constructed from the shared engine and
/// served behind the audit recorder.
///
/// EVERY one is here — the 58 operations `smithy_query(query="digest")` lists — the ones
/// this emulator has not implemented included: a stub answers 501 and the wrapper records
/// nothing for it, so an operation implemented later is auditable the moment it serves a
/// call rather than when someone remembers to add it.
///
/// The CLASS per row is CloudTrail's own: the control plane of an account's tables, backups,
/// exports, imports, replicas, policies and tags is a MANAGEMENT event, while the calls on
/// the ITEMS a table holds are DATA events — recorded in no event history and delivered
/// only into the bucket of a trail whose selectors admit them ([`super::data_events`], which
/// states that set once and is what this table reads it from).
///
/// The kind per row is the operation's own documented semantics: `reports` for the
/// `Describe*` / `List*` / `Get*` readers, which "return information about" a resource and
/// change none of it; `mutates` for every operation that creates, changes, restores,
/// exports, imports or removes something the account then holds — `ExportTableToPointInTime`
/// and `ImportTable` included, since each one submits work whose record the account keeps,
/// and `UpdateContributorInsights` and the tagging pair included, since each changes a
/// resource's configuration.
pub fn audited_table<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> OperationRoutingTable {
    let recorder = Arc::new(DynamoDbAudit::new(Arc::clone(engine)));
    let mut table = AuditedTable::new(recorder, JSON_ENVELOPE);
    audited!(
        table, engine,
        create_backup::CreateBackup => mutates,
        create_global_table::CreateGlobalTable => mutates,
        create_table::CreateTable => mutates,
        delete_backup::DeleteBackup => mutates,
        delete_table::DeleteTable => mutates,
        describe_backup::DescribeBackup => reports,
        describe_continuous_backups::DescribeContinuousBackups => reports,
        describe_contributor_insights::DescribeContributorInsights => reports,
        describe_endpoints::DescribeEndpoints => reports,
        describe_export::DescribeExport => reports,
        describe_global_table::DescribeGlobalTable => reports,
        describe_global_table_settings::DescribeGlobalTableSettings => reports,
        describe_import::DescribeImport => reports,
        describe_kinesis_streaming_destination::DescribeKinesisStreamingDestination => reports,
        describe_limits::DescribeLimits => reports,
        describe_table::DescribeTable => reports,
        describe_table_replica_auto_scaling::DescribeTableReplicaAutoScaling => reports,
        describe_time_to_live::DescribeTimeToLive => reports,
        disable_kinesis_streaming_destination::DisableKinesisStreamingDestination => mutates,
        enable_kinesis_streaming_destination::EnableKinesisStreamingDestination => mutates,
        export_table_to_point_in_time::ExportTableToPointInTime => mutates,
        import_table::ImportTable => mutates,
        list_backups::ListBackups => reports,
        list_bulk_operations::ListBulkOperations => reports,
        list_contributor_insights::ListContributorInsights => reports,
        list_exports::ListExports => reports,
        list_global_tables::ListGlobalTables => reports,
        list_imports::ListImports => reports,
        list_tables::ListTables => reports,
        restore_table_from_backup::RestoreTableFromBackup => mutates,
        restore_table_to_point_in_time::RestoreTableToPointInTime => mutates,
        update_continuous_backups::UpdateContinuousBackups => mutates,
        update_contributor_insights::UpdateContributorInsights => mutates,
        update_global_table::UpdateGlobalTable => mutates,
        update_global_table_settings::UpdateGlobalTableSettings => mutates,
        update_kinesis_streaming_destination::UpdateKinesisStreamingDestination => mutates,
        update_table::UpdateTable => mutates,
        update_table_replica_auto_scaling::UpdateTableReplicaAutoScaling => mutates,
        update_time_to_live::UpdateTimeToLive => mutates,
    );
    // The ARN-ADDRESSED surfaces, whose whole selector is a member the model declares
    // longer than the framework's own echo ceiling: their refusals quote the value the
    // request carried ([`quoting_refused_values`]).
    quoting_refused_values!(
        table, engine,
        delete_resource_policy::DeleteResourcePolicy => Mutating,
        get_resource_policy::GetResourcePolicy => Reporting,
        list_tags_of_resource::ListTagsOfResource => Reporting,
        put_resource_policy::PutResourcePolicy => Mutating,
        tag_resource::TagResource => Mutating,
        untag_resource::UntagResource => Mutating,
    );
    // This service's DATA plane: the calls on the items a table holds, which CloudTrail
    // records as data events of that table rather than as management events of the account.
    // The read/write kind is the call's own, exactly as above — what differs is where the
    // record goes (see [`super::data_events`]).
    let data_events = Arc::new(super::data_events::TableDataEvents::new(Arc::clone(
        engine,
    )));
    data_plane!(
        table, engine, &data_events,
        batch_execute_statement::BatchExecuteStatement => CallKind::Mutating,
        batch_get_item::BatchGetItem => CallKind::Reporting,
        batch_write_item::BatchWriteItem => CallKind::Mutating,
        delete_item::DeleteItem => CallKind::Mutating,
        execute_statement::ExecuteStatement => CallKind::Mutating,
        execute_transaction::ExecuteTransaction => CallKind::Mutating,
        get_item::GetItem => CallKind::Reporting,
        put_item::PutItem => CallKind::Mutating,
        query::Query => CallKind::Reporting,
        scan::Scan => CallKind::Reporting,
        search_vectors::SearchVectors => CallKind::Reporting,
        start_bulk_put_items::StartBulkPutItems => CallKind::Mutating,
        transact_get_items::TransactGetItems => CallKind::Reporting,
        transact_write_items::TransactWriteItems => CallKind::Mutating,
        update_item::UpdateItem => CallKind::Mutating,
    );
    table.finish()
}
