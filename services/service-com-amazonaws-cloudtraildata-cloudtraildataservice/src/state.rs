// SPDX-License-Identifier: CC0-1.0

//! Engine state for the com-amazonaws-cloudtraildata-cloudtraildataservice service.
//!
//! CloudTrail Lake channels are created by the *CloudTrail* control plane
//! (`CreateChannel`), which is a different service crate, so the registry lives here as
//! shared, seedable state keyed by channel ARN: the call that mints a channel registers
//! it here in the same request through [`register_channel`], the call that deletes one
//! takes it back out through [`deregister_channel`], and `PutAuditEvents` ingests into
//! it. A test that needs a channel without driving the control plane registers one the
//! same way, or writes
//! [`ComAmazonawsCloudtraildataCloudtraildataserviceState::register_channel`] directly.
//!
//! Ingestion is this service's own: every event `PutAuditEvents` decides the outcome of
//! is recorded against its channel here, so the control plane that reports the channel
//! reads those facts back through [`channel_ingestion`] instead of keeping a copy.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// How many ingested events one channel retains.
///
/// The ingest log is the emulator's own record of what it accepted, and what a reader of
/// the channel reports of it is the MOST RECENT accepted event — which is the newest
/// entry, never an evicted one — so retaining a bounded window of it changes no response
/// this emulator renders, while an unbounded one would let a caller grow the emulator's
/// memory by repeating `PutAuditEvents`. Oldest entries are evicted first.
pub const MAX_RETAINED_EVENTS_PER_CHANNEL: usize = 1_000;

/// How many `eventData` bytes one channel's ingest log retains, for the same reason as
/// [`MAX_RETAINED_EVENTS_PER_CHANNEL`]: an event may carry up to 1 MB, so a count alone
/// would not bound the bytes held.
pub const MAX_RETAINED_BYTES_PER_CHANNEL: usize = 16 * 1024 * 1024;

/// The largest `eventData` payload a single event may carry.
///
/// From `PutAuditEvents`' own doc: "You can add up to 100 of these events (or up to
/// 1 MB) per PutAuditEvents request" — so an event whose `eventData` alone already
/// exceeds 1 MB can never be ingested, whatever else the request holds, and is reported
/// for that event as `FieldTooLong`. (Charging a whole BATCH against the same 1 MB
/// budget is a separate gap, deferred by the bootstrap issue.) It lives here, beside the
/// retention bounds, so the handler and this crate's tests read one definition.
pub const MAX_EVENT_DATA_BYTES: usize = 1024 * 1024;

/// The IAM condition key a channel's resource policy states an external ID under.
///
/// `PutAuditEvents$externalId` is "conditionally required when the channel's resource policy
/// includes an external ID", and `sts:ExternalId` is the condition key AWS documents that
/// policy including it as — so a request's `externalId` is the value this key carries when
/// the channel's policy is evaluated ([`Channel::accepts_external_id`]).
pub const EXTERNAL_ID_CONDITION_KEY: &str = "sts:ExternalId";

/// The operation a channel's resource policy authorizes, whose `externalId` that policy's
/// condition narrows. The ACTION is spelled by this service's own namespace statement
/// ([`crate::service::authorization::action_for`]), so the policy is evaluated against the
/// same action a grant of this operation is written with.
const INGEST_OPERATION: &str = "PutAuditEvents";

/// The parts of a CloudTrail Lake channel ARN:
/// `arn:<partition>:cloudtrail:<region>:<account-id>:channel/<channel-id>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelArn {
    /// The ARN exactly as it was parsed.
    pub arn: String,
    /// The partition field (`aws`, `aws-cn`, ...).
    pub partition: String,
    /// The region field.
    pub region: String,
    /// The 12-digit account that owns the channel.
    pub account_id: String,
    /// The ARN suffix: the channel's own id.
    pub channel_id: String,
}

/// How a caller names a channel: `PutAuditEvents$channelArn` is documented as "The ARN
/// or ID (the ARN suffix) of a channel", so both forms resolve to the same channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChannelRef {
    /// A full channel ARN.
    Arn(ChannelArn),
    /// A bare channel id (the ARN suffix).
    Id(String),
}

