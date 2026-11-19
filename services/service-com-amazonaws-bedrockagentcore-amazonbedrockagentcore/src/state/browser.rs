// SPDX-License-Identifier: CC0-1.0

//! The AgentCore *browser* tools plane: the sessions a browser holds.
//!
//! A browser is created through the CONTROL plane, a different service, so a browser is
//! registered here by the first `StartBrowserSession` that names it — the same way a
//! code interpreter is registered by the first session started on it. The record a start
//! writes ([`BrowserSessionRecord`]) is exactly what `GetBrowserSession` reports back:
//! its streams ([`BrowserSessionStreamRecord`]) are materialised by the start, and its
//! status is read off the stop that terminated it or off the clock against the session's
//! effective time-to-live ([`BrowserSessionRecord::status_as_of`]), so no call is needed
//! to make a termination that is already due visible.
//!
//! `StartBrowserSession` is this plane's only growth site and a caller chooses both the
//! `browserIdentifier` and the `clientToken` it inserts under, so each level carries a
//! maximum and evicts its least recently written record once that maximum is passed
//! ([`MAX_BROWSERS`], [`MAX_BROWSER_SESSIONS`],
//! [`MAX_IDEMPOTENCY_RECORDS_PER_BROWSER`]).

use std::collections::{BTreeMap, HashMap};
use std::time::{Duration, SystemTime};

use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as control_plane;

use crate::ops::types::{
    AutomationStream, BrowserEnterprisePolicy, BrowserExtension, BrowserProfileConfiguration,
    BrowserSessionStream, Certificate, LiveViewStream, ProxyConfiguration,
    StartBrowserSessionInput, ToolsFileSystemConfiguration, ViewPort,
};

use super::{
    least_recently_written, ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState,
    SessionLocator,
};

/// The most browsers this emulator retains sessions for; the least recently used one is
/// evicted once the store passes it. An EMULATOR ceiling, not an AWS quota: a caller
/// names the `browserIdentifier` every start inserts under.
pub const MAX_BROWSERS: usize = 32;

/// The most sessions this emulator retains for one browser.
pub const MAX_BROWSER_SESSIONS: usize = 256;

/// The most `clientToken` records this emulator retains for one browser.
pub const MAX_IDEMPOTENCY_RECORDS_PER_BROWSER: usize = 1_000;

/// The most replayed-call `clientToken` records ONE session keeps, so a caller looping
/// on `StopBrowserSession` or `UpdateBrowserStream` with a fresh token cannot grow the
/// record without limit.
pub const MAX_IDEMPOTENT_CALL_TOKENS_PER_SESSION: usize = 256;

/// `BrowserSessionStatus` for a session that is still inside its time-to-live, so it can
/// serve work (source: smithy_query query="shape" shape="BrowserSessionStatus", values
/// READY | TERMINATED).
pub const BROWSER_SESSION_READY: &str = "READY";

/// `BrowserSessionStatus` for a session a stop has terminated or whose time-to-live has
/// run out, from the same enum.
pub const BROWSER_SESSION_TERMINATED: &str = "TERMINATED";

/// `AutomationStreamStatus` for an automation stream that carries commands (source:
/// smithy_query query="shape" shape="AutomationStreamStatus", values ENABLED |
/// DISABLED).
pub const AUTOMATION_STREAM_ENABLED: &str = "ENABLED";

/// `AutomationStreamStatus` for one that does not, from the same enum.
pub const AUTOMATION_STREAM_DISABLED: &str = "DISABLED";

/// The `@default` the model declares for
/// `StartBrowserSessionRequest$sessionTimeoutSeconds` (source: smithy_query
/// query="operation" operation="StartBrowserSession"). The protocol's request walk binds
/// it server-side for a request that carries no timeout, which is the only trace such a
/// request leaves a handler.
pub const MODELLED_SESSION_TIMEOUT_DEFAULT: i32 = 3_600;

/// The time-to-live, in seconds, a browser session runs under when its start asks for
/// none: the service assigns its own fifteen-minute default rather than the value the
/// request member's `@default` declares, and the assigned value is what every read of
/// the session reports.
pub const SERVICE_SESSION_TIMEOUT_DEFAULT: i32 = 900;

