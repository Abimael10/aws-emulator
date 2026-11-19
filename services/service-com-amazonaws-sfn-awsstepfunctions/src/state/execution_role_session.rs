// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIAL an execution's integration calls are made with: a SESSION of the
//! state machine's execution role.
//!
//! "The Amazon Resource Name (ARN) of the IAM role to use for this state machine"
//! (`CreateStateMachine$roleArn`) is the identity the service does the execution's work
//! as: Step Functions assumes that role and calls each integration with the temporary
//! credentials the assume vends, so the call is made BY THE ROLE. The service serving
//! it therefore resolves the role's own session as its caller — which is what
//! `sts:GetCallerIdentity` reports for an AWS SDK integration, and what every
//! least-privilege assumption about a state machine rests on: two state machines whose
//! roles carry different grants are refused different things, and anything keyed on the
//! caller identity sees the role rather than whoever runs the emulator.
//!
//! This module is where that credential comes from:
//!
//! * the role is resolved against the IAM store of the SAME shared engine, which is
//!   where the account's roles live, so the session speaks for the very record
//!   `iam:CreateRole` wrote;
//! * the session is minted by the TOKEN SERVICE's own mint, so a call signed with it is
//!   resolved as that session wherever a temporary credential's principal is read, and
//!   the credential is one the emulator's own request verifier accepts. Nothing here
//!   constructs an access key id, a secret or a session token;
//! * its identity is the assumed-role session an assume vends:
//!   `arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>`, under
//!   the session name this service opens the session with — the same name a refusal of
//!   one of the execution's calls blames ([`super::task_authorization`]);
//! * it belongs to the ROLE rather than to one call: the credentials a real execution
//!   assumes are held for the run, so one session is held per role here and re-minted
//!   once the clock has brought it near enough to its expiry that a run could outlast
//!   it ([`REFRESH_MARGIN`]). The one it replaces is given up in the credential store,
//!   so a request presenting it no longer verifies, and in the token service's
//!   registry, so it names no identity either.
//!
//! Where nothing can be SHOWN about the role — this emulator holds no IAM record of it
//! (an execution role is normally provisioned outside the client that creates the state
//! machine, which is why `CreateStateMachine` serves a `roleArn` the account does not
//! hold), there is no token service to mint a session with, or the call that runs the
//! walk reaches no credential store (a continuation the clock runs rather than a
//! request) — no session is minted and the calls are signed with the credential the
//! framework hands this emulator's handlers, which is what every other in-process call
//! between two emulated services presents. Refusing on a role this emulator knows
//! nothing about would deny calls AWS permits.
//!
//! The credential never reaches the caller: it is held by this crate, put on the
//! `Authorization` and session-token headers of the calls the walk makes
//! ([`super::integration`]), and in no response, log or history event of the execution.
//!
//! TO LIFT: this is the SECOND reading of "hold a session of a role for the work a
//! service does as it" in this tree; the first is
//! `services/service-com-amazonaws-lambda-awsgirapiservice/src/state/execution/execution_role_session.rs`
//! (a function's own calls), and `service-com-amazonaws-eksauth-eksauthfrontend`,
//! `service-com-amazonaws-s3control-awss3controlservicev20180820` and
//! `service-com-amazonaws-apigatewayv2-apigatewayv2` each vend such a session their own
//! way. The shared home for it is `services/common`, which CANNOT host it today: the
//! holder has to call the token service's own mint, and that service's crate depends on
//! `services/common` (`service-com-amazonaws-sts-…/Cargo.toml`), so the dependency would
//! be a cycle. Whoever lifts it takes the mint with it — either into the vended
//! framework beside the credential store or into a crate under `services/` that the
//! token service's crate may depend on — and takes every reading above together.

use ::std::collections::BTreeMap;
use ::std::time::{Duration, SystemTime};

use ::emulator_server::CredentialStore;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// How long a minted session lasts.
///
/// `sts:AssumeRole` documents 3600 seconds as the default duration of a role session
/// ("`DurationSeconds` … Default: 3600 seconds"), and a session is never minted for
/// longer than the role itself admits (`Role$MaxSessionDuration`), so a role configured
/// for a shorter maximum gets a shorter session.
const SESSION_SECONDS: i32 = 3600;

