// SPDX-License-Identifier: CC0-1.0

//! This front door's AUDIT leg: every call it serves is a management event of
//! `cognito-idp.amazonaws.com`, recorded in the account's event history where CloudTrail's
//! `LookupEvents` reads it.
//!
//! Every control-plane call of an account is a management event, which makes the recording
//! a property of SERVING a call and not of any one operation — so it is not written per
//! handler. The shared wrapper ([`service_common::audited_operation`]) is handed the
//! deserialized input before the handler runs, the handler's own `Result` after, and the
//! rendered response last, and this module is only what THIS service contributes to each
//! record:
//!
//! * its `eventSource` ([`EVENT_SOURCE`]);
//! * the Region and the account the call was served for;
//! * the members whose values its records WITHHOLD ([`SENSITIVE_MEMBERS`]);
//! * the `sub` of the user a call addressed, which is how a record of a call on one user
//!   says WHICH user when the name the request used is a value these records withhold
//!   ([`ReportingTheAddressedUser`]); and
//! * which operations REPORT state and which CHANGE it — the one fact a record cannot read
//!   off a request, so it is stated per operation in [`audited_table`].
//!
//! Every request this service serves reaches that table, because it is the table
//! `Service::override_operation` serves a request from — so no request can be served by a
//! path that records nothing.
//!
//! ## The three outcomes of one call
//!
//! CloudTrail records the call, not the answer: a call that SUCCEEDED, one a handler
//! REFUSED (an unknown user, a pool that is not there) and one the authorization gate in
//! front of the operation refused are all events of this service, told apart by the
//! record's `errorCode` / `errorMessage` and joined to the caller's own response by its
//! `requestID`. The first two are the shared wrapper's: it is handed the handler's
//! `Result`. The third never reaches a handler — the gate answers it before the operation
//! reads anything ([`super::iam_authorization`]) — so that one record is built here
//! ([`record_refused_at_authorization`]) and carries no `requestParameters`, because the
//! call was never dispatched with any.
//!
//! A call this emulator has not implemented answers 501 and is NOT recorded: the wrapper
//! drops it, because event history is the record of the calls an account's APIs actually
//! served. Neither is a request the WAFv2 web ACL in front of a user pool blocked
//! ([`super::BehindWebAcl`]): WAF decides at the edge, so such a request reaches this API
//! no more than it reaches the pool.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use service_common::audited_operation::{
    report_additional_event_data, AuditedService, AuditedTable, CallKind,
};
use service_common::management_events::{
    recorded_caller, recorded_request_parameters, recorded_shape, recorded_user_agent,
    ManagementEventLog, RecordedSession, ServedApiCall, WithheldFromRecord,
};
use service_common::query_error_envelope::RequestIdReportingOperation;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, OperationRoutingTable,
    PreDispatchProtocol, Service,
};

use super::iam_authorization::IamAuthorized;
use super::{BehindWebAcl, WithSubmittedValues};

/// The `eventSource` every record of this service carries: this service's endpoint prefix
/// (`smithy_query(query="service_info")` -> `endpointPrefix: cognito-idp`) in the
/// `<prefix>.amazonaws.com` form CloudTrail stamps on every event.
pub const EVENT_SOURCE: &str = "cognito-idp.amazonaws.com";

/// The request and response members this service's records carry CloudTrail's redaction
/// literal for instead of the value the call carried.
///
/// Every member this model declares `@sensitive`, named as the model spells it — the whole
/// set, checked against the model itself by this module's own test, so a member the model
/// marks sensitive cannot be left out of it. A password, an access or refresh token, an
/// auth session, a TOTP secret, an app client's id and secret and every user ATTRIBUTE
/// value are all declared sensitive by this model, and a user's own name with them: Amazon
/// Cognito's records carry none of those values, and the record of a call on one user
/// states that the call named a user without stating which name it used.
///
/// The set is by member NAME, so it holds for every operation that carries a member of
/// that name — a password on a sign-up and the same password on an admin set are both
/// withheld.
///
/// WHY redacted rather than omitted: CloudTrail substitutes its own fixed literal
/// ([`service_common::management_events::HIDDEN_SENSITIVE_VALUE`]) for a value the serving
/// API declares sensitive, so the record still shows that the call carried the member. That
/// is what lets an auditor see that a caller sent a password without the password ever
/// reaching event history.
pub const SENSITIVE_MEMBERS: &[&str] = &[
    // The user's own name.
    USERNAME_MEMBER,
    // Every password member of the model: on a sign-up, a set, a create's temporary one
    // and both halves of a change.
    "Password",
    "TemporaryPassword",
    "PreviousPassword",
    "ProposedPassword",
    // The tokens a signed-in client holds, the one a revocation names, the one that
    // authorizes feedback on a sign-in event, and the hash a confidential client proves
    // itself with.
    "AccessToken",
    "RefreshToken",
    "IdToken",
    "Token",
    "FeedbackToken",
    "SecretHash",
    "Secret",
    // The auth session a challenge is answered under, the parameters and responses that
    // carry a user's credentials through the auth flows, and the device fingerprint a
    // client sends for threat protection.
    "Session",
    "AuthParameters",
    "ChallengeResponses",
    "UserContextData",
    // The app client's own id and secret: this model declares both sensitive.
    "ClientId",
    "ClientSecret",
    "ClientSecretValue",
    // The TOTP seed `AssociateSoftwareToken` answers with and the code that proves it.
    "SecretCode",
    "UserCode",
    // Every user ATTRIBUTE value (`AttributeType$Value` targets a sensitive shape), the
    // one member that carries a user's email, phone number and `sub`.
    "Value",
];

