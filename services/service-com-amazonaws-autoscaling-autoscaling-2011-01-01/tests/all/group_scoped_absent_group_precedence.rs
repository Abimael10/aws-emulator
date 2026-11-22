// SPDX-License-Identifier: CC0-1.0

//! What a group-scoped call answers when the Auto Scaling group it names is not there,
//! and WHICH check decides it, over the awsQuery wire.
//!
//! Two rules, each driven end to end:
//!
//! 1. **An absent-group refusal is the fixed `AutoScalingGroup name not found - ` text
//!    followed by the detail THAT operation reports, with the name taken from the request
//!    wherever the detail carries one.** The detail is each operation's own — the
//!    attachment reads say the group could not be found under the name they were given,
//!    quoted; the warm-pool read says no such group exists; the notification-configuration
//!    writers state the name on its own; the refresh, policy, scheduled-action and
//!    activity reads label the resource and the name — so no single sentence is right for
//!    all of them, and none of them may drop the name the request supplied and leave the
//!    detail field travelling as the literal `null`.
//!
//! 2. **A group-scoped paginated read resolves the group it reports from BEFORE it
//!    validates the pagination members of the request.** A `NextToken` names a page of the
//!    list a group holds and a `MaxRecords` bounds that page, so a name no group answers to
//!    is refused for the GROUP — the service-common `ValidationError` of rule 1 — however
//!    unusable the cursor or the page size travelling with it; a client told its cursor is
//!    stale would restart a page walk over a group that does not exist.
//!
//! The tables span what the rules generalise over: the operation and the detail form it
//! serves, the group name's length, case, character set and spelling (a name spelled like
//! the unfilled detail token among them), whether the name never existed or was created and
//! then deleted, and which pagination members travel with the request (an unissued cursor,
//! a cursor shaped like an issued one, an empty cursor, a page size under the floor, a page
//! size over the ceiling, and a cursor and page size together). Each row asserts the WHOLE
//! message, and each table carries controls on a group that EXISTS — the pagination refusal
//! still fires there, and a read carrying no pagination member is still served — so no row
//! can pass by refusing everything.

use smithy_server_runtime::MultiServiceRouter;

use crate::delete_notification_configuration::{topic_arn, TEST_NOTIFICATION};
use crate::lifecycle_hooks::create_group;
use crate::support::{borrowed, new_engine, p, query, router_with, tag_value, xml_unescape};

/// The service-common error code this service answers a bad request under.
const VALIDATION_ERROR: &str = "ValidationError";

/// The fixed text every absent-group refusal of this service opens with. Spelled out here
/// rather than imported, so a wrong constant in the implementation cannot make a row pass.
const ABSENT_GROUP_PREFIX: &str = "AutoScalingGroup name not found - ";

/// The unfilled detail token a refusal that drops the name it was given travels with: no
/// row of this suite may answer it, because every operation here reports the name.
const UNFILLED_DETAIL: &str = "null";

/// The read whose name keeps naming something after the group is gone: a deleted group's
/// scaling activities stay in the account's own log.
const ACTIVITY_READ: &str = "DescribeScalingActivities";

/// The detail form one operation reports after [`ABSENT_GROUP_PREFIX`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detail {
    /// The group could not be found under the name asked for, QUOTED — the form the
    /// attachment reads serve.
    NotFindable,
    /// No such group exists, naming the one asked for — the form the warm-pool
    /// operations serve.
    NoSuchGroup,
    /// The name asked for ALONE — the form the notification-configuration writers serve.
    BareName,
    /// The resource labelled, then the name asked for — the form the refresh, policy,
    /// scheduled-action and activity reads serve.
    LabelledName,
}

impl Detail {
    /// The WHOLE message this form states for `name`: the fixed prefix, then this form's
    /// own fixed text around the one variable part, the name the request carried.
    fn message(self, name: &str) -> String {
        match self {
            Self::NotFindable => {
                format!("{ABSENT_GROUP_PREFIX}Could not find Auto Scaling group named '{name}'")
            }
            Self::NoSuchGroup => {
                format!("{ABSENT_GROUP_PREFIX}No such Auto Scaling group: {name}")
            }
            Self::BareName => format!("{ABSENT_GROUP_PREFIX}{name}"),
            Self::LabelledName => {
                format!("{ABSENT_GROUP_PREFIX}AutoScalingGroup {name} not found")
            }
        }
    }
}

/// One group-scoped call: the action, the members it carries beside `AutoScalingGroupName`,
/// and the detail form its absent-group refusal serves.
struct GroupScopedCall {
    action: &'static str,
    extra: Vec<(String, String)>,
    detail: Detail,
}