/// The time-to-live a session started with `requested` seconds actually runs under: the
/// requested value, or [`SERVICE_SESSION_TIMEOUT_DEFAULT`] when the request named no
/// timeout at all.
///
/// A request that names none is bound to [`MODELLED_SESSION_TIMEOUT_DEFAULT`] before any
/// handler sees it, so that value IS "no timeout requested" on this wire; every other
/// value in the member's range is the session's own.
pub fn effective_session_timeout_seconds(requested: i32) -> i32 {
    if requested == MODELLED_SESSION_TIMEOUT_DEFAULT {
        SERVICE_SESSION_TIMEOUT_DEFAULT
    } else {
        requested
    }
}

/// The path every stream of one browser session is addressed under, beneath the address
/// this emulator answers on: the service lays a session's streams out per browser and
/// session, with one trailing segment per stream
/// (`/browser-streams/{browserIdentifier}/sessions/{sessionId}/<stream>`).
const STREAM_PATH_PREFIX: &str = "browser-streams";

/// The trailing segment of the stream a browser-automation client drives the session
/// over.
const AUTOMATION_STREAM_SEGMENT: &str = "automation";

/// The scheme the automation stream is addressed over: the stream is the bidirectional
/// channel a client "establish[es] a WebSocket connection to … for sending commands and
/// receiving responses" (source: smithy_query query="shape" shape="AutomationStream",
/// member `streamEndpoint`), so its address is a WebSocket URL on the host this emulator
/// answers on, where the live view is read over that host's own scheme.
const AUTOMATION_STREAM_SCHEME: &str = "wss";

/// The scheme every address this emulator answers on carries
/// ([`smithy_server_runtime::EndpointUrl`] parses no other), and therefore the one the
/// automation stream's scheme replaces.
const ENDPOINT_SCHEME_PREFIX: &str = "https://";

/// The trailing segment of the stream an embedded live view reads the session from.
const LIVE_VIEW_STREAM_SEGMENT: &str = "live-view";

/// The streams one browser session exposes: the addresses `StartBrowserSession`
/// materialised for it and the automation stream's status, which is the ONE place
/// `StartBrowserSession`, `GetBrowserSession` and `UpdateBrowserStream` read a session's
/// streams from, so no two of them can report different ones.
#[derive(Debug, Clone)]
pub struct BrowserSessionStreamRecord {
    /// Where a browser-automation client dials this session. `AutomationStream` models
    /// the member `@required` with `@length 10..512`, so a session always carries one.
    pub automation_endpoint: String,
    /// Where an embedded live view reads this session from.
    pub live_view_endpoint: String,
    /// Whether the automation stream carries commands: enabled for a session the service
    /// has just started, and afterwards whatever the last `UpdateBrowserStream` set.
    pub automation_enabled: bool,
}

impl BrowserSessionStreamRecord {
    /// The streams a session gets when it is started: an endpoint per stream under
    /// `endpoint_base` — the address this emulator answers on, which is where a client
    /// of THIS emulator reaches a resource of it — and an automation stream the started
    /// session can be driven over.
    ///
    /// The host and port of an endpoint are the environment's; its presence and the
    /// session it addresses are the session's.
    pub fn materialized(endpoint_base: &str, browser_identifier: &str, session_id: &str) -> Self {
        let session_path =
            format!("{STREAM_PATH_PREFIX}/{browser_identifier}/sessions/{session_id}");
        let host = endpoint_base
            .strip_prefix(ENDPOINT_SCHEME_PREFIX)
            .unwrap_or(endpoint_base);
        Self {
            automation_endpoint: format!(
                "{AUTOMATION_STREAM_SCHEME}://{host}/{session_path}/{AUTOMATION_STREAM_SEGMENT}"
            ),
            live_view_endpoint: format!(
                "{endpoint_base}/{session_path}/{LIVE_VIEW_STREAM_SEGMENT}"
            ),
            automation_enabled: true,
        }
    }
}

/// One browser's sessions, as `StartBrowserSession` wrote them.
#[derive(Debug, Default)]
pub struct BrowserStore {
    /// Sessions keyed by their minted `sessionId`. Bounded by
    /// [`MAX_BROWSER_SESSIONS`].
    pub sessions: HashMap<String, BrowserSessionRecord>,
    /// `clientToken` -> the session that token already started. `StartBrowserSession` is
    /// `@idempotent` and its `clientToken` is an `@idempotencyToken`, so a replayed
    /// token answers with the first session rather than starting a second one. Bounded
    /// by [`MAX_IDEMPOTENCY_RECORDS_PER_BROWSER`], and a record is dropped with the
    /// session it points at.
    pub idempotency: HashMap<String, SessionLocator>,
    /// The sequence of the last session started on this browser, which is what its
    /// eviction order is read from.
    pub last_seq: u64,
}

