// SPDX-License-Identifier: CC0-1.0

//! Rendering one Route 53 Resolver DNS Firewall REQUEST the way the CloudFormation
//! resource-provider plugin's invalid-request exception states it: the name of the shape the
//! called operation's input is modelled under, then the members the provider set, each as
//! `<Member>=<Value>`.
//!
//! This is the half of that wording which is about the REQUEST rather than about any one
//! type's provider: which calls are rendered at all, and how each member's value reads. The
//! per-type half — which provider words a refusal this way, and for which class of refusal —
//! lives beside it in [`super::firewall_association_handler`], so the rendering can be read
//! and tested on its own.

use super::*;

/// The suffix Route 53 Resolver's model names an operation's INPUT shape with.
///
/// Each call rendered below takes an input shape named for the operation plus this
/// (`smithy_query(operation="AssociateFirewallRuleGroup,DisassociateFirewallRuleGroup,\
/// GetFirewallRuleGroupAssociation")` reports `input_shape: <Operation>Request` for each), and
/// that shape's name is what the rendering opens with.
const REQUEST_SHAPE_SUFFIX: &str = "Request";

/// What separates two rendered members of a request, and two rendered elements of a list.
const MEMBER_SEPARATOR: &str = ", ";

/// The members of each request rendered here, in the order Route 53 Resolver's model declares
/// them — the order the rendering states them in, since a request is rendered member by member
/// in the order its shape declares them rather than in the order the provider happened to set
/// them.
///
/// One entry per operation the DNS Firewall rule group ASSOCIATION's arms call, which are the
/// calls whose refusal that type's provider words this way
/// ([`super::firewall_association_handler`]), with each entry's members read from that
/// operation's input shape (the `smithy_query` call [`REQUEST_SHAPE_SUFFIX`] cites). A member
/// the provider did not set is no part of the request and is not rendered, which is what makes
/// the rendering an account of the call that was actually made; an operation with no entry here
/// is another type's provider's call and has no rendering, so its refusal is reported as the
/// owning service worded it.
const RENDERED_REQUEST_MEMBERS: &[(&str, &[&str])] = &[
    (
        "AssociateFirewallRuleGroup",
        &[
            "CreatorRequestId",
            "FirewallRuleGroupId",
            "VpcId",
            "Priority",
            "Name",
            "MutationProtection",
            "Tags",
        ],
    ),
    (
        "DisassociateFirewallRuleGroup",
        &["FirewallRuleGroupAssociationId"],
    ),
    (
        "GetFirewallRuleGroupAssociation",
        &["FirewallRuleGroupAssociationId"],
    ),
];

/// The STRUCTURE a request member that holds structures carries, per member: the shape its
/// elements are modelled under and that shape's own members, in the order the model declares
/// them, since a structure is rendered as its shape's name and the members it carries in
/// parentheses.
///
/// The one such member of the calls above is the tag set the associate takes: its `Tags` is a
/// `TagList` whose elements are that service's `Tag`, with a `Key` and a `Value`
/// (`smithy_query(shape="TagList,Tag")`). Every other member of those calls holds a scalar, so
/// no other entry is needed; a structure under a member with no entry has no shape name to
/// state and is left out of the rendering rather than rendered under a name nothing models.
const RENDERED_REQUEST_STRUCTURES: &[(&str, &str, &[&str])] =
    &[("Tags", "Tag", &["Key", "Value"])];

/// The request a provider issued, rendered as the plugin's exception states it: the input
/// shape's name, then the members the request carries as `<Member>=<Value>`, in the order
/// [`RENDERED_REQUEST_MEMBERS`] holds for the operation.
///
/// `None` where the operation has no entry there, or where the request is no document at all:
/// there is then nothing to render, and the refusal is reported as it came.
pub(crate) fn rendered_firewall_request(operation: &str, request: &Value) -> Option<String> {
    let members = RENDERED_REQUEST_MEMBERS
        .iter()
        .find(|(called, _)| *called == operation)
        .map(|(_, members)| *members)?;
    let request = request.as_object()?;
    let rendered: Vec<String> = members
        .iter()
        .filter_map(|member| {
            let value = rendered_value(member, request.get(*member)?)?;
            Some(format!("{member}={value}"))
        })
        .collect();
    Some(format!(
        "{operation}{REQUEST_SHAPE_SUFFIX}({})",
        rendered.join(MEMBER_SEPARATOR)
    ))
}

