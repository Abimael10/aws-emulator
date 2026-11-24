// SPDX-License-Identifier: CC0-1.0

//! The METADATA an execution environment publishes to the HANDLER running in it: the
//! environment's own facts, served on the address its environment names.
//!
//! # Why a handler needs them
//!
//! "Which Region am I in, and which zone is this environment placed in" is not answered to a
//! program by an API call: it is published TO the node the program runs on, and the program
//! reads it from an address only that node can reach. That is how a client library discovers
//! its Region and how an agent names the zone it reports, and a handler written to that pattern
//! reads it the same way. A handler whose read of that address resolves to nothing does not get
//! a worse answer: it FAILS, and the invocation comes back as an unhandled error, which is why
//! this interface is served rather than left unaddressed.
//!
//! # The two addressings
//!
//! * The EXECUTION ENVIRONMENT's own interface, at the address `AWS_LAMBDA_METADATA_API` names
//!   ([`METADATA_API_ADDRESS`], the loopback interface beside the runtime API's own —
//!   [`super::runtime_api`]). `GET`
//!   [`EXECUTION_ENVIRONMENT_PATH`] answers the document of the environment the request was
//!   made from. The read is BEARER-TOKEN gated: the environment is handed a token of its own,
//!   which its environment names (`AWS_LAMBDA_METADATA_TOKEN`), and a read presents it in the
//!   `Authorization` header — a read presenting none, or presenting anything else, is refused
//!   `401` carrying that status's reason, so the requirement is observable from inside the
//!   function.
//!
//!   The two variable names, the path, the `Authorization` header, the document's own member
//!   names and the refusal's body are this interface's RECORDED contract, taken from the
//!   behavioral recording of a handler reading it (which fails with a `KeyError` naming each
//!   variable when the environment does not name it, `GET`s exactly that path, and whose check
//!   asserts the refused read's body and reads the document's `AvailabilityZoneID`) — not from
//!   the shape of this implementation, and not from the issue's own prose, which described the
//!   read as the node wire below.
//! * The LINK-LOCAL address every node's metadata service answers at
//!   ([`::service_common::instance_metadata_service`]), whose wire this emulator already states
//!   once for every node that publishes one: a PUT of the token path mints a session token, and
//!   a read under [`METADATA_PREFIX`] presenting a token is answered from the same facts. A
//!   handler that reads its placement the way a program on an instance does is answered there
//!   rather than failing on an address nothing serves.
//!
//! Both are answered by the HOST, out of the state of the environment the request was made in
//! ([`MetadataSession`], which is the ENVIRONMENT's and not one invocation's), and the statuses
//! are the ones a metadata service answers with: `400` for a token request that states no usable
//! lifetime, `401` for a read that presents no valid token, `404` for a path the environment
//! publishes nothing at, `405` for a method a path does not take, and `503` where no token
//! could be minted at all.
//!
//! # What is real here
//!
//! Every value is a fact of the EXECUTION ENVIRONMENT the request was made from: the function it
//! is running and the version and handler of it, the account the function belongs to, the Region
//! the invocation was addressed to, an Availability Zone that Region really publishes (the one
//! table this workspace keeps those names in, which is also what Amazon EC2 reports for the
//! Region), the architecture it runs on and the instant the environment was initialised. Nothing
//! is echoed from a request, and a path whose value an execution environment has none of is
//! answered `404` rather than with an invented value — an environment is not an instance, so it
//! publishes no instance id, image or type, and the members of the node's identity document that
//! name them are `null`, as AWS writes a member a node has no value for.
//!
//! No credential of the function's execution role is published: the metadata categories that
//! carry one exist so that a program can read the credentials of the role its NODE runs as, and
//! the credential of this invocation's execution role is the host's alone
//! ([`super::guest_http`] signs with it and the guest never holds it). The token the environment
//! is handed is a credential of the environment for THIS interface and nothing else.
//!
//! Nothing here opens a socket or resolves a name: the address is compared as text and the
//! answer is built in this process, exactly as the runtime API beside it is
//! ([`super::runtime_api`]).

use ::std::sync::{Arc, Mutex, OnceLock, PoisonError};
use ::std::time::{Duration, Instant, SystemTime};

use ::serde_json::{json, Value};
use ::service_common::instance_metadata_service::{
    metadata_path, PublishedInstanceMetadata, METADATA_PREFIX, METADATA_TOKEN_HEADER,
    METADATA_TOKEN_PATH, METADATA_TOKEN_TTL_HEADER,
};

use super::guest_http::Answered;
use super::runtime_api::{header, FunctionFacts, Reached, Requested, Sandbox};
use super::warm_pool::EnvironmentKey;

/// The address the EXECUTION ENVIRONMENT's own metadata interface is published at: the
/// loopback interface and the port this emulator publishes it on, beside the runtime API's own
/// ([`super::runtime_api::RUNTIME_API_ADDRESS`]) — the host and port a handler is told to
/// address it by.
///
/// ONE spelling, shared with the environment every run is given
/// (`super::run_environment`'s `AWS_LAMBDA_METADATA_API`), so the address a handler is told to
/// use and the address this module answers on cannot drift.
pub const METADATA_API_ADDRESS: &str = "127.0.0.1:9002";

/// The path the document of the environment the request was made from is published at, under
/// the version of the interface that publishes it: one dated path, as every other interface of
/// an execution environment is versioned ([`super::runtime_api`]).
pub const EXECUTION_ENVIRONMENT_PATH: &str = "/2026-01-15/metadata/execution-environment";

/// The header a read of that interface presents the token the environment was handed in: the
/// one a caller that was GIVEN a credential presents it in, with or without the bearer scheme.
const AUTHORIZATION_HEADER: &str = "Authorization";

/// The path the zone the environment is placed in is published at ("the Availability Zone in
/// which the instance launched", Amazon EC2 User Guide, "Instance metadata categories").
const PLACEMENT_AVAILABILITY_ZONE_PATH: &str = "/latest/meta-data/placement/availability-zone";

