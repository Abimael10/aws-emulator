// SPDX-License-Identifier: CC0-1.0

//! The account's DEVICE DEFENDER AUDIT SUPPRESSIONS: what identifies one, the record one
//! holds, and what every reader of one reports.
//!
//! A suppression "filters out specific findings of a Device Defender audit"
//! (`smithy_query(query="shape",shape="AuditSuppression")`), and WHAT IT IS ABOUT is its
//! identity: every operation of the family addresses one by the PAIR `checkName` +
//! `resourceIdentifier`, both `@required` on the create, the read, the update and the
//! delete alike (`smithy_query(query="operation",operation="DescribeAuditSuppression")`),
//! and the family models no id, name or ARN of its own. So the store is keyed by that pair
//! ([`AuditSuppressionKey`]) rather than by a name a caller chose.
//!
//! `BTreeMap` (not `HashMap`) because `ListAuditSuppressions` is `@paginated` and a page
//! boundary is only meaningful over a stable order — the reason every other record in this
//! store is ordered.
//!
//! One record carries everything a reader hands back, so `DescribeAuditSuppression` after
//! `CreateAuditSuppression` reports the created suppression member for member: a reader
//! clones what the writer stored and the two cannot drift.
//!
//! THE KEY IS THE IDENTIFIER CANONICALISED, not the JSON a request happened to send. A
//! `ResourceIdentifier` is a flat set of optional members plus two nested identifiers, and
//! a nested identifier none of whose members is set names nothing at all — so the key
//! flattens the pair ([`AuditSuppressionKey::of`]) and two requests that identify the same
//! resource address the same suppression however the client spelled the absent parts. An
//! identifier that identifies NO resource is refused ([`AuditSuppressionRefusal`]): it is
//! "information that identifies the noncompliant resource", so nothing is what it cannot
//! be the identity of.
//!
//! Which CHECK a suppression may be created for is the catalog of audit checks
//! ([`super::AUDIT_CHECK_NAMES`]) — `checkName` is "an audit check name" — and a name
//! outside it is refused in the one wording this crate states an unknown check in
//! ([`super::AuditConfigurationRefusal::UnknownCheck`]), so the suppression family and the
//! audit configuration cannot disagree about what an audit check is. Whether the account
//! has ENABLED that check is deliberately NOT a condition here, unlike the checks an audit
//! TASK performs ([`super::audit_tasks::targetable_checks`]): a suppression records that a
//! finding is to be filtered out whenever the check runs, so it outlives any one setting of
//! that check.
//!
//! An `expirationDate` is kept exactly as the writer supplied it — "the epoch timestamp in
//! seconds at which this suppression expires" — and no reader of this store treats a date
//! that has passed as a record that is gone: the model documents no such removal, and the
//! stored suppression is what every later call reports.

use crate::ops::types::{
    DescribeAuditSuppressionOutput, IssuerCertificateIdentifier, PolicyVersionIdentifier,
    ResourceIdentifier,
};

/// How many audit suppressions this emulator keeps for the account.
///
/// AWS publishes a service quota on an account's audit suppressions; the exact figure is
/// not in the model, so this is the emulator's own ceiling on a store a caller can grow —
/// the same reason [`super::MAX_SCHEDULED_AUDITS`] exists — and `CreateAuditSuppression`
/// past it refuses with the modelled `LimitExceededException` rather than growing without
/// bound.
pub const MAX_AUDIT_SUPPRESSIONS: usize = 1000;

/// The input member every operation of this family carries the audit check at —
/// `smithy_query(query="operation",operation="DescribeAuditSuppression")`: `checkName`,
/// `@required` — which is what this family's not-found sentence names.
pub const AUDIT_SUPPRESSION_CHECK_NAME_MEMBER: &str = "checkName";

/// The input member every operation of this family carries the suppressed resource at:
/// `resourceIdentifier`, `@required`, the other half of a suppression's identity.
pub const AUDIT_SUPPRESSION_RESOURCE_MEMBER: &str = "resourceIdentifier";

