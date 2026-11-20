// SPDX-License-Identifier: CC0-1.0

//! The consent portal: one [`ConsentPortalRecord`] per `consentPortalId`, holding the
//! sources it serves, the identity provider it authenticates end users against, the
//! `CREATING` -> `ACTIVE` transition the clock owes it, the address this emulator publishes
//! for it, and the refusals a missing or busy portal is reported with.
//!
//! Lives beside the stores in [`super`] rather than in any one `ops/` file, so the create,
//! the read, the listing, the update and the delete of a consent portal cannot drift from
//! each other.
//!
//! A portal is addressed only by the `consentPortalId` the create minted
//! (`ConsentPortalIdentifier` admits nothing else, `smithy_query shape=ConsentPortalIdentifier`),
//! so the store is keyed by it and the name index only answers whether the account already
//! holds that name.

use super::*;

use crate::ops::types::{
    ConsentPortalIdpConfig, ConsentPortalSource, ConsentPortalSummary, CreateConsentPortalInput,
    CreateConsentPortalOutput, GetConsentPortalOutput, UpdateConsentPortalInput,
    UpdateConsentPortalOutput,
};

/// The ARN resource type a consent portal carries: `ConsentPortalArnType` ends in
/// `…:consent-portal/{consentPortalId}` (`smithy_query shape=ConsentPortalArnType`).
const ARN_TYPE_CONSENT_PORTAL: &str = "consent-portal";

/// `arn:aws:bedrock-agentcore:{region}:{account}:consent-portal/{consentPortalId}` —
/// `ConsentPortalArnType`'s form.
pub fn consent_portal_arn(region: &str, consent_portal_id: &str) -> String {
    ::engine::emulator_arn(
        ARN_SERVICE,
        region,
        &format!("{ARN_TYPE_CONSENT_PORTAL}/{consent_portal_id}"),
    )
}

/// `ConsentPortalStatus` members this service drives. The enum's full value list is
/// `CREATING | ACTIVE | UPDATING | UPDATE_FAILED | DELETING | FAILED`
/// (`smithy_query shape=ConsentPortalStatus`); `UPDATE_FAILED` and `FAILED` belong to a
/// provisioning that failed, which nothing here makes happen.
pub const CONSENT_PORTAL_STATUS_CREATING: &str = "CREATING";
pub const CONSENT_PORTAL_STATUS_ACTIVE: &str = "ACTIVE";
pub const CONSENT_PORTAL_STATUS_UPDATING: &str = "UPDATING";
pub const CONSENT_PORTAL_STATUS_DELETING: &str = "DELETING";

/// How long a freshly created or updated consent portal stays in its transient status
/// before the clock owes it `ACTIVE`, and how long one whose deletion was accepted stays
/// readable in `DELETING` before the clock owes its removal.
///
/// Both the create and the update are accepted with HTTP 202 and report a transient status
/// (`smithy_query operation=CreateConsentPortal,UpdateConsentPortal`), and AWS documents no
/// duration for either transition, so the span is chosen: the same one the sibling
/// resources' transitions take ([`super::REGISTRY_SETTLE_DELAY`]), long enough that a caller
/// acting straight after the call still observes the transient status.
pub const CONSENT_PORTAL_SETTLE_DELAY: Duration = super::REGISTRY_SETTLE_DELAY;

/// How many consent portals this emulator will hold.
///
/// `CreateConsentPortal` models `ServiceQuotaExceededException` (HTTP 402) for exceeding the
/// real service's quota; this number is not a fidelity claim about that quota but the
/// emulator's own ceiling, set generously so no realistic workflow meets it, so a caller
/// looping on create cannot grow this process's memory without bound.
pub const MAX_CONSENT_PORTALS: usize = 1_000;

/// How this control plane spells the consent portal in a duplicate-name refusal.
const CONSENT_PORTAL_CONFLICT_LABEL: &str = "Consent portal";

/// The modelled not-found error for a consent portal this account does not hold: the
/// identifier the request carried, in the sentence this plane words a missing portal with
/// ("No consent portal found for id <consentPortalId>").
///
/// The text around the identifier is fixed and the identifier is the caller's own value, so
/// every operation that resolves a portal reports a missing one identically. This family
/// words it in its own sentence rather than through this service's shared
/// [`super::resource_not_found`], which names the resource but no identifier.
pub fn consent_portal_not_found(consent_portal_identifier: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(format!(
            "No consent portal found for id {consent_portal_identifier}"
        )),
    }
}

