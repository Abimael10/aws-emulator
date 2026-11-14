// SPDX-License-Identifier: CC0-1.0

//! What the account's CloudTrail does with a MANAGEMENT event of this service: the three
//! sinks one record of one served control-plane call reaches.
//!
//! A management event — the administration of identity pools, the calls a client makes with
//! AWS developer credentials ([`super::record_served_call`]) — has three readers, and they
//! are all fed from ONE record:
//!
//! * event history, which CloudTrail's `LookupEvents` reports, naming the request the
//!   CALLER was answered under;
//! * the bucket of every trail of the account that is logging calls of its kind, as the
//!   gzipped log file CloudTrail delivers there
//!   ([`service_common::trail_log_files`]); and
//! * the account's default EventBridge bus, which carries each logged call that CHANGED the
//!   account as `AWS API Call via CloudTrail` ([`service_common::api_call_events`]).
//!
//! Which of them a given call reaches is the account's CloudTrail configuration and not
//! this service's: the log answers what is delivered for a record
//! ([`service_common::management_events::ManagementEventLog::record_delivered`]), and the
//! layer each operation is served behind makes those deliveries once the response exists
//! ([`service_common::audited_operation::RecordedCall`]). So this file is the one place
//! that hands this service's records to that seam — the same seam every other recording
//! service in this tree reaches, so a trail that logs the account's management events
//! receives this service's calls beside theirs.

use std::sync::Arc;

use ::service_common::audited_operation::AuditedService;
use ::service_common::management_events::{RecordedSession, ServedApiCall};

/// What this service contributes to the record of each management call it serves.
///
/// Holds the shared engine — the record goes into the log that engine carries, the one
/// CloudTrail reads — so it is generic over the engine type exactly as the service is.
pub struct IdentityPoolAudit<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> IdentityPoolAudit<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for IdentityPoolAudit<Eng> {
    fn record(
        &self,
        call: ServedApiCall,
        answered_as: &str,
    ) -> Option<::service_common::api_call_events::DeliveredApiCall> {
        let Some(log) = super::management_event_log(&*self.engine) else {
            // Not silent: an unrecorded call leaves no trace in the log by definition, so
            // it leaves one here instead.
            ::tracing::warn!(
                operation = call.event_name,
                "the engine carries no management-event log: the call was served unrecorded"
            );
            return None;
        };
        // The record, and what the account's CloudTrail delivers for it while a trail of
        // the Region is logging calls of its kind; making those deliveries is the wrapper's.
        // The guard is BOUND rather than used as a temporary: a local is released before the
        // handle it borrows, which a temporary at the end of this block would not be.
        let mut recording = log.lock();
        recording.record_delivered(
            call,
            ::std::time::SystemTime::now(),
            answered_as.to_string(),
        )
    }

    fn recorded_session(&self, access_key_id: &str) -> Option<RecordedSession> {
        // The account's own session directory, so a call made with a role session is
        // recorded as that session and as the role that issued it: the shared store the
        // minting service wrote it to.
        super::recorded_session(&*self.engine, Some(access_key_id))
    }

    fn event_source(&self) -> &'static str {
        super::EVENT_SOURCE
    }

    fn region(&self, ctx: &::smithy_server_runtime::OperationContext<'_>) -> String {
        // The Region the call was served in, read the way every handler of this service
        // reads it.
        super::request_region(ctx.parts())
    }

    fn account_id(&self) -> String {
        ::engine::emulator_account_id().to_string()
    }

    fn sensitive_members(&self) -> &'static [&'static str] {
        SENSITIVE_MEMBERS
    }
}

/// The members this service's model declares `@sensitive`, whose values no record carries:
/// the token a `Logins` entry presents (`LoginsMap`'s value targets `IdentityProviderToken`),
/// the OpenID token an exchange answers (`GetOpenIdTokenResponse$Token` and
/// `GetOpenIdTokenForDeveloperIdentityResponse$Token` target `OIDCToken`) and the secret key
/// of a vended session (`Credentials$SecretKey`)
/// (`smithy_query(query="shape",shape="LoginsMap,GetOpenIdTokenResponse,Credentials")`).
///
/// Each is named as the event document names its member, which is the spelling a handler
/// writes into the record it builds.
const SENSITIVE_MEMBERS: &[&str] = &["logins", "token", "secretKey"];
