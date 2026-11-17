// SPDX-License-Identifier: CC0-1.0

//! Whether a cloud connector can actually reach the third-party cloud environment it
//! targets: the cross-cloud federation its IAM role has to be able to obtain, and the
//! findings a validation of one connector reports.
//!
//! A cloud connector federates INTO the third-party cloud: Systems Manager takes the role
//! `RoleArn` names and exchanges an Amazon Web Services web identity token for the
//! provider's own credential. That exchange is a call the role has to be permitted to make
//! ([`WEB_IDENTITY_TOKEN_ACTION`]), so a role whose permissions withhold it can obtain no
//! token and the connector can federate nothing — which is why the create resolves the
//! question BEFORE it mints a connector ([`federation_failure`]) and why
//! `ValidateCloudConnector` reports it as a finding of the connector it is asked about
//! ([`findings`]).
//!
//! Both the create and the validation ask the SAME questions of the same records — which
//! role of this account the value names and what its trust policy admits
//! ([`super::activation_role`]), then what IAM answers about that role's permissions
//! ([`::service_common::iam_role::role_withholds_action`]) — so a connector the create
//! admitted validates clean, and one whose role lost the permission after it was minted
//! validates with the finding that says so. A refusal and a finding are therefore both
//! grounded in what IAM ANSWERED: an emulator with no IAM mounted to ask refuses nothing
//! and reports no such finding.

use crate::ops::types::{ValidationFinding, ValidationFindingScope};

/// The action a cloud connector's role has to be permitted for the federation step: STS
/// "returns a signed JSON Web Token (JWT) that represents the calling identity" through
/// `GetWebIdentityToken`, which is the token the connector exchanges with the third-party
/// cloud.
pub const WEB_IDENTITY_TOKEN_ACTION: &str = "sts:GetWebIdentityToken";

/// The resource that authorization question is asked about: the token is vended against the
/// CALLING identity rather than against a resource of its own, so a grant of it is a grant
/// over every resource.
pub const WEB_IDENTITY_TOKEN_RESOURCE: &str = "*";

/// How a request whose role cannot obtain that token is refused.
///
/// The whole sentence is FIXED text for this condition: it names the step that failed and
/// what to check, and interpolates nothing from the request — neither the role nor the
/// connector appears in it.
pub const WEB_IDENTITY_TOKEN_UNAVAILABLE: &str =
    "Unable to obtain an AWS web identity token for cross-cloud federation. Please verify \
     the cloud connector's AWS IAM role is permitted to call sts:GetWebIdentityToken.";

/// Whether IAM answered that the role `role_arn` names may not call
/// [`WEB_IDENTITY_TOKEN_ACTION`] — the one reading both the create's refusal and the
/// validation's finding rest on.
pub async fn web_identity_token_is_unobtainable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    ::service_common::iam_role::role_withholds_action(
        ctx,
        region,
        role_arn,
        WEB_IDENTITY_TOKEN_ACTION,
        WEB_IDENTITY_TOKEN_RESOURCE,
    )
    .await
}

/// The `ValidationFindingType` of a finding that reports a check that PASSED: one of the
/// modelled enum's values (`crate::ops::types::VALIDATION_FINDING_TYPE_VALUES`).
pub const FINDING_TYPE_INFO: &str = "INFO";

/// The `ValidationFindingType` of a finding that reports a check that FAILED, and so a
/// connector that cannot do what it was created for. The other end of the same enum.
pub const FINDING_TYPE_ERROR: &str = "ERROR";

/// The `ValidationFindingCode` of the finding that reports the tenant a connector targets
/// (`crate::ops::types::VALIDATION_FINDING_CODE_VALUES`).
pub const FINDING_CODE_TENANT_SUMMARY: &str = "TenantSummary";

/// The `ValidationFindingCode` of the finding that reports a targeted subscription the
/// connector can reach.
pub const FINDING_CODE_SUBSCRIPTION_ACCESSIBLE: &str = "SubscriptionAccessible";

/// The `ValidationFindingCode` of the finding that reports a target the connector cannot
/// reach, which is every target it holds while its federation fails.
pub const FINDING_CODE_TARGET_INACCESSIBLE: &str = "TargetInaccessible";

