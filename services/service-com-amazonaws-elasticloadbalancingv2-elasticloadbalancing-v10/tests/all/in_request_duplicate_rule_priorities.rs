// SPDX-License-Identifier: CC0-1.0

//! What `SetRulePriorities` answers a `RulePriorities` list that carries ONE value twice,
//! told apart from what it answers a conflict with what is STORED.
//!
//! One rule is asserted here, over a set of inputs rather than over the one call a
//! recording carries: a value the request's own list repeats is a request-shape error —
//! the list does not say which of the two entries the caller meant, and no change to
//! stored state can make it satisfiable — so it is refused at input validation under this
//! API's generic input-refusal code, naming the member and the value the list repeated;
//! only a priority a rule OUTSIDE the request already evaluates at is the modelled
//! `PriorityInUse`, which freeing that priority makes placeable.
//!
//! The rows differ along what that rule generalises over: which member the list repeats
//! (the priority, the rule), the repeated value (including both bounds `RulePriority`
//! publishes), how many pairs repeat it (two, three), where the repeats sit in the list
//! (adjacent, with a placeable pair between them), how many pairs the list carries, and
//! whether the two rules belong to one listener or to two. Each refusal is read back
//! through `DescribeRules` on both listeners, so a refused call is also shown to have
//! moved nothing.

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, forward_to, host_header,
};
use crate::provisioning_loop::{
    action, call, create_http_target_group, emulator, first_text, members, p, Reply,
};
use crate::target_group_attributes_and_modification::unescaped;
use smithy_server_runtime::MultiServiceRouter;

/// The code this API answers an input it cannot accept with — the generic validation
/// refusal, which `SetRulePriorities` models no error shape for
/// (`smithy_query(query="operation",operation="SetRulePriorities")`: the three modelled
/// errors are `OperationNotPermittedException`, `PriorityInUseException` and
/// `RuleNotFoundException`).
const VALIDATION_ERROR: &str = "ValidationError";

/// The awsQuery code of `PriorityInUseException`, which `SetRulePriorities` models.
const PRIORITY_IN_USE: &str = "PriorityInUse";

/// The sentence `SetRulePriorities` words a refusal about the SET of priorities it was
/// asked to place with — the wording its state-dependent refusals carry, where the request
/// says nothing about which pair the caller has to change.
const ABOUT_THE_SET: &str = "One or more priorities not found";

/// The lowest and the highest value `RulePriority` admits
/// (`smithy_query(query="shape",shape="RulePriority")`: range 1..50000), so the table
/// drives the repeat at both published bounds as well as in between.
const PRIORITY_BOUNDS: [i32; 2] = [1, 50_000];

/// The priorities the three rules of the listener under test start at.
const LISTENER_PRIORITIES: [i32; 3] = [10, 20, 30];

/// The priorities the two rules of the OTHER listener start at — the SAME numbers, because
/// a priority lives in its listener's own namespace.
const OTHER_PRIORITIES: [i32; 2] = [10, 20];

/// Two listeners of one load balancer, with the rules a caller added to each.
struct Fixture {
    /// The listener the first [`LISTENER_PRIORITIES`] rules belong to.
    listener: String,
    /// Another listener of the same load balancer, carrying the rest.
    other: String,
    /// Every rule a call under test may name: the listener's three first, in creation
    /// order, then the other listener's two.
    rules: Vec<String>,
}

/// The index in [`Fixture::rules`] of the first rule of the OTHER listener.
const FIRST_OTHER_RULE: usize = LISTENER_PRIORITIES.len();

/// Build the fixture on a fresh emulator.
async fn fixture(router: &MultiServiceRouter) -> Fixture {
    let lb = create_load_balancer(router, "duplicate-priority-alb").await;
    let group = create_http_target_group(router, "duplicate-priority-tg").await;
    let listener = create_listener(router, &lb, &group, 80).await;
    let other = create_listener(router, &lb, &group, 8080).await;
    let mut rules = Vec::new();
    for (owner, priorities) in [
        (&listener, LISTENER_PRIORITIES.as_slice()),
        (&other, OTHER_PRIORITIES.as_slice()),
    ] {
        for priority in priorities {
            rules.push(
                add_rule(
                    router,
                    &format!("seed a rule at priority {priority}"),
                    owner,
                    *priority,
                    &[
                        host_header(&format!("p{priority}-{}.example.com", rules.len())),
                        forward_to(&group),
                    ]
                    .concat(),
                )
                .await,
            );
        }
    }
    Fixture {
        listener,
        other,
        rules,
    }
}

