// SPDX-License-Identifier: CC0-1.0

//! This front door's AUDIT leg: every control-plane call it serves is a management event of
//! `logs.amazonaws.com`, recorded in the account's event history where CloudTrail's
//! `LookupEvents` reads it.
//!
//! Every control-plane call of an account is a management event — read-only calls and
//! REFUSED calls included — which makes the recording a property of SERVING a call and not
//! of any one operation. So it is not written per handler: an operation that records its own
//! call records the same facts as its neighbour, and an operation nobody remembered to edit
//! records nothing at all. The shared wrapper ([`service_common::audited_operation`]) is
//! handed the deserialized input before the handler runs, the handler's own `Result` after,
//! and the rendered response last; this module is only what THIS service contributes to each
//! record:
//!
//! * its `eventSource` ([`EVENT_SOURCE`]);
//! * the Region and the account the call was served for;
//! * the log group or log stream a call addressed ([`LogsAudit::resources`]);
//! * the ONE id a response of this endpoint names its request by
//!   ([`LogsAudit::names_the_request`], the same reading [`super::stated_request_id`] does),
//!   which is the record's `requestID`; and
//! * which operations REPORT state and which CHANGE it — the one fact a record cannot read
//!   off a request, so it is stated per operation in [`audited_table`].
//!
//! Every request this service serves reaches that table, because it is the routing table
//! this service's definition answers with
//! ([`service_common::audited_operation::AuditedDefinition`]), so no request can be served
//! by a path that records nothing.
//!
//! ## The three outcomes of one call
//!
//! CloudTrail records the CALL, not the answer: a call that succeeded, one a handler refused
//! (a log group that is not there, a parameter the service will not accept) and one the IAM
//! gate in front of the operation refused are all management events of this service, told
//! apart by the record's `errorCode` / `errorMessage` and joined to the caller's own response
//! by its `requestID`. The first two are the shared wrapper's: it is handed the handler's
//! `Result`. The third never reaches a handler — the gate answers it before the operation
//! reads anything ([`super::identity_authorization`]) — so that one record is built here
//! ([`record_refused_at_authorization`]), carries the AUTHORIZATION layer's own error code
//! ([`RECORDED_ACCESS_DENIED_CODE`]) and carries no `requestParameters`, because the call was
//! never dispatched with any.
//!
//! ## What is NOT recorded
//!
//! A call on the CONTENTS of a log group — writing log events into a stream, reading a
//! stream's events back, tailing them live — is not a control-plane call, so it is no
//! management event and event history holds none ([`NOT_A_MANAGEMENT_EVENT`]). Neither is a
//! call this emulator has not implemented: it answers 501, and the wrapper drops it, because
//! event history is the record of the calls an account's APIs actually served.

use std::sync::Arc;

use aws_smithy_runtime_api::http::Response as RtResponse;
use service_common::audited_operation::{AuditedService, AuditedTable, CallKind};
use service_common::management_events::{
    recorded_caller, recorded_member_name, recorded_request_parameters, recorded_user_agent,
    AuditedResource, ManagementEventLog, RecordedSession, ServedApiCall,
};
use service_common::on_behalf_of_calls::{management_event_log, session_audit_directory};
use service_common::query_error_envelope::{named_request_as, new_request_id};
use smithy_server_runtime::{
    ErasedOperation, OperationContext, OperationRoutingTable, PreDispatchProtocol,
};

use super::identity_authorization::IamAuthorized;

/// The `eventSource` every record of this service carries: this service's endpoint prefix
/// (`smithy_query(query="service_info")` -> `endpointPrefix: logs`) in the
/// `<prefix>.amazonaws.com` form CloudTrail stamps on every event — which is the same
/// principal this service calls other services as ([`crate::state::SERVICE_PRINCIPAL`]), so
/// the two spellings of this service's name cannot drift apart.
pub const EVENT_SOURCE: &str = crate::state::SERVICE_PRINCIPAL;

