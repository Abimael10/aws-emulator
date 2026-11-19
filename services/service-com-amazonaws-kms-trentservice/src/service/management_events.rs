// SPDX-License-Identifier: CC0-1.0

//! This service's AUDIT leg: every call it serves is a management event of
//! `kms.amazonaws.com`, recorded in the account's event history where CloudTrail's
//! `LookupEvents` reads it — and the id that record names the request by is the very id
//! the caller's own response carried.
//!
//! # The rule
//!
//! Every response AWS sends names the request it answered, in the response header its
//! protocol reports that id in (`x-amzn-RequestId` for this service's `awsJson1_1`), on a
//! success and on a failure alike; an SDK surfaces it as the call's
//! `ResponseMetadata.RequestId`. For KMS that one id is also the join key its audit
//! surface is defined on: `KeyLastUsageData` reports "the request ID of the cryptographic
//! operation" and "the CloudTrail event ID of the cryptographic operation"
//! (`smithy_query(query="shape",shape="KeyLastUsageData")`), and the CloudTrail record of
//! that call carries the same request id as its `requestID`. So one call states ONE
//! request id across all three surfaces — the caller's response, the key's usage record
//! and event history — and the usage record names the event that lookup reports.
//!
//! Neither half is a property of any modelled shape, so no handler can write it: the
//! header is the RESPONSE's and the record is the account's. Both are therefore owned by
//! the layers every call of this service passes through, and this module is what this
//! service contributes to them:
//!
//! * its `eventSource` ([`EVENT_SOURCE`]);
//! * the Region the call was served in and the account it was served for;
//! * the session a call's temporary credential belongs to, so a call made with a role
//!   session is attributed to that session and to the role that issued it;
//! * which values its records do not carry ([`REDACTED_MEMBERS`], [`OMITTED_MEMBERS`]);
//! * which operations REPORT state and which CHANGE it — the one fact a record cannot read
//!   off a request, so it is stated per operation in [`audited_table`];
//! * the id the response names its request by ([`KmsAudit::names_the_request`]), which is
//!   the id the call already stated if it wrote a usage record under one
//!   ([`crate::state::stated_request_id`]) and a freshly minted one otherwise — with
//!   [`ServedCall`] clearing that slot at the START of every served call, so one call's id
//!   can never name another's response.
//!
//! Every request this service serves reaches that table, because it is the routing table
//! this service's definition answers with
//! ([`service_common::audited_operation::AuditedDefinition`], nested inside this service's
//! request-id wrapper) — so no request can be served by a path that records nothing.
//!
//! A KMS call is a MANAGEMENT event, the cryptographic operations included: CloudTrail
//! records them in event history by default, which is why `LookupEvents` — which "looks up
//! management events" — reports an `Encrypt` of this account.
//!
//! Two kinds of answer are NOT records, both for the shared wrapper's own reason — event
//! history is the record of the calls an account's APIs actually SERVED:
//!
//! * a call the scaffold refused as not implemented (answered 501), which reached no
//!   handler;
//! * a request refused before any handler ran — a body that does not deserialize, a member
//!   outside a modelled constraint, an authentication refusal — which is answered without
//!   the typed call a record is built from ever happening. Such a response still names its
//!   request: the definition wrapper in `super` reports the id on it.

use std::sync::Arc;

use aws_smithy_runtime_api::http::Response as RtResponse;
use service_common::audited_operation::{AuditedService, AuditedTable, CallKind};
use service_common::management_events::{
    ManagementEventLog, RecordedSession, ServedApiCall, SessionAuditDirectory,
};
use service_common::query_error_envelope::{named_request, with_stated_request_id_header};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, OperationContext, OperationRoutingTable, PreDispatchProtocol,
    Service,
};

/// This service's model-derived half, routing through the AUDITED operations — the shared
/// wrapper every recording service uses, re-exported under the name this crate holds it by.
pub use service_common::audited_operation::AuditedDefinition;

/// The `eventSource` every record of this service carries: this service's endpoint prefix
/// (`smithy_query(query="service_info")` -> `endpointPrefix: kms`) in the
/// `<prefix>.amazonaws.com` form CloudTrail stamps on every event.
pub const EVENT_SOURCE: &str = "kms.amazonaws.com";

/// The members whose value a record of this service carries the redaction literal in place
/// of: the `@sensitive` members of this model that a call hands over as CREDENTIALS — the
/// password of a custom key store's cluster and the two halves of an external key store's
/// proxy credential (`smithy_query(query="shape",shape="KeyStorePasswordType")` and
/// `smithy_query(query="shape",shape="XksProxyAuthenticationCredentialType")` report
/// `sensitive: true` for each).
const REDACTED_MEMBERS: [&str; 3] = ["KeyStorePassword", "AccessKeyId", "RawSecretAccessKey"];

/// The members a record of this service leaves OUT entirely — no key for them at all: the
/// plaintext a caller hands a cryptographic operation (`Encrypt$Plaintext`, and the
/// `Message` of `Sign`, `Verify`, `GenerateMac` and `VerifyMac`, both `PlaintextType`,
/// which the model marks `@sensitive`).
///
/// AWS's own record of such a call carries the parameters that say WHICH key and under what
/// terms — the key id, the encryption context, the algorithm — and no member for the
/// material itself, so the record states nothing about it rather than stating that
/// something was withheld.
pub const OMITTED_MEMBERS: [&str; 2] = ["Plaintext", "Message"];