/// The range `Role$MaxSessionDuration` may carry: "the maximum session duration (in
/// seconds) … Minimum value of 3600. Maximum value of 43200" (`UpdateRole`'s own
/// documentation of the member). A stored value outside it is not a maximum AWS could
/// have recorded, so it caps nothing here and the default duration stands.
const MAX_SESSION_DURATION_RANGE: ::std::ops::RangeInclusive<i32> = 3600..=43_200;

/// How much of a held session's life has to be left for it to be used again.
///
/// A run may wait for the calls its `Task` states make for as long as this emulator
/// lets one walk wait ([`super::asl::MAX_CALL_WAIT_DURATION`]), so a session with at
/// least that much left cannot expire in the middle of a run that starts now; one with
/// less is re-minted first. Stated as a span and not as a count of executions, because
/// what makes a credential unusable is the CLOCK.
const REFRESH_MARGIN: Duration = super::asl::MAX_CALL_WAIT_DURATION;

/// The credential one execution's calls are signed with, and the session it speaks for.
///
/// Its `Debug` form carries no secret: the id names the credential, which is what a
/// refusal of a call is read back by.
#[derive(Clone)]
pub struct RoleSession {
    /// The access key id the call presents, which is what the serving side resolves the
    /// session by.
    pub access_key_id: String,
    /// The secret the call is signed with, and the session token it presents beside it.
    pub secret_access_key: String,
    pub session_token: Option<String>,
    /// The name the session was opened under, which its identity's ARN carries.
    pub session_name: String,
}

impl ::std::fmt::Debug for RoleSession {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("RoleSession")
            .field("access_key_id", &self.access_key_id)
            .field("session_name", &self.session_name)
            .finish_non_exhaustive()
    }
}

/// The sessions this emulator holds for its state machines' execution roles, keyed by
/// the role.
///
/// By the ROLE and not by the execution: what the identity of a call has to say is which
/// role the work is done as — that is what a service authorizing it reads and what a
/// refusal blames — so the credentials are held for the role, as a service holding
/// credentials for the work it does as one would. A session per execution would mint a
/// credential at every start, and nothing in this emulator tells two executions apart by
/// the session NAME.
///
/// Engine-scoped rather than a field of this service's own store, so minting one takes
/// the token service's lock and this small one and never this service's state guard.
/// LOCK ORDER: the IAM record of the role is read and released FIRST, then this store is
/// taken, and the mint then takes the token service's registry, the credential store and
/// the IAM store it records the session in — the order [`role_session`] takes them in,
/// and the only order anything may.
#[derive(Debug, Default)]
pub struct ExecutionRoleSessions {
    /// One held session per role ARN, with the instant it stops being valid.
    held: BTreeMap<String, Held>,
}

/// One held session: what a call is signed with, and when it stops being valid.
#[derive(Debug, Clone)]
struct Held {
    session: RoleSession,
    /// The instant the token service minted it to last until.
    expires_at: SystemTime,
}

/// The sessions store in the SAME shared engine, or `None` where the engine carries
/// none.
fn sessions_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ExecutionRoleSessions>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ExecutionRoleSessions>)
}

/// The credential the integration calls of an execution running as `role_arn` are
/// signed with, minting one if this emulator holds none that is still good for a whole
/// run.
///
/// `store` is the credential store a minted session is recorded in; a caller that has
/// none — a continuation the clock runs rather than a request — gets a session only
/// where one is already held. `None` wherever nothing can be shown about the role (see
/// the module docs), which leaves the calls signed with this emulator's own credential.
#[must_use]
pub fn role_session<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: Option<&CredentialStore>,
    role_arn: &str,
    now: SystemTime,
) -> Option<RoleSession> {
    if role_arn.is_empty() {
        return None;
    }
    let role = held_role(engine, role_arn)?;
    let sessions = sessions_state(engine)?;
    let mut sessions = sessions.lock();
    // A session the clock has already expired is held by nobody: it is given up here,
    // so a credential no execution presents stays in neither store for the life of the
    // process.
    let expired: Vec<String> = sessions
        .held
        .iter()
        .filter(|(_, held)| held.expires_at <= now)
        .map(|(key, _)| key.clone())
        .collect();
    for stale in expired {
        if let Some(held) = sessions.held.remove(&stale) {
            forget(engine, store, &held.session.access_key_id);
        }
    }
    // A held session is used again only while it has a whole run's worth of life left.
    let still_good = sessions
        .held
        .get(&role.arn)
        .filter(|held| held.expires_at > now + REFRESH_MARGIN);
    if let Some(held) = still_good {
        return Some(held.session.clone());
    }
    let minted = mint(engine, store?, &role, now)?;
    let session = minted.session.clone();
    // The session this one replaces is forgotten where it was held, so a credential no
    // execution presents any more stops being valid.
    if let Some(replaced) = sessions.held.insert(role.arn, minted) {
        forget(engine, store, &replaced.session.access_key_id);
    }
    Some(session)
}

