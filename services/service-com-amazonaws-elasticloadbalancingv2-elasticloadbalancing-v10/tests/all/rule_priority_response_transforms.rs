// SPDX-License-Identifier: CC0-1.0

//! Which members a RE-PRIORITISED rule is reported with, driven over the wire exactly as a
//! client drives it: rules added with `CreateRule`, moved with `SetRulePriorities` and read
//! back with `DescribeRules`.
//!
//! One rule is asserted here, over a SET of rules rather than over the one call a recording
//! happens to carry: `SetRulePriorities` reports `Rule$Transforms` only for a rule that
//! actually holds a transform, and leaves the member OUT of a rule that holds none — where
//! the operations reporting a rule's whole configuration (the create, the modification and
//! the read) report that member present and empty. An absent member and an empty list are
//! different answers on this wire form, so both halves of the split are asserted on the same
//! rules in the same run.
//!
//! The rows differ along what that rule generalises over: how many transforms a rule holds
//! (none, one, one of each declared type), which type it holds, how the rule came to hold
//! them (named on the create, put there by a modification, or removed again by one), and how
//! many rules one call moves (a single rule, and every rule of the listener at once).

use crate::listener_rules::{
    add_rule, create_listener, create_load_balancer, forward_to, host_header,
    host_header_rewrite, url_rewrite, Form,
};
use crate::provisioning_loop::{
    action, call, create_http_target_group, emulator, first_text, members, p,
};
use crate::rule_deletion_and_condition_value_spellings::describe_rule;
use smithy_server_runtime::MultiServiceRouter;

/// One row of the table: a rule, the transforms it ends up holding, and the priority it
/// starts at.
struct Row {
    case: &'static str,
    /// The priority the rule is created at; every row's is distinct, so the moves below
    /// never collide.
    priority: i32,
    /// The `Transforms.member.*` fields the CREATE carries.
    created_with: Form,
    /// The request fields a `ModifyRule` call applies afterwards, or `None` when the rule is
    /// left exactly as it was created.
    modified_with: Option<Form>,
    /// The `Type` of every transform the rule then holds, in order — empty for a rule
    /// holding none.
    transforms: Vec<&'static str>,
}

/// The table every assertion below is made over.
///
/// `host-header-rewrite` and `url-rewrite` are the two types `TransformTypeEnum` declares
/// (`smithy_query(query="shape",shape="TransformTypeEnum")`), so the rows cover a rule
/// holding neither, each one of them, and both at once.
fn table(target_group: &str) -> Vec<Row> {
    vec![
        Row {
            case: "a rule created with no transform at all",
            priority: 10,
            created_with: Vec::new(),
            modified_with: None,
            transforms: Vec::new(),
        },
        Row {
            case: "a rule created with one host header rewrite",
            priority: 20,
            created_with: host_header_rewrite(1, "^one\\.", "uno."),
            modified_with: None,
            transforms: vec!["host-header-rewrite"],
        },
        Row {
            case: "a rule created with one url rewrite",
            priority: 30,
            created_with: url_rewrite(1, "^/old/(.*)", "/new/$1"),
            modified_with: None,
            transforms: vec!["url-rewrite"],
        },
        Row {
            case: "a rule created with one transform of each declared type",
            priority: 40,
            created_with: [
                host_header_rewrite(1, "^both\\.", "due."),
                url_rewrite(2, "^/a/(.*)", "/b/$1"),
            ]
            .concat(),
            modified_with: None,
            transforms: vec!["host-header-rewrite", "url-rewrite"],
        },
        Row {
            case: "a rule a modification gave a transform to",
            priority: 50,
            created_with: Vec::new(),
            modified_with: Some(
                [forward_to(target_group), url_rewrite(1, "^/x/(.*)", "/y/$1")].concat(),
            ),
            transforms: vec!["url-rewrite"],
        },
        Row {
            case: "a rule a modification removed every transform from",
            priority: 60,
            created_with: host_header_rewrite(1, "^gone\\.", "went."),
            modified_with: Some(vec![p("ResetTransforms", "true")]),
            transforms: Vec::new(),
        },
    ]
}

/// Whether one reported rule carries the `Transforms` member at all, which is a different
/// answer from carrying it empty (`<Transforms></Transforms>` or `<Transforms/>`).
fn carries_transforms_member(rule: &str) -> bool {
    rule.contains("<Transforms>") || rule.contains("<Transforms/>")
}

