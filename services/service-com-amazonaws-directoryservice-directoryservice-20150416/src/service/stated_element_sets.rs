// SPDX-License-Identifier: CC0-1.0

//! Which collection members a clause nests the PRESENCE requirement for, read off this
//! service's own model.
//!
//! ## The rule
//!
//! A constraint a list's ELEMENTS break is stated at the list MEMBER
//! ([`super::list_element_refusals`]), with the element shape's declared constraints nested
//! inside `Member must satisfy constraint: [ … ]` — and that nested set CLOSES with the
//! requirement that the member not be null exactly where the model declares that member
//! `@required`. Every other list member — one the model leaves optional, and one carrying a
//! `@default`, which a request may still omit altogether — carries no such requirement in
//! its clause.
//!
//! What shows it, three recorded refusals of this service, one per form the declaration
//! takes:
//!
//! * a create whose nested `VpcSettings.SubnetIds` carries entries outside the subnet-id
//!   expression closes its nested set with that requirement, and
//!   `DirectoryVpcSettings$SubnetIds` is `@required`;
//! * a directory listing whose `DirectoryIds` filter carries an entry outside the
//!   directory-id expression closes its nested set with the element expression alone, and
//!   `DescribeDirectoriesRequest$DirectoryIds` is a member the model leaves optional;
//! * a conditional forwarder create whose `DnsIpAddrs` carries an entry outside the
//!   address expression closes its nested set with the element expression alone TOO, and
//!   `CreateConditionalForwarderRequest$DnsIpAddrs` is the third form: a member carrying a
//!   `@default` of `[]`. So a `@default` is not what decides the requirement — only
//!   `@required` is.
//!
//! The decision is therefore the model's declaration of the MEMBER rather than anything
//! about one request, so it holds for every list member of every operation of this service
//! rather than for the three a recording happens to cover.
//!
//! ## Where it is read from
//!
//! This service's own Smithy model, parsed once over the shapes a REQUEST can carry (each
//! operation's input and the closure of shapes its members reach): the schemas the framework
//! builds carry a member's `@length`, `@range`, `@pattern`, enum value set, `@uniqueItems`
//! and `@sensitive` traits and not its `@required`
//! (`vendor/smithy-macros/src/codegen.rs`), so the declaration is read from the model
//! document the crate is generated from — the same source, under the same compile-time
//! constant, that three sibling service crates read a member trait the schema does not
//! carry from.
//!
//! The declarations are held under the KEY a clause's path states a member by: the member's
//! own name for one an operation input declares itself, and `<holder>.<member>` for one of a
//! structure that input nests, which is how the validating walk spells such a path
//! (`vpcSettings.subnetIds`). Keyed that way rather than by the bare name because one member
//! NAME is not one declaration: `CustomerDnsIps` is `@required` in an assessment's
//! configuration and carries a `@default` in an AD Connector's connect settings, and a clause
//! about either states what the model declares for THAT one.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use ::aws_smithy_schema_server::constraints::path;
use ::serde_json::Value;

/// This service's Smithy model: the one source for which members a request must carry.
const MODEL_JSON: &str =
    include_str!("../../smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json");

/// The Smithy trait that marks a member a request must carry.
const REQUIRED_TRAIT: &str = "smithy.api#required";

/// Whether this model declares `@required`, for every member a REQUEST can carry, under the
/// key a clause's path states that member by ([`stated_key`]).
///
/// Two shapes that declare ONE key differently — the same member name held by the same
/// holder's name in two structures — leave it answered as the looser of the two, so a
/// requirement is nested only where the model requires the member whichever of them the
/// clause came from. The shapes a RESPONSE carries are not read at all: they describe what
/// the service reports rather than what a caller submits, and a member a create requires is
/// routinely optional in the description that reports it back.
static DECLARED_OF_A_REQUEST: LazyLock<HashMap<String, bool>> =
    LazyLock::new(declared_request_members);

/// Whether the nested element set a clause states at the collection member `path` closes
/// with the requirement that the member not be null.
///
/// `path` is the dotted member path the validating walk builds (`directoryIds`,
/// `vpcSettings.subnetIds`), and the declaration is looked up by its most specific KNOWN
/// key: the member together with the member that holds it, falling back to the member alone,
/// which is the key an operation input's own member is declared under. A path the model
/// declares nothing for states no requirement of this service.
#[must_use]
pub fn requires_the_member(path: &str) -> bool {
    let mut segments = path.rsplit('.');
    let Some(member) = segments.next() else {
        return false;
    };
    if let Some(holder) = segments.next() {
        if let Some(declared) = DECLARED_OF_A_REQUEST.get(&stated_key(Some(holder), member)) {
            return *declared;
        }
    }
    DECLARED_OF_A_REQUEST
        .get(&stated_key(None, member))
        .copied()
        .unwrap_or(false)
}

/// The key one member is declared under: the member's own name for one an operation input
/// declares itself, and `<holder>.<member>` for one of a structure that input nests — each
/// segment spelled the way the validating walk spells a path segment ([`path::member`],
/// asked rather than copied so the two cannot disagree about how one member is named).
fn stated_key(holder: Option<&str>, member: &str) -> String {
    match holder {
        Some(holder) => format!("{}.{}", path::member(holder), path::member(member)),
        None => path::member(member),
    }
}