/// What the IAM store holds about one execution role, as a session of it is minted from.
struct ResolvedRole {
    /// The role's ARN, exactly as IAM holds it — the spelling a policy names it by.
    arn: String,
    /// Its name, which the session's own ARN carries.
    name: String,
    /// Its unique id, which the session's `AssumedRoleId` is built from.
    id: String,
    /// The account whose IAM holds it, read off its ARN.
    account: String,
    /// The partition that ARN names, so a session of a role of another partition is
    /// named in that partition.
    partition: String,
    /// "The maximum session duration (in seconds) that you want to set for the specified
    /// role" (`Role$MaxSessionDuration`), which caps the session minted here.
    max_session_duration: i32,
}

/// The role `role_arn` names, when the IAM store of this engine holds one under it.
///
/// `None` when nothing is known about the role: no IAM store on this engine, a value
/// that names no IAM role, or a name this account's IAM holds no role under.
fn held_role<Eng: Send + Sync + 'static>(engine: &Eng, role_arn: &str) -> Option<ResolvedRole> {
    let parsed = ::service_common::arn::parse(role_arn)?;
    let iam =
        ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state::service_state(engine)?;
    let held = iam.lock();
    let role = held.roles.values().find(|role| role.arn == role_arn)?;
    Some(ResolvedRole {
        arn: role.arn.clone(),
        name: role.role_name.clone(),
        id: role.role_id.clone(),
        account: parsed.account.to_string(),
        partition: parsed.partition.to_string(),
        max_session_duration: role
            .max_session_duration
            .filter(|duration| MAX_SESSION_DURATION_RANGE.contains(duration))
            .unwrap_or(SESSION_SECONDS),
    })
}

/// Mint one session of `role`, through the token service's own mint.
fn mint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &CredentialStore,
    role: &ResolvedRole,
    now: SystemTime,
) -> Option<Held> {
    let registry = sts_store::service_state(engine)?;
    let session_name = super::task_authorization::new_session_name();
    let identity = sts_store::Identity {
        user_id: format!("{}:{session_name}", role.id),
        account: role.account.clone(),
        arn: format!(
            "arn:{}:sts::{}:assumed-role/{}/{session_name}",
            role.partition, role.account, role.name
        ),
    };
    let duration_seconds = SESSION_SECONDS.min(role.max_session_duration);
    let minted = sts_store::mint_role_session(
        engine,
        store,
        &registry,
        now,
        sts_store::NewSession {
            role_arn: Some(role.arn.clone()),
            ..sts_store::NewSession::new(
                identity,
                sts_store::SessionKind::AssumedRole,
                duration_seconds,
            )
        },
    )
    .ok()?;
    Some(Held {
        session: RoleSession {
            access_key_id: minted.credentials.access_key_id,
            secret_access_key: minted.credentials.secret_access_key,
            session_token: Some(minted.credentials.session_token),
            session_name,
        },
        expires_at: minted.session.expires_at,
    })
}

/// Forget a session this module minted, in both stores that held it: the credential
/// store (so a request presenting it no longer verifies) and the token service's own
/// registry (so it names no identity either).
fn forget<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: Option<&CredentialStore>,
    access_key_id: &str,
) {
    sts_store::forget_audited_session(engine, access_key_id);
    match (sts_store::service_state(engine), store) {
        (Some(registry), Some(store)) => registry.lock().discard(access_key_id, store),
        (_, Some(store)) => {
            let _ = store.delete(access_key_id);
        }
        (_, None) => {}
    }
}
