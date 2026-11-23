// SPDX-License-Identifier: CC0-1.0

//! WHICH rule of a referenced rule group acted on a request the web ACL in front of an
//! Application Load Balancer decided, as the record left in that web ACL's logging destination
//! attributes it.
//!
//! The rule under test, one level above any single request: **a log record of a request a web ACL
//! evaluated reports one `ruleGroupList` entry per rule-group reference the evaluation actually
//! RAN — in the order it ran them, each naming the referenced group by its own identifier (the ARN
//! for a group this account owns) — whose `terminatingRule` is the group-local `{ruleId, action}`
//! of the rule INSIDE that group that ended the evaluation and is null for a group that ended
//! nothing, with that group's merely counting rules under `nonTerminatingMatchingRules`; a
//! reference the evaluation never reached has no entry, and the member is present and empty for a
//! request no rule group was run against.**
//!
//! The web ACL rule that RUNS a group is all the record's own `terminatingRuleId` can name (with
//! `terminatingRuleType` `GROUP`), so the per-group entry is the only place the record says which
//! of the group's rules refused the request, and the only evidence that a group was evaluated at
//! all for a request it did not terminate.
//!
//! The rows differ along what the rule generalises over: what the matching rule inside a group did
//! to the request (ended it with a `Block`, ended it with an `Allow`, only counted it, matched
//! nothing), WHICH of two referenced groups held that rule, and therefore how many references the
//! evaluation reached before it stopped. A separate case pins the empty member for a web ACL whose
//! rules reference no group at all. The sibling [`crate::web_acl_log_records_per_request`] pins
//! that every evaluated request leaves one record, with ONE referenced group; this suite is about
//! the attribution INSIDE those records, and it drives the same harness.
//!
//! Every row drives a real request to the address the load balancer published and reads the record
//! back out of the CloudWatch Logs log group the web ACL's `LoggingConfiguration` names, through
//! that service's own reader, so what is asserted is the record as a consumer reads it.

use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::web_acl_enforcement::{byte_match, rule, BLOCK_STATUS};
use crate::web_acl_log_records_per_request::{
    create_listener, emulator_with_waf_and_logs, log_destination, log_to, path_of, record_for,
    records_in, rule_group, runs_the_group, web_acl_in_front, GroupRule, DEFAULT_ACTION_RULE,
    GROUP_TYPE, PROBE_HEADER, REGULAR_TYPE, SERVED_BODY, SERVED_STATUS, WAF_LOGS_PREFIX,
};

/// The rules inside the FIRST referenced rule group, and the probe value each matches.
const DENY_IN_FIRST: &str = "deny-inside-first";
const DENY_IN_FIRST_PROBE: &str = "block-in-first";
const PERMIT_IN_FIRST: &str = "permit-inside-first";
const PERMIT_IN_FIRST_PROBE: &str = "allow-in-first";
const TALLY_IN_FIRST: &str = "tally-inside-first";
const TALLY_IN_FIRST_PROBE: &str = "count-in-first";

/// The rules inside the SECOND referenced rule group, and the probe value each matches.
const DENY_IN_SECOND: &str = "deny-inside-second";
const DENY_IN_SECOND_PROBE: &str = "block-in-second";
const TALLY_IN_SECOND: &str = "tally-inside-second";
const TALLY_IN_SECOND_PROBE: &str = "count-in-second";

/// A probe value no rule of either group matches, so the web ACL's `DefaultAction` decides.
const UNMATCHED_PROBE: &str = "leave-me-alone";

/// The web ACL's own rules, each of which RUNS one of the two groups: the first at the lower
/// priority, so the evaluation reaches the second group only when the first terminates nothing.
const RUNS_THE_FIRST: &str = "run-the-first-group";
const RUNS_THE_SECOND: &str = "run-the-second-group";

/// The web ACL rule of a web ACL that references NO group, used by the empty-member case.
const OWN_RULE: &str = "block-without-a-group";

