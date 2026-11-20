// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIAL a data-source call this endpoint SIGNS is made with: a session of the role
//! the data source names.
//!
//! "The system assumes this role when accessing the data source" (`DataSource$serviceRoleArn`),
//! so a call AppSync signs on a resolver's behalf is made BY THAT ROLE: the service it reaches
//! resolves the role's own session as its caller, and that service's own authorization decides
//! the call — which is what every least-privilege assumption about a data source rests on. Two
//! data sources whose roles carry different grants are refused different things, and the
//! refusal a field is handed is the one the CALLED service worded, not one this endpoint
//! invented for it.
//!
//! This module is where that credential comes from:
//!
//! * the role is resolved against the IAM store of the SAME shared engine, which is where the
//!   account's roles live, so the session speaks for the very record `iam:CreateRole` wrote;
//! * the session is minted by the TOKEN SERVICE's own mint
//!   ([`sts_store::mint_role_session`]), so a call signed with it is resolved as that session
//!   wherever a temporary credential's principal is read, and the credential is one the
//!   emulator's own request verifier accepts. Nothing here constructs an access key id, a
//!   secret or a session token, and none of them is ever reported to a caller, written to a
//!   file or logged;
//! * its identity is the assumed-role session an assume vends, under the session name this
//!   endpoint opens one with — the same principal a refusal of such a call names
//!   ([`super::data_source_role::assumed_role_principal`]), so the two cannot disagree;
//! * it lives for ONE call: the caller forgets it as soon as that call returns
//!   ([`forget`]), in the credential store (so a request presenting it no longer verifies) and
//!   in the token service's registry (so it names no identity either), on every path out.
//!
//! The credential store is reached through the framework's own context constructor over the
//! callback this plane holds, because a request on this plane has no operation context of its
//! own — the seam another data plane of this emulator reaches it through as well
//! (`service-com-amazonaws-apigatewayv2-apigatewayv2`'s `service::invoke`). Where nothing can
//! be SHOWN about the role — this emulator holds no IAM record of it (a data source's service
//! role is routinely provisioned outside the client that creates the data source), there is no
//! token service to mint a session with, or the mint is refused — no session is minted and the
//! call is signed with the credential the framework hands this emulator's handlers, which is
//! what every other in-process call between two emulated services presents. Refusing on a role
//! this emulator knows nothing about would deny calls AWS permits.
//!
//! TO LIFT: this is the fourth reading of "hold a session of a role for the work a service does
//! as it" in this tree — `service-com-amazonaws-sfn-awsstepfunctions`'s
//! `state::execution_role_session`, `service-com-amazonaws-lambda-awsgirapiservice`'s
//! `state::execution::execution_role_session` and
//! `service-com-amazonaws-apigatewayv2-apigatewayv2`'s `service::invoke::service_action` each
//! vend one their own way. The shared home for it is `services/common`, which cannot host it
//! today: the holder has to call the token service's own mint, and that service's crate depends
//! on `services/common`, so the dependency would be a cycle. Whoever lifts it takes the mint
//! with it and takes every reading above together.

use ::std::time::SystemTime;

use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

use super::PlaneContext;

/// How long a minted session lasts.
///
/// `sts:AssumeRole` documents 3600 seconds as the default duration of a role session
/// ("`DurationSeconds` … Default: 3600 seconds"), and a session is never minted for longer than
/// the role itself admits (`Role$MaxSessionDuration`), so a role configured for a shorter
/// maximum gets a shorter session. One call is served well inside it either way.
const SESSION_SECONDS: i32 = 3600;

/// The range `Role$MaxSessionDuration` may carry: "the maximum session duration (in seconds) …
/// Minimum value of 3600. Maximum value of 43200" (`UpdateRole`'s own documentation of the
/// member). A stored value outside it is not a maximum AWS could have recorded, so it caps
/// nothing here and the default duration stands.
const MAX_SESSION_DURATION_RANGE: ::std::ops::RangeInclusive<i32> = 3600..=43_200;

/// This service's own identity, as its model publishes it (`smithy_query(query="service_info")`:
/// the service shape and its `sigv4Name`). It names the context this module builds to reach the
/// emulator's credential store; nothing of it travels on a call.
const SERVICE_SHAPE_ID: &str = "com.amazonaws.appsync#AWSDeepdishControlPlaneService";
const SIGNING_NAME: &str = "appsync";

/// The credential one signed data-source call presents, and the session it speaks for.
///
/// Its `Debug` form carries no secret: the id names the credential, which is what a refusal of
/// the call is read back by.
#[derive(Clone)]
pub struct RoleSession {
    /// The access key id the call presents, which is what the serving side resolves the session
    /// by.
    pub access_key_id: String,
    /// The secret the call is signed with, and the session token it presents beside it.
    pub secret_access_key: String,
    pub session_token: String,
}