/// The modelled member a request names the user it addresses by — the one member of
/// [`SENSITIVE_MEMBERS`] the lookup below has to READ, because the user whose `sub` a
/// record names is the user that name resolves to.
const USERNAME_MEMBER: &str = "Username";

/// [`SENSITIVE_MEMBERS`] without [`USERNAME_MEMBER`]: what the user lookup's own walk over
/// a request leaves OUT entirely.
///
/// The lookup reads one member of a request and nothing else, so every other value this
/// service withholds is omitted from the document that walk builds rather than redacted in
/// it — an omitted member is never read at all, so no password, token or secret a call
/// carried is materialised anywhere on the way to the record. This module's own test checks
/// the two sets against each other, so the lookup cannot start reading a value the model
/// marks sensitive by a set drifting out of step.
const SENSITIVE_MEMBERS_BESIDES_THE_USERNAME: &[&str] = &[
    "Password",
    "TemporaryPassword",
    "PreviousPassword",
    "ProposedPassword",
    "AccessToken",
    "RefreshToken",
    "IdToken",
    "Token",
    "FeedbackToken",
    "SecretHash",
    "Secret",
    "Session",
    "AuthParameters",
    "ChallengeResponses",
    "UserContextData",
    "ClientId",
    "ClientSecret",
    "ClientSecretValue",
    "SecretCode",
    "UserCode",
    "Value",
];

/// What the user lookup's walk over a request withholds: every sensitive member except the
/// name of the user the request addresses, and that one by OMISSION (see
/// [`SENSITIVE_MEMBERS_BESIDES_THE_USERNAME`]).
const WITHHELD_FROM_THE_USER_LOOKUP: WithheldFromRecord<'static> = WithheldFromRecord {
    redacted: &[],
    omitted: SENSITIVE_MEMBERS_BESIDES_THE_USERNAME,
};

/// The member a recorded document names the user pool a call addressed by: the modelled
/// `UserPoolId` as [`service_common::management_events::recorded_member_name`] spells it.
const RECORDED_USER_POOL_ID: &str = "userPoolId";

/// The member a recorded document names the addressed user by, spelled the same way.
const RECORDED_USERNAME: &str = "username";

/// The key `additionalEventData` carries the addressed user's own id under.
const RECORDED_SUB: &str = "sub";

/// What this service contributes to the record of each call it serves.
///
/// Holds the shared engine — the record goes into the log that engine carries, the one
/// CloudTrail reads — so it is generic over the engine type exactly as the service is.
pub struct UserPoolAudit<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> UserPoolAudit<Eng> {
    /// Build the recorder from the shared engine.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The account's log in that engine, or `None` for an engine that carries none.
    fn log(&self) -> Option<::engine::ServiceState<ManagementEventLog>> {
        management_event_log(&*self.engine)
    }
}

