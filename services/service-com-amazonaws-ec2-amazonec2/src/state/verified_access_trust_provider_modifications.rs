// SPDX-License-Identifier: CC0-1.0

//! What one `ModifyVerifiedAccessTrustProvider` request changes about a stored Verified Access
//! trust provider: the record of that request its idempotency token is kept under, the ONE
//! definition of what applying it leaves behind, and the one writer that applies it.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// One `ModifyVerifiedAccessTrustProvider` request, as the token it was given with recorded
/// it: the trust provider it modified and the members it set.
///
/// "A unique, case-sensitive token that you provide to ensure idempotency of your modification
/// request" (`ModifyVerifiedAccessTrustProviderRequest$ClientToken`) — so a repeat of one
/// request applies nothing further, while the SAME token carrying a DIFFERENT request is the
/// mismatch EC2 refuses ([`idempotent_parameter_mismatch`]) rather than a second modification.
/// Telling those two apart needs the REQUEST, which is why a recorded modification carries the
/// members it set and not just the trust provider it named.
///
/// The members are held as the MODELLED request shapes themselves
/// (`smithy_query(query="operation",operation="ModifyVerifiedAccessTrustProvider")`), so no
/// member of a request can be recorded in a form the model does not describe.
#[derive(Debug, Clone)]
pub struct VerifiedAccessTrustProviderModification {
    /// The trust provider the request named.
    pub verified_access_trust_provider_id: String,
    /// "A description for the Verified Access trust provider"
    /// (`ModifyVerifiedAccessTrustProviderRequest$Description`), or `None` when the request
    /// carried none and so left the stored description as it was.
    pub description: Option<String>,
    /// "The options for an OpenID Connect-compatible user-identity trust provider"
    /// (`ModifyVerifiedAccessTrustProviderRequest$OidcOptions`), or `None` when the request
    /// carried none.
    pub oidc_options: Option<crate::ops::types::ModifyVerifiedAccessTrustProviderOidcOptions>,
    /// "The options for a device-based trust provider"
    /// (`ModifyVerifiedAccessTrustProviderRequest$DeviceOptions`), or `None` when the request
    /// carried none.
    pub device_options: Option<crate::ops::types::ModifyVerifiedAccessTrustProviderDeviceOptions>,
    /// "The OpenID Connect (OIDC) options"
    /// (`ModifyVerifiedAccessTrustProviderRequest$NativeApplicationOidcOptions`), or `None`
    /// when the request carried none.
    pub native_application_oidc_options:
        Option<crate::ops::types::ModifyVerifiedAccessNativeApplicationOidcOptions>,
    /// "The options for server side encryption"
    /// (`ModifyVerifiedAccessTrustProviderRequest$SseSpecification`), or `None` when the
    /// request carried none.
    pub sse_specification: Option<crate::ops::types::VerifiedAccessSseSpecificationRequest>,
}

/// The members of one request's OIDC options, lined up in the order the model declares them so
/// that two of them can be compared: the modelled shapes are generated without an equality of
/// their own, and a replay has to be told from a different request wearing a spent token.
fn oidc_members(
    options: &crate::ops::types::ModifyVerifiedAccessTrustProviderOidcOptions,
) -> Vec<Option<&str>> {
    vec![
        options.issuer.as_deref(),
        options.authorization_endpoint.as_deref(),
        options.token_endpoint.as_deref(),
        options.user_info_endpoint.as_deref(),
        options.client_id.as_deref(),
        options.client_secret.as_deref(),
        options.scope.as_deref(),
    ]
}

/// The members of one request's native application OIDC options, on the terms
/// [`oidc_members`] gives the OIDC options.
fn native_members(
    options: &crate::ops::types::ModifyVerifiedAccessNativeApplicationOidcOptions,
) -> Vec<Option<&str>> {
    vec![
        options.public_signing_key_endpoint.as_deref(),
        options.issuer.as_deref(),
        options.authorization_endpoint.as_deref(),
        options.token_endpoint.as_deref(),
        options.user_info_endpoint.as_deref(),
        options.client_id.as_deref(),
        options.client_secret.as_deref(),
        options.scope.as_deref(),
    ]
}

/// The members of one request's server-side-encryption options, on the terms
/// [`oidc_members`] gives the OIDC options.
fn sse_members(
    specification: &crate::ops::types::VerifiedAccessSseSpecificationRequest,
) -> (Option<bool>, Option<&str>) {
    (
        specification.customer_managed_key_enabled,
        specification.kms_key_arn.as_deref(),
    )
}