/// The ARN vendor (service namespace) field a channel ARN carries: a CloudTrail Lake
/// channel is a resource of the CloudTrail control plane, which is what mints it, so an
/// ARN of any other vendor names no channel.
const CHANNEL_ARN_VENDOR: &str = "cloudtrail";

/// The resource prefix of a channel ARN's resource field:
/// `arn:aws:cloudtrail:<region>:<account>:channel/<uuid>` — the form the CloudTrail
/// control plane mints a channel ARN in (`CreateChannel`), and the only resource field
/// that names a channel.
const CHANNEL_RESOURCE_PREFIX: &str = "channel/";

/// How many digits an AWS account id is written with.
const ACCOUNT_ID_DIGITS: usize = 12;

/// The ARN partition this emulator serves, read off an ARN `::engine::emulator_arn`
/// builds rather than written here as a literal, so the partition a channel ARN is held
/// to has one source of truth. A reference built in another partition names a resource
/// no call to this emulator can reach.
pub fn emulator_partition() -> String {
    let minted = ::engine::emulator_arn(CHANNEL_ARN_VENDOR, "", CHANNEL_RESOURCE_PREFIX);
    ::service_common::arn::parse(&minted)
        .map(|arn| arn.partition.to_string())
        .unwrap_or_default()
}

/// True when `value` is written the way CloudTrail writes a channel id: the canonical
/// hyphenated UUID the control plane mints one in (`CreateChannel`, whose `UUID` is
/// `@length 36` of `^[a-f0-9\-]+$` in 8-4-4-4-12 groups) — which is also what this
/// emulator's CloudTrail crate mints.
///
/// Read through the maintained `uuid` crate and compared against its canonical
/// rendering, so only that one spelling passes: a simple, braced or URN spelling of the
/// same UUID is not an id the service ever wrote, and neither is any other string,
/// whatever bytes or encoding it carries.
fn is_channel_id(value: &str) -> bool {
    ::uuid::Uuid::parse_str(value).is_ok_and(|parsed| parsed.hyphenated().to_string() == value)
}

/// Parse a CloudTrail Lake channel ARN, or `None` when `value` is not one.
///
/// A channel ARN is `arn:aws:cloudtrail:<region>:<account-id>:channel/<channel-id>`. The
/// six fields and the vendor are read through `::service_common::arn`, the ONE ARN parse
/// the services share, and this reader adds what that parse leaves to the service: the
/// partition this emulator serves ([`emulator_partition`]), the Region label and the
/// 12-digit account a regional account-owned resource is addressed by, and a `channel`
/// resource whose suffix is a channel id ([`is_channel_id`]). Every other `arn:` value —
/// another partition, another service, another CloudTrail resource type, a missing field,
/// a resource suffix that is not a channel id — is not a channel ARN.
pub fn parse_channel_arn(value: &str) -> Option<ChannelArn> {
    let arn = ::service_common::arn::parse_of(value, CHANNEL_ARN_VENDOR)?;
    if arn.partition != emulator_partition() {
        return None;
    }
    let region = ::service_common::arn::region_of(value, CHANNEL_ARN_VENDOR)?;
    if arn.account.len() != ACCOUNT_ID_DIGITS || !arn.account.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let channel_id = arn.resource.strip_prefix(CHANNEL_RESOURCE_PREFIX)?;
    if !is_channel_id(channel_id) {
        return None;
    }
    Some(ChannelArn {
        arn: value.to_string(),
        partition: arn.partition.to_string(),
        region: region.to_string(),
        account_id: arn.account.to_string(),
        channel_id: channel_id.to_string(),
    })
}

