// SPDX-License-Identifier: CC0-1.0

//! `ModifyTrafficMirrorFilterRule` and the `DeleteTrafficMirrorFilterRule` beside it, driven
//! over the wire the service actually speaks (`ec2Query`, through the crate's shared
//! [`super::ec2_query`] harness).
//!
//! The rule under test: a mutation that answers 200 is COMMITTED, so every later read of the
//! resource reports what the mutation left behind — never the value it replaced. A Traffic
//! Mirror filter rule lives in its direction's own set on the filter that holds it
//! (`TrafficMirrorFilter$IngressFilterRules`/`$EgressFilterRules`), so the reader is
//! `DescribeTrafficMirrorFilters`, and the rule has three halves here:
//!
//! 1. each property the modify request assigns replaces the stored one, each property named in
//!    `RemoveFields` goes back to the default — "when you remove a property from a Traffic
//!    Mirror filter rule, the property is set to the default"
//!    (`ModifyTrafficMirrorFilterRuleRequest$RemoveFields`) — and a property the request
//!    carries no value for keeps the value the rule already held;
//! 2. a rule whose direction or number changed is reported where that direction and number put
//!    it, the sets staying in ascending rule-number order;
//! 3. a delete takes the rule out of the filter's set, so the id stops resolving and the set
//!    no longer carries it.
//!
//! The rows of [`a_modification_is_what_every_later_read_of_the_filter_reports`] differ along
//! what the rule generalises over — which property is assigned, whether it is assigned or
//! removed, what type the value has (an enum member, a number, a port range, a CIDR block,
//! free text), and whether the rule stays in the direction and at the number it had — so no
//! single row is the specification.

use ec2::state::{
    DRY_RUN_OPERATION, DRY_RUN_OPERATION_MESSAGE, DRY_RUN_OPERATION_STATUS,
    INVALID_PARAMETER_COMBINATION, INVALID_TRAFFIC_MIRROR_FILTER_RULE_ID_NOT_FOUND,
    TRAFFIC_DIRECTION_EGRESS, TRAFFIC_DIRECTION_INGRESS,
    TRAFFIC_MIRROR_FILTER_RULE_ALREADY_EXISTS,
};
use service_com_amazonaws_ec2_amazonec2 as ec2;
use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, error_code, error_message};
use super::wire::{child, child_text, document, param, set_items, Param};

/// The Region every call here is signed for. A Traffic Mirror filter rule's modelled shape
/// names no Region, so the Region only selects the envelope.
const REGION: &str = "us-west-2";

/// The rule actions the model declares
/// (`crate::ops::types::TRAFFIC_MIRROR_RULE_ACTION_VALUES` is `accept | reject`).
const RULE_ACTIONS: &[&str] = ec2::ops::types::TRAFFIC_MIRROR_RULE_ACTION_VALUES;

/// The destination block every rule here is created with.
const DESTINATION_CIDR: &str = "10.2.0.0/16";

/// The source block every rule here is created with.
const SOURCE_CIDR: &str = "10.1.0.0/16";

/// The number every rule here is created at.
const RULE_NUMBER: i32 = 100;

/// One Traffic Mirror filter rule as a response reports it: every member a modification can
/// change, read off the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedRule {
    rule_id: Option<String>,
    direction: Option<String>,
    rule_number: Option<String>,
    rule_action: Option<String>,
    protocol: Option<String>,
    destination_cidr_block: Option<String>,
    source_cidr_block: Option<String>,
    destination_port_range: Option<(String, String)>,
    source_port_range: Option<(String, String)>,
    description: Option<String>,
}

/// One call of `action` signed for the Region above.
async fn invoke(router: &MultiServiceRouter, action: &str, params: &[Param]) -> (u16, String) {
    call(router, REGION, action, params).await
}