/// One browser session: what `StartBrowserSession` persisted, which is what
/// `GetBrowserSession` reports.
///
/// Every configuration member the start accepted is kept as the request asked for it,
/// because the read reports each one back; nothing here is defaulted to a value AWS does
/// not publish.
#[derive(Debug, Clone)]
pub struct BrowserSessionRecord {
    /// Minted by [`mint_browser_session_id`]; never client-supplied.
    pub session_id: String,
    pub browser_identifier: String,
    pub name: Option<String>,
    pub created_at: SystemTime,
    /// The session's EFFECTIVE time-to-live in seconds: what the request asked for, or
    /// the service's own default when it asked for nothing
    /// ([`effective_session_timeout_seconds`]). This is the value the read reports and
    /// the span the status is read against, so the two cannot disagree.
    pub session_timeout_seconds: i32,
    /// The viewport the start asked for, or `None` when it asked for none: AWS
    /// publishes no default viewport size, so this emulator reports no dimensions
    /// rather than dimensions of its own choosing.
    pub view_port: Option<ViewPort>,
    pub extensions: Option<Vec<BrowserExtension>>,
    pub enterprise_policies: Option<Vec<BrowserEnterprisePolicy>>,
    pub profile_configuration: Option<BrowserProfileConfiguration>,
    pub proxy_configuration: Option<ProxyConfiguration>,
    pub certificates: Option<Vec<Certificate>>,
    pub filesystem_configurations: Option<Vec<ToolsFileSystemConfiguration>>,
    /// When this session was last changed by a call: its creation instant until a call
    /// changes it, and the instant of the stop or stream update that last did
    /// afterwards.
    pub last_updated_at: SystemTime,
    /// The streams this session exposes, materialised when it was started.
    pub streams: BrowserSessionStreamRecord,
    /// What the OS-level actions `InvokeBrowser` performed on this session have left on
    /// it: where its pointer stands, how far it has been scrolled, how much has been
    /// typed into it. The screen those actions happen on is the session's viewport
    /// ([`BrowserSessionRecord::surface`]), so this plane's input state and its rendered
    /// screenshots cannot disagree about where anything is.
    pub input: super::BrowserInputState,
    /// When `StopBrowserSession` terminated this session, or `None` while no stop has. A
    /// stopped session is `TERMINATED` from that instant on, whatever is left of its
    /// time-to-live, and the documented effect of a stop is that it cannot be restarted.
    pub stopped_at: Option<SystemTime>,
    /// The `clientToken` of each call this session has already served, with the instant
    /// that call answered with: both `StopBrowserSession` and `UpdateBrowserStream` are
    /// `@idempotent` with an `@idempotencyToken` `clientToken`, so a replayed token
    /// answers with the call it already performed rather than conflicting with it.
    /// Bounded by [`MAX_IDEMPOTENT_CALL_TOKENS_PER_SESSION`].
    pub idempotent_calls: BTreeMap<(BrowserSessionCall, String), SystemTime>,
    /// The value [`ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState::next_seq`]
    /// handed this session: its eviction order, and what keeps two sessions started in
    /// the same millisecond apart.
    pub seq: u64,
}

impl BrowserSessionRecord {
    /// A session as the call that starts it leaves it: the configuration that call asked
    /// for, created at `created_at`, with the streams that call materialised under
    /// `endpoint_base`, nothing stopped and nothing changed since.
    pub fn started(
        seq: u64,
        created_at: SystemTime,
        input: &StartBrowserSessionInput,
        endpoint_base: &str,
    ) -> Self {
        let session_id = mint_browser_session_id(seq);
        Self {
            streams: BrowserSessionStreamRecord::materialized(
                endpoint_base,
                &input.browser_identifier,
                &session_id,
            ),
            session_id,
            browser_identifier: input.browser_identifier.clone(),
            name: input.name.clone(),
            created_at,
            session_timeout_seconds: effective_session_timeout_seconds(
                input.session_timeout_seconds,
            ),
            view_port: input.view_port.clone(),
            extensions: input.extensions.clone(),
            enterprise_policies: input.enterprise_policies.clone(),
            profile_configuration: input.profile_configuration.clone(),
            proxy_configuration: input.proxy_configuration.clone(),
            certificates: input.certificates.clone(),
            filesystem_configurations: input.filesystem_configurations.clone(),
            // Nothing has changed this session yet, so the last instant it changed is
            // the instant it came into being.
            last_updated_at: created_at,
            // A session that has just been started has performed no action, so its
            // screen carries no pointer and nothing has been typed into it.
            input: super::BrowserInputState::default(),
            stopped_at: None,
            idempotent_calls: BTreeMap::new(),
            seq,
        }
    }

