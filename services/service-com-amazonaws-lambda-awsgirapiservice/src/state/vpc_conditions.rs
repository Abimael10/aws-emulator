// SPDX-License-Identifier: CC0-1.0

//! The VPC CONFIGURATION a request NAMES, as the condition keys IAM decides it by.
//!
//! Confining functions to approved network placements is a control a policy writer binds at
//! the IAM layer: an `Allow` of a call that attaches a function to a VPC can be narrowed to
//! the permitted security groups, subnets or VPC, so a call that asks for another placement is
//! not covered by that grant and falls to IAM's default deny. For that to hold, the placement
//! the request asks for has to reach the decision, which is what this module hands the gate
//! ([`super::caller_authorization::authorize_caller_with_conditions`]). Without it the clause
//! names a key the request publishes nothing for, the statement cannot be decided, and a
//! control the client tested as effective constrains nothing.
//!
//! The keys are named after this service's IAM prefix and the members they carry
//! ([`super::invoke_authorization::ACTION_PREFIX`]), the way every service-specific condition
//! key is: `lambda:SubnetIds` and `lambda:SecurityGroupIds` for the two lists
//! [`VpcConfig`](crate::ops::types::VpcConfig) binds
//! (`smithy_query(query="shape", shape="VpcConfig")`), and `lambda:VpcIds` for the VPC those
//! subnets belong to. The VPC is NOT a member of the request: "the ID of the VPC"
//! (`VpcConfigResponse$VpcId`) is EC2's own record of each configured subnet, so it is
//! resolved against the account's inventory by the one reading of it this crate has
//! ([`super::vpc_interfaces::configured_vpc_ids`]) rather than taken from the body.
//!
//! Each key is MULTI-VALUED, because the members are lists: the context carries one pair per
//! id the request names, which is how IAM's set qualifiers (`ForAllValues:` / `ForAnyValue:`)
//! decide a clause over all or any of them. The ids are the DISTINCT ones the configuration
//! names, bounded by the members' own ceilings, exactly as the attachment itself reads them
//! ([`super::vpc_interfaces`]), so what one request can publish does not follow the length of
//! the list it arrived with.
//!
//! The keys are PUBLISHED on every call that binds such a configuration, whether or not this
//! request named one ([`VPC_CONFIG_CONDITION_KEYS`]): carrying no placement is a fact about
//! such a request — the member is optional and this one omitted it — rather than a key this
//! emulator does not model, and that is what lets a statement conditioned on the keys decide a
//! call that asks for no attachment at all instead of leaving the clause undecidable.

use crate::ops::types::VpcConfig;

/// The condition key naming the VPC a request's configuration reaches: the VPC the subnets it
/// names belong to, as EC2 records each of them.
pub const VPC_IDS_CONDITION_KEY: &str = "lambda:VpcIds";

/// The condition key naming the subnets a request's configuration attaches the function to,
/// as its own `SubnetIds` member spells them.
pub const SUBNET_IDS_CONDITION_KEY: &str = "lambda:SubnetIds";

/// The condition key naming the security groups a request's configuration applies, as its own
/// `SecurityGroupIds` member spells them.
pub const SECURITY_GROUP_IDS_CONDITION_KEY: &str = "lambda:SecurityGroupIds";

/// The condition keys such a call MODELS — every key it would carry values for, named whether
/// or not this request asked for an attachment, so a statement written over one of them is
/// decided on the absence of values as well as on the values themselves (see the module docs).
pub const VPC_CONFIG_CONDITION_KEYS: &[&str] = &[
    VPC_IDS_CONDITION_KEY,
    SUBNET_IDS_CONDITION_KEY,
    SECURITY_GROUP_IDS_CONDITION_KEY,
];