/// The path the Region is published at ("the Region in which the instance launched").
const PLACEMENT_REGION_PATH: &str = "/latest/meta-data/placement/region";

/// The path the ID of that zone is published at ("the AZ ID of the Availability Zone in which
/// the instance launched").
const PLACEMENT_AVAILABILITY_ZONE_ID_PATH: &str =
    "/latest/meta-data/placement/availability-zone-id";

/// The listing of the placement category, whose entries are the two paths above.
const PLACEMENT_PREFIX: &str = "/latest/meta-data/placement";

/// The prefix of the paths that publish the node's identity.
const IDENTITY_PREFIX: &str = "/latest/dynamic/instance-identity";

/// The path the identity document is published at.
const IDENTITY_DOCUMENT_PATH: &str = "/latest/dynamic/instance-identity/document";

/// `version` of the identity document: the schema version the published documents carry
/// (Amazon EC2 User Guide, "Instance identity documents").
const IDENTITY_DOCUMENT_VERSION: &str = "2017-09-30";

/// The categories the listing of [`METADATA_PREFIX`] names, in the order a directory listing
/// gives them. A category this environment has no value for is left out of the listing by the
/// reader that builds it.
const METADATA_CATEGORIES: &[&str] = &[
    "placement/availability-zone",
    "placement/availability-zone-id",
    "placement/region",
];

/// The media type every value is published under: the metadata service answers text, and the
/// identity document is text that happens to be JSON.
const PLAIN_TEXT: &str = "text/plain";

/// The shortest and longest lifetime a token request may ask for: "the lifetime of the token,
/// in seconds. Maximum is six hours (21,600 seconds)" — so one second is the least a token can
/// be good for and six hours the most.
const MIN_TOKEN_TTL_SECONDS: u64 = 1;
const MAX_TOKEN_TTL_SECONDS: u64 = 21_600;

/// How many bytes one session token is minted out of.
const TOKEN_BYTES: usize = 32;

/// How many session tokens one environment's metadata service holds at once.
///
/// A bound on the store below, which lives as long as the environment and so outlives the run
/// that minted any one token: a token request past this many drops the OLDEST token the
/// environment still holds, exactly as the session store drops the environment that has gone
/// longest without a read. Expired tokens are dropped before that bound is read, so the count
/// only ever stands for tokens that are still good. It is well above the tokens one program
/// mints — a client of this interface asks for one and presents it until it runs out.
const MAX_ISSUED_TOKENS: usize = 16;

/// How many execution environments' metadata sessions this process holds at once: a bound on
/// what the store below holds, past which the environments that have gone longest without a
/// metadata read are dropped. Well above the function versions one client invokes in a steady
/// burst, so such a client's environments keep their sessions throughout.
const MAX_SESSIONS: usize = 64;

/// The statuses the metadata service answers with, as AWS lists them (Amazon EC2 User Guide,
/// "Instance metadata and user data"): a request that states no usable parameter, one that is
/// not authenticated, a path nothing is published at, a method a path does not take, and the
/// service being unable to answer at all.
const MISSING_OR_INVALID_PARAMETERS: u16 = 400;
const UNAUTHORIZED: u16 = 401;
const NOT_FOUND: u16 = 404;
const NOT_ALLOWED: u16 = 405;
const UNAVAILABLE: u16 = 503;

/// The METADATA SESSION state of one execution environment: when it was initialised, the token
/// the environment itself was handed, and the session tokens its metadata service has issued.
///
/// A token is minted by one request and read by the next, so it is state — and it is the
/// ENVIRONMENT's, not one invocation's ([`session_of`]): every invocation served in that
/// environment reads its metadata service through this one session, and a token presented in
/// another environment is no token there.
#[derive(Debug)]
pub struct MetadataSession {
    /// The instant the execution environment was initialised, which is what the identity
    /// document reports as when the node was brought up.
    initialised_at: SystemTime,
    /// The token the EXECUTION ENVIRONMENT itself was handed, which its own environment names
    /// (`AWS_LAMBDA_METADATA_TOKEN`): a handler that was given a token does not have to ask
    /// for one, and a read presenting it is answered for as long as the environment it was
    /// handed to lasts — it is the environment's own credential rather than one asked for for a
    /// span. `None` where this process could not mint one, in which case the environment is
    /// given no token variable at all rather than a predictable value.
    environment_token: Option<String>,
    /// Every session token this environment's metadata service has issued and that is still
    /// good, oldest first. It is bounded on both axes, because the session outlives the run
    /// that minted any one of them: a token whose lifetime has run out is dropped when the next
    /// one is issued or presented, and what is left is held to [`MAX_ISSUED_TOKENS`] with the
    /// oldest dropped to make room.
    issued: Mutex<Vec<IssuedToken>>,
}

/// ONE session token this environment's metadata service issued.
#[derive(Debug)]
struct IssuedToken {
    /// The token itself, as the PUT that asked for it was answered with.
    token: String,
    /// When it stops being valid: the lifetime that request asked for, from the instant it was
    /// issued.
    expires_at: SystemTime,
}

impl Default for MetadataSession {
    fn default() -> Self {
        Self::new()
    }
}

impl MetadataSession {
    /// The metadata session of an execution environment being initialised NOW, which has
    /// issued no token yet and holds the one the environment itself was handed.
    #[must_use]
    pub fn new() -> Self {
        Self {
            initialised_at: SystemTime::now(),
            environment_token: minted_token(),
            issued: Mutex::new(Vec::new()),
        }
    }

    /// The token this execution environment was handed, which its own environment names
    /// ([`MetadataSession::environment_token`]), and `None` where none could be minted.
    #[must_use]
    pub fn environment_token(&self) -> Option<&str> {
        self.environment_token.as_deref()
    }

