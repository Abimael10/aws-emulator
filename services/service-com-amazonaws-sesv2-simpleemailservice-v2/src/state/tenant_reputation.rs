// SPDX-License-Identifier: CC0-1.0

//! The records a tenant's reputation entity and its suppression preferences are HELD
//! as, and the values a read reports them under.
//!
//! The rule these records exist for: an optional member of a response reports what
//! something SET, so a preference or a status nothing has ever written is reported as
//! ABSENT rather than as the value its documented default resolves to. A client can
//! then tell "nobody has stated this" from "it was stated, and this is the value" —
//! which an answer that materialises the default makes indistinguishable. So the
//! store keeps `None` for a record no request and no service decision has written,
//! and the value in force is derived where it is needed instead of being written into
//! the record on creation.
//!
//! Split out of the state module root so one resource plane's records live together;
//! every item is re-exported from [`crate::state`], so a handler still reaches it as
//! `crate::state::…`.

use super::*;

/// The prefix a `TenantId` carries: SES hands a tenant's id back as `tn-` followed by
/// [`TENANT_ID_HEX_DIGITS`] lowercase hexadecimal characters.
pub const TENANT_ID_PREFIX: &str = "tn-";

/// How many hexadecimal characters follow [`TENANT_ID_PREFIX`] in a `TenantId`.
pub const TENANT_ID_HEX_DIGITS: usize = 30;

/// The `ReputationImpact` of an entity no reputation finding is active against.
///
/// `ReputationEntity$ReputationImpact` is "the reputation impact level for this
/// entity, representing the highest impact reputation finding currently active", and
/// an entity with no active finding has no impact level, which SES reports as this
/// value rather than by leaving the member out.
pub const REPUTATION_IMPACT_NONE: &str = "NONE";

/// The `StatusCause` a customer-managed status record carries when a caller set the
/// status itself.
///
/// `StatusRecord$Cause` is "a description of the reason for the current status, such
/// as automated actions taken due to reputation findings or manual status updates";
/// `UpdateReputationEntityCustomerManagedStatus` is the manual update, so the record
/// it writes states that as its cause.
pub const CUSTOMER_MANAGED_STATUS_CAUSE: &str = "Status manually updated.";

/// One status record of a reputation entity: the sending status whoever wrote it left
/// the entity in, why they wrote it, and when.
///
/// An entity carries two of these — the account's own and Amazon SES's — and each is
/// held only once its owner has written it (see the module header).
#[derive(Debug, Clone)]
pub struct ReputationStatusRecord {
    /// `StatusRecord$Status`: the `SendingStatus` this record states.
    pub status: String,
    /// `StatusRecord$Cause`: why the status was written, when there is a reason to
    /// state.
    pub cause: Option<String>,
    /// `StatusRecord$LastUpdatedTimestamp`: when this record was last written.
    pub updated_at: SystemTime,
}

impl ReputationStatusRecord {
    /// The record a caller's own status write leaves: the status it asked for, written
    /// at `now`, caused by that manual update ([`CUSTOMER_MANAGED_STATUS_CAUSE`]).
    pub fn manually_updated(status: String, now: SystemTime) -> Self {
        Self {
            status,
            cause: Some(CUSTOMER_MANAGED_STATUS_CAUSE.to_string()),
            updated_at: now,
        }
    }

    /// The modelled `StatusRecord` view of this record.
    pub fn view(&self) -> crate::ops::types::StatusRecord {
        crate::ops::types::StatusRecord {
            status: Some(self.status.clone()),
            cause: self.cause.clone(),
            last_updated_timestamp: Some(::aws_smithy_types::DateTime::from(self.updated_at)),
        }
    }
}

/// The suppression list preferences a caller STATED for one tenant.
///
/// Held only once a request has stated them: a tenant nothing has stated preferences
/// for uses the account-level list, which
/// `TenantSuppressionAttributes$SuppressionScope` documents as the default, so there
/// is nothing of the tenant's own to report (see the module header).
#[derive(Debug, Clone)]
pub struct TenantSuppressionPreferences {
    /// `TenantSuppressionAttributes$SuppressedReasons`: the reasons an address is
    /// added to this tenant's suppression list, in the order supplied.
    pub suppressed_reasons: Vec<String>,
    /// `TenantSuppressionAttributes$SuppressionScope`, resolved to its documented
    /// default when the request that stated the preferences named no scope.
    pub suppression_scope: String,
}

impl TenantSuppressionPreferences {
    /// The modelled `TenantSuppressionAttributes` view of these preferences.
    pub fn view(&self) -> crate::ops::types::TenantSuppressionAttributes {
        crate::ops::types::TenantSuppressionAttributes {
            // A tenant that adds nothing automatically has no reason set at all,
            // rather than an empty one.
            suppressed_reasons: (!self.suppressed_reasons.is_empty())
                .then(|| self.suppressed_reasons.clone()),
            suppression_scope: Some(self.suppression_scope.clone()),
        }
    }
}

/// The `ReputationImpact` of the reputation entity of the resource `resource_arn`,
/// over the account's reputation findings `findings`.
///
/// `ReputationEntity$ReputationImpact` is "the highest impact reputation finding
/// currently active" for the entity, and states where those findings are read:
/// "reputation findings can be retrieved using the `ListRecommendations` operation".
/// So the level is the highest `RecommendationImpact` among the findings this account
/// holds that are about this resource and are still open, and
/// [`REPUTATION_IMPACT_NONE`] while none is — never a level nothing raised.
pub fn reputation_impact(
    findings: &[crate::ops::types::Recommendation],
    resource_arn: &str,
) -> String {
    let active: Vec<&str> = findings
        .iter()
        .filter(|finding| {
            finding.resource_arn.as_deref() == Some(resource_arn)
                && finding.status.as_deref() == Some(RECOMMENDATION_STATUS_OPEN)
        })
        .filter_map(|finding| finding.impact.as_deref())
        .collect();
    // `RECOMMENDATION_IMPACTS` lists the levels in the order the enum does, lowest
    // impact first, so the highest active one is the last of them any finding carries.
    RECOMMENDATION_IMPACTS
        .iter()
        .rev()
        .find(|level| active.contains(level))
        .map_or(REPUTATION_IMPACT_NONE, |level| *level)
        .to_string()
}

/// The `ResourceType` token a tenant's resource listing reports for the resource
/// `resource_arn` names, or `None` when the ARN names no kind of resource a tenant may
/// hold.
///
/// SES is RECORDED reporting `TenantResource$ResourceType` as the resource-type token
/// the resource's own ARN carries — `identity` for an associated email identity —
/// rather than as the `ResourceType` enum value of that kind, and the token a client
/// filters or groups the reported rows by is the one it reads, so that is what is
/// reported for each kind a tenant may hold ([`TENANT_RESOURCE_TYPES`]). The enum
/// value stays the one the REQUEST side names, which is what
/// [`tenant_resource_type`] answers.
pub fn reported_tenant_resource_type(resource_arn: &str) -> Option<&'static str> {
    let (resource_type, _) = ses_resource_of(resource_arn)?;
    TENANT_RESOURCE_TYPES
        .iter()
        .find(|(token, _)| resource_type == *token)
        .map(|(token, _)| *token)
}