/// The `Type` of every transform one reported rule carries, in the order it reported them.
fn reported_transform_types(rule: &str) -> Vec<String> {
    members(rule, "Transforms")
        .iter()
        .map(|transform| first_text(transform, "Type").unwrap_or_default())
        .collect()
}

/// What a re-prioritisation must report about one rule: its transforms when it holds any,
/// and no `Transforms` member at all when it holds none.
fn assert_reprioritised(case: &str, rule: &str, transforms: &[&str]) {
    if transforms.is_empty() {
        assert!(
            !carries_transforms_member(rule),
            "case {case}: SetRulePriorities must leave the Transforms member out of a rule \
             holding no transform, got {rule}"
        );
        return;
    }
    assert!(
        carries_transforms_member(rule),
        "case {case}: SetRulePriorities must report the Transforms member of a rule holding \
         {transforms:?}, got {rule}"
    );
    assert_eq!(
        reported_transform_types(rule),
        transforms,
        "case {case}: SetRulePriorities expected transforms {transforms:?}, got {rule}"
    );
}

/// What the paths reporting a rule's whole CONFIGURATION must report about it: the
/// `Transforms` member on every rule, empty where the rule holds no transform.
fn assert_configuration(case: &str, where_from: &str, rule: &str, transforms: &[&str]) {
    assert!(
        carries_transforms_member(rule),
        "case {case}: {where_from} must report the Transforms member on every rule, got {rule}"
    );
    assert_eq!(
        reported_transform_types(rule),
        transforms,
        "case {case}: {where_from} expected transforms {transforms:?}, got {rule}"
    );
}

/// Build the rules of one row and answer the ARN of the rule it left behind.
async fn seed(router: &MultiServiceRouter, row: &Row, listener: &str, target_group: &str) -> String {
    let arn = add_rule(
        router,
        row.case,
        listener,
        row.priority,
        &[
            host_header(&format!("p{}.example.com", row.priority)),
            forward_to(target_group),
            row.created_with.clone(),
        ]
        .concat(),
    )
    .await;
    if let Some(fields) = row.modified_with.as_ref() {
        let mut form = action("ModifyRule");
        form.push(p("RuleArn", &arn));
        form.extend(fields.iter().cloned());
        let reply = call(router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {}: expected 200 from ModifyRule, got {} {}",
            row.case, reply.status, reply.body
        );
        let modified = members(&reply.body, "Rules");
        assert_eq!(
            modified.len(),
            1,
            "case {}: a modification answers with the one rule it changed, got {}",
            row.case,
            reply.body
        );
        // The modification reports the rule's configuration, so it carries the member
        // whether or not the rule is left holding a transform.
        assert_configuration(row.case, "ModifyRule", &modified[0], &row.transforms);
    }
    arn
}

/// Read one rule back by ARN and answer the XML of the rule the read reported.
async fn read_rule(router: &MultiServiceRouter, case: &str, arn: &str) -> String {
    let reply = describe_rule(router, arn).await;
    assert_eq!(
        reply.status, 200,
        "case {case}: expected 200 from DescribeRules, got {} {}",
        reply.status, reply.body
    );
    let rules = members(&reply.body, "Rules");
    assert_eq!(
        rules.len(),
        1,
        "case {case}: a read by ARN answers with the one rule it names, got {}",
        reply.body
    );
    rules.into_iter().next().unwrap_or_default()
}