/// One call of `action` that has to succeed, and the body it answered.
async fn ok(case: &str, router: &MultiServiceRouter, action: &str, params: &[Param]) -> String {
    let (status, body) = invoke(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// Pin that `action` refused `params` with HTTP `status`, `code` and, when given, `message`.
async fn refused(
    case: &str,
    router: &MultiServiceRouter,
    action: &str,
    params: &[Param],
    status: u16,
    code: &str,
    message: Option<&str>,
) {
    let (answered, body) = invoke(router, action, params).await;
    assert_eq!(
        answered, status,
        "case {case:?}: expected HTTP {status} from {action}, got {answered} with body {body}"
    );
    let answered_code = error_code(&body);
    assert_eq!(
        answered_code, code,
        "case {case:?}: expected the code {code} from {action}, got {answered_code:?} in {body}"
    );
    if let Some(message) = message {
        let answered_message = error_message(&body);
        assert_eq!(
            answered_message, message,
            "case {case:?}: expected the message {message:?} from {action}, got \
             {answered_message:?}"
        );
    }
}

/// One rule element as [`ReportedRule`].
fn read_rule(node: roxmltree::Node<'_, '_>) -> ReportedRule {
    let port_range = |name: &str| {
        child(node, name).map(|range| {
            (
                child_text(range, "fromPort").unwrap_or_default(),
                child_text(range, "toPort").unwrap_or_default(),
            )
        })
    };
    ReportedRule {
        rule_id: child_text(node, "trafficMirrorFilterRuleId"),
        direction: child_text(node, "trafficDirection"),
        rule_number: child_text(node, "ruleNumber"),
        rule_action: child_text(node, "ruleAction"),
        protocol: child_text(node, "protocol"),
        destination_cidr_block: child_text(node, "destinationCidrBlock"),
        source_cidr_block: child_text(node, "sourceCidrBlock"),
        destination_port_range: port_range("destinationPortRange"),
        source_port_range: port_range("sourcePortRange"),
        description: child_text(node, "description"),
    }
}

/// The one rule a `CreateTrafficMirrorFilterRule` or `ModifyTrafficMirrorFilterRule` response
/// carries.
fn answered_rule(case: &str, body: &str) -> ReportedRule {
    let parsed = document(case, body);
    let node = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "trafficMirrorFilterRuleId").is_some())
        .unwrap_or_else(|| panic!("case {case:?}: no rule (no <trafficMirrorFilterRuleId>) in {body}"));
    read_rule(node)
}

/// The rules the filter `filter_id` reports in the direction `set` names, in the order the
/// filter reports them.
async fn described_rules(
    case: &str,
    router: &MultiServiceRouter,
    filter_id: &str,
    set: &str,
) -> Vec<ReportedRule> {
    let body = ok(
        case,
        router,
        "DescribeTrafficMirrorFilters",
        &[param("TrafficMirrorFilterId.1", filter_id)],
    )
    .await;
    let parsed = document(case, &body);
    let Some(filter) = parsed
        .descendants()
        .find(|node| node.is_element() && child(*node, "trafficMirrorFilterId").is_some())
    else {
        panic!("case {case:?}: no filter (no <trafficMirrorFilterId>) in {body}");
    };
    set_items(filter, set).into_iter().map(read_rule).collect()
}

/// The one rule the filter reports in `set`, or a panic naming the case.
async fn described_rule(
    case: &str,
    router: &MultiServiceRouter,
    filter_id: &str,
    set: &str,
) -> ReportedRule {
    let mut rules = described_rules(case, router, filter_id, set).await;
    assert_eq!(
        rules.len(),
        1,
        "case {case:?}: expected exactly one rule in {set}, got {rules:?}"
    );
    rules.remove(0)
}

/// Create one Traffic Mirror filter and answer its id.
async fn created_filter(case: &str, router: &MultiServiceRouter) -> String {
    let body = ok(case, router, "CreateTrafficMirrorFilter", &[]).await;
    document(case, &body)
        .descendants()
        .find(|node| node.is_element() && child(*node, "trafficMirrorFilterId").is_some())
        .and_then(|node| child_text(node, "trafficMirrorFilterId"))
        .unwrap_or_else(|| panic!("case {case:?}: no <trafficMirrorFilterId> in {body}"))
}