/// The `action` values a record reports, from `ActionValue`.
const BLOCK_ACTION: &str = "BLOCK";
const ALLOW_ACTION: &str = "ALLOW";
const COUNT_ACTION: &str = "COUNT";

/// The rules of the first referenced group: one that blocks, one that allows and one that only
/// counts — the three kinds of thing a group's rule can do to a request it matches.
fn first_group_rules() -> Vec<GroupRule<'static>> {
    vec![
        GroupRule {
            name: DENY_IN_FIRST,
            probe: DENY_IN_FIRST_PROBE,
            action: json!({ "Block": {} }),
        },
        GroupRule {
            name: PERMIT_IN_FIRST,
            probe: PERMIT_IN_FIRST_PROBE,
            action: json!({ "Allow": {} }),
        },
        GroupRule {
            name: TALLY_IN_FIRST,
            probe: TALLY_IN_FIRST_PROBE,
            action: json!({ "Count": {} }),
        },
    ]
}

/// The rules of the second referenced group, which the evaluation reaches only when the first
/// group terminated nothing.
fn second_group_rules() -> Vec<GroupRule<'static>> {
    vec![
        GroupRule {
            name: DENY_IN_SECOND,
            probe: DENY_IN_SECOND_PROBE,
            action: json!({ "Block": {} }),
        },
        GroupRule {
            name: TALLY_IN_SECOND,
            probe: TALLY_IN_SECOND_PROBE,
            action: json!({ "Count": {} }),
        },
    ]
}

/// Everything a case drives: the load balancer with its listener, the web ACL in front of it, the
/// groups it references and the destination its records go to.
struct Deployed {
    router: MultiServiceRouter,
    dns_name: String,
    /// The ARNs of the referenced groups, in the order the web ACL runs them; empty for the web
    /// ACL that references none.
    group_arns: Vec<String>,
    /// The name of the log group the records are read out of.
    log_group: String,
}

/// A deployment named `name` whose web ACL runs the two referenced rule groups, in that order.
async fn deployed_with_two_groups(name: &str) -> Deployed {
    let (router, engine) = emulator_with_waf_and_logs();
    let (lb_arn, dns_name) = create_load_balancer(&router, name).await;
    create_listener(&router, &lb_arn).await;
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));
    let first = rule_group(&router, &format!("{name}-first"), &first_group_rules()).await;
    let second = rule_group(&router, &format!("{name}-second"), &second_group_rules()).await;
    let rules = json!([
        runs_the_group(RUNS_THE_FIRST, 0, &first),
        runs_the_group(RUNS_THE_SECOND, 1, &second),
    ]);
    let web_acl = web_acl_in_front(&router, name, rules, &lb_arn).await;
    let log_group = format!("{WAF_LOGS_PREFIX}{name}");
    let destination = log_destination(&router, &log_group).await;
    log_to(&router, &web_acl, &destination).await;
    Deployed {
        router,
        dns_name,
        group_arns: vec![first, second],
        log_group,
    }
}

/// A deployment named `name` whose web ACL has ONE rule of its own and references no group.
async fn deployed_without_a_group(name: &str) -> Deployed {
    let (router, engine) = emulator_with_waf_and_logs();
    let (lb_arn, dns_name) = create_load_balancer(&router, name).await;
    create_listener(&router, &lb_arn).await;
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));
    let rules = json!([rule(
        OWN_RULE,
        byte_match(
            json!({ "SingleHeader": { "Name": PROBE_HEADER } }),
            DENY_IN_FIRST_PROBE,
            "EXACTLY",
        ),
        json!({ "Block": {} }),
    )]);
    let web_acl = web_acl_in_front(&router, name, rules, &lb_arn).await;
    let log_group = format!("{WAF_LOGS_PREFIX}{name}");
    let destination = log_destination(&router, &log_group).await;
    log_to(&router, &web_acl, &destination).await;
    Deployed {
        router,
        dns_name,
        group_arns: Vec::new(),
        log_group,
    }
}

