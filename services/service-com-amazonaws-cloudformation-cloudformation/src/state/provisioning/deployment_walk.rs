// SPDX-License-Identifier: CC0-1.0

//! The DEPLOYMENT WALK's own helpers: what [`super::reconcile`] reads about one declaration
//! before it asks the service that owns the resource for anything.
//!
//! Their own file so [`super`] stays inside the length the build holds every file to; the
//! walk itself, and the records it reports back, live there.

use super::*;

/// Record the identity an owning service ASSIGNED to a resource it created: the id its
/// `Ref` resolves to from here on, and the `Id`/`<Type>Id` attributes read off that id,
/// which the naming rules had computed from the name instead.
///
/// Taking an identifier over is [`super::super::template::adopt_identity`], the one place that
/// step is written — the same one a binding's composite primary identifier goes through —
/// so the attributes and the id a resource reports cannot drift apart whichever of them
/// produced it.
///
/// `None` is every type whose physical id CloudFormation computes, which the create already
/// carried out to that service: nothing to adopt.
pub(super) fn adopt_minted_identity(resource: &mut StoredResource, assigned: Option<String>) {
    let Some(assigned) = assigned else {
        return;
    };
    super::super::template::adopt_identity(resource, assigned);
}

/// The stack-level reason a deployment whose RESOURCES failed reports, or `None` for a walk
/// that failed none of them.
///
/// One summary sentence per verb the failures fall under
/// ([`super::super::resource_failure_summary`]), each naming every resource that failed under that
/// verb in the order the walk reached them. A deployment attempts every resource whose
/// dependencies allow it, so more than one can have failed — and the field a client parses
/// to recover the set of failed logical ids has to name all of them. One deployment can be
/// doing two things at once (creating one resource while changing another), and then each
/// verb reports its own set, which is what keeps the sentence true of the resources it names.
pub(super) fn failed_resource_summary(failed: &[(String, &'static str)]) -> Option<String> {
    let mut verbs: Vec<&'static str> = Vec::new();
    for (_, action) in failed {
        if !verbs.contains(action) {
            verbs.push(action);
        }
    }
    let mut summary = String::new();
    for verb in verbs {
        let named = failed
            .iter()
            .filter(|(_, action)| *action == verb)
            .map(|(logical_id, _)| logical_id.as_str());
        summary.push_str(&super::super::resource_failure_summary(verb, named));
    }
    (!summary.is_empty()).then_some(summary)
}