/// The identity of ONE audit suppression: the audit check it filters a finding of, and the
/// resource that finding is about.
///
/// Every member of the modelled `ResourceIdentifier` is flattened into one orderable key,
/// including the members of its two NESTED identifiers, so that a nested identifier left
/// out entirely and one sent with no member set are the same identity — neither names a
/// resource part. Ordered (`Ord`) because the store is a `BTreeMap`: a `@paginated` listing
/// of suppressions needs a stable order to cut a page on.
#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AuditSuppressionKey {
    /// "An audit check name" — one of [`super::AUDIT_CHECK_NAMES`].
    pub check_name: String,
    /// "The ID of the certificate attached to the resource."
    pub device_certificate_id: Option<String>,
    /// "The ID of the CA certificate used to authorize the certificate."
    pub ca_certificate_id: Option<String>,
    /// "The ID of the Amazon Cognito identity pool."
    pub cognito_identity_pool_id: Option<String>,
    /// "The client ID."
    pub client_id: Option<String>,
    /// `policyVersionIdentifier$policyName` — "the name of the policy".
    pub policy_name: Option<String>,
    /// `policyVersionIdentifier$policyVersionId` — "the ID of the version of the policy
    /// associated with the resource".
    pub policy_version_id: Option<String>,
    /// "The account with which the resource is associated."
    pub account: Option<String>,
    /// "The ARN of the IAM role that has overly permissive actions."
    pub iam_role_arn: Option<String>,
    /// "The ARN of the role alias that has overly permissive actions."
    pub role_alias_arn: Option<String>,
    /// `issuerCertificateIdentifier$issuerCertificateSubject` — "the subject of the issuer
    /// certificate".
    pub issuer_certificate_subject: Option<String>,
    /// `issuerCertificateIdentifier$issuerId` — "the issuer ID".
    pub issuer_id: Option<String>,
    /// `issuerCertificateIdentifier$issuerCertificateSerialNumber` — "the issuer
    /// certificate serial number".
    pub issuer_certificate_serial_number: Option<String>,
    /// "The ARN of the identified device certificate."
    pub device_certificate_arn: Option<String>,
}

impl AuditSuppressionKey {
    /// The identity the pair `check_name` + `resource_identifier` addresses.
    ///
    /// The ONE reading of a suppression's identity: the create, the read and every other
    /// operation that addresses one builds its key here, so they cannot disagree about
    /// which record a request names.
    pub fn of(check_name: &str, resource_identifier: &ResourceIdentifier) -> Self {
        let policy_version = resource_identifier
            .policy_version_identifier
            .clone()
            .unwrap_or_default();
        let issuer = resource_identifier
            .issuer_certificate_identifier
            .clone()
            .unwrap_or_default();
        Self {
            check_name: check_name.to_string(),
            device_certificate_id: resource_identifier.device_certificate_id.clone(),
            ca_certificate_id: resource_identifier.ca_certificate_id.clone(),
            cognito_identity_pool_id: resource_identifier.cognito_identity_pool_id.clone(),
            client_id: resource_identifier.client_id.clone(),
            policy_name: policy_version.policy_name,
            policy_version_id: policy_version.policy_version_id,
            account: resource_identifier.account.clone(),
            iam_role_arn: resource_identifier.iam_role_arn.clone(),
            role_alias_arn: resource_identifier.role_alias_arn.clone(),
            issuer_certificate_subject: issuer.issuer_certificate_subject,
            issuer_id: issuer.issuer_id,
            issuer_certificate_serial_number: issuer.issuer_certificate_serial_number,
            device_certificate_arn: resource_identifier.device_certificate_arn.clone(),
        }
    }

    /// Does this key identify a resource at all — is any member of the identifier set?
    ///
    /// A `ResourceIdentifier` is "information that identifies the noncompliant resource",
    /// so one carrying no member identifies nothing and cannot be half of a suppression's
    /// identity.
    pub fn identifies_a_resource(&self) -> bool {
        [
            &self.device_certificate_id,
            &self.ca_certificate_id,
            &self.cognito_identity_pool_id,
            &self.client_id,
            &self.policy_name,
            &self.policy_version_id,
            &self.account,
            &self.iam_role_arn,
            &self.role_alias_arn,
            &self.issuer_certificate_subject,
            &self.issuer_id,
            &self.issuer_certificate_serial_number,
            &self.device_certificate_arn,
        ]
        .into_iter()
        .any(Option::is_some)
    }

    /// The identifier this key flattened, as a reader of the suppression reports it.
    ///
    /// The two NESTED identifiers are reported only when a member of one is set, so a
    /// reader states the parts that identify the resource and claims no empty identifier
    /// the writer never supplied.
    pub fn resource_identifier(&self) -> ResourceIdentifier {
        let policy_version_identifier = (self.policy_name.is_some()
            || self.policy_version_id.is_some())
        .then(|| PolicyVersionIdentifier {
            policy_name: self.policy_name.clone(),
            policy_version_id: self.policy_version_id.clone(),
        });
        let issuer_certificate_identifier = (self.issuer_certificate_subject.is_some()
            || self.issuer_id.is_some()
            || self.issuer_certificate_serial_number.is_some())
        .then(|| IssuerCertificateIdentifier {
            issuer_certificate_subject: self.issuer_certificate_subject.clone(),
            issuer_id: self.issuer_id.clone(),
            issuer_certificate_serial_number: self.issuer_certificate_serial_number.clone(),
        });
        ResourceIdentifier {
            device_certificate_id: self.device_certificate_id.clone(),
            ca_certificate_id: self.ca_certificate_id.clone(),
            cognito_identity_pool_id: self.cognito_identity_pool_id.clone(),
            client_id: self.client_id.clone(),
            policy_version_identifier,
            account: self.account.clone(),
            iam_role_arn: self.iam_role_arn.clone(),
            role_alias_arn: self.role_alias_arn.clone(),
            issuer_certificate_identifier,
            device_certificate_arn: self.device_certificate_arn.clone(),
        }
    }
}