    /// The instant this session's time-to-live runs out, or `None` when that instant is
    /// not representable (so nothing can be past it).
    ///
    /// `sessionTimeoutSeconds` is modelled as "the duration in seconds (time-to-live)
    /// after which the session automatically terminates, regardless of ongoing
    /// activity", so the session's own effective value is the span — nothing chosen
    /// here. The model's range makes it positive; a value that is not is read as no time
    /// at all rather than as a time in the past.
    pub fn expires_at(&self) -> Option<SystemTime> {
        let ttl = Duration::from_secs(u64::try_from(self.session_timeout_seconds).unwrap_or(0));
        self.created_at.checked_add(ttl)
    }

    /// This session's `BrowserSessionStatus` at `now`: `TERMINATED` once a stop has
    /// terminated it or its time-to-live has run out, `READY` until then. Both readings
    /// come off stored state and the clock, so a read reports the termination that was
    /// already due without writing anything.
    pub fn status_as_of(&self, now: SystemTime) -> &'static str {
        if self.stopped_at.is_some() {
            return BROWSER_SESSION_TERMINATED;
        }
        match self.expires_at() {
            Some(expiry) if now >= expiry => BROWSER_SESSION_TERMINATED,
            _ => BROWSER_SESSION_READY,
        }
    }

    /// This session's streams, as every operation that reports them reports them: the
    /// endpoints the start materialised, and the automation stream's status as the start
    /// enabled it and `UpdateBrowserStream` last set it.
    ///
    /// `AutomationStreamStatus` is the STREAM's own status, not the session's: the
    /// session's `BrowserSessionStatus` is what says whether the session can be used at
    /// all, so a termination is reported there and leaves the stream reporting the status
    /// that was set for it.
    pub fn streams(&self) -> BrowserSessionStream {
        BrowserSessionStream {
            automation_stream: AutomationStream {
                stream_endpoint: self.streams.automation_endpoint.clone(),
                stream_status: if self.streams.automation_enabled {
                    AUTOMATION_STREAM_ENABLED.to_string()
                } else {
                    AUTOMATION_STREAM_DISABLED.to_string()
                },
            },
            live_view_stream: Some(LiveViewStream {
                stream_endpoint: Some(self.streams.live_view_endpoint.clone()),
            }),
        }
    }

    /// Terminate this session at `now`, as `StopBrowserSession` does: the status every
    /// later read reports becomes `TERMINATED`, and `now` is the `lastUpdatedAt` this
    /// stop and every replay of its `client_token` answer with.
    pub fn stop(&mut self, now: SystemTime, client_token: Option<&str>) {
        self.stopped_at = Some(now);
        self.last_updated_at = now;
        self.record_call(BrowserSessionCall::Stop, client_token, now);
    }

    /// Set whether this session's automation stream carries commands, as
    /// `UpdateBrowserStream` does, at `now`.
    pub fn set_automation_stream_enabled(
        &mut self,
        enabled: bool,
        now: SystemTime,
        client_token: Option<&str>,
    ) {
        self.streams.automation_enabled = enabled;
        self.last_updated_at = now;
        self.record_call(BrowserSessionCall::StreamUpdate, client_token, now);
    }

    /// The instant the `call` this `client_token` already performed answered with, or
    /// `None` when this token has performed none of that call.
    pub fn replayed_call(
        &self,
        call: BrowserSessionCall,
        client_token: &str,
    ) -> Option<SystemTime> {
        self.idempotent_calls
            .get(&(call, client_token.to_string()))
            .copied()
    }

    /// Record that `client_token` performed `call` at `at`, holding
    /// [`MAX_IDEMPOTENT_CALL_TOKENS_PER_SESSION`]: the earliest-recorded token gives way
    /// first, so the call a caller just made stays answerable.
    fn record_call(
        &mut self,
        call: BrowserSessionCall,
        client_token: Option<&str>,
        at: SystemTime,
    ) {
        let Some(token) = client_token else {
            return;
        };
        self.idempotent_calls.insert((call, token.to_string()), at);
        while self.idempotent_calls.len() > MAX_IDEMPOTENT_CALL_TOKENS_PER_SESSION {
            let Some(oldest) = self
                .idempotent_calls
                .iter()
                .min_by_key(|(key, at)| (**at, (*key).clone()))
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.idempotent_calls.remove(&oldest);
        }
    }
}

