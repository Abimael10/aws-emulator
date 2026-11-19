// SPDX-License-Identifier: CC0-1.0

//! The calls one emulated service makes to ANOTHER service's API ON THE CALLER'S BEHALF,
//! as the account's event history records them.
//!
//! Some of the API calls an account makes are not made by a client at all: a service makes
//! them for the client, while serving a call of its own. Amazon SQS asking KMS for a data
//! key so it can seal a message of an encrypted queue is the shape — the client called
//! `SendMessage`, and the `GenerateDataKey` that send needed was made by Amazon SQS.
//!
//! CloudTrail records such a call like any other management call of the account, with two
//! differences a reader can see:
//!
//! * the record belongs to the service that was CALLED — its `eventSource` and its
//!   operation's `eventName` — not to the service that made the call; and
//! * the `userIdentity` is the identity of the CLIENT whose call needed it, with
//!   `invokedBy` naming the service that made it on that client's behalf, which is how an
//!   auditor tells a call a service made for a client from one the client made itself.
//!
//! So the recording is the same store, the same identity resolution and the same bounds as
//! every other recorded call ([`crate::management_events`]): what this module adds is the
//! ONE place that builds such a record, so two services reaching a third on a caller's
//! behalf cannot report one differently.
//!
//! Recording is BEST EFFORT, like every other recorder in this tree: an engine that carries
//! no log (a unit test over a bare store) records nothing and the call it was made for is
//! served exactly as it would be otherwise.

use std::time::SystemTime;

use crate::management_events::{
    recorded_caller, recorded_user_agent, ManagementEventLog, SessionAuditDirectory,
};

/// One call an emulated service made to another service's API for the caller it was
/// serving.
#[derive(Debug, Clone)]
pub struct OnBehalfOfCall {
    /// The operation that was called, as the CALLED service's model spells it
    /// (`GenerateDataKey`).
    pub event_name: String,
    /// The service that was called, in CloudTrail's `<endpoint-prefix>.amazonaws.com` form
    /// (`kms.amazonaws.com`).
    pub event_source: String,
    /// The service that made the call, in that same form (`sqs.amazonaws.com`) — the
    /// record's `userIdentity.invokedBy`.
    pub invoked_by: String,
    /// The Region the call was made in, which is the Region of the resource it was made
    /// for: a record is filed in the Region of the call it records.
    pub aws_region: String,
    /// Whether the called operation only READ state, which is the record's `readOnly`.
    pub read_only: bool,
    /// The parameters the call carried, as the record's `requestParameters`.
    pub request_parameters: serde_json::Value,
    /// The values it answered with, as the record's `responseElements`; `None` for a call
    /// whose answer the record does not carry — which is every answer holding key material.
    pub response_elements: Option<serde_json::Value>,
}

/// Record `call` in the account's event history as a call made on behalf of the caller of
/// `parts`, and answer the event's id.
///
/// `parts` is the request of the call the CLIENT made — the one this call was needed for —
/// because that is whose identity the record names: the access key id it was signed with,
/// the session that credential belongs to when the account's directory holds one, and the
/// user agent it sent.
///
/// `None` when the engine carries no event history, which records nothing.
///
/// The record is named by a request id of its own, because no id a client was answered
/// under belongs to this call. A service that KNOWS the id the called service answered its
/// in-process call under — because it goes on to report that id somewhere a caller can read
/// it — states it through [`record_on_behalf_of_answered`] instead, so the two cannot name
/// two different requests for one call.
pub fn record_on_behalf_of<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &::http::request::Parts,
    call: OnBehalfOfCall,
    now: SystemTime,
) -> Option<String> {
    record_on_behalf_of_answered(
        engine,
        parts,
        call,
        now,
        crate::query_error_envelope::new_request_id(),
    )
}

/// [`record_on_behalf_of`], with `request_id` as the id the CALLED service answered this
/// in-process call under — the record's `requestID`.
///
/// What a service reaches for when that id is reported somewhere else as well: the called
/// service's own state may name the call by it (KMS's usage tracking reports the request id
/// of the last cryptographic operation), and a reader joining the two surfaces has to find
/// one request rather than two. Everything else — the identity resolution, `invokedBy`, the
/// store and its bounds — is [`record_on_behalf_of`]'s.
pub fn record_on_behalf_of_answered<Eng: Send + Sync + 'static>(
    engine: &Eng,
    parts: &::http::request::Parts,
    call: OnBehalfOfCall,
    now: SystemTime,
    request_id: String,
) -> Option<String> {
    let log = management_event_log(engine)?;
    let caller = recorded_caller(parts);
    // The session the caller's credential belongs to, when it is a temporary one the
    // account's directory holds: a call a role session needed is attributed to that session
    // and to the role that issued it, exactly as the session's own calls are.
    let session = caller
        .access_key_id
        .as_deref()
        .and_then(|access_key_id| {
            session_audit_directory(engine)
                .and_then(|directory| directory.lock().session(access_key_id).cloned())
        });
    let served = crate::management_events::ServedApiCall {
        event_name: call.event_name,
        event_source: call.event_source,
        aws_region: call.aws_region,
        read_only: call.read_only,
        access_key_id: caller.access_key_id,
        user_name: session
            .as_ref()
            .map_or(caller.user_name, |session| session.user_name.clone()),
        principal_type: session.as_ref().map_or_else(
            || caller.principal_type.to_string(),
            |session| session.principal_type.clone(),
        ),
        principal_id: session.as_ref().map(|session| session.principal_id.clone()),
        // A call signed with an AWS credential was authenticated by this account, not by an
        // external provider.
        identity_provider: None,
        // The one fact that tells this record from one the client made itself.
        invoked_by: Some(call.invoked_by),
        session,
        account_id: ::engine::emulator_account_id().to_string(),
        user_agent: recorded_user_agent(parts),
        // The pairs CloudTrail reports in an event's `Resources` are not this recorder's to
        // invent: a record naming a resource nothing resolved would be reported to a lookup
        // by `ResourceName` as fact.
        resources: Vec::new(),
        request_parameters: call.request_parameters,
        response_elements: call.response_elements,
        // A service making a call for a caller reports nothing beside the parameters it
        // made it with.
        additional_event_data: None,
        // A call that failed is not recorded here: the service that makes one of these calls
        // relays the failure to its own caller, and the record of the failed call belongs to
        // the service that refused it.
        error_code: None,
        error_message: None,
    };
    // The id the called service answered THIS call under — a client's own response names
    // the call the client made, not this one.
    // The guard is BOUND rather than used as a temporary: a local is released before the
    // handle it borrows, which a temporary at the end of this block would not be.
    let mut recording = log.lock();
    Some(recording.record_answered(served, now, request_id))
}

