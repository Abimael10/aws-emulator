// SPDX-License-Identifier: CC0-1.0

//! The CREDENTIALS one instance's guest is served for the IAM role its instance profile
//! carries — a SESSION of that role, named for the instance.
//!
//! # The rule this module implements
//!
//! "Applications on the instance retrieve the security credentials provided by the role from
//! the instance metadata item `iam/security-credentials/<role-name>`" (Amazon EC2 User Guide,
//! "Use an IAM role to grant permissions to applications running on Amazon EC2 instances"):
//! an instance that runs as an instance profile has temporary credentials FOR the role that
//! profile carries published to its guest, and that is how every SDK, agent and script inside
//! the machine signs the calls it makes. So a call made from inside the guest is a call made
//! BY THAT ROLE, and the service serving it judges it against the policies in force on the
//! role exactly as it judges any other signed request — which is the whole point of an
//! instance profile: two instances whose roles carry different grants are refused different
//! things, and changing which role the instance runs as changes who its calls are made by.
//!
//! A metadata service that publishes the role's NAME and no credentials under it implements
//! none of that: the documented read answers nothing, so nothing in the guest can call AWS at
//! all and a permission difference cannot be told from a machine that never had credentials.
//!
//! # Where the credential comes from
//!
//! Nothing here constructs an access key id, a secret or a session token:
//!
//! * which role the instance runs as is read through this crate's ONE resolution of that fact
//!   ([`super::instance_profile_role`] — this store's association, then IAM's own profile
//!   report), so no second copy of either reading is kept;
//! * the SESSION is minted by the TOKEN SERVICE's own role-session mint
//!   ([`sts_store::mint_role_session`]), which records it everywhere a temporary credential's
//!   principal is resolved from: `sts:GetCallerIdentity` under the credential reports the
//!   session, every service that authorizes a request decides it on the policies in force on
//!   the role, and the credential is one the emulator's own request verifier accepts;
//! * its identity is the assumed-role session AWS reports for an instance's calls —
//!   `arn:<partition>:sts::<account>:assumed-role/<role name>/<instance id>`, with the
//!   composite `<role id>:<instance id>` user id — so a refusal says which INSTANCE was
//!   refused.
//!
//! The one mint of a role session for a service that runs other work as a role lives in the
//! service that runs functions, and two other services reach it cross-crate; this crate
//! cannot, because that crate already depends on THIS one (it reads the account's VPC
//! inventory), and a dependency the other way would be a cycle. So the holder below states the
//! same three things that mint states — one session per `(instance, role)`, re-minted once the
//! clock has brought it near its expiry, and the one it replaces given up in both stores — and
//! takes the token service's own mint for the credential itself.
//!
//! # It belongs to the INSTANCE, not to one read
//!
//! "The credentials are rotated automatically" and "new credentials are made available at
//! least five minutes before the expiration of the old credentials" (the same guide), so a
//! guest that reads the path twice reads one credential rather than two: the session is held
//! for the instance it was minted for and is served again until the clock brings it within
//! [`REFRESH_MARGIN`] of its expiry. A guest whose instance now runs as ANOTHER role is served
//! a session of that role instead, because the held one is keyed by the role it speaks for —
//! which is what makes replacing an instance profile association observable from inside the
//! machine.
//!
//! The holder is engine-scoped rather than a field of this service's store, so minting one
//! takes the token service's registry, the credential store and the IAM store and never this
//! service's state guard. LOCK ORDER: this service's store (to read which profile the instance
//! runs as) is released before IAM's is taken, and the mint's own stores last — the order
//! [`instance_role_credentials`] takes them in, and the only order anything may.

use ::std::collections::BTreeMap;
use ::std::time::{Duration, SystemTime};

use ::emulator_server::CredentialStore;
use ::service_com_amazonaws_sts_awssecuritytokenservicev20110615::state as sts_store;

/// How long a session minted for an instance's role lasts.
///
/// `sts:AssumeRole` documents 3600 seconds as the default duration of a role session
/// ("`DurationSeconds` … Default: 3600 seconds"), and no role's own maximum is below it:
/// "the maximum session duration (in seconds) that you want to set for the specified role …
/// Minimum value of 3600. Maximum value of 43200" (`UpdateRole`'s own documentation of
/// `Role$MaxSessionDuration`), so a role's maximum can never shorten this one and nothing
/// here reads it.
const SESSION_SECONDS: i32 = 3600;

/// How much of a held session's life has to be left for it to be served again: "new
/// credentials are made available at least five minutes before the expiration of the old
/// credentials" (Amazon EC2 User Guide, "Use an IAM role to grant permissions to applications
/// running on Amazon EC2 instances"), so a session the clock has brought nearer than that to
/// its expiry is replaced before it is published.
const REFRESH_MARGIN: Duration = Duration::from_secs(5 * 60);

