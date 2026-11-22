// SPDX-License-Identifier: CC0-1.0

//! The policy of a stored Verified Access endpoint: what one `ModifyVerifiedAccessEndpointPolicy`
//! request changes about it, the record of that request its idempotency token is kept under, the
//! one definition of what applying it leaves behind, the one writer that applies it, and the one
//! reading of a stored policy every report of one answers with.
//!
//! "Modifies the specified Amazon Web Services Verified Access endpoint policy"
//! (`ModifyVerifiedAccessEndpointPolicy`) changes facts of an endpoint this store already holds:
//! the document beside the record ([`VerifiedAccessEndpointRecord::policy_document`]), whether
//! that document is in force ([`VerifiedAccessEndpointRecord::policy_enabled`]) and the server
//! side encryption options the endpoint shape itself carries
//! (`VerifiedAccessEndpoint$SseSpecification`). So a modify writes the endpoint the create
//! stored, and `GetVerifiedAccessEndpointPolicy` reads it back through
//! [`verified_access_endpoint_policy_reported`] — the one reading both that reader and the
//! modify's own answer go through, so the two can never report one stored policy differently.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// The server side encryption options of a Verified Access resource, as this store holds them.
///
/// "Verified Access provides server side encryption by default to data at rest using Amazon Web
/// Services-owned KMS keys. You also have the option of using customer managed KMS keys"
/// (`VerifiedAccessSseSpecificationRequest`) — a request names them in one shape and every
/// report of them answers in another, so they are held here in ONE form that both
/// [`Self::of_requested`](VerifiedAccessSseOptions::of_requested) reads a request into and
/// [`Self::reported`](VerifiedAccessSseOptions::reported) answers a report from. Comparable, so
/// a recorded modification can be told apart from a different request carrying the same
/// idempotency token.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VerifiedAccessSseOptions {
    /// "Enable or disable the use of customer managed KMS keys for server side encryption"
    /// (`VerifiedAccessSseSpecificationRequest$CustomerManagedKeyEnabled`), or `None` when the
    /// request named no preference.
    pub customer_managed_key_enabled: Option<bool>,
    /// "The ARN of the KMS key"
    /// (`VerifiedAccessSseSpecificationRequest$KmsKeyArn`), or `None` when the request named
    /// none. The key belongs to ANOTHER service, which this store does not hold, so it is kept
    /// as given.
    pub kms_key_arn: Option<String>,
}

impl VerifiedAccessSseOptions {
    /// The options the request block `requested` names — every member absent when the request
    /// carried no block at all.
    pub fn of_requested(
        requested: Option<&crate::ops::types::VerifiedAccessSseSpecificationRequest>,
    ) -> Self {
        Self {
            customer_managed_key_enabled: requested
                .and_then(|requested| requested.customer_managed_key_enabled),
            kms_key_arn: requested.and_then(|requested| requested.kms_key_arn.clone()),
        }
    }

    /// These options as "The options in use for server side encryption"
    /// (`VerifiedAccessSseSpecificationResponse`) reads them.
    ///
    /// The response shape reports WHETHER a customer managed key is in use and which one, so
    /// options that named no preference report the Amazon Web Services owned key: not customer
    /// managed, and no key ARN to name.
    pub fn reported(&self) -> crate::ops::types::VerifiedAccessSseSpecificationResponse {
        crate::ops::types::VerifiedAccessSseSpecificationResponse {
            customer_managed_key_enabled: Some(self.customer_managed_key_enabled.unwrap_or(false)),
            kms_key_arn: self.kms_key_arn.clone(),
        }
    }
}

/// "The options in use for server side encryption"
/// (`VerifiedAccessSseSpecificationResponse`) for the request block `requested`, the ONE
/// reading of a requested specification every operation of this service that stores one answers
/// with.
pub fn verified_access_sse_reported(
    requested: Option<&crate::ops::types::VerifiedAccessSseSpecificationRequest>,
) -> crate::ops::types::VerifiedAccessSseSpecificationResponse {
    VerifiedAccessSseOptions::of_requested(requested).reported()
}

