// SPDX-License-Identifier: CC0-1.0

//! Which collection members a clause nests the PRESENCE requirement for, read off this
//! service's own model.
//!
//! ## The rule
//!
//! A constraint a list's ELEMENTS break is stated at the list MEMBER
//! ([`super::list_element_refusals`]), with the element shape's declared constraints nested
//! inside `Member must satisfy constraint: [ … ]` — and that nested set CLOSES with the
//! requirement that the member not be null exactly where the model makes that member one a
//! request always carries: a member the model declares `@required`, and one carrying a
//! `@default`, which a caller cannot submit as null either. A member the model leaves
//! OPTIONAL — one a request may omit altogether — carries no such requirement in its
//! clause.
//!
//! What shows it: the recorded refusal of a create whose nested `VpcSettings.SubnetIds`
//! carries entries outside the subnet-id expression closes its nested set with that
//! requirement, and `DirectoryVpcSettings$SubnetIds` is `@required`; the recorded refusal of
//! a directory listing whose `DirectoryIds` filter carries an entry outside the
//! directory-id expression closes its nested set with the element expression alone, and
//! `DescribeDirectoriesRequest$DirectoryIds` is a member the model leaves optional. The
//! decision is therefore the model's declaration of the MEMBER rather than anything about
//! one request, so it holds for every list member of every operation of this service rather
//! than for the two a recording happens to cover.
//!
//! ## Where it is read from
//!
//! This service's own Smithy model, parsed once over the shapes a REQUEST can carry (each
//! operation's input and the closure of shapes its members reach): the schemas the framework
//! builds carry a member's `@length`, `@range`, `@pattern`, enum value set, `@uniqueItems`
//! and `@sensitive` traits and not its `@required` or `@default`
//! (`vendor/smithy-macros/src/codegen.rs`), so the declaration is read from the model
//! document the crate is generated from — the same source, under the same compile-time
//! constant, that three sibling service crates read a member trait the schema does not
//! carry from.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use ::aws_smithy_schema_server::constraints::path;
use ::serde_json::{Map, Value};

/// This service's Smithy model: the one source for which members a request always carries.
const MODEL_JSON: &str =
    include_str!("../../smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json");

/// The Smithy trait that marks a member a request must carry.
const REQUIRED_TRAIT: &str = "smithy.api#required";

/// The Smithy trait that gives a member a value a request that omits it is read with. A
/// member carrying one is never null either, so a clause about it states the same
/// requirement a `@required` member's does — and the trait written with a `null` value
/// REMOVES the default, which leaves the member optional.
const DEFAULT_TRAIT: &str = "smithy.api#default";

/// The member NAMES this model never leaves optional in a REQUEST: every member declared
/// `@required`, or with a `@default`, in every shape a request can carry it in.
///
/// Keyed by the member's own name, which is how a clause's path names it — a member of a
/// structure an input nests is stated by a dotted path whose LAST segment is that name — and
/// a name two REQUEST shapes declare differently is left OUT, so a requirement is nested
/// only where the model says the member is always carried whichever shape the clause came
/// from. The shapes a RESPONSE carries are not read at all: they describe what the service
/// reports rather than what a caller submits, and a member a create requires is routinely
/// optional in the description that reports it back.
static ALWAYS_CARRIED: LazyLock<HashSet<String>> = LazyLock::new(always_carried_members);

/// Whether the nested element set a clause states at the collection member `path` closes
/// with the requirement that the member not be null.
///
/// `path` is the dotted member path the validating walk builds (`directoryIds`,
/// `vpcSettings.subnetIds`), so the member it names is its last segment.
#[must_use]
pub fn requires_the_member(path: &str) -> bool {
    path.rsplit('.')
        .next()
        .is_some_and(|member| ALWAYS_CARRIED.contains(member))
}

