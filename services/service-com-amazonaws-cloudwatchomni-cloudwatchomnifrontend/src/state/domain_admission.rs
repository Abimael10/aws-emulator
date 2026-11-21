// SPDX-License-Identifier: CC0-1.0

//! The rules a domain create or patch is admitted under, and the text each refusal
//! carries.
//!
//! A CloudWatchOmni domain belongs to the ACCOUNT, not to the request: `ListDomains`'s
//! model doc reads "Returns the caller's domains: the account-scoped domain and the
//! organization-scoped domain, if either exists. At most two domains are returned", so
//! an account holds at most ONE account-scoped domain. A create that would mint a second
//! one is therefore a conflict with the domain the account already holds rather than a
//! second record, whatever name it asks for.
//!
//! The create is also idempotent by token — `clientToken` carries `@idempotencyToken`,
//! whose doc reads "Repeated requests with the same token return the original result
//! instead of creating a duplicate" — and the service admits the create only WITH that
//! token: a singleton an account cannot safely retry a create for is one a timed-out
//! caller can neither re-create nor tell apart from someone else's, so a request that
//! carries no token is refused before anything is minted.
//!
//! The model fixes the text of neither refusal, so each is recorded behavior and lives
//! here as ONE template, reached by every operation that applies the rule.

/// The message a create that carries no `clientToken` earns, naming the operation that
/// required one.
///
/// Fixed text apart from the operation's own name, which is the only thing that differs
/// between two operations stating it: the sentence is about the member the request left
/// out, so nothing else of the request reaches it.
pub fn client_token_required_message(operation: &str) -> String {
    format!("A clientToken is required for {operation}")
}

/// The message a create that would mint a second account-scoped domain earns.
///
/// Wholly fixed text: the conflict is with the ACCOUNT's domain rather than with either
/// name, so neither the stored domain nor the requested one is named.
pub const DOMAIN_ALREADY_EXISTS_FOR_ACCOUNT: &str = "A domain already exists for this account";

/// The message a request that would leave a domain federating through Identity Center
/// with no Identity Center instance behind it earns.
///
/// The rule is the model's own: `IdentityProviderConfiguration.identityCenterConfiguration`
/// is documented "Identity Center configuration. Required when identityProviders includes
/// IDC". The text is recorded behavior and wholly FIXED — it names the member whose value
/// is missing and the provider that required it, and carries no `domainId`, no provider
/// list and no other value of the request.
pub const IDENTITY_CENTER_INSTANCE_REQUIRED: &str =
    "identityCenterInstanceArn is required when authorizationProviders includes IDC";

/// Whether a domain left listing `identity_providers` under `configuration` is missing the
/// Identity Center instance that provider set requires.
///
/// Decided from the state a request would LEAVE the domain in — the providers it will list
/// and the configuration it will carry — rather than from the members one request
/// happened to send, so one rule covers a create, a patch that adds the provider and a
/// patch that takes the instance away.
pub fn identity_center_instance_required(
    identity_providers: &[String],
    configuration: Option<&super::IdentityProviderConfiguration>,
) -> bool {
    identity_providers
        .iter()
        .any(|provider| provider == super::IDENTITY_PROVIDER_IDC)
        && super::identity_center_instance_arn(configuration).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::{IdentityCenterConfiguration, IdentityProviderConfiguration};

    /// The token refusal names the operation it was required for and nothing else.
    #[test]
    fn the_token_refusal_names_the_operation_that_required_one() {
        // Rows span the operations of this model whose input declares an
        // `@idempotencyToken` member (`smithy_query(query="operation",
        // operation="CreateDomain,CreateDomainForOrganization")`), including one not yet
        // implemented, because the template takes the operation name verbatim.
        for (operation, expected) in [
            ("CreateDomain", "A clientToken is required for CreateDomain"),
            (
                "CreateDomainForOrganization",
                "A clientToken is required for CreateDomainForOrganization",
            ),
        ] {
            let actual = client_token_required_message(operation);
            assert_eq!(
                actual, expected,
                "case {operation:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The configuration a request carries when it names `instance`, or the empty one it
    /// carries when it names none.
    fn configuration(instance: Option<&str>) -> IdentityProviderConfiguration {
        IdentityProviderConfiguration {
            identity_center_configuration: instance.map(|arn| IdentityCenterConfiguration {
                identity_center_instance_arn: Some(arn.to_string()),
            }),
        }
    }

    /// An Identity Center instance is required exactly while the resulting provider list
    /// names the IDC provider.
    #[test]
    fn the_identity_center_instance_follows_the_resulting_provider_list() {
        let instance = "arn:aws:sso:::instance/ssoins-1234567890abcdef";
        // Rows span the `IdentityProviderList` the model admits (`@length(1..2)`,
        // `@uniqueItems`, member enum `IdentityProvider` [IAM, IDC]
        // (`smithy_query(query="shape", shape="IdentityProvider")`)) against each form the
        // configuration takes: absent, present but naming no instance, and naming one.
        for (case, providers, carried, required) in [
            ("IAM alone, no configuration", vec!["IAM"], None, false),
            (
                "IAM alone, a configuration naming an instance",
                vec!["IAM"],
                Some(configuration(Some(instance))),
                false,
            ),
            ("IDC alone, no configuration", vec!["IDC"], None, true),
            (
                "IDC alone, a configuration naming no instance",
                vec!["IDC"],
                Some(configuration(None)),
                true,
            ),
            (
                "IDC alone, a configuration naming an instance",
                vec!["IDC"],
                Some(configuration(Some(instance))),
                false,
            ),
            (
                "both providers, no configuration",
                vec!["IAM", "IDC"],
                None,
                true,
            ),
            (
                "both providers in the other order, an instance",
                vec!["IDC", "IAM"],
                Some(configuration(Some(instance))),
                false,
            ),
        ] {
            let providers: Vec<String> =
                providers.iter().map(|name| (*name).to_string()).collect();
            let actual = identity_center_instance_required(&providers, carried.as_ref());
            assert_eq!(
                actual, required,
                "case {case:?}: expected {required:?}, got {actual:?}"
            );
        }
    }

    /// The refusal states the member and the provider that required it, and nothing of
    /// the request.
    #[test]
    fn the_identity_center_refusal_is_wholly_fixed_text() {
        assert_eq!(
            IDENTITY_CENTER_INSTANCE_REQUIRED,
            "identityCenterInstanceArn is required when authorizationProviders includes IDC",
            "got {IDENTITY_CENTER_INSTANCE_REQUIRED:?}"
        );
        for submitted in ["d-0123456789abcdefghijklmno", "omni-domain"] {
            assert!(
                !IDENTITY_CENTER_INSTANCE_REQUIRED.contains(submitted),
                "the message carries the submitted {submitted:?}, got \
                 {IDENTITY_CENTER_INSTANCE_REQUIRED:?}"
            );
        }
    }
}
