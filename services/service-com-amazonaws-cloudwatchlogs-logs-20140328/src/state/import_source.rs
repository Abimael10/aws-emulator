// SPDX-License-Identifier: CC0-1.0

//! Resolving the data source an import task reads from, against the service that holds it.
//!
//! `CreateImportTask` "starts an import from a data source to CloudWatch Log", and
//! "currently, CloudTrail Event Data Store is the only supported data source": its
//! `importSourceArn` therefore names a resource of ANOTHER service, which this service
//! resolves where that service holds it rather than assuming. CloudTrail is reached the way
//! the framework documents a handler reaching another emulated service
//! ([`smithy_server_runtime::OperationContext::callback`]): an AWS SDK client over the
//! in-process callback's HTTP client and credentials, so nothing here opens a socket or
//! leaves the host. The question is put with `GetEventDataStore`, the operation CloudTrail
//! documents for reading one store ("Returns information about an event data store specified
//! as either an ARN or the ID portion of the ARN").
//!
//! The answer distinguishes a value that is in NO FORM one of that service's event data stores
//! is named by from a well-formed one that service holds no store under
//! ([`ImportSource::Malformed`] against [`ImportSource::Absent`]): the first is a defect in the
//! value the request carried, the second a resource that is not there — two different
//! conditions, which the operation refuses as two different classes.
//!
//! The one caller-supplied value that reaches the client's AUTHORITY is the signing Region,
//! and it is narrowed before it gets there ([`service_common::arn::region_label`]), so no
//! request can re-point this client at a host of its own choosing: it reaches the emulator's
//! own CloudTrail and nothing else, whatever source a caller names. The client itself is the
//! shared callback client every call to another service of this emulator is made through
//! ([`crate::state::callback_client`]).

use super::callback_client::callback_client;

/// What the owning service answers about the source an import names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportSource {
    /// CloudTrail holds an event data store under the ARN, in a state an import may read
    /// from.
    Active,
    /// CloudTrail holds the event data store, but it is not in a state an import may read
    /// from.
    Inactive,
    /// No event data store answers to the ARN: the ARN is one a store of that service is
    /// named by, and CloudTrail reported holding none under it.
    Absent,
    /// The value is in no form an event data store is named by at all — not an ARN of that
    /// service's event data stores, or one whose resource suffix is no identifier that
    /// service ever mints ([`event_data_store_id`]). Nothing is looked up for it: no store
    /// could answer to it.
    Malformed,
    /// The source could not be resolved — an emulator assembled with no CloudTrail to ask,
    /// or any other failure to ask it. NOTHING is then established about the source, so it
    /// grounds no refusal.
    Unresolved,
}

/// The ARN service segment of the one supported data source's owning service.
const SOURCE_SERVICE: &str = "cloudtrail";

/// The ARN resource-type segment of the one supported data source: an event data store
/// (`arn:aws:cloudtrail:<region>:<account>:eventdatastore/<id>`).
const SOURCE_RESOURCE_TYPE: &str = "eventdatastore/";

/// What CloudTrail reports about the source an import task would read from `source_arn`.
///
/// The ARN is read first: the only supported data source is a CloudTrail Lake event data
/// store, so a value that is not an ARN naming one of those — a resource of another kind or
/// another service, or an event data store ARN carrying no identifier that service mints — is
/// [`ImportSource::Malformed`] without asking it, there being no store it could answer to.
/// Whether a store ANSWERS to a well-formed one is CloudTrail's to say.
pub async fn resolve(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    source_arn: &str,
) -> ImportSource {
    if !names_event_data_store(source_arn) {
        return ImportSource::Malformed;
    }
    // Which Region the store is looked up in decides nothing here — the emulator serves one
    // CloudTrail over the in-process callback — so a scope that carries no Region label falls
    // back to the default rather than putting caller text in the authority.
    let region =
        ::service_common::arn::region_label(region).unwrap_or(crate::state::DEFAULT_REGION);
    let client = callback_client!(aws_sdk_cloudtrail, ctx.callback(), "cloudtrail", region);
    match client
        .get_event_data_store()
        .event_data_store(source_arn)
        .send()
        .await
    {
        // CloudTrail holds the store, and states the status it is in.
        Ok(store) => match store.status() {
            Some(status) if is_active(status) => ImportSource::Active,
            // A status outside the set an import may read from.
            Some(_) => ImportSource::Inactive,
            // CloudTrail stated no status at all, so nothing is established about whether the
            // store may be read from and nothing is refused on it.
            None => ImportSource::Active,
        },
        // CloudTrail says no event data store of this account answers to the ARN: the
        // refusal is grounded in its answer.
        Err(error) if reported_absent(&error) => ImportSource::Absent,
        // CloudTrail says the identifier it was handed is in no form a store of its own is
        // named by, which is a defect in the VALUE rather than a missing store — the same
        // thing the reading above establishes for a value this service can already see is in
        // no such form.
        Err(error) if reported_arn_invalid(&error) => ImportSource::Malformed,
        // Any other answer is this emulator failing to ask, not CloudTrail reporting on the
        // store, so it refuses nothing — and it is worth a warning, because a deployment with
        // no CloudTrail to ask is one where this resolution does not run. The caller's own
        // value is rendered in its escaped `Debug` form, so a source ARN carrying a newline
        // cannot forge a log line.
        Err(error) => {
            ::tracing::warn!(
                import_source = ?source_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "an import task's source could not be resolved against CloudTrail"
            );
            ImportSource::Unresolved
        }
    }
}

