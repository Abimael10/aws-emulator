// SPDX-License-Identifier: CC0-1.0

//! Whether a `COGNITO_USER_POOLS` authorizer's `providerARNs` entry NAMES an Amazon Cognito
//! user pool, for the precondition every write of that member turns on each of its entries.
//!
//! `providerARNs` is "a list of the Amazon Cognito user pool ARNs for the
//! `COGNITO_USER_POOLS` authorizer. Each element is of this format:
//! `arn:aws:cognito-idp:{region}:{account_id}:userpool/{user_pool_id}`"
//! (`Authorizer$providerARNs`), so each entry names a pool whose tokens that authorizer
//! ACCEPTS: the pool the entry names is what a presented token has to have been signed by
//! ([`crate::service::invoke::cognito_user_pools`]). A value that names no pool therefore
//! accepts nothing — every call through such an authorizer is refused whatever credential it
//! presents — so an authorizer carrying one could never authorize a method, and API Gateway
//! refuses the write instead of storing it.
//!
//! WHAT THIS GATE READS IS THE MEMBER, and nothing else: whether the value is a user-pool ARN
//! at all. Whether the pool that ARN names is one Amazon Cognito HOLDS is a fact of the other
//! service's records, and it is not stable across the authorizer's life — a pool may be deleted
//! at any time after the authorizer was written — so it is resolved where it is used, by the
//! data plane, once per call, against the pool's own records: that reading is what decides a
//! presented token, and an authorizer whose pool has gone away accepts nothing.
//!
//! The ARN reading itself lives here, because both planes read that member: the control plane
//! to decide whether a write is admitted, and the data plane to find the pool a presented token
//! has to come from.

use super::AuthorizerEntry;

// The `AuthorizerType` whose `providerARNs` name the pools it accepts tokens from: the
// operation that owns that catalog states the value, so this gate and the create's own
// type-coherence rule read one spelling of it.
use crate::ops::create_authorizer::AUTHORIZER_TYPE_COGNITO_USER_POOLS;

/// The service namespace a user pool's ARN names, which is the user-pool service's own
/// `arnNamespace`.
const USER_POOL_ARN_VENDOR: &str = "cognito-idp";

/// The resource prefix a user pool's ARN carries:
/// `arn:aws:cognito-idp:{region}:{account}:userpool/{pool-id}`, the form
/// `Authorizer$providerARNs` lists.
const USER_POOL_ARN_RESOURCE: &str = "userpool/";

/// How a refusal of an entry that names no user pool OPENS.
///
/// FIXED TEXT: API Gateway states this sentence for the member and then LISTS the entries at
/// fault, which are the only part of the message the request supplies — so what varies between
/// two refusals is which values were rejected and nothing else.
pub const INVALID_PROVIDER_ARNS: &str =
    "ProviderARNs need to be valid Cognito Userpools. Invalid ARNs-";

/// The user-pool id a `providerARNs` entry names, or `None` for a value that names no user
/// pool.
///
/// The ARN's resource is [`USER_POOL_ARN_RESOURCE`] then the pool's own id, which is the form
/// `Authorizer$providerARNs` lists; the tree's one ARN parse reads the six fields so this
/// agrees with every other service on what an ARN is. A value that is no ARN at all, an ARN of
/// another service (an identity pool is Amazon Cognito Identity's resource, not a user pool),
/// and an ARN whose resource reaches PAST one pool (a pool's app client) each name no pool.
pub fn user_pool_id_of(arn: &str) -> Option<&str> {
    let parsed = ::service_common::arn::parse(arn.trim())?;
    if parsed.vendor != USER_POOL_ARN_VENDOR {
        return None;
    }
    let id = parsed.resource.strip_prefix(USER_POOL_ARN_RESOURCE)?.trim();
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

/// Refuse an authorizer carrying a `providerARNs` entry that names no user pool, or answer `Ok`
/// for one every entry of which names one — and for one whose `type` lists no pools at all.
///
/// The check reads the STORED record rather than one request's members, for the reason
/// [`crate::ops::create_authorizer::require_members_of_type`] records: the rule holds of the
/// record however it came to look that way, so the create judges the record it would store and
/// `UpdateAuthorizer` the record its patches produced.
///
/// `providerARNs` is the `COGNITO_USER_POOLS` authorizer's member — "for a TOKEN or REQUEST
/// authorizer, this is not defined" — so only that type's entries are read.
pub fn require_provider_user_pools(entry: &AuthorizerEntry) -> Result<(), String> {
    if entry.r#type != AUTHORIZER_TYPE_COGNITO_USER_POOLS {
        return Ok(());
    }
    let unnamed: Vec<&str> = entry
        .provider_arns
        .iter()
        .filter(|arn| user_pool_id_of(arn).is_none())
        .map(String::as_str)
        .collect();
    if unnamed.is_empty() {
        return Ok(());
    }
    Err(invalid_provider_arns(&unnamed))
}