/// Send one `SetRulePriorities` naming each `(rule ARN, priority)` pair, in order.
async fn set_priorities(router: &MultiServiceRouter, pairs: &[(String, i32)]) -> Reply {
    let mut form = action("SetRulePriorities");
    for (index, (rule_arn, priority)) in pairs.iter().enumerate() {
        let member = index + 1;
        form.push(p(
            &format!("RulePriorities.member.{member}.RuleArn"),
            rule_arn,
        ));
        form.push(p(
            &format!("RulePriorities.member.{member}.Priority"),
            &priority.to_string(),
        ));
    }
    call(router, &form).await
}

/// Read one listener's rules and answer `(RuleArn, Priority)` for each, in the order the
/// read reported them.
async fn stored_priorities(
    router: &MultiServiceRouter,
    case: &str,
    listener_arn: &str,
) -> Vec<(String, String)> {
    let mut form = action("DescribeRules");
    form.push(p("ListenerArn", listener_arn));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: expected 200 from DescribeRules, got {} {}",
        reply.status, reply.body
    );
    members(&reply.body, "Rules")
        .iter()
        .map(|rule| {
            (
                first_text(rule, "RuleArn").unwrap_or_default(),
                first_text(rule, "Priority").unwrap_or_default(),
            )
        })
        .collect()
}

/// Assert one refusal: its status, its awsQuery code and its whole message.
fn assert_refused(case: &str, reply: &Reply, code: &str, message: &str) {
    let actual = (
        reply.status,
        reply.error_code(),
        unescaped(&reply.text("Message").unwrap_or_default()),
    );
    assert_eq!(
        actual,
        (400, code.to_string(), message.to_string()),
        "case {case}: expected 400 {code} {message:?}, got {actual:?} in {}",
        reply.body
    );
}

/// One row of the duplicate table: the case, the pairs the request carries as
/// `(index into [`Fixture::rules`], priority)`, and the member whose repeated value the
/// refusal must name.
struct Row {
    case: &'static str,
    pairs: Vec<(usize, i32)>,
    /// The member label the refusal states, and the value it repeats — a priority, or the
    /// index of the rule the list names twice.
    repeated: Repeated,
}

/// What the request repeated: a priority value, or the rule at this index of
/// [`Fixture::rules`].
enum Repeated {
    Priority(i32),
    Rule(usize),
}

