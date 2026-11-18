// SPDX-License-Identifier: CC0-1.0

//! What a function's TENANT ISOLATION configuration requires of an invocation of it.
//!
//! `TenancyConfig` is "the tenant isolation mode configuration for a Lambda function. This
//! allows you to configure specific tenant isolation strategies for your function
//! invocations", and the mode it carries is REQUIRED of it
//! (`smithy_query(query="shape",shape="TenancyConfig")`:
//! `TenantIsolationMode` is `@required`, "Tenant isolation mode allows for invocation to be
//! sent to a corresponding execution environment dedicated to a specific tenant ID") — so a
//! function configured this way has no un-tenanted execution environment to be invoked in:
//! every invocation of it belongs to one tenant, named by `TenantId`, "the identifier of the
//! tenant in a multi-tenant Lambda function".
//!
//! That is why an invocation of such a function that names no tenant is REFUSED rather than
//! served: there is no environment it could be sent to, and running it in one built for no
//! tenant is exactly what the configuration exists to prevent — an environment is keyed by
//! the tenant it serves ([`super::execution::warm_pool`]), so nothing a handler left behind
//! for one tenant is reachable from another's invocation.
//!
//! # How it is stated
//!
//! The requirement is CONDITIONAL on the stored configuration of the function the call
//! addressed, so no constraint of an input shape can express it and no envelope of the
//! request walk states it. Lambda words such a requirement itself, as a sentence naming the
//! member it needs and the condition that needs it — the form this service's own recorded
//! refusals take for every other conditionally required member: "Runtime is required if the
//! deployment package is a .zip file archive", and `Handler` likewise
//! ([`crate::ops::create_function`]) — so this refusal is worded in that same form, fixed
//! text naming this member and this condition.
//!
//! It travels as the modelled `InvalidParameterValueException` (400, "One of the parameters
//! in the request is not valid"), which is the error AWS answers this refusal with: RECORDED,
//! from the corpus unit
//! `lambda.invoke-without-tenant-id-on-tenant-isolated-function-rejected`, whose check names
//! that code for an invoke of a tenant-isolated function carrying no tenant.
//!
//! The requirement is the FUNCTION's rather than one operation's, so it is read in one place
//! and applied by every invoking operation that can name a tenant, before anything runs,
//! before any environment is chosen and before the invocation is metered.
//!
//! A function carrying no `TenancyConfig` at all asks for nothing here and is invoked with
//! or without a tenant exactly as before.

use super::*;

/// Lambda's sentence for an invocation of a tenant-isolated function that names no tenant:
/// the member it needs, and the condition that needs it.
///
/// Fixed text, in the form this service words every other conditionally required member's
/// refusal in (see the module header): nothing of the request or of the stored configuration
/// is quoted into it, because what makes the member required is the configuration being in
/// force at all rather than any value it carries.
pub const ABSENT_TENANT_ID_MESSAGE: &str =
    "TenantId is required if the function is configured for tenant isolation";

/// The refusal an invocation of a function configured for tenant isolation owes when it
/// names no tenant, or `None` when the invocation may be served.
///
/// `tenancy_config` is the configuration the invocation RESOLVED — the version's own stored
/// one — and `tenant_id` what the call carried. A configuration whose mode is not one that
/// isolates per tenant requires nothing, as does one on an invocation that named a tenant.
#[must_use]
pub fn untenanted_invocation_refusal(
    tenancy_config: Option<&TenancyConfig>,
    tenant_id: Option<&str>,
) -> Option<String> {
    let mode = tenancy_config.map(|config| config.tenant_isolation_mode.as_str())?;
    if !isolates_per_tenant(mode) {
        return None;
    }
    if tenant_id.is_some_and(|tenant| !tenant.is_empty()) {
        return None;
    }
    Some(ABSENT_TENANT_ID_MESSAGE.to_string())
}