/// The `ValidationFindingCode` of the finding that reports a role Systems Manager cannot
/// assume at all.
pub const FINDING_CODE_ROLE_ASSUMPTION_FAILED: &str = "AwsRoleAssumptionFailed";

/// The `ValidationFindingCode` of the finding that reports a role that cannot obtain the
/// web identity token the federation step needs.
pub const FINDING_CODE_WEB_IDENTITY_TOKEN_FAILED: &str = "WebIdentityTokenFailed";

/// The `ValidationFindingScopeType` of a finding about the Azure TENANT a connector targets
/// (`crate::ops::types::VALIDATION_FINDING_SCOPE_TYPE_VALUES`).
pub const FINDING_SCOPE_TENANT: &str = "azure:tenant";

/// The `ValidationFindingScopeType` of a finding about ONE Azure subscription the connector
/// targets. The other value of the same enum.
pub const FINDING_SCOPE_SUBSCRIPTION: &str = "azure:subscription";

/// What a validation of a connector whose federation holds reports about its tenant: the
/// application the connector acts as in that tenant, which is what the configuration names.
pub fn tenant_summary_message(application_id: &str) -> String {
    format!("The cloud connector targets this tenant as application {application_id}")
}

/// What a validation reports about a subscription the connector targets and can reach.
pub const SUBSCRIPTION_ACCESSIBLE_MESSAGE: &str =
    "The cloud connector's federated credential reaches this subscription";

/// What a validation reports about a target the connector holds while its federation
/// fails: the target is not reachable, because nothing the connector could present to the
/// provider can be obtained.
pub const TARGET_INACCESSIBLE_MESSAGE: &str =
    "The cloud connector cannot reach this target while its cross-cloud federation fails";

/// Why the federation of ONE connector fails, as a validation reports it: which step of it
/// IAM answered about.
///
/// `None` — nothing failed — is the connector whose role IAM admits and permits, and also
/// the connector no IAM answered about at all: a finding, like a refusal, is grounded in
/// what IAM said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FederationFailure {
    /// IAM answered that this service cannot assume the connector's role: the account
    /// holds no such role, or its trust policy withholds the assume.
    RoleAssumption,
    /// IAM answered that the role's permissions withhold
    /// [`WEB_IDENTITY_TOKEN_ACTION`], so the federation step can obtain no token.
    WebIdentityToken,
}

impl FederationFailure {
    /// The `ValidationFindingCode` this failure is reported under.
    pub fn code(self) -> &'static str {
        match self {
            Self::RoleAssumption => FINDING_CODE_ROLE_ASSUMPTION_FAILED,
            Self::WebIdentityToken => FINDING_CODE_WEB_IDENTITY_TOKEN_FAILED,
        }
    }

    /// The sentence that finding carries: for the federation step, the sentence a create is
    /// refused with for the same answer, so a client reads one wording for one condition.
    pub fn message(self, role_arn: &str) -> String {
        match self {
            Self::RoleAssumption => super::activation_role::unassumable_role_message(role_arn),
            Self::WebIdentityToken => WEB_IDENTITY_TOKEN_UNAVAILABLE.to_string(),
        }
    }
}

