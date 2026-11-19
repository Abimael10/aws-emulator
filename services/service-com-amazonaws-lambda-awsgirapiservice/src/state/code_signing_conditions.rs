// SPDX-License-Identifier: CC0-1.0

//! The CODE-SIGNING CONFIGURATION a request NAMES, as the condition key IAM decides it by.
//!
//! Code signing is a deployment control a policy writer binds at the IAM layer: an `Allow` of
//! an operation that attaches a configuration can be narrowed to the approved configuration,
//! so a call that asks for another one — or for none at all — is not covered by that grant and
//! falls to IAM's default deny. For that to hold, the value the request carries has to reach
//! the decision, which is what this module hands the gate
//! ([`super::caller_authorization::authorize_caller_with_conditions`]).
//!
//! The key is named after this service's IAM prefix and the member it carries
//! ([`super::invoke_authorization::ACTION_PREFIX`]), the way every service-specific condition
//! key is: `lambda:CodeSigningConfigArn`. The operations that publish it are the ones whose
//! input binds a code-signing configuration ARN as a value ABOUT the request rather than as
//! the resource the request addresses — `CreateFunction`, whose optional
//! `CodeSigningConfigArn` member asks for the function to be created under a configuration,
//! and `PutFunctionCodeSigningConfig`, whose required member attaches one to a function that
//! already exists (`smithy_query(query="operation", operation="CreateFunction,PutFunctionCodeSigningConfig")`).
//! An operation that takes the configuration as its own `@httpLabel` resource publishes
//! nothing here: there the ARN is the resource the statement's `Resource` element is matched
//! against.
//!
//! The key is PUBLISHED on every one of those calls, whether or not it named a value
//! ([`CODE_SIGNING_CONFIG_CONDITION_KEYS`]): absence is a fact about such a request — the
//! member is optional and this one omitted it — rather than a key this emulator does not
//! model, and that is what lets a statement conditioned on the key decide a call that supplies
//! none instead of leaving the clause undecidable.

/// The condition key a call that names a CODE-SIGNING CONFIGURATION publishes a value for:
/// the ARN of the configuration the request asks for, as its own member spells it.
pub const CODE_SIGNING_CONFIG_ARN_CONDITION_KEY: &str = "lambda:CodeSigningConfigArn";

/// The condition keys such a call MODELS — every key it would carry a value for, named whether
/// or not this request supplied one, so a statement written over the key is decided on the
/// value's absence as well as on its value (see the module docs).
pub const CODE_SIGNING_CONFIG_CONDITION_KEYS: &[&str] = &[CODE_SIGNING_CONFIG_ARN_CONDITION_KEY];

/// The condition context a call that names the configuration `requested` publishes: the one
/// pair when the request carried an ARN, and nothing at all when it carried none — the absence
/// itself, which [`CODE_SIGNING_CONFIG_CONDITION_KEYS`] is what makes decidable.
#[must_use]
pub fn code_signing_config_conditions(requested: Option<&str>) -> Vec<(String, String)> {
    requested
        .map(|arn| {
            vec![(
                CODE_SIGNING_CONFIG_ARN_CONDITION_KEY.to_string(),
                arn.to_string(),
            )]
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the context carries the request's own value for the key when it named one, and no
    /// entry at all when it did not — the key itself being modelled either way.
    #[test]
    fn the_context_carries_the_arn_the_request_named_and_nothing_when_it_named_none() {
        // (case, the member the request carried, the value published for the key)
        let rows: &[(&str, Option<&str>, Option<&str>)] = &[
            (
                "a configuration of this account",
                Some("arn:aws:lambda:us-east-1:000000000000:code-signing-config:csc-0123456789abcdef0"),
                Some("arn:aws:lambda:us-east-1:000000000000:code-signing-config:csc-0123456789abcdef0"),
            ),
            (
                "a configuration of another Region",
                Some("arn:aws:lambda:eu-west-1:000000000000:code-signing-config:csc-fedcba98765432100"),
                Some("arn:aws:lambda:eu-west-1:000000000000:code-signing-config:csc-fedcba98765432100"),
            ),
            ("no configuration at all", None, None),
        ];

        for (case, requested, expected) in rows {
            let published = code_signing_config_conditions(*requested);
            let expected: Vec<(String, String)> = expected
                .map(|value| {
                    vec![(
                        CODE_SIGNING_CONFIG_ARN_CONDITION_KEY.to_string(),
                        value.to_string(),
                    )]
                })
                .unwrap_or_default();
            assert_eq!(
                published, expected,
                "case {case:?}: expected the context {expected:?}, got {published:?}"
            );
            assert!(
                CODE_SIGNING_CONFIG_CONDITION_KEYS
                    .contains(&CODE_SIGNING_CONFIG_ARN_CONDITION_KEY),
                "case {case:?}: the key must be modelled whether or not a value travels, got \
                 {CODE_SIGNING_CONFIG_CONDITION_KEYS:?}"
            );
        }
    }
}