/// The sentence an authorizer whose `providerARNs` name no pool is refused with: the member's
/// own fixed opening ([`INVALID_PROVIDER_ARNS`]) and then the rejected entries, one per line,
/// as the request wrote them.
fn invalid_provider_arns(unnamed: &[&str]) -> String {
    let mut message = String::from(INVALID_PROVIDER_ARNS);
    for arn in unnamed {
        message.push('\n');
        message.push_str(arn);
    }
    message
}

#[cfg(test)]
mod tests {
    use super::{invalid_provider_arns, require_provider_user_pools, INVALID_PROVIDER_ARNS};
    use crate::state::AuthorizerEntry;

    /// An authorizer record of `authorizer_type` over `provider_arns`, as a handler assembles
    /// one before judging it.
    fn authorizer(authorizer_type: &str, provider_arns: &[&str]) -> AuthorizerEntry {
        AuthorizerEntry {
            name: "pool-authorizer".to_string(),
            r#type: authorizer_type.to_string(),
            provider_arns: provider_arns
                .iter()
                .map(|arn| (*arn).to_string())
                .collect(),
            ..AuthorizerEntry::default()
        }
    }

    /// One row: the case, the authorizer's own type, the entries it lists, and the entries a
    /// refusal has to name — `None` for a record the member admits.
    type Row<'a> = (&'a str, &'a str, &'a [&'a str], Option<Vec<&'a str>>);

    /// RULE: a `COGNITO_USER_POOLS` authorizer is admitted only when EVERY `providerARNs` entry
    /// names a user pool, and the refusal lists the entries that do not.
    #[test]
    fn every_entry_of_a_user_pool_authorizer_has_to_name_a_pool() {
        // Rows differ along the authorizer's type, how many entries it lists and what kind of
        // value each entry is.
        let rows: &[Row<'_>] = &[
            (
                "one pool ARN",
                "COGNITO_USER_POOLS",
                &["arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_abc123"],
                None,
            ),
            (
                "two pool ARNs",
                "COGNITO_USER_POOLS",
                &[
                    "arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_abc123",
                    "arn:aws-cn:cognito-idp:cn-north-1:000000000000:userpool/cn-north-1_p",
                ],
                None,
            ),
            (
                "a value that is no ARN at all",
                "COGNITO_USER_POOLS",
                &["not-an-arn"],
                Some(vec!["not-an-arn"]),
            ),
            (
                "an identity POOL, which is another service's resource",
                "COGNITO_USER_POOLS",
                &["arn:aws:cognito-identity:us-east-1:000000000000:identitypool/us-east-1:abc"],
                Some(vec![
                    "arn:aws:cognito-identity:us-east-1:000000000000:identitypool/us-east-1:abc",
                ]),
            ),
            (
                "a pool's APP CLIENT, which is not the pool",
                "COGNITO_USER_POOLS",
                &["arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_a/client/c"],
                Some(vec![
                    "arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_a/client/c",
                ]),
            ),
            (
                "a pool ARN carrying no id",
                "COGNITO_USER_POOLS",
                &["arn:aws:cognito-idp:us-east-1:000000000000:userpool/"],
                Some(vec!["arn:aws:cognito-idp:us-east-1:000000000000:userpool/"]),
            ),
            (
                "one pool ARN and one value that is not one",
                "COGNITO_USER_POOLS",
                &[
                    "arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_abc123",
                    "us-east-1_abc123",
                ],
                Some(vec!["us-east-1_abc123"]),
            ),
            (
                "a TOKEN authorizer, whose type lists no pools",
                "TOKEN",
                &["not-an-arn"],
                None,
            ),
            (
                "a REQUEST authorizer, whose type lists no pools",
                "REQUEST",
                &["not-an-arn"],
                None,
            ),
        ];
        for (case, authorizer_type, provider_arns, refused) in rows {
            let judged = require_provider_user_pools(&authorizer(authorizer_type, provider_arns));
            let expected = refused.as_ref().map(|unnamed| invalid_provider_arns(unnamed));
            assert_eq!(
                judged.err(),
                expected,
                "case {case:?}: expected {expected:?} for {provider_arns:?}"
            );
        }
    }

    /// RULE: the refusal opens with the member's fixed sentence and then states the rejected
    /// entries, one per line, exactly as the request wrote them.
    #[test]
    fn the_refusal_lists_the_entries_that_name_no_pool() {
        // Rows differ along how many entries were rejected.
        let rows: &[(&str, &[&str], String)] = &[
            (
                "one entry",
                &["not-an-arn"],
                format!("{INVALID_PROVIDER_ARNS}\nnot-an-arn"),
            ),
            (
                "two entries",
                &["not-an-arn", "also-not-an-arn"],
                format!("{INVALID_PROVIDER_ARNS}\nnot-an-arn\nalso-not-an-arn"),
            ),
        ];
        for (case, unnamed, expected) in rows {
            let actual = invalid_provider_arns(unnamed);
            assert_eq!(
                &actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }
}
