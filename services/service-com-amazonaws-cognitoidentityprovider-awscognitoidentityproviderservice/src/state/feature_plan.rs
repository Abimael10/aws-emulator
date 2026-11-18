// SPDX-License-Identifier: CC0-1.0

//! The user pool FEATURE PLAN boundary: the modelled plans in inclusion order,
//! the sentence a refusal of a plan-gated feature carries, and which
//! configuration asks for which feature.
//!
//! Every operation that configures a plan-gated feature asks the questions here,
//! so the boundary, the feature names and the wording cannot disagree between
//! them.

use super::*;

/// The feature plan managed login is NOT included in
/// (`CreateUserPoolDomain.ManagedLoginVersion`); one of the modelled
/// `UserPoolTierType` values.
pub const LITE_USER_POOL_TIER: &str = "LITE";

/// The feature plan email message MFA needs: `EmailMfaConfigType` is "To
/// activate this setting, your user pool must be in the Essentials tier or
/// higher" (this service's model); one of the modelled
/// [`crate::ops::types::USER_POOL_TIER_TYPE_VALUES`].
pub const ESSENTIALS_USER_POOL_TIER: &str = "ESSENTIALS";

/// The modelled feature plans ([`crate::ops::types::USER_POOL_TIER_TYPE_VALUES`])
/// in INCLUSION order, cheapest first: a plan includes every feature the plans
/// before it include, which is what "the Essentials tier or higher"
/// (`EmailMfaConfigType`) and "the Plus feature plan"
/// (`LogConfigurationType.EventSource`) each name a floor of.
const USER_POOL_TIER_ORDER: [&str; 3] = [
    LITE_USER_POOL_TIER,
    ESSENTIALS_USER_POOL_TIER,
    PLUS_USER_POOL_TIER,
];

/// Whether a pool on `tier` includes a feature whose floor is the plan
/// `minimum`: the plans are ordered by inclusion, so every plan from `minimum`
/// upwards includes it and every plan below it does not. An unmodelled `tier`
/// includes nothing beyond the cheapest plan, so a gated feature is refused
/// rather than granted.
#[must_use]
pub fn user_pool_tier_includes(tier: &str, minimum: &str) -> bool {
    let rank = |plan: &str| {
        USER_POOL_TIER_ORDER
            .iter()
            .position(|known| *known == plan)
            .unwrap_or(0)
    };
    rank(tier) >= rank(minimum)
}

/// The refusal a feature whose floor is the plan `minimum` owes a pool on
/// `tier`, or `None` when that pool's plan includes the feature.
///
/// Every operation that configures a plan-gated feature asks this one question,
/// so the boundary and the sentence it is reported with cannot disagree between
/// them. `feature` is the feature's display name, as
/// [`feature_unavailable_in_tier_message`] renders it.
#[must_use]
pub fn feature_plan_refusal(tier: &str, minimum: &str, feature: &str) -> Option<String> {
    (!user_pool_tier_includes(tier, minimum))
        .then(|| feature_unavailable_in_tier_message(tier, feature))
}

/// The message a `FeatureUnavailableInTierException` carries: a tier refusal
/// reports the pool's configured feature plan and the feature that plan does not
/// include, in one template, so a caller can parse the feature out of it and
/// tell its operator what to upgrade.
///
/// `tier` is the pool's stored `UserPoolTier` (a `UserPoolTierType` value) and
/// `feature` names the feature the request asked for.
pub fn feature_unavailable_in_tier_message(tier: &str, feature: &str) -> String {
    format!("The following feature is not available for the {tier} pricing tier configured: {feature}")
}

/// The message a `FeatureUnavailableInTierException` carries for a pool whose
/// OWN configuration asks for a feature its feature plan excludes: the request
/// sets the plan and the feature TOGETHER, so the refusal reports the features
/// that have to be off for the plan it configured rather than a feature that is
/// unavailable to configure.
///
/// `tier` is the plan the pool is left on and `feature` the feature's display
/// name, as [`feature_unavailable_in_tier_message`] renders it for the
/// operations that configure a feature on a pool whose plan is already set.
pub fn features_need_disabling_message(tier: &str, feature: &str) -> String {
    format!(
        "The following features need to be disabled for the {tier} pricing tier configured: \
         {feature}"
    )
}