/// What this service contributes to the record of each call it serves.
///
/// Holds the shared engine — the record goes into the log that engine carries, the one
/// CloudTrail reads, and the usage record it names lives in this service's own store — so
/// it is generic over the engine type exactly as the service is.
pub struct KmsAudit<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> KmsAudit<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The account's log in that engine, or `None` for an engine that carries none.
    fn log(&self) -> Option<::engine::ServiceState<ManagementEventLog>> {
        management_event_log(&*self.engine)
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for KmsAudit<Eng> {
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
        // The record, the event CloudTrail wrote it as, and the event the account's
        // CloudTrail fans out for it while a trail of the Region is logging calls of its
        // kind; publishing it is the wrapper's. The guard is BOUND rather than used as a
        // temporary, and dropped before this service's own store is reached, so one lock
        // is held at a time.
        let (delivered, recorded_as) = {
            let mut recording = log.lock();
            let delivered = recording.record_delivered(
                call,
                ::std::time::SystemTime::now(),
                answered_as.to_string(),
            );
            // The record just written is the one naming THIS call's request; reading its
            // id back is what lets the usage record name an event a lookup reports.
            let recorded_as = recording
                .iter()
                .rev()
                .find(|event| event.request_id == answered_as)
                .map(|event| event.event_id.clone());
            (delivered, recorded_as)
        };
        // A cryptographic call wrote a usage record under the id its response names
        // ([`crate::state::request_id_of_this_call`]); the event that call was recorded in
        // exists only now, so the record is named from it here.
        if let Some(recorded_as) = recorded_as {
            if let Some(state) = crate::state::service_state(&*self.engine) {
                state
                    .lock()
                    .name_recorded_usage_event(answered_as, &recorded_as);
            }
        }
        delivered
    }

    fn event_source(&self) -> &'static str {
        EVENT_SOURCE
    }

    fn region(&self, ctx: &OperationContext<'_>) -> String {
        // The Region the call was served in, read off the request the way every handler of
        // this service reads it: a KMS endpoint serves one Region and a key lives in the
        // Region it was created in, so a record belongs to the Region its call was signed
        // for.
        crate::state::request_region(ctx)
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

    fn sensitive_members(&self) -> &'static [&'static str] {
        &REDACTED_MEMBERS
    }

    fn omitted_members(&self) -> &'static [&'static str] {
        &OMITTED_MEMBERS
    }

    fn names_the_request(&self, resp: RtResponse) -> (RtResponse, String) {
        // One id per call across every surface that reports it (see the module docs): a
        // call that already stated the id it is answered under — because it wrote that id
        // into the key's usage record — is answered under THAT id, and every other call is
        // named by the shared rule's freshly minted one. Either way the id the record
        // carries is read back off the response, so the caller and the record can never
        // hold two ids for one call.
        match crate::state::stated_request_id() {
            Some(stated) => named_request(with_stated_request_id_header(resp, &stated)),
            None => named_request(resp),
        }
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
/// this service speaks `awsJson1_1` (`smithy_query(query="digest")` reports the one
/// protocol `awsJson1_1`), whose errors carry the fault shape in the body's `__type`
/// rather than in a header — the same value the generated per-operation shim passes, so a
/// refusal raised ahead of a handler reads exactly as it did before.
pub const JSON_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Json {
    content_type: "application/x-amz-json-1.1",
    // The awsJson families convey the error type in the body's `__type`, not in a header.
    errortype_header: false,
};

/// One modelled operation of this service, served with the slot that holds the id of the
/// call CLEARED first.
///
/// NOT a re-route and not an operation of its own: it serves the audited operation the
/// table filed, and adds the one thing that layer cannot — the start of the call, which is
/// where an id a previous call on this thread may still hold is discarded
/// ([`crate::state::forget_any_unstated_request_id`]). Together with
/// [`KmsAudit::names_the_request`], which takes the id when the response is named, that
/// bounds the holding to exactly one served call, the same way the shared recorder bounds
/// its own pending record.
struct ServedCall<O> {
    inner: O,
}

impl<O: ErasedOperation> ErasedOperation for ServedCall<O> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r aws_smithy_runtime_api::http::Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // Whatever this thread may still hold belongs to no call this run serves.
            crate::state::forget_any_unstated_request_id();
            // Nothing is awaited AFTER the framework sequence this delegates to, which is
            // what the shared table's hand-off of the held record rests on.
            self.inner.run(svc, req, ctx).await
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation", which is the operation this wrapper
        // serves — so wrapping cannot weaken the schemes the model declares for it.
        self.inner.auth_schemes()
    }
}