/// The form fields of one `CreateTrafficMirrorFilterRule` on `filter_id`: the members the
/// create requires, plus whatever `extra` the row adds.
fn create_rule_params(filter_id: &str, rule_number: i32, extra: Vec<Param>) -> Vec<Param> {
    let mut params = vec![
        param("TrafficMirrorFilterId", filter_id),
        param("TrafficDirection", TRAFFIC_DIRECTION_INGRESS),
        param("RuleNumber", &rule_number.to_string()),
        param("RuleAction", RULE_ACTIONS[0]),
        param("DestinationCidrBlock", DESTINATION_CIDR),
        param("SourceCidrBlock", SOURCE_CIDR),
    ];
    params.extend(extra);
    params
}

/// Create one filter and one ingress rule on it, and answer `(filter id, rule id)`.
async fn created_rule(
    case: &str,
    router: &MultiServiceRouter,
    extra: Vec<Param>,
) -> (String, String) {
    let filter_id = created_filter(case, router).await;
    let body = ok(
        case,
        router,
        "CreateTrafficMirrorFilterRule",
        &create_rule_params(&filter_id, RULE_NUMBER, extra),
    )
    .await;
    let rule_id = answered_rule(case, &body)
        .rule_id
        .unwrap_or_else(|| panic!("case {case:?}: the created rule must carry an id: {body}"));
    (filter_id, rule_id)
}

/// One row of the modification table: what the rule was created with beyond the required
/// members, the fields the modify request carries, and the direction set and members every
/// later read must then report.
struct ModifyCase {
    case: &'static str,
    created: Vec<Param>,
    modified: Vec<Param>,
    /// The direction set the rule must be reported in afterwards.
    set: &'static str,
    /// What the rule must then report, as `(member, value)` pairs read off [`ReportedRule`].
    expected: Expected,
}

/// What a row expects of the rule after its modification: only the members the row is about,
/// each `None` meaning "absent — back to the default".
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Expected {
    direction: &'static str,
    rule_number: &'static str,
    rule_action: &'static str,
    protocol: Option<&'static str>,
    destination_cidr_block: &'static str,
    source_cidr_block: &'static str,
    destination_port_range: Option<(&'static str, &'static str)>,
    source_port_range: Option<(&'static str, &'static str)>,
    description: Option<&'static str>,
}

impl Expected {
    /// The rule this expectation describes, as a response would report it, under `rule_id`.
    fn as_reported(&self, rule_id: &str) -> ReportedRule {
        ReportedRule {
            rule_id: Some(rule_id.to_string()),
            direction: Some(self.direction.to_string()),
            rule_number: Some(self.rule_number.to_string()),
            rule_action: Some(self.rule_action.to_string()),
            protocol: self.protocol.map(str::to_string),
            destination_cidr_block: Some(self.destination_cidr_block.to_string()),
            source_cidr_block: Some(self.source_cidr_block.to_string()),
            destination_port_range: self
                .destination_port_range
                .map(|(from, to)| (from.to_string(), to.to_string())),
            source_port_range: self
                .source_port_range
                .map(|(from, to)| (from.to_string(), to.to_string())),
            description: self.description.map(str::to_string),
        }
    }
}

/// The expectation a rule created with nothing but the required members starts from.
fn created_expectation() -> Expected {
    Expected {
        direction: TRAFFIC_DIRECTION_INGRESS,
        rule_number: "100",
        rule_action: RULE_ACTIONS[0],
        protocol: None,
        destination_cidr_block: DESTINATION_CIDR,
        source_cidr_block: SOURCE_CIDR,
        destination_port_range: None,
        source_port_range: None,
        description: None,
    }
}