impl<Eng: Send + Sync + 'static> AuditedService for UserPoolAudit<Eng> {
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
        recording.record_delivered(call, ::std::time::SystemTime::now(), answered_as.to_string())
    }

    fn recorded_session(&self, access_key_id: &str) -> Option<RecordedSession> {
        // The account's own session directory, so a call made with a role session is
        // recorded as that session: the shared store the minting service wrote it to.
        session_audit_directory(&*self.engine)
            .and_then(|directory| directory.lock().session(access_key_id).cloned())
    }

    fn event_source(&self) -> &'static str {
        EVENT_SOURCE
    }

    fn region(&self, ctx: &OperationContext<'_>) -> String {
        // The Region the call was served in, read the way every handler of this service
        // reads it — which is also the Region of every pool id and ARN the call mints.
        crate::state::request_region(ctx.parts())
    }

    fn account_id(&self) -> String {
        ::engine::emulator_account_id().to_string()
    }

    // `resources` is the shared recorder's default: none. The pairs CloudTrail reports in
    // an event's `Resources` are not this fix's to invent — a record naming a resource
    // nothing measured would be reported to a lookup by `ResourceName` as fact — so a
    // record of a call to this service carries the call, its parameters and its outcome,
    // and no resource list.

    fn sensitive_members(&self) -> &'static [&'static str] {
        SENSITIVE_MEMBERS
    }

    // `stopped_at_authorization` is the shared recorder's default: NO refusal a HANDLER of
    // this service answers was decided before dispatch. The one refusal that is — the
    // authorization gate's — never reaches the wrapper at all, and its record is built by
    // [`record_refused_at_authorization`] below with no parameters on it.
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

/// The account's SESSION directory in that same shared engine: the sessions STS minted,
/// each keyed by the access key id it was minted under, which is what lets a record of a
/// call made with one name the session it was made by.
///
/// A SHARED type, for the same reason as the log above: the service that MINTS a session
/// and the services that record the calls made WITH it are not the same service.
pub fn session_audit_directory<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<::service_common::management_events::SessionAuditDirectory>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(
            ::engine::InMemoryEngine::state::<
                ::service_common::management_events::SessionAuditDirectory,
            >,
        )
}

/// Whether a call of `kind` only READ state, as a record's `readOnly` reports it.
///
/// One reading of [`CallKind`] for the one record this module builds itself, so the
/// refusal of a read is recorded as a read exactly as the read itself would have been.
fn read_only(kind: CallKind) -> bool {
    matches!(kind, CallKind::Reporting)
}

/// One modelled operation, served so that the record of its call names the USER the
/// request addressed by that user's own `sub`.
///
/// WHY a record needs it: this model declares a `Username` sensitive, so the record of a
/// call on one user withholds the name the request used — and a record that says a user was
/// read without saying WHICH user cannot answer "who read which user". Amazon Cognito
/// reports the addressed user's `sub` in the event's `additionalEventData` for exactly that
/// reason: the `sub` is the pool's own immutable id for the user, so it identifies the user
/// without carrying a value the API withholds, and it is the same id the pool reports among
/// that user's attributes.
///
/// WHERE it sits: INSIDE the audited call, so the lookup happens while the request can
/// still be read and the user can be resolved as the operation itself resolved it. The user
/// is resolved on BOTH sides of the handler, because which side holds it depends on what
/// the call DID: a call that created the user is resolvable only after it ran, and a call
/// that REMOVED the user only before — a record of a delete names the user it deleted, a
/// user the pool no longer holds. The state after the call is preferred where it resolves,
/// so a create names the user it just made; otherwise the id the pool held for that user
/// when the call arrived is what names it, and a `sub` is the pool's immutable id for a
/// user either way. The facts are reported through the shared slot the audited wrapper
/// takes them from ([`report_additional_event_data`]), as the LAST thing this handler does.
///
/// WHICH calls carry one: those whose request names a pool AND a user, which is every
/// `Admin*` operation on one user of a pool and nothing else — a request naming no user
/// addresses none, and a user name that resolves to no stored user on either side of the
/// call identifies none, so neither reports anything rather than reporting an id nothing
/// resolves.
///
/// WHAT it reports, and nothing else: the `sub` the POOL minted for that user, read from
/// this service's own store. It is an identifier of a resource and not credential material —
/// the pool answers it to every reader of the user's attributes — and it is not the value
/// the request carried: the attribute values a call sends, the name it used and every other
/// member this model marks sensitive stay withheld from the record
/// ([`SENSITIVE_MEMBERS`]), here as everywhere.
pub(crate) struct ReportingTheAddressedUser<O, Eng> {
    /// The operation that serves the call.
    operation: O,
    /// The shared engine, so the user is resolved from the one store this service's
    /// handlers read.
    engine: Arc<Eng>,
}

impl<O, Eng> ReportingTheAddressedUser<O, Eng> {
    /// Wrap `operation`, already constructed from the shared engine.
    pub(crate) fn new(operation: O, engine: Arc<Eng>) -> Self {
        Self { operation, engine }
    }
}