/// Whether `arn` names a CloudTrail Lake event data store — the one kind of resource that is
/// a supported import source.
fn names_event_data_store(arn: &str) -> bool {
    event_data_store_id(arn).is_some()
}

/// The event data store id an import source ARN carries
/// (`arn:aws:cloudtrail:<region>:<account>:eventdatastore/<id>`), or `None` when the ARN names
/// no event data store.
///
/// THE one reading of an import source ARN: the resolution above asks the owning service about
/// a source this places, and the managed destination log group an accepted import delivers
/// into is named after the store this answers
/// ([`crate::state::import_destination_log_group_name`]).
///
/// The resource suffix has to BE an identifier the owning service gives an event data store —
/// the UUID its published ARN format carries
/// (`arn:aws:cloudtrail:<region>:<account>:eventdatastore/<uuid>`), which is what
/// [`is_event_data_store_id`] reads — so a value of any other shape names no store of that
/// service at all rather than one it happens not to hold.
pub fn event_data_store_id(arn: &str) -> Option<&str> {
    let (service, resource) = crate::state::arn_service_and_resource(arn)?;
    if service != SOURCE_SERVICE {
        return None;
    }
    let id = resource.strip_prefix(SOURCE_RESOURCE_TYPE)?;
    is_event_data_store_id(id).then_some(id)
}

/// Whether `value` is an identifier the owning service gives an event data store: the UUID
/// its ARN format publishes as that resource's suffix
/// (`eventdatastore/1a0abbc2-dfa0-46e6-885a-5ce09f2ee3ee`).
///
/// Only the canonical lower-case hyphenated rendering counts, which is the whole of that
/// form: the maintained UUID parser also accepts the simple, braced and URN spellings and
/// upper-case hex, and an identifier written in any of those is not one the owning service
/// ever handed out, so this compares the parsed value back against the text it was read from
/// rather than taking "parses as a UUID" for the form.
fn is_event_data_store_id(value: &str) -> bool {
    ::uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

/// Whether the status CloudTrail reports for an event data store is one an import may read
/// from.
///
/// "The specified source must be in an ACTIVE state", and the model's `EventDataStoreStatus`
/// enum carries no `ACTIVE`: the statuses in which a store is there and in service are the
/// one it is created in and the one it settles into. This is an ALLOW-LIST, so a status that
/// is on its way out (queued for deletion) or not in service (an ingestion change) is not
/// read from, and a status the enum gains later is not either until it is named here.
fn is_active(status: &::aws_sdk_cloudtrail::types::EventDataStoreStatus) -> bool {
    matches!(
        status,
        ::aws_sdk_cloudtrail::types::EventDataStoreStatus::Created
            | ::aws_sdk_cloudtrail::types::EventDataStoreStatus::Enabled
    )
}

/// A `GetEventDataStore` failure, as the owning service's client reports it.
type GetEventDataStoreFailure = ::aws_sdk_cloudtrail::error::SdkError<
    ::aws_sdk_cloudtrail::operation::get_event_data_store::GetEventDataStoreError,
    ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
>;

/// Whether a CloudTrail `GetEventDataStore` failure is CloudTrail reporting that it holds no
/// event data store under the identifier, as opposed to the call not reaching CloudTrail at
/// all.
fn reported_absent(error: &GetEventDataStoreFailure) -> bool {
    error
        .as_service_error()
        .is_some_and(|service_error| service_error.is_event_data_store_not_found_exception())
}

/// Whether a CloudTrail `GetEventDataStore` failure is CloudTrail reporting that the
/// identifier is in no form one of its event data stores is named by, as opposed to the call
/// not reaching CloudTrail at all.
fn reported_arn_invalid(error: &GetEventDataStoreFailure) -> bool {
    error
        .as_service_error()
        .is_some_and(|service_error| service_error.is_event_data_store_arn_invalid_exception())
}