/// The modelled conflict for a consent portal `name` this account already holds.
///
/// `CreateConsentPortal`'s name "must be unique within your account"
/// (`smithy_query doc shape=CreateConsentPortalRequest member=name`), and the refusal is
/// this control plane's fixed sentence ([`super::name_conflict`]) around the name the
/// request carried.
pub fn consent_portal_name_conflict(name: &str) -> ConflictException {
    super::name_conflict(CONSENT_PORTAL_CONFLICT_LABEL, name)
}

/// The modelled conflict for a change asked of a consent portal whose own asynchronous work
/// is still under way: the portal is not `ACTIVE`, so the request collides with the
/// operation already running on it.
///
/// The sentence is fixed and tells the caller what to wait for; its variable parts are the
/// id the request carried and the status the portal is in right now, as the enum spells it
/// — the same token every response reports it in.
pub fn consent_portal_status_conflict(consent_portal_id: &str, status: &str) -> ConflictException {
    ConflictException {
        message: Some(format!(
            "Consent portal {consent_portal_id} is in {status} status and cannot be modified. \
             Retry the request once its status is {CONSENT_PORTAL_STATUS_ACTIVE}."
        )),
    }
}

/// One consent portal: everything `CreateConsentPortal` persisted for it, the address this
/// emulator publishes for it, and the transitions the clock owes it.
///
/// Every read of the portal reports this record, so a create, a listing and a later read
/// cannot disagree about its configuration or its status.
#[derive(Debug, Clone)]
pub struct ConsentPortalRecord {
    pub consent_portal_id: String,
    pub consent_portal_arn: String,
    pub name: String,
    pub description: Option<String>,
    pub execution_role_arn: String,
    pub idp_config: ConsentPortalIdpConfig,
    pub sources: Vec<ConsentPortalSource>,
    /// The address this emulator serves this portal's page at ([`consent_portal_url`]),
    /// recorded when the portal was created so every later read reports one address.
    pub portal_url: String,
    /// The tags the create carried. Held with the portal — it is what the request persisted
    /// — though no operation of this service reports a portal's tags yet
    /// (`ListTagsForResource` is still scaffolded).
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The status until [`settles_at`](Self::settles_at) passes.
    pub transient_status: String,
    /// The status from [`settles_at`](Self::settles_at) onwards.
    pub settled_status: String,
    /// The instant the transient status is owed its transition. Stored, never slept on: a
    /// read materialises the transition the clock already made due.
    pub settles_at: SystemTime,
    /// The instant an accepted deletion removes this record, once `DeleteConsentPortal` has
    /// accepted one; `None` for a portal nobody has deleted. Stored, never slept on: the
    /// removal is the clock's, materialised by
    /// [`expire_deleted_consent_portals`](ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState::expire_deleted_consent_portals)
    /// on the next call that reads this store.
    pub removal_due_at: Option<SystemTime>,
}

impl ConsentPortalRecord {
    /// The portal a create stores for the request `requested` in `region`, under the id that
    /// create minted and at the address `portal_url` this emulator serves it at: `CREATING`,
    /// with the transition to `ACTIVE` owed by the clock rather than by the call.
    pub fn created(
        region: &str,
        consent_portal_id: String,
        portal_url: String,
        requested: CreateConsentPortalInput,
        now: SystemTime,
    ) -> Self {
        Self {
            consent_portal_arn: consent_portal_arn(region, &consent_portal_id),
            consent_portal_id,
            name: requested.name,
            description: requested.description,
            execution_role_arn: requested.execution_role_arn,
            idp_config: requested.idp_config,
            sources: requested.sources,
            portal_url,
            tags: sorted_map(requested.tags),
            created_at: now,
            updated_at: now,
            transient_status: CONSENT_PORTAL_STATUS_CREATING.to_string(),
            settled_status: CONSENT_PORTAL_STATUS_ACTIVE.to_string(),
            settles_at: now + CONSENT_PORTAL_SETTLE_DELAY,
            removal_due_at: None,
        }
    }

    /// The status this portal reports at `now`: the transient one until the transition the
    /// clock owes comes due, the settled one afterwards. No read performs it — it is due at
    /// that instant whether or not anybody looks.
    pub fn status_at(&self, now: SystemTime) -> &str {
        if now < self.settles_at {
            &self.transient_status
        } else {
            &self.settled_status
        }
    }