/// The feature a tier refusal names for user pool email message MFA: the
/// `EmailMfaConfiguration` member of `SetUserPoolMfaConfig` configures "user
/// pool email message MFA and sign-in with one-time passwords (OTPs)"
/// (`EmailMfaConfigType`), reported through
/// [`feature_unavailable_in_tier_message`] the way every other tier refusal is.
pub const EMAIL_MFA_FEATURE: &str = "Email MFA";

/// The feature name a tier refusal gives for a `ManagedLoginVersion` of
/// `version`: the branding version is part of the feature asked for, since only
/// managed login (version 2) needs a feature plan other than
/// [`LITE_USER_POOL_TIER`].
pub fn managed_login_feature_name(version: i32) -> String {
    format!("Managed Login Version {version}")
}

/// The feature a tier refusal names for choice-based (selection-based) sign-in:
/// the pool's `SignInPolicy` and the app client's `ALLOW_USER_AUTH` flow are the
/// two halves of ONE feature, and each documents the same floor — "To activate
/// this setting, your user pool must be in the Essentials tier or higher"
/// (`SignInPolicyType`, and `ALLOW_USER_AUTH` under
/// `CreateUserPoolClient.ExplicitAuthFlows`) — so both halves are refused under
/// this one name.
pub const CHOICE_BASED_SIGN_IN_FEATURE: &str = "Choice Based Authentication";

/// The feature a tier refusal names for threat protection: `UserPoolAddOnsType`
/// carries "settings for activation of threat protection" and "to activate this
/// setting, your user pool must be on the Plus tier" (this service's model).
pub const THREAT_PROTECTION_FEATURE: &str = "Threat Protection";

/// The feature name a tier refusal gives for a pre-token-generation trigger of
/// `version`: the trigger version is part of the feature asked for, since
/// "higher-numbered versions add fields that support new features"
/// (`PreTokenGenerationVersionConfigType.LambdaVersion`) and the base version
/// ([`lambda_triggers::PRE_TOKEN_GENERATION_V1`]) asks for none of them, exactly
/// as a branding version is part of [`managed_login_feature_name`].
pub fn token_customization_feature_name(version: &str) -> String {
    format!("Token Customization with Pre-Token Generation Lambda {version}")
}

/// The pre-token-generation trigger version a pool's `LambdaConfig` asks for
/// token customization with, or `None` when it asks for none.
///
/// `PreTokenGenerationVersionConfigType.LambdaVersion` is "the user pool trigger
/// version of the request that Amazon Cognito sends to your Lambda function.
/// Higher-numbered versions add fields that support new features" (this
/// service's model), and the base version
/// ([`lambda_triggers::PRE_TOKEN_GENERATION_V1`]) is the one the legacy
/// `PreTokenGeneration` member predates — the fields that customize a token
/// beyond it arrive with every HIGHER version, whichever of the modelled ones
/// (`PreTokenGenerationLambdaVersionType`) the request names. So what is read is
/// the VERSION the configuration asks for, not whether the member is present:
/// the base version, the legacy member and no trigger at all each ask for
/// nothing gated.
#[must_use]
pub fn configures_token_customization(lambda_config: Option<&LambdaConfigType>) -> Option<&str> {
    lambda_config
        .and_then(|config| config.pre_token_generation_config.as_ref())
        .map(|config| config.lambda_version.as_str())
        .filter(|version| *version != lambda_triggers::PRE_TOKEN_GENERATION_V1)
}

/// Whether `policies` configures choice-based sign-in — a first authentication
/// factor BEYOND username and password.
///
/// `SignInPolicyType.AllowedFirstAuthFactors` is "the sign-in methods that a
/// user pool supports as the first factor. You can permit users to start
/// authentication with a standard username and password, or with other one-time
/// password and hardware factors" (this service's model), and a pool that
/// configures no policy admits [`PASSWORD_AUTH_FACTOR`] alone
/// ([`effective_policies`]) — so a policy naming only that factor asks for
/// nothing beyond what every plan serves, while any other factor is the gated
/// feature. Both the create and the update path ask this one function, so the
/// two cannot disagree about which configuration needs which plan.
#[must_use]
pub fn configures_choice_based_sign_in(policies: Option<&UserPoolPolicyType>) -> bool {
    policies
        .and_then(|policies| policies.sign_in_policy.as_ref())
        .and_then(|policy| policy.allowed_first_auth_factors.as_ref())
        .is_some_and(|factors| factors.iter().any(|factor| factor != PASSWORD_AUTH_FACTOR))
}