/// Two modifications are the same request when they name the same trust provider and carry the
/// same members, member for member — the question
/// [`ComAmazonawsEc2Amazonec2State::verified_access_trust_provider_modification_for_token`]
/// is asked to tell a REPLAY of one request from a DIFFERENT request wearing its token.
impl PartialEq for VerifiedAccessTrustProviderModification {
    fn eq(&self, other: &Self) -> bool {
        self.verified_access_trust_provider_id == other.verified_access_trust_provider_id
            && self.description == other.description
            && self.oidc_options.as_ref().map(oidc_members)
                == other.oidc_options.as_ref().map(oidc_members)
            && self
                .device_options
                .as_ref()
                .map(|options| options.public_signing_key_url.as_deref())
                == other
                    .device_options
                    .as_ref()
                    .map(|options| options.public_signing_key_url.as_deref())
            && self
                .native_application_oidc_options
                .as_ref()
                .map(native_members)
                == other
                    .native_application_oidc_options
                    .as_ref()
                    .map(native_members)
            && self.sse_specification.as_ref().map(sse_members)
                == other.sse_specification.as_ref().map(sse_members)
    }
}

impl Eq for VerifiedAccessTrustProviderModification {}

/// Replace `stored` with `update` when the request carried that member, and leave it as it was
/// when it did not — the rule every member of a modification is applied by, at any depth.
fn apply_member(stored: &mut Option<String>, update: &Option<String>) {
    if let Some(value) = update.clone() {
        *stored = Some(value);
    }
}

impl VerifiedAccessTrustProviderModification {
    /// `provider` as this modification leaves it: every member the request carried replaces the
    /// stored value, and every member it did not carry keeps it.
    ///
    /// The ONE definition of what a modification does to a trust provider, so the pre-check of
    /// the store's byte budget and
    /// [`ComAmazonawsEc2Amazonec2State::modify_verified_access_trust_provider`]'s write cannot
    /// disagree about the record the call leaves behind. `LastUpdatedTime` is NOT set here: the
    /// instant a modification was applied is the store's to stamp, not a figure a pre-check may
    /// invent.
    ///
    /// The options blocks are applied MEMBER BY MEMBER rather than wholesale, because each
    /// member of them is optional in the request shape the model declares for a modify, while
    /// the create's shape requires the endpoints that identify the provider: a modify that
    /// carries one endpoint would otherwise take the others away. The OIDC client secret is
    /// write-only, so what a report carries in its place is the mask every other reader of a
    /// trust provider answers with ([`REDACTED_CLIENT_SECRET`]), and the native application
    /// options report no secret at all — the response shape carries no member for one.
    pub fn applied_to(
        &self,
        provider: &crate::ops::types::VerifiedAccessTrustProvider,
    ) -> crate::ops::types::VerifiedAccessTrustProvider {
        let mut applied = provider.clone();
        apply_member(&mut applied.description, &self.description);
        if let Some(update) = self.oidc_options.as_ref() {
            let mut options = applied.oidc_options.take().unwrap_or_default();
            apply_member(&mut options.issuer, &update.issuer);
            apply_member(
                &mut options.authorization_endpoint,
                &update.authorization_endpoint,
            );
            apply_member(&mut options.token_endpoint, &update.token_endpoint);
            apply_member(&mut options.user_info_endpoint, &update.user_info_endpoint);
            apply_member(&mut options.client_id, &update.client_id);
            apply_member(&mut options.scope, &update.scope);
            if update.client_secret.is_some() {
                options.client_secret = Some(REDACTED_CLIENT_SECRET.to_string());
            }
            applied.oidc_options = Some(options);
        }
        if let Some(update) = self.device_options.as_ref() {
            let mut options = applied.device_options.take().unwrap_or_default();
            // A modify's device options carry no tenant id — the modelled shape has no member
            // for one — so the tenant the create named stays as it was.
            apply_member(
                &mut options.public_signing_key_url,
                &update.public_signing_key_url,
            );
            applied.device_options = Some(options);
        }
        if let Some(update) = self.native_application_oidc_options.as_ref() {
            let mut options = applied
                .native_application_oidc_options
                .take()
                .unwrap_or_default();
            apply_member(
                &mut options.public_signing_key_endpoint,
                &update.public_signing_key_endpoint,
            );
            apply_member(&mut options.issuer, &update.issuer);
            apply_member(
                &mut options.authorization_endpoint,
                &update.authorization_endpoint,
            );
            apply_member(&mut options.token_endpoint, &update.token_endpoint);
            apply_member(&mut options.user_info_endpoint, &update.user_info_endpoint);
            apply_member(&mut options.client_id, &update.client_id);
            apply_member(&mut options.scope, &update.scope);
            applied.native_application_oidc_options = Some(options);
        }
        if let Some(update) = self.sse_specification.as_ref() {
            let mut specification = applied.sse_specification.take().unwrap_or_default();
            if let Some(enabled) = update.customer_managed_key_enabled {
                specification.customer_managed_key_enabled = Some(enabled);
            }
            apply_member(&mut specification.kms_key_arn, &update.kms_key_arn);
            applied.sse_specification = Some(specification);
        }
        applied
    }
}