/// Resolve the `channelArn` input value to a [`ChannelRef`], or `None` when it names no
/// channel at all (the caller answers that with `InvalidChannelARN`).
///
/// An `arn:`-prefixed value must be a channel ARN; anything else is read as the ARN
/// suffix (the bare channel id) the operation's doc also accepts. Note that a request
/// cannot reach the bare-id branch: `PutAuditEvents$channelArn` carries
/// `@pattern ^arn:.*$`, which the boundary enforces before any handler runs, so that
/// branch serves this crate's seeding/state API and a future control-plane caller that
/// holds only the channel id.
pub fn parse_channel_ref(value: &str) -> Option<ChannelRef> {
    if value.starts_with("arn:") {
        parse_channel_arn(value).map(ChannelRef::Arn)
    } else if is_channel_id(value) {
        Some(ChannelRef::Id(value.to_string()))
    } else {
        None
    }
}

/// How many event ids this process has minted, so two ids never collide even if two
/// requests draw the same random bytes in the same nanosecond.
static EVENT_IDS_MINTED: AtomicU64 = AtomicU64::new(0);

/// A fresh `AuditEventResultEntry$eventID`: the id CloudTrail assigns to an event it
/// ingested, in the lowercase hyphenated version-4 UUID form the service returns (and
/// within the member's modelled `^[-_A-Za-z0-9]+$`, `@length 1..128`).
///
/// The 16 bytes are random and carry nothing from the request: `sha2` over the std
/// hasher's per-instance keys (`RandomState` is seeded by the OS), the monotonic clock
/// and this process's mint counter, with the version and variant nibbles then set as
/// RFC 4122 section 4.4 specifies. Only those two nibbles and the hyphenation are
/// written here; the randomness, the digest and the hex-coding come from crates this
/// crate already carries. Deriving an id from the event's own content would make two
/// identical events share one id, which the response's `successful` pairs must not.
pub fn new_event_id() -> String {
    use sha2::Digest as _;
    use std::hash::BuildHasher as _;

    let entropy = std::collections::hash_map::RandomState::new();
    let minted = EVENT_IDS_MINTED.fetch_add(1, Ordering::Relaxed);
    let mut hasher = sha2::Sha256::new();
    hasher.update(entropy.hash_one(minted).to_be_bytes());
    hasher.update(entropy.hash_one(u64::MAX - minted).to_be_bytes());
    hasher.update(minted.to_be_bytes());
    hasher.update(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|since| since.as_nanos())
            .unwrap_or_default()
            .to_be_bytes(),
    );

    let digest = hasher.finalize();
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // Version 4 in the high nibble of octet 6 and variant 0b10 in the two high bits of
    // octet 8: what makes the value a well-formed v4 UUID rather than 16 loose bytes.
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;

    let hex = ::hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// One ingestion ATTEMPT this service decided the outcome of: one event of one
/// `PutAuditEvents` batch, accepted or refused.
///
/// A channel keeps the most recent attempt and the most recent REFUSED attempt
/// ([`Channel::latest_attempt`], [`Channel::latest_refusal`]), which is what the
/// CloudTrail control plane's channel ingestion table is a view of: nothing reports an
/// older attempt than those, so nothing older is retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngestAttempt {
    /// `AuditEvent$id`: the event id the caller supplied for this attempt.
    pub source_id: String,
    /// `AuditEventResultEntry$eventID`: the id CloudTrail assigned, for an attempt it
    /// ACCEPTED. `None` for a refused one — the service assigns an event id only to an
    /// event it ingested, which is why the response's `failed` entries carry none.
    pub event_id: Option<String>,
    /// `ResultErrorEntry$errorCode`: why the attempt was refused, `None` when it was
    /// accepted.
    pub error_code: Option<String>,
    /// When this emulator decided the attempt.
    pub attempted_at: SystemTime,
}

/// One audit event this emulator ingested, in the order it was accepted.
#[derive(Debug, Clone)]
pub struct IngestedEvent {
    /// `AuditEvent$id`: the original event id supplied by the caller.
    pub source_id: String,
    /// `AuditEventResultEntry$eventID`: the id CloudTrail assigned on ingest.
    pub event_id: String,
    /// The raw `eventData` JSON payload as submitted.
    pub event_data: String,
    /// `AuditEvent$eventDataChecksum`, when the caller supplied one.
    pub event_data_checksum: Option<String>,
    /// When this emulator accepted the event.
    pub ingested_at: SystemTime,
}

