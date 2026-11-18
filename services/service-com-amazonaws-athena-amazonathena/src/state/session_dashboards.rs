// SPDX-License-Identifier: CC0-1.0

//! The user interface ONE session serves, and the ARN that addresses it — what
//! `GetResourceDashboard` answers for.
//!
//! One concern of this service's state, in two halves:
//!
//! * WHICH session an ARN names ([`addressed_session`]). `GetResourceDashboardRequest$
//!   ResourceARN` is documented "The Amazon Resource Name (ARN) for a session", so the
//!   resource the call addresses is a session of this account — read out of the ARN the
//!   caller submitted and resolved against the one [`SessionRegistry`] every other
//!   session-addressed operation reads, rather than against a second copy. The ARN is read
//!   through [`::service_common::arn`], the one place this tree parses a caller-supplied
//!   ARN.
//! * WHAT that session's interface is ([`SessionRecord::dashboard_url`]).
//!   `GetResourceDashboardResponse$Url` is documented "The Live UI/Persistence UI url for a
//!   session": those are the two interfaces a Spark session has, and WHICH of them the URL
//!   addresses follows the session's own state rather than the request — a session whose
//!   resources are running serves its live interface, and one whose resources "are no longer
//!   running" (`SessionStatus$State`, the TERMINATED state) is served by the persistent
//!   interface that outlives them. So a dashboard is readable for as long as the record is,
//!   and the address a caller is handed changes when the session does.
//!
//! The URL is PRESIGNED against this emulator's own signer ([`session_dashboard_url`]), so a
//! caller that follows it reaches the emulator that answered it rather than an address
//! nothing serves, and it stops working once the validity it was signed for has passed.
//! Everything in it but the signature is derived from the session the ARN resolved to, so
//! two reads of one session's dashboard address the same interface.
//!
//! Nothing here writes: a dashboard is not a resource of its own but a projection of a
//! session, so reading one leaves the registry exactly as the next caller would have found
//! it.

use std::time::{Duration, SystemTime};

use super::*;

/// The resource type an Athena SESSION is addressed by inside an ARN.
///
/// Athena's own resource-type table publishes `workgroup`, `datacatalog` and
/// `capacity-reservation` (see [`WORK_GROUP_RESOURCE_TYPE`] and its neighbours) and no
/// type for a session, which is addressed by its `SessionId` everywhere else in this API.
/// So the session an ARN names is read from the resource field's own tail: this token where
/// the ARN names the session directly (`session/<SessionId>`), and the session segment of a
/// workgroup-scoped ARN (`workgroup/<WorkGroupName>/session/<SessionId>`), which is the
/// form that spells out the workgroup a session belongs to ([`SessionRecord::work_group`]).
pub const SESSION_RESOURCE_TYPE: &str = "session";

/// The message a dashboard read answers an ARN it resolved no session of this account from
/// with.
///
/// ONE sentence for the whole class, naming the MEMBER that was wrong and nothing of the
/// value it carried: a value that is not an ARN, an ARN of another vendor, Region or
/// account, one naming no session at all, and one naming a session this account does not
/// hold are all answered alike, because what the service reports is that the submitted
/// `ResourceARN` resolved to nothing — and a refusal that quoted the ARN back would
/// disclose which ARNs do resolve.
pub const RESOURCE_ARN_NOT_FOUND: &str = "ResourceARN is not found";

/// The session a `ResourceARN` addresses: its id, and the workgroup when the ARN scopes it
/// to one.
///
/// Borrowed from the ARN that was read, so resolving one allocates nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AddressedSession<'arn> {
    /// The workgroup the ARN scoped the session to, for the form that names one; `None`
    /// where the ARN names the session alone. A session BELONGS to the workgroup it was
    /// started in, so an ARN naming another workgroup's name addresses no session this
    /// account holds.
    pub work_group: Option<&'arn str>,
    /// The `SessionId` the ARN names — the key the [`SessionRegistry`] holds a session
    /// under.
    pub session_id: &'arn str,
}

