// SPDX-License-Identifier: CC0-1.0

//! What `DescribeRules` answers a request that ENUMERATES the rules it wants, driven over
//! the wire exactly as a client drives it.
//!
//! One rule is asserted here, over a SET of inputs rather than over the one call a
//! recording happens to carry: `Marker` and `PageSize` name a page of the collection a
//! read walks — the rules of one listener — so a request that names the rules to report
//! has already stated the whole answer and is given all of it in one page, with no
//! `NextMarker`, whatever page members it carries.
//!
//! The rows differ along what that rule generalises over: how many rules the selector
//! names, the order it names them in, whether it repeats one, whether the listener is
//! named beside it, and which page members the request carries — none, a page size far
//! below the selection, one at the published maximum, a marker this read issued, and a
//! value this store never issued. Beside them the same read's walk of the listener's own
//! collection is asserted to page as before, which is what makes the rule about the
//! selection rather than about page members being ignored everywhere.

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, forward_to, host_header, rules_of, summaries,
};
use crate::provisioning_loop::{
    action, call, create_http_target_group, emulator, first_text, members, p, Reply,
};
use smithy_server_runtime::MultiServiceRouter;

/// The priorities of the rules hung on the listener under test, in evaluation order. Four
/// of them, so a selection can leave a rule out on either side and a page size can fall
/// well inside the selection.
const PRIORITIES: &[i32] = &[10, 20, 30, 40];

/// One row of the selection table: the case, the rules the selector names as indices into
/// [`PRIORITIES`] (in the order the request lists them), and whether the request names the
/// listener beside them.
struct SelectionRow {
    case: &'static str,
    named: &'static [usize],
    with_listener: bool,
}

/// The selections, differing in how many rules they name, in the order they name them, in
/// whether one is repeated, and in whether the listener is named beside them.
const SELECTIONS: &[SelectionRow] = &[
    SelectionRow {
        case: "one rule",
        named: &[0],
        with_listener: false,
    },
    SelectionRow {
        case: "two rules, the later one first",
        named: &[3, 1],
        with_listener: false,
    },
    SelectionRow {
        case: "every rule of the listener but its default",
        named: &[0, 1, 2, 3],
        with_listener: false,
    },
    SelectionRow {
        case: "a rule named twice beside another",
        named: &[2, 2, 0],
        with_listener: false,
    },
    SelectionRow {
        case: "one rule, beside the listener that holds it",
        named: &[1],
        with_listener: true,
    },
    SelectionRow {
        case: "three rules, beside the listener that holds them",
        named: &[2, 0, 3],
        with_listener: true,
    },
];

/// Send one `DescribeRules` request carrying `fields`.
async fn describe(router: &MultiServiceRouter, fields: &[(String, String)]) -> Reply {
    let mut form = action("DescribeRules");
    form.extend(fields.iter().cloned());
    call(router, &form).await
}

/// The `RuleArn` of every rule in one reply, in the order it reported them.
fn reported_arns(reply: &Reply) -> Vec<String> {
    members(&reply.body, "Rules")
        .iter()
        .filter_map(|rule| first_text(rule, "RuleArn"))
        .collect()
}

/// The rules `row` asks for, as the read must report them: each named rule once —
/// repeating an ARN asks for the same rule — in the listener's evaluation order when the
/// listener is named beside the selection, and in the order the selector listed them
/// otherwise.
fn expected_arns(row: &SelectionRow, arns: &[String]) -> Vec<String> {
    let mut indices: Vec<usize> = Vec::new();
    for index in row.named {
        if !indices.contains(index) {
            indices.push(*index);
        }
    }
    if row.with_listener {
        indices.sort_unstable();
    }
    indices.iter().map(|index| arns[*index].clone()).collect()
}

/// The request fields of one selection: the rules it names, and the listener when the row
/// names it too.
fn selection_fields(
    row: &SelectionRow,
    listener_arn: &str,
    arns: &[String],
) -> Vec<(String, String)> {
    let mut fields: Vec<(String, String)> = Vec::new();
    if row.with_listener {
        fields.push(p("ListenerArn", listener_arn));
    }
    for (position, index) in row.named.iter().enumerate() {
        fields.push(p(
            &format!("RuleArns.member.{}", position + 1),
            &arns[*index],
        ));
    }
    fields
}