    /// Issue one session token, good for `ttl` from now.
    ///
    /// The tokens this environment holds are pruned of the ones whose lifetime has run out and
    /// then held to [`MAX_ISSUED_TOKENS`], oldest dropped first, so the store cannot grow with
    /// the invocations the environment serves.
    ///
    /// `None` where this process could not draw one from the platform's own source, in which
    /// case the service issues no token at all rather than a predictable one.
    fn issue(&self, ttl: Duration) -> Option<String> {
        let token = minted_token()?;
        let now = SystemTime::now();
        let mut held = self.issued.lock().unwrap_or_else(PoisonError::into_inner);
        held.retain(|held| held.expires_at > now);
        held.push(IssuedToken {
            token: token.clone(),
            expires_at: now + ttl,
        });
        while held.len() > MAX_ISSUED_TOKENS {
            held.remove(0);
        }
        Some(token)
    }

    /// Whether `presented` is a token this environment may be read with: the one the
    /// environment itself was handed, or one its metadata service issued and which is still
    /// within the lifetime it was issued for.
    ///
    /// A token whose lifetime has run out is dropped here too, so a store nothing has issued
    /// into since is not scanned for tokens that can no longer answer anything.
    fn holds(&self, presented: &str) -> bool {
        if presented.is_empty() {
            return false;
        }
        if self.environment_token.as_deref() == Some(presented) {
            return true;
        }
        let now = SystemTime::now();
        let mut held = self.issued.lock().unwrap_or_else(PoisonError::into_inner);
        held.retain(|held| held.expires_at > now);
        held.iter().any(|held| held.token == presented)
    }
}

/// One session token: random bytes from the platform's own source, base64 as the metadata
/// service publishes a token. Nothing derives it from the environment — a token is a
/// credential of the session, not a fact of the node.
fn minted_token() -> Option<String> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    ::getrandom::fill(&mut bytes).ok()?;
    Some(::aws_smithy_types::base64::encode(bytes))
}

/// The metadata sessions this process holds: the environment each belongs to, when it was last
/// reached, and the session itself — in the order they were last reached in.
type HeldSessions = Vec<(EnvironmentKey, Instant, Arc<MetadataSession>)>;

/// The metadata session of the execution environment `key` names: the SAME session for every
/// invocation served in that environment.
///
/// A session token is the NODE's, not one invocation's: "Lambda maintains the execution
/// environment for some time in anticipation of another invocation", and the global state a
/// handler leaves behind — a token it minted and cached outside the handler among it — is
/// there for the next invocation ([`super::warm_pool`]). So a token stays good for the
/// lifetime it was issued for however many invocations that environment goes on to serve, and
/// the instant the environment was initialised is the instant its FIRST invocation initialised
/// it. A key no invocation has been served under yet opens a session of its own, so no two
/// environments share one.
///
/// A session is RETIRED when its environment has gone as long without a read as the pool keeps
/// an idle environment standing (`super::warm_pool::IDLE_LIFETIME`), so a key whose environment
/// that pool has since retired opens a session of its own rather than inheriting the one the
/// environment before it was initialised with; and what this process holds is bounded by
/// [`MAX_SESSIONS`] whatever happens, the environments that have gone longest without a read
/// dropped to make room.
pub fn session_of(key: &EnvironmentKey) -> Arc<MetadataSession> {
    static SESSIONS: OnceLock<Mutex<HeldSessions>> = OnceLock::new();
    let mut held = SESSIONS
        .get_or_init(|| Mutex::new(Vec::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let now = Instant::now();
    held.retain(|(_key, reached, _session)| {
        now.duration_since(*reached) < super::warm_pool::IDLE_LIFETIME
    });
    // The order is the order they were last reached in, so the entry taken out to make room
    // is the environment that has gone longest without a metadata read.
    if let Some(at) = held.iter().position(|(named, _reached, _session)| named == key) {
        let (key, _reached, session) = held.remove(at);
        held.push((key, now, Arc::clone(&session)));
        return session;
    }
    let session = Arc::new(MetadataSession::new());
    held.push((key.clone(), now, Arc::clone(&session)));
    while held.len() > MAX_SESSIONS {
        held.remove(0);
    }
    session
}

/// What ONE execution environment publishes at the metadata address, read off the facts of the
/// invocation whose environment it is.
#[derive(Debug)]
struct ExecutionEnvironment<'a> {
    /// The function this environment is running, and where it is running it.
    facts: &'a FunctionFacts,
    /// When the environment was initialised.
    initialised_at: SystemTime,
}

impl PublishedInstanceMetadata for ExecutionEnvironment<'_> {
    /// The value this environment publishes at `path`, as the lines it answers with, and
    /// `None` for a path it publishes nothing at.
    fn published(&self, path: &str) -> Option<Vec<String>> {
        let one = |value: String| Some(vec![value]);
        match path {
            METADATA_PREFIX => Some(self.listed(METADATA_CATEGORIES, METADATA_PREFIX)),
            PLACEMENT_PREFIX => Some(self.listed(
                &["availability-zone", "availability-zone-id", "region"],
                PLACEMENT_PREFIX,
            )),
            PLACEMENT_AVAILABILITY_ZONE_PATH => one(self.availability_zone()?.zone_name),
            PLACEMENT_AVAILABILITY_ZONE_ID_PATH => one(self.availability_zone()?.zone_id),
            PLACEMENT_REGION_PATH => one(self.region()?.to_string()),
            IDENTITY_PREFIX => Some(
                [IDENTITY_DOCUMENT_PATH]
                    .into_iter()
                    .filter(|published| self.published(published).is_some())
                    .filter_map(|published| published.rsplit('/').next())
                    .map(str::to_string)
                    .collect(),
            ),
            IDENTITY_DOCUMENT_PATH => one(self.identity_document()?),
            _ => None,
        }
    }
}

