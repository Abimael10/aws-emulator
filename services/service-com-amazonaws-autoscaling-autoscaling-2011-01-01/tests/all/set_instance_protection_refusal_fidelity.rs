// SPDX-License-Identifier: CC0-1.0

//! How `SetInstanceProtection` REFUSES, over the awsQuery wire.
//!
//! Two rules, one per table:
//!
//! 1. A bound on how many instance ids ONE call may name is a fact of the REQUEST's
//!    shape, so a call past it is refused with the service-common `ValidationError`
//!    (HTTP 400, `Sender`) quoting the documented bound — never with the operation's
//!    modelled `LimitExceededFault`, which the model documents as "You have already
//!    reached a limit for your Amazon EC2 Auto Scaling resources (for example, Auto
//!    Scaling groups, launch configurations, or lifecycle hooks)"
//!    (`smithy_query(query="shape",shape="LimitExceededFault")`) — an account-resource
//!    condition no shape of request can cause. The count is checked AHEAD of the group
//!    and of any per-id membership resolution, so what the ids name, and whether the
//!    group exists at all, does not change the refusal.
//!
//! 2. An instance id the named Auto Scaling group does not hold is refused with that
//!    same `ValidationError`, in one sentence naming BOTH variable parts the caller
//!    sent: the offending instance id and the group it is not part of.
//!
//! The rows span what each rule generalises over — how far past the bound the call is,
//! what its ids name, whether the group exists, how the group is named and what form the
//! foreign id takes — and every row reads the group back through
//! `DescribeAutoScalingGroups`, so no refusal can pass while having written something.
//! The harness (the group, the call and the reader) is the one the sibling
//! `instance_scale_in_protection` suite drives, not a second copy of it.

use crate::instance_scale_in_protection::{
    create_group, not_in_group_message, reported_instances, set_protection,
    too_many_instance_ids_message, MAX_INSTANCE_IDS_PER_CALL,
};
use crate::support::{new_engine, router_with};

/// A well-formed instance id no instance of the account has, built from `seed` so each
/// row's ids are its own. The 17-hexadecimal-digit form Amazon EC2 mints, in a range the
/// account's own instances are not minted from — their ids count up from the low end, so
/// a fabricated id has to sit well above them to be foreign at all.
fn fabricated_id(seed: usize) -> String {
    format!("i-0fff{seed:013x}")
}

/// What the ids of an oversized call name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ids {
    /// Every id is fabricated: none belongs to any group of the account.
    AllForeign,
    /// The group's own single instance first, then fabricated ids.
    MemberFirst,
}

/// One oversized-call row: the case, how many ids the call names, what those ids are, and
/// whether the group the call names exists at all.
struct CountRow {
    case: &'static str,
    ids: usize,
    naming: Ids,
    group_exists: bool,
}