/// A list that carries one value twice is refused at input validation, naming the member
/// and the value it repeated — and moves nothing.
#[tokio::test]
async fn a_value_the_request_repeats_is_refused_as_a_bad_request_naming_that_value() {
    let rows = vec![
        Row {
            case: "the same priority in two adjacent pairs",
            pairs: vec![(0, 40), (1, 40)],
            repeated: Repeated::Priority(40),
        },
        Row {
            case: "the same priority with a placeable pair between the two",
            pairs: vec![(0, 41), (1, 42), (2, 41)],
            repeated: Repeated::Priority(41),
        },
        Row {
            case: "one priority asked for by three pairs",
            pairs: vec![(0, 43), (1, 43), (2, 43)],
            repeated: Repeated::Priority(43),
        },
        Row {
            case: "the lowest priority the model publishes, twice",
            pairs: vec![(0, PRIORITY_BOUNDS[0]), (1, PRIORITY_BOUNDS[0])],
            repeated: Repeated::Priority(PRIORITY_BOUNDS[0]),
        },
        Row {
            case: "the highest priority the model publishes, twice",
            pairs: vec![(0, PRIORITY_BOUNDS[1]), (1, PRIORITY_BOUNDS[1])],
            repeated: Repeated::Priority(PRIORITY_BOUNDS[1]),
        },
        Row {
            // The list is read before any rule is resolved, so which listeners the two
            // rules belong to decides nothing here: the request is what is malformed.
            case: "two rules of DIFFERENT listeners onto one priority",
            pairs: vec![(0, 44), (FIRST_OTHER_RULE, 44)],
            repeated: Repeated::Priority(44),
        },
        Row {
            // The repeat is found before the store is consulted, so a value a rule outside
            // the request also holds is still answered as the bad request it is.
            case: "a repeated priority that a rule outside the request also holds",
            pairs: vec![(0, LISTENER_PRIORITIES[1]), (2, LISTENER_PRIORITIES[1])],
            repeated: Repeated::Priority(LISTENER_PRIORITIES[1]),
        },
        Row {
            case: "one rule named by two pairs, at two priorities",
            pairs: vec![(1, 45), (1, 46)],
            repeated: Repeated::Rule(1),
        },
        Row {
            case: "one rule named by two pairs of a longer list",
            pairs: vec![(0, 47), (2, 48), (0, 49)],
            repeated: Repeated::Rule(0),
        },
    ];

    let (router, _engine) = emulator();
    let fixture = fixture(&router).await;
    let before = stored_priorities(&router, "before any refusal", &fixture.listener).await;
    let other_before = stored_priorities(&router, "before any refusal", &fixture.other).await;

    for row in rows {
        let case = row.case;
        let pairs: Vec<(String, i32)> = row
            .pairs
            .iter()
            .map(|(index, priority)| (fixture.rules[*index].clone(), *priority))
            .collect();
        // The sentence AWS's input validation states for a member a request carries twice:
        // the fixed text with the repeated value from the request filling its one slot.
        let expected = match row.repeated {
            Repeated::Priority(priority) => {
                format!("Priority '{priority}' was provided multiple times")
            }
            Repeated::Rule(index) => format!(
                "Rule '{}' was provided multiple times",
                fixture.rules[index]
            ),
        };

        let reply = set_priorities(&router, &pairs).await;
        assert_refused(case, &reply, VALIDATION_ERROR, &expected);

        // Nothing was placed: both listeners report exactly the priorities they had.
        let stored = stored_priorities(&router, case, &fixture.listener).await;
        assert_eq!(
            stored, before,
            "case {case}: expected a refused call to move nothing, {before:?}, got {stored:?}"
        );
        let other_after = stored_priorities(&router, case, &fixture.other).await;
        assert_eq!(
            other_after, other_before,
            "case {case}: expected the other listener's rules unchanged at {other_before:?}, \
             got {other_after:?}"
        );
    }
}

/// The complement the rule is told apart from: a priority a rule OUTSIDE the request holds
/// is the modelled conflict, which freeing that priority makes placeable — so a client may
/// resolve it and retry the identical request.
#[tokio::test]
async fn a_priority_held_outside_the_request_stays_the_modelled_conflict() {
    let (router, _engine) = emulator();
    let fixture = fixture(&router).await;

    // Every priority another rule of the SAME listener evaluates at, which the request
    // does not name.
    for taken in [LISTENER_PRIORITIES[1], LISTENER_PRIORITIES[2]] {
        let reply = set_priorities(&router, &[(fixture.rules[0].clone(), taken)]).await;
        assert_refused(
            &format!("the reorder onto the taken priority {taken}"),
            &reply,
            PRIORITY_IN_USE,
            ABOUT_THE_SET,
        );
    }

    // And it IS resolvable: freeing the priority makes the identical request succeed, which
    // is what the two refusal classes mean to a client.
    let freed = LISTENER_PRIORITIES[1];
    let mut form = action("DeleteRule");
    form.push(p("RuleArn", &fixture.rules[1]));
    let deleted = call(&router, &form).await;
    assert_eq!(
        deleted.status, 200,
        "case free the taken priority: expected 200, got {} {}",
        deleted.status, deleted.body
    );
    let reply = set_priorities(&router, &[(fixture.rules[0].clone(), freed)]).await;
    assert_eq!(
        reply.status, 200,
        "case the retry after freeing {freed}: expected 200, got {} {}",
        reply.status, reply.body
    );
    let stored = stored_priorities(&router, "the retry after freeing", &fixture.listener).await;
    assert!(
        stored
            .iter()
            .any(|(arn, priority)| arn == &fixture.rules[0] && priority == &freed.to_string()),
        "case the retry after freeing {freed}: expected the moved rule at {freed}, got {stored:?}"
    );
}