impl<O: Operation, Eng: Send + Sync + 'static> Operation for ReportingTheAddressedUser<O, Eng> {
    type Input = O::Input;
    type Output = O::Output;
    type Error = O::Error;

    const NAME: &'static str = O::NAME;
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = O::INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = O::OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = O::SUCCESS_STATUS;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // Read BEFORE the handler runs, because the handler consumes the input; the pool
        // and the name are what the request addressed, whatever the call then answered.
        let addressed = addressed_user(&recorded_shape(
            &input,
            O::INPUT_SCHEMA,
            WITHHELD_FROM_THE_USER_LOOKUP,
        ));
        // Resolved BEFORE the handler as well, against the state the call arrived at: a
        // call that REMOVES the user it addresses leaves nothing to resolve afterwards, and
        // the record of such a call still names the user it acted on.
        let resolve = |(user_pool_id, username): &(String, String)| {
            stored_sub(&*self.engine, user_pool_id, username)
        };
        let held_before = addressed.as_ref().and_then(resolve);
        let outcome = self.operation.handle(input, ctx).await;
        // And AFTER it, against the state the call leaves behind: the user a create just
        // made is a user the pool holds, and the name a request used resolves the same way
        // the operation itself resolves it (by username, by `sub` or by a sign-in name of
        // the pool). The state after the call wins where it resolves the user, so the id
        // reported is the one the pool answers its readers with now.
        let held_after = addressed.as_ref().and_then(resolve);
        if let Some(sub) = held_after.or(held_before) {
            report_additional_event_data(::serde_json::json!({ RECORDED_SUB: sub }));
        }
        outcome
    }
}

impl<O, Eng> ErasedOperation for ReportingTheAddressedUser<O, Eng>
where
    O: Operation + ErasedOperation,
    Eng: Send + Sync + 'static,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The framework's own per-request sequence, driven with THIS wrapper as the
            // operation, exactly as the audited wrapper around it drives its own: nothing
            // about the wire changes.
            ::smithy_server_runtime::run_operation_http(self, svc, req, ctx, JSON_ENVELOPE).await
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The wrapped operation's modelled schemes: wrapping must not change which
        // credentials sign a call.
        self.operation.auth_schemes()
    }
}

/// The pool and the user name a request addressed, read off the document built from that
/// request ([`WITHHELD_FROM_THE_USER_LOOKUP`]), or `None` for a request that names no
/// single user of a pool.
fn addressed_user(request: &::serde_json::Value) -> Option<(String, String)> {
    let member = |name: &str| {
        request
            .get(name)
            .and_then(::serde_json::Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some((member(RECORDED_USER_POOL_ID)?, member(RECORDED_USERNAME)?))
}

/// The `sub` the pool `user_pool_id` holds for the user `username` names, or `None` when
/// this engine holds no such pool or no such user.
///
/// A READ of this service's own store, taken the way its handlers take one: the name is
/// resolved by the pool itself, so the user found is the user the call addressed.
fn stored_sub<Eng: Send + Sync + 'static>(
    engine: &Eng,
    user_pool_id: &str,
    username: &str,
) -> Option<String> {
    let state = crate::state::service_state(engine)?;
    let guard = state.lock();
    let user = guard.pool(user_pool_id)?.user(username)?;
    user.attributes.get(crate::state::SUB_ATTRIBUTE).cloned()
}

/// The `errorCode` CloudTrail records an IAM authorization denial under.
///
/// It is the AUTHORIZATION layer's own code and not the error code the API answered its
/// caller with: the caller of a refused call to this endpoint is told
/// `AccessDeniedException` (the AWS common error this model declares no shape for, see
/// [`super::iam_authorization`]), while the record of that call carries this. The two are
/// read by different clients — an SDK branches on the code it was answered with, a detection
/// rule matches on the code in event history — so both are stated, each where it belongs.
pub const RECORDED_ACCESS_DENIED_CODE: &str = "AccessDenied";

/// Record the call the authorization gate REFUSED before the operation ran, as a management
/// event of this service.
///
/// Such a call reached no handler, so the shared wrapper never sees it — and it is still a
/// call this account's API served an answer to, which is what event history holds. The
/// record carries the authorization layer's own code ([`RECORDED_ACCESS_DENIED_CODE`]) with
/// the message the caller was answered with, and NO `requestParameters`: those report what a
/// call that was authorized and dispatched carried, and this one was neither
/// ([`service_common::management_events::recorded_request_parameters`]).
///
/// The record is HELD rather than written, exactly as the wrapper holds its own: it names
/// the request by the id the CALLER was answered under, and that id exists only once the
/// response has been rendered. The layer every operation of this service is served behind
/// takes it from there, writes it with that id and makes the deliveries the account's
/// CloudTrail makes for it. Called as the LAST thing before the refusal is answered, with
/// nothing awaited in between, which is what that hand-off rests on.
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
        .and_then(|access_key_id| {
            session_audit_directory(engine)
                .and_then(|directory| directory.lock().session(access_key_id).cloned())
        });
    ::service_common::audited_operation::hold_management_event(ServedApiCall {
        event_name: operation.to_string(),
        event_source: EVENT_SOURCE.to_string(),
        aws_region: crate::state::request_region(ctx.parts()),
        read_only: read_only(kind),
        access_key_id: caller.access_key_id,
        user_name: session
            .as_ref()
            .map_or(caller.user_name, |session| session.user_name.clone()),
        principal_type: session.as_ref().map_or_else(
            || caller.principal_type.to_string(),
            |session| session.principal_type.clone(),
        ),
        // The unique id of the IAM entity a LONG-TERM credential speaks for is IAM's to
        // mint, and this service resolves none, so such a record leaves it out rather than
        // naming a principal nothing resolves. A session's own principal id is the
        // session's, and the directory holds it.
        principal_id: session.as_ref().map(|session| session.principal_id.clone()),
        // A call signed with an AWS credential was authenticated by this account, not by an
        // external provider.
        identity_provider: None,
        // The refused caller made the call itself.
        invoked_by: None,
        session,
        account_id: ::engine::emulator_account_id().to_string(),
        user_agent: recorded_user_agent(ctx.parts()),
        resources: Vec::new(),
        request_parameters: recorded_request_parameters(::serde_json::Value::Null, true),
        response_elements: None,
        // Nothing beside the parameters: a call this gate stopped addressed whatever it
        // addressed, and the gate resolved none of it.
        additional_event_data: None,
        error_code: Some(RECORDED_ACCESS_DENIED_CODE.to_string()),
        error_message: Some(message.to_string()),
    });
}