/// The `TenantIsolationMode` values that dedicate an execution environment to ONE tenant,
/// and so the ones whose invocations have to name the tenant they are for.
///
/// From the model's own enum (`smithy_query(query="shape",shape="TenantIsolationMode")`:
/// `PER_TENANT`), whose one value is such a mode — "Tenant isolation mode allows for
/// invocation to be sent to a corresponding execution environment dedicated to a specific
/// tenant ID". A mode this model GAINS is covered only once it is placed here, so a mode that
/// isolates nothing never silently starts requiring a tenant; the test below is what fails
/// when the model declares one this list does not classify.
const PER_TENANT_MODES: &[&str] = &["PER_TENANT"];

/// Whether `mode` is one of those modes ([`PER_TENANT_MODES`]).
#[must_use]
fn isolates_per_tenant(mode: &str) -> bool {
    PER_TENANT_MODES.contains(&mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One tenancy configuration, as a function configured for isolation holds one.
    fn isolated(mode: &str) -> TenancyConfig {
        TenancyConfig {
            tenant_isolation_mode: mode.to_string(),
        }
    }

    /// RULE: an invocation of a function configured for tenant isolation has to name the
    /// tenant it is for; nothing else here requires one.
    ///
    /// The rows differ along what the rule generalises over: whether the function carries a
    /// configuration at all, which mode it carries, and what the invocation named as its
    /// tenant.
    #[test]
    fn an_invocation_of_an_isolated_function_has_to_name_its_tenant() {
        let per_tenant = isolated("PER_TENANT");
        let unknown_mode = isolated("NOT_A_MODE");
        // (case, the function's configuration, the tenant the call named, refused)
        let rows: Vec<(&str, Option<&TenancyConfig>, Option<&str>, bool)> = vec![
            (
                "an isolated function, invoked with a tenant",
                Some(&per_tenant),
                Some("tenant-a"),
                false,
            ),
            (
                "an isolated function, invoked with a tenant of digits",
                Some(&per_tenant),
                Some("00000000"),
                false,
            ),
            (
                "an isolated function, invoked with no tenant at all",
                Some(&per_tenant),
                None,
                true,
            ),
            (
                "an isolated function, invoked with an empty tenant",
                Some(&per_tenant),
                Some(""),
                true,
            ),
            (
                "a function configured for no tenancy, invoked with no tenant",
                None,
                None,
                false,
            ),
            (
                "a function configured for no tenancy, invoked with a tenant",
                None,
                Some("tenant-a"),
                false,
            ),
            (
                "a mode this model does not declare, invoked with no tenant",
                Some(&unknown_mode),
                None,
                false,
            ),
        ];
        for (case, tenancy_config, tenant_id, refused) in rows {
            let stated = untenanted_invocation_refusal(tenancy_config, tenant_id);
            assert_eq!(
                stated.is_some(),
                refused,
                "case {case:?}: expected refused {refused}, got {stated:?}"
            );
        }
    }

    /// The sentence names the member the invocation left out and the condition that needs
    /// it, and names nothing of the request or of the stored configuration (see the module
    /// header): it is the fixed text of this error class.
    #[test]
    fn the_refusal_names_the_member_and_the_condition_that_requires_it() {
        let stated = untenanted_invocation_refusal(Some(&isolated("PER_TENANT")), None);
        assert_eq!(
            stated.as_deref(),
            Some("TenantId is required if the function is configured for tenant isolation"),
            "expected the sentence naming the member and the condition, got {stated:?}"
        );
    }

    /// RULE: every `TenantIsolationMode` the model declares is CLASSIFIED here, so a mode
    /// this model gains is read deliberately rather than falling into one answer by default.
    ///
    /// The generated catalog of the member's values is the source
    /// (`crate::ops::types::TENANT_ISOLATION_MODE_VALUES`, which the model's
    /// `TenantIsolationMode` enum renders): a value it gains that dedicates an environment
    /// belongs in [`PER_TENANT_MODES`], and one that isolates nothing has to be placed
    /// outside it with the reason — either way this test is what asks for the decision.
    #[test]
    fn every_mode_the_model_declares_is_classified() {
        for mode in crate::ops::types::TENANT_ISOLATION_MODE_VALUES {
            assert!(
                PER_TENANT_MODES.contains(mode),
                "case {mode:?}: the model declares a tenant isolation mode this module does \
                 not classify; place it in PER_TENANT_MODES, or outside it with the reason \
                 it dedicates no environment"
            );
        }
    }
}
