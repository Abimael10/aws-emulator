// SPDX-License-Identifier: CC0-1.0

//! How `SetInstanceProtection` resolves the group it was asked about, over the awsQuery
//! wire.
//!
//! The rule under test: the instance ids a call carries are resolved against the
//! MEMBERSHIP of the group the request names, so that group is resolved FIRST — a name
//! the account holds no group for has no membership to resolve an id against, and is
//! refused `ValidationError` (HTTP 400) with the absent-group sentence whose detail names
//! the group it was asked about, whatever instance ids the call carries and whether they
//! belong to some other group of the account. Only once the group is resolved does an id
//! that group does not hold decide the refusal, in the sentence naming that instance and
//! that group.
//!
//! The rows span what the rule generalises over: how the name comes to hold no group (one
//! never created, one deleted after it existed, one differing from a live group only in
//! letter case, one that merely extends a live group's name), what the named ids are (a
//! fabricated id, the shorter id form Amazon EC2 also mints, an instance of another live
//! group of the same account, the instance a deleted group held) and how many ids the
//! request names. One row per refusal class reads the live groups back through
//! `DescribeAutoScalingGroups`, so no refusal can pass while having written something,
//! and a positive control protects a real member first, so a blanket refusal cannot pass
//! either.
//!
//! The harness (the group, the call and the reader) is the one the
//! `instance_scale_in_protection` suite drives, not a second copy of it.

use smithy_server_runtime::MultiServiceRouter;

use crate::instance_scale_in_protection::{
    absent_group_message, create_group, not_in_group_message, reported_instances, set_protection,
};
use crate::support::{new_engine, query, router_with};

/// The live group every row keeps beside the absent name: it holds the instance the
/// positive control protects, and the read-back that shows a refusal wrote nothing.
const LIVE_GROUP: &str = "asg-live";

/// A second live group, so a row can name an instance that really is part of SOME group
/// of the account while still not being part of the group the request named.
const OTHER_GROUP: &str = "asg-other-live";

/// The group a row creates and then deletes, so the name it refuses against is one that
/// held a group — and an instance — a moment earlier.
const DELETED_GROUP: &str = "asg-deleted";

/// How the name the request carries comes to hold no group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbsentName {
    /// A name no group of the account was ever created with.
    NeverCreated(&'static str),
    /// The name of a group that was created and then deleted.
    Deleted,
    /// The live group's name in different letter case: a group is resolved by its whole
    /// name, case included.
    LiveGroupOtherCase,
    /// The live group's name with more characters after it.
    LiveGroupExtended,
}

/// Which instance id a row names, resolved once the groups exist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NamedId {
    /// A well-formed 17-hexadecimal-digit id no instance of the account has.
    Fabricated(usize),
    /// The shorter 8-hexadecimal-digit id form Amazon EC2 also mints.
    ShortForm,
    /// The instance the live group holds.
    LiveMember,
    /// The instance the other live group holds.
    OtherMember,
    /// The instance the deleted group held before it was deleted.
    FormerMember,
}

/// One row: the case, the absent name the request carries and the ids it names, in
/// request order.
struct Row {
    case: &'static str,
    absent: AbsentName,
    named: &'static [NamedId],
}

/// A well-formed instance id no instance of the account has, built from `seed` so each
/// row's ids are its own. The ids the account's own instances carry count up from the low
/// end, so a fabricated one sits well above them.
fn fabricated_id(seed: usize) -> String {
    format!("i-0fff{seed:013x}")
}

/// Delete `name` along with the instances it holds, and read the account back: the group
/// is gone once this returns, which is what makes the row's name absent and the instance
/// it held a FORMER member.
async fn delete_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", name), ("ForceDelete", "true")],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting {name:?} must succeed, got {status} with {body:?}"
    );
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "reading {name:?} back must succeed, got {status} with {body:?}"
    );
    assert!(
        !body.contains(name),
        "the account must hold no group named {name:?} after the delete, got {body:?}"
    );
}