/// The caller-chosen bytes one recorded Verified Access trust provider modification holds for
/// the process's lifetime: the members the request set and the token the map is keyed by, over
/// a fixed floor for the entry itself. The trust provider id is the service's own, so it is
/// bounded already and not counted here.
///
/// The FLOOR is what lets [`MAX_STORED_BYTES`] bound how many modifications can be recorded, on
/// the terms [`verified_access_instance_modification_payload_bytes`] states for that resource:
/// a client token is the caller's and may be a single byte, nothing reclaims a recorded
/// modification, and a trust provider may be modified any number of times, so this map has no
/// record-count ceiling the way a create's store has.
pub fn verified_access_trust_provider_modification_payload_bytes(
    modification: &VerifiedAccessTrustProviderModification,
    client_token: &str,
) -> usize {
    let text = |value: &Option<String>| value.as_ref().map_or(0, String::len);
    let mut bytes = ::std::mem::size_of::<VerifiedAccessTrustProviderModification>()
        .saturating_add(text(&modification.description))
        .saturating_add(client_token.len());
    if let Some(options) = modification.oidc_options.as_ref() {
        bytes = bytes
            .saturating_add(text(&options.issuer))
            .saturating_add(text(&options.authorization_endpoint))
            .saturating_add(text(&options.token_endpoint))
            .saturating_add(text(&options.user_info_endpoint))
            .saturating_add(text(&options.client_id))
            .saturating_add(text(&options.client_secret))
            .saturating_add(text(&options.scope));
    }
    if let Some(options) = modification.device_options.as_ref() {
        bytes = bytes.saturating_add(text(&options.public_signing_key_url));
    }
    if let Some(options) = modification.native_application_oidc_options.as_ref() {
        bytes = bytes
            .saturating_add(text(&options.public_signing_key_endpoint))
            .saturating_add(text(&options.issuer))
            .saturating_add(text(&options.authorization_endpoint))
            .saturating_add(text(&options.token_endpoint))
            .saturating_add(text(&options.user_info_endpoint))
            .saturating_add(text(&options.client_id))
            .saturating_add(text(&options.client_secret))
            .saturating_add(text(&options.scope));
    }
    if let Some(specification) = modification.sse_specification.as_ref() {
        bytes = bytes.saturating_add(text(&specification.kms_key_arn));
    }
    bytes
}

impl ComAmazonawsEc2Amazonec2State {
    /// The modification the `ModifyVerifiedAccessTrustProvider` `ClientToken` `token` named, or
    /// `None` when no modification has been made under it.
    pub fn verified_access_trust_provider_modification_for_token(
        &self,
        token: &str,
    ) -> Option<&VerifiedAccessTrustProviderModification> {
        self.verified_access_trust_provider_modification_client_tokens
            .get(token)
    }

    /// Apply `modification` to the trust provider it names as of `at`, record it under
    /// `client_token` when the request carried one, and answer the trust provider as it NOW
    /// stands — `None` when this store holds no trust provider under that id.
    ///
    /// The one way a stored trust provider is modified, so what the modify answers and what a
    /// later `DescribeVerifiedAccessTrustProviders` of the same trust provider reports cannot
    /// differ: the view it hands back is the stored record itself. A member the modification
    /// leaves `None` is one the request did not carry, so the stored value stays as it was; a
    /// member it carries replaces that value ([`VerifiedAccessTrustProviderModification::
    /// applied_to`]). Every instance that has this trust provider ATTACHED is brought back in
    /// step with it ([`Self::resync_verified_access_trust_provider_attachments`]), because the
    /// entry an instance carries is a copy of facts this record owns.
    ///
    /// The caller-chosen bytes are re-charged across the write — the record this replaces is
    /// refunded and the record it becomes is charged — so a caller cannot grow the store past
    /// [`MAX_STORED_BYTES`] by modifying one trust provider repeatedly; a handler asks
    /// [`Self::would_exceed_stored_bytes_after_refund`] about that net change first.
    pub fn modify_verified_access_trust_provider(
        &mut self,
        modification: &VerifiedAccessTrustProviderModification,
        client_token: Option<&str>,
        at: SystemTime,
    ) -> Option<crate::ops::types::VerifiedAccessTrustProvider> {
        let held = self
            .verified_access_trust_providers
            .get(&modification.verified_access_trust_provider_id)?;
        let refund = verified_access_trust_provider_payload_bytes(held, None);
        let mut view = modification.applied_to(held);
        // "The last updated time" (`VerifiedAccessTrustProvider$LastUpdatedTime`): the instant
        // this modification was applied to the trust provider.
        view.last_updated_time = Some(iso8601_utc(at));
        let charge = verified_access_trust_provider_payload_bytes(&view, None);
        self.verified_access_trust_providers.insert(
            modification.verified_access_trust_provider_id.clone(),
            view.clone(),
        );
        self.release_stored_bytes(refund);
        self.add_stored_bytes(charge);
        if let Some(token) = client_token {
            self.add_stored_bytes(
                verified_access_trust_provider_modification_payload_bytes(modification, token),
            );
            self.verified_access_trust_provider_modification_client_tokens
                .insert(token.to_string(), modification.clone());
        }
        self.resync_verified_access_trust_provider_attachments(
            &modification.verified_access_trust_provider_id,
            at,
        );
        Some(view)
    }
}