/// One value of the request under `member`, as the rendering states it: a scalar as the text
/// of the value itself, with no quoting of a string; a list as its rendered elements between
/// brackets; a structure as [`rendered_structure`] states it.
///
/// `None` for a member the request carries no value for, which is a member the provider did
/// not set.
fn rendered_value(member: &str, value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Bool(flag) => Some(flag.to_string()),
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) => Some(text.clone()),
        Value::Array(elements) => Some(format!(
            "[{}]",
            elements
                .iter()
                .filter_map(|element| rendered_value(member, element))
                .collect::<Vec<String>>()
                .join(MEMBER_SEPARATOR)
        )),
        Value::Object(structure) => rendered_structure(member, structure),
    }
}

/// One structure under `member`, as the rendering states it: the shape its elements are
/// modelled under, then the members it carries in parentheses, exactly as a whole request is
/// rendered.
///
/// `None` for a member [`RENDERED_REQUEST_STRUCTURES`] states no shape for.
fn rendered_structure(member: &str, structure: &Map<String, Value>) -> Option<String> {
    let (_, shape, members) = RENDERED_REQUEST_STRUCTURES
        .iter()
        .find(|(holder, _, _)| *holder == member)?;
    let rendered: Vec<String> = members
        .iter()
        .filter_map(|member| {
            let value = rendered_value(member, structure.get(*member)?)?;
            Some(format!("{member}={value}"))
        })
        .collect();
    Some(format!("{shape}({})", rendered.join(MEMBER_SEPARATOR)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request is rendered as its input shape's name and the members it carries, in the
    /// order the model declares them and whatever order they were set in — one row per form a
    /// member's value takes: a single scalar, every member of the associate, a number, an enum
    /// value and a list of structures.
    #[test]
    fn a_request_renders_as_its_shape_name_and_the_members_it_carries() {
        let cases: &[(&str, &str, Value, Option<&str>)] = &[
            (
                "one member",
                "DisassociateFirewallRuleGroup",
                serde_json::json!({ "FirewallRuleGroupAssociationId": "rslvr-frgassoc-0a1b" }),
                Some(
                    "DisassociateFirewallRuleGroupRequest(\
                     FirewallRuleGroupAssociationId=rslvr-frgassoc-0a1b)",
                ),
            ),
            (
                "the same member on the read",
                "GetFirewallRuleGroupAssociation",
                serde_json::json!({ "FirewallRuleGroupAssociationId": "rslvr-frgassoc-2c3d" }),
                Some(
                    "GetFirewallRuleGroupAssociationRequest(\
                     FirewallRuleGroupAssociationId=rslvr-frgassoc-2c3d)",
                ),
            ),
            (
                "members in the model's order, not the order they were set in",
                "AssociateFirewallRuleGroup",
                serde_json::json!({
                    "MutationProtection": "ENABLED",
                    "Priority": 101,
                    "Name": "filtering",
                    "VpcId": "vpc-0a1b2c3d4e5f60718",
                    "FirewallRuleGroupId": "rslvr-frg-0a1b",
                    "CreatorRequestId": "stack-Association-ABC",
                }),
                Some(
                    "AssociateFirewallRuleGroupRequest(CreatorRequestId=stack-Association-ABC, \
                     FirewallRuleGroupId=rslvr-frg-0a1b, VpcId=vpc-0a1b2c3d4e5f60718, \
                     Priority=101, Name=filtering, MutationProtection=ENABLED)",
                ),
            ),
            (
                "a member the provider did not set, and a list of structures",
                "AssociateFirewallRuleGroup",
                serde_json::json!({
                    "CreatorRequestId": "stack-Association-ABC",
                    "FirewallRuleGroupId": "rslvr-frg-0a1b",
                    "VpcId": "vpc-00112233445566778",
                    "Priority": 7,
                    "Name": "filtering",
                    "Tags": [
                        { "Key": "Owner", "Value": "networking" },
                        { "Key": "aws:cloudformation:logical-id", "Value": "Association" },
                    ],
                }),
                Some(
                    "AssociateFirewallRuleGroupRequest(CreatorRequestId=stack-Association-ABC, \
                     FirewallRuleGroupId=rslvr-frg-0a1b, VpcId=vpc-00112233445566778, \
                     Priority=7, Name=filtering, Tags=[Tag(Key=Owner, Value=networking), \
                     Tag(Key=aws:cloudformation:logical-id, Value=Association)])",
                ),
            ),
            (
                "a call another type's provider makes has no rendering here",
                "CreateFirewallDomainList",
                serde_json::json!({ "CreatorRequestId": "stack-DomainList-ABC", "Name": "list" }),
                None,
            ),
        ];
        for (case, operation, request, expected) in cases {
            let actual = rendered_firewall_request(operation, request);
            assert_eq!(
                actual.as_deref(),
                *expected,
                "case {case:?}: expected the rendering {expected:?}, got {actual:?}"
            );
        }
    }
}