/// The group-scoped calls this suite drives, one per detail form and then some: the two
/// attachment reads and their superseding read, the warm-pool read, both
/// notification-configuration writers, and the four reads a group scopes by name.
fn group_scoped_calls() -> Vec<GroupScopedCall> {
    let topic = topic_arn("asg-absent-group-topic");
    vec![
        GroupScopedCall {
            action: "DescribeLoadBalancerTargetGroups",
            extra: Vec::new(),
            detail: Detail::NotFindable,
        },
        GroupScopedCall {
            action: "DescribeLoadBalancers",
            extra: Vec::new(),
            detail: Detail::NotFindable,
        },
        GroupScopedCall {
            action: "DescribeTrafficSources",
            extra: Vec::new(),
            detail: Detail::NotFindable,
        },
        GroupScopedCall {
            action: "DescribeWarmPool",
            extra: Vec::new(),
            detail: Detail::NoSuchGroup,
        },
        GroupScopedCall {
            action: "DeleteNotificationConfiguration",
            extra: vec![p("TopicARN", topic.clone())],
            detail: Detail::BareName,
        },
        GroupScopedCall {
            action: "PutNotificationConfiguration",
            extra: vec![
                p("TopicARN", topic),
                p("NotificationTypes.member.1", TEST_NOTIFICATION),
            ],
            detail: Detail::BareName,
        },
        GroupScopedCall {
            action: "DescribeInstanceRefreshes",
            extra: Vec::new(),
            detail: Detail::LabelledName,
        },
        GroupScopedCall {
            action: "DescribePolicies",
            extra: Vec::new(),
            detail: Detail::LabelledName,
        },
        GroupScopedCall {
            action: "DescribeScheduledActions",
            extra: Vec::new(),
            detail: Detail::LabelledName,
        },
        GroupScopedCall {
            action: ACTIVITY_READ,
            extra: Vec::new(),
            detail: Detail::LabelledName,
        },
    ]
}

/// Group names a client can submit for a group that does not exist, differing in length,
/// case, character set, separators and spelling. `AutoScalingGroupName` is
/// `XmlStringMaxLen255` (len 1..255,
/// `smithy_query(query="operation",operation="DescribeLoadBalancerTargetGroups")`), so the
/// boundary validator admits every row here and only a handler can refuse it.
fn absent_group_names() -> Vec<(&'static str, String)> {
    let mut names: Vec<(&'static str, String)> = [
        ("the shortest name the model allows", "a"),
        ("a name of the shape the recorded calls used", "probe-asg-0123456789ab"),
        ("an upper-case name", "ASG-UPPER-CASE"),
        ("a name carrying every separator the model allows", "asg.name_with-separators 1"),
        ("a name spelled like the unfilled detail token", UNFILLED_DETAIL),
        ("a name spelled like a part of the detail sentence", "AutoScalingGroup"),
        ("a name carrying XML metacharacters", "<asg & 'name' \"quoted\">"),
        ("a non-ASCII name", "グループ-ünïcode"),
    ]
    .into_iter()
    .map(|(case, name)| (case, name.to_string()))
    .collect();
    // The longest name the member's length constraint admits.
    names.push(("the longest name the model allows", "z".repeat(255)));
    names
}

/// Post one call and answer its status, wire error code and unescaped message.
async fn answer(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(String, String)],
) -> (u16, String, String) {
    let (status, body) = query(router, action, &borrowed(params)).await;
    (
        status,
        tag_value(&body, "Code"),
        xml_unescape(&tag_value(&body, "Message")),
    )
}

/// Drive one group-scoped call for `name` and assert the whole absent-group refusal it
/// serves: HTTP 400, the service-common code, the detail form that operation reports, and
/// the name the request carried inside it.
async fn assert_absent_group_refusal(
    router: &MultiServiceRouter,
    call: &GroupScopedCall,
    name_case: &str,
    name: &str,
    pagination_case: &str,
    pagination: &[(String, String)],
) {
    let case = format!(
        "{} for {name_case} ({name:?}) with {pagination_case}",
        call.action
    );
    let mut params = vec![p("AutoScalingGroupName", name)];
    params.extend(call.extra.iter().cloned());
    params.extend(pagination.iter().cloned());
    let (status, code, message) = answer(router, call.action, &params).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {message:?}"
    );
    assert_eq!(
        code, VALIDATION_ERROR,
        "case {case:?}: expected the error code {VALIDATION_ERROR:?}, got {code:?} with \
         {message:?}"
    );
    let expected = call.detail.message(name);
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
    assert!(
        message.contains(name),
        "case {case:?}: the refusal must name the group the request asked for, got {message:?}"
    );
}