/// Which call of a browser session a recorded `clientToken` served, so the token of one
/// operation is never read as the token of another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BrowserSessionCall {
    /// `StopBrowserSession`.
    Stop,
    /// `UpdateBrowserStream`.
    StreamUpdate,
}

impl ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState {
    /// The stored browser session addressed by (browser, session), or `None` when this
    /// emulator holds no such session.
    pub fn browser_session(
        &self,
        browser_identifier: &str,
        session_id: &str,
    ) -> Option<&BrowserSessionRecord> {
        self.browsers
            .get(browser_identifier)?
            .sessions
            .get(session_id)
    }

    /// The stored browser session addressed by (browser, session), for a caller that is
    /// about to CHANGE it.
    pub fn browser_session_mut(
        &mut self,
        browser_identifier: &str,
        session_id: &str,
    ) -> Option<&mut BrowserSessionRecord> {
        self.browsers
            .get_mut(browser_identifier)?
            .sessions
            .get_mut(session_id)
    }

    /// The session a `clientToken` already started on this browser, or `None` when that
    /// token started none (or the session it started is no longer held).
    pub fn browser_session_for_token(
        &self,
        browser_identifier: &str,
        client_token: &str,
    ) -> Option<&BrowserSessionRecord> {
        let browser = self.browsers.get(browser_identifier)?;
        let session_id = &browser.idempotency.get(client_token)?.session_id;
        browser.sessions.get(session_id)
    }

    /// Persist one browser session — registering its browser on the way — and hold every
    /// bound this plane declares, so no caller can grow the store without limit.
    pub fn insert_browser_session(
        &mut self,
        session: BrowserSessionRecord,
        client_token: Option<String>,
    ) {
        let seq = session.seq;
        let session_id = session.session_id.clone();
        let browser = self
            .browsers
            .entry(session.browser_identifier.clone())
            .or_default();
        browser.last_seq = seq;
        browser.sessions.insert(session_id.clone(), session);
        if let Some(token) = client_token {
            browser
                .idempotency
                .insert(token, SessionLocator { session_id, seq });
            while browser.idempotency.len() > MAX_IDEMPOTENCY_RECORDS_PER_BROWSER {
                let Some(oldest) =
                    least_recently_written(&browser.idempotency, |locator| locator.seq)
                else {
                    break;
                };
                browser.idempotency.remove(&oldest);
            }
        }

        while browser.sessions.len() > MAX_BROWSER_SESSIONS {
            let Some(oldest) = least_recently_written(&browser.sessions, |session| session.seq)
            else {
                break;
            };
            browser.sessions.remove(&oldest);
            browser
                .idempotency
                .retain(|_, locator| locator.session_id != oldest);
        }
        while self.browsers.len() > MAX_BROWSERS {
            let Some(oldest) = least_recently_written(&self.browsers, |browser| browser.last_seq)
            else {
                break;
            };
            self.browsers.remove(&oldest);
        }
    }
}

/// Mint a `BrowserSessionId`, in the form
/// [`super::mint_alphanumeric_session_id`] states for that shape's pattern
/// (`^[0-9a-zA-Z]{1,40}$`, which `CodeInterpreterSessionId` publishes too).
pub fn mint_browser_session_id(seq: u64) -> String {
    super::mint_alphanumeric_session_id(seq)
}

/// Whether the browser a request names is one this emulator holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowserAvailability {
    /// The browser exists: the tool the service provides, or one the control plane
    /// created and has not removed.
    Held,
    /// No browser of that identifier exists, so nothing can be started on it.
    Unknown,
    /// The control plane's own store could not be reached, which is this emulator's
    /// fault and not the request's.
    StoreUnavailable,
}