/// The envelope a pre-dispatch failure of one of this service's operations renders in:
/// this service speaks `awsJson1_1`, which is the same value the generated per-operation
/// shim passes.
const JSON_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Json {
    content_type: "application/x-amz-json-1.1",
    // The awsJson families convey the error type in the body's `__type`, not in a header.
    errortype_header: false,
};

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

/// Serve one operation of this service audited, as a call of the stated kind.
///
/// The audited call is the INNERMOST layer but one, so the record a lookup reports is the
/// operation's own input and outcome whatever the layers outside it do to the response.
/// Inside it sits [`ReportingTheAddressedUser`], so the record of a call on one user names
/// that user by the `sub` the pool holds for it. Outside it are the layers this service
/// already serves every operation behind, in the same order: [`WithSubmittedValues`], so a
/// request refused before the handler runs states the collection the caller submitted;
/// [`IamAuthorized`], so an operation the model requires a signature for is served only to
/// a caller IAM allows to perform it — and the call it refuses is recorded by
/// [`record_refused_at_authorization`], which is why the gate is told the row's kind;
/// [`BehindWebAcl`], so a pool with a web ACL associated is decided by that web ACL first;
/// and, outermost, [`RequestIdReportingOperation::header`], so every response names the
/// request it answers — which is the id the record carries.
macro_rules! audited {
    (
        $table:expr, $engine:expr,
        $( $module:ident :: $operation:ident => $kind:ident ),* $(,)?
    ) => {{
        $(
            {
                let for_web_acl = ::std::sync::Arc::clone($engine);
                let for_iam = ::std::sync::Arc::clone($engine);
                $table.serve_behind(
                    ReportingTheAddressedUser::new(
                        crate::ops::$module::$operation::new(::std::sync::Arc::clone($engine)),
                        ::std::sync::Arc::clone($engine),
                    ),
                    call_kind!($kind),
                    move |audited, _pre_dispatch| {
                        RequestIdReportingOperation::header(BehindWebAcl::new(
                            stringify!($operation),
                            for_web_acl,
                            ::std::boxed::Box::new(IamAuthorized::new(
                                stringify!($operation),
                                for_iam,
                                call_kind!($kind),
                                ::std::boxed::Box::new(WithSubmittedValues::new(audited))
                                    as ::std::boxed::Box<dyn ErasedOperation>,
                            )) as ::std::boxed::Box<dyn ErasedOperation>,
                        ))
                    },
                );
            }
        )*
    }};
}

