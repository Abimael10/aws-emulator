// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-appconfigdata-appconfigdata service.
//!
//! AppConfigData is a two-operation polling data plane, so what it owns is two maps:
//! [`ComAmazonawsAppconfigdataAppconfigdataState::sessions`], every live
//! configuration session keyed by the opaque token the client currently holds, and
//! the name bindings of [`NameBindings`].
//! `StartConfigurationSession` inserts one session; each `GetLatestConfiguration`
//! inserts the rotated successor of the token it was called with, so the token a client
//! holds is always resolvable until that token's lifetime runs out.
//!
//! Everything else a poll answers with — which applications, environments and
//! configuration profiles exist, which configuration version is deployed into an
//! environment, and the bytes of that version — belongs to the AppConfig CONTROL
//! PLANE, and is read out of that service's own store through the one shared engine
//! rather than copied into a second map here ([`resolve_deployed_triple`],
//! [`served_configuration`]). A configuration deployed through `StartDeployment` is
//! therefore the one a poll serves.
//!
//! What a session keeps of its own about that hierarchy is its own resolution of it: the
//! configuration it was last served ([`Session::bound_configuration`]) and, once a poll has
//! found the triple withdrawn, when the refusal that withdrawal owes falls due
//! ([`Session::withdrawal_due_at`]). Every token of one poll chain shares that copy while
//! the version in force stays the same, and a poll that can no longer re-resolve the triple
//! is answered FROM it — the first such poll is what makes the refusal due, one
//! [`WITHDRAWAL_SPAN`] later. Past that instant the poll is refused with the modelled
//! `ResourceNotFoundException` naming the resource that is gone, as is a session that was
//! bound to no configuration at all ([`Session::binding_in_force`]).
//!
//! What is withdrawn is the TRIPLE, so the refusal is owed by the whole poll chain and not
//! by the one token that happened to find the triple gone: the deadline is written onto
//! every token entry of the chain
//! ([`ComAmazonawsAppconfigdataAppconfigdataState::withdraw_chain`]) and cleared from every
//! one of them by a poll whose own resolution held
//! ([`ComAmazonawsAppconfigdataAppconfigdataState::restore_chain`]). A client re-presenting
//! a token it still holds — a token is bounded by TIME and by nothing else — therefore
//! converges on the refusal just as one following the rotation does.
//!
//! The other thing the data plane keeps of its own about that hierarchy is how a NAME
//! resolves. Every session identifier is "the ID or the name" of its level, and an id
//! is immutable while a name is not: the control plane may rename a record, which frees
//! the old name for another record to take. So the data plane binds a name the first
//! time it resolves one — to the id it resolved to — and answers that name from the
//! binding afterwards ([`NameBindings`]), instead of asking the control plane again
//! what currently carries it. A rename therefore neither breaks the consumers
//! configured with the old name nor re-points that name at whatever record claims it
//! next.

/// Whether the caller's own identity-based IAM policies allow the action a request asks
/// for, which both operations decide before they read anything.
pub mod identity_authorization;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sha2::{Digest, Sha256};

use ::service_com_amazonaws_appconfig_amazonappconfig::state::{
    configuration_sources, configuration_version_not_hosted, no_deployed_configuration,
    ComAmazonawsAppconfigAmazonappconfigState as AppConfigState, RolloutBucket,
};

/// How long a configuration token stays usable after it is minted.
///
/// The model documents the lifetime on both `ConfigurationToken` and
/// `NextPollConfigurationToken`: "To support long poll use cases, the token is valid
/// for up to 24 hours. If a `GetLatestConfiguration` call uses an expired token, the
/// system returns `BadRequestException`." A documented duration keeps its documented
/// value.
pub const TOKEN_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

/// The poll interval reported to a session that asked for no minimum of its own.
///
/// `RequiredMinimumPollIntervalInSeconds` is optional, and
/// `NextPollIntervalInSeconds` is required, so a session that set no minimum still
/// has to be told how long to wait. AppConfig's polling guidance is one poll a
/// minute (the model's own example for the minimum is 60 seconds), so that is the
/// interval this emulator reports when the session named none. A session that DID
/// name a minimum is told exactly its own minimum, because it may not poll faster
/// than that.
pub const DEFAULT_POLL_INTERVAL_IN_SECONDS: i32 = 60;

/// Upper bound on the number of entries [`ComAmazonawsAppconfigdataAppconfigdataState::sessions`]
/// holds.
///
/// Every poll stores the successor token's session, so a client polling in a loop
/// would otherwise grow the map for as long as the process lives. Expiry alone does
/// not bound it — a token lives up to [`TOKEN_LIFETIME`] — so the store also keeps a
/// hard cap, past which the entries least likely to serve another poll are dropped
/// first (the ones that have already served one, then the ones nearest their expiry). A
/// real client holds one token at a time, so the cap is reached only by a request loop,
/// and the token it is holding is the last thing evicted.
pub const MAX_SESSIONS: usize = 4_096;

/// How long after a poll FIRST finds the session's triple withdrawn the refusal falls due.
///
/// A session is resolved when it is STARTED, and a poll re-resolves the triple to pick up a
/// newer version in force — not to re-authorise the session — so an edit to the control
/// plane made while a session is polling never refuses the poll that follows it, however
/// long the client waited between polls: that poll is served from what the session is bound
/// to and is what makes the withdrawal due, this span later
/// ([`Session::withdrawal_due_at`]). The withdrawal is then a transition the CLOCK makes
/// due, so the poll after it is refused whether or not anybody asked in between. The span
/// is never zero — a client polling straight after the edit is still served — and it is a
/// span chosen for this emulator, nothing AWS documents, so it is short enough for a client
/// retrying its poll to converge on the refusal.
pub const WITHDRAWAL_SPAN: Duration = Duration::from_secs(3);

/// When the refusal falls due for a poll that found the triple withdrawn at `now`.
///
/// `SystemTime + Duration` panics on an unrepresentable result, so a grossly misconfigured
/// host clock must not unwind through a handler: it falls back to `now`, which makes the
/// withdrawal due immediately after the poll it was recorded by rather than aborting the
/// request.
pub fn withdrawal_deadline(now: SystemTime) -> SystemTime {
    now.checked_add(WITHDRAWAL_SPAN).unwrap_or(now)
}