/// One `SetRulePriorities` call per rule: the moved rule is reported with its transforms
/// where it holds any and with no `Transforms` member where it holds none, and the read of
/// the same rule still reports the member either way.
#[tokio::test]
async fn a_reprioritised_rule_reports_transforms_only_when_it_holds_any() {
    let (router, _engine) = emulator();
    let target_group = create_http_target_group(&router, "reprio-transforms-tg").await;
    let lb = create_load_balancer(&router, "reprio-transforms-lb").await;
    let listener = create_listener(&router, &lb, &target_group, 80).await;

    for row in table(&target_group) {
        let case = row.case;
        let arn = seed(&router, &row, &listener, &target_group).await;

        // The move itself: a priority nothing else holds, so the row under test is the
        // only thing the call decides.
        let mut form = action("SetRulePriorities");
        form.push(p("RulePriorities.member.1.RuleArn", &arn));
        form.push(p(
            "RulePriorities.member.1.Priority",
            &(row.priority + 1000).to_string(),
        ));
        let reply = call(&router, &form).await;
        assert_eq!(
            reply.status, 200,
            "case {case}: expected 200, got {} {}",
            reply.status, reply.body
        );
        let moved = members(&reply.body, "Rules");
        assert_eq!(
            moved.len(),
            1,
            "case {case}: the call answers with the one rule it moved, got {}",
            reply.body
        );
        assert_eq!(
            first_text(&moved[0], "Priority").as_deref(),
            Some((row.priority + 1000).to_string().as_str()),
            "case {case}: the moved rule reports its new priority, got {}",
            moved[0]
        );
        assert_reprioritised(case, &moved[0], &row.transforms);

        // The read path reports the same rule's configuration, member present either way:
        // what the re-prioritisation left out of its answer it did not remove from the
        // store.
        let stored = read_rule(&router, case, &arn).await;
        assert_configuration(case, "DescribeRules by ARN", &stored, &row.transforms);
    }
}

/// One `SetRulePriorities` call moving EVERY rule of the listener at once: each rule in the
/// answer is reported by the same rule, so a call that moves a rule holding transforms
/// beside one holding none carries the member on the first and not on the second.
#[tokio::test]
async fn one_call_moving_every_rule_reports_each_rule_by_the_same_rule() {
    let (router, _engine) = emulator();
    let target_group = create_http_target_group(&router, "reprio-all-tg").await;
    let lb = create_load_balancer(&router, "reprio-all-lb").await;
    let listener = create_listener(&router, &lb, &target_group, 80).await;

    let rows = table(&target_group);
    let mut arns = Vec::new();
    for row in &rows {
        arns.push(seed(&router, row, &listener, &target_group).await);
    }

    let mut form = action("SetRulePriorities");
    for (index, (row, arn)) in rows.iter().zip(&arns).enumerate() {
        let member = index + 1;
        form.push(p(&format!("RulePriorities.member.{member}.RuleArn"), arn));
        form.push(p(
            &format!("RulePriorities.member.{member}.Priority"),
            &(row.priority + 1000).to_string(),
        ));
    }
    let reply = call(&router, &form).await;
    assert_eq!(
        reply.status, 200,
        "case every rule of the listener: expected 200, got {} {}",
        reply.status, reply.body
    );
    let moved = members(&reply.body, "Rules");
    assert_eq!(
        moved.len(),
        rows.len(),
        "case every rule of the listener: expected {} rules, got {}",
        rows.len(),
        reply.body
    );
    for (row, (arn, rule)) in rows.iter().zip(arns.iter().zip(&moved)) {
        assert_eq!(
            first_text(rule, "RuleArn").as_deref(),
            Some(arn.as_str()),
            "case {}: the answer reports the rules in the order the request named them, got {}",
            row.case,
            reply.body
        );
        assert_reprioritised(row.case, rule, &row.transforms);
    }

    // A read of the whole listener reports every one of those rules with the member, the
    // rule the listener mints with itself included.
    let mut form = action("DescribeRules");
    form.push(p("ListenerArn", &listener));
    let read = call(&router, &form).await;
    assert_eq!(
        read.status, 200,
        "case every rule of the listener: expected 200 from DescribeRules, got {} {}",
        read.status, read.body
    );
    let reported = members(&read.body, "Rules");
    for (row, arn) in rows.iter().zip(&arns) {
        let rule = reported
            .iter()
            .find(|rule| first_text(rule, "RuleArn").as_deref() == Some(arn.as_str()))
            .unwrap_or_else(|| {
                panic!(
                    "case {}: the listener must report the rule it holds, got {}",
                    row.case, read.body
                )
            });
        assert_configuration(row.case, "DescribeRules by listener", rule, &row.transforms);
    }
    let default_rule = reported
        .iter()
        .find(|rule| first_text(rule, "IsDefault").as_deref() == Some("true"))
        .unwrap_or_else(|| {
            panic!(
                "case the default rule: every listener reports one, got {}",
                read.body
            )
        });
    assert_configuration("the default rule", "DescribeRules by listener", default_rule, &[]);
}