/// Every modelled operation of this service, each constructed from the shared engine and
/// served AUDITED behind the layers this service serves its operations behind.
///
/// EVERY one is here, the ones this emulator has not implemented included: a stub answers
/// 501 and the wrapper records nothing for it, so an operation implemented later is
/// auditable the moment it serves a call rather than when someone remembers to add it.
/// This table is keyed by each operation's own shape name — the routing key
/// `ServerHttpProtocol::routing_key` resolves and the generated table is keyed by — and
/// this service's own test over the MODEL checks that every modelled operation is in it.
///
/// The kind per row is the operation's own semantics: `reports` for the `Get*` / `List*` /
/// `Describe*` readers that answer a question about the account's pools and change none of
/// it, and `mutates` for every operation that creates, changes or removes something the
/// account then holds — which includes every call that MINTS or rotates a credential
/// (`InitiateAuth` and the admin and challenge-response flows, `GetClientToken`,
/// `GetTokensFromRefreshToken`, `AssociateSoftwareToken`, `StartWebAuthnRegistration`), one
/// that SENDS a message (`ForgotPassword`, `ResendConfirmationCode`,
/// `GetUserAttributeVerificationCode`) and one that signs a session out.
pub fn audited_table<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> OperationRoutingTable {
    let recorder = Arc::new(UserPoolAudit::new(Arc::clone(engine)));
    let mut table = AuditedTable::new(recorder, JSON_ENVELOPE);
    audited!(
        table, engine,
        add_custom_attributes::AddCustomAttributes => mutates,
        add_user_pool_client_secret::AddUserPoolClientSecret => mutates,
        admin_add_user_to_group::AdminAddUserToGroup => mutates,
        admin_confirm_sign_up::AdminConfirmSignUp => mutates,
        admin_create_user::AdminCreateUser => mutates,
        admin_delete_software_token::AdminDeleteSoftwareToken => mutates,
        admin_delete_user::AdminDeleteUser => mutates,
        admin_delete_user_attributes::AdminDeleteUserAttributes => mutates,
        admin_disable_provider_for_user::AdminDisableProviderForUser => mutates,
        admin_disable_user::AdminDisableUser => mutates,
        admin_enable_user::AdminEnableUser => mutates,
        admin_forget_device::AdminForgetDevice => mutates,
        admin_get_device::AdminGetDevice => reports,
        admin_get_user::AdminGetUser => reports,
        admin_get_user_auth_factors::AdminGetUserAuthFactors => reports,
        admin_initiate_auth::AdminInitiateAuth => mutates,
        admin_link_provider_for_user::AdminLinkProviderForUser => mutates,
        admin_list_devices::AdminListDevices => reports,
        admin_list_groups_for_user::AdminListGroupsForUser => reports,
        admin_list_user_auth_events::AdminListUserAuthEvents => reports,
        admin_remove_user_from_group::AdminRemoveUserFromGroup => mutates,
        admin_reset_user_password::AdminResetUserPassword => mutates,
        admin_respond_to_auth_challenge::AdminRespondToAuthChallenge => mutates,
        admin_set_user_mfa_preference::AdminSetUserMFAPreference => mutates,
        admin_set_user_password::AdminSetUserPassword => mutates,
        admin_set_user_settings::AdminSetUserSettings => mutates,
        admin_update_auth_event_feedback::AdminUpdateAuthEventFeedback => mutates,
        admin_update_device_status::AdminUpdateDeviceStatus => mutates,
        admin_update_user_attributes::AdminUpdateUserAttributes => mutates,
        admin_user_global_sign_out::AdminUserGlobalSignOut => mutates,
        associate_software_token::AssociateSoftwareToken => mutates,
        change_password::ChangePassword => mutates,
        complete_web_authn_registration::CompleteWebAuthnRegistration => mutates,
        confirm_device::ConfirmDevice => mutates,
        confirm_forgot_password::ConfirmForgotPassword => mutates,
        confirm_sign_up::ConfirmSignUp => mutates,
        create_group::CreateGroup => mutates,
        create_identity_provider::CreateIdentityProvider => mutates,
        create_managed_login_branding::CreateManagedLoginBranding => mutates,
        create_resource_server::CreateResourceServer => mutates,
        create_terms::CreateTerms => mutates,
        create_user_import_job::CreateUserImportJob => mutates,
        create_user_pool::CreateUserPool => mutates,
        create_user_pool_client::CreateUserPoolClient => mutates,
        create_user_pool_domain::CreateUserPoolDomain => mutates,
        create_user_pool_replica::CreateUserPoolReplica => mutates,
        delete_group::DeleteGroup => mutates,
        delete_identity_provider::DeleteIdentityProvider => mutates,
        delete_managed_login_branding::DeleteManagedLoginBranding => mutates,
        delete_resource_server::DeleteResourceServer => mutates,
        delete_terms::DeleteTerms => mutates,
        delete_user::DeleteUser => mutates,
        delete_user_attributes::DeleteUserAttributes => mutates,
        delete_user_pool::DeleteUserPool => mutates,
        delete_user_pool_client::DeleteUserPoolClient => mutates,
        delete_user_pool_client_secret::DeleteUserPoolClientSecret => mutates,
        delete_user_pool_domain::DeleteUserPoolDomain => mutates,
        delete_user_pool_replica::DeleteUserPoolReplica => mutates,
        delete_web_authn_credential::DeleteWebAuthnCredential => mutates,
        describe_identity_provider::DescribeIdentityProvider => reports,
        describe_managed_login_branding::DescribeManagedLoginBranding => reports,
        describe_managed_login_branding_by_client::DescribeManagedLoginBrandingByClient => reports,
        describe_resource_server::DescribeResourceServer => reports,
        describe_risk_configuration::DescribeRiskConfiguration => reports,
        describe_terms::DescribeTerms => reports,
        describe_terms_by_client::DescribeTermsByClient => reports,
        describe_user_import_job::DescribeUserImportJob => reports,
        describe_user_pool::DescribeUserPool => reports,
        describe_user_pool_client::DescribeUserPoolClient => reports,
        describe_user_pool_domain::DescribeUserPoolDomain => reports,
        forget_device::ForgetDevice => mutates,
        forgot_password::ForgotPassword => mutates,
        get_client_token::GetClientToken => mutates,
        get_csv_header::GetCSVHeader => reports,
        get_device::GetDevice => reports,
        get_group::GetGroup => reports,
        get_identity_provider_by_identifier::GetIdentityProviderByIdentifier => reports,
        get_log_delivery_configuration::GetLogDeliveryConfiguration => reports,
        get_provisioned_limit::GetProvisionedLimit => reports,
        get_signing_certificate::GetSigningCertificate => reports,
        get_tokens_from_refresh_token::GetTokensFromRefreshToken => mutates,
        get_ui_customization::GetUICustomization => reports,
        get_user::GetUser => reports,
        get_user_attribute_verification_code::GetUserAttributeVerificationCode => mutates,
        get_user_auth_factors::GetUserAuthFactors => reports,
        get_user_pool_mfa_config::GetUserPoolMfaConfig => reports,
        global_sign_out::GlobalSignOut => mutates,
        initiate_auth::InitiateAuth => mutates,
        list_devices::ListDevices => reports,
        list_groups::ListGroups => reports,
        list_identity_providers::ListIdentityProviders => reports,
        list_resource_servers::ListResourceServers => reports,
        list_tags_for_resource::ListTagsForResource => reports,
        list_terms::ListTerms => reports,
        list_user_import_jobs::ListUserImportJobs => reports,
        list_user_pool_client_secrets::ListUserPoolClientSecrets => reports,
        list_user_pool_clients::ListUserPoolClients => reports,
        list_user_pool_replicas::ListUserPoolReplicas => reports,
        list_user_pools::ListUserPools => reports,
        list_users::ListUsers => reports,
        list_users_in_group::ListUsersInGroup => reports,
        list_web_authn_credentials::ListWebAuthnCredentials => reports,
        resend_confirmation_code::ResendConfirmationCode => mutates,
        respond_to_auth_challenge::RespondToAuthChallenge => mutates,
        revoke_token::RevokeToken => mutates,
        set_log_delivery_configuration::SetLogDeliveryConfiguration => mutates,
        set_risk_configuration::SetRiskConfiguration => mutates,
        set_ui_customization::SetUICustomization => mutates,
        set_user_mfa_preference::SetUserMFAPreference => mutates,
        set_user_pool_mfa_config::SetUserPoolMfaConfig => mutates,
        set_user_settings::SetUserSettings => mutates,
        sign_up::SignUp => mutates,
        start_user_import_job::StartUserImportJob => mutates,
        start_web_authn_registration::StartWebAuthnRegistration => mutates,
        stop_user_import_job::StopUserImportJob => mutates,
        tag_resource::TagResource => mutates,
        untag_resource::UntagResource => mutates,
        update_auth_event_feedback::UpdateAuthEventFeedback => mutates,
        update_device_status::UpdateDeviceStatus => mutates,
        update_group::UpdateGroup => mutates,
        update_identity_provider::UpdateIdentityProvider => mutates,
        update_managed_login_branding::UpdateManagedLoginBranding => mutates,
        update_provisioned_limit::UpdateProvisionedLimit => mutates,
        update_resource_server::UpdateResourceServer => mutates,
        update_terms::UpdateTerms => mutates,
        update_user_attributes::UpdateUserAttributes => mutates,
        update_user_pool::UpdateUserPool => mutates,
        update_user_pool_client::UpdateUserPoolClient => mutates,
        update_user_pool_domain::UpdateUserPoolDomain => mutates,
        update_user_pool_replica::UpdateUserPoolReplica => mutates,
        verify_software_token::VerifySoftwareToken => mutates,
        verify_user_attribute::VerifyUserAttribute => mutates,
    );
    table.finish()
}