/// When a token minted at `now` stops being usable.
///
/// `SystemTime + Duration` panics on an unrepresentable result, so a grossly
/// misconfigured host clock must not unwind through a handler: it falls back to
/// `now`, which makes the token expire immediately (a modelled `BadRequestException`)
/// rather than aborting the request.
fn expiry(now: SystemTime) -> SystemTime {
    now.checked_add(TOKEN_LIFETIME).unwrap_or(now)
}

/// One configuration session: the triple it polls, the poll floor it asked for, and
/// how far through the configuration's version history the client has been served.
///
/// A session is stored once per token: rotating a token stores the (updated) session
/// under a new key and records the poll on the old entry, which stays usable for the
/// rest of its own lifetime.
#[derive(Debug, Clone)]
pub struct Session {
    /// `ApplicationIdentifier` as the session was started with.
    pub application_identifier: String,
    /// `EnvironmentIdentifier` as the session was started with.
    pub environment_identifier: String,
    /// `ConfigurationProfileIdentifier` as the session was started with.
    pub configuration_profile_identifier: String,
    /// `RequiredMinimumPollIntervalInSeconds`, when the session set one.
    pub required_minimum_poll_interval_in_seconds: Option<i32>,
    /// The configuration version this client has already received, as
    /// [`ServedConfiguration::version`] names it; `None` before its first poll, so the
    /// first poll always returns content.
    pub last_served_version: Option<String>,
    /// The configuration this session is BOUND to: the one the last resolution of its
    /// triple against the AppConfig control plane produced — recorded when the session
    /// was started and carried forward by every poll that resolves. `None` for a session
    /// whose triple had nothing in force when it started.
    ///
    /// What a poll serves is what the control plane has in force at the instant of the
    /// poll; this is what it is served from once the triple can no longer be re-resolved,
    /// until the withdrawal that poll made due has fallen due
    /// ([`Session::binding_in_force`]).
    /// Shared behind an `Arc` because rotating a
    /// token clones the session: a configuration VERSION is immutable once written, so
    /// every token of one poll chain shares this one copy of it instead of holding a new
    /// one per poll, and polling in a loop holds no more of them than the chain's current
    /// version. What the store holds at once is bounded by the same two limits as the
    /// sessions themselves — each entry's expiry and [`MAX_SESSIONS`].
    pub bound_configuration: Option<Arc<ServedConfiguration>>,
    /// When the refusal the control plane's edit owes this session falls due: the instant
    /// recorded by the FIRST poll that could not re-resolve the session's triple, one
    /// [`WITHDRAWAL_SPAN`] past that poll. `None` while the triple still resolves — the
    /// state every session starts in, and the one a poll whose own resolution held puts it
    /// back in, so a record the control plane holds again withdraws nothing.
    ///
    /// A poll served from the binding carries the deadline it found on, never a fresh one,
    /// so the withdrawal falls due however often the chain is polled
    /// ([`Session::binding_in_force`]). The instant belongs to the CHAIN rather than to one
    /// of its tokens: every entry of this session's [`Session::chain`] carries the same
    /// value, written and cleared together
    /// ([`ComAmazonawsAppconfigdataAppconfigdataState::withdraw_chain`]), so whichever
    /// still-live token of the chain the client presents next is answered from the same
    /// deadline.
    pub withdrawal_due_at: Option<SystemTime>,
    /// The poll CHAIN this entry belongs to: minted with the session and carried by every
    /// token its polls rotate, so the entries the tokens of one session resolve to are
    /// recognisable as answering for the same triple.
    ///
    /// What the chain is for is the withdrawal above, which the triple owes and therefore
    /// owes to all of the chain at once. It is internal to this store — no response carries
    /// it, and a token is resolved by the map key, never by this.
    pub chain: String,
    /// Where this session stands in a gradual rollout: the position the control plane
    /// compares against a deployment's `PercentageComplete` when it decides which
    /// deployment is in force for this session
    /// (`service_com_amazonaws_appconfig_amazonappconfig::state::RolloutBucket`).
    ///
    /// A configuration session is one TARGET of a deployment, and a rollout reaches its
    /// targets gradually, so a session opened while a rollout is part-way through is
    /// served either the new version or the previous one depending on where it stands —
    /// never a blend of the two. The position is minted with the session
    /// ([`ComAmazonawsAppconfigdataAppconfigdataState::mint_rollout_bucket`]) and carried
    /// by every token of its poll chain, so a session the rollout has reached stays
    /// reached and one it has not is reached only when the rollout grows past it.
    pub rollout_bucket: RolloutBucket,
    /// When the session itself was started.
    pub created_at: SystemTime,
    /// When THIS token stops being usable ([`TOKEN_LIFETIME`] after it was minted).
    pub expires_at: SystemTime,
    /// When this session was last polled, which is what the session's own minimum
    /// poll interval is measured from. `None` until the first poll.
    pub last_polled_at: Option<SystemTime>,
    /// When the token this entry is keyed by was last used for a poll, if it has been.
    /// A token is bounded by its lifetime and by nothing else, so this refuses no poll:
    /// it records what the entry has already done, which is what orders eviction when
    /// the store is at its cap ([`MAX_SESSIONS`]).
    pub last_used_at: Option<SystemTime>,
}

impl Session {
    /// The configuration this session is bound to, while the withdrawal it owes has not
    /// fallen due at `now` — what a poll is served when the control plane can no longer
    /// re-resolve the session's triple.
    ///
    /// `None` from [`Session::withdrawal_due_at`] onwards, and `None` for a session bound
    /// to no configuration at all: in both cases a poll whose triple no longer resolves has
    /// nothing to serve. A session whose chain no poll has yet found withdrawn carries no
    /// deadline, so what it is bound to is served whenever it next polls, however long it
    /// waited.
    pub fn binding_in_force(&self, now: SystemTime) -> Option<&Arc<ServedConfiguration>> {
        self.bound_configuration
            .as_ref()
            .filter(|_| self.withdrawal_due_at.is_none_or(|due| now < due))
    }

    /// Whether the token this entry is keyed by has already served a poll — so the
    /// client was handed a successor to present instead, and this entry is the one the
    /// store gives up first when it has to give one up.
    pub fn has_served_a_poll(&self) -> bool {
        self.last_used_at.is_some()
    }

    /// When this entry was last touched: the poll that minted its token, or — for a
    /// session's first token, which no poll has used yet — when the session started.
    /// The order the store reclaims by when it is at its cap.
    pub fn last_touched_at(&self) -> SystemTime {
        self.last_polled_at.unwrap_or(self.created_at)
    }
}