/// The `errorCode` CloudTrail records an IAM authorization denial under.
///
/// It is the AUTHORIZATION layer's own code and not the error code the API answered its
/// caller with: the caller of a refused call to this `awsJson1_1` endpoint is told
/// `AccessDeniedException` (the AWS common error this model declares no shape of its own
/// for, see [`crate::scaffolded::ACCESS_DENIED_CODE`]), while the record of that call carries
/// this. The two are read by different clients — an SDK branches on the code it was answered
/// with, a detection rule matches on the code in event history — so both are stated, each
/// where it belongs.
pub const RECORDED_ACCESS_DENIED_CODE: &str = "AccessDenied";

/// The `Resource$resourceType` CloudTrail names a log GROUP by, in the CloudFormation
/// spelling every `Resources` entry of an event uses.
const LOG_GROUP_RESOURCE_TYPE: &str = "AWS::Logs::LogGroup";

/// The `Resource$resourceType` CloudTrail names a log STREAM by.
const LOG_STREAM_RESOURCE_TYPE: &str = "AWS::Logs::LogStream";

/// The operations whose calls are NOT management events: the calls on the CONTENTS of a log
/// group rather than on the account's log resources — the write of log events and the three
/// reads of them, one of which streams.
///
/// Event history holds an account's control-plane calls; a call that carries or returns the
/// log records themselves is the data plane, and CloudTrail does not record one as a
/// management event. Named here as the documentation of the set [`audited_table`] serves
/// unrecorded, so the set is one list rather than an absence per row.
pub const NOT_A_MANAGEMENT_EVENT: &[&str] = &[
    "FilterLogEvents",
    "GetLogEvents",
    "GetLogObject",
    "GetLogRecord",
    "PutLogEvents",
    "StartLiveTail",
];

/// What this service contributes to the record of each call it serves.
///
/// Holds the shared engine — the record goes into the log that engine carries, the one
/// CloudTrail's `LookupEvents` reads — so it is generic over the engine type exactly as the
/// service is.
pub struct LogsAudit<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> LogsAudit<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The account's log in that engine, or `None` for an engine that carries none.
    fn log(&self) -> Option<::engine::ServiceState<ManagementEventLog>> {
        management_event_log(&*self.engine)
    }

    /// `call` with the message it failed with naming the request the CALLER was answered
    /// under, where that message states this service's SDK error envelope.
    ///
    /// A refusal of this service is worded where it is decided, which is before the response
    /// that answers it exists, so the envelope clause is worded with a marker and the id
    /// written in where the response is finished ([`super::stated_request_id`]). The record of
    /// such a call carries the message the caller was answered with, so it states the same
    /// id — through that module's own reading of the clause, so a record and a response can
    /// never state two ids for one call.
    fn naming_the_answered_request(&self, call: ServedApiCall, answered_as: &str) -> ServedApiCall {
        let mut call = call;
        if let Some(stated) = call
            .error_message
            .as_deref()
            .and_then(|message| super::stated_request_id::substitute_request_id(message, answered_as))
        {
            call.error_message = Some(stated);
        }
        call
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for LogsAudit<Eng> {
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
        // of the Region is logging calls of its kind; publishing it is the wrapper's. The
        // guard is BOUND rather than used as a temporary: a local is released before the
        // handle it borrows, which a temporary at the end of this block would not be.
        let mut recording = log.lock();
        recording.record_delivered(
            self.naming_the_answered_request(call, answered_as),
            ::std::time::SystemTime::now(),
            answered_as.to_string(),
        )
    }

    fn recorded_session(&self, access_key_id: &str) -> Option<RecordedSession> {
        recorded_session(&*self.engine, access_key_id)
    }

    fn event_source(&self) -> &'static str {
        EVENT_SOURCE
    }

    fn region(&self, ctx: &OperationContext<'_>) -> String {
        // The Region the call was served in, read the way every handler of this service
        // reads it — which is also the Region of every ARN those handlers mint.
        crate::state::request_region(ctx.parts())
    }

    fn account_id(&self) -> String {
        ::engine::emulator_account_id().to_string()
    }

    fn resources(
        &self,
        _operation: &str,
        request_parameters: &serde_json::Value,
        _region: &str,
    ) -> Vec<AuditedResource> {
        // The resource a call addressed is named by MEMBERS of its request, whichever
        // operation carries them, so neither which operation served the call nor the Region
        // it was served in enters into it: a resource is named by the name its account holds
        // it under.
        addressed_resources(request_parameters)
    }

    fn sensitive_members(&self) -> &'static [&'static str] {
        // This service's model declares no `@sensitive` member at all, so no value a request
        // carries is one its records withhold.
        &[]
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        // The ONE reading of the id a response of this endpoint names its request by, the
        // same one every response passes through ([`super::stated_request_id::report`]): the
        // id already stated where a layer stated one, and a freshly minted id only where
        // none did. So the record carries the id the CALLER was answered under, whichever
        // layer reported it first.
        named_request_as(resp, new_request_id)
    }

    // `stopped_at_authorization` is the shared recorder's default: no refusal a HANDLER of
    // this service answers was decided before dispatch. The one refusal that is — the IAM
    // gate's — never reaches the wrapper at all, and its record is built by
    // [`record_refused_at_authorization`] below with no parameters on it.
}