/// Whether `flows` configures choice-based sign-in on an app client: the
/// `ALLOW_USER_AUTH` flow "covers username-password, secure remote password
/// (SRP), passwordless, and passkey authentication" and names the same floor
/// ([`CHOICE_BASED_SIGN_IN_FEATURE`]). Asked by the create and the update path
/// alike.
#[must_use]
pub fn configures_user_auth_flow(flows: &[String]) -> bool {
    flows
        .iter()
        .any(|flow| USER_AUTH_FLOWS.contains(&flow.as_str()))
}

/// The refusal an APP CLIENT configuration owes the feature plan of the pool it
/// belongs to, or `None` when that plan includes every feature the
/// configuration asks for.
///
/// An app-client member whose own documentation names a feature plan is gated on
/// it: `ALLOW_USER_AUTH` documents "the Essentials tier or higher"
/// ([`CHOICE_BASED_SIGN_IN_FEATURE`]), which is the only such member of an app
/// client's configuration (this service's model). The plan is the POOL's stored
/// one, which the client request does not set, so the refusal reports the
/// feature as unavailable ([`feature_unavailable_in_tier_message`]) rather than
/// as one to disable. Both the create and the update path ask this one function.
#[must_use]
pub fn client_feature_plan_refusal(tier: &str, explicit_auth_flows: Option<&[String]>) -> Option<String> {
    if explicit_auth_flows.is_some_and(configures_user_auth_flow) {
        if let Some(message) = feature_plan_refusal(
            tier,
            ESSENTIALS_USER_POOL_TIER,
            CHOICE_BASED_SIGN_IN_FEATURE,
        ) {
            return Some(message);
        }
    }
    None
}

/// The refusal a USER POOL configuration owes the feature plan it is on, or
/// `None` when that plan includes every feature the configuration asks for.
///
/// A member whose own documentation names a feature plan is gated on it, and
/// each one states its own floor: `SignInPolicyType` ("the Essentials tier or
/// higher"), `LambdaConfigType.PreTokenGenerationConfig` (token customization,
/// one of the features `UserPoolTier` "determines the eligibility of the user
/// pool for") and `UserPoolAddOnsType` ("the Plus tier") are the such members
/// of a pool's configuration (this service's model). What each member asks FOR
/// is what the gate reads, not whether the member is present: `UserPoolAddOns`
/// asks for the gated feature in the mode that acts on traffic
/// ([`ADVANCED_SECURITY_MODE_ENFORCED`]) and for neither logging-only threat
/// protection ([`ADVANCED_SECURITY_MODE_OFF`] and `AUDIT`, which "take[s] no
/// action") nor a `SignInPolicy` naming only [`PASSWORD_AUTH_FACTOR`], which
/// configures no further factor, nor a pre-token-generation trigger on the base
/// version ([`configures_token_customization`]).
///
/// The plan the pool is CONFIGURED on is what decides: an update is judged
/// against the plan it leaves the pool on, and a create against the plan its own
/// request names — a create that names none configures no plan and is put on one
/// that includes its features instead
/// ([`effective_user_pool_tier_for_configuration`]). Both paths ask this one
/// function, so neither can gate a feature the other serves.
#[must_use]
pub fn pool_feature_plan_refusal(
    tier: &str,
    policies: Option<&UserPoolPolicyType>,
    add_ons: Option<&UserPoolAddOnsType>,
    lambda_config: Option<&LambdaConfigType>,
) -> Option<String> {
    pool_configuration_feature_floors(policies, add_ons, lambda_config)
        .into_iter()
        .find(|(floor, _)| !user_pool_tier_includes(tier, floor))
        .map(|(_, feature)| features_need_disabling_message(tier, &feature))
}