/// One `ModifyVerifiedAccessEndpointPolicy` request, as the token it was given with recorded
/// it: the endpoint whose policy it modified and the members it set.
///
/// "A unique, case-sensitive token that you provide to ensure idempotency of your modification
/// request" (`ModifyVerifiedAccessEndpointPolicyRequest$ClientToken`) — so a repeat of one
/// request applies nothing further, while the SAME token carrying a DIFFERENT request is the
/// mismatch EC2 refuses ([`idempotent_parameter_mismatch`]) rather than a second modification.
/// Telling those two apart needs the REQUEST, which is why a recorded modification carries the
/// members it set and not just the endpoint it named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAccessEndpointPolicyModification {
    /// The endpoint the request named.
    pub verified_access_endpoint_id: String,
    /// "The Verified Access policy document"
    /// (`ModifyVerifiedAccessEndpointPolicyRequest$PolicyDocument`), or `None` when the request
    /// carried none and so left the stored document as it was.
    pub policy_document: Option<String>,
    /// "The status of the Verified Access policy"
    /// (`ModifyVerifiedAccessEndpointPolicyRequest$PolicyEnabled`), or `None` when the request
    /// carried none and so left the stored status as it was.
    pub policy_enabled: Option<bool>,
    /// "The options for server side encryption"
    /// (`ModifyVerifiedAccessEndpointPolicyRequest$SseSpecification`), or `None` when the
    /// request carried no specification and so left the stored one as it was.
    pub sse_specification: Option<VerifiedAccessSseOptions>,
}

impl VerifiedAccessEndpointPolicyModification {
    /// `record` as this modification leaves it: every member the request carried replaces the
    /// stored value, and every member it did not carry keeps it.
    ///
    /// The ONE definition of what a policy modification does to an endpoint, so the pre-check
    /// of the store's byte budget and
    /// [`ComAmazonawsEc2Amazonec2State::modify_verified_access_endpoint_policy`]'s write cannot
    /// disagree about the record the call leaves behind. `LastUpdatedTime` is NOT set here: the
    /// instant a modification was applied is the store's to stamp, not a figure a pre-check may
    /// invent.
    pub fn applied_to(
        &self,
        record: &VerifiedAccessEndpointRecord,
    ) -> VerifiedAccessEndpointRecord {
        let mut applied = record.clone();
        if let Some(policy_document) = self.policy_document.clone() {
            applied.policy_document = Some(policy_document);
        }
        if let Some(policy_enabled) = self.policy_enabled {
            applied.policy_enabled = policy_enabled;
        }
        if let Some(sse_specification) = self.sse_specification.as_ref() {
            applied.view.sse_specification = Some(sse_specification.reported());
        }
        applied
    }
}

/// The caller-chosen bytes one recorded Verified Access endpoint policy modification holds for
/// the process's lifetime: the members the request set and the token the map is keyed by, over a
/// fixed floor for the entry itself. The endpoint id is the service's own, so it is bounded
/// already and not counted here.
///
/// The FLOOR is what lets [`MAX_STORED_BYTES`] bound how many modifications can be recorded, on
/// the terms [`verified_access_group_modification_payload_bytes`] states for that resource: a
/// client token is the caller's and may be a single byte, nothing reclaims a recorded
/// modification, and an endpoint's policy may be modified any number of times, so this map has
/// no record-count ceiling the way a create's store has.
pub fn verified_access_endpoint_policy_modification_payload_bytes(
    modification: &VerifiedAccessEndpointPolicyModification,
    client_token: &str,
) -> usize {
    ::std::mem::size_of::<VerifiedAccessEndpointPolicyModification>()
        .saturating_add(modification.policy_document.as_ref().map_or(0, String::len))
        .saturating_add(
            modification
                .sse_specification
                .as_ref()
                .and_then(|sse| sse.kms_key_arn.as_ref())
                .map_or(0, String::len),
        )
        .saturating_add(client_token.len())
}

/// One stored Verified Access endpoint's policy as a REPORT of it reads: the document the
/// endpoint holds, whether that document is in force, and the server side encryption options in
/// use for it.
///
/// The ONE reading of a stored policy, so `GetVerifiedAccessEndpointPolicy` and the answer of
/// `ModifyVerifiedAccessEndpointPolicy` cannot report the same stored endpoint two ways. The
/// members a given operation's output carries are the ones it reads off this.
#[derive(Debug, Clone)]
pub struct ReportedVerifiedAccessEndpointPolicy {
    /// "The Verified Access policy document"
    /// (`GetVerifiedAccessEndpointPolicyResult$PolicyDocument`) — absent on an endpoint no
    /// document has ever been given for.
    pub policy_document: Option<String>,
    /// "The status of the Verified Access policy"
    /// (`GetVerifiedAccessEndpointPolicyResult$PolicyEnabled`): whether the document is in
    /// force. Always reported, because the store always knows it; an endpoint holding no
    /// document in force reports none, whatever its stored status says.
    pub policy_enabled: Option<bool>,
    /// "The options in use for server side encryption"
    /// (`ModifyVerifiedAccessEndpointPolicyResult$SseSpecification`), read off the endpoint
    /// shape that carries them.
    pub sse_specification: Option<crate::ops::types::VerifiedAccessSseSpecificationResponse>,
}