/// The resources a call addressed, from the identifiers its request carried: the log GROUP it
/// names, the log STREAM beside it where it names one, and — for a call addressed to a
/// resource by ARN rather than by name, which is what a tagging call carries — the log group
/// that ARN names. The same reading of a request the authorization decision ahead of it takes
/// ([`super::identity_authorization`]), so what a record names and what a call is judged
/// against cannot disagree.
///
/// Each one is named by its USER-CREATED NAME, because that is what the field a lookup reports
/// it in holds: `Resource$ResourceName` is "the name of the resource referenced by the
/// event … These are user-created names"
/// (`smithy_query(query="doc", shape="Resource", member="ResourceName")` on CloudTrail's
/// model). So an identifier is folded to the resource's name whether the caller wrote the bare
/// name or the group's ARN, and a caller that addresses one group either way is recorded as
/// addressing the same resource.
///
/// A request that names no log group addresses no resource of this service, and records none
/// rather than one built from a value this service does not recognise.
fn addressed_resources(request_parameters: &serde_json::Value) -> Vec<AuditedResource> {
    let member = |name: &str| {
        request_parameters
            .get(recorded_member_name(name))
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let named = member(LOG_GROUP_NAME_MEMBER)
        .or_else(|| member(LOG_GROUP_IDENTIFIER_MEMBER))
        // A request that names no log group of its own but IS addressed to a resource by ARN —
        // a tagging call, whose `resourceArn` is the resource being tagged — addresses the log
        // group that ARN names, exactly as the decision ahead of it is taken against it.
        .or_else(|| member(RESOURCE_ARN_MEMBER));
    let Some(identifier) = named else {
        return Vec::new();
    };
    let Some(group) = log_group_name(identifier) else {
        return Vec::new();
    };
    let mut addressed = vec![AuditedResource::new(LOG_GROUP_RESOURCE_TYPE, group)];
    if let Some(stream) = member(LOG_STREAM_NAME_MEMBER) {
        // A call that names a stream acts on that stream, which is a resource of its own kind
        // under the group named beside it.
        addressed.push(AuditedResource::new(
            LOG_STREAM_RESOURCE_TYPE,
            stream.to_string(),
        ));
    }
    addressed
}

/// The user-created NAME of the log group `identifier` names: the value itself for a bare
/// name, and the group segment of an ARN for an identifier the caller wrote as one.
///
/// A log group's name carries no colon (`LogGroupName`'s pattern is
/// `^[\.\-_/#A-Za-z0-9]+$`, `smithy_query(query="shape", shape="LogGroupName")`), so the name
/// ends at the first colon of the ARN's resource segment — which is what drops the trailing
/// `:*` of the ARN of everything UNDER a group, and the `:log-stream:<stream>` of one stream's.
///
/// `None` for an ARN that names no log group of this service at all, which names no resource a
/// record of this service reports.
fn log_group_name(identifier: &str) -> Option<String> {
    if !identifier.starts_with("arn:") {
        return Some(identifier.to_string());
    }
    let (service, resource) = crate::state::arn_service_and_resource(identifier)?;
    if service != crate::state::identity_authorization::ACTION_PREFIX {
        return None;
    }
    let named = resource.strip_prefix("log-group:")?.split(':').next()?;
    (!named.is_empty()).then(|| named.to_string())
}

/// The member a request names the log group it addresses by, as the model spells it.
const LOG_GROUP_NAME_MEMBER: &str = "logGroupName";
/// The member a request names the log group it addresses by when the model lets it carry
/// either the name or the ARN.
const LOG_GROUP_IDENTIFIER_MEMBER: &str = "logGroupIdentifier";
/// The member a request names the log stream it addresses by.
const LOG_STREAM_NAME_MEMBER: &str = "logStreamName";
/// The member a request names the resource it addresses by its ARN (`AmazonResourceName`) —
/// what a tagging call of this service is addressed by.
const RESOURCE_ARN_MEMBER: &str = "resourceArn";

/// The session the credential `access_key_id` belongs to, read off the account's SESSION
/// directory in the shared engine: the sessions STS minted, each keyed by the access key id it
/// was minted under, which is what lets a record of a call made with one name the session it
/// was made by — the role that issued it included.
///
/// Both that directory and the log above are reached through the SHARED accessors
/// ([`service_common::on_behalf_of_calls`]), and not through a field of this service's own
/// state: CloudTrail reports the management calls of every service, and the service that MINTS
/// a session is not the service that records the calls made with it, so every recorder reaches
/// one log and one directory rather than a copy of each.
fn recorded_session<Eng: Send + Sync + 'static>(
    engine: &Eng,
    access_key_id: &str,
) -> Option<RecordedSession> {
    session_audit_directory(engine)
        .and_then(|directory| directory.lock().session(access_key_id).cloned())
}