    /// Whether a deletion has been accepted for this portal: it reports `DELETING` and the
    /// clock owes its removal.
    pub fn is_deleting(&self) -> bool {
        self.removal_due_at.is_some()
    }

    /// Apply an accepted modification at `now`, with the PATCH semantics
    /// `UpdateConsentPortalRequest` states by declaring each editable member optional: a
    /// member the request carries replaces the stored one, a member it leaves out leaves the
    /// stored one alone. The portal's name, its sources, its id and its ARN are not
    /// modifiable by this operation — the request carries no member for them — so they stay
    /// as the create stored them.
    ///
    /// The portal reports `UPDATING` until the clock owes it `ACTIVE` one settle span later,
    /// as the create's own transition is owed.
    pub fn accept_update(&mut self, requested: UpdateConsentPortalInput, now: SystemTime) {
        if let Some(execution_role_arn) = requested.execution_role_arn {
            self.execution_role_arn = execution_role_arn;
        }
        if let Some(idp_config) = requested.idp_config {
            self.idp_config = idp_config;
        }
        if let Some(description) = requested.description {
            self.description = Some(description);
        }
        self.transient_status = CONSENT_PORTAL_STATUS_UPDATING.to_string();
        self.settled_status = CONSENT_PORTAL_STATUS_ACTIVE.to_string();
        self.settles_at = now + CONSENT_PORTAL_SETTLE_DELAY;
        self.updated_at = now;
    }

    /// Accept a deletion at `now`: the portal reports `DELETING` from this instant on and the
    /// clock owes its removal one settle span later.
    ///
    /// Idempotent, as `DeleteConsentPortal` is (`@idempotent`): a repeat while the deletion
    /// is still under way leaves the record and the instant it is owed alone rather than
    /// re-arming the clock.
    pub fn accept_deletion(&mut self, now: SystemTime) {
        if self.is_deleting() {
            return;
        }
        self.transient_status = CONSENT_PORTAL_STATUS_DELETING.to_string();
        self.settled_status = CONSENT_PORTAL_STATUS_DELETING.to_string();
        self.settles_at = now;
        self.removal_due_at = Some(now + CONSENT_PORTAL_SETTLE_DELAY);
        self.updated_at = now;
    }

    /// This portal as `CreateConsentPortal`'s response envelope: every member
    /// `CreateConsentPortalResponse` declares, read off the record just written.
    ///
    /// `statusReason` "provides additional information about the current status"
    /// (`smithy_query doc shape=CreateConsentPortalResponse member=statusReason`), which for
    /// a portal this emulator created and is settling on the clock is nothing beyond the
    /// status itself: it has failed in no way, so there is no reason to report.
    pub fn create_output(&self, now: SystemTime) -> CreateConsentPortalOutput {
        CreateConsentPortalOutput {
            sources: self.sources.clone(),
            consent_portal_arn: self.consent_portal_arn.clone(),
            consent_portal_id: self.consent_portal_id.clone(),
            created_at: timestamp(self.created_at),
            description: self.description.clone(),
            execution_role_arn: self.execution_role_arn.clone(),
            idp_config: self.idp_config.clone(),
            name: self.name.clone(),
            portal_url: Some(self.portal_url.clone()),
            status: self.status_at(now).to_string(),
            status_reason: None,
            updated_at: timestamp(self.updated_at),
        }
    }

    /// This portal as `GetConsentPortal`'s response envelope — `GetConsentPortalResponse`
    /// declares the same members as `CreateConsentPortalResponse`, so it too is rendered
    /// from the one stored record.
    pub fn get_output(&self, now: SystemTime) -> GetConsentPortalOutput {
        let created = self.create_output(now);
        GetConsentPortalOutput {
            sources: created.sources,
            consent_portal_arn: created.consent_portal_arn,
            consent_portal_id: created.consent_portal_id,
            created_at: created.created_at,
            description: created.description,
            execution_role_arn: created.execution_role_arn,
            idp_config: created.idp_config,
            name: created.name,
            portal_url: created.portal_url,
            status: created.status,
            status_reason: created.status_reason,
            updated_at: created.updated_at,
        }
    }