/// Every member name this model declares `@required` or with a `@default` wherever a
/// REQUEST can carry it, spelled the way the validating walk spells a path segment
/// ([`path::member`], asked rather than copied so the two cannot disagree about how one
/// member is named).
///
/// The shapes read are the ones reachable from an operation's INPUT: each input structure,
/// the structures its members target, and the shapes its collections carry — the whole
/// closure, so a member of a nested structure is reached however deeply the input nests it.
/// Built once: the model is a compile-time constant, so the answer cannot change while the
/// process runs — and a document that does not parse is a broken build rather than a
/// request-time failure, so it is not swallowed into an empty set that would quietly drop
/// the requirement from every clause of this service.
fn always_carried_members() -> HashSet<String> {
    let model: Value = ::serde_json::from_str(MODEL_JSON)
        .expect("MODEL_JSON is this crate's own Smithy model and parses");
    let Some(shapes) = model.get("shapes").and_then(Value::as_object) else {
        return HashSet::new();
    };
    // A name is collected only if EVERY request shape declaring it declares it that way:
    // two shapes that disagree leave the clause the framework's own set rather than a
    // guessed one.
    let mut carried: HashMap<&str, bool> = HashMap::new();
    let mut reached: HashSet<&str> = HashSet::new();
    let mut pending: Vec<&str> = shapes
        .values()
        .filter(|shape| shape.get("type").and_then(Value::as_str) == Some("operation"))
        .filter_map(|operation| target_of(operation.get("input")))
        .collect();
    while let Some(id) = pending.pop() {
        if !reached.insert(id) {
            continue;
        }
        let Some(shape) = shapes.get(id) else {
            continue;
        };
        // A collection carries its entries' shape under its own slot rather than as a named
        // member, so the walk descends through it without reading a name off it.
        for slot in ["member", "key", "value"] {
            pending.extend(target_of(shape.get(slot)));
        }
        let Some(members) = shape.get("members").and_then(Value::as_object) else {
            continue;
        };
        for (name, member) in members {
            let always = always_carried(member);
            carried
                .entry(name.as_str())
                .and_modify(|held| *held = *held && always)
                .or_insert(always);
            pending.extend(target_of(Some(member)));
        }
    }
    carried
        .into_iter()
        .filter(|(_, always)| *always)
        .map(|(name, _)| path::member(name))
        .collect()
}

/// The shape a member or a collection slot targets.
fn target_of(slot: Option<&Value>) -> Option<&str> {
    slot?.get("target")?.as_str()
}

/// Whether one member declaration makes the member one a request always carries.
fn always_carried(member: &Value) -> bool {
    let Some(traits) = member.get("traits").and_then(Value::as_object) else {
        return false;
    };
    traits.contains_key(REQUIRED_TRAIT) || carries_a_default(traits)
}

/// Whether a member's traits give it a default value: the trait is declared and its value is
/// not `null`, which is how a model REMOVES a default a member once had.
fn carries_a_default(traits: &Map<String, Value>) -> bool {
    traits
        .get(DEFAULT_TRAIT)
        .is_some_and(|declared| !declared.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the presence requirement is nested for the collection members the model never
    /// leaves optional and for no other — read off the model, so a member the model declares
    /// `@required`, one carrying a `@default` and one it leaves optional are each answered
    /// as the model declares them.
    #[test]
    fn the_requirement_follows_the_models_own_declaration() {
        // (case, the member path a clause is stated at, whether its set closes with the
        // presence requirement). Each row's declaration is named in the comment beside it,
        // from `smithy_query(query="shape",shape="<Shape>")`.
        let rows: [(&str, &str, bool); 7] = [
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
            // `CreateConditionalForwarderRequest$DnsIpAddrs` and the update's: `@default`
            // `[]`, so a request never carries it as null.
            ("a forwarder's DNS addresses", "dnsIpAddrs", true),
            // `RemoveIpRoutesRequest$CidrIps`: `@default` `[]`.
            ("the address blocks a removal names", "cidrIps", true),
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
    }
}