/// Upper bound on the number of name bindings [`NameBindings`] holds.
///
/// A binding is recorded only for a name that actually resolved, so the map grows with
/// the names the control plane's records have CARRIED rather than with the number of
/// requests; renaming a record in a loop would still grow it without limit, so the store
/// also keeps a hard cap, past which the bindings recorded longest ago are dropped first.
pub const MAX_NAME_BINDINGS: usize = 4_096;

/// One name the data plane has bound, as the map of [`NameBindings`] is keyed.
///
/// A name addresses a record only within its own parent — an environment and a
/// configuration profile are children of one application — so the key carries the scope
/// the name was resolved in alongside the level it names. Two applications may each hold
/// an environment called the same thing, and the two bindings are different keys.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct BoundName {
    /// The level of the hierarchy the name names, as the modelled `ResourceType` value
    /// for it ([`RESOURCE_TYPE_APPLICATION`] and the two below it).
    level: &'static str,
    /// The id of the record the name is scoped to: the application for an environment or
    /// a configuration profile, and [`ACCOUNT_SCOPE`] for an application, whose names
    /// are scoped to the account.
    scope: String,
    /// The name as the request spelled it.
    name: String,
}

/// The scope an application name is bound in: an application has no parent record, so
/// its name is unique within the account rather than within another resource.
const ACCOUNT_SCOPE: &str = "";

/// A name this service resolved against the control plane for the first time, and the id
/// it resolved to — handed back by [`resolve_deployed_triple`] for the caller to record
/// once it has decided the call succeeds.
#[derive(Debug, Clone)]
pub struct LearnedName {
    key: BoundName,
    id: String,
}

/// Which id each NAME this data plane has seen resolves to.
///
/// A session identifier is "the ID or the name" of its level. An id is immutable, so it
/// needs no binding; a name is not, so the first resolution of one is recorded here and
/// every later resolution of that name answers from the record it was bound to — even
/// after the control plane renames that record, and even after another record takes the
/// freed name, which therefore keeps answering for the record that held it rather than
/// for the one that holds it now.
#[derive(Debug, Default, Clone)]
pub struct NameBindings {
    /// Each bound name, with the id it resolves to and when it was bound (which is the
    /// order the cap reclaims by).
    bound: HashMap<BoundName, (String, SystemTime)>,
}

impl NameBindings {
    /// The id `name` is bound to at `level` within `scope`, if it is bound at all.
    fn bound_id(&self, level: &'static str, scope: &str, name: &str) -> Option<&str> {
        self.bound
            .get(&BoundName {
                level,
                scope: scope.to_string(),
                name: name.to_string(),
            })
            .map(|(id, _)| id.as_str())
    }

    /// Record the names a resolution learned, and hold the map to
    /// [`MAX_NAME_BINDINGS`].
    ///
    /// A name resolved live is bound to the id it resolved to, replacing a binding whose
    /// record the control plane no longer holds (that is the only way a name is resolved
    /// live again once it has been bound).
    fn bind(&mut self, learned: Vec<LearnedName>, now: SystemTime) {
        for LearnedName { key, id } in learned {
            self.bound.insert(key, (id, now));
        }
        if self.bound.len() <= MAX_NAME_BINDINGS {
            return;
        }
        let excess = self.bound.len() - MAX_NAME_BINDINGS;
        let mut candidates: Vec<(SystemTime, BoundName)> = self
            .bound
            .iter()
            .map(|(key, (_, bound_at))| (*bound_at, key.clone()))
            .collect();
        // Oldest binding first; the key breaks a tie so reclaiming does not depend on
        // hash order.
        candidates.sort();
        for (_, key) in candidates.into_iter().take(excess) {
            self.bound.remove(&key);
        }
    }

    /// How many names are bound — the cap of [`MAX_NAME_BINDINGS`] applies to this.
    pub fn len(&self) -> usize {
        self.bound.len()
    }

    /// Whether no name is bound yet.
    pub fn is_empty(&self) -> bool {
        self.bound.is_empty()
    }
}

/// Per-service state for com-amazonaws-appconfigdata-appconfigdata. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsAppconfigdataAppconfigdataState {
    /// Live sessions, keyed by the configuration token that resolves to them.
    pub sessions: HashMap<String, Session>,
    /// Which id each name this data plane has resolved is bound to; see [`NameBindings`].
    name_bindings: NameBindings,
    /// How many tokens this store has minted; part of the token material, so no two
    /// tokens are ever derived from the same input.
    tokens_minted: u64,
    /// How many poll chains this store has minted; part of a chain identifier's material,
    /// so no two chains are ever derived from the same input ([`Session::chain`]).
    chains_minted: u64,
}

impl ComAmazonawsAppconfigdataAppconfigdataState {
    /// The name bindings as they stand, to resolve a request's identifiers with.
    ///
    /// Handed out as an owned COPY so the resolution — which takes the AppConfig control
    /// plane's own lock — runs with no guard of this store held: the two stores are
    /// taken one at a time, never nested.
    pub fn name_bindings(&self) -> NameBindings {
        self.name_bindings.clone()
    }

    /// Record the names a resolution learned ([`NameBindings::bind`]).
    ///
    /// Called only by the operation that MINTS a session: a call this service refuses
    /// persists nothing, and a poll resolves the identifiers its session was started
    /// with, which were bound when that session was minted.
    pub fn bind_names(&mut self, learned: Vec<LearnedName>, now: SystemTime) {
        self.name_bindings.bind(learned, now);
    }

    /// Start a session over `(application, environment, profile)` and return the
    /// token that resolves to it.
    ///
    /// `bound_configuration` is what the triple resolved to as the session was started
    /// ([`Session::bound_configuration`]), and `rollout_bucket` where the session stands
    /// in a rollout ([`Session::rollout_bucket`]) — the one this resolution was made
    /// with, so what the session polls and what it is bound to are decided for the same
    /// target.
    #[allow(clippy::too_many_arguments)]
    pub fn start_session(
        &mut self,
        application_identifier: &str,
        environment_identifier: &str,
        configuration_profile_identifier: &str,
        required_minimum_poll_interval_in_seconds: Option<i32>,
        bound_configuration: Option<Arc<ServedConfiguration>>,
        rollout_bucket: RolloutBucket,
        now: SystemTime,
    ) -> String {
        let chain = self.mint_chain(now);
        let session = Session {
            application_identifier: application_identifier.to_string(),
            environment_identifier: environment_identifier.to_string(),
            configuration_profile_identifier: configuration_profile_identifier.to_string(),
            required_minimum_poll_interval_in_seconds,
            last_served_version: None,
            bound_configuration,
            withdrawal_due_at: None,
            chain,
            rollout_bucket,
            created_at: now,
            expires_at: expiry(now),
            last_polled_at: None,
            last_used_at: None,
        };
        self.store_session(session, now)
    }