/// What one `ruleGroupList` entry must say: which referenced group it is for (by its position in
/// the order the web ACL runs them), the group's own rule that ended the evaluation, and the
/// group's rules that only counted the request.
struct GroupEntry {
    group: usize,
    terminating: Option<(&'static str, &'static str)>,
    counted: Vec<(&'static str, &'static str)>,
}

impl GroupEntry {
    /// An entry for the group at `group` that neither terminated nor counted anything: it RAN, and
    /// the request went past it.
    fn evaluated(group: usize) -> Self {
        Self {
            group,
            terminating: None,
            counted: Vec::new(),
        }
    }

    /// An entry whose group ended the evaluation with its own rule `rule_id` applying `action`.
    fn terminated_by(group: usize, rule_id: &'static str, action: &'static str) -> Self {
        Self {
            group,
            terminating: Some((rule_id, action)),
            counted: Vec::new(),
        }
    }

    /// An entry whose group only counted the request, under its own rule `rule_id`.
    fn counted_by(group: usize, rule_id: &'static str) -> Self {
        Self {
            group,
            terminating: None,
            counted: vec![(rule_id, COUNT_ACTION)],
        }
    }
}

/// One row: the probe value a request carries, what the client is answered, and what the record of
/// that request must say about the groups the evaluation ran.
struct Row {
    case: &'static str,
    probe: &'static str,
    status: u16,
    /// The record's own `action`, `terminatingRuleId` and `terminatingRuleType`.
    action: &'static str,
    terminating_rule: &'static str,
    terminating_type: &'static str,
    /// The `ruleGroupList` the record must carry, in order.
    groups: Vec<GroupEntry>,
}

/// Every row of the table.
fn rows() -> Vec<Row> {
    vec![
        // The first group's rule BLOCKS: the record names that inner rule under the group, and the
        // second reference is never reached, so it has no entry.
        Row {
            case: "a rule inside the first referenced group blocked the request",
            probe: DENY_IN_FIRST_PROBE,
            status: BLOCK_STATUS,
            action: BLOCK_ACTION,
            terminating_rule: RUNS_THE_FIRST,
            terminating_type: GROUP_TYPE,
            groups: vec![GroupEntry::terminated_by(0, DENY_IN_FIRST, BLOCK_ACTION)],
        },
        // The same shape for a terminating action that is not a refusal: an `Allow` inside the
        // group ends the evaluation just as a `Block` does, and is attributed the same way.
        Row {
            case: "a rule inside the first referenced group allowed the request",
            probe: PERMIT_IN_FIRST_PROBE,
            status: SERVED_STATUS,
            action: ALLOW_ACTION,
            terminating_rule: RUNS_THE_FIRST,
            terminating_type: GROUP_TYPE,
            groups: vec![GroupEntry::terminated_by(0, PERMIT_IN_FIRST, ALLOW_ACTION)],
        },
        // A rule inside the first group only COUNTS: that group terminated nothing, so its entry
        // carries a null terminating rule and the counting rule, and the evaluation goes on to the
        // second group, which gets an entry of its own.
        Row {
            case: "a rule inside the first referenced group counted the request",
            probe: TALLY_IN_FIRST_PROBE,
            status: SERVED_STATUS,
            action: ALLOW_ACTION,
            terminating_rule: DEFAULT_ACTION_RULE,
            terminating_type: REGULAR_TYPE,
            groups: vec![
                GroupEntry::counted_by(0, TALLY_IN_FIRST),
                GroupEntry::evaluated(1),
            ],
        },
        // The SECOND group is the one that blocks: both references ran, and the entries say which
        // of them ended the evaluation and which merely let the request past.
        Row {
            case: "a rule inside the second referenced group blocked the request",
            probe: DENY_IN_SECOND_PROBE,
            status: BLOCK_STATUS,
            action: BLOCK_ACTION,
            terminating_rule: RUNS_THE_SECOND,
            terminating_type: GROUP_TYPE,
            groups: vec![
                GroupEntry::evaluated(0),
                GroupEntry::terminated_by(1, DENY_IN_SECOND, BLOCK_ACTION),
            ],
        },
        Row {
            case: "a rule inside the second referenced group counted the request",
            probe: TALLY_IN_SECOND_PROBE,
            status: SERVED_STATUS,
            action: ALLOW_ACTION,
            terminating_rule: DEFAULT_ACTION_RULE,
            terminating_type: REGULAR_TYPE,
            groups: vec![
                GroupEntry::evaluated(0),
                GroupEntry::counted_by(1, TALLY_IN_SECOND),
            ],
        },
        // No rule of either group matches, so the web ACL's default action decides: both groups are
        // still listed, with nothing under them, which is the only proof they were evaluated.
        Row {
            case: "no rule of either referenced group matched the request",
            probe: UNMATCHED_PROBE,
            status: SERVED_STATUS,
            action: ALLOW_ACTION,
            terminating_rule: DEFAULT_ACTION_RULE,
            terminating_type: REGULAR_TYPE,
            groups: vec![GroupEntry::evaluated(0), GroupEntry::evaluated(1)],
        },
    ]
}