impl ::std::fmt::Debug for RoleSession {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("RoleSession")
            .field("access_key_id", &self.access_key_id)
            .finish_non_exhaustive()
    }
}

/// Mint the session ONE signed call of a data source naming `role_arn` is made with, or `None`
/// where nothing can be shown about that role (see the module header), which leaves the call to
/// be signed with this emulator's own credential.
#[must_use]
pub fn minted(context: &PlaneContext, role_arn: &str) -> Option<RoleSession> {
    let role = held_role(context, role_arn)?;
    let registry = sts_store::service_state(&*context.engine)?;
    let identity = sts_store::Identity {
        user_id: format!("{}:{}", role.id, super::data_source_role::ROLE_SESSION_NAME),
        account: role.account.clone(),
        // The session ARN a refusal of such a call names, built in the one place this crate
        // builds it, so the principal the called service blames is the one this session is.
        arn: super::data_source_role::assumed_role_principal(&role.arn),
    };
    let request = sts_store::NewSession {
        role_arn: Some(role.arn.clone()),
        role_id: Some(role.id.clone()),
        ..sts_store::NewSession::new(
            identity,
            sts_store::SessionKind::AssumedRole,
            SESSION_SECONDS.min(role.max_session_duration),
        )
    };
    let parts = empty_parts()?;
    let service = ::smithy_server_runtime::ServiceIdentity::new(SERVICE_SHAPE_ID, SIGNING_NAME);
    let ctx = ::smithy_server_runtime::OperationContext::new(
        &parts,
        &service,
        context.callback.clone(),
    );
    let minted = sts_store::mint_role_session(
        &*context.engine,
        ctx.credential_store(),
        &registry,
        SystemTime::now(),
        request,
    )
    .ok()?;
    Some(RoleSession {
        access_key_id: minted.credentials.access_key_id,
        secret_access_key: minted.credentials.secret_access_key,
        session_token: minted.credentials.session_token,
    })
}

/// Forget the session `access_key_id` names, in both stores that held it: the credential store
/// (so a request presenting it no longer verifies) and the token service's own registry (so it
/// names no identity either).
///
/// Called on every path out of the call the session was minted for, so a credential no call
/// presents any more is valid nowhere.
pub fn forget(context: &PlaneContext, access_key_id: &str) {
    sts_store::forget_audited_session(&*context.engine, access_key_id);
    let Some(parts) = empty_parts() else {
        return;
    };
    let service = ::smithy_server_runtime::ServiceIdentity::new(SERVICE_SHAPE_ID, SIGNING_NAME);
    let ctx = ::smithy_server_runtime::OperationContext::new(
        &parts,
        &service,
        context.callback.clone(),
    );
    match sts_store::service_state(&*context.engine) {
        Some(registry) => registry.lock().discard(access_key_id, ctx.credential_store()),
        None => {
            let _ = ctx.credential_store().delete(access_key_id);
        }
    }
}

/// What the IAM store holds about one data-source role, as a session of it is minted from.
struct ResolvedRole {
    /// The role's ARN, exactly as IAM holds it — the spelling a policy names it by.
    arn: String,
    /// Its unique id, which the session's own `AssumedRoleId` is built from.
    id: String,
    /// The account whose IAM holds it, read off its ARN.
    account: String,
    /// "The maximum session duration (in seconds) that you want to set for the specified role"
    /// (`Role$MaxSessionDuration`), which caps the session minted here.
    max_session_duration: i32,
}

/// The role `role_arn` names, when the IAM store of this engine holds one under it.
///
/// `None` when nothing is known about the role: no IAM store on this engine, a value that names
/// no ARN at all, or a name this account's IAM holds no role under.
fn held_role(context: &PlaneContext, role_arn: &str) -> Option<ResolvedRole> {
    let parsed = ::service_common::arn::parse(role_arn)?;
    let iam = ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(
        &*context.engine,
    )?;
    let held = iam.lock();
    let role = held.roles.values().find(|role| role.arn == role_arn)?;
    Some(ResolvedRole {
        arn: role.arn.clone(),
        id: role.role_id.clone(),
        account: parsed.account.to_string(),
        max_session_duration: role
            .max_session_duration
            .filter(|duration| MAX_SESSION_DURATION_RANGE.contains(duration))
            .unwrap_or(SESSION_SECONDS),
    })
}

/// The request head the framework's own context constructor takes, for a context built to reach
/// the emulator's credential store rather than to serve a request: a request on this plane has
/// none of its own, and nothing of this head is read.
fn empty_parts() -> Option<::http::request::Parts> {
    ::http::Request::builder()
        .uri("/")
        .body(())
        .ok()
        .map(|request| request.into_parts().0)
}