/// Record the call the authorization gate REFUSED before the operation ran, as a management
/// event of this service.
///
/// Such a call reached no handler, so the shared wrapper never sees it — and it is still a
/// call this account's API answered, which is what event history holds. The record carries
/// the authorization layer's own code ([`RECORDED_ACCESS_DENIED_CODE`]) with the message the
/// caller was answered with, and NO `requestParameters`: those report what a call that was
/// authorized and dispatched carried, and this one was neither
/// ([`service_common::management_events::recorded_request_parameters`]).
///
/// The record is HELD rather than written, exactly as the wrapper holds its own: it names the
/// request by the id the CALLER was answered under, and that id exists only once the response
/// has been rendered. The layer every audited row of this service is served behind takes it
/// from there, writes it with that id and makes the deliveries the account's CloudTrail makes
/// for it. Called as the LAST thing before the refusal is answered, with nothing awaited in
/// between, which is what that hand-off rests on.
pub(crate) fn record_refused_at_authorization<Eng: Send + Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    operation: &str,
    kind: CallKind,
    message: &str,
) {
    let caller = recorded_caller(ctx.parts());
    let session = caller
        .access_key_id
        .as_deref()
        .and_then(|access_key_id| recorded_session(engine, access_key_id));
    ::service_common::audited_operation::hold_management_event(ServedApiCall {
        event_name: operation.to_string(),
        event_source: EVENT_SOURCE.to_string(),
        aws_region: crate::state::request_region(ctx.parts()),
        // A refused read is recorded as a read, exactly as the read itself would have been:
        // `readOnly` is the OPERATION's own property and not the outcome's.
        read_only: matches!(kind, CallKind::Reporting),
        access_key_id: caller.access_key_id,
        user_name: session
            .as_ref()
            .map_or(caller.user_name, |session| session.user_name.clone()),
        principal_type: session.as_ref().map_or_else(
            || caller.principal_type.to_string(),
            |session| session.principal_type.clone(),
        ),
        // The unique id of the IAM entity a LONG-TERM credential speaks for is IAM's to mint,
        // and this service resolves none, so such a record leaves it out rather than naming a
        // principal nothing resolves. A session's own principal id is the session's, and the
        // directory holds it.
        principal_id: session.as_ref().map(|session| session.principal_id.clone()),
        // A call signed with an AWS credential was authenticated by this account, not by an
        // external provider.
        identity_provider: None,
        // The refused caller made the call itself.
        invoked_by: None,
        session,
        account_id: ::engine::emulator_account_id().to_string(),
        user_agent: recorded_user_agent(ctx.parts()),
        // A call stopped at the permission check addressed whatever it addressed, and the
        // gate resolved none of it: the resources an event names are the ones the call acted
        // on.
        resources: Vec::new(),
        request_parameters: recorded_request_parameters(::serde_json::Value::Null, true),
        response_elements: None,
        additional_event_data: None,
        error_code: Some(RECORDED_ACCESS_DENIED_CODE.to_string()),
        error_message: Some(message.to_string()),
    });
}

