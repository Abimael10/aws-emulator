// SPDX-License-Identifier: CC0-1.0

//! Which stack a stack is NESTED IN: the parent/root relation every read of a stack reports,
//! derived from the records the store already holds.
//!
//! A stack created by an `AWS::CloudFormation::Stack` resource is a stack of its own
//! ([`super::provisioning`]'s nested-stack provider creates it through CloudFormation's own
//! `CreateStack`), and the resource that created it holds its stack id as the resource's
//! physical id — which is exactly what `DescribeStackResource` reports for the type. So the
//! relation is already recorded, once, where CloudFormation records it: the PARENT of a stack
//! is the stack holding a resource of that type whose physical id is this stack's id, and the
//! ROOT is the top of that chain ("the stack ID of the top-level stack to which the nested
//! stack ultimately belongs").
//!
//! Derived here rather than copied onto each stack so the two can never disagree: a resource
//! record and a parent pointer would be two statements of one fact, and the resource record
//! is the one the per-resource reads already answer from.
//!
//! The derivation is read as ONE index per read ([`Nesting`]): a projection reports the
//! relation for every stack of a page, so the records are walked once for the page rather than
//! once per stack. The same index answers what TERMINATION PROTECTION is in force on a stack
//! ([`Nesting::termination_protection_in_force`]), because that is a property of one stack of
//! a hierarchy — "for nested stacks, termination protection is set on the root stack and can't
//! be changed directly on the nested stack" — and so is read off the relation too.

use std::collections::BTreeMap;

use super::{ComAmazonawsCloudformationCloudformationState, StoredStack, STACK_RESOURCE_TYPE};

/// How deep a chain of parents is walked before the walk gives up.
///
/// A stack's parent chain is finite by construction — each nested stack is created by a
/// resource of the stack above it — and this is the bound that keeps a read finite even for
/// records that somehow point at each other. It is past the deepest nesting a template can
/// reach: each level's generated stack name carries its parent's whole name plus a separator,
/// a logical id and a unique segment, so a chain this long cannot be built under the
/// documented stack-name maximum ([`super::STACK_NAME_MAX_LEN`]).
const MAX_PARENT_WALK: usize = 16;

/// The nesting relation of one Region, read once: for each stack that an
/// `AWS::CloudFormation::Stack` resource created, the stack id of the stack that created it.
#[derive(Debug, Default)]
pub struct Nesting {
    /// Keyed by the CHILD's stack id, which is the resource's physical id.
    parent_of: BTreeMap<String, String>,
    /// Whether each stack of the Region carries termination protection of its own, keyed by
    /// stack id — read in the same pass, because what is in force on a stack of a hierarchy
    /// is the ROOT's flag and not its own
    /// ([`Nesting::termination_protection_in_force`]).
    protected: BTreeMap<String, bool>,
}

impl Nesting {
    /// `StackSummary$ParentId` / `Stack$ParentId` for one stack: "the stack ID of the direct
    /// parent of this stack", or `None` for a stack no `AWS::CloudFormation::Stack` resource
    /// created — a top-level stack, which carries neither member.
    pub fn parent_id_of(&self, stack: &StoredStack) -> Option<String> {
        self.parent_of.get(&stack.stack_id).cloned()
    }

    /// `StackSummary$RootId` / `Stack$RootId`: "the stack ID of the top-level stack to which
    /// the nested stack ultimately belongs" — the last stack the parent chain reaches.
    ///
    /// `None` for a stack with no parent: a top-level stack is the root of nothing it reports,
    /// so it carries neither member. For a first-level child the root IS the parent ("for the
    /// first level of nested stacks, the root stack is also the parent stack").
    pub fn root_id_of(&self, stack: &StoredStack) -> Option<String> {
        let mut root = self.parent_id_of(stack)?;
        for _ in 0..MAX_PARENT_WALK {
            match self.parent_of.get(&root) {
                Some(above) => root = above.clone(),
                None => break,
            }
        }
        Some(root)
    }

    /// Whether TERMINATION PROTECTION is in force on `stack`: the flag of the stack at the
    /// TOP of its hierarchy, and its own only where it is the top.
    ///
    /// `Stack$EnableTerminationProtection`: "for nested stacks, termination protection is set
    /// on the root stack and can't be changed directly on the nested stack" — so protection is
    /// a property of ONE stack of a hierarchy, the root, and every stack below it is protected
    /// exactly when that one is. That is what every reader of the flag asks: what a nested
    /// stack reports for it, and whether a deletion of a stack is refused for it.
    ///
    /// A hierarchy whose root this index holds no record for (a stack of another Region, which
    /// the relation of this one does not reach) leaves the stack judged on its own flag: there
    /// is no root here to read.
    pub fn termination_protection_in_force(&self, stack: &StoredStack) -> bool {
        match self.root_id_of(stack) {
            Some(root) => *self
                .protected
                .get(&root)
                .unwrap_or(&stack.enable_termination_protection),
            None => stack.enable_termination_protection,
        }
    }
}