/// The session the ARN `arn` addresses, where `region` is the Region the CALL was made in
/// (see [`authorization::region_from_request`]), or `None` for an ARN no session of this
/// endpoint resolves from.
///
/// A dashboard read reaches the sessions of the account it was signed for, in the Region it
/// was addressed to, so an ARN of another vendor, Region or account resolves nothing here —
/// the same reading [`addressed_resource`] makes of a tagging call's ARN, except that
/// "another account" is not told apart from the rest: this operation reports only that the
/// submitted member resolved to no session ([`RESOURCE_ARN_NOT_FOUND`]).
///
/// The partition is not read: one endpoint serves one partition, and the account and Region
/// above already say whether the ARN addresses what this endpoint holds. An EMPTY account or
/// Region field names no account or Region to mismatch — AWS writes a partition-global or
/// account-less resource that way — so such an ARN is read for the session it names.
#[must_use]
pub fn addressed_session<'arn>(arn: &'arn str, region: &str) -> Option<AddressedSession<'arn>> {
    let parsed = ::service_common::arn::parse(arn)?;
    if parsed.vendor != authorization::ACTION_PREFIX {
        return None;
    }
    if !parsed.account.is_empty() && parsed.account != ::engine::emulator_account_id() {
        return None;
    }
    if !parsed.region.is_empty() && parsed.region != region {
        return None;
    }
    // AWS writes a resource in either form, `<type>/<id>` and `<type>:<id>`, so both
    // separators divide the resource field's segments — the division
    // `::service_common::arn::resource_type` already reads the leading token by.
    let segments: Vec<&'arn str> = parsed.resource.split(['/', ':']).collect();
    let addressed = match segments[..] {
        [SESSION_RESOURCE_TYPE, session_id] => AddressedSession {
            work_group: None,
            session_id,
        },
        [WORK_GROUP_RESOURCE_TYPE, work_group, SESSION_RESOURCE_TYPE, session_id] => {
            AddressedSession {
                work_group: Some(work_group),
                session_id,
            }
        }
        _ => return None,
    };
    (!addressed.session_id.is_empty()
        && addressed
            .work_group
            .is_none_or(|work_group| !work_group.is_empty()))
    .then_some(addressed)
}

/// The path segment the interface of a session whose resources are RUNNING is served on:
/// the live interface of `GetResourceDashboardResponse$Url`'s pair.
pub const LIVE_UI_PATH: &str = "live";

/// The path segment the interface of a session whose resources are GONE is served on: the
/// persistent interface of that same pair, which is what remains to read once a session has
/// been shut down.
pub const PERSISTENCE_UI_PATH: &str = "persistence";

/// How long a dashboard URL this service hands out stays valid.
///
/// A CHOSEN bound: Athena publishes no validity for a session's interface URL, so this is
/// long enough for a caller to open the interface it addresses and short enough that a URL
/// which was cached or shared stops working.
pub const SESSION_DASHBOARD_URL_VALIDITY: Duration = Duration::from_secs(60 * 60);

/// The presigned address of the interface `ui_path` of the session `session_id`.
///
/// The ONE composition of a dashboard address in this service: query-signed by this
/// emulator's own signer for [`SESSION_DASHBOARD_URL_VALIDITY`], so a caller that follows it
/// reaches the emulator that answered, and the URL carries the instant it was signed and how
/// long it stays valid — which is what makes it stop working rather than open an interface
/// for ever.
///
/// `None` when the request cannot be signed, which the answering call reports as its
/// modelled server error rather than an address that verifies nowhere.
#[must_use]
pub fn session_dashboard_url(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    session_id: &str,
    ui_path: &str,
) -> Option<String> {
    let path = format!("/sessions/{session_id}/{ui_path}/");
    let request = ::smithy_server_runtime::SignedUrlRequest::new(
        ::http::Method::GET,
        ctx.identity().signing_name(),
        &path,
    );
    match ctx.url_signer().sign(request, SESSION_DASHBOARD_URL_VALIDITY) {
        Ok(url) => Some(url.as_str().to_string()),
        Err(error) => {
            ::tracing::warn!(error = ?error, "a session dashboard URL could not be presigned");
            None
        }
    }
}

impl SessionRecord {
    /// Which of the two interfaces this session serves at `now`: the live one while its
    /// resources are running, the persistent one once they are gone.
    #[must_use]
    pub fn dashboard_ui_path(&self, now: SystemTime) -> &'static str {
        if self.is_terminated_at(now) {
            PERSISTENCE_UI_PATH
        } else {
            LIVE_UI_PATH
        }
    }

    /// The presigned address of this session's interface at `now`, composed by the one
    /// builder above from the session's OWN id and state.
    #[must_use]
    pub fn dashboard_url(
        &self,
        ctx: &::smithy_server_runtime::OperationContext<'_>,
        now: SystemTime,
    ) -> Option<String> {
        session_dashboard_url(ctx, &self.id, self.dashboard_ui_path(now))
    }
}