impl ExecutionEnvironment<'_> {
    /// The entries of one category's listing: those of `categories`, under `prefix`, that this
    /// environment really publishes a value for.
    fn listed(&self, categories: &[&str], prefix: &str) -> Vec<String> {
        categories
            .iter()
            .filter(|category| self.published(&format!("{prefix}/{category}")).is_some())
            .map(|category| (*category).to_string())
            .collect()
    }

    /// The Region this environment runs in, and `None` for an invocation that was addressed to
    /// none — where nothing about placement is published rather than a Region being made up.
    fn region(&self) -> Option<&str> {
        Some(self.facts.region.as_str()).filter(|region| !region.is_empty())
    }

    /// The Availability Zone this environment is placed in: the FIRST zone its Region
    /// publishes, as the zone RECORD that Region's catalogue holds — its name and its zone id.
    ///
    /// The catalogue is Amazon EC2's own (`…ec2…::state::availability_zones_in_region`, over the
    /// one table this workspace keeps a Region's published zones in,
    /// `::service_common::aws_availability_zones`), which is what that service's
    /// `DescribeAvailabilityZones` answers from \u2014 so the zone a handler reads here is a zone of
    /// its Region that this account really has, under the same name and the same id, and not a
    /// value of the right shape. WHICH of them an environment is placed in is one choice, taken
    /// the same way for every environment of a Region so that two reads of one function's
    /// placement agree.
    ///
    /// `None` for an environment whose Region is not known, and for a Region that catalogue
    /// publishes no zone for.
    fn availability_zone(
        &self,
    ) -> Option<::service_com_amazonaws_ec2_amazonec2::state::AvailabilityZoneRecord> {
        ::service_com_amazonaws_ec2_amazonec2::state::availability_zones_in_region(self.region()?)
            .into_iter()
            .next()
    }

    /// The document the EXECUTION ENVIRONMENT's own interface publishes: the facts of the
    /// environment the reading handler is running in.
    ///
    /// Every member is read off the invocation this environment is serving — the function it is
    /// running, the account that owns it, where it is running and on what, and when the
    /// environment was initialised. A member the environment has no value for is left out
    /// rather than written as a value it does not have.
    ///
    /// The zone is named BOTH ways a zone is named, because the two are different facts about
    /// it — its name in this account and the id of the zone itself — and both come from the
    /// Region's own zone catalogue ([`ExecutionEnvironment::availability_zone`]).
    fn execution_environment_document(&self) -> Value {
        let mut document = ::serde_json::Map::new();
        let zone = self.availability_zone();
        let mut named = |name: &str, value: &str| {
            if !value.is_empty() {
                document.insert(name.to_string(), Value::String(value.to_string()));
            }
        };
        named("AccountID", &self.facts.account_id);
        named("Architecture", &self.facts.architecture);
        named(
            "AvailabilityZone",
            zone.as_ref().map_or("", |zone| zone.zone_name.as_str()),
        );
        named(
            "AvailabilityZoneID",
            zone.as_ref().map_or("", |zone| zone.zone_id.as_str()),
        );
        named("FunctionName", &self.facts.function_name);
        named("FunctionVersion", &self.facts.function_version);
        named("Handler", &self.facts.handler);
        named("InitializedAt", &iso8601_utc(self.initialised_at));
        named("Region", self.region().unwrap_or_default());
        Value::Object(document)
    }

    /// The identity document of this environment: the document the metadata service publishes
    /// for the node, with every member read off the environment's own facts.
    ///
    /// The members are the ones an AWS identity document carries (Amazon EC2 User Guide,
    /// "Instance identity documents"). An execution environment is not an instance — it has no
    /// instance id, image, type or kernel, and no address of its own to publish — so those
    /// members are `null`, which is what AWS writes for a member a node has no value for.
    ///
    /// `None` for an environment whose Region is not known, which has no placement to describe.
    fn identity_document(&self) -> Option<String> {
        let zone = self.availability_zone()?;
        let architecture = match self.facts.architecture.as_str() {
            "" => Value::Null,
            named => Value::String(named.to_string()),
        };
        Some(
            json!({
                "accountId": self.facts.account_id,
                "architecture": architecture,
                "availabilityZone": zone.zone_name,
                "billingProducts": Value::Null,
                "devpayProductCodes": Value::Null,
                "imageId": Value::Null,
                "instanceId": Value::Null,
                "instanceType": Value::Null,
                "kernelId": Value::Null,
                "marketplaceProductCodes": Value::Null,
                "pendingTime": iso8601_utc(self.initialised_at),
                "privateIp": Value::Null,
                "ramdiskId": Value::Null,
                "region": self.region()?,
                "version": IDENTITY_DOCUMENT_VERSION,
            })
            .to_string(),
        )
    }
}

/// What the metadata interfaces of an execution environment answer `request`, and
/// [`Reached::Elsewhere`] for a request addressed anywhere else.
///
/// An execution environment is what the answer is read off, so a request made from none —
/// a run this emulator gave no sandbox — addresses nothing here either.
#[must_use]
pub fn answered(request: &Requested<'_>) -> Reached {
    let Some(addressed) = addressed(request.url) else {
        return Reached::Elsewhere;
    };
    let Some(sandbox) = request.sandbox else {
        return Reached::Elsewhere;
    };
    Reached::Answered(match addressed {
        Addressed::ExecutionEnvironment(path) => environment_answer(request, sandbox, &path),
        Addressed::Node(path) => node_answer(request, sandbox, &path),
    })
}

/// WHICH of the environment's metadata interfaces a URL addresses, and the path it names on
/// it.
#[derive(Debug, PartialEq, Eq)]
enum Addressed {
    /// The execution environment's own interface, at the address its environment names.
    ExecutionEnvironment(String),
    /// The link-local address every node's metadata service answers at.
    Node(String),
}

/// Read `url` as the address of one of those two interfaces, and `None` for a URL that
/// addresses neither.
///
/// Both are read by the `http` crate's own URI type — the maintained parser this workspace
/// already carries for a request line, and the one the node's address is read with
/// ([`::service_common::instance_metadata_service::metadata_path`]) — so nothing here splits a
/// URL by hand and a value that is no URL at all addresses nothing. The environment's own
/// interface is addressed by the host and port its environment names, compared whole, exactly
/// as the runtime API's address is ([`super::runtime_api`]); a trailing separator is trimmed
/// off the path, as that reader trims one.
fn addressed(url: &str) -> Option<Addressed> {
    if let Some(path) = metadata_path(url) {
        return Some(Addressed::Node(path));
    }
    let parsed = ::http::Uri::try_from(url).ok()?;
    if parsed.authority().map(::http::uri::Authority::as_str) != Some(METADATA_API_ADDRESS) {
        return None;
    }
    Some(Addressed::ExecutionEnvironment(
        parsed.path().trim_end_matches('/').to_string(),
    ))
}