/// Assert what one `ruleGroupList` entry says the group it names did, member by member: WAF's
/// published log format carries further members in a `terminatingRule` (its `ruleMatchDetails`,
/// say), so what the rule requires of the entry is read off the members it is about rather than by
/// comparing the whole object.
fn assert_entry(case: &str, index: usize, record: &Value, listed: &Value, entry: &GroupEntry) {
    // `terminatingRule` is written for every entry, null where the group ended nothing, so an
    // entry that simply omits it is not the same answer as one reporting there was none.
    let terminating = listed.get("terminatingRule").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected entry {index} to carry a terminatingRule member, got {record}"
        )
    });
    match entry.terminating {
        None => assert!(
            terminating.is_null(),
            "case {case:?}: expected entry {index} to report that its group ended no evaluation, \
             got {record}"
        ),
        Some((rule_id, action)) => {
            assert_eq!(
                terminating.get("ruleId"),
                Some(&json!(rule_id)),
                "case {case:?}: expected entry {index} to name {rule_id:?} as the group's own \
                 terminating rule, got {record}"
            );
            assert_eq!(
                terminating.get("action"),
                Some(&json!(action)),
                "case {case:?}: expected entry {index} to report {action:?} as what that rule \
                 applied, got {record}"
            );
        }
    }
    let counted = listed.get("nonTerminatingMatchingRules").unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected entry {index} to carry a nonTerminatingMatchingRules member, \
             got {record}"
        )
    });
    let counted = counted.as_array().unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected entry {index}'s nonTerminatingMatchingRules to be a list, \
             got {record}"
        )
    });
    assert_eq!(
        counted.len(),
        entry.counted.len(),
        "case {case:?}: expected entry {index} to report {} of its group's rules as counting the \
         request, got {record}",
        entry.counted.len()
    );
    for (at, (rule_id, action)) in entry.counted.iter().enumerate() {
        assert_eq!(
            counted[at].get("ruleId"),
            Some(&json!(rule_id)),
            "case {case:?}: expected entry {index} to name {rule_id:?} as a counting rule of its \
             group, got {record}"
        );
        assert_eq!(
            counted[at].get("action"),
            Some(&json!(action)),
            "case {case:?}: expected entry {index} to report {action:?} for that counting rule, \
             got {record}"
        );
    }
}

/// The `ruleGroupList` of a record, which must be PRESENT whatever it holds: an absent member is
/// what this rule exists to rule out, so it is never read as the empty list.
fn listed_groups<'record>(case: &str, record: &'record Value) -> &'record Vec<Value> {
    record
        .get("ruleGroupList")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the record to carry a ruleGroupList, got {record}")
        })
}