/// How many instances' sessions this holder keeps at once.
///
/// An EMULATOR ceiling, not an AWS quota: one record per instance that has been served
/// credentials, so the holder needs a declared cap. A mint past it replaces the record whose
/// session expires FIRST, giving that credential up in both stores, so the holder cannot grow
/// without end and a live session is never silently left behind.
const MAX_HELD_SESSIONS: usize = 4_096;

/// The `Code` of a credentials document the metadata service resolved: the word AWS's own
/// document carries.
pub const ROLE_CREDENTIALS_SUCCESS: &str = "Success";

/// The `Type` of the credentials the metadata service publishes, as AWS's own document
/// carries it.
pub const ROLE_CREDENTIALS_TYPE: &str = "AWS-HMAC";

/// The credentials one instance's guest is served for the role its instance profile carries,
/// as its metadata service publishes them under that role's name.
#[derive(Clone)]
pub struct GuestRoleCredentials {
    /// The NAME of the role these credentials belong to, which is the path segment they are
    /// published under: a read under any other name is answered by nothing.
    pub role_name: String,
    /// "The access key id that identifies the temporary security credentials"
    /// (`Credentials$AccessKeyId`), which is where the service serving a call reads WHO made
    /// it.
    pub access_key_id: String,
    /// "The secret access key that can be used to sign requests"
    /// (`Credentials$SecretAccessKey`).
    pub secret_access_key: String,
    /// "The token that users must pass to the service API to use the temporary credentials"
    /// (`Credentials$SessionToken`).
    pub session_token: String,
    /// When this credential was minted, which the document's `LastUpdated` carries.
    pub issued_at: SystemTime,
    /// "The date on which the current credentials expire" (`Credentials$Expiration`), which
    /// the document's `Expiration` carries.
    pub expires_at: SystemTime,
}

/// Prints the role the credentials belong to and NO field of the credential itself, so
/// neither a key id nor a secret nor a session token can reach a log through a debug form of
/// this type — or of any type holding one, a [`super::guest_console::GuestInstanceProfile`]
/// and a whole [`super::guest_console::GuestMachine`] among them, each of which derives its
/// debug form from this.
impl ::std::fmt::Debug for GuestRoleCredentials {
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("GuestRoleCredentials")
            .field("role_name", &self.role_name)
            .finish_non_exhaustive()
    }
}

/// The sessions this emulator holds for its instances, keyed by the instance each was minted
/// for (see the module's LOCK ORDER note).
#[derive(Debug, Default)]
pub struct InstanceRoleSessions {
    /// One held session per instance.
    held: BTreeMap<String, Held>,
}

/// One held session: what the guest is served, and the role it speaks for.
#[derive(Debug, Clone)]
struct Held {
    /// The credential itself. Its debug form carries no secret.
    credentials: GuestRoleCredentials,
    /// The ARN of the role it was minted for, so an instance that now runs as another role is
    /// not served a session of the old one.
    role_arn: String,
    /// `Role$RoleId` of that role as the mint read it: a role deleted and created again under
    /// the same name is another principal behind the same ARN, so a session of the first one
    /// is not served for the second.
    role_id: String,
}

/// The holder in the SAME shared engine, or `None` where the engine carries none.
fn sessions_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<InstanceRoleSessions>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<InstanceRoleSessions>)
}

/// What the ROLE of the instance's instance profile is, as a session of it is minted from.
#[derive(Debug, Clone)]
struct ResolvedRole {
    /// The role's ARN, exactly as IAM holds it — the spelling a policy names the role by.
    arn: String,
    /// Its name, which the session's own ARN carries and the credentials are published under.
    name: String,
    /// Its unique id, which the session's user id is built from.
    id: String,
    /// The account whose IAM holds it, read off its ARN.
    account: String,
    /// The partition that ARN names, so a session of a role of another partition is named in
    /// that partition.
    partition: String,
}