/// What the EXECUTION ENVIRONMENT's own interface answers for `path`.
///
/// The read presents the token the environment was handed, and one that presents no valid
/// token is refused `401` whatever path it names: the document describes the environment the
/// caller is in, so a caller that cannot show it is in it is answered nothing about it.
fn environment_answer(request: &Requested<'_>, sandbox: &Sandbox, path: &str) -> Answered {
    if !sandbox.metadata().holds(presented_token(request.headers)) {
        return environment_refusal(UNAUTHORIZED, "");
    }
    if path != EXECUTION_ENVIRONMENT_PATH {
        return environment_refusal(NOT_FOUND, "");
    }
    // A document is READ, so the two methods a read is made with are the ones it takes.
    if !matches!(request.method, "GET" | "HEAD") {
        return environment_refusal(NOT_ALLOWED, "GET, HEAD");
    }
    let environment = ExecutionEnvironment {
        facts: sandbox.facts(),
        initialised_at: sandbox.metadata().initialised_at,
    };
    json_answer(
        request.method,
        &environment.execution_environment_document(),
    )
}

/// What the NODE's metadata service answers for `path`: the wire every node of this emulator
/// publishes, served from the same facts.
fn node_answer(request: &Requested<'_>, sandbox: &Sandbox, path: &str) -> Answered {
    if path == METADATA_TOKEN_PATH {
        return token(request, sandbox);
    }
    // A fact is READ, so the two methods a read is made with are the ones these paths take.
    if !matches!(request.method, "GET" | "HEAD") {
        return refused(NOT_ALLOWED, "GET, HEAD");
    }
    // The token is REQUIRED: a read presenting none, or presenting one this environment did
    // not issue or was not handed, is not authenticated — whatever path it names.
    if !sandbox.metadata().holds(presented_token(request.headers)) {
        return refused(UNAUTHORIZED, "");
    }
    let environment = ExecutionEnvironment {
        facts: sandbox.facts(),
        initialised_at: sandbox.metadata().initialised_at,
    };
    let Some(lines) = environment.published(path) else {
        return refused(NOT_FOUND, "");
    };
    published(request.method, lines.join("\n"))
}

/// The TOKEN a read presents, in either header the metadata service takes one in: the
/// metadata-token header IMDSv2 states, and the `Authorization` header a caller that was
/// HANDED a token presents it in, with or without the bearer scheme.
fn presented_token(headers: &[(String, String)]) -> &str {
    if let Some(presented) = header(headers, METADATA_TOKEN_HEADER) {
        return presented;
    }
    let authorization = header(headers, AUTHORIZATION_HEADER)
        .unwrap_or_default()
        .trim();
    authorization
        .strip_prefix("Bearer ")
        .or_else(|| authorization.strip_prefix("bearer "))
        .unwrap_or(authorization)
        .trim()
}

/// The refusal the EXECUTION ENVIRONMENT's own interface answers with: its status, the methods
/// a path takes for a refusal that is about the method, and the status's REASON as the body
/// — the one line a caller reads off a refused request there, as that interface words one.
fn environment_refusal(status: u16, allowed: &str) -> Answered {
    let mut answer = refused(status, allowed);
    let reason = ::http::StatusCode::from_u16(status)
        .ok()
        .and_then(|status| status.canonical_reason())
        .unwrap_or_default();
    answer.headers.push((
        "Content-Type".to_string(),
        PLAIN_TEXT.to_string(),
    ));
    answer.body = format!("{reason}\n").into_bytes();
    answer
}

/// The answer to a request addressed at the node's TOKEN path: the token a PUT stating a usable
/// lifetime is minted, and the refusal for every other request of it.
fn token(request: &Requested<'_>, sandbox: &Sandbox) -> Answered {
    if request.method != "PUT" {
        return refused(NOT_ALLOWED, "PUT");
    }
    // The lifetime is stated in that header, and a request that states none — or states one
    // outside the lifetimes a token may have — asks for no token this service can issue.
    let Some(ttl) = header(request.headers, METADATA_TOKEN_TTL_HEADER).and_then(token_ttl) else {
        return refused(MISSING_OR_INVALID_PARAMETERS, "");
    };
    let Some(minted) = sandbox.metadata().issue(Duration::from_secs(ttl)) else {
        return refused(UNAVAILABLE, "");
    };
    let mut answer = published(request.method, minted);
    // The service reports the lifetime the token it just issued is good for, which is the one
    // the request asked for.
    answer.headers.push((
        METADATA_TOKEN_TTL_HEADER.to_string(),
        ttl.to_string(),
    ));
    answer
}

/// The lifetime a token request's header asks for, in seconds, or `None` for a value that is
/// not a lifetime a token may have: text that is not a count of seconds, or a count outside
/// [`MIN_TOKEN_TTL_SECONDS`]..=[`MAX_TOKEN_TTL_SECONDS`].
fn token_ttl(stated: &str) -> Option<u64> {
    let asked: u64 = stated.trim().parse().ok()?;
    (MIN_TOKEN_TTL_SECONDS..=MAX_TOKEN_TTL_SECONDS)
        .contains(&asked)
        .then_some(asked)
}

/// An answer carrying `document` as its body, under the media type a JSON document is served
/// as. A `HEAD` is answered with everything about it but its bytes.
fn json_answer(method: &str, document: &Value) -> Answered {
    Answered {
        status: 200,
        headers: vec![(
            "Content-Type".to_string(),
            "application/json".to_string(),
        )],
        body: if method == "HEAD" {
            Vec::new()
        } else {
            ::serde_json::to_vec(document).unwrap_or_default()
        },
    }
}