/// A registered CloudTrail Lake channel and its append-only ingest log.
#[derive(Debug, Clone)]
pub struct Channel {
    /// `arn:<partition>:cloudtrail:<region>:<account-id>:channel/<channel-id>`.
    pub arn: String,
    /// The ARN suffix, accepted as an alias for the ARN on input.
    pub channel_id: String,
    /// The account that owns the channel; a caller from another account is refused
    /// with `ChannelInsufficientPermission`.
    pub owner_account_id: String,
    /// The resource-based policy attached to the channel, as the CloudTrail control plane
    /// stores it — `None` for a channel carrying none.
    ///
    /// The document is held rather than anything read out of it, so what it says about a
    /// request is DECIDED by the shared IAM evaluator rather than summarised here
    /// ([`Channel::accepts_external_id`]). Written by [`set_channel_resource_policy`], the
    /// seam the call that attaches, replaces or detaches that policy drives.
    ///
    /// Bounded by the control plane that writes it: a `ResourcePolicy` is modelled
    /// `@length 1..8192` and one channel carries one policy, so this registry holds at most
    /// that per channel — and the evaluator reads nothing past its own
    /// `iam_authorization::MAX_POLICY_BYTES` backstop.
    pub resource_policy: Option<String>,
    /// The channel's event source (`Custom`, a partner source name, ...).
    pub source: String,
    /// Events accepted on this channel, oldest first, bounded by
    /// [`MAX_RETAINED_EVENTS_PER_CHANNEL`] / [`MAX_RETAINED_BYTES_PER_CHANNEL`].
    /// Private so the bounds and `retained_bytes` cannot drift apart: append through
    /// [`Channel::record_ingest`] and read through [`Channel::events`].
    ingested: VecDeque<IngestedEvent>,
    /// The `eventData` bytes currently retained in `ingested`.
    retained_bytes: usize,
    /// The most recent ingestion attempt on this channel, whatever its outcome. Private
    /// so it cannot drift from the log: written by [`Channel::record_ingest`] and
    /// [`Channel::record_refusal`], the two calls that decide an attempt, and read
    /// through [`Channel::latest_attempt`].
    latest_attempt: Option<IngestAttempt>,
    /// The most recent attempt this channel REFUSED, which is a different fact from the
    /// most recent attempt: a refusal stays reportable after later events are accepted.
    latest_refusal: Option<IngestAttempt>,
}

impl Channel {
    /// A channel with no ingested events, built from the ARN the control plane minted
    /// for it. `None` when `arn` is not a channel ARN; the owner account and the
    /// channel id are read off the ARN rather than passed separately, so they can never
    /// disagree with it.
    pub fn from_arn(arn: &str, source: impl Into<String>) -> Option<Self> {
        let parsed = parse_channel_arn(arn)?;
        Some(Self {
            arn: parsed.arn,
            channel_id: parsed.channel_id,
            owner_account_id: parsed.account_id,
            // A channel is minted with no resource policy attached, so a fresh
            // registration requires no external id; the policy write is what carries
            // one ([`set_channel_resource_policy`]).
            resource_policy: None,
            source: source.into(),
            ingested: VecDeque::new(),
            retained_bytes: 0,
            // A channel nothing has ingested onto yet has attempted nothing, which is
            // what a reader of its ingestion facts reports nothing for.
            latest_attempt: None,
            latest_refusal: None,
        })
    }