/// Every member a REQUEST of this model can carry, with whether the model declares it
/// `@required`, under the key a clause's path states it by.
///
/// The shapes read are the ones reachable from an operation's INPUT: each input structure,
/// the structures its members target, and the shapes its collections carry — the whole
/// closure, so a member of a nested structure is reached however deeply the input nests it,
/// and each is reached WITH the name of the member that holds it, which is the other half of
/// its key. Built once: the model is a compile-time constant, so the answer cannot change
/// while the process runs — and a document that does not parse is a broken build rather than
/// a request-time failure, so it is not swallowed into an empty map that would quietly drop
/// the requirement from every clause of this service.
fn declared_request_members() -> HashMap<String, bool> {
    let model: Value = ::serde_json::from_str(MODEL_JSON)
        .expect("MODEL_JSON is this crate's own Smithy model and parses");
    let Some(shapes) = model.get("shapes").and_then(Value::as_object) else {
        return HashMap::new();
    };
    let mut declared: HashMap<String, bool> = HashMap::new();
    let mut reached: HashSet<(Option<&str>, &str)> = HashSet::new();
    // Each shape is walked once per holder it is reached through, since the holder is half of
    // the key its members are declared under; the model is finite and a shape already walked
    // under the same holder is not walked again, so the walk terminates on a model that
    // nests a shape inside itself.
    let mut pending: Vec<(Option<&str>, &str)> = shapes
        .values()
        .filter(|shape| shape.get("type").and_then(Value::as_str) == Some("operation"))
        .filter_map(|operation| target_of(operation.get("input")))
        .map(|input| (None, input))
        .collect();
    while let Some((holder, id)) = pending.pop() {
        if !reached.insert((holder, id)) {
            continue;
        }
        let Some(shape) = shapes.get(id) else {
            continue;
        };
        // A collection carries its entries' shape under its own slot rather than as a named
        // member, so the walk descends through it under the member that HOLDS the collection:
        // a path names no collection slot of its own.
        for slot in ["member", "key", "value"] {
            pending.extend(target_of(shape.get(slot)).map(|target| (holder, target)));
        }
        let Some(members) = shape.get("members").and_then(Value::as_object) else {
            continue;
        };
        for (name, member) in members {
            let required = declared_required(member);
            declared
                .entry(stated_key(holder, name))
                .and_modify(|held| *held = *held && required)
                .or_insert(required);
            pending.extend(target_of(Some(member)).map(|target| (Some(name.as_str()), target)));
        }
    }
    declared
}

/// The shape a member or a collection slot targets.
fn target_of(slot: Option<&Value>) -> Option<&str> {
    slot?.get("target")?.as_str()
}

/// Whether one member declaration makes the member one a request MUST carry: the model
/// declares it `@required`.
///
/// A `@default` is deliberately not read: a member carrying one may be omitted from the
/// request altogether, and the recorded refusal of a conditional forwarder create whose
/// defaulted `DnsIpAddrs` breaks its element expression states the element set alone (see
/// the module header).
fn declared_required(member: &Value) -> bool {
    member
        .get("traits")
        .and_then(Value::as_object)
        .is_some_and(|traits| traits.contains_key(REQUIRED_TRAIT))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the presence requirement is nested for the collection members the model
    /// declares `@required` and for no other — read off the model, so a member the model
    /// declares `@required`, one carrying a `@default` and one it leaves optional are each
    /// answered as the model declares them.
    #[test]
    fn the_requirement_follows_the_models_own_declaration() {
        // (case, the member path a clause is stated at, whether its set closes with the
        // presence requirement). Each row's declaration is named in the comment beside it,
        // from `smithy_query(query="shape",shape="<Shape>")`.
        let rows: [(&str, &str, bool); 10] = [
            // `DirectoryVpcSettings$SubnetIds` and `DirectoryConnectSettings$SubnetIds`:
            // both `@required`.
            ("a create's nested subnets", "vpcSettings.subnetIds", true),
            (
                "an AD Connector's nested subnets",
                "connectSettings.subnetIds",
                true,
            ),
            // `RemoveTagsFromResourceRequest$TagKeys`: `@required`.
            ("the tag keys a removal names", "tagKeys", true),
            // `AssessmentConfiguration$InstanceIds`: `@required`.
            (
                "the instances an assessment names",
                "assessmentConfiguration.instanceIds",
                true,
            ),
            // `AssessmentConfiguration$CustomerDnsIps`: `@required` — the SAME member name
            // the AD Connector's settings carry a `@default` for, so the two are answered by
            // their own declarations rather than by one answer for the name.
            (
                "the self-managed DNS servers an assessment names",
                "assessmentConfiguration.customerDnsIps",
                true,
            ),
            (
                "an AD Connector's self-managed DNS servers",
                "connectSettings.customerDnsIps",
                false,
            ),
            // `CreateConditionalForwarderRequest$DnsIpAddrs` and the update's: `@default`
            // `[]`, which a request may omit, so the model does not require them.
            ("a forwarder's DNS addresses", "dnsIpAddrs", false),
            // `RemoveIpRoutesRequest$CidrIps`: `@default` `[]` too.
            ("the address blocks a removal names", "cidrIps", false),
            // `DescribeDirectoriesRequest$DirectoryIds`: optional.
            ("a directory listing's filter", "directoryIds", false),
            // `DescribeDomainControllersRequest$DomainControllerIds`: optional.
            (
                "a domain controller listing's filter",
                "domainControllerIds",
                false,
            ),
        ];
        for (case, path, expected) in rows {
            let stated = requires_the_member(path);
            assert_eq!(
                stated, expected,
                "case {case:?}: expected the presence requirement nested = {expected}, got \
                 {stated}"
            );
        }

        assert!(
            !requires_the_member("notAMemberOfThisModel"),
            "a name the model does not declare states no requirement of this service"
        );
        assert!(
            !requires_the_member("notAShapeOfThisModel.subnetIds"),
            "a holder the model does not declare falls back to the member's own declaration"
        );
    }
}