    /// Record, on every token entry of `chain`, that a poll of it found the session's
    /// triple withdrawn, and answer the instant the refusal falls due.
    ///
    /// What the control plane withdrew is the TRIPLE, which every token of one chain
    /// resolves to, so the refusal is owed by the chain and not by the token that happened
    /// to find the triple gone. A token is bounded by TIME and by nothing else, so a client
    /// may still present any token the chain has minted; recording the deadline on all of
    /// them is what makes the chain converge on the refusal whichever one it presents next.
    ///
    /// The instant recorded is `carried` — the deadline the poll found on the session it
    /// answered — and only a poll that finds none, the first of its chain to find the
    /// triple withdrawn, makes a fresh one one [`WITHDRAWAL_SPAN`] out. So polling faster
    /// cannot postpone the refusal.
    pub fn withdraw_chain(
        &mut self,
        chain: &str,
        carried: Option<SystemTime>,
        now: SystemTime,
    ) -> SystemTime {
        let due = carried.unwrap_or_else(|| withdrawal_deadline(now));
        self.write_chain_withdrawal(chain, Some(due));
        due
    }

    /// Clear the refusal every token entry of `chain` owes: the poll's own resolution of
    /// the triple HELD, so the control plane answers for it again and nothing is withdrawn
    /// from the chain — including from the entries an earlier withdrawn poll put a deadline
    /// on.
    pub fn restore_chain(&mut self, chain: &str) {
        self.write_chain_withdrawal(chain, None);
    }

    /// Write `due` as the withdrawal deadline of every entry belonging to `chain`.
    fn write_chain_withdrawal(&mut self, chain: &str, due: Option<SystemTime>) {
        for session in self.sessions.values_mut() {
            if session.chain == chain {
                session.withdrawal_due_at = due;
            }
        }
    }

    /// Mint the identifier of a fresh poll chain, which the session being started and every
    /// successor its polls rotate carry ([`Session::chain`]).
    ///
    /// Derived the way a token is — from a mint counter and the current instant, hashed
    /// under its own domain tag, so a chain identifier is never equal to a token this store
    /// also minted and no two chains collide.
    fn mint_chain(&mut self, now: SystemTime) -> String {
        self.chains_minted += 1;
        let nanos = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let mut hasher = Sha256::new();
        hasher.update(b"configuration-session-poll-chain");
        hasher.update(self.chains_minted.to_be_bytes());
        hasher.update(nanos.to_be_bytes());
        hex::encode(hasher.finalize())
    }

    /// Mint the position a session being started stands at in a gradual rollout
    /// ([`Session::rollout_bucket`]).
    ///
    /// Derived the way a token is — from this store's mint counter and the current
    /// instant, under its own domain tag so it is not the token's own hash — so the
    /// positions sessions are minted at are spread over the whole range and independent
    /// of each other, which is what makes a partial rollout split the sessions polling
    /// it. Reads the counter without advancing it: a call that mints no session writes
    /// nothing, and the counter has moved on by the time the next session is minted,
    /// because minting one mints a token.
    pub fn mint_rollout_bucket(&self, now: SystemTime) -> RolloutBucket {
        let nanos = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        RolloutBucket::of_key(&format!(
            "configuration-session-rollout-bucket:{}:{nanos}",
            self.tokens_minted
        ))
    }

    /// Store `session` under a freshly minted token and return that token.
    ///
    /// Used both to start a session and to rotate one: the caller updates the
    /// session it read (its served version, its poll instant) and stores it back,
    /// which is what gives the client a fresh token pointing at the progressed
    /// session. Storing also reclaims the entries whose lifetime has run out, so
    /// repeated polling cannot grow the store without limit.
    pub fn store_session(&mut self, mut session: Session, now: SystemTime) -> String {
        self.reclaim_sessions(now);
        let token = self.mint_token(now);
        session.expires_at = expiry(now);
        session.last_used_at = None;
        self.sessions.insert(token.clone(), session);
        token
    }

    /// The session `token` resolves to, if any.
    pub fn session(&self, token: &str) -> Option<&Session> {
        self.sessions.get(token)
    }

    /// Record that `token` served a poll at `now`.
    ///
    /// It stays usable until its own expiry — the model bounds a token by time, not by
    /// use — so this changes no verdict; it is what tells the store which entries to
    /// give up first at its cap ([`Session::has_served_a_poll`]).
    pub fn record_token_use(&mut self, token: &str, now: SystemTime) {
        if let Some(session) = self.sessions.get_mut(token) {
            session.last_used_at = Some(now);
        }
    }

    /// Drop the session entries that can no longer serve a poll, and hold the store
    /// to [`MAX_SESSIONS`].
    ///
    /// Two classes go, in order: an entry past its own expiry (its token is refused
    /// either way), and — only if the store is still at the cap — the entries least
    /// likely to serve another poll: the ones that have already served one first, then
    /// the ones whose token was minted longest ago. So under a mint flood an idle
    /// session is evicted before an active one, and the entry the current call is about
    /// to store always survives it; a client that holds one token at a time never
    /// reaches the cap at all.
    fn reclaim_sessions(&mut self, now: SystemTime) {
        self.sessions.retain(|_, session| now < session.expires_at);
        // One slot for the entry about to be stored.
        if self.sessions.len() < MAX_SESSIONS {
            return;
        }
        let excess = self.sessions.len() + 1 - MAX_SESSIONS;
        let mut candidates: Vec<(bool, SystemTime, String)> = self
            .sessions
            .iter()
            .map(|(token, session)| {
                (
                    !session.has_served_a_poll(),
                    session.last_touched_at(),
                    token.clone(),
                )
            })
            .collect();
        // `!has_served_a_poll` sorts the entries that already served one first, then the
        // oldest activity; the token breaks a tie so eviction does not depend on hash
        // order.
        candidates.sort();
        for (_, _, token) in candidates.into_iter().take(excess) {
            self.sessions.remove(&token);
        }
    }