    /// The same channel, carrying the resource policy AWS documents for a custom
    /// integration, conditional on `external_id`: the grant of the ingest this channel's
    /// `externalId` is then held to.
    #[must_use]
    pub fn requiring_external_id(mut self, external_id: impl Into<String>) -> Self {
        let document = ::serde_json::json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "ChannelPolicy",
                "Effect": "Allow",
                "Principal": "*",
                "Action": crate::service::authorization::action_for(INGEST_OPERATION),
                "Resource": self.arn.clone(),
                "Condition": {
                    "StringEquals": { EXTERNAL_ID_CONDITION_KEY: external_id.into() },
                },
            }],
        });
        self.resource_policy = Some(document.to_string());
        self
    }

    /// Whether the `externalId` a request carried (or did not carry) satisfies this
    /// channel's resource policy.
    ///
    /// `externalId` is "conditionally required when the channel's resource policy includes an
    /// external ID", so the value the request carries is the value the policy's condition
    /// over [`EXTERNAL_ID_CONDITION_KEY`] is DECIDED against — by the shared IAM evaluator
    /// ([`::service_common::iam_authorization::key_value_satisfies_grants`]), which is what
    /// makes every spelling of that condition decide the way this tree decides it: an
    /// equality (case-sensitive) admits its own values, a pattern admits what it matches, an
    /// `…IfExists` clause admits a request carrying nothing, and so on. A channel whose
    /// policy's grants of the ingest condition on the key not at all — including the channel
    /// carrying no policy — accepts whatever the request supplies, including nothing.
    pub fn accepts_external_id(&self, supplied: Option<&str>) -> bool {
        let Some(document) = self.resource_policy.as_deref() else {
            return true;
        };
        ::service_common::iam_authorization::key_value_satisfies_grants(
            document,
            &crate::service::authorization::action_for(INGEST_OPERATION),
            EXTERNAL_ID_CONDITION_KEY,
            supplied,
        )
    }

    /// Append an accepted event to the ingest log, evicting the oldest entries until
    /// the log is back inside both retention bounds. The event just accepted is never
    /// evicted, so the log always holds what the response acknowledged — and it is this
    /// channel's most recent ingestion ATTEMPT too, since accepting it decided one.
    pub fn record_ingest(&mut self, event: IngestedEvent) {
        self.latest_attempt = Some(IngestAttempt {
            source_id: event.source_id.clone(),
            event_id: Some(event.event_id.clone()),
            error_code: None,
            attempted_at: event.ingested_at,
        });
        self.retained_bytes = self.retained_bytes.saturating_add(event.event_data.len());
        self.ingested.push_back(event);
        while self.ingested.len() > 1
            && (self.ingested.len() > MAX_RETAINED_EVENTS_PER_CHANNEL
                || self.retained_bytes > MAX_RETAINED_BYTES_PER_CHANNEL)
        {
            if let Some(evicted) = self.ingested.pop_front() {
                self.retained_bytes = self
                    .retained_bytes
                    .saturating_sub(evicted.event_data.len());
            }
        }
    }

    /// Record an attempt this channel REFUSED, under the `ResultErrorEntry$errorCode`
    /// the response reports for it.
    ///
    /// A refused event is not ingested, so nothing of it joins the ingest log and no
    /// event id is assigned to it; what the refusal leaves behind is the attempt itself,
    /// which is what the channel's ingestion facts report as its most recent failure.
    pub fn record_refusal(
        &mut self,
        source_id: impl Into<String>,
        error_code: impl Into<String>,
        attempted_at: SystemTime,
    ) {
        let attempt = IngestAttempt {
            source_id: source_id.into(),
            event_id: None,
            error_code: Some(error_code.into()),
            attempted_at,
        };
        self.latest_refusal = Some(attempt.clone());
        self.latest_attempt = Some(attempt);
    }

    /// The retained ingest log, oldest first.
    pub fn events(&self) -> impl ExactSizeIterator<Item = &IngestedEvent> {
        self.ingested.iter()
    }

    /// The most recent event this channel ACCEPTED, or `None` when it has accepted none.
    ///
    /// It is the newest entry of the ingest log, which retention never evicts
    /// ([`Channel::record_ingest`] evicts from the oldest end and keeps the event it just
    /// accepted), so the most recent accepted event stays readable however much the log
    /// has shed.
    pub fn latest_ingest(&self) -> Option<&IngestedEvent> {
        self.ingested.back()
    }

    /// The most recent ingestion attempt on this channel, accepted or refused.
    pub fn latest_attempt(&self) -> Option<&IngestAttempt> {
        self.latest_attempt.as_ref()
    }

    /// The most recent attempt this channel refused.
    pub fn latest_refusal(&self) -> Option<&IngestAttempt> {
        self.latest_refusal.as_ref()
    }

    /// How many events the ingest log currently retains.
    pub fn retained_event_count(&self) -> usize {
        self.ingested.len()
    }

    /// How many `eventData` bytes the ingest log currently retains.
    pub fn retained_bytes(&self) -> usize {
        self.retained_bytes
    }

    /// The event id this channel assigned to the first retained event carrying
    /// `source_id`, if any.
    pub fn event_id_for(&self, source_id: &str) -> Option<&str> {
        self.ingested
            .iter()
            .find(|e| e.source_id == source_id)
            .map(|e| e.event_id.as_str())
    }
}