    /// This portal as `UpdateConsentPortal`'s response envelope — the same members again,
    /// off the same record, with the status and the timestamps the accepted change left on
    /// it.
    pub fn update_output(&self, now: SystemTime) -> UpdateConsentPortalOutput {
        let created = self.create_output(now);
        UpdateConsentPortalOutput {
            sources: created.sources,
            consent_portal_arn: created.consent_portal_arn,
            consent_portal_id: created.consent_portal_id,
            created_at: created.created_at,
            description: created.description,
            execution_role_arn: created.execution_role_arn,
            idp_config: created.idp_config,
            name: created.name,
            portal_url: created.portal_url,
            status: created.status,
            status_reason: created.status_reason,
            updated_at: created.updated_at,
        }
    }

    /// This portal as `ListConsentPortals` reports it: the same record projected onto
    /// `ConsentPortalSummary`, which declares every member a read does but the execution
    /// role, the identity provider configuration and the status reason — so a listing and a
    /// read cannot disagree about the portal.
    pub fn summary(&self, now: SystemTime) -> ConsentPortalSummary {
        ConsentPortalSummary {
            sources: self.sources.clone(),
            consent_portal_arn: self.consent_portal_arn.clone(),
            consent_portal_id: self.consent_portal_id.clone(),
            created_at: timestamp(self.created_at),
            description: self.description.clone(),
            name: self.name.clone(),
            portal_url: Some(self.portal_url.clone()),
            status: self.status_at(now).to_string(),
            updated_at: timestamp(self.updated_at),
        }
    }
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// Mint a `consentPortalId` for `name` that no stored consent portal holds: the name +
    /// `-` + 10 alphanumerics, per `ConsentPortalIdType`'s pattern
    /// `^[a-zA-Z0-9\-_]{1,50}-[A-Za-z0-9]{10}$`.
    ///
    /// The stem is the name as sent: `ConsentPortalNameType` is `^[a-zA-Z0-9_-]{1,50}$`,
    /// exactly the stem that pattern admits.
    pub fn mint_consent_portal_id(&mut self, name: &str, at: SystemTime) -> String {
        loop {
            self.mint_counter = self.mint_counter.wrapping_add(1);
            let candidate = format!("{name}-{}", suffix_for(name, self.mint_counter, at));
            if !self.consent_portals.contains_key(&candidate) {
                return candidate;
            }
        }
    }

    /// Remove every consent portal whose deletion the clock has already made due.
    ///
    /// `DeleteConsentPortal` ACCEPTS the deletion (204) and the portal reports `DELETING`
    /// until then; the record itself goes when the span it owes passes, so a caller polling
    /// it sees `DELETING` and then the modelled 404, and the name it held is free again. The
    /// removal is owed by the CLOCK — it was due at this instant whether or not anybody
    /// called — so every operation that reads this store materialises it first, and nothing
    /// ever sleeps for it.
    pub fn expire_deleted_consent_portals(&mut self, now: SystemTime) {
        let expired: Vec<String> = self
            .consent_portals
            .iter()
            .filter(|(_, record)| record.removal_due_at.is_some_and(|at| now >= at))
            .map(|(id, _)| id.clone())
            .collect();
        for consent_portal_id in expired {
            if let Some(record) = self.consent_portals.remove(&consent_portal_id) {
                self.consent_portal_names.remove(&record.name);
            }
        }
    }
}