    /// Mint an opaque token: the SHA-256 of this store's mint counter and the
    /// current instant, hex-encoded.
    ///
    /// It carries none of the request's values — the session's identifiers are
    /// looked up THROUGH the token, never recovered from it — and the counter makes
    /// two tokens minted for identical input differ. 64 hex characters satisfy the
    /// model's `^\S{1,8192}$` token pattern.
    fn mint_token(&mut self, now: SystemTime) -> String {
        let nanos = now
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        loop {
            self.tokens_minted += 1;
            let mut hasher = Sha256::new();
            hasher.update(self.tokens_minted.to_be_bytes());
            hasher.update(nanos.to_be_bytes());
            let token = hex::encode(hasher.finalize());
            // A fresh token must not collide with a live one: resolving a token has
            // to name exactly one session.
            if !self.sessions.contains_key(&token) {
                return token;
            }
        }
    }
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsAppconfigdataAppconfigdataState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsAppconfigdataAppconfigdataState>)
}

// =============================================================================
// The AppConfig control plane, read through the one shared engine
// =============================================================================

/// The modelled `ResourceType` values this service reports a missing resource as, from
/// `RESOURCE_TYPE_VALUES` in `crate::ops::types`.
pub const RESOURCE_TYPE_APPLICATION: &str = "Application";
/// See [`RESOURCE_TYPE_APPLICATION`].
pub const RESOURCE_TYPE_ENVIRONMENT: &str = "Environment";
/// See [`RESOURCE_TYPE_APPLICATION`].
pub const RESOURCE_TYPE_CONFIGURATION_PROFILE: &str = "ConfigurationProfile";
/// See [`RESOURCE_TYPE_APPLICATION`].
pub const RESOURCE_TYPE_CONFIGURATION: &str = "Configuration";
/// See [`RESOURCE_TYPE_APPLICATION`].
pub const RESOURCE_TYPE_DEPLOYMENT: &str = "Deployment";

/// The `StartConfigurationSession` input members that name each level of the
/// hierarchy, which the modelled `ReferencedBy` map is keyed by.
pub const APPLICATION_IDENTIFIER_MEMBER: &str = "ApplicationIdentifier";
/// See [`APPLICATION_IDENTIFIER_MEMBER`].
pub const ENVIRONMENT_IDENTIFIER_MEMBER: &str = "EnvironmentIdentifier";
/// See [`APPLICATION_IDENTIFIER_MEMBER`].
pub const CONFIGURATION_PROFILE_IDENTIFIER_MEMBER: &str = "ConfigurationProfileIdentifier";

/// A resource a request referred to that the AppConfig control plane does not hold — a
/// level of its hierarchy, the deployment that would have put a configuration into an
/// environment, or the configuration itself — carrying everything the modelled
/// `ResourceNotFoundException` reports.
#[derive(Debug, Clone)]
pub struct UnresolvedResource {
    /// The modelled `ResourceType` value for the level that is missing.
    pub resource_type: &'static str,
    /// The input members that named it, and the identifiers they carried — the modelled
    /// `ReferencedBy` is "a map indicating which parameters in the REQUEST reference
    /// the resource that was not found", so it is filled only where the request
    /// itself named the resource, with EVERY member that referenced it: a missing level
    /// of the hierarchy is named by its own identifier, and a deployment is named by the
    /// whole triple that identifies it. A poll references the resource through the
    /// session its token resolves to, so it reports the same members, carrying the
    /// identifiers that session was started with.
    referenced_by: Vec<(&'static str, String)>,
    /// What the refusal says, built from the request rather than stored as prose.
    pub message: String,
}

impl UnresolvedResource {
    /// A level of the hierarchy the request named by `member` and the control plane
    /// does not hold.
    ///
    /// The message is the service's OWN fixed text for that level — the modelled
    /// `ResourceType` value followed by "not found" — and carries nothing from the
    /// request: WHICH identifier failed to resolve travels in `ReferencedBy`, which is
    /// the member a client reads it from.
    fn named(resource_type: &'static str, member: &'static str, identifier: &str) -> Self {
        Self {
            resource_type,
            referenced_by: vec![(member, identifier.to_string())],
            message: format!("{resource_type} not found"),
        }
    }

    /// No configuration has ever been deployed into the environment for a configuration
    /// profile that does resolve.
    ///
    /// A deployment has no identifier of its own in a session's request: it is
    /// identified by the triple, so all three members referenced the resource that was
    /// not found and `ReferencedBy` carries all three. The message is the same fixed
    /// text as for every other level and interpolates nothing.
    fn deployment(
        application_identifier: &str,
        environment_identifier: &str,
        configuration_profile_identifier: &str,
    ) -> Self {
        Self {
            resource_type: RESOURCE_TYPE_DEPLOYMENT,
            referenced_by: vec![
                (APPLICATION_IDENTIFIER_MEMBER, application_identifier.to_string()),
                (ENVIRONMENT_IDENTIFIER_MEMBER, environment_identifier.to_string()),
                (
                    CONFIGURATION_PROFILE_IDENTIFIER_MEMBER,
                    configuration_profile_identifier.to_string(),
                ),
            ],
            message: format!("{RESOURCE_TYPE_DEPLOYMENT} not found"),
        }
    }

    /// The modelled `ReferencedBy` map, and `None` where the call's own input named
    /// nothing the refusal is about — the member is absent rather than empty, because an
    /// empty map would claim the request referenced no parameter at all.
    pub fn referenced_by(&self) -> Option<HashMap<String, String>> {
        if self.referenced_by.is_empty() {
            return None;
        }
        Some(
            self.referenced_by
                .iter()
                .map(|(member, identifier)| ((*member).to_string(), identifier.clone()))
                .collect(),
        )
    }