/// Every plan-gated feature a pool configuration ASKS FOR, each with the plan
/// floor its own documentation names and the name a refusal reports it under.
///
/// The gated members of a pool's configuration are `SignInPolicyType` ("the
/// Essentials tier or higher"), `LambdaConfigType.PreTokenGenerationConfig`
/// (token customization, which `UserPoolTier` "determines the eligibility of the
/// user pool for") and `UserPoolAddOnsType` ("the Plus tier") (this service's
/// model), and what each asks FOR is what counts — not whether the member is
/// present. Both the gate ([`pool_feature_plan_refusal`]) and the plan a create
/// that names none is put on ([`effective_user_pool_tier_for_configuration`])
/// read this one table, so the plan a pool is given can never exclude a feature
/// the same configuration is refused for.
#[must_use]
fn pool_configuration_feature_floors(
    policies: Option<&UserPoolPolicyType>,
    add_ons: Option<&UserPoolAddOnsType>,
    lambda_config: Option<&LambdaConfigType>,
) -> Vec<(&'static str, String)> {
    let mut floors = Vec::new();
    if configures_choice_based_sign_in(policies) {
        floors.push((
            ESSENTIALS_USER_POOL_TIER,
            CHOICE_BASED_SIGN_IN_FEATURE.to_string(),
        ));
    }
    // Token customization is "access-token customization", one of the features
    // `UserPoolTier` "determines the eligibility of the user pool for"
    // (`CreateUserPool.UserPoolTier`), and the fields that customize a token
    // arrive with every pre-token-generation trigger version above the base one,
    // so the version asked for is what the refusal names.
    if let Some(version) = configures_token_customization(lambda_config) {
        floors.push((
            ESSENTIALS_USER_POOL_TIER,
            token_customization_feature_name(version),
        ));
    }
    if add_ons.is_some_and(|add_ons| add_ons.advanced_security_mode == ADVANCED_SECURITY_MODE_ENFORCED)
    {
        floors.push((PLUS_USER_POOL_TIER, THREAT_PROTECTION_FEATURE.to_string()));
    }
    floors
}

/// The feature plan an UPDATE leaves a pool on: the plan the request names, or
/// the plan the pool already holds when the request names none.
///
/// A pool's feature plan is what it is BILLED and gated on, and the model gives
/// a change of it a refusal of its own — `TierChangeNotAllowedException` is
/// "thrown when you've attempted to change your feature plan but the operation
/// isn't permitted" (this service's model) — so a plan change is something a
/// request ASKS for by naming a plan, and an update that names none asks for no
/// plan change. That is the same shape as the other members a pool keeps across
/// an update that does not mention them (its tags, its name): the plan the pool
/// is left on is the one [`pool_feature_plan_refusal`] judges this update's
/// configuration against, so a request that changes an unrelated member can
/// neither gain nor lose a plan-gated feature.
///
/// `stored` is the plan the addressed pool holds, and `None` when this account
/// holds no such pool — a request the pool lookup answers as the missing pool it
/// is, so what it would have been judged against is the documented
/// [`DEFAULT_USER_POOL_TIER`].
#[must_use]
pub fn effective_user_pool_tier_for_update(requested: Option<String>, stored: Option<&str>) -> String {
    requested.unwrap_or_else(|| stored.unwrap_or(DEFAULT_USER_POOL_TIER).to_string())
}

/// The feature plan a pool whose configuration is `policies`, `add_ons` and
/// `lambda_config` is created on when the request names `requested`.
///
/// A request that names a plan CONFIGURES it, so that plan is the pool's and
/// [`pool_feature_plan_refusal`] judges the configuration against it. A request
/// that names none configures no plan: "`UserPoolTier` ... determines the
/// eligibility of the user pool for features like managed login, access-token
/// customization, and threat protection" (`CreateUserPool.UserPoolTier` in this
/// service's model), so the pool is created on the cheapest plan that includes
/// every feature its configuration asks for — the documented
/// [`DEFAULT_USER_POOL_TIER`] when it asks for none, and the highest floor among
/// the features it does ask for otherwise.
#[must_use]
pub fn effective_user_pool_tier_for_configuration(
    requested: Option<String>,
    policies: Option<&UserPoolPolicyType>,
    add_ons: Option<&UserPoolAddOnsType>,
    lambda_config: Option<&LambdaConfigType>,
) -> String {
    if let Some(requested) = requested {
        return requested;
    }
    pool_configuration_feature_floors(policies, add_ons, lambda_config)
        .into_iter()
        .fold(DEFAULT_USER_POOL_TIER, |plan, (floor, _)| {
            if user_pool_tier_includes(plan, floor) {
                plan
            } else {
                floor
            }
        })
        .to_string()
}