/// The credentials the guest of the instance `instance_id` names is served for the role its
/// instance profile carries, minting a session if this emulator holds none that is still good
/// to publish — and `None` for an instance that runs as no instance profile, one whose profile
/// the account's IAM records hold nothing for, one whose profile carries no role, and an
/// engine with no token service to mint a session with.
///
/// Call it with NO guard of this service's store held: it takes this store's guard to read
/// which profile the instance runs as, releases it, and then takes IAM's and the mint's (see
/// the module's LOCK ORDER note). It is what the caller that reaches a guest with the engine
/// in hand hands over to the machine ([`super::guest_console::GuestIdentity`]), beside the
/// name of the role itself.
#[must_use]
pub fn instance_role_credentials<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &CredentialStore,
    instance_id: &str,
    now: SystemTime,
) -> Option<GuestRoleCredentials> {
    let role = instance_role(engine, instance_id)?;
    let sessions = sessions_state(engine)?;
    let mut sessions = sessions.lock();
    // A session the clock has already expired is held by nobody: it is given up here, so a
    // credential no guest is served any more does not stay in either store for the life of
    // the process.
    let expired: Vec<String> = sessions
        .held
        .iter()
        .filter(|(_, held)| held.credentials.expires_at <= now)
        .map(|(instance, _)| instance.clone())
        .collect();
    for stale in expired {
        if let Some(held) = sessions.held.remove(&stale) {
            forget(engine, store, &held.credentials.access_key_id);
        }
    }
    // The held session is served again while it speaks for the role the instance runs as NOW
    // and the clock has not brought it near its expiry.
    let still_good = sessions.held.get(instance_id).filter(|held| {
        held.role_arn == role.arn
            && held.role_id == role.id
            && held.credentials.expires_at > now + REFRESH_MARGIN
    });
    if let Some(held) = still_good {
        return Some(held.credentials.clone());
    }
    // Room for one more record, made before anything is minted: the session that expires
    // first is given up, so a refused mint leaves nothing behind and the holder is bounded.
    if sessions.held.len() >= MAX_HELD_SESSIONS && !sessions.held.contains_key(instance_id) {
        let soonest = sessions
            .held
            .iter()
            .min_by_key(|(_, held)| held.credentials.expires_at)
            .map(|(instance, _)| instance.clone());
        if let Some(instance) = soonest {
            if let Some(held) = sessions.held.remove(&instance) {
                forget(engine, store, &held.credentials.access_key_id);
            }
        }
    }
    let minted = mint(engine, store, &role, instance_id, now)?;
    let credentials = minted.credentials.clone();
    // The session this one replaces is forgotten where it was held, so a credential no guest
    // is served any more stops being valid.
    if let Some(replaced) = sessions.held.insert(instance_id.to_string(), minted) {
        forget(engine, store, &replaced.credentials.access_key_id);
    }
    Some(credentials)
}

/// The role the instance `instance_id` RUNS AS, as a session of it is minted from.
///
/// The role itself is resolved through this crate's ONE resolution of that fact
/// ([`super::instance_profile_role`], which takes this store's guard to read which profile the
/// instance runs as and releases it before IAM's is taken), so the name the guest is published
/// and the role a session is minted for cannot come from two different records.
fn instance_role<Eng: Send + Sync + 'static>(
    engine: &Eng,
    instance_id: &str,
) -> Option<ResolvedRole> {
    let role = super::instance_profile_role(engine, instance_id)?;
    let parsed = ::service_common::arn::parse(&role.arn)?;
    Some(ResolvedRole {
        arn: role.arn.clone(),
        name: role.role_name.clone(),
        id: role.role_id.clone(),
        account: parsed.account.to_string(),
        partition: parsed.partition.to_string(),
    })
}

/// Mint one session of `role` for the instance `instance_id`, through the token service's own
/// role-session mint.
fn mint<Eng: Send + Sync + 'static>(
    engine: &Eng,
    store: &CredentialStore,
    role: &ResolvedRole,
    instance_id: &str,
    now: SystemTime,
) -> Option<Held> {
    let registry = sts_store::service_state(engine)?;
    // The identity AWS reports for the calls an instance makes with its profile: the session
    // is named for the INSTANCE, and its user id is the composite of the role's own id and
    // that name.
    let identity = sts_store::Identity {
        user_id: format!("{}:{instance_id}", role.id),
        account: role.account.clone(),
        arn: format!(
            "arn:{}:sts::{}:assumed-role/{}/{instance_id}",
            role.partition, role.account, role.name
        ),
    };
    let duration_seconds = SESSION_SECONDS;
    let minted = sts_store::mint_role_session(
        engine,
        store,
        &registry,
        now,
        sts_store::NewSession {
            role_arn: Some(role.arn.clone()),
            role_id: Some(role.id.clone()),
            ..sts_store::NewSession::new(
                identity,
                sts_store::SessionKind::AssumedRole,
                duration_seconds,
            )
        },
    )
    .ok()?;
    Some(Held {
        credentials: GuestRoleCredentials {
            role_name: role.name.clone(),
            access_key_id: minted.credentials.access_key_id,
            secret_access_key: minted.credentials.secret_access_key,
            session_token: minted.credentials.session_token,
            issued_at: now,
            expires_at: minted.session.expires_at,
        },
        role_arn: role.arn.clone(),
        role_id: role.id.clone(),
    })
}

/// Forget a session this module minted, in both stores that held it: the credential store (so
/// a request presenting it no longer verifies) and the token service's own registry (so it
/// names no identity either).
fn forget<Eng: Send + Sync + 'static>(engine: &Eng, store: &CredentialStore, access_key_id: &str) {
    sts_store::forget_audited_session(engine, access_key_id);
    match sts_store::service_state(engine) {
        Some(registry) => registry.lock().discard(access_key_id, store),
        None => {
            let _ = store.delete(access_key_id);
        }
    }
}