impl ComAmazonawsCloudformationCloudformationState {
    /// The nesting relation of one Region ([`Nesting`]), built in ONE pass over that Region's
    /// stacks and their resource records.
    ///
    /// The relation is looked for within the Region: a nested stack is created by the parent's
    /// deployment, in the Region that deployment ran in.
    pub fn nesting_in(&self, region: &str) -> Nesting {
        let mut parent_of = BTreeMap::new();
        let mut protected = BTreeMap::new();
        for parent in self.stacks.iter().filter(|stack| stack.region == region) {
            protected.insert(
                parent.stack_id.clone(),
                parent.enable_termination_protection,
            );
            for deployed in &parent.resources {
                if deployed.resource.resource_type != STACK_RESOURCE_TYPE {
                    continue;
                }
                let child = &deployed.resource.physical_resource_id;
                if child.is_empty() || child == &parent.stack_id {
                    continue;
                }
                parent_of.insert(child.clone(), parent.stack_id.clone());
            }
        }
        Nesting {
            parent_of,
            protected,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::state::{DeployedResource, StoredResource, DEFAULT_REGION};

    /// One stack of the Region, carrying `protected` and declaring `children` as nested
    /// stacks of its own — the resource records the relation is derived from.
    fn stack(stack_id: &str, name: &str, protected: bool, children: &[&str]) -> StoredStack {
        let mut stack = StoredStack::new(
            stack_id.to_string(),
            name.to_string(),
            None,
            SystemTime::UNIX_EPOCH,
        );
        stack.enable_termination_protection = protected;
        stack.resources = children
            .iter()
            .enumerate()
            .map(|(position, child)| {
                DeployedResource::new(
                    StoredResource {
                        logical_id: format!("Child{position}"),
                        resource_type: STACK_RESOURCE_TYPE.to_string(),
                        physical_resource_id: (*child).to_string(),
                        attributes: BTreeMap::new(),
                        metadata: None,
                        properties: ::serde_json::Value::Null,
                        materialised: true,
                        deletion_policy: None,
                        update_replace_policy: None,
                    },
                    crate::state::resource_status::CREATE_COMPLETE,
                    SystemTime::UNIX_EPOCH,
                )
            })
            .collect();
        stack
    }

    /// A Region holding a three-level hierarchy and one unrelated top-level stack, with the
    /// hierarchy's ROOT carrying `protected`.
    fn region(protected: bool) -> ComAmazonawsCloudformationCloudformationState {
        ComAmazonawsCloudformationCloudformationState {
            stacks: vec![
                stack("arn:root/1", "app", protected, &["arn:child/2"]),
                stack("arn:child/2", "app-Child-ab", false, &["arn:kid/3"]),
                stack("arn:kid/3", "app-Child-ab-Kid-cd", false, &[]),
                stack("arn:other/9", "other", false, &[]),
            ],
            ..Default::default()
        }
    }

    /// RULE: termination protection is the property of the stack at the TOP of a nesting
    /// hierarchy — "for nested stacks, termination protection is set on the root stack and
    /// can't be changed directly on the nested stack" — so what is in force on a stack of a
    /// hierarchy is the root's flag, at every depth, and a top-level stack answers with its
    /// own.
    ///
    /// The rows span what the reading generalises over: the DEPTH of the stack (the root
    /// itself, the first level of nesting and the second), a top-level stack of another
    /// hierarchy, and whether the root carries protection at all.
    #[test]
    fn the_protection_in_force_on_a_nested_stack_is_the_root_stacks() {
        for protected in [false, true] {
            let state = region(protected);
            let nesting = state.nesting_in(DEFAULT_REGION);
            // (the stack, what it reports, its parent, its root)
            let rows: Vec<(&str, bool, Option<&str>, Option<&str>)> = vec![
                ("arn:root/1", protected, None, None),
                ("arn:child/2", protected, Some("arn:root/1"), Some("arn:root/1")),
                ("arn:kid/3", protected, Some("arn:child/2"), Some("arn:root/1")),
                ("arn:other/9", false, None, None),
            ];
            for (stack_id, expected, parent, root) in rows {
                let held = state
                    .stacks
                    .iter()
                    .find(|stack| stack.stack_id == stack_id)
                    .expect("a seeded stack");
                let in_force = nesting.termination_protection_in_force(held);
                assert_eq!(
                    in_force, expected,
                    "case {stack_id:?} with the root protected={protected}: expected \
                     {expected}, got {in_force}"
                );
                assert_eq!(
                    nesting.parent_id_of(held).as_deref(),
                    parent,
                    "case {stack_id:?}: expected the parent {parent:?}"
                );
                assert_eq!(
                    nesting.root_id_of(held).as_deref(),
                    root,
                    "case {stack_id:?}: expected the root {root:?}"
                );
            }
        }
    }
}