/// RULE 1: every group-scoped call names the group it could not find, in the detail form
/// that operation reports — for a name that never existed and for one created and deleted.
#[tokio::test]
async fn an_absent_group_is_refused_in_each_operations_own_detail_form() {
    let router = router_with(new_engine());
    let calls = group_scoped_calls();

    for (name_case, name) in absent_group_names() {
        for call in &calls {
            assert_absent_group_refusal(
                &router,
                call,
                name_case,
                &name,
                "no pagination member",
                &[],
            )
            .await;
        }
    }

    // A name the account HELD and no longer does is refused exactly like one that never
    // existed: the refusal is about what the store holds now.
    let deleted = "asg-created-then-deleted";
    create_group(&router, deleted).await;
    let (status, body) = query(
        &router,
        "DeleteAutoScalingGroup",
        &[("AutoScalingGroupName", deleted)],
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting {deleted:?} must succeed, got {status} with {body:?}"
    );
    for call in &calls {
        // The activity log is the one read a deleted group's name still names something
        // in: "activities are sorted by start time" over the account's own log, and
        // `IncludeDeletedGroups` exists precisely because the activities of a group the
        // account no longer holds are still addressable by its name. It is asserted
        // below instead.
        if call.action == ACTIVITY_READ {
            continue;
        }
        assert_absent_group_refusal(
            &router,
            call,
            "a name created and then deleted",
            deleted,
            "no pagination member",
            &[],
        )
        .await;
    }
    let (status, body) = query(
        &router,
        ACTIVITY_READ,
        &[("AutoScalingGroupName", deleted)],
    )
    .await;
    assert_eq!(
        status, 200,
        "the activity log of the deleted group {deleted:?} must still be served, got {status} \
         with {body:?}"
    );
}

/// The group-scoped paginated reads, with the detail form each one's absent-group refusal
/// serves: the subset of [`group_scoped_calls`] that pages.
fn paginated_reads() -> Vec<GroupScopedCall> {
    group_scoped_calls()
        .into_iter()
        .filter(|call| call.action.starts_with("Describe"))
        .collect()
}

/// The pagination member sets a request can carry beside the group name. `MaxRecords` is
/// refused below 1 and above the ceiling each read documents, and a `NextToken` this
/// service never issued names no page — so every row here would be refused on its own if
/// the group existed.
fn pagination_member_sets() -> Vec<(&'static str, Vec<(String, String)>)> {
    vec![
        (
            "a cursor this service never issued",
            vec![p("NextToken", "not-a-token-this-service-issued")],
        ),
        (
            "a cursor shaped like an issued one",
            vec![p("NextToken", "next-5")],
        ),
        ("an empty cursor", vec![p("NextToken", "")]),
        (
            "a page size under the floor every read holds",
            vec![p("MaxRecords", "0")],
        ),
        (
            "a page size over the ceiling every read holds",
            vec![p("MaxRecords", "101")],
        ),
        (
            "a cursor and a page size together",
            vec![p("NextToken", "next-5"), p("MaxRecords", "0")],
        ),
    ]
}

/// RULE 2: a group-scoped paginated read resolves the group before the pagination members,
/// so the refusal is the absent-group one whatever cursor or page size travels with it.
#[tokio::test]
async fn a_paginated_read_resolves_the_group_before_its_pagination_members() {
    let router = router_with(new_engine());
    let reads = paginated_reads();
    let names = [
        ("a name that never existed", "asg-no-such-group".to_string()),
        ("an upper-case name", "ASG-PAGED".to_string()),
        (
            "a name spelled like the unfilled detail token",
            UNFILLED_DETAIL.to_string(),
        ),
    ];

    for (name_case, name) in &names {
        for read in &reads {
            for (pagination_case, pagination) in pagination_member_sets() {
                assert_absent_group_refusal(
                    &router,
                    read,
                    name_case,
                    name,
                    pagination_case,
                    &pagination,
                )
                .await;
            }
        }
    }

    // The controls, on a group that EXISTS: the cursor is still checked and still refused
    // naming the token submitted — never as an absent group — and a read carrying no
    // pagination member at all is still served.
    let held = "asg-paged";
    create_group(&router, held).await;
    let stale = "not-a-token-this-service-issued";
    for read in &reads {
        let case = format!("{} on the stored group {held:?}", read.action);
        let mut params = vec![p("AutoScalingGroupName", held), p("NextToken", stale)];
        params.extend(read.extra.iter().cloned());
        let (status, code, message) = answer(&router, read.action, &params).await;
        assert_eq!(
            status, 400,
            "case {case:?}: the stale cursor must be refused, got {status} with {message:?}"
        );
        assert!(
            message.contains(stale),
            "case {case:?}: the refusal must name the cursor it rejected, got {message:?} with \
             the code {code:?}"
        );
        assert!(
            !message.starts_with(ABSENT_GROUP_PREFIX),
            "case {case:?}: a stored group must not be reported as absent, got {message:?}"
        );

        let mut served = vec![p("AutoScalingGroupName", held)];
        served.extend(read.extra.iter().cloned());
        let (status, body) = query(&router, read.action, &borrowed(&served)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a read carrying no pagination member must be served, got {status} \
             with {body:?}"
        );
    }
}