/// Past the documented bound the refusal is the same whatever the ids name and whether
/// the group exists: the count of ids is what is wrong with the request.
#[tokio::test]
async fn an_oversized_call_is_refused_as_a_validation_error() {
    let rows = [
        CountRow {
            case: "one id past the bound, none of them a member",
            ids: MAX_INSTANCE_IDS_PER_CALL + 1,
            naming: Ids::AllForeign,
            group_exists: true,
        },
        CountRow {
            case: "two ids past the bound, none of them a member",
            ids: MAX_INSTANCE_IDS_PER_CALL + 2,
            naming: Ids::AllForeign,
            group_exists: true,
        },
        CountRow {
            case: "far past the bound",
            ids: MAX_INSTANCE_IDS_PER_CALL * 2 + 7,
            naming: Ids::AllForeign,
            group_exists: true,
        },
        CountRow {
            case: "one id past the bound, led by a real member of the group",
            ids: MAX_INSTANCE_IDS_PER_CALL + 1,
            naming: Ids::MemberFirst,
            group_exists: true,
        },
        CountRow {
            case: "one id past the bound, for a group the account does not hold",
            ids: MAX_INSTANCE_IDS_PER_CALL + 1,
            naming: Ids::AllForeign,
            group_exists: false,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row, so the group and the ids of one row never explain
        // another's.
        let router = router_with(new_engine());
        let held = create_group(&router, "asg-count", 1).await;
        let member = held.first().cloned().unwrap_or_else(|| {
            panic!(
                "case {:?}: the group must hold the instance it was created with",
                row.case
            )
        });

        // A positive control: the member really is protected before the refusal, so the
        // read-back below shows the refused call wrote nothing rather than showing the
        // flag it never set.
        let (status, code, message) =
            set_protection(&router, "asg-count", std::slice::from_ref(&member), true).await;
        assert_eq!(
            status, 200,
            "case {:?}: protecting the member first must succeed, got {status} with {code:?} \
             {message:?}",
            row.case
        );

        let mut requested: Vec<String> = Vec::new();
        if row.naming == Ids::MemberFirst {
            requested.push(member.clone());
        }
        while requested.len() < row.ids {
            requested.push(fabricated_id(index * 1_000 + requested.len()));
        }
        let name = if row.group_exists {
            "asg-count"
        } else {
            "asg-count-absent"
        };

        let (status, code, message) = set_protection(&router, name, &requested, false).await;
        assert_eq!(
            status, 400,
            "case {:?}: a call naming {} ids must be refused, got {status} with {code:?} \
             {message:?}",
            row.case,
            requested.len()
        );
        assert_eq!(
            code, "ValidationError",
            "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
            row.case
        );
        let expected = too_many_instance_ids_message();
        assert_eq!(
            message, expected,
            "case {:?}: expected the message {expected:?}, got {message:?}",
            row.case
        );

        let actual = reported_instances(&router, "asg-count").await;
        assert_eq!(
            actual.len(),
            1,
            "case {:?}: the refused call leaves the group holding its instance, got {actual:?}",
            row.case
        );
        assert!(
            actual.iter().all(|instance| instance.protected_from_scale_in),
            "case {:?}: the refused call left the member's protection as it was, got {actual:?}",
            row.case
        );
    }
}

/// AT the bound the count is satisfied, so the per-id membership resolution is what
/// decides: a call naming exactly the maximum, one of whose ids the group does not hold,
/// is refused with the membership sentence rather than the count one.
#[tokio::test]
async fn a_call_at_the_bound_is_refused_on_membership_instead() {
    let router = router_with(new_engine());
    let held = create_group(&router, "asg-at-bound", 1).await;
    let member = held
        .first()
        .cloned()
        .expect("the group must hold the instance it was created with");

    let mut requested = vec![member];
    while requested.len() < MAX_INSTANCE_IDS_PER_CALL {
        requested.push(fabricated_id(requested.len()));
    }
    assert_eq!(
        requested.len(),
        MAX_INSTANCE_IDS_PER_CALL,
        "the call must name exactly the documented maximum"
    );

    let (status, code, message) = set_protection(&router, "asg-at-bound", &requested, true).await;
    assert_eq!(
        status, 400,
        "a call naming an id the group does not hold must be refused, got {status} with \
         {code:?} {message:?}"
    );
    assert_eq!(
        code, "ValidationError",
        "expected the ValidationError code, got {code:?} {message:?}"
    );
    let expected = not_in_group_message(&requested[1], "asg-at-bound");
    assert_eq!(
        message, expected,
        "expected the membership message {expected:?} at the bound, got {message:?}"
    );

    let actual = reported_instances(&router, "asg-at-bound").await;
    assert!(
        actual.iter().all(|instance| !instance.protected_from_scale_in),
        "a refused call protects nothing, got {actual:?}"
    );
}

/// Where the foreign id of a membership row comes from.
#[derive(Debug, Clone, Copy)]
enum ForeignId {
    /// A well-formed id no instance of the account has, in the 17-digit form.
    Fabricated(usize),
    /// The shorter 8-hexadecimal-digit form Amazon EC2 also mints.
    ShortForm,
    /// The instance of another Auto Scaling group of the same account.
    OtherGroup,
}

/// One membership row: the case, the group the call names and the foreign id it names.
struct MembershipRow {
    case: &'static str,
    /// How the group is named, spanning the member's own bounds: a one-character name,
    /// names carrying the punctuation a name may carry, and a generated-looking one.
    group: &'static str,
    foreign: ForeignId,
}

/// The sentence names both variable parts the request sent — the offending instance id
/// and the group name — whatever the two are, and the refusal writes nothing: the
/// foreign id does not join the group and a member protected by an earlier call stays
/// protected.
#[tokio::test]
async fn a_non_member_is_refused_with_the_instance_and_the_group_named() {
    let rows = [
        MembershipRow {
            case: "a fabricated id against a one-character group name",
            group: "a",
            foreign: ForeignId::Fabricated(1),
        },
        MembershipRow {
            case: "a fabricated id against a dashed group name",
            group: "asg-protection-refusal",
            foreign: ForeignId::Fabricated(2),
        },
        MembershipRow {
            case: "a fabricated id against a group name carrying dots and underscores",
            group: "Team.Service_ASG-01",
            foreign: ForeignId::Fabricated(3),
        },
        MembershipRow {
            case: "a short-form instance id",
            group: "asg-short-form",
            foreign: ForeignId::ShortForm,
        },
        MembershipRow {
            case: "an instance of another group of the account",
            group: "asg-two-groups",
            foreign: ForeignId::OtherGroup,
        },
    ];

    for row in &rows {
        // One emulator per row: the other group's instance is this row's foreign id
        // alone.
        let router = router_with(new_engine());
        let held = create_group(&router, row.group, 1).await;
        let member = held.first().cloned().unwrap_or_else(|| {
            panic!(
                "case {:?}: the group must hold the instance it was created with",
                row.case
            )
        });

        let foreign = match row.foreign {
            ForeignId::Fabricated(seed) => fabricated_id(seed),
            ForeignId::ShortForm => "i-0abc1234".to_string(),
            ForeignId::OtherGroup => create_group(&router, "asg-elsewhere", 1)
                .await
                .first()
                .cloned()
                .unwrap_or_else(|| {
                    panic!("case {:?}: the other group must hold an instance", row.case)
                }),
        };

        // The positive control of the check this row generalises: a real member is
        // protected by a call that succeeds, and the refusal that follows leaves it so.
        let (status, code, message) =
            set_protection(&router, row.group, std::slice::from_ref(&member), true).await;
        assert_eq!(
            status, 200,
            "case {:?}: protecting the member must succeed, got {status} with {code:?} \
             {message:?}",
            row.case
        );

        let (status, code, message) =
            set_protection(&router, row.group, std::slice::from_ref(&foreign), true).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {code:?} {message:?}",
            row.case
        );
        assert_eq!(
            code, "ValidationError",
            "case {:?}: expected the ValidationError code, got {code:?} {message:?}",
            row.case
        );
        let expected = not_in_group_message(&foreign, row.group);
        assert_eq!(
            message, expected,
            "case {:?}: expected the message {expected:?}, got {message:?}",
            row.case
        );

        let actual = reported_instances(&router, row.group).await;
        let ids: Vec<&str> = actual
            .iter()
            .map(|instance| instance.instance_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![member.as_str()],
            "case {:?}: the refused id did not join the group, got {actual:?}",
            row.case
        );
        assert!(
            actual.iter().all(|instance| instance.protected_from_scale_in),
            "case {:?}: the member protected by the earlier call stays protected, got \
             {actual:?}",
            row.case
        );
    }
}