/// The envelope a pre-dispatch failure of one of this service's operations renders in: this
/// service speaks `awsJson1_1`, which is the same value the generated per-operation shim
/// passes.
const JSON_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Json {
    content_type: "application/x-amz-json-1.1",
    // The awsJson families convey the error type in the body's `__type`, not in a header.
    errortype_header: false,
};

/// The kind of call one row of [`audited_table`] states: `reports` for an operation that only
/// REPORTS state, `mutates` for one that CHANGES the account's resources — which is the
/// event's `readOnly`, and what a lookup can filter on.
macro_rules! call_kind {
    (reports) => {
        CallKind::Reporting
    };
    (mutates) => {
        CallKind::Mutating
    };
}

/// Serve one operation of this service AUDITED, as a call of the stated kind, behind the IAM
/// decision taken in front of every operation.
///
/// The audited call stays INSIDE the gate, and the record of the call is written by the layer
/// the table files every row behind: so a call the gate refused and a call the handler served
/// are both recorded, and the record reports the operation's own input and output whatever
/// the layers outside it do to the response.
macro_rules! audited {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident => $kind:ident ),* $(,)?
    ) => {{
        $(
            {
                let for_iam = ::std::sync::Arc::clone($engine);
                $table.serve_behind(
                    crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                    call_kind!($kind),
                    move |audited, _pre_dispatch| {
                        IamAuthorized::new(
                            stringify!($operation),
                            for_iam,
                            ::std::option::Option::Some(call_kind!($kind)),
                            ::std::boxed::Box::new(audited) as ::std::boxed::Box<dyn ErasedOperation>,
                        )
                    },
                );
            }
        )*
    }};
}

/// Serve one operation of this service behind the IAM decision and RECORD NOTHING for it: the
/// calls on the contents of a log group, which are no management events
/// ([`NOT_A_MANAGEMENT_EVENT`]).
///
/// The gate is told so (`None` for the kind), because a call that is not a management event
/// has no record when it is refused either.
macro_rules! unrecorded {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident ),* $(,)?
    ) => {{
        $(
            $table.add_operation(
                stringify!($operation),
                ::std::boxed::Box::new(IamAuthorized::new(
                    stringify!($operation),
                    ::std::sync::Arc::clone($engine),
                    ::std::option::Option::None,
                    ::std::boxed::Box::new(
                        crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                    ) as ::std::boxed::Box<dyn ErasedOperation>,
                )),
            );
        )*
    }};
}