/// Why a request about an audit suppression names no suppression this service can address,
/// mapped to the operation's modelled `InvalidRequestException` ("The request is not
/// valid", HTTP 400) by the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditSuppressionRefusal {
    /// A `checkName` that names no audit check ([`super::AUDIT_CHECK_NAMES`]).
    UnknownCheck(String),
    /// A `resourceIdentifier` that identifies no resource at all — no member of it is set.
    UnidentifiedResource,
}

impl AuditSuppressionRefusal {
    /// The message a handler refuses the request with: the member and the rule it broke, so
    /// the caller can fix the request from the text alone.
    pub fn message(&self) -> String {
        match self {
            // A name outside the catalog is ONE rule however it arrives, so it is worded
            // where the catalog lives and not a second time here — exactly as an audit
            // task's unknown check is ([`super::AuditTaskRefusal::UnknownCheck`]).
            Self::UnknownCheck(check_name) => {
                super::AuditConfigurationRefusal::UnknownCheck(check_name.clone()).message()
            }
            Self::UnidentifiedResource => {
                format!("{AUDIT_SUPPRESSION_RESOURCE_MEMBER} must identify a resource")
            }
        }
    }
}

/// The identity the pair a request carried addresses, or why it addresses none.
///
/// Both halves are read here — the check is one of the account's audit checks, and the
/// identifier identifies a resource — so every operation of the family refuses the same
/// request the same way, and a refused request reaches no record.
pub fn audit_suppression_key(
    check_name: &str,
    resource_identifier: &ResourceIdentifier,
) -> Result<AuditSuppressionKey, AuditSuppressionRefusal> {
    if !super::is_audit_check(check_name) {
        return Err(AuditSuppressionRefusal::UnknownCheck(
            check_name.to_string(),
        ));
    }
    let key = AuditSuppressionKey::of(check_name, resource_identifier);
    if !key.identifies_a_resource() {
        return Err(AuditSuppressionRefusal::UnidentifiedResource);
    }
    Ok(key)
}

/// How this service states that the pair a request named is no audit suppression this
/// account holds.
///
/// A suppression is a DEVICE DEFENDER configuration resource, so the miss is stated in that
/// family's sentence ([`super::unresolved_resource_message`]): the fixed word `Resource`,
/// the input member the request carried the resource's name at, and ` was not found`, with
/// neither the account nor a terminating period. The member it names is the one half of
/// this family's identity that IS a name — the audit check — since the other half is a
/// structure rather than a name a sentence can state.
pub fn missing_audit_suppression_message(check_name: &str) -> String {
    super::unresolved_resource_message(AUDIT_SUPPRESSION_CHECK_NAME_MEMBER, check_name)
}

/// One audit suppression in the store, as this service persists it.
///
/// It holds what `DescribeAuditSuppression` reports member for member — the pair that
/// identifies it, how long it lasts and what it is for — plus the `clientRequestToken` the
/// create was made with, which the family documents as unique across an account's
/// suppressions. A reader clones this record, so the writer and the reader cannot drift.
#[derive(Debug, Clone)]
pub struct RegisteredAuditSuppression {
    /// The identity the create addressed; also this record's key in the store.
    pub key: AuditSuppressionKey,
    /// "The epoch timestamp in seconds at which this suppression expires", as the writer
    /// supplied it.
    pub expiration_date: Option<::aws_smithy_types::DateTime>,
    /// "Indicates whether a suppression should exist indefinitely or not."
    pub suppress_indefinitely: Option<bool>,
    /// "The description of the audit suppression."
    pub description: Option<String>,
    /// The `clientRequestToken` the create carried: "each audit supression must have a
    /// unique client request token", so the token a suppression was created with is part of
    /// the record and a second create under it is a conflict.
    pub client_request_token: String,
}

impl RegisteredAuditSuppression {
    /// A newly created suppression: the identity it addresses, the span it lasts for and
    /// the token it was created under.
    pub fn new(
        key: AuditSuppressionKey,
        expiration_date: Option<::aws_smithy_types::DateTime>,
        suppress_indefinitely: Option<bool>,
        description: Option<String>,
        client_request_token: String,
    ) -> Self {
        Self {
            key,
            expiration_date,
            suppress_indefinitely,
            description,
            client_request_token,
        }
    }