    /// No configuration is deployed for a triple that does resolve, or the version the
    /// deployment names is not in AppConfig's hosted configuration store.
    fn configuration(message: String) -> Self {
        Self {
            resource_type: RESOURCE_TYPE_CONFIGURATION,
            referenced_by: Vec::new(),
            message,
        }
    }
}

/// The configuration AppConfig currently serves for one (application, environment,
/// configuration profile) triple.
#[derive(Debug, Clone)]
pub struct ServedConfiguration {
    /// The configuration content a consumer of this profile receives for the in-force
    /// version, which is what the profile's `Type` makes of the stored bytes
    /// (`ConfigurationProfile::served_content`): a freeform profile's bytes unchanged, and
    /// for a feature-flags profile the resolved flag document rather than the authoring
    /// document the hosted version stores.
    pub content: Vec<u8>,
    /// The MIME type the hosted version declares, reported as `ContentType`.
    pub content_type: String,
    /// The hosted version's label, when it carries one.
    pub version_label: Option<String>,
    /// Which configuration version this is, as [`Session::last_served_version`]
    /// records it: the hosted version number the in-force deployment names. Comparing
    /// it with what the session was last served is what makes "the client already has
    /// the latest version" a comparison of two stored values.
    pub version: String,
}

/// The AppConfig control plane's own store, reached through the one shared engine.
///
/// `None` when that store is unreachable (the engine is not the shared in-memory one),
/// which a handler answers with the same modelled server error it uses for its own
/// store being unreachable.
fn app_config_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<AppConfigState>> {
    ::service_com_amazonaws_appconfig_amazonappconfig::state::service_state(engine)
}

/// What a session is bound to the moment it is started: the names its resolution
/// learned, and the configuration its triple had in force.
#[derive(Debug, Clone)]
pub struct SessionBinding {
    /// The names this resolution resolved against the control plane for the first time,
    /// for the caller to record once it has decided the call succeeds
    /// ([`ComAmazonawsAppconfigdataAppconfigdataState::bind_names`]).
    pub learned_names: Vec<LearnedName>,
    /// The configuration the triple serves at this instant, as
    /// [`Session::bound_configuration`] carries it — `None` when the profile has been
    /// deployed into the environment but has nothing in force there.
    pub configuration: Option<Arc<ServedConfiguration>>,
}

/// Resolve what a session polls against the AppConfig control plane, as
/// `StartConfigurationSession` must before it mints a session: the three identifiers,
/// the deployment that put a configuration into that environment, and the configuration
/// the session is thereby bound to.
///
/// Each identifier is "the ID or the name" of its level, so each resolves either way. An
/// id is resolved against the control plane's records, because an id names one record for
/// as long as it exists; a NAME is resolved through `bindings`, this service's own record
/// of what that name resolved to the first time it did, and the names this resolution
/// learned are handed back for the caller to bind once it has decided the call succeeds
/// ([`ComAmazonawsAppconfigdataAppconfigdataState::bind_names`]).
///
/// The application is resolved first, because the other two levels are its children. Of
/// those two the configuration PROFILE is resolved before the environment — it is what
/// identifies the configuration a session is over — so a request that resolves neither is
/// refused on the configuration profile.
///
/// A session is over a DEPLOYED configuration — the operation's own doc is "starts a
/// configuration session used to retrieve a deployed configuration" — so a triple whose
/// configuration profile has no configuration IN SERVICE in that environment
/// ([`AppConfigState::deployed_configuration_of`], which is every deployment of it that was
/// rolled back or reverted having left none) has no configuration for a session to be over,
/// and the deployment is the resource that is missing. Whether the configuration that
/// deployment put in force is the one THIS session is served is a separate question,
/// answered per poll by [`served_configuration`]: what makes a session possible is that the
/// profile HAS a deployed configuration here.
///
/// [`AppConfigState::deployed_configuration_of`]: ::service_com_amazonaws_appconfig_amazonappconfig::state::Environment::deployed_configuration_of
///
/// `target` is where the session being started stands in a gradual rollout
/// ([`Session::rollout_bucket`]), which is what the configuration it is bound to is
/// resolved for: a session a rollout under way has not reached yet is bound to the
/// version that preceded it.
///
/// A one-directional, guard-free read of the AppConfig store: it takes that store's
/// lock, holds no other while it does, and hands back owned values — so it is called
/// with NO guard of this service's own store held. Nothing is written: existence of a
/// deployment record is not something the clock changes.
#[allow(clippy::too_many_arguments)]
pub fn resolve_deployed_triple<Eng: Send + Sync + 'static>(
    engine: &Eng,
    bindings: &NameBindings,
    region: &str,
    application_identifier: &str,
    environment_identifier: &str,
    configuration_profile_identifier: &str,
    target: RolloutBucket,
    now: SystemTime,
) -> Option<Result<SessionBinding, UnresolvedResource>> {
    // A deployment whose environment's monitored CloudWatch alarms have already made a
    // rollback due is rolled back before this resolution reads the environment: a
    // deployment the alarm, or an unreadable monitor role, already ended never put a
    // configuration into service here, and that is what decides whether there is anything
    // for a session to be over. Called with no AppConfig guard held.
    ::service_com_amazonaws_appconfig_amazonappconfig::state::alarm_monitors::settle(engine, now);
    let state = app_config_state(engine)?;
    let store = state.lock();
    let (resolved, learned_names) = match resolve_in(
        &store,
        bindings,
        application_identifier,
        environment_identifier,
        configuration_profile_identifier,
    ) {
        Err(unresolved) => return Some(Err(unresolved)),
        Ok(resolution) => resolution,
    };
    // Every id came out of the resolution above, so each lookup holds.
    let deployed = store
        .application(&resolved.application_id)
        .and_then(|application| application.environments.get(&resolved.environment_id))
        .is_some_and(|environment| {
            environment
                .deployed_configuration_of(&resolved.configuration_profile_id)
                .is_some()
        });
    if !deployed {
        return Some(Err(UnresolvedResource::deployment(
            application_identifier,
            environment_identifier,
            configuration_profile_identifier,
        )));
    }
    // What the triple has in force right now is what the session is bound to. A triple
    // that has been deployed into but has nothing in force binds no configuration, and
    // that is the one session a poll has nothing to serve for.
    let pending = served_in(
        &store,
        &resolved,
        application_identifier,
        environment_identifier,
        configuration_profile_identifier,
        target,
        now,
    )
    .and_then(Result::ok);
    // The AppConfig guard is released before an EXTERNAL source is read, so the two stores
    // are taken one at a time, never nested.
    drop(store);
    let configuration = pending
        .and_then(|pending| {
            source_content(
                engine,
                region,
                configuration_profile_identifier,
                pending,
            )
            .ok()
        })
        .map(Arc::new);
    Some(Ok(SessionBinding {
        learned_names,
        configuration,
    }))
}

