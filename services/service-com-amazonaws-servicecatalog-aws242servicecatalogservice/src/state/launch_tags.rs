// SPDX-License-Identifier: CC0-1.0

//! The TAG SET the CloudFormation stack behind a provisioned product carries.
//!
//! A launch's tags are not only the ones the request carried. Service Catalog governs the
//! deployment, so the stack it stands up for a provisioned product carries three things:
//!
//! * the caller's own `Tags`;
//! * the TagOptions the LAUNCH PATH forces onto everything launched through it — the same
//!   set `DescribeProvisioningParameters` offers under `TagOptions`, which is why that
//!   reading lives here ([`launch_tag_options`]) and both the read and the launch go
//!   through it; and
//! * the tags that record the GRAPH the launch went through ([`LaunchGraph`]), the only
//!   machine-readable link from a deployed stack back to the portfolio, product, version,
//!   principal and provisioned product that produced it.
//!
//! Every value is derived from the request and from this service's own stored state: the
//! resolved launch path, the product version, the caller's identity and the provisioned
//! product this launch minted. Nothing here invents a tag or copies one from a request's
//! text.

use std::collections::{BTreeMap, BTreeSet};

use crate::ops::types::TagOptionSummary;

use super::ComAmazonawsServicecatalogAws242servicecatalogserviceState as CatalogState;

/// The tag recording the portfolio whose launch path granted the launch.
pub const TAG_KEY_PORTFOLIO_ARN: &str = "aws:servicecatalog:portfolioArn";

/// The tag recording the product that was launched.
pub const TAG_KEY_PRODUCT_ARN: &str = "aws:servicecatalog:productArn";

/// The tag recording the product version (`ProvisioningArtifactId`) that was launched.
pub const TAG_KEY_PROVISIONING_ARTIFACT_IDENTIFIER: &str =
    "aws:servicecatalog:provisioningArtifactIdentifier";

/// The tag recording the principal that asked for the launch.
pub const TAG_KEY_PROVISIONING_PRINCIPAL_ARN: &str = "aws:servicecatalog:provisioningPrincipalArn";

/// The tag recording the provisioned product the launch stood the stack up for.
pub const TAG_KEY_PROVISIONED_PRODUCT_ARN: &str = "aws:servicecatalog:provisionedProductArn";

/// WHAT one launch went through, as the stack it deploys records it.
///
/// Each member is a value this service resolved for THIS request — the launch path's
/// portfolio, the product and the version named, the principal the request speaks for and
/// the provisioned product the launch is for — so the tags derived from it name the graph
/// that produced the stack rather than anything a caller wrote.
#[derive(Debug, Clone)]
pub struct LaunchGraph {
    /// The ARN of the portfolio the resolved launch path is.
    pub portfolio_arn: String,
    /// The ARN of the product being launched.
    pub product_arn: String,
    /// The `ProvisioningArtifactId` of the version being launched.
    pub provisioning_artifact_id: String,
    /// The ARN of the principal this request speaks for, as `sts:GetCallerIdentity`
    /// reports it for the credential the request was signed with.
    pub provisioning_principal_arn: String,
    /// The ARN of the provisioned product the stack belongs to.
    pub provisioned_product_arn: String,
}