/// Per-service state for com-amazonaws-cloudtraildata-cloudtraildataservice. A handler reaches it through [`service_state`].
#[derive(Debug, Default)]
pub struct ComAmazonawsCloudtraildataCloudtraildataserviceState {
    /// Registered channels, keyed by channel ARN.
    channels: HashMap<String, Channel>,
}

impl ComAmazonawsCloudtraildataCloudtraildataserviceState {
    /// Register (or replace) a channel, keyed by its ARN. Returns the registration it
    /// displaced, if any.
    pub fn register_channel(&mut self, channel: Channel) -> Option<Channel> {
        self.channels.insert(channel.arn.clone(), channel)
    }

    /// The ARN of the one registered channel whose ARN suffix is `id`, or `None` when
    /// no channel carries that suffix or when SEVERAL do: the same suffix under two
    /// accounts or regions is ambiguous, and resolving it by `HashMap` iteration order
    /// would pick a different channel from run to run.
    fn arn_for_channel_id(&self, id: &str) -> Option<&str> {
        let mut matching = self
            .channels
            .values()
            .filter(|c| c.channel_id == *id)
            .map(|c| c.arn.as_str());
        let only = matching.next()?;
        matching.next().is_none().then_some(only)
    }

    /// The channel `reference` names: an exact ARN match, or the one channel whose ARN
    /// suffix is the given id.
    pub fn channel(&self, reference: &ChannelRef) -> Option<&Channel> {
        let key = match reference {
            ChannelRef::Arn(arn) => arn.arn.as_str(),
            ChannelRef::Id(id) => self.arn_for_channel_id(id)?,
        };
        self.channels.get(key)
    }

    /// [`Self::channel`], for a handler that appends to the channel's ingest log.
    pub fn channel_mut(&mut self, reference: &ChannelRef) -> Option<&mut Channel> {
        let key = match reference {
            ChannelRef::Arn(arn) => arn.arn.clone(),
            ChannelRef::Id(id) => self.arn_for_channel_id(id)?.to_string(),
        };
        self.channels.get_mut(&key)
    }

    /// Take the channel `reference` names back out of the registry, returning the
    /// registration it held, if any — the effect retiring the channel has on this
    /// registry, and the companion of [`Self::register_channel`].
    pub fn remove_channel(&mut self, reference: &ChannelRef) -> Option<Channel> {
        let key = match reference {
            ChannelRef::Arn(arn) => arn.arn.clone(),
            ChannelRef::Id(id) => self.arn_for_channel_id(id)?.to_string(),
        };
        self.channels.remove(&key)
    }
}