#[tokio::test]
async fn a_call_naming_a_group_the_account_does_not_hold_is_refused_for_that_group() {
    let rows = [
        Row {
            case: "a name never created, one fabricated id",
            absent: AbsentName::NeverCreated("asg-never-created"),
            named: &[NamedId::Fabricated(1)],
        },
        Row {
            case: "a name never created, three fabricated ids",
            absent: AbsentName::NeverCreated("asg-never-created-2"),
            named: &[
                NamedId::Fabricated(2),
                NamedId::Fabricated(3),
                NamedId::Fabricated(4),
            ],
        },
        Row {
            case: "a one-character name never created, the short id form",
            absent: AbsentName::NeverCreated("a"),
            named: &[NamedId::ShortForm],
        },
        Row {
            case: "a name carrying dots and underscores, led by an instance of the live group",
            absent: AbsentName::NeverCreated("Team.Service_ASG-01"),
            named: &[NamedId::LiveMember, NamedId::Fabricated(5)],
        },
        Row {
            case: "a name never created, naming an instance of another live group",
            absent: AbsentName::NeverCreated("asg-never-created-3"),
            named: &[NamedId::OtherMember],
        },
        Row {
            case: "the name of a deleted group, naming the instance it held",
            absent: AbsentName::Deleted,
            named: &[NamedId::FormerMember],
        },
        Row {
            case: "the live group's name in another letter case",
            absent: AbsentName::LiveGroupOtherCase,
            named: &[NamedId::LiveMember],
        },
        Row {
            case: "the live group's name with more characters after it",
            absent: AbsentName::LiveGroupExtended,
            named: &[NamedId::OtherMember, NamedId::Fabricated(7)],
        },
    ];

    for row in &rows {
        // One emulator per row: the live groups, their instances and the deleted group of
        // one row never explain another's refusal.
        let router = router_with(new_engine());
        let live = create_group(&router, LIVE_GROUP, 1).await;
        let live_member = live.first().cloned().unwrap_or_else(|| {
            panic!(
                "case {:?}: the live group must hold the instance it was created with",
                row.case
            )
        });
        let other = create_group(&router, OTHER_GROUP, 1).await;
        let other_member = other.first().cloned().unwrap_or_else(|| {
            panic!(
                "case {:?}: the other live group must hold its instance",
                row.case
            )
        });

        // The deleted group's own instance, recorded while the group still held it: once
        // the group is gone, the name and that id are both facts of the account's state
        // rather than values this suite made up.
        let mut former_member = String::new();
        if row.absent == AbsentName::Deleted {
            let held = create_group(&router, DELETED_GROUP, 1).await;
            former_member = held.first().cloned().unwrap_or_else(|| {
                panic!(
                    "case {:?}: the group to be deleted must hold its instance",
                    row.case
                )
            });
            delete_group(&router, DELETED_GROUP).await;
        }

        // The positive control: a real member of a live group IS protected by a call
        // naming that group, so the refusal below is about the name the request carries
        // and not about the operation refusing everything.
        let (status, code, message) =
            set_protection(&router, LIVE_GROUP, std::slice::from_ref(&live_member), true).await;
        assert_eq!(
            status, 200,
            "case {:?}: protecting a real member must succeed, got {status} with {code:?} \
             {message:?}",
            row.case
        );

        let name = match row.absent {
            AbsentName::NeverCreated(name) => name.to_string(),
            AbsentName::Deleted => DELETED_GROUP.to_string(),
            AbsentName::LiveGroupOtherCase => LIVE_GROUP.to_uppercase(),
            AbsentName::LiveGroupExtended => format!("{LIVE_GROUP}-and-more"),
        };
        let requested: Vec<String> = row
            .named
            .iter()
            .copied()
            .map(|named| match named {
                NamedId::Fabricated(seed) => fabricated_id(seed),
                NamedId::ShortForm => "i-0abc1234".to_string(),
                NamedId::LiveMember => live_member.clone(),
                NamedId::OtherMember => other_member.clone(),
                NamedId::FormerMember => former_member.clone(),
            })
            .collect();

        let (status, code, message) = set_protection(&router, &name, &requested, true).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400 for {name:?}, got {status} with {code:?} {message:?}",
            row.case
        );
        assert_eq!(
            code, "ValidationError",
            "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
            row.case
        );
        // The refusal is about the GROUP, and it names the group it was asked about:
        // never the instance sentence, which would claim the call got as far as
        // resolving an id against a membership that does not exist.
        let expected = absent_group_message(&name);
        assert_eq!(
            message, expected,
            "case {:?}: expected the message {expected:?}, got {message:?}",
            row.case
        );

        // Nothing moved: the live groups still hold their instances, the control's
        // protection stands, and the other group's instance was not protected by a call
        // that named it against a group the account does not hold.
        let actual = reported_instances(&router, LIVE_GROUP).await;
        let ids: Vec<&str> = actual
            .iter()
            .map(|instance| instance.instance_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![live_member.as_str()],
            "case {:?}: the refused call left the live group's instances as they were, got \
             {actual:?}",
            row.case
        );
        assert!(
            actual.iter().all(|instance| instance.protected_from_scale_in),
            "case {:?}: the member the control protected stays protected, got {actual:?}",
            row.case
        );
        let actual = reported_instances(&router, OTHER_GROUP).await;
        assert!(
            actual.iter().all(|instance| !instance.protected_from_scale_in),
            "case {:?}: the refused call protected no instance of the other group, got \
             {actual:?}",
            row.case
        );
    }
}

/// The precedence the rule rests on, stated as its own table: with the SAME instance ids,
/// a name that holds no group is refused for the group and a name that holds one is
/// refused for the instance. The ids span an id no instance of the account has, the short
/// id form and an instance of another live group — the refusal class is decided by the
/// group, not by what the id is.
#[tokio::test]
async fn the_group_is_resolved_before_the_instance_ids_it_would_be_measured_against() {
    let cases = [
        ("an id no instance of the account has", None),
        ("the short id form", Some("i-0abc1234")),
    ];

    for (case, short_form) in cases {
        let router = router_with(new_engine());
        let live = create_group(&router, LIVE_GROUP, 1).await;
        let live_member = live
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the live group must hold its instance"));
        let other = create_group(&router, OTHER_GROUP, 1).await;
        let other_member = other
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the other group must hold its instance"));

        let foreign = short_form.map_or(other_member, str::to_string);
        let requested = vec![foreign.clone()];

        // The same ids, against a name that holds no group: the group decides.
        let absent = "asg-absent";
        let (status, code, message) = set_protection(&router, absent, &requested, true).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 against {absent:?}, got {status} with {code:?} \
             {message:?}"
        );
        let expected = absent_group_message(absent);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the absent-group message {expected:?}, got {message:?}"
        );

        // And against the name that holds one: the instance decides.
        let (status, code, message) = set_protection(&router, LIVE_GROUP, &requested, true).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 against {LIVE_GROUP:?}, got {status} with {code:?} \
             {message:?}"
        );
        let expected = not_in_group_message(&foreign, LIVE_GROUP);
        assert_eq!(
            message, expected,
            "case {case:?}: expected the membership message {expected:?}, got {message:?}"
        );

        // And the group that does hold the id it names serves the call, so neither
        // refusal above is the operation refusing everything.
        let (status, code, message) = set_protection(
            &router,
            LIVE_GROUP,
            std::slice::from_ref(&live_member),
            true,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: protecting a real member must succeed, got {status} with {code:?} \
             {message:?}"
        );
    }
}
