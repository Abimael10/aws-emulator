// SPDX-License-Identifier: CC0-1.0

//! HOW DEEP a thing-group hierarchy may go, and how a create that would go deeper is
//! refused.
//!
//! A group's place in the hierarchy is its own record's `parentGroupName`
//! ([`super::ThingGroupConfiguration`]), so the hierarchy is a chain of records and a
//! group's DEPTH is the length of the chain from the root down to it
//! ([`ComAmazonawsIotAwsiotserviceState::thing_group_hierarchy_depth`]). AWS IoT publishes
//! a maximum on that depth ([`MAX_THING_GROUP_HIERARCHY_DEPTH`]) and names it in the
//! refusal itself, so a create whose parent already sits at the bottom level is refused and
//! records nothing — the hierarchy is the one thing this family's reads walk, and a chain
//! longer than the service admits could never have been built on AWS.
//!
//! The depth is derived from the records rather than stored beside them, for the reason the
//! children of a group are ([`super::thing_group_membership`]): there is ONE record of the
//! parent relation, so the walk a reader takes and the ceiling a writer enforces cannot
//! disagree.

use super::ComAmazonawsIotAwsiotserviceState;

/// How many LEVELS of thing groups one hierarchy may hold.
///
/// AWS IoT Core's published thing-group limits cap the "maximum depth of thing group
/// hierarchy" at seven levels: a root group is the first level, so a group at the seventh
/// takes no child at all. The figure is not in the Smithy model, but the service states it
/// in the refusal it answers a deeper create with ([`over_deep_hierarchy_message`]), which
/// is why the constant and that sentence live together.
pub const MAX_THING_GROUP_HIERARCHY_DEPTH: usize = 7;

/// How AWS IoT refuses a `CreateThingGroup` whose `parentGroupName` is already at the
/// deepest level the hierarchy admits.
///
/// The fixed text is this family's — including the space before the colon after the family
/// noun, the way this family's other refusals about one group word it
/// (`Cannot delete thing group : <name> …`) — and the variable parts are the parent the
/// request named and the published ceiling the service enforces.
pub fn over_deep_hierarchy_message(parent_group_name: &str) -> String {
    format!(
        "Cannot create child group for parent group : {parent_group_name}. Depth of group \
         hierarchy exceeds allowed max depth :{MAX_THING_GROUP_HIERARCHY_DEPTH}"
    )
}

impl ComAmazonawsIotAwsiotserviceState {
    /// How many levels down the hierarchy the group `thing_group_name` sits: `1` for a
    /// group with no parent, and one more for each group above it.
    ///
    /// Read off the chain `ThingGroupMetadata$rootToParentThingGroups` is reported from
    /// ([`ComAmazonawsIotAwsiotserviceState::root_to_parent_thing_groups`]), so a reader of
    /// a group's ancestry and this depth cannot disagree; a name the registry holds no
    /// group under is at depth `0`, since it is in no hierarchy at all.
    pub fn thing_group_hierarchy_depth(&self, thing_group_name: &str) -> usize {
        self.root_to_parent_thing_groups(thing_group_name).len()
    }

    /// Whether the group `parent_group_name` may take a CHILD group: a child sits one level
    /// below its parent, so it fits exactly while the parent is above the deepest level
    /// [`MAX_THING_GROUP_HIERARCHY_DEPTH`] admits.
    pub fn admits_child_thing_group(&self, parent_group_name: &str) -> bool {
        self.thing_group_hierarchy_depth(parent_group_name) < MAX_THING_GROUP_HIERARCHY_DEPTH
    }
}