/// The answer a published value is carried in: the value as the bytes of the lines it is
/// published as, under the media type the metadata service answers text with.
///
/// A `HEAD` is answered with everything about the value but its bytes, which is what that
/// method asks for.
fn published(method: &str, value: String) -> Answered {
    Answered {
        status: 200,
        headers: vec![("Content-Type".to_string(), PLAIN_TEXT.to_string())],
        body: if method == "HEAD" {
            Vec::new()
        } else {
            value.into_bytes()
        },
    }
}

/// The answer a request this service refuses gets: its status, and the methods the path does
/// take for a refusal that is about the method (RFC 9110 §15.5.6 — a `405` states them).
///
/// No body: the body of such a response is the metadata service's own error page, which it
/// publishes none of here, so nothing is answered for a request it refused.
fn refused(status: u16, allowed: &str) -> Answered {
    Answered {
        status,
        headers: if allowed.is_empty() {
            Vec::new()
        } else {
            vec![("Allow".to_string(), allowed.to_string())]
        },
        body: Vec::new(),
    }
}

/// `at` as the RFC 3339 / ISO 8601 UTC instant the identity document's `pendingTime` carries:
/// whole seconds, which is the form that member is published in.
fn iso8601_utc(at: SystemTime) -> String {
    let instant = ::aws_smithy_types::DateTime::from(at);
    ::aws_smithy_types::DateTime::from_secs(instant.secs())
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The facts of an environment under test: an invocation of a function in a Region.
    fn facts(region: &str) -> FunctionFacts {
        FunctionFacts {
            function_name: "probe".to_string(),
            function_version: "$LATEST".to_string(),
            handler: "lambda_function.lambda_handler".to_string(),
            account_id: "000000000000".to_string(),
            region: region.to_string(),
            architecture: "arm64".to_string(),
        }
    }

    /// A request addressed at the metadata service, carrying `headers`.
    fn asking<'a>(
        method: &'a str,
        url: &'a str,
        headers: &'a [(String, String)],
        sandbox: Option<&'a Sandbox>,
    ) -> Requested<'a> {
        Requested {
            method,
            url,
            headers,
            body: &[],
            sandbox,
        }
    }

    /// The status one request was answered with, and `None` for a URL this service says
    /// nothing about.
    fn status(reached: Reached) -> Option<u16> {
        match reached {
            Reached::Answered(answer) => Some(answer.status),
            Reached::NeverAnswered | Reached::Elsewhere => None,
        }
    }

    /// The URL of `path` on the NODE's own metadata address, built from the one spelling of
    /// that address this workspace keeps
    /// ([`::service_common::instance_metadata_service::metadata_address`]) rather than written
    /// out here.
    fn node_url(path: &str) -> String {
        format!(
            "http://{}{path}",
            ::service_common::instance_metadata_service::metadata_address()
        )
    }

    /// The token a PUT of the token path mints in `sandbox`.
    fn minted_in(sandbox: &Sandbox) -> String {
        let headers = vec![(METADATA_TOKEN_TTL_HEADER.to_string(), "21600".to_string())];
        let reached = answered(&asking(
            "PUT",
            &node_url(METADATA_TOKEN_PATH),
            &headers,
            Some(sandbox),
        ));
        match reached {
            Reached::Answered(answer) => {
                assert_eq!(
                    answer.status, 200,
                    "case \"a token request\": expected 200, got {}",
                    answer.status
                );
                String::from_utf8(answer.body).expect("a token is text")
            }
            other => panic!("case \"a token request\": expected an answer, got {other:?}"),
        }
    }

    /// One row of a wire table: the case, the method, the URL, the headers the request carries
    /// and the status it is answered with — `None` for a URL this module says nothing about.
    type Row = (
        &'static str,
        &'static str,
        String,
        Vec<(String, String)>,
        Option<u16>,
    );

    /// A read is answered only with the token the environment itself issued, and the statuses
    /// span what the wire generalises over: the method, the path and the token presented.
    #[test]
    fn a_read_is_answered_only_when_it_presents_an_issued_token() {
        let sandbox = Sandbox::new(facts("eu-central-1"));
        let token = minted_in(&sandbox);
        let presented = |value: &str| vec![(METADATA_TOKEN_HEADER.to_string(), value.to_string())];
        // (case, the method, the URL, the headers it carries, the status it is answered with)
        let cases: &[Row] = &[
            (
                "a read presenting the issued token",
                "GET",
                node_url(PLACEMENT_AVAILABILITY_ZONE_PATH),
                presented(&token),
                Some(200),
            ),
            (
                "the same read presenting nothing",
                "GET",
                node_url(PLACEMENT_AVAILABILITY_ZONE_PATH),
                Vec::new(),
                Some(401),
            ),
            (
                "the same read presenting another token",
                "GET",
                node_url(PLACEMENT_AVAILABILITY_ZONE_PATH),
                presented("not-a-token-this-environment-issued"),
                Some(401),
            ),
            (
                "the identity document",
                "GET",
                node_url(IDENTITY_DOCUMENT_PATH),
                presented(&token),
                Some(200),
            ),
            (
                "a path this environment publishes nothing at",
                "GET",
                node_url("/latest/meta-data/instance-id"),
                presented(&token),
                Some(404),
            ),
            (
                "a method a read path does not take",
                "DELETE",
                node_url(PLACEMENT_REGION_PATH),
                presented(&token),
                Some(405),
            ),
            (
                "a GET of the token path",
                "GET",
                node_url(METADATA_TOKEN_PATH),
                presented(&token),
                Some(405),
            ),
            (
                "a token request stating no lifetime",
                "PUT",
                node_url(METADATA_TOKEN_PATH),
                Vec::new(),
                Some(400),
            ),
            (
                "a token request stating a lifetime no token may have",
                "PUT",
                node_url(METADATA_TOKEN_PATH),
                vec![(METADATA_TOKEN_TTL_HEADER.to_string(), "21601".to_string())],
                Some(400),
            ),
            (
                "another address altogether",
                "GET",
                format!("http://127.0.0.1:9001{PLACEMENT_REGION_PATH}"),
                presented(&token),
                None,
            ),
        ];
        for (case, method, url, headers, expected) in cases {
            let answer = status(answered(&asking(method, url, headers, Some(&sandbox))));
            assert_eq!(
                answer, *expected,
                "case {case:?}: expected {expected:?}, got {answer:?}"
            );
        }
    }

    /// A token one environment issued is no token in another, and a read made from no
    /// environment at all addresses nothing here.
    #[test]
    fn a_token_is_a_credential_of_the_environment_that_issued_it() {
        let first = Sandbox::new(facts("us-west-2"));
        let second = Sandbox::new(facts("us-west-2"));
        let token = minted_in(&first);
        let headers = vec![(METADATA_TOKEN_HEADER.to_string(), token.clone())];
        let url = node_url(PLACEMENT_AVAILABILITY_ZONE_PATH);
        let elsewhere = status(answered(&asking("GET", &url, &headers, Some(&second))));
        assert_eq!(
            elsewhere,
            Some(401),
            "case \"a token presented in another environment\": expected 401, got {elsewhere:?}"
        );
        let unsandboxed = status(answered(&asking("GET", &url, &headers, None)));
        assert_eq!(
            unsandboxed, None,
            "case \"a read from no execution environment\": expected no answer, got \
             {unsandboxed:?}"
        );
    }

    /// Every published value is a fact of the environment: the zone is one its Region really
    /// publishes, the document names that Region and zone, and the listings name the paths that
    /// carry a value.
    #[test]
    fn the_published_values_are_the_environments_own_facts() {
        // The rows differ along the Region, which is what the placement generalises over: one
        // whose zone letters are the usual three, one that publishes more, and one whose second
        // letter names no zone.
        for region in ["eu-central-1", "us-west-2", "ap-northeast-1"] {
            let published = facts(region);
            let environment = ExecutionEnvironment {
                facts: &published,
                initialised_at: SystemTime::now(),
            };
            let zones = ::service_common::aws_availability_zones::availability_zones(region);
            let zone = environment
                .published(PLACEMENT_AVAILABILITY_ZONE_PATH)
                .expect("a Region's environment publishes its zone");
            assert!(
                zones.contains(&zone[0]),
                "case {region:?}: expected a zone of {zones:?}, got {zone:?}"
            );
            let document = environment
                .published(IDENTITY_DOCUMENT_PATH)
                .expect("the identity document is published");
            let document: Value = ::serde_json::from_str(&document[0])
                .expect("the identity document must be JSON");
            assert_eq!(
                document.get("region").and_then(Value::as_str),
                Some(region),
                "case {region:?}: expected the document to name the Region, got {document}"
            );
            assert_eq!(
                document.get("availabilityZone").and_then(Value::as_str),
                Some(zone[0].as_str()),
                "case {region:?}: expected the document to name the same zone, got {document}"
            );
            assert_eq!(
                document.get("architecture").and_then(Value::as_str),
                Some(published.architecture.as_str()),
                "case {region:?}: expected the architecture the function runs on, got {document}"
            );
            assert_eq!(
                document.get("instanceId"),
                Some(&Value::Null),
                "case {region:?}: an execution environment has no instance id, got {document}"
            );
            assert_eq!(
                environment.published(METADATA_PREFIX),
                Some(
                    METADATA_CATEGORIES
                        .iter()
                        .map(|category| (*category).to_string())
                        .collect()
                ),
                "case {region:?}: the listing names the categories that carry a value"
            );
        }
        // An invocation addressed to no Region has no placement to publish, and publishes
        // none rather than a name of the right shape.
        let published = facts("");
        let environment = ExecutionEnvironment {
            facts: &published,
            initialised_at: SystemTime::now(),
        };
        assert_eq!(
            environment.published(PLACEMENT_AVAILABILITY_ZONE_PATH),
            None,
            "case \"no Region\": expected no zone, got one"
        );
        assert_eq!(
            environment.published(METADATA_PREFIX),
            Some(Vec::new()),
            "case \"no Region\": expected an empty listing, got entries"
        );
    }

    /// What one environment holds is BOUNDED however many tokens are asked for in it: the
    /// tokens past the bound are dropped oldest first, and a token whose lifetime has run out
    /// answers nothing.
    #[test]
    fn an_environments_token_store_is_bounded() {
        let sandbox = Sandbox::new(facts("us-east-1"));
        // Twice the bound, so the oldest half is dropped while the newest half still answers.
        let minted: Vec<String> = (0..MAX_ISSUED_TOKENS * 2)
            .map(|_| minted_in(&sandbox))
            .collect();
        let held = sandbox
            .metadata()
            .issued
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        assert!(
            held <= MAX_ISSUED_TOKENS,
            "case \"a token store asked for twice its bound\": expected at most \
             {MAX_ISSUED_TOKENS} held, got {held}"
        );
        let newest = minted.last().expect("a token was minted");
        assert!(
            sandbox.metadata().holds(newest),
            "case \"the newest token\": expected it to still answer, got a refusal"
        );
        let oldest = minted.first().expect("a token was minted");
        assert!(
            !sandbox.metadata().holds(oldest),
            "case \"a token past the bound\": expected it to be dropped, got an answer"
        );
        // A token whose own lifetime has run out answers nothing either: the store holds it
        // with the instant it stops being valid, so one issued for the shortest lifetime stops
        // answering at that instant without anything having to ask again.
        let expired = sandbox
            .metadata()
            .issue(Duration::ZERO)
            .expect("a token is minted");
        assert!(
            !sandbox.metadata().holds(&expired),
            "case \"a token whose lifetime has run out\": expected no answer, got one"
        );
    }

    /// A token is good for the lifetime its request asked for, and only lifetimes a token may
    /// have are accepted: the bounds the service publishes, and one past each of them.
    #[test]
    fn a_token_request_states_a_lifetime_within_the_published_bounds() {
        // (case, the header value, the lifetime it asks for)
        let cases: &[(&str, &str, Option<u64>)] = &[
            ("the shortest lifetime", "1", Some(1)),
            ("the longest lifetime", "21600", Some(21_600)),
            ("a lifetime inside them", "300", Some(300)),
            ("one below the shortest", "0", None),
            ("one past the longest", "21601", None),
            ("a value that is no count of seconds", "forever", None),
            ("a negative count", "-1", None),
            ("nothing at all", "", None),
        ];
        for (case, stated, expected) in cases {
            let asked = token_ttl(stated);
            assert_eq!(
                asked, *expected,
                "case {case:?}: expected {expected:?}, got {asked:?}"
            );
        }
    }

    /// One row of the environment interface's table: the case, the method, the URL, the headers
    /// the request carries, and the status and body it is answered with — `None` for a URL this
    /// module says nothing about.
    type AnsweredRow = (
        &'static str,
        &'static str,
        String,
        Vec<(String, String)>,
        Option<(u16, &'static str)>,
    );

    /// The EXECUTION ENVIRONMENT's own interface answers its document to a read that presents
    /// the token the environment was handed, in either form a caller presents one, and refuses
    /// every other read with that status's own reason as the body.
    #[test]
    fn the_environment_document_is_answered_to_a_read_that_presents_its_token() {
        let sandbox = Sandbox::new(facts("us-east-1"));
        let token = sandbox
            .metadata()
            .environment_token()
            .expect("an environment is handed a token")
            .to_string();
        let served = format!("http://{METADATA_API_ADDRESS}{EXECUTION_ENVIRONMENT_PATH}");
        let authorization =
            |value: String| vec![(AUTHORIZATION_HEADER.to_string(), value)];
        // The rows differ along what the interface generalises over: the token presented and
        // the form it is presented in, the path, the method, and the address.
        // (case, the method, the URL, the headers, the status and the body it answers)
        let cases: &[AnsweredRow] = &[
            (
                "a read presenting the token it was handed",
                "GET",
                served.clone(),
                authorization(token.clone()),
                Some((200, "")),
            ),
            (
                "the same read presenting it under the bearer scheme",
                "GET",
                served.clone(),
                authorization(format!("Bearer {token}")),
                Some((200, "")),
            ),
            (
                "the same read presenting it in the metadata-token header",
                "GET",
                served.clone(),
                vec![(METADATA_TOKEN_HEADER.to_string(), token.clone())],
                Some((200, "")),
            ),
            (
                "the same read presenting nothing",
                "GET",
                served.clone(),
                Vec::new(),
                Some((401, "Unauthorized\n")),
            ),
            (
                "the same read presenting a token nothing handed out",
                "GET",
                served.clone(),
                authorization("not-a-token-of-this-environment".to_string()),
                Some((401, "Unauthorized\n")),
            ),
            (
                "another path of the interface",
                "GET",
                format!("http://{METADATA_API_ADDRESS}/2026-01-15/metadata/elsewhere"),
                authorization(token.clone()),
                Some((404, "Not Found\n")),
            ),
            (
                "a method the document's path does not take",
                "POST",
                served.clone(),
                authorization(token.clone()),
                Some((405, "Method Not Allowed\n")),
            ),
            (
                "the same path on another address",
                "GET",
                format!("http://127.0.0.1:9001{EXECUTION_ENVIRONMENT_PATH}"),
                authorization(token.clone()),
                None,
            ),
        ];
        for (case, method, url, headers, expected) in cases {
            let answer = match answered(&asking(method, url, headers, Some(&sandbox))) {
                Reached::Answered(answer) => Some((
                    answer.status,
                    String::from_utf8_lossy(&answer.body).to_string(),
                )),
                Reached::NeverAnswered | Reached::Elsewhere => None,
            };
            match (expected, &answer) {
                (Some((status, body)), Some((answered_status, answered_body))) => {
                    assert_eq!(
                        answered_status, status,
                        "case {case:?}: expected {status}, got {answered_status}"
                    );
                    if !body.is_empty() {
                        assert_eq!(
                            answered_body, body,
                            "case {case:?}: expected the body {body:?}, got {answered_body:?}"
                        );
                    }
                }
                (None, None) => {}
                _ => panic!("case {case:?}: expected {expected:?}, got {answer:?}"),
            }
        }
        // The document itself is the environment's own facts, with the zone named both ways
        // the Region's own catalogue names it.
        let Reached::Answered(answer) = answered(&asking(
            "GET",
            &served,
            &authorization(token),
            Some(&sandbox),
        )) else {
            panic!("case \"the document\": expected an answer, got none");
        };
        let document: Value =
            ::serde_json::from_slice(&answer.body).expect("the document must be JSON");
        let zone =
            ::service_com_amazonaws_ec2_amazonec2::state::availability_zones_in_region("us-east-1")
                .into_iter()
                .next()
                .expect("the Region publishes zones");
        // (the member, the value the environment's own facts give it)
        let members: &[(&str, &str)] = &[
            ("AccountID", "000000000000"),
            ("Architecture", "arm64"),
            ("AvailabilityZone", zone.zone_name.as_str()),
            ("AvailabilityZoneID", zone.zone_id.as_str()),
            ("FunctionName", "probe"),
            ("FunctionVersion", "$LATEST"),
            ("Handler", "lambda_function.lambda_handler"),
            ("Region", "us-east-1"),
        ];
        for (member, expected) in members {
            let held = document.get(member).and_then(Value::as_str);
            assert_eq!(
                held,
                Some(*expected),
                "case {member:?}: expected {expected:?}, got {held:?} in {document}"
            );
        }
        assert!(
            document
                .get("InitializedAt")
                .and_then(Value::as_str)
                .is_some_and(|at| at.ends_with('Z')),
            "case \"InitializedAt\": expected a UTC instant, got {document}"
        );
    }
}