/// Why the federation of the connector that acts under `role_arn` fails, or `None` when
/// nothing IAM answered says it does.
///
/// The two steps are asked in the order the federation takes them: this service assumes the
/// role first ([`super::activation_role::arn_refusal`]), and the assumed role then obtains
/// the web identity token the provider's credential is exchanged for
/// ([`web_identity_token_is_unobtainable`]). A create resolves this BEFORE it mints a
/// connector and refuses with the failing step's own sentence; a validation reports it as a
/// finding of the connector it is asked about.
pub async fn federation_failure(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> Option<FederationFailure> {
    if super::activation_role::arn_refusal(ctx, region, role_arn)
        .await
        .is_some()
    {
        return Some(FederationFailure::RoleAssumption);
    }
    web_identity_token_is_unobtainable(ctx, region, role_arn)
        .await
        .then_some(FederationFailure::WebIdentityToken)
}

/// One finding of `code`, reported at `scope_type`/`scope_id`.
fn finding(
    finding_type: &str,
    code: &str,
    message: String,
    scope_type: &str,
    scope_id: &str,
) -> ValidationFinding {
    ValidationFinding {
        r#type: Some(finding_type.to_string()),
        code: Some(code.to_string()),
        message: Some(message),
        // "A message from the third-party cloud provider related to the validation
        // finding": these findings are what Systems Manager itself established about the
        // connector, so no provider answered beside them.
        provider_message: None,
        scope: Some(ValidationFindingScope {
            r#type: Some(scope_type.to_string()),
            id: Some(scope_id.to_string()),
        }),
    }
}

/// The findings a validation of `record` reports, given what IAM answered about its role
/// (`failure`).
///
/// Every finding is read out of what the connector's own configuration persisted and what
/// IAM answered about the role it holds — the tenant it targets, the application it acts
/// as, the subscriptions it targets — so two validations of one unchanged connector report
/// the same findings and a validation changes none of them.
///
/// The order is total, which is what lets a page resume at the finding it stopped on: the
/// tenant it targets first, then what IAM answered about the role, then its targets in the
/// order the configuration lists them.
pub fn findings(
    record: &super::cloud_connectors::CloudConnectorRecord,
    failure: Option<FederationFailure>,
) -> Vec<ValidationFinding> {
    let tenant_id = record.tenant_id();
    let mut findings = vec![finding(
        FINDING_TYPE_INFO,
        FINDING_CODE_TENANT_SUMMARY,
        tenant_summary_message(record.application_id()),
        FINDING_SCOPE_TENANT,
        tenant_id,
    )];
    if let Some(failure) = failure {
        findings.push(finding(
            FINDING_TYPE_ERROR,
            failure.code(),
            failure.message(&record.role_arn),
            FINDING_SCOPE_TENANT,
            tenant_id,
        ));
    }
    for subscription_id in record.subscription_ids() {
        // A subscription is reachable only through a federated credential, so while the
        // federation fails every target the connector holds is a target it cannot reach.
        let (finding_type, code, message) = match failure {
            None => (
                FINDING_TYPE_INFO,
                FINDING_CODE_SUBSCRIPTION_ACCESSIBLE,
                SUBSCRIPTION_ACCESSIBLE_MESSAGE,
            ),
            Some(_) => (
                FINDING_TYPE_ERROR,
                FINDING_CODE_TARGET_INACCESSIBLE,
                TARGET_INACCESSIBLE_MESSAGE,
            ),
        };
        findings.push(finding(
            finding_type,
            code,
            message.to_string(),
            FINDING_SCOPE_SUBSCRIPTION,
            subscription_id,
        ));
    }
    findings
}

/// The published ceiling of `ValidateCloudConnectorMaxResults` (range 0..75), and the page
/// size a request that names no positive `MaxResults` is served: "The maximum number of
/// validation findings to return" bounds a page the caller asked to be bounded, so a caller
/// asking for no bound is served the whole page the operation allows.
pub const MAX_VALIDATION_FINDINGS_PAGE_SIZE: i32 = 75;

/// The smallest page a `MaxResults` can ask for: one finding. The modelled range reaches
/// down to zero because the member carries the shape default `0`, which is the value a
/// request naming no page size carries rather than a page of no findings.
pub const MIN_VALIDATION_FINDINGS_PAGE_SIZE: i32 = 1;

/// The page size a validation serves, or the refusal message for a `MaxResults` outside the
/// modelled range — which the protocol's boundary validator already refuses, so this is a
/// backstop rather than the first line of defence.
pub fn validation_findings_page_size(max_results: Option<i32>) -> Result<usize, String> {
    match max_results {
        None | Some(0) => Ok(MAX_VALIDATION_FINDINGS_PAGE_SIZE as usize),
        requested => super::page_size(
            requested,
            MIN_VALIDATION_FINDINGS_PAGE_SIZE,
            MAX_VALIDATION_FINDINGS_PAGE_SIZE,
        ),
    }
}