impl LaunchGraph {
    /// This graph as the tags that record it, one per member.
    fn governance_tags(&self) -> [(&'static str, &str); 5] {
        [
            (TAG_KEY_PORTFOLIO_ARN, &self.portfolio_arn),
            (TAG_KEY_PRODUCT_ARN, &self.product_arn),
            (
                TAG_KEY_PROVISIONING_ARTIFACT_IDENTIFIER,
                &self.provisioning_artifact_id,
            ),
            (
                TAG_KEY_PROVISIONING_PRINCIPAL_ARN,
                &self.provisioning_principal_arn,
            ),
            (
                TAG_KEY_PROVISIONED_PRODUCT_ARN,
                &self.provisioned_product_arn,
            ),
        ]
    }
}

/// The tag set the stack of one provisioned product carries: the caller's `Tags`, the
/// TagOptions the launch path inherits onto it, and the tags recording `graph`.
///
/// `offered` is the launch path's TagOptions as [`launch_tag_options`] reports them — one
/// entry per KEY, carrying the values that path may use for it. A key the REQUEST already
/// tagged keeps the request's value: the caller chose among the values offered for it. A
/// key the request did not tag is applied when the path agrees on exactly ONE value, which
/// is then the value the administrator forced; a key offering several leaves the choice to
/// the request, and a key offering NONE is the conflict
/// `DescribeProvisioningParameters` documents ("if the output contains a TagOption key with
/// an empty list of values, there is a TagOption conflict for that key … launch is not
/// blocked"), so neither is applied by this service.
///
/// The tags recording the launch graph are written LAST, because they are the service's own
/// record of what it deployed and not a value a request may shadow.
///
/// A `BTreeMap`, so the stack is created with each key once and in one stable order.
pub fn launch_stack_tags(
    requested: &BTreeMap<String, String>,
    offered: &[TagOptionSummary],
    graph: &LaunchGraph,
) -> BTreeMap<String, String> {
    let mut tags = requested.clone();
    for option in offered {
        let Some(key) = option.key.as_ref() else {
            continue;
        };
        if tags.contains_key(key) {
            continue;
        }
        if let [value] = option.values.as_deref().unwrap_or_default() {
            tags.insert(key.clone(), value.clone());
        }
    }
    for (key, value) in graph.governance_tags() {
        tags.insert(key.to_string(), value.to_string());
    }
    tags
}

/// The tags of `tags` a call that is NOT the service's own may write: the keys outside the
/// prefix AWS reserves for the tags a service applies itself.
///
/// The launch records its graph under that prefix ([`LaunchGraph`]), which only a call made
/// as this service's own principal may set — the service serving it refuses a reserved key
/// from anybody else. So a deployment this service cannot make as its own principal submits
/// the caller's tags and the launch path's TagOptions and leaves the record of the graph off,
/// rather than submitting a tag set the deployment would be refused for: the stack a launch
/// stands up is what the provisioned product IS, so losing it is worse than losing the tags
/// that describe it.
pub fn caller_owned_tags(tags: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    tags.iter()
        .filter(|(key, _)| !is_service_owned_tag_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Whether `key` is in the prefix AWS reserves for the tags a service applies itself
/// (`Tag$Key`: "Tags owned by Amazon Web Services have the reserved prefix: `aws:`"), matched
/// case-insensitively as the services that refuse such a key match it.
fn is_service_owned_tag_key(key: &str) -> bool {
    key.get(..SERVICE_OWNED_TAG_KEY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SERVICE_OWNED_TAG_KEY_PREFIX))
}

/// See [`is_service_owned_tag_key`].
const SERVICE_OWNED_TAG_KEY_PREFIX: &str = "aws:";

/// "Information about the TagOptions associated with the resource": the TagOptions one
/// launch offers, one entry per KEY, with the values the launch may use for it.
///
/// A launch is a product reached through a portfolio, so the TagOptions associated with
/// EITHER of those two resources apply to it — the same stored associations
/// `AssociateTagOptionWithResource` writes and `DescribeProduct`'s admin view reports.
/// Only an ACTIVE TagOption is offered: `TagOptionDetail$Active` is what says whether a
/// TagOption may be used, and `UpdateTagOption` is what sets it.
///
/// Where the two resources carry the same key, the values this launch may use are the ones
/// they AGREE on, which is empty exactly when they agree on none: "if the output contains a
/// TagOption key with an empty list of values, there is a TagOption conflict for that key",
/// which `DescribeProvisioningParameters` describes as a conflict the end user cannot fix
/// and which does not block the launch.
///
/// The ONE reading of a launch's TagOptions: `DescribeProvisioningParameters` reports it and
/// a launch applies it ([`launch_stack_tags`]), so what a caller is offered and what the
/// deployed stack carries cannot disagree.
pub fn launch_tag_options(
    state: &CatalogState,
    portfolio_id: &str,
    product_id: &str,
) -> Vec<TagOptionSummary> {
    let portfolio_values = active_tag_option_values(state, portfolio_id);
    let product_values = active_tag_option_values(state, product_id);
    let mut keys: Vec<&String> = portfolio_values.keys().chain(product_values.keys()).collect();
    keys.sort();
    keys.dedup();
    keys.into_iter()
        .map(|key| {
            let values = match (portfolio_values.get(key), product_values.get(key)) {
                (Some(portfolio), Some(product)) => {
                    portfolio.intersection(product).cloned().collect()
                }
                (Some(only), None) | (None, Some(only)) => only.iter().cloned().collect(),
                // Unreachable: the key came from one of the two maps.
                (None, None) => Vec::new(),
            };
            TagOptionSummary {
                key: Some(key.clone()),
                values: Some(values),
            }
        })
        .collect()
}

/// The values the ACTIVE TagOptions associated with one resource offer, by TagOption key: a
/// `BTreeMap` of `BTreeSet`s, so the keys and the values a launch reports are in one stable
/// order whichever resource contributed them.
fn active_tag_option_values(
    state: &CatalogState,
    resource_id: &str,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut values: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for tag_option in state
        .resource_tag_option_ids(resource_id)
        .into_iter()
        .flatten()
        .filter_map(|id| state.tag_options.get(id))
        .filter(|tag_option| tag_option.active)
    {
        values
            .entry(tag_option.key.clone())
            .or_default()
            .insert(tag_option.value.clone());
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A launch graph whose every member is distinguishable, so a tag carrying the wrong
    /// one is visible.
    fn graph() -> LaunchGraph {
        LaunchGraph {
            portfolio_arn: "arn:aws:catalog:us-east-1:000000000000:portfolio/port-1".to_string(),
            product_arn: "arn:aws:catalog:us-east-1:000000000000:product/prod-1".to_string(),
            provisioning_artifact_id: "pa-1".to_string(),
            provisioning_principal_arn: "arn:aws:iam::000000000000:user/emulator".to_string(),
            provisioned_product_arn: "arn:aws:servicecatalog:us-east-1:000000000000:stack/app/pp-1"
                .to_string(),
        }
    }

    /// One offered TagOption key with the values the launch path agrees on for it.
    fn offered(key: &str, values: &[&str]) -> TagOptionSummary {
        TagOptionSummary {
            key: Some(key.to_string()),
            values: Some(values.iter().map(|value| value.to_string()).collect()),
        }
    }

    /// One row of the composition table: what the request tagged, what the launch path
    /// offers, and the tags the stack must carry beyond the launch graph.
    struct Row {
        case: &'static str,
        requested: &'static [(&'static str, &'static str)],
        offered: Vec<TagOptionSummary>,
        expected: &'static [(&'static str, &'static str)],
    }

    /// RULE: the stack's tags are the request's, plus every TagOption the launch path
    /// agrees on ONE value for and the request did not tag, plus the tags recording the
    /// launch graph.
    #[test]
    fn the_stack_carries_the_request_the_tag_options_and_the_launch_graph() {
        let graph = graph();
        // Rows differ along what the request tagged and what the path offers: nothing,
        // one value, several values (the caller's choice), none (a conflict), and a key
        // the request already carries.
        let rows = [
            Row {
                case: "no request tags and nothing offered",
                requested: &[],
                offered: Vec::new(),
                expected: &[],
            },
            Row {
                case: "a request tag and nothing offered",
                requested: &[("env", "prod")],
                offered: Vec::new(),
                expected: &[("env", "prod")],
            },
            Row {
                case: "one offered value, not tagged by the request",
                requested: &[],
                offered: vec![offered("costcenter", &["cc-1"])],
                expected: &[("costcenter", "cc-1")],
            },
            Row {
                case: "several offered values leave the choice to the request",
                requested: &[],
                offered: vec![offered("costcenter", &["cc-1", "cc-2"])],
                expected: &[],
            },
            Row {
                case: "a conflicted key offers no value at all",
                requested: &[],
                offered: vec![offered("costcenter", &[])],
                expected: &[],
            },
            Row {
                case: "the request's value wins over the values offered",
                requested: &[("costcenter", "cc-9")],
                offered: vec![offered("costcenter", &["cc-1", "cc-2"])],
                expected: &[("costcenter", "cc-9")],
            },
            Row {
                case: "several offered keys, each with one value",
                requested: &[("env", "prod")],
                offered: vec![offered("costcenter", &["cc-1"]), offered("team", &["ops"])],
                expected: &[("env", "prod"), ("costcenter", "cc-1"), ("team", "ops")],
            },
        ];
        for Row {
            case,
            requested,
            offered,
            expected,
        } in &rows
        {
            let requested: BTreeMap<String, String> = requested
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            let composed = launch_stack_tags(&requested, offered, &graph);
            let mut wanted: BTreeMap<String, String> = expected
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect();
            for (key, value) in graph.governance_tags() {
                wanted.insert(key.to_string(), value.to_string());
            }
            assert_eq!(
                composed, wanted,
                "case {case:?}: expected {wanted:?}, got {composed:?}"
            );
        }
    }

    /// RULE: the tags recording the launch graph are the service's own, so a request that
    /// tags one of those keys itself does not shadow what was deployed.
    #[test]
    fn the_launch_graph_is_not_shadowed_by_a_request_tag() {
        let graph = graph();
        let requested: BTreeMap<String, String> = [(
            TAG_KEY_PORTFOLIO_ARN.to_string(),
            "something the caller wrote".to_string(),
        )]
        .into_iter()
        .collect();
        let composed = launch_stack_tags(&requested, &[], &graph);
        let reported = composed.get(TAG_KEY_PORTFOLIO_ARN);
        assert_eq!(
            reported,
            Some(&graph.portfolio_arn),
            "case \"a request tagging a governance key\": expected {:?}, got {reported:?}",
            graph.portfolio_arn
        );
    }
}