#[cfg(test)]
mod tests {
    use super::{
        SENSITIVE_MEMBERS, SENSITIVE_MEMBERS_BESIDES_THE_USERNAME, USERNAME_MEMBER,
    };
    use std::collections::BTreeSet;

    /// This service's Smithy model, as it ships beside this crate. Read at test time
    /// rather than embedded: it is megabytes of JSON that no non-test build needs.
    const MODEL_PATH: &str = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/smithy-model/com-amazonaws-cognitoidentityprovider-awscognitoidentityproviderservice.json"
    );

    /// The Smithy trait a model declares a value sensitive with.
    const SENSITIVE_TRAIT: &str = "smithy.api#sensitive";

    /// Whether `shape` carries the sensitive trait itself.
    fn declared_sensitive(shape: &serde_json::Value) -> bool {
        shape
            .get("traits")
            .and_then(serde_json::Value::as_object)
            .is_some_and(|traits| traits.contains_key(SENSITIVE_TRAIT))
    }

    /// RULE applied to the whole surface: every member this MODEL declares sensitive — by
    /// the trait on the member or by the trait on the shape it targets — is a member this
    /// service's records withhold.
    ///
    /// The set comes from the model rather than from a second hand list, so a sensitive
    /// member added to the model and not to [`SENSITIVE_MEMBERS`] fails here instead of
    /// quietly writing a secret into the account's event history.
    #[test]
    fn every_sensitive_member_of_the_model_is_withheld_from_a_record() {
        let model = std::fs::read_to_string(MODEL_PATH)
            .expect("the model that generated this crate ships beside it");
        let model: serde_json::Value =
            serde_json::from_str(&model).expect("the model that generated this crate is JSON");
        let shapes = model["shapes"]
            .as_object()
            .expect("a Smithy model declares shapes");
        let mut sensitive: BTreeSet<&str> = BTreeSet::new();
        for shape in shapes.values() {
            let Some(members) = shape.get("members").and_then(serde_json::Value::as_object) else {
                continue;
            };
            for (name, member) in members {
                let target = member
                    .get("target")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|target| shapes.get(target));
                if declared_sensitive(member) || target.is_some_and(declared_sensitive) {
                    sensitive.insert(name.as_str());
                }
            }
        }
        assert!(
            sensitive.len() > 10,
            "case the model's surface: expected the model's sensitive members, got \
             {sensitive:?}"
        );
        let withheld: BTreeSet<&str> = SENSITIVE_MEMBERS.iter().copied().collect();
        let missing: Vec<&str> = sensitive.difference(&withheld).copied().collect();
        assert!(
            missing.is_empty(),
            "case the model's sensitive members: every one is withheld from a record; these \
             are not: {missing:?}"
        );
        let unmodelled: Vec<&str> = withheld.difference(&sensitive).copied().collect();
        assert!(
            unmodelled.is_empty(),
            "case the withheld members: each one is a member this model declares sensitive; \
             these are not: {unmodelled:?}"
        );
    }

    /// RULE: the set the user lookup's walk withholds is every member the records withhold
    /// except the one name that lookup READS — so no value this model marks sensitive can
    /// be materialised by that walk, whatever the record's own set grows to hold.
    #[test]
    fn the_user_lookup_reads_the_username_and_withholds_every_other_sensitive_member() {
        let expected: Vec<&str> = SENSITIVE_MEMBERS
            .iter()
            .copied()
            .filter(|member| *member != USERNAME_MEMBER)
            .collect();
        assert_eq!(
            SENSITIVE_MEMBERS_BESIDES_THE_USERNAME, expected.as_slice(),
            "case the lookup's withheld set: expected {expected:?}, got {:?}",
            SENSITIVE_MEMBERS_BESIDES_THE_USERNAME
        );
        assert!(
            SENSITIVE_MEMBERS.contains(&USERNAME_MEMBER),
            "case the member the lookup reads: a record withholds it, got {SENSITIVE_MEMBERS:?}"
        );
    }
}