/// The account's management-event log in the shared engine, or `None` for an engine that
/// carries none.
///
/// The SAME handle every recording service reaches, so a call recorded here and a call
/// recorded by the serving wrapper land in one log.
pub fn management_event_log<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ManagementEventLog>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ManagementEventLog>)
}

/// The account's directory of live sessions in that same engine: what lets a record of a
/// call made with a temporary credential name the session it was made with.
pub fn session_audit_directory<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<SessionAuditDirectory>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<SessionAuditDirectory>)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The request a client made, signed with `access_key_id` in its credential scope.
    fn client_request(access_key_id: &str) -> ::http::request::Parts {
        let request = ::http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "authorization",
                format!(
                    "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/us-east-1/sqs/\
                     aws4_request, SignedHeaders=host, Signature=0"
                ),
            )
            .header("user-agent", "aws-sdk-rust/probe")
            .body(())
            .expect("a well-formed request");
        request.into_parts().0
    }

    /// One call of `event_name`, made by `invoked_by` against `event_source`.
    fn call(event_name: &str, event_source: &str, invoked_by: &str) -> OnBehalfOfCall {
        OnBehalfOfCall {
            event_name: event_name.to_string(),
            event_source: event_source.to_string(),
            invoked_by: invoked_by.to_string(),
            aws_region: "us-east-1".to_string(),
            read_only: true,
            request_parameters: serde_json::json!({"keyId": "a-key"}),
            response_elements: None,
        }
    }

    /// The record names the CALLED service and the operation called, and carries the
    /// caller's own identity with `invokedBy` naming the service that made the call. The
    /// rows differ along which service made which call for which caller.
    #[test]
    fn a_record_names_the_called_service_and_the_calling_service_principal() {
        let engine = ::engine::InMemoryEngine::new();
        let rows = [
            (
                "a data key an SQS send needed",
                "AKIDTEST",
                "GenerateDataKey",
                "kms.amazonaws.com",
                "sqs.amazonaws.com",
            ),
            (
                "a data key an S3 put needed",
                "AKIDOTHER",
                "GenerateDataKey",
                "kms.amazonaws.com",
                "s3.amazonaws.com",
            ),
            (
                "a role an assumed-role session's service call needed",
                "ASIASESSION",
                "Decrypt",
                "kms.amazonaws.com",
                "sqs.amazonaws.com",
            ),
        ];
        for (case, access_key_id, event_name, event_source, invoked_by) in rows {
            let parts = client_request(access_key_id);
            let recorded = record_on_behalf_of(
                &engine,
                &parts,
                call(event_name, event_source, invoked_by),
                SystemTime::now(),
            );
            assert!(
                recorded.is_some(),
                "case {case:?}: the shared engine carries event history, so the call must be \
                 recorded, got {recorded:?}"
            );
            let log = management_event_log(&engine).expect("the engine carries event history");
            let held = log.lock();
            let event = held
                .iter()
                .next_back()
                .expect("the record just written is the newest one")
                .to_cloudtrail_event_json();
            for (member, expected) in [
                ("eventName", event_name),
                ("eventSource", event_source),
            ] {
                assert_eq!(
                    event[member],
                    serde_json::json!(expected),
                    "case {case:?}: expected {member} {expected:?}, got {event}"
                );
            }
            assert_eq!(
                event["userIdentity"]["invokedBy"],
                serde_json::json!(invoked_by),
                "case {case:?}: expected invokedBy {invoked_by:?}, got {event}"
            );
            assert_eq!(
                event["userIdentity"]["accessKeyId"],
                serde_json::json!(access_key_id),
                "case {case:?}: the record names the CALLER's credential, got {event}"
            );
            assert!(
                event["responseElements"].is_null(),
                "case {case:?}: a call whose answer the record does not carry records none, \
                 got {event}"
            );
        }
    }
}