/// Every operation this service serves from a routing table, each constructed from the shared
/// engine: the control-plane calls served AUDITED, and the calls on a log group's contents
/// served unrecorded ([`NOT_A_MANAGEMENT_EVENT`]).
///
/// EVERY one is here, the ones this emulator has not implemented included: a stub answers 501
/// and the wrapper records nothing for it, so an operation implemented later is auditable the
/// moment it serves a call rather than when someone remembers to add it. The table is keyed
/// by each operation's own shape name — the routing key `ServerHttpProtocol::routing_key`
/// resolves and the generated table is keyed by.
///
/// The kind per row is the operation's own semantics: `reports` for the `Describe*` / `Get*` /
/// `List*` readers and the two `Test*` evaluations, which answer a question about the account
/// and change none of it, and `mutates` for every operation that creates, changes, tags,
/// starts, stops or removes something the account then holds — `StartQuery` and `StopQuery`
/// included, since each one submits or withdraws work the account holds.
///
/// The two operations whose output is a `@streaming` union (`StartLiveTail`, `GetLogObject`)
/// are both in [`NOT_A_MANAGEMENT_EVENT`], and are served by their own hand-authored
/// dispatches ([`crate::service::EmulatedService::override_operation`]) rather than from this
/// table, because the generated shim in it declines such a request.
pub fn audited_table<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> OperationRoutingTable {
    let recorder = Arc::new(LogsAudit::new(Arc::clone(engine)));
    let mut table = AuditedTable::new(recorder, JSON_ENVELOPE);
    audited!(
        table, engine,
        associate_kms_key::AssociateKmsKey => mutates,
        associate_source_to_s3_table_integration::AssociateSourceToS3TableIntegration => mutates,
        cancel_export_task::CancelExportTask => mutates,
        cancel_import_task::CancelImportTask => mutates,
        create_delivery::CreateDelivery => mutates,
        create_export_task::CreateExportTask => mutates,
        create_import_task::CreateImportTask => mutates,
        create_log_anomaly_detector::CreateLogAnomalyDetector => mutates,
        create_log_group::CreateLogGroup => mutates,
        create_log_stream::CreateLogStream => mutates,
        create_lookup_table::CreateLookupTable => mutates,
        create_scheduled_query::CreateScheduledQuery => mutates,
        delete_account_policy::DeleteAccountPolicy => mutates,
        delete_data_protection_policy::DeleteDataProtectionPolicy => mutates,
        delete_delivery::DeleteDelivery => mutates,
        delete_delivery_destination::DeleteDeliveryDestination => mutates,
        delete_delivery_destination_policy::DeleteDeliveryDestinationPolicy => mutates,
        delete_delivery_source::DeleteDeliverySource => mutates,
        delete_destination::DeleteDestination => mutates,
        delete_index_policy::DeleteIndexPolicy => mutates,
        delete_integration::DeleteIntegration => mutates,
        delete_log_anomaly_detector::DeleteLogAnomalyDetector => mutates,
        delete_log_group::DeleteLogGroup => mutates,
        delete_log_stream::DeleteLogStream => mutates,
        delete_lookup_table::DeleteLookupTable => mutates,
        delete_metric_filter::DeleteMetricFilter => mutates,
        delete_query_definition::DeleteQueryDefinition => mutates,
        delete_resource_policy::DeleteResourcePolicy => mutates,
        delete_retention_policy::DeleteRetentionPolicy => mutates,
        delete_scheduled_query::DeleteScheduledQuery => mutates,
        delete_subscription_filter::DeleteSubscriptionFilter => mutates,
        delete_syslog_configuration::DeleteSyslogConfiguration => mutates,
        delete_transformer::DeleteTransformer => mutates,
        describe_account_policies::DescribeAccountPolicies => reports,
        describe_configuration_templates::DescribeConfigurationTemplates => reports,
        describe_deliveries::DescribeDeliveries => reports,
        describe_delivery_destinations::DescribeDeliveryDestinations => reports,
        describe_delivery_sources::DescribeDeliverySources => reports,
        describe_destinations::DescribeDestinations => reports,
        describe_export_tasks::DescribeExportTasks => reports,
        describe_field_indexes::DescribeFieldIndexes => reports,
        describe_import_task_batches::DescribeImportTaskBatches => reports,
        describe_import_tasks::DescribeImportTasks => reports,
        describe_index_policies::DescribeIndexPolicies => reports,
        describe_log_groups::DescribeLogGroups => reports,
        describe_log_streams::DescribeLogStreams => reports,
        describe_lookup_tables::DescribeLookupTables => reports,
        describe_metric_filters::DescribeMetricFilters => reports,
        describe_queries::DescribeQueries => reports,
        describe_query_definitions::DescribeQueryDefinitions => reports,
        describe_resource_policies::DescribeResourcePolicies => reports,
        describe_subscription_filters::DescribeSubscriptionFilters => reports,
        disassociate_kms_key::DisassociateKmsKey => mutates,
        disassociate_source_from_s3_table_integration::DisassociateSourceFromS3TableIntegration
            => mutates,
        get_data_protection_policy::GetDataProtectionPolicy => reports,
        get_delivery::GetDelivery => reports,
        get_delivery_destination::GetDeliveryDestination => reports,
        get_delivery_destination_policy::GetDeliveryDestinationPolicy => reports,
        get_delivery_source::GetDeliverySource => reports,
        get_integration::GetIntegration => reports,
        get_log_anomaly_detector::GetLogAnomalyDetector => reports,
        get_log_fields::GetLogFields => reports,
        get_log_group_fields::GetLogGroupFields => reports,
        get_lookup_table::GetLookupTable => reports,
        get_query_results::GetQueryResults => reports,
        get_scheduled_query::GetScheduledQuery => reports,
        get_scheduled_query_history::GetScheduledQueryHistory => reports,
        get_storage_tier_policy::GetStorageTierPolicy => reports,
        get_transformer::GetTransformer => reports,
        list_aggregate_log_group_summaries::ListAggregateLogGroupSummaries => reports,
        list_anomalies::ListAnomalies => reports,
        list_integrations::ListIntegrations => reports,
        list_log_anomaly_detectors::ListLogAnomalyDetectors => reports,
        list_log_groups::ListLogGroups => reports,
        list_log_groups_for_query::ListLogGroupsForQuery => reports,
        list_scheduled_queries::ListScheduledQueries => reports,
        list_sources_for_s3_table_integration::ListSourcesForS3TableIntegration => reports,
        list_syslog_configurations::ListSyslogConfigurations => reports,
        list_tags_for_resource::ListTagsForResource => reports,
        list_tags_log_group::ListTagsLogGroup => reports,
        put_account_policy::PutAccountPolicy => mutates,
        put_bearer_token_authentication::PutBearerTokenAuthentication => mutates,
        put_data_protection_policy::PutDataProtectionPolicy => mutates,
        put_delivery_destination::PutDeliveryDestination => mutates,
        put_delivery_destination_policy::PutDeliveryDestinationPolicy => mutates,
        put_delivery_source::PutDeliverySource => mutates,
        put_destination::PutDestination => mutates,
        put_destination_policy::PutDestinationPolicy => mutates,
        put_index_policy::PutIndexPolicy => mutates,
        put_integration::PutIntegration => mutates,
        put_log_group_deletion_protection::PutLogGroupDeletionProtection => mutates,
        put_metric_filter::PutMetricFilter => mutates,
        put_query_definition::PutQueryDefinition => mutates,
        put_resource_policy::PutResourcePolicy => mutates,
        put_retention_policy::PutRetentionPolicy => mutates,
        put_storage_tier_policy::PutStorageTierPolicy => mutates,
        put_subscription_filter::PutSubscriptionFilter => mutates,
        put_syslog_configuration::PutSyslogConfiguration => mutates,
        put_transformer::PutTransformer => mutates,
        start_query::StartQuery => mutates,
        stop_query::StopQuery => mutates,
        tag_log_group::TagLogGroup => mutates,
        tag_resource::TagResource => mutates,
        test_metric_filter::TestMetricFilter => reports,
        test_transformer::TestTransformer => reports,
        untag_log_group::UntagLogGroup => mutates,
        untag_resource::UntagResource => mutates,
        update_anomaly::UpdateAnomaly => mutates,
        update_delivery_configuration::UpdateDeliveryConfiguration => mutates,
        update_log_anomaly_detector::UpdateLogAnomalyDetector => mutates,
        update_lookup_table::UpdateLookupTable => mutates,
        update_scheduled_query::UpdateScheduledQuery => mutates,
    );
    let mut table = table.finish();
    // The calls on a log group's CONTENTS: served exactly as before, recorded nowhere.
    unrecorded!(
        table, engine,
        filter_log_events::FilterLogEvents,
        get_log_events::GetLogEvents,
        get_log_record::GetLogRecord,
        put_log_events::PutLogEvents,
    );
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two operations the audited table does NOT hold: the ones whose output is a
    /// `@streaming` union, which `Service::override_operation` serves from their own
    /// hand-authored dispatches because the generated shim in a routing table declines such a
    /// request. Both are calls on a log group's contents ([`NOT_A_MANAGEMENT_EVENT`]), so
    /// neither is a management event either.
    const SERVED_OUTSIDE_THE_TABLE: &[&str] = &[
        super::super::GET_LOG_OBJECT_OPERATION,
        super::super::START_LIVE_TAIL_OPERATION,
    ];

    /// This service's Smithy model, as it ships beside this crate. Read at test time rather
    /// than embedded: it is megabytes of JSON that no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-cloudwatchlogs-logs-20140328.json"
    );

    /// Every operation THE MODEL declares, by the shape name a routing key resolves to.
    fn modelled_operations() -> Vec<String> {
        let model = ::std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        let model: serde_json::Value =
            serde_json::from_str(&model).expect("the model that generated this crate is JSON");
        let operations: Vec<String> = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes")
            .iter()
            .filter(|(_, shape)| shape["type"] == "operation")
            .filter_map(|(id, _)| id.rsplit('#').next().map(str::to_string))
            .collect();
        assert!(
            operations.len() > 100,
            "case \"the model's surface\": expected this service's operations, got {} of them",
            operations.len()
        );
        operations
    }

    /// RULE applied to the whole surface: every operation the model declares is SERVED, and
    /// served from this table unless it is one of the two the table cannot hold
    /// ([`SERVED_OUTSIDE_THE_TABLE`]).
    ///
    /// The names come from the MODEL rather than from a second hand list, because this table
    /// REPLACES the generated one for every request this service routes
    /// ([`service_common::audited_operation::AuditedDefinition`]): an operation added to the
    /// model and not to a row here would stop being servable altogether — answering a routing
    /// error rather than its own answer, a stub's 501 included — and a key spelled differently
    /// from the generated table's would never be reached. Both fail here instead.
    #[test]
    fn every_modelled_operation_is_served_from_this_table_or_its_own_dispatch() {
        let engine = Arc::new(::engine::InMemoryEngine::new());
        let generated = crate::ServiceDefinition::new(Arc::clone(&engine));
        let audited = audited_table(&engine);
        for operation in modelled_operations() {
            assert!(
                generated.routing_table().get(&operation).is_some(),
                "case {operation:?}: the generated table keys an operation by its shape name; \
                 this one resolved to nothing, so the key spelled in this module is wrong"
            );
            let served_elsewhere = SERVED_OUTSIDE_THE_TABLE.contains(&operation.as_str());
            assert_eq!(
                audited.get(&operation).is_some(),
                !served_elsewhere,
                "case {operation:?}: expected this table to serve it = {}, so a call to it \
                 is served by the path this module states and by no other",
                !served_elsewhere
            );
        }
    }

    /// RULE: the calls that are no management events are exactly the calls on a log group's
    /// CONTENTS — and each of them is still an operation this service serves.
    #[test]
    fn only_the_calls_on_a_log_groups_contents_are_recorded_nowhere() {
        let modelled = modelled_operations();
        for operation in NOT_A_MANAGEMENT_EVENT {
            assert!(
                modelled.iter().any(|modelled| modelled == operation),
                "case {operation:?}: the unrecorded set names operations of the model; this \
                 one is in no shape of it"
            );
        }
        for operation in SERVED_OUTSIDE_THE_TABLE {
            assert!(
                NOT_A_MANAGEMENT_EVENT.contains(operation),
                "case {operation:?}: an operation served outside the audited table records \
                 nothing, so it must be named among the calls that are no management events"
            );
        }
        // And the two kinds of log-data call are both there: the write and the reads.
        for operation in ["PutLogEvents", "GetLogEvents", "FilterLogEvents"] {
            assert!(
                NOT_A_MANAGEMENT_EVENT.contains(&operation),
                "case {operation:?}: a call on the contents of a log group is no management \
                 event"
            );
        }
    }
}
