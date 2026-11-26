// SPDX-License-Identifier: CC0-1.0

//! That the TWO lookups of the AWS-MANAGED JOB TEMPLATE catalog agree, name for name, on
//! what a name the catalog does not hold means.
//!
//! The wire harness, and the table of names the catalog does not hold, are the sibling
//! suite's (`crate::managed_job_templates`), so nothing of either is written twice; what is
//! asserted here is the one rule that is about the two operations TOGETHER rather than
//! about either of them:
//!
//! the catalog is the service's own published reference data, named in its reserved `AWS-`
//! managed-template namespace and written by no request, so what a lookup miss MEANS is a
//! property of the NAME — a name of that reserved namespace the catalog does not hold is a
//! managed template that is MISSING (`ResourceNotFoundException`, 404, "ManagedJobTemplate
//! with name: <name> not found"), and a name outside it names no managed template the
//! service could publish (`InvalidRequestException`, 400, "Invalid ManagedJobTemplate name:
//! <name>"). Because it is a property of the name, `DescribeManagedJobTemplate`, which
//! reads the name off the `{templateName}` URI label, and `ListManagedJobTemplates`, which
//! reads it off the `templateName` filter, answer one name with the SAME status, the same
//! `x-amzn-errortype` and the same sentence: one name cannot be a missing template to one
//! of them and an invalid value to the other.
//!
//! A refused lookup also leaves the published catalog as it was — it is read-only data — so
//! the same name is refused the same way twice and a full listing is unchanged afterwards.

use crate::managed_job_templates::{unpublished, unpublished_names, Emulator, Reply};

/// What a client branches on, as one value, so a describe's answer and a listing's can be
/// compared whole rather than member by member.
fn refusal(reply: &Reply) -> (u16, Option<String>, String) {
    (reply.status, reply.error_code.clone(), reply.message())
}

/// RULE: both lookups of one name the catalog does not hold answer the SAME refusal, and it
/// is the one the name's namespace fixes.
#[tokio::test]
async fn both_lookups_of_one_missing_name_answer_the_same_refusal() {
    let emulator = Emulator::new();
    for name in unpublished_names() {
        let (status, error_type, message) = unpublished(&name);
        let expected = (status, Some(error_type.to_string()), message);

        let described = emulator.describe(&name, "").await;
        let listed = emulator.list(&format!("templateName={name}")).await;

        assert_eq!(
            refusal(&described),
            expected,
            "case {name:?}: expected the describe to answer {expected:?}, got {described:?}"
        );
        assert_eq!(
            refusal(&listed),
            expected,
            "case {name:?}: expected the listing filter to answer {expected:?}, got {listed:?}"
        );
    }
}

/// RULE: refusing a lookup writes nothing — the same name earns the same refusal twice, and
/// the whole published catalog reads exactly as it did before the refusals.
#[tokio::test]
async fn a_refused_lookup_leaves_the_published_catalog_as_it_was() {
    let emulator = Emulator::new();
    let before = emulator.list("").await;
    for name in unpublished_names() {
        let first = emulator.describe(&name, "").await;
        let second = emulator.describe(&name, "").await;
        assert_eq!(
            refusal(&first),
            refusal(&second),
            "case {name:?}: expected the same refusal twice, got {first:?} then {second:?}"
        );
        let _ = emulator.list(&format!("templateName={name}")).await;
    }
    let after = emulator.list("").await;
    assert_eq!(
        after.body, before.body,
        "after every refused lookup: expected the catalog as it was, got {:?}",
        after.body
    );
}