/// The segment that names this service's CONSENT PORTAL plane, behind the reserved namespace
/// segment every plane this emulator serves at its own front door is addressed under
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`):
/// `/_aws/bedrock-agentcore-consent-portal/{consentPortalId}`.
///
/// Shared by the URL a portal reports and by the route that answers there, so the address
/// this service publishes is the address it serves.
pub const CONSENT_PORTAL_PLANE_SEGMENT: &str = "bedrock-agentcore-consent-portal";

/// The request path the consent portal `consent_portal_id` is served at.
///
/// A `consentPortalId` is admitted only by `^[a-zA-Z0-9\-_]{1,50}-[A-Za-z0-9]{10}$`, so
/// every character it can carry is already a path character and the id needs no escaping.
pub fn consent_portal_path(consent_portal_id: &str) -> String {
    format!(
        "/{}/{CONSENT_PORTAL_PLANE_SEGMENT}/{consent_portal_id}",
        ::service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE
    )
}

/// Where an end user reaches the consent portal `consent_portal_id`: the endpoint THIS
/// emulator is served at (`endpoint_base`) followed by that portal's own path.
///
/// `portalUrl` is "The URL used to access the consent portal"
/// (`smithy_query doc shape=ConsentPortalSummary member=portalUrl`) — an address a caller
/// FOLLOWS. AWS serves its own portals on a host this emulator neither owns nor answers on,
/// so the published address is this emulator's own and `crate::service` serves exactly that
/// path from the stored portal.
pub fn consent_portal_url(endpoint_base: &str, consent_portal_id: &str) -> String {
    format!(
        "{}{}",
        endpoint_base.trim_end_matches('/'),
        consent_portal_path(consent_portal_id)
    )
}

/// The consent portal a request `uri` addresses, for a path of the form
/// [`consent_portal_path`] mints, and `None` for every other path.
///
/// The inverse of the minting above, read through the shared addressing module, so a URL
/// this service published resolves back to the portal behind it. A trailing `/` is the same
/// address (that is how a browser normalises a bare path), and nothing below the portal's
/// own segment is this route: the portal is one page, not a path space.
pub fn consent_portal_route(uri: &str) -> Option<&str> {
    use ::service_common::data_plane_addressing::{first_segment, EMULATOR_PATH_NAMESPACE};
    let path = request_target_path(uri);
    let (namespace, rest) = first_segment(path)?;
    if namespace != EMULATOR_PATH_NAMESPACE {
        return None;
    }
    let (plane, rest) = first_segment(rest)?;
    if plane != CONSENT_PORTAL_PLANE_SEGMENT {
        return None;
    }
    let (consent_portal_id, below) = first_segment(rest)?;
    (!consent_portal_id.is_empty() && below == "/").then_some(consent_portal_id)
}

/// The PATH of a request target, whether it arrived in origin form (`/a/b?q`) or in absolute
/// form (`https://host/a/b?q`), which is how a request that carries its endpoint in the URI
/// spells the same address.
pub(super) fn request_target_path(uri: &str) -> &str {
    let path = uri.split(['?', '#']).next().unwrap_or(uri);
    match path.split_once("://") {
        Some((_scheme, authority_and_path)) => authority_and_path
            .find('/')
            .map_or("/", |at| &authority_and_path[at..]),
        None => path,
    }
}

/// The media type a consent portal is served as: a web page, served from this emulator's own
/// origin.
pub const CONSENT_PORTAL_MEDIA_TYPE: &str = "text/html; charset=utf-8";

/// The page a fetch of a portal's address renders: the portal this address belongs to, as
/// the store holds it right now — its name, its id, the status it currently reports and the
/// scopes its identity provider configuration asks an end user to consent to.
///
/// Every value comes from the stored record, so the page an end user opens cannot disagree
/// with what `GetConsentPortal` reports about the same portal. The name and the scopes are
/// caller-supplied strings, so they are written into the document through the escaping of
/// the `html-escape` crate rather than inlined.
pub fn consent_portal_page(record: &ConsentPortalRecord, now: SystemTime) -> String {
    let scopes = record
        .idp_config
        .scopes
        .iter()
        .map(|scope| format!("<li>{}</li>\n", escaped(scope)))
        .collect::<String>();
    format!(
        "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
         <title>{title}</title>\n</head>\n<body>\n<h1>{name}</h1>\n\
         <dl>\n<dt>Consent portal</dt><dd>{id}</dd>\n<dt>Status</dt><dd>{status}</dd>\n</dl>\n\
         <ul>\n{scopes}</ul>\n</body>\n</html>\n",
        title = escaped(&format!("AgentCore consent portal — {}", record.name)),
        name = escaped(&record.name),
        id = escaped(&record.consent_portal_id),
        status = escaped(record.status_at(now)),
    )
}

/// The page a fetch of a portal address this emulator holds no consent portal for renders: a
/// portal that was never created, or one whose deletion has completed.
pub fn consent_portal_not_found_page() -> String {
    "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n\
     <title>AgentCore consent portal</title>\n</head>\n<body>\n\
     <h1>No such consent portal</h1>\n</body>\n</html>\n"
        .to_string()
}

/// `value` as text inside an HTML element: the `html-escape` crate's own escaping, so no
/// stored string can open or close an element of the page it is written into.
///
/// `pub(super)` so every page this service publishes escapes with the one helper
/// ([`super::payment_connectors`] renders the other one).
pub(super) fn escaped(value: &str) -> String {
    ::html_escape::encode_safe(value).into_owned()
}