#[tokio::test]
async fn a_rule_arns_selection_is_answered_whole_whatever_page_members_it_carries() {
    let (router, _engine) = emulator();
    let load_balancer = create_load_balancer(&router, "rule-selection-alb").await;
    let group = create_http_target_group(&router, "rule-selection-tg").await;
    let listener = create_listener(&router, &load_balancer, &group, 80).await;

    let mut arns: Vec<String> = Vec::new();
    for priority in PRIORITIES {
        arns.push(
            add_rule(
                &router,
                &format!("the rule at priority {priority}"),
                &listener,
                *priority,
                &[
                    host_header(&format!("p{priority}.example.com")),
                    forward_to(&group),
                ]
                .concat(),
            )
            .await,
        );
    }

    let whole = rules_of(&router, "the listener's own collection", &listener).await;
    assert_eq!(
        whole.len(),
        PRIORITIES.len() + 1,
        "case the listener's own collection: expected the added rules and the default rule, \
         got {whole:?}"
    );

    // The marker a walk of this listener's rules hands back, which is the one value a
    // caller could legitimately resume such a walk from.
    let first_page = describe(&router, &[p("ListenerArn", &listener), p("PageSize", "1")]).await;
    let issued = first_page.text("NextMarker").unwrap_or_else(|| {
        panic!(
            "case the first page of the collection: expected a NextMarker, got {}",
            first_page.body
        )
    });

    // (the case, the page members the request carries beside its selection)
    //
    // `PageSize` is modelled `range 1..400`
    // (`smithy_query(query="shape",shape="PageSize")`), so 400 is its published maximum
    // and 1 the smallest page a caller can ask for.
    let paging: Vec<(&str, Vec<(String, String)>)> = vec![
        ("no page member", Vec::new()),
        ("the smallest page size", vec![p("PageSize", "1")]),
        ("a page size inside the selection", vec![p("PageSize", "2")]),
        ("the largest page size", vec![p("PageSize", "400")]),
        ("a marker this read issued", vec![p("Marker", &issued)]),
        (
            "a marker this read issued and a page size",
            vec![p("Marker", &issued), p("PageSize", "1")],
        ),
        (
            "a value this store never issued as a marker",
            vec![p("Marker", "not-a-marker")],
        ),
    ];

    for row in SELECTIONS {
        let expected = expected_arns(row, &arns);
        for (paging_case, page_members) in &paging {
            let case = format!("{} with {paging_case}", row.case);
            let mut fields = selection_fields(row, &listener, &arns);
            fields.extend(page_members.iter().cloned());
            let reply = describe(&router, &fields).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected 200, got {} body {}",
                reply.status, reply.body
            );
            assert_eq!(
                reported_arns(&reply),
                expected,
                "case {case:?}: expected every rule the selector names in one page, got {}",
                reply.body
            );
            assert_eq!(
                reply.text("NextMarker"),
                None,
                "case {case:?}: a selection leaves no page to resume, got {}",
                reply.body
            );
            let reported_priorities: Vec<String> = summaries(&reply)
                .into_iter()
                .map(|(priority, _)| priority)
                .collect();
            let expected_priorities: Vec<String> = expected
                .iter()
                .map(|arn| {
                    let index = arns
                        .iter()
                        .position(|held| held == arn)
                        .expect("a selected rule is one of the rules added above");
                    PRIORITIES[index].to_string()
                })
                .collect();
            assert_eq!(
                reported_priorities, expected_priorities,
                "case {case:?}: expected the priorities of the rules it named, got {}",
                reply.body
            );
        }
    }

    // The page members still bound a WALK of the listener's collection: that is the
    // collection they name a page of. A page holds what it asked for, and a `NextMarker`
    // comes back only while rules remain beyond it.
    for page_size in 1..=whole.len() {
        let reply = describe(
            &router,
            &[
                p("ListenerArn", &listener),
                p("PageSize", &page_size.to_string()),
            ],
        )
        .await;
        let page = summaries(&reply);
        assert_eq!(
            page.len(),
            page_size,
            "case a walk at PageSize {page_size}: expected a full page, got {page:?}"
        );
        assert_eq!(
            reply.text("NextMarker").is_some(),
            page_size < whole.len(),
            "case a walk at PageSize {page_size}: a NextMarker comes back only while rules \
             remain, got {}",
            reply.body
        );
    }

    // Reading reports; it changes nothing a later caller sees.
    assert_eq!(
        rules_of(&router, "after every read", &listener).await,
        whole,
        "case after every read: the listener's rules are unchanged"
    );
}