/// Give this service's channel registry the channel the CloudTrail control plane just
/// created, reporting whether the registry holds that channel once this returns.
///
/// The CHANNEL RESOURCE is the control plane's: `cloudtrail:CreateChannel` is the only
/// call that mints a channel ARN and `cloudtrail:DeleteChannel` the only one that retires
/// it, so it is those calls — in the SAME request that mints or retires the channel —
/// which make the channel present here, through this function and
/// [`deregister_channel`]. A caller that just created a channel can therefore ingest onto
/// it with no propagation step to wait for, and one whose channel was deleted cannot. The
/// registration carries what this service's own checks need beyond the ARN: the owner
/// account (read off the ARN, so it can never disagree with it) and the channel's event
/// `source`.
///
/// Idempotent: an ARN this registry already holds keeps its registration AND its ingest
/// log, so registering twice never drops events this service accepted.
///
/// What the channel's `externalId` requirement is does NOT come from here: it is stated
/// by the channel's RESOURCE POLICY, which no create carries (`cloudtrail:CreateChannel`
/// has no policy member), so the call that attaches, replaces or detaches that policy is
/// what carries it, through [`set_channel_external_id_requirement`]. A channel this
/// function has just registered therefore requires none, which is what a channel with no
/// policy attached requires.
///
/// The registry's SIZE is bounded by the control plane that writes it: it mints at most
/// its own channel quota of channels, each registers once here, and
/// [`deregister_channel`] releases the registration when the channel is retired — so no
/// sequence of calls grows this map past that quota. What each channel then RETAINS is
/// bounded by [`MAX_RETAINED_EVENTS_PER_CHANNEL`] / [`MAX_RETAINED_BYTES_PER_CHANNEL`].
///
/// `false` when `arn` is not a channel ARN, or when this store cannot be reached at all
/// (the engine is not the shared in-memory one) — never a silent success: a caller that
/// could not register the channel answers with its own modelled error rather than
/// reporting a channel the data plane cannot ingest onto.
///
/// LOCK ORDER: this takes THIS store's lock, holds no other while it does, and hands back
/// an owned value rather than a guard — so a caller asks it with NO guard of its own store
/// held, which keeps the cross-service write one-directional and a lock cycle between the
/// two stores unreachable.
pub fn register_channel<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: &str,
    source: &str,
) -> bool {
    let Some(parsed) = parse_channel_arn(arn) else {
        return false;
    };
    let Some(state) = service_state(engine) else {
        return false;
    };
    let reference = ChannelRef::Arn(parsed);
    let mut store = state.lock();
    if store.channel(&reference).is_some() {
        return true;
    }
    let Some(channel) = Channel::from_arn(arn, source) else {
        return false;
    };
    store.register_channel(channel);
    true
}

/// Give this service's registration of the channel `arn` the RESOURCE POLICY the control
/// plane attached to it, so `PutAuditEvents` holds a request's `externalId` to that policy.
///
/// `externalId` is "conditionally required when the channel's resource policy includes an
/// external ID", and a channel's resource-based policy is the CONTROL PLANE's document:
/// `cloudtrail:PutResourcePolicy` attaches and replaces it and
/// `cloudtrail:DeleteResourcePolicy` detaches it. Each of those calls is therefore what
/// carries it here, in the SAME request that writes it — so a caller that just attached a
/// policy including an external ID must present one on the very next ingest, and one that
/// just detached it no longer needs to. `policy` is the document now attached, or `None` for
/// a channel carrying none — the channel that accepts whatever `externalId` a request
/// carries, including none.
///
/// The DOCUMENT is what travels, not a summary of it: what it says about one request is
/// decided where the request is served ([`Channel::accepts_external_id`]), through the
/// shared IAM evaluator, so this seam carries no second reading of a policy.
///
/// `Some(true)` when this registry held the channel and now carries that policy,
/// `Some(false)` when it holds no channel under that ARN — a channel this data plane
/// cannot ingest onto at all, so there is no policy for it to carry — and `None`
/// when the ARN is not a channel ARN or this store could not be reached.
///
/// LOCK ORDER: as [`register_channel`].
pub fn set_channel_resource_policy<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: &str,
    policy: Option<&str>,
) -> Option<bool> {
    let parsed = parse_channel_arn(arn)?;
    let state = service_state(engine)?;
    let mut store = state.lock();
    let Some(channel) = store.channel_mut(&ChannelRef::Arn(parsed)) else {
        return Some(false);
    };
    channel.resource_policy = policy.map(str::to_string);
    Some(true)
}