/// The policy of the stored endpoint `record`, as every report of one reads it — see
/// [`ReportedVerifiedAccessEndpointPolicy`].
///
/// "A document of no text is no policy at all", so a document that is blank reads as the absent
/// member rather than as an empty one, and an endpoint holding no document in force reports no
/// policy in force whatever its stored status says: a status cannot be in force over a document
/// that is not there.
pub fn verified_access_endpoint_policy_reported(
    record: &VerifiedAccessEndpointRecord,
) -> ReportedVerifiedAccessEndpointPolicy {
    let policy_document = record
        .policy_document
        .as_deref()
        .filter(|document| !document.trim().is_empty())
        .map(str::to_string);
    ReportedVerifiedAccessEndpointPolicy {
        policy_enabled: Some(record.policy_enabled && policy_document.is_some()),
        policy_document,
        sse_specification: record.view.sse_specification.clone(),
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// The policy modification the `ModifyVerifiedAccessEndpointPolicy` `ClientToken` `token`
    /// named, or `None` when no modification has been made under it.
    pub fn verified_access_endpoint_policy_modification_for_token(
        &self,
        token: &str,
    ) -> Option<&VerifiedAccessEndpointPolicyModification> {
        self.verified_access_endpoint_policy_client_tokens.get(token)
    }

    /// Apply `modification` to the endpoint it names as of `at`, record it under `client_token`
    /// when the request carried one, and answer the policy as it NOW stands — `None` when this
    /// store holds no endpoint under that id.
    ///
    /// The one way a stored endpoint's policy is modified, so what the modify answers and what a
    /// later read of the same endpoint reports cannot differ: the policy it hands back is read
    /// off the stored record through [`verified_access_endpoint_policy_reported`], like every
    /// other report of one. A member the modification leaves `None` is one the request did not
    /// carry, so the stored value stays as it was; a member it carries replaces that value
    /// ([`VerifiedAccessEndpointPolicyModification::applied_to`]).
    ///
    /// The caller-chosen bytes are re-charged across the write — the record this replaces is
    /// refunded and the record it becomes is charged — so a caller cannot grow the store past
    /// [`MAX_STORED_BYTES`] by rewriting one endpoint's policy repeatedly; a handler asks
    /// [`Self::would_exceed_stored_bytes_after_refund`] about that net change first. The
    /// `EndpointDomainPrefix` the generated endpoint domain carries is left OUT of both figures:
    /// this call does not touch that name, so the bytes the create charged for it stay charged
    /// until the endpoint's removal gives them back.
    pub fn modify_verified_access_endpoint_policy(
        &mut self,
        modification: &VerifiedAccessEndpointPolicyModification,
        client_token: Option<&str>,
        at: SystemTime,
    ) -> Option<ReportedVerifiedAccessEndpointPolicy> {
        let held = self
            .verified_access_endpoints
            .get(&modification.verified_access_endpoint_id)?;
        let refund = verified_access_endpoint_payload_bytes(held, None, None);
        let mut record = modification.applied_to(held);
        // "The last updated time" (`VerifiedAccessEndpoint$LastUpdatedTime`): the instant this
        // modification was applied to the endpoint, whose policy is part of its configuration.
        record.view.last_updated_time = Some(iso8601_utc(at));
        let charge = verified_access_endpoint_payload_bytes(&record, None, None);
        let reported = verified_access_endpoint_policy_reported(&record);
        self.verified_access_endpoints
            .insert(modification.verified_access_endpoint_id.clone(), record);
        self.release_stored_bytes(refund);
        self.add_stored_bytes(charge);
        if let Some(token) = client_token {
            self.add_stored_bytes(
                verified_access_endpoint_policy_modification_payload_bytes(modification, token),
            );
            self.verified_access_endpoint_policy_client_tokens
                .insert(token.to_string(), modification.clone());
        }
        Some(reported)
    }
}