/// Whether `browser_identifier` names a browser, read off the component that OWNS
/// browsers.
///
/// A browser is NOT this data plane's resource: the service-provided browser tool
/// (`aws.browser.v1`) is held by every account without anything creating it, and a custom
/// browser is created, reported and deleted through the AgentCore control plane
/// (`CreateBrowser` / `GetBrowser` / `DeleteBrowser`). So a session can only be started on
/// a browser that control plane returns from its own read — an identifier naming none is
/// an identifier naming nothing, whatever it looks like.
///
/// A browser's teardown completes inside the control plane's `DeleteBrowser` call, so its
/// store holds exactly the browsers that exist at this instant: this answers what
/// `GetBrowser` would answer at the same instant, with no transition left to materialise.
///
/// WHY THE OWNER'S STORE AND NOT THE SHARED MIRROR: the sibling code interpreter plane
/// asks `service_common::agentcore_resources::AgentCoreResourceInventory`, a registry the
/// control plane writes as it creates and deletes. That inventory has no browser half,
/// and a registry entry is not the owner's read: a browser the owner's store no longer
/// holds is the answer a session has to be refused on. Converging the two planes on one
/// idiom means giving the inventory that reach, not adding a third mechanism here.
pub fn browser_availability<Eng: Send + Sync + 'static>(
    engine: &Eng,
    browser_identifier: &str,
) -> BrowserAvailability {
    let Some(control) = control_plane::state::service_state(engine) else {
        return BrowserAvailability::StoreUnavailable;
    };
    if browser_identifier == control_plane::state::SYSTEM_BROWSER_ID {
        return BrowserAvailability::Held;
    }
    let store = control.lock();
    if store.browsers.contains_key(browser_identifier) {
        BrowserAvailability::Held
    } else {
        BrowserAvailability::Unknown
    }
}

/// Why a call addressed to a browser cannot be served, with the sentence it is refused
/// with: the ONE home of both messages, so the operations that resolve a browser cannot
/// word the same condition differently. Which modelled error carries the sentence is the
/// operation's own choice, because each one's error channel names its own variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserUnavailable {
    /// The identifier names no browser: the modelled `ResourceNotFoundException`, worded
    /// as this service words a lookup that missed
    /// ([`super::resource_not_found_message`]).
    NotFound(String),
    /// The component that owns browsers could not be reached: a modelled
    /// `InternalServerException`, which is this emulator's fault and not the request's.
    StoreUnavailable(String),
}

/// `Ok(())` when `browser_identifier` names a browser this emulator holds, or why it
/// does not and the sentence that refusal carries ([`browser_availability`] with the
/// wording attached).
pub fn resolve_browser<Eng: Send + Sync + 'static>(
    engine: &Eng,
    browser_identifier: &str,
) -> Result<(), BrowserUnavailable> {
    match browser_availability(engine, browser_identifier) {
        BrowserAvailability::Held => Ok(()),
        BrowserAvailability::Unknown => Err(BrowserUnavailable::NotFound(
            super::resource_not_found_message(
                super::NotFoundResource::Browser,
                browser_identifier,
            ),
        )),
        BrowserAvailability::StoreUnavailable => Err(BrowserUnavailable::StoreUnavailable(
            BROWSER_STORE_UNAVAILABLE.to_string(),
        )),
    }
}

/// The `InternalServerException` message a call gets when the component that owns
/// browsers could not be reached.
pub const BROWSER_STORE_UNAVAILABLE: &str = "the browser store is unavailable";

/// How this service words the `ValidationException` an action gets when the browser
/// session it names is no longer running: `Browser session <sessionId> is not active. Start
/// a new session or provide a valid session ID`, with the session taken from the request as
/// the sentence's only variable part.
///
/// A session that has been stopped or has run out of time-to-live cannot be made usable
/// again — the model documents that a stopped session cannot be restarted — so the
/// condition is a request member that no longer addresses a session anything can be
/// performed on, not a state the caller can resolve and retry, which is why the sentence
/// names starting a new session as what the caller does next.
pub fn browser_session_not_active(session_id: &str) -> String {
    format!(
        "Browser session {session_id} is not active. Start a new session or provide a valid \
         session ID"
    )
}