/// RULE: a `ModifyTrafficMirrorFilterRule` that answers 200 has COMMITTED its change, so the
/// rule it answers and the rule `DescribeTrafficMirrorFilters` reports afterwards both carry
/// what the request left behind — in the direction set and at the number that request puts it.
#[tokio::test]
async fn a_modification_is_what_every_later_read_of_the_filter_reports() {
    let cases = vec![
        ModifyCase {
            case: "the rule action replaced",
            created: Vec::new(),
            modified: vec![param("RuleAction", RULE_ACTIONS[1])],
            set: "ingressFilterRuleSet",
            expected: Expected {
                rule_action: RULE_ACTIONS[1],
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "a description on a rule created without one",
            created: Vec::new(),
            modified: vec![param("Description", "probe accept rule")],
            set: "ingressFilterRuleSet",
            expected: Expected {
                description: Some("probe accept rule"),
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "a description replacing the one the create stored",
            created: vec![param("Description", "probe app port")],
            modified: vec![param("Description", "probe app port range")],
            set: "ingressFilterRuleSet",
            expected: Expected {
                description: Some("probe app port range"),
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "the description removed",
            created: vec![param("Description", "probe app port")],
            modified: vec![param("RemoveField.1", "description")],
            set: "ingressFilterRuleSet",
            expected: created_expectation(),
        },
        ModifyCase {
            case: "a protocol assigned",
            created: Vec::new(),
            modified: vec![param("Protocol", "6")],
            set: "ingressFilterRuleSet",
            expected: Expected {
                protocol: Some("6"),
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "the protocol removed",
            created: vec![param("Protocol", "17")],
            modified: vec![param("RemoveField.1", "protocol")],
            set: "ingressFilterRuleSet",
            expected: created_expectation(),
        },
        ModifyCase {
            case: "both port ranges assigned",
            created: Vec::new(),
            modified: vec![
                param("Protocol", "6"),
                param("DestinationPortRange.FromPort", "443"),
                param("DestinationPortRange.ToPort", "443"),
                param("SourcePortRange.FromPort", "1024"),
                param("SourcePortRange.ToPort", "65535"),
            ],
            set: "ingressFilterRuleSet",
            expected: Expected {
                protocol: Some("6"),
                destination_port_range: Some(("443", "443")),
                source_port_range: Some(("1024", "65535")),
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "one port range removed while the other is assigned",
            created: vec![
                param("Protocol", "17"),
                param("DestinationPortRange.FromPort", "80"),
                param("DestinationPortRange.ToPort", "80"),
            ],
            modified: vec![
                param("RemoveField.1", "destination-port-range"),
                param("SourcePortRange.FromPort", "0"),
                param("SourcePortRange.ToPort", "0"),
            ],
            set: "ingressFilterRuleSet",
            expected: Expected {
                protocol: Some("17"),
                source_port_range: Some(("0", "0")),
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "both address ranges replaced with another IPv4 pair",
            created: Vec::new(),
            modified: vec![
                param("DestinationCidrBlock", "172.16.0.0/12"),
                param("SourceCidrBlock", "192.168.0.0/24"),
            ],
            set: "ingressFilterRuleSet",
            expected: Expected {
                destination_cidr_block: "172.16.0.0/12",
                source_cidr_block: "192.168.0.0/24",
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "both address ranges replaced with an IPv6 pair",
            created: Vec::new(),
            modified: vec![
                param("DestinationCidrBlock", "2001:db8:1::/64"),
                param("SourceCidrBlock", "2001:db8:2::/64"),
            ],
            set: "ingressFilterRuleSet",
            expected: Expected {
                destination_cidr_block: "2001:db8:1::/64",
                source_cidr_block: "2001:db8:2::/64",
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "the rule renumbered",
            created: Vec::new(),
            modified: vec![param("RuleNumber", "1")],
            set: "ingressFilterRuleSet",
            expected: Expected {
                rule_number: "1",
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "the rule moved to the other direction",
            created: Vec::new(),
            modified: vec![param("TrafficDirection", TRAFFIC_DIRECTION_EGRESS)],
            set: "egressFilterRuleSet",
            expected: Expected {
                direction: TRAFFIC_DIRECTION_EGRESS,
                ..created_expectation()
            },
        },
        ModifyCase {
            case: "every property of the rule at once",
            created: vec![param("Description", "probe app port")],
            modified: vec![
                param("TrafficDirection", TRAFFIC_DIRECTION_EGRESS),
                param("RuleNumber", "32000"),
                param("RuleAction", RULE_ACTIONS[1]),
                param("Protocol", "6"),
                param("DestinationCidrBlock", "10.9.0.0/16"),
                param("SourceCidrBlock", "10.8.0.0/16"),
                param("DestinationPortRange.FromPort", "1"),
                param("DestinationPortRange.ToPort", "2"),
                param("SourcePortRange.FromPort", "3"),
                param("SourcePortRange.ToPort", "4"),
                param("Description", "probe app port range"),
            ],
            set: "egressFilterRuleSet",
            expected: Expected {
                direction: TRAFFIC_DIRECTION_EGRESS,
                rule_number: "32000",
                rule_action: RULE_ACTIONS[1],
                protocol: Some("6"),
                destination_cidr_block: "10.9.0.0/16",
                source_cidr_block: "10.8.0.0/16",
                destination_port_range: Some(("1", "2")),
                source_port_range: Some(("3", "4")),
                description: Some("probe app port range"),
            },
        },
    ];

    for row in cases {
        let case = row.case;
        let (router, _engine) = super::ec2_query::emulator();
        let (filter_id, rule_id) = created_rule(case, &router, row.created).await;

        let mut params = vec![param("TrafficMirrorFilterRuleId", &rule_id)];
        params.extend(row.modified);
        let body = ok(case, &router, "ModifyTrafficMirrorFilterRule", &params).await;

        let expected = row.expected.as_reported(&rule_id);
        let answered = answered_rule(case, &body);
        assert_eq!(
            answered, expected,
            "case {case:?}: the modify must answer the rule it stored, expected {expected:?}, \
             got {answered:?}"
        );

        let read_back = described_rule(case, &router, &filter_id, row.set).await;
        assert_eq!(
            read_back, expected,
            "case {case:?}: the read after the modify must report the modified rule, expected \
             {expected:?}, got {read_back:?}"
        );

        // A rule lives in ONE direction's set, so the other set carries nothing of it.
        let other = if row.set == "ingressFilterRuleSet" {
            "egressFilterRuleSet"
        } else {
            "ingressFilterRuleSet"
        };
        let others = described_rules(case, &router, &filter_id, other).await;
        assert!(
            others.is_empty(),
            "case {case:?}: {other} must carry no rule, got {others:?}"
        );
    }
}

/// RULE: the rules of one direction are reported in ascending rule-number order — "the rules
/// are processed in ascending order by rule number"
/// (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`) — so a renumbered rule is reported
/// where its NEW number puts it.
///
/// The rows differ along which number the modified rule is moved to: before every other rule,
/// between them, and after them all.
#[tokio::test]
async fn a_renumbered_rule_is_reported_where_its_new_number_puts_it() {
    for (case, moved_to, order) in [
        ("renumbered below every other rule", 1, vec![1, 200, 300]),
        ("renumbered between the others", 250, vec![200, 250, 300]),
        ("renumbered above every other rule", 400, vec![200, 300, 400]),
    ] {
        let (router, _engine) = super::ec2_query::emulator();
        let filter_id = created_filter(case, &router).await;
        let mut rule_ids = Vec::new();
        for number in [RULE_NUMBER, 200, 300] {
            let body = ok(
                case,
                &router,
                "CreateTrafficMirrorFilterRule",
                &create_rule_params(&filter_id, number, Vec::new()),
            )
            .await;
            rule_ids.push(answered_rule(case, &body).rule_id.unwrap_or_default());
        }

        ok(
            case,
            &router,
            "ModifyTrafficMirrorFilterRule",
            &[
                param("TrafficMirrorFilterRuleId", &rule_ids[0]),
                param("RuleNumber", &moved_to.to_string()),
            ],
        )
        .await;

        let numbers: Vec<i32> = described_rules(case, &router, &filter_id, "ingressFilterRuleSet")
            .await
            .into_iter()
            .map(|rule| {
                rule.rule_number
                    .unwrap_or_default()
                    .parse::<i32>()
                    .unwrap_or_default()
            })
            .collect();
        assert_eq!(
            numbers, order,
            "case {case:?}: expected the rule numbers {order:?}, got {numbers:?}"
        );
    }
}

/// RULE, the other half: a mutation that is REFUSED commits nothing, so the rule reads back
/// exactly as it did before the refused call.
///
/// The rows differ along what the request is refused FOR: an id of no rule of the account, a
/// property both assigned and removed, a pair of address ranges of different families, a
/// number another rule of the same direction already holds, and a probe asking whether the
/// call would have worked.
#[tokio::test]
async fn a_refused_mutation_leaves_the_rule_as_it_was() {
    /// `(case, whether the row names its own rule id, extra fields, status, code, message)`.
    type Row = (
        &'static str,
        Option<&'static str>,
        Vec<Param>,
        u16,
        &'static str,
        Option<String>,
    );

    let unknown = "tmfr-0123456789abcdef0";
    let rows: Vec<Row> = vec![
        (
            "an id this account holds no rule for",
            Some(unknown),
            vec![param("Description", "never stored")],
            400,
            INVALID_TRAFFIC_MIRROR_FILTER_RULE_ID_NOT_FOUND,
            Some(format!("{unknown} does not exist")),
        ),
        (
            "a description both assigned and removed",
            None,
            vec![
                param("Description", "never stored"),
                param("RemoveField.1", "description"),
            ],
            400,
            INVALID_PARAMETER_COMBINATION,
            None,
        ),
        (
            "a protocol both assigned and removed",
            None,
            vec![param("Protocol", "6"), param("RemoveField.1", "protocol")],
            400,
            INVALID_PARAMETER_COMBINATION,
            None,
        ),
        (
            "address ranges of two different families",
            None,
            vec![param("DestinationCidrBlock", "2001:db8::/64")],
            400,
            INVALID_PARAMETER_COMBINATION,
            Some(
                "DestinationCidrBlock and SourceCidrBlock must both be an IPv4 range or an \
                 IPv6 range"
                    .to_string(),
            ),
        ),
        (
            "a probe of a modification that would have worked",
            None,
            vec![
                param("Description", "never stored"),
                param("DryRun", "true"),
            ],
            DRY_RUN_OPERATION_STATUS,
            DRY_RUN_OPERATION,
            Some(DRY_RUN_OPERATION_MESSAGE.to_string()),
        ),
    ];

    for (case, names_rule, extra, status, code, message) in rows {
        let (router, _engine) = super::ec2_query::emulator();
        let (filter_id, rule_id) = created_rule(
            case,
            &router,
            vec![param("Description", "probe app port")],
        )
        .await;
        let before = described_rule(case, &router, &filter_id, "ingressFilterRuleSet").await;

        let mut params = vec![param(
            "TrafficMirrorFilterRuleId",
            names_rule.unwrap_or(rule_id.as_str()),
        )];
        params.extend(extra);
        refused(
            case,
            &router,
            "ModifyTrafficMirrorFilterRule",
            &params,
            status,
            code,
            message.as_deref(),
        )
        .await;

        let after = described_rule(case, &router, &filter_id, "ingressFilterRuleSet").await;
        assert_eq!(
            after, before,
            "case {case:?}: a refused mutation must leave the rule as it was, expected \
             {before:?}, got {after:?}"
        );
    }
}

/// RULE: "this number must be unique for each Traffic Mirror rule in a given direction"
/// (`CreateTrafficMirrorFilterRuleRequest$RuleNumber`), so a modification that would take a
/// number another rule of that direction already holds is refused and nothing moves — while
/// the same number in the OTHER direction is free.
#[tokio::test]
async fn a_rule_number_another_rule_of_the_direction_holds_is_refused() {
    let case = "a second ingress rule renumbered onto the first";
    let (router, _engine) = super::ec2_query::emulator();
    let filter_id = created_filter(case, &router).await;
    let mut rule_ids = Vec::new();
    for number in [RULE_NUMBER, 200] {
        let body = ok(
            case,
            &router,
            "CreateTrafficMirrorFilterRule",
            &create_rule_params(&filter_id, number, Vec::new()),
        )
        .await;
        rule_ids.push(answered_rule(case, &body).rule_id.unwrap_or_default());
    }

    refused(
        case,
        &router,
        "ModifyTrafficMirrorFilterRule",
        &[
            param("TrafficMirrorFilterRuleId", &rule_ids[1]),
            param("RuleNumber", &RULE_NUMBER.to_string()),
        ],
        400,
        TRAFFIC_MIRROR_FILTER_RULE_ALREADY_EXISTS,
        None,
    )
    .await;

    // The same number is free in the other direction, so moving the rule THERE at that number
    // is the very same request succeeding.
    let case = "the same number in the other direction";
    ok(
        case,
        &router,
        "ModifyTrafficMirrorFilterRule",
        &[
            param("TrafficMirrorFilterRuleId", &rule_ids[1]),
            param("RuleNumber", &RULE_NUMBER.to_string()),
            param("TrafficDirection", TRAFFIC_DIRECTION_EGRESS),
        ],
    )
    .await;
    let moved = described_rule(case, &router, &filter_id, "egressFilterRuleSet").await;
    assert_eq!(
        moved.rule_number.as_deref(),
        Some(RULE_NUMBER.to_string().as_str()),
        "case {case:?}: expected the moved rule at {RULE_NUMBER}, got {moved:?}"
    );
}

/// RULE: a delete takes the rule OUT of the filter it lived on, so the filter no longer
/// reports it and the id stops resolving for every operation that names a rule.
///
/// The rows differ along which direction the deleted rule sat in and how many rules the filter
/// held beside it, so the removal is read as a removal of ONE rule rather than of a set.
#[tokio::test]
async fn a_deleted_rule_stops_being_reported_and_its_id_stops_resolving() {
    for (case, direction, set, beside) in [
        (
            "the only rule of the filter",
            TRAFFIC_DIRECTION_INGRESS,
            "ingressFilterRuleSet",
            0,
        ),
        (
            "one ingress rule of three",
            TRAFFIC_DIRECTION_INGRESS,
            "ingressFilterRuleSet",
            2,
        ),
        (
            "an egress rule beside an ingress one",
            TRAFFIC_DIRECTION_EGRESS,
            "egressFilterRuleSet",
            1,
        ),
    ] {
        let (router, _engine) = super::ec2_query::emulator();
        let filter_id = created_filter(case, &router).await;
        for number in 0..beside {
            ok(
                case,
                &router,
                "CreateTrafficMirrorFilterRule",
                &create_rule_params(&filter_id, 500 + number, Vec::new()),
            )
            .await;
        }
        let body = ok(
            case,
            &router,
            "CreateTrafficMirrorFilterRule",
            &[
                create_rule_params(&filter_id, RULE_NUMBER, Vec::new()),
                vec![param("TrafficDirection", direction)],
            ]
            .concat(),
        )
        .await;
        let rule_id = answered_rule(case, &body).rule_id.unwrap_or_default();

        let deleted = ok(
            case,
            &router,
            "DeleteTrafficMirrorFilterRule",
            &[param("TrafficMirrorFilterRuleId", &rule_id)],
        )
        .await;
        let reported = super::ec2_query::element(&deleted, "trafficMirrorFilterRuleId");
        assert_eq!(
            reported.as_deref(),
            Some(rule_id.as_str()),
            "case {case:?}: the delete must report the id it removed, got {deleted}"
        );

        let remaining = described_rules(case, &router, &filter_id, set).await;
        assert!(
            remaining
                .iter()
                .all(|rule| rule.rule_id.as_deref() != Some(rule_id.as_str())),
            "case {case:?}: {set} must no longer carry {rule_id}, got {remaining:?}"
        );

        // The id resolves for nothing afterwards: a second delete and a modification of it are
        // both refused with this resource's own `NotFound`.
        for action in ["DeleteTrafficMirrorFilterRule", "ModifyTrafficMirrorFilterRule"] {
            refused(
                case,
                &router,
                action,
                &[
                    param("TrafficMirrorFilterRuleId", &rule_id),
                    param("Description", "never stored"),
                ],
                400,
                INVALID_TRAFFIC_MIRROR_FILTER_RULE_ID_NOT_FOUND,
                Some(&format!("{rule_id} does not exist")),
            )
            .await;
        }
    }
}