/// Assert everything the record of one row must say about the groups the evaluation ran.
fn assert_attribution(case: &str, deployment: &Deployed, row: &Row, record: &Value) {
    assert_eq!(
        record["action"],
        json!(row.action),
        "case {case:?}: expected the record's action to be {:?}, got {record}",
        row.action
    );
    assert_eq!(
        record["terminatingRuleId"],
        json!(row.terminating_rule),
        "case {case:?}: expected the record to name the terminating rule {:?}, got {record}",
        row.terminating_rule
    );
    assert_eq!(
        record["terminatingRuleType"],
        json!(row.terminating_type),
        "case {case:?}: expected the record to name the terminating rule type {:?}, got {record}",
        row.terminating_type
    );
    let listed = listed_groups(case, record);
    assert_eq!(
        listed.len(),
        row.groups.len(),
        "case {case:?}: expected {} rule-group entries for the references the evaluation ran, got \
         {record}",
        row.groups.len()
    );
    for (index, entry) in row.groups.iter().enumerate() {
        let expected_id = deployment
            .group_arns
            .get(entry.group)
            .unwrap_or_else(|| panic!("case {case:?}: the row names group {}", entry.group));
        assert_eq!(
            listed[index]["ruleGroupId"],
            json!(expected_id),
            "case {case:?}: expected entry {index} to name the group {expected_id}, got {record}"
        );
        assert_entry(case, index, record, &listed[index], entry);
    }
}

/// RULE: a record reports one `ruleGroupList` entry per rule-group reference the evaluation ran,
/// naming the group and the rule INSIDE it that terminated — or nothing, for a group that
/// terminated nothing.
#[tokio::test]
async fn a_record_attributes_the_rule_inside_each_referenced_group_that_acted() {
    let deployment = deployed_with_two_groups("waf-rgl-alb").await;
    let table = rows();
    for (index, row) in table.iter().enumerate() {
        let case = row.case;
        let path = path_of(index);
        let answered = send(
            &deployment.router,
            "GET",
            &deployment.dns_name,
            &path,
            &[(PROBE_HEADER, row.probe)],
        )
        .await;
        assert_eq!(
            answered.status, row.status,
            "case {case:?}: expected the client to be answered {}, got {} with {:?}",
            row.status, answered.status, answered.body
        );
        // A request the web ACL let through is answered by the LISTENER's own default action, so
        // its body says the request really reached the load balancer's configuration rather than
        // being answered 200 by anything else on the way.
        if row.status == SERVED_STATUS {
            assert_eq!(
                answered.body, SERVED_BODY,
                "case {case:?}: expected the listener's own answer, got {:?}",
                answered.body
            );
        }

        let records = records_in(&deployment.router, case, &deployment.log_group, index + 1).await;
        let record = record_for(case, &records, &path);
        assert_attribution(case, &deployment, row, record);
    }
}

/// RULE, at its other end: a web ACL whose rules reference no rule group at all leaves the member
/// present and EMPTY, because "no group ran" is something the evaluation knows.
#[tokio::test]
async fn a_record_of_a_web_acl_that_references_no_group_lists_none() {
    let case = "the web ACL references no rule group";
    let deployment = deployed_without_a_group("waf-rgl-alb-none").await;
    let path = path_of(0);
    let answered = send(
        &deployment.router,
        "GET",
        &deployment.dns_name,
        &path,
        &[(PROBE_HEADER, DENY_IN_FIRST_PROBE)],
    )
    .await;
    assert_eq!(
        answered.status, BLOCK_STATUS,
        "case {case:?}: expected the web ACL's own rule to refuse the request, got {} with {:?}",
        answered.status, answered.body
    );
    let records = records_in(&deployment.router, case, &deployment.log_group, 1).await;
    let record = record_for(case, &records, &path);
    assert!(
        listed_groups(case, record).is_empty(),
        "case {case:?}: expected the record to list no rule group, got {record}"
    );
    assert_eq!(
        record["terminatingRuleId"],
        json!(OWN_RULE),
        "case {case:?}: expected the record to name the web ACL's own rule, got {record}"
    );
    assert_eq!(
        record["terminatingRuleType"],
        json!(REGULAR_TYPE),
        "case {case:?}: expected the web ACL's own rule to be a {REGULAR_TYPE} rule, got {record}"
    );
}