/// What this data plane recorded about ONE channel's ingestion, for the control plane
/// that owns the channel resource to report.
///
/// Ingestion happens HERE (`PutAuditEvents`), so these are the facts only this service
/// holds: the most recent event it accepted on the channel, the most recent event it
/// attempted at all, and the error the most recent refused attempt carried. Each member
/// is absent exactly when this service has no such fact — it is never completed with a
/// value nothing recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelIngestion {
    /// When the most recent ACCEPTED event was ingested.
    pub latest_success_at: Option<SystemTime>,
    /// The `eventID` assigned to that accepted event.
    pub latest_success_event_id: Option<String>,
    /// When the most recent attempt — accepted or refused — was decided.
    pub latest_attempt_at: Option<SystemTime>,
    /// The `eventID` assigned to that attempt, which exists only if it was accepted.
    pub latest_attempt_event_id: Option<String>,
    /// The per-event error code the most recent REFUSED attempt was answered with.
    pub latest_error_code: Option<String>,
}

/// The ingestion facts this data plane holds for the channel `arn`, or `None` when there
/// are none to report: the ARN is not a channel ARN, this store cannot be reached, no
/// channel is registered under it, or nothing has ever attempted to ingest onto it.
///
/// The reporting direction of the control-plane seam: `cloudtrail:CreateChannel` makes
/// the channel present here ([`register_channel`]) and the call that reports the channel
/// reads back what this service then recorded for it, so a channel's ingestion table is
/// the ingest log and the refusals of THIS service rather than a second copy kept by the
/// caller. A READ: it reports what is stored and changes nothing.
///
/// LOCK ORDER: as [`register_channel`] — this takes THIS store's lock, holds no other
/// while it does, and hands back an owned value, so the caller asks it with no guard of
/// its own store held.
pub fn channel_ingestion<Eng: Send + Sync + 'static>(
    engine: &Eng,
    arn: &str,
) -> Option<ChannelIngestion> {
    let parsed = parse_channel_arn(arn)?;
    let state = service_state(engine)?;
    let store = state.lock();
    let channel = store.channel(&ChannelRef::Arn(parsed))?;
    let attempt = channel.latest_attempt()?;
    let accepted = channel.latest_ingest();
    Some(ChannelIngestion {
        latest_success_at: accepted.map(|event| event.ingested_at),
        latest_success_event_id: accepted.map(|event| event.event_id.clone()),
        latest_attempt_at: Some(attempt.attempted_at),
        latest_attempt_event_id: attempt.event_id.clone(),
        latest_error_code: channel
            .latest_refusal()
            .and_then(|refused| refused.error_code.clone()),
    })
}

/// Take the channel `arn` back out of this service's registry — the effect
/// `cloudtrail:DeleteChannel` has on the data plane — so a channel the control plane no
/// longer holds can no longer be ingested onto.
///
/// `Some(true)` when a registration was removed, `Some(false)` when this registry held
/// none under that ARN, and `None` when the ARN is not a channel ARN or this store could
/// not be reached at all: a registry this emulator cannot write answers the question not
/// at all, and is never a reason to report the channel as retired here.
///
/// LOCK ORDER: as [`register_channel`].
pub fn deregister_channel<Eng: Send + Sync + 'static>(engine: &Eng, arn: &str) -> Option<bool> {
    let parsed = parse_channel_arn(arn)?;
    let state = service_state(engine)?;
    let removed = state
        .lock()
        .remove_channel(&ChannelRef::Arn(parsed))
        .is_some();
    Some(removed)
}

/// This service's store in the shared engine: call it with a handler's `&*self.engine`.
///
/// Answer `None` with a modelled server error, never with a fresh engine.
pub fn service_state<Eng: Send + Sync + 'static>(
    engine: &Eng,
) -> Option<::engine::ServiceState<ComAmazonawsCloudtraildataCloudtraildataserviceState>> {
    (engine as &dyn ::std::any::Any)
        .downcast_ref::<::engine::InMemoryEngine>()
        .map(::engine::InMemoryEngine::state::<ComAmazonawsCloudtraildataCloudtraildataserviceState>)
}