/// The condition context a call that names the VPC configuration `vpc_config` publishes: one
/// `(key, value)` pair per id it names under each key above, and nothing at all for a request
/// that asks for no attachment — the absence itself, which
/// [`VPC_CONFIG_CONDITION_KEYS`] is what makes decidable.
///
/// A READ of the account's EC2 inventory (for the VPC alone) and of the request; it changes
/// nothing, and it reaches no store of this service, so a handler calls it BEFORE it locks its
/// own store, which is the lock order the gate documents.
#[must_use]
pub fn vpc_config_conditions<Eng: Send + Sync + 'static>(
    engine: &Eng,
    vpc_config: Option<&VpcConfig>,
) -> Vec<(String, String)> {
    conditions(
        &super::vpc_interfaces::configured_vpc_ids(engine, vpc_config),
        &super::vpc_interfaces::attached_subnets(vpc_config).unwrap_or_default(),
        &super::vpc_interfaces::security_group_ids(vpc_config),
    )
}

/// Those pairs, over ids already resolved: separated from the reading above so it is a pure
/// function of what the request named and what the inventory answered.
fn conditions(
    vpc_ids: &[String],
    subnet_ids: &[String],
    security_group_ids: &[String],
) -> Vec<(String, String)> {
    [
        (VPC_IDS_CONDITION_KEY, vpc_ids),
        (SUBNET_IDS_CONDITION_KEY, subnet_ids),
        (SECURITY_GROUP_IDS_CONDITION_KEY, security_group_ids),
    ]
    .into_iter()
    .flat_map(|(key, values)| {
        values
            .iter()
            .map(move |value| (key.to_string(), value.clone()))
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids a case names, as the owned strings the reading above hands over.
    fn ids(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    /// RULE: the context carries one pair per id the request names under each of the three
    /// keys, and no entry at all for a key it names none for — the keys themselves being
    /// modelled either way.
    ///
    /// The rows differ along what the rule generalises over: how many ids travel under a key
    /// (several, one, none), and which of the keys the request named anything for.
    #[test]
    fn the_context_carries_one_pair_per_id_the_request_names() {
        // (case, the VPCs resolved, the subnets named, the groups named, the pairs published)
        type Row<'a> = (&'a str, Vec<String>, Vec<String>, Vec<String>, Vec<(&'a str, &'a str)>);
        let rows: Vec<Row<'_>> = vec![
            (
                "one subnet in one VPC, carrying one group",
                ids(&["vpc-0a1b2c3d4e5f60718"]),
                ids(&["subnet-0a1b2c3d4e5f60718"]),
                ids(&["sg-0a1b2c3d4e5f60718"]),
                vec![
                    (VPC_IDS_CONDITION_KEY, "vpc-0a1b2c3d4e5f60718"),
                    (SUBNET_IDS_CONDITION_KEY, "subnet-0a1b2c3d4e5f60718"),
                    (SECURITY_GROUP_IDS_CONDITION_KEY, "sg-0a1b2c3d4e5f60718"),
                ],
            ),
            (
                "two subnets and two groups, each id published once in the order named",
                ids(&["vpc-0a1b2c3d4e5f60718"]),
                ids(&["subnet-0a1b2c3d4e5f60719", "subnet-0a1b2c3d4e5f60718"]),
                ids(&["sg-0a1b2c3d4e5f60719", "sg-0a1b2c3d4e5f60718"]),
                vec![
                    (VPC_IDS_CONDITION_KEY, "vpc-0a1b2c3d4e5f60718"),
                    (SUBNET_IDS_CONDITION_KEY, "subnet-0a1b2c3d4e5f60719"),
                    (SUBNET_IDS_CONDITION_KEY, "subnet-0a1b2c3d4e5f60718"),
                    (SECURITY_GROUP_IDS_CONDITION_KEY, "sg-0a1b2c3d4e5f60719"),
                    (SECURITY_GROUP_IDS_CONDITION_KEY, "sg-0a1b2c3d4e5f60718"),
                ],
            ),
            (
                "subnets of two VPCs, both published",
                ids(&["vpc-0a1b2c3d4e5f60718", "vpc-0a1b2c3d4e5f60719"]),
                ids(&["subnet-0a1b2c3d4e5f60718", "subnet-0a1b2c3d4e5f60719"]),
                Vec::new(),
                vec![
                    (VPC_IDS_CONDITION_KEY, "vpc-0a1b2c3d4e5f60718"),
                    (VPC_IDS_CONDITION_KEY, "vpc-0a1b2c3d4e5f60719"),
                    (SUBNET_IDS_CONDITION_KEY, "subnet-0a1b2c3d4e5f60718"),
                    (SUBNET_IDS_CONDITION_KEY, "subnet-0a1b2c3d4e5f60719"),
                ],
            ),
            (
                "a group named with no subnet, so no VPC resolves",
                Vec::new(),
                Vec::new(),
                ids(&["sg-0a1b2c3d4e5f60718"]),
                vec![(SECURITY_GROUP_IDS_CONDITION_KEY, "sg-0a1b2c3d4e5f60718")],
            ),
            (
                "no attachment at all",
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            ),
        ];

        for (case, vpc_ids, subnet_ids, group_ids, expected) in rows {
            let published = conditions(&vpc_ids, &subnet_ids, &group_ids);
            let expected: Vec<(String, String)> = expected
                .into_iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            assert_eq!(
                published, expected,
                "case {case:?}: expected the context {expected:?}, got {published:?}"
            );
            for (key, _) in &published {
                assert!(
                    VPC_CONFIG_CONDITION_KEYS.contains(&key.as_str()),
                    "case {case:?}: {key:?} must be one of the keys the call models, got \
                     {VPC_CONFIG_CONDITION_KEYS:?}"
                );
            }
        }
    }

    /// Every key the call models is published whether or not a value travels, and each is named
    /// after this service's IAM prefix and the member it carries.
    #[test]
    fn each_key_is_modelled_and_named_after_its_member() {
        let prefix = format!("{}:", super::super::invoke_authorization::ACTION_PREFIX);
        for key in VPC_CONFIG_CONDITION_KEYS {
            assert!(
                key.starts_with(&prefix),
                "case {key:?}: a service-specific condition key is named under {prefix:?}"
            );
        }
        for key in [
            VPC_IDS_CONDITION_KEY,
            SUBNET_IDS_CONDITION_KEY,
            SECURITY_GROUP_IDS_CONDITION_KEY,
        ] {
            assert!(
                VPC_CONFIG_CONDITION_KEYS.contains(&key),
                "case {key:?}: the key must be modelled whether or not a value travels, got \
                 {VPC_CONFIG_CONDITION_KEYS:?}"
            );
        }
    }

    /// The ids published are the DISTINCT ones the configuration names, bounded by the
    /// members' own ceilings — the same reading the attachment itself makes of the lists.
    #[test]
    fn the_ids_published_are_the_distinct_ones_the_configuration_names() {
        let subnet = |index: usize| format!("subnet-{index:017x}");
        let group = |index: usize| format!("sg-{index:017x}");
        let vpc_config = VpcConfig {
            subnet_ids: Some(
                (0..=super::super::vpc_interfaces::MAX_ATTACHED_SUBNETS)
                    .map(subnet)
                    .chain([subnet(0)])
                    .collect(),
            ),
            security_group_ids: Some(
                (0..=super::super::vpc_interfaces::MAX_ATTACHED_SECURITY_GROUPS)
                    .map(group)
                    .chain([group(0)])
                    .collect(),
            ),
            ipv6_allowed_for_dual_stack: None,
        };
        let subnets = super::super::vpc_interfaces::attached_subnets(Some(&vpc_config))
            .unwrap_or_default();
        let groups = super::super::vpc_interfaces::security_group_ids(Some(&vpc_config));
        let published = conditions(&[], &subnets, &groups);
        let count = |key: &str| {
            published
                .iter()
                .filter(|(published, _)| published == key)
                .count()
        };
        assert_eq!(
            count(SUBNET_IDS_CONDITION_KEY),
            super::super::vpc_interfaces::MAX_ATTACHED_SUBNETS,
            "the subnets published stop at the member's ceiling, got {published:?}"
        );
        assert_eq!(
            count(SECURITY_GROUP_IDS_CONDITION_KEY),
            super::super::vpc_interfaces::MAX_ATTACHED_SECURITY_GROUPS,
            "the groups published stop at the member's ceiling, got {published:?}"
        );
    }
}