    /// The record as `DescribeAuditSuppression` reports it — every member of the stored
    /// record, so the read and the write cannot drift.
    pub fn to_description(&self) -> DescribeAuditSuppressionOutput {
        DescribeAuditSuppressionOutput {
            check_name: Some(self.key.check_name.clone()),
            resource_identifier: Some(self.key.resource_identifier()),
            expiration_date: self.expiration_date,
            suppress_indefinitely: self.suppress_indefinitely,
            description: self.description.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One audit check of the catalog, read through it so the case cannot name a check the
    /// service does not have.
    fn a_check() -> &'static str {
        crate::state::AUDIT_CHECK_NAMES[0]
    }

    /// An identifier naming one device certificate.
    fn by_certificate(device_certificate_id: &str) -> ResourceIdentifier {
        ResourceIdentifier {
            device_certificate_id: Some(device_certificate_id.to_string()),
            ..ResourceIdentifier::default()
        }
    }

    /// Two requests that identify the same resource address one suppression, whichever way
    /// the client spelled the parts it does not name: a nested identifier left out and one
    /// sent empty are the same identity.
    #[test]
    fn an_empty_nested_identifier_is_the_identity_without_it() {
        let without = by_certificate("abc");
        let with_empty = ResourceIdentifier {
            policy_version_identifier: Some(PolicyVersionIdentifier::default()),
            issuer_certificate_identifier: Some(IssuerCertificateIdentifier::default()),
            ..by_certificate("abc")
        };
        let left = AuditSuppressionKey::of(a_check(), &without);
        let right = AuditSuppressionKey::of(a_check(), &with_empty);
        assert_eq!(
            left, right,
            "case empty nested identifiers: expected one identity, got {left:?} and {right:?}"
        );
    }

    /// An identifier that names a part of a nested identifier identifies a resource, and the
    /// key reports that identifier back with only the nested parts the writer named.
    #[test]
    fn a_nested_identifier_round_trips_through_the_key() {
        let identifier = ResourceIdentifier {
            policy_version_identifier: Some(PolicyVersionIdentifier {
                policy_name: Some("a-policy".to_string()),
                policy_version_id: Some("2".to_string()),
            }),
            ..ResourceIdentifier::default()
        };
        let key = AuditSuppressionKey::of(a_check(), &identifier);
        assert!(
            key.identifies_a_resource(),
            "case policy version: expected an identified resource, got {key:?}"
        );
        let reported = key.resource_identifier();
        let policy_version = reported
            .policy_version_identifier
            .expect("case policy version: the reported identifier names the policy version");
        assert_eq!(
            (
                policy_version.policy_name.as_deref(),
                policy_version.policy_version_id.as_deref()
            ),
            (Some("a-policy"), Some("2")),
            "case policy version: expected the stored policy version, got {policy_version:?}"
        );
        assert!(
            reported.issuer_certificate_identifier.is_none(),
            "case policy version: expected no issuer identifier, got {:?}",
            reported.issuer_certificate_identifier
        );
    }

    /// An identifier with no member at all identifies nothing, and the pair it is half of is
    /// refused rather than keyed.
    #[test]
    fn an_identifier_naming_nothing_is_refused() {
        let refusal = audit_suppression_key(a_check(), &ResourceIdentifier::default());
        assert_eq!(
            refusal,
            Err(AuditSuppressionRefusal::UnidentifiedResource),
            "case empty identifier: got {refusal:?}"
        );
    }

    /// A `checkName` outside the catalog of audit checks is refused, in the one wording this
    /// crate states an unknown check in.
    #[test]
    fn a_check_outside_the_catalog_is_refused() {
        let refusal = audit_suppression_key("NOT_AN_AUDIT_CHECK", &by_certificate("abc"));
        assert_eq!(
            refusal,
            Err(AuditSuppressionRefusal::UnknownCheck(
                "NOT_AN_AUDIT_CHECK".to_string()
            )),
            "case unknown check: got {refusal:?}"
        );
        assert_eq!(
            AuditSuppressionRefusal::UnknownCheck("NOT_AN_AUDIT_CHECK".to_string()).message(),
            crate::state::AuditConfigurationRefusal::UnknownCheck("NOT_AN_AUDIT_CHECK".to_string())
                .message(),
            "case unknown check: expected the crate's one unknown-check sentence"
        );
    }

    /// Every audit check of the catalog is a check a suppression can be created for: the
    /// family's own condition on `checkName` is the catalog, not the account's settings.
    #[test]
    fn every_catalog_check_can_be_suppressed() {
        for check_name in crate::state::AUDIT_CHECK_NAMES {
            let key = audit_suppression_key(check_name, &by_certificate("abc"));
            assert!(
                key.is_ok(),
                "case {check_name}: expected a key, got {key:?}"
            );
        }
    }
}