/// The configuration a session over this triple polls, at `now`.
///
/// The configuration in force for a configuration profile is the one the most recent
/// deployment into that environment deployed, counting only deployments whose rollout
/// has begun and has not been undone — a deployment a caller stopped is rolling back
/// to, or has restored, the configuration that preceded it, so its own version is not
/// what the data plane serves.
///
/// A rollout still under way has reached only part of its targets, and a configuration
/// session is one target: `target` is where this session stands
/// ([`Session::rollout_bucket`]), so a session a rollout has not reached yet is served
/// the previous version's exact bytes until it does, and the fleet converges on the new
/// version as the rollout grows.
///
/// What that version's stored bytes MEAN is the configuration profile's `Type`, so the
/// content handed back is the profile's own
/// `ConfigurationProfile::served_content` of them rather than the stored bytes: a
/// feature-flags profile stores an authoring document and serves the flag document
/// resolved from it.
///
/// WHERE those bytes live is the configuration profile's `LocationUri`: a profile on the
/// AppConfig hosted configuration store has its versions in the control plane's own store,
/// and one pointing at another source — a Parameter Store parameter, say — has the version
/// the deployment named at that source, read out of the source's own store
/// (`configuration_sources::source_configuration`) and served with the media type that
/// source describes it by. `region` is the Region the reading request was signed for, which
/// is the endpoint the source is resolved through.
///
/// Reporting-only: the environment is brought up to whatever state the CLOCK already
/// owes it on a COPY, so a poll never writes to the control plane. The session's
/// identifiers are resolved through `bindings` exactly as the session's own mint
/// resolved them ([`resolve_deployed_triple`]), and a poll binds no name of its own: the
/// names it resolves were bound when the session was started.
#[allow(clippy::too_many_arguments)]
pub fn served_configuration<Eng: Send + Sync + 'static>(
    engine: &Eng,
    bindings: &NameBindings,
    region: &str,
    application_identifier: &str,
    environment_identifier: &str,
    configuration_profile_identifier: &str,
    target: RolloutBucket,
    now: SystemTime,
) -> Option<Result<ServedConfiguration, UnresolvedResource>> {
    // A deployment whose environment's monitored CloudWatch alarms have already made a
    // rollback due is rolled back before this poll reads what is in force: that breach fell
    // due at an instant that has passed, and it is the control plane's own reading of it
    // (`…appconfig…::state::alarm_monitors::settle`), so a poll sees the same rollback every
    // reader of the deployment does — and the version that preceded it back in force.
    // Called with no AppConfig guard held, which is why it is here and not in `served_in`.
    ::service_com_amazonaws_appconfig_amazonappconfig::state::alarm_monitors::settle(engine, now);
    let state = app_config_state(engine)?;
    let store = state.lock();
    let (resolved, _learned) = match resolve_in(
        &store,
        bindings,
        application_identifier,
        environment_identifier,
        configuration_profile_identifier,
    ) {
        Err(unresolved) => return Some(Err(unresolved)),
        Ok(resolution) => resolution,
    };
    let pending = match served_in(
        &store,
        &resolved,
        application_identifier,
        environment_identifier,
        configuration_profile_identifier,
        target,
        now,
    )? {
        Err(unresolved) => return Some(Err(unresolved)),
        Ok(pending) => pending,
    };
    // The AppConfig guard is released before an EXTERNAL source is read, so the two stores
    // are taken one at a time, never nested.
    drop(store);
    Some(source_content(
        engine,
        region,
        configuration_profile_identifier,
        pending,
    ))
}

/// What a triple has in force, before the content of an external source has been read.
///
/// A configuration profile's `LocationUri` says which store its configuration lives in,
/// and only the AppConfig hosted store is one this service can read under the control
/// plane's own guard; every other store is read by
/// [`configuration_sources::source_configuration`] once that guard is released, which is
/// what this carries the resolution to ([`source_content`]).
#[derive(Debug, Clone)]
enum PendingConfiguration {
    /// The hosted version in force, already resolved out of AppConfig's own store.
    Hosted(ServedConfiguration),
    /// The configuration lives at an external source: the location the profile points at,
    /// and the SOURCE version the in-force deployment named.
    Source {
        location_uri: String,
        configuration_version: String,
    },
}

/// The configuration a reader serves for `pending`, reading the external source when the
/// profile's configuration lives in one.
///
/// A deployment of a profile whose `LocationUri` is not the hosted store names a version of
/// that SOURCE, so the content served is the content that source holds for that version,
/// described by that source's own media type — and the deployed version stays pinned, since
/// a later write at the source mints a new version and leaves the deployed one where it
/// was. A source that holds no such version has nothing for this read to serve, which is
/// the same `Configuration` refusal a hosted version the store does not hold answers with.
///
/// Called with NO guard of the AppConfig store held: the source's own store is read
/// one-directionally, exactly as the control plane is.
fn source_content<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    configuration_profile_identifier: &str,
    pending: PendingConfiguration,
) -> Result<ServedConfiguration, UnresolvedResource> {
    let (location_uri, configuration_version) = match pending {
        PendingConfiguration::Hosted(served) => return Ok(served),
        PendingConfiguration::Source {
            location_uri,
            configuration_version,
        } => (location_uri, configuration_version),
    };
    match configuration_sources::source_configuration(
        engine,
        region,
        &location_uri,
        &configuration_version,
    ) {
        Ok(source) => Ok(ServedConfiguration {
            content: source.content,
            content_type: source.content_type,
            // `VersionLabel` is "the user-defined label for the AppConfig hosted
            // configuration version" (`smithy_query(query="shape",
            // shape="GetLatestConfigurationResponse")`), so a version of another store
            // carries none.
            version_label: None,
            // Which configuration version this is: the SOURCE version the deployment
            // named, which is what a later deployment naming another version changes —
            // and so what makes "the client already has the latest version" a comparison
            // of two stored values for an external source too.
            version: configuration_version,
        }),
        Err(unavailable) => Err(UnresolvedResource::configuration(
            unavailable.refusal_message(
                configuration_profile_identifier,
                &configuration_version,
                &location_uri,
            ),
        )),
    }
}