/// The kind of call one row of [`audited_table`] states: `reports` for an operation that
/// only REPORTS state, `mutates` for one that CHANGES the account's resources — which is
/// the event's `readOnly`, and what a lookup can filter on.
///
/// The row's token names the shared table's own value for it ([`CallKind::Reporting`] /
/// [`CallKind::Mutating`]), so nothing about what a kind means is restated here.
macro_rules! call_kind {
    (reports) => {
        CallKind::Reporting
    };
    (mutates) => {
        CallKind::Mutating
    };
}

/// Serve one operation of this service audited, as a call of the stated kind, behind
/// [`ServedCall`] — so every call of this service both records and starts with no id held
/// from another one.
macro_rules! audited {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident => $kind:ident ),* $(,)?
    ) => {{
        $(
            $table.serve_behind(
                crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                call_kind!($kind),
                |audited, _envelope| ServedCall { inner: audited },
            );
        )*
    }};
}

/// Every modelled operation of this service, each constructed from the shared engine and
/// served behind the audit recorder.
///
/// EVERY one is here — the 56 operations `smithy_query(query="list_operations")` lists —
/// the ones this emulator has not implemented included: a stub answers 501 and the wrapper
/// records nothing for it, so an operation implemented later is auditable the moment it
/// serves a call rather than when someone remembers to add it.
///
/// The kind per row is the operation's own documented semantics. `reports` for the calls
/// that change none of the account's resources: the `Describe*` / `Get*` / `List*` readers,
/// and the CRYPTOGRAPHIC operations — encrypting, decrypting, re-encrypting, deriving or
/// generating a data key, signing, verifying, MACing and drawing random bytes all USE a key
/// without changing it or anything else the account holds, which is why CloudTrail records
/// such a call read-only. `mutates` for every operation that creates, enables, disables,
/// rotates, replicates, retires, revokes, tags, re-describes, imports into, schedules the
/// deletion of or otherwise reconfigures something the account then holds.
pub fn audited_table<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> OperationRoutingTable {
    let recorder = Arc::new(KmsAudit::new(Arc::clone(engine)));
    let mut table = AuditedTable::new(recorder, JSON_ENVELOPE);
    audited!(
        table, engine,
        cancel_key_deletion::CancelKeyDeletion => mutates,
        connect_custom_key_store::ConnectCustomKeyStore => mutates,
        create_alias::CreateAlias => mutates,
        create_custom_key_store::CreateCustomKeyStore => mutates,
        create_grant::CreateGrant => mutates,
        create_key::CreateKey => mutates,
        decapsulate::Decapsulate => reports,
        decrypt::Decrypt => reports,
        delete_alias::DeleteAlias => mutates,
        delete_custom_key_store::DeleteCustomKeyStore => mutates,
        delete_imported_key_material::DeleteImportedKeyMaterial => mutates,
        derive_shared_secret::DeriveSharedSecret => reports,
        describe_custom_key_stores::DescribeCustomKeyStores => reports,
        describe_key::DescribeKey => reports,
        disable_key::DisableKey => mutates,
        disable_key_rotation::DisableKeyRotation => mutates,
        disconnect_custom_key_store::DisconnectCustomKeyStore => mutates,
        enable_key::EnableKey => mutates,
        enable_key_rotation::EnableKeyRotation => mutates,
        encapsulate::Encapsulate => reports,
        encrypt::Encrypt => reports,
        generate_data_key::GenerateDataKey => reports,
        generate_data_key_pair::GenerateDataKeyPair => reports,
        generate_data_key_pair_without_plaintext::GenerateDataKeyPairWithoutPlaintext => reports,
        generate_data_key_without_plaintext::GenerateDataKeyWithoutPlaintext => reports,
        generate_mac::GenerateMac => reports,
        generate_random::GenerateRandom => reports,
        get_key_last_usage::GetKeyLastUsage => reports,
        get_key_policy::GetKeyPolicy => reports,
        get_key_rotation_status::GetKeyRotationStatus => reports,
        get_parameters_for_import::GetParametersForImport => reports,
        get_public_key::GetPublicKey => reports,
        import_key_material::ImportKeyMaterial => mutates,
        list_aliases::ListAliases => reports,
        list_grants::ListGrants => reports,
        list_key_policies::ListKeyPolicies => reports,
        list_key_rotations::ListKeyRotations => reports,
        list_keys::ListKeys => reports,
        list_resource_tags::ListResourceTags => reports,
        list_retirable_grants::ListRetirableGrants => reports,
        put_key_policy::PutKeyPolicy => mutates,
        re_encrypt::ReEncrypt => reports,
        replicate_key::ReplicateKey => mutates,
        retire_grant::RetireGrant => mutates,
        revoke_grant::RevokeGrant => mutates,
        rotate_key_on_demand::RotateKeyOnDemand => mutates,
        schedule_key_deletion::ScheduleKeyDeletion => mutates,
        sign::Sign => reports,
        tag_resource::TagResource => mutates,
        untag_resource::UntagResource => mutates,
        update_alias::UpdateAlias => mutates,
        update_custom_key_store::UpdateCustomKeyStore => mutates,
        update_key_description::UpdateKeyDescription => mutates,
        update_primary_region::UpdatePrimaryRegion => mutates,
        verify::Verify => reports,
        verify_mac::VerifyMac => reports,
    );
    table.finish()
}