/// The configuration a triple whose ids are already resolved has in force at `now`,
/// inside an already-locked AppConfig store — the one read both the session's mint and
/// its polls answer from, so neither decides what "in force" means for itself.
///
/// For a profile whose configuration lives in AppConfig's hosted store that is the
/// version's content itself; for one that points at another store it is the SOURCE version
/// the in-force deployment named, whose content is read once this guard is released
/// ([`PendingConfiguration`], [`source_content`]).
///
/// `None` only for ids the store does not hold, which cannot happen for a resolution
/// this store itself produced.
#[allow(clippy::too_many_arguments)]
fn served_in(
    store: &AppConfigState,
    resolved: &ResolvedTriple,
    application_identifier: &str,
    environment_identifier: &str,
    configuration_profile_identifier: &str,
    target: RolloutBucket,
    now: SystemTime,
) -> Option<Result<PendingConfiguration, UnresolvedResource>> {
    let application = store.application(&resolved.application_id)?;
    let profile = application.profiles.get(&resolved.configuration_profile_id)?;
    let environment = application.environments.get(&resolved.environment_id)?;

    // A COPY is materialised: a deployment whose rollout window and bake time have
    // both elapsed is `COMPLETE` whether or not anybody has read it, and bringing the
    // copy up to date leaves the stored record untouched.
    let mut environment = environment.clone();
    environment.materialize(now);

    let Some(deployment) =
        environment.in_force_deployment(&resolved.configuration_profile_id, target)
    else {
        return Some(Err(UnresolvedResource::configuration(
            no_deployed_configuration(
                application_identifier,
                environment_identifier,
                configuration_profile_identifier,
            ),
        )));
    };
    // WHICH STORE the version lives in is the profile's own `LocationUri`: the hosted
    // versions below are AppConfig's own store, and a profile pointing at any other source
    // has its deployed version's content there instead, read once this store's guard is
    // released ([`source_content`]). So the hosted-store refusal below answers only for a
    // profile that uses the hosted store.
    if !matches!(
        configuration_sources::source_of(&profile.location_uri),
        configuration_sources::ConfigurationSource::Hosted
    ) {
        return Some(Ok(PendingConfiguration::Source {
            location_uri: profile.location_uri.clone(),
            configuration_version: deployment.configuration_version.clone(),
        }));
    }
    let Some(version) = profile.resolve_hosted_version(&deployment.configuration_version) else {
        return Some(Err(UnresolvedResource::configuration(
            configuration_version_not_hosted(
                configuration_profile_identifier,
                &deployment.configuration_version,
            ),
        )));
    };
    Some(Ok(PendingConfiguration::Hosted(ServedConfiguration {
        // What the profile's `Type` makes of the stored bytes: the control plane owns that
        // rule, so the data plane asks the profile rather than deciding here.
        content: profile.served_content(&version.content),
        content_type: version.content_type.clone(),
        version_label: version.version_label.clone(),
        version: version.version_number.to_string(),
    })))
}

/// The ids a triple resolves to inside an already-locked AppConfig store.
struct ResolvedTriple {
    application_id: String,
    environment_id: String,
    configuration_profile_id: String,
}

/// Resolve the three identifiers against `store` and `bindings`: the application first,
/// then — of its two children — the configuration profile before the environment (see
/// [`resolve_deployed_triple`]), answering the ids and the names this resolution learned.
fn resolve_in(
    store: &AppConfigState,
    bindings: &NameBindings,
    application_identifier: &str,
    environment_identifier: &str,
    configuration_profile_identifier: &str,
) -> Result<(ResolvedTriple, Vec<LearnedName>), UnresolvedResource> {
    let mut learned = Vec::new();
    let unresolved_application = || {
        UnresolvedResource::named(
            RESOURCE_TYPE_APPLICATION,
            APPLICATION_IDENTIFIER_MEMBER,
            application_identifier,
        )
    };
    let application_id = resolve_identifier(
        bindings,
        &mut learned,
        RESOURCE_TYPE_APPLICATION,
        ACCOUNT_SCOPE,
        application_identifier,
        |id| store.application(id).is_some(),
        |name| {
            store
                .resolve_application(name)
                .map(|application| application.id.clone())
        },
    )
    .ok_or_else(unresolved_application)?;
    // The id came out of the resolution above, which answers only for an application
    // this store holds.
    let application = store
        .application(&application_id)
        .ok_or_else(unresolved_application)?;
    let configuration_profile_id = resolve_identifier(
        bindings,
        &mut learned,
        RESOURCE_TYPE_CONFIGURATION_PROFILE,
        &application_id,
        configuration_profile_identifier,
        |id| application.profiles.contains_key(id),
        |name| {
            application
                .resolve_profile(name)
                .map(|profile| profile.id.clone())
        },
    )
    .ok_or_else(|| {
        UnresolvedResource::named(
            RESOURCE_TYPE_CONFIGURATION_PROFILE,
            CONFIGURATION_PROFILE_IDENTIFIER_MEMBER,
            configuration_profile_identifier,
        )
    })?;
    let environment_id = resolve_identifier(
        bindings,
        &mut learned,
        RESOURCE_TYPE_ENVIRONMENT,
        &application_id,
        environment_identifier,
        |id| application.environments.contains_key(id),
        |name| {
            application
                .resolve_environment(name)
                .map(|environment| environment.id.clone())
        },
    )
    .ok_or_else(|| {
        UnresolvedResource::named(
            RESOURCE_TYPE_ENVIRONMENT,
            ENVIRONMENT_IDENTIFIER_MEMBER,
            environment_identifier,
        )
    })?;
    Ok((
        ResolvedTriple {
            application_id,
            environment_id,
            configuration_profile_id,
        },
        learned,
    ))
}

/// Resolve ONE identifier of a session's triple: the id of the record it names, or
/// `None` when nothing at this level answers to it.
///
/// `holds_id` says whether the control plane holds a record with that id at this level
/// and scope, and `by_name` resolves a NAME against the control plane's current records.
/// An identifier is answered, in order, by:
///
/// 1. the record the control plane holds under it as an ID — an id names one record for
///    as long as that record exists, so nothing about it is remembered here;
/// 2. the binding this service already recorded for the name, for as long as the record
///    it was bound to is still there — so a control-plane rename neither breaks the name
///    nor hands it to whatever record claims it next;
/// 3. the control plane's current records, by name — the first resolution of a name, and
///    the only one that LEARNS a binding (which is also how a binding whose record is
///    gone gives way to the record that holds the name now).
fn resolve_identifier(
    bindings: &NameBindings,
    learned: &mut Vec<LearnedName>,
    level: &'static str,
    scope: &str,
    identifier: &str,
    holds_id: impl Fn(&str) -> bool,
    by_name: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if holds_id(identifier) {
        return Some(identifier.to_string());
    }
    if let Some(bound) = bindings.bound_id(level, scope, identifier) {
        if holds_id(bound) {
            return Some(bound.to_string());
        }
    }
    let id = by_name(identifier)?;
    learned.push(LearnedName {
        key: BoundName {
            level,
            scope: scope.to_string(),
            name: identifier.to_string(),
        },
        id: id.clone(),
    });
    Some(id)
}
