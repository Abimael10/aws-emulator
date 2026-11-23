// SPDX-License-Identifier: CC0-1.0

//! What a web ACL in front of an Application Load Balancer LEAVES BEHIND about the traffic it
//! decided: one log record per request, in the single destination its logging configuration
//! names.
//!
//! The rule under test, one level above any single request: **while the web ACL associated with
//! a load balancer carries a logging configuration, EVERY request it evaluates for that load
//! balancer leaves exactly ONE record in the destination that configuration names — so a
//! sequence of n evaluated requests leaves n records there, each naming the web ACL that
//! decided, the rule that ended the evaluation and its kind (the rule group for a request a
//! referenced rule group terminated), the action applied and the request it describes.**
//!
//! A `LoggingConfiguration` "defines an association between logging destinations and a web ACL
//! resource, for logging from WAF" and `PutLoggingConfiguration` "enables the specified
//! LoggingConfiguration, to start logging from a web ACL", and "you can associate one logging
//! destination to a web ACL" (`LoggingConfiguration$LogDestinationConfigs`) whose name "must
//! start with `aws-waf-logs-`" — so the destination is ONE place that accumulates the records of
//! all the traffic the web ACL evaluated, a request it blocked as much as one it let through.
//! `AssociateWebACLRequest$ResourceArn` names an Application Load Balancer as a protected
//! resource form, so the traffic here is this service's.
//!
//! The sibling suite [`crate::web_acl_enforcement`] pins what the CLIENT is answered for such a
//! request; this one pins what the destination HOLDS afterwards, which is the only record of what
//! the firewall in front of the application decided. Both are driven through the router's front
//! door, with WAFv2 (the binding, the evaluation and the record) and CloudWatch Logs (the
//! destination) mounted beside this service on one engine, so a record is read back where a
//! client reads it — through that service's own reader — and not out of the delivering service's
//! store.
//!
//! The rows differ along what the rule generalises over: HOW MANY requests the web ACL evaluates
//! in a row, in WHICH ORDER their decisions fall, and WHICH decision each one is (a rule inside
//! the referenced rule group terminated it, or no rule matched and the web ACL's `DefaultAction`
//! decided). The recorded case — one rule-group-blocked request and one default-allowed request
//! sharing one destination — is one row.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::alb_data_plane::{create_load_balancer, send, settle};
use crate::provisioning_loop::{account, action, call, p, REGION};
use crate::web_acl_enforcement::{byte_match, waf_ok, BLOCK_STATUS};

/// The body the listener's default action answers with: the request reached the load balancer's
/// own configuration, so the web ACL let it through.
pub(crate) const SERVED_BODY: &str = "served-by-the-listener";

/// The status that answer carries.
pub(crate) const SERVED_STATUS: u16 = 200;

/// The header every request carries, and the two values a request is driven with: the one the
/// rule inside the rule group matches, and one it does not.
pub(crate) const PROBE_HEADER: &str = "x-waf-probe";
const MATCHING_PROBE: &str = "deny-me";
const UNMATCHED_PROBE: &str = "let-me-in";

/// The rule INSIDE the rule group this account owns, which blocks what it matches.
const GROUP_RULE: &str = "deny-the-probe";

/// The web ACL rule that RUNS that group, which is the rule a record names as having ended the
/// evaluation of a request the group terminated.
const RUNS_THE_GROUP: &str = "run-the-group";

/// The `terminatingRuleId` of a request no rule terminated, which the web ACL's `DefaultAction`
/// decided — "the action to perform if none of the Rules contained in the WebACL match"
/// (`WebACL$DefaultAction`).
pub(crate) const DEFAULT_ACTION_RULE: &str = "Default_Action";

/// The `terminatingRuleType` of the web ACL's own `DefaultAction`, and the one of a rule that
/// runs a rule group this account owns.
pub(crate) const REGULAR_TYPE: &str = "REGULAR";
pub(crate) const GROUP_TYPE: &str = "GROUP";

/// The prefix the name of a logging destination must begin with
/// (`smithy_query(query="doc",shape="LoggingConfiguration")`: "the name that you give the
/// destination must start with `aws-waf-logs-`").
pub(crate) const WAF_LOGS_PREFIX: &str = "aws-waf-logs-";

/// How long a delivered record is given to be readable in its destination group, and how long to
/// wait between two looks. Generous on purpose: a row waits for the RECORDS, never for a
/// duration, so a loaded host does not decide the outcome.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(20);

/// A router carrying this service, the WAFv2 that holds the binding, the evaluation and the
/// record, and the CloudWatch Logs that holds the destination — all over ONE engine. All three
/// are needed: a record is only produced when the traffic, the web ACL and the destination live
/// in one emulator.
pub(crate) fn emulator_with_waf_and_logs() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    (router, engine)
}

/// One CloudWatch Logs call (`POST /` with `X-Amz-Target: Logs_20140328.<Operation>`), the way a
/// client makes it, which must succeed.
async fn logs_ok(router: &MultiServiceRouter, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/logs/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "setup: CloudWatch Logs {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// Create the log group `name` through CloudWatch Logs' own API and answer the ARN a logging
/// configuration names it by, so the destination is a group the account really holds.
pub(crate) async fn log_destination(router: &MultiServiceRouter, name: &str) -> String {
    assert!(
        name.starts_with(WAF_LOGS_PREFIX),
        "setup: a WAF logging destination's name must start with {WAF_LOGS_PREFIX:?}, got {name:?}"
    );
    logs_ok(router, "CreateLogGroup", json!({ "logGroupName": name })).await;
    format!("arn:aws:logs:{REGION}:{}:log-group:{name}", account())
}

/// Every record the log group `name` holds, each parsed as the JSON document a WAF log record is,
/// read through CloudWatch Logs' own reader.
async fn in_log_group(router: &MultiServiceRouter, name: &str) -> Vec<Value> {
    let read = logs_ok(router, "FilterLogEvents", json!({ "logGroupName": name })).await;
    read["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .map(|event| {
                    serde_json::from_str(event["message"].as_str().unwrap_or_default())
                        .unwrap_or(Value::Null)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Look in the log group until it holds `want` records, which is what a reader of the destination
/// waits for, and answer every record it holds then.
pub(crate) async fn records_in(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    want: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let records = in_log_group(router, group).await;
        if records.len() >= want {
            return records;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the destination {group} holds {} of the {want} records the requests \
             it took so far must have left, got {records:?}",
            records.len()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A minimal `VisibilityConfig`, as every web ACL, rule and rule group needs one.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": true,
        "CloudWatchMetricsEnabled": true,
        "MetricName": metric,
    })
}

/// One rule of a rule group this account owns: its name, the value of [`PROBE_HEADER`] it matches
/// EXACTLY, and the `Action` it carries.
pub(crate) struct GroupRule<'rule> {
    pub(crate) name: &'rule str,
    pub(crate) probe: &'rule str,
    pub(crate) action: Value,
}

/// The rule group this account owns holding `rules`, each matching its own probe value on the
/// probe header; answers the ARN a web ACL references it by.
pub(crate) async fn rule_group(
    router: &MultiServiceRouter,
    name: &str,
    rules: &[GroupRule<'_>],
) -> String {
    let declared: Vec<Value> = rules
        .iter()
        .enumerate()
        .map(|(priority, inner)| {
            json!({
                "Name": inner.name,
                "Priority": priority,
                "Statement": byte_match(
                    json!({ "SingleHeader": { "Name": PROBE_HEADER } }),
                    inner.probe,
                    "EXACTLY",
                ),
                "Action": inner.action,
                "VisibilityConfig": visibility(inner.name),
            })
        })
        .collect();
    let created = waf_ok(
        router,
        "CreateRuleGroup",
        json!({
            "Name": name,
            "Scope": "REGIONAL",
            "Capacity": 100,
            "VisibilityConfig": visibility(name),
            "Rules": declared,
        }),
    )
    .await;
    created["Summary"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateRuleGroup must answer Summary.ARN, got {created}"))
        .to_string()
}

/// One web ACL rule that RUNS the referenced rule group `group_arn`, at `priority`, with the
/// group's own rule actions left in force (`OverrideAction` `None`).
pub(crate) fn runs_the_group(name: &str, priority: usize, group_arn: &str) -> Value {
    json!({
        "Name": name,
        "Priority": priority,
        "Statement": { "RuleGroupReferenceStatement": { "ARN": group_arn } },
        "OverrideAction": { "None": {} },
        "VisibilityConfig": visibility(name),
    })
}

/// A web ACL that allows by default and carries `rules`, created and put in front of `lb_arn`;
/// answers its ARN.
pub(crate) async fn web_acl_in_front(
    router: &MultiServiceRouter,
    name: &str,
    rules: Value,
    lb_arn: &str,
) -> String {
    let created = waf_ok(
        router,
        "CreateWebACL",
        json!({
            "Name": name,
            "Scope": "REGIONAL",
            "DefaultAction": { "Allow": {} },
            "VisibilityConfig": visibility(name),
            "Rules": rules,
        }),
    )
    .await;
    let arn = created["Summary"]["ARN"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateWebACL must answer Summary.ARN, got {created}"))
        .to_string();
    waf_ok(
        router,
        "AssociateWebACL",
        json!({ "ResourceArn": lb_arn, "WebACLArn": arn }),
    )
    .await;
    arn
}

/// Point the logging of the web ACL `web_acl` at `destination`.
pub(crate) async fn log_to(router: &MultiServiceRouter, web_acl: &str, destination: &str) {
    waf_ok(
        router,
        "PutLoggingConfiguration",
        json!({
            "LoggingConfiguration": {
                "ResourceArn": web_acl,
                "LogDestinationConfigs": [destination],
            }
        }),
    )
    .await;
}

/// The HTTP:80 listener every request the web ACL lets through is answered by: one default
/// `fixed-response` of [`SERVED_STATUS`] carrying [`SERVED_BODY`].
pub(crate) async fn create_listener(router: &MultiServiceRouter, lb_arn: &str) {
    let mut form = action("CreateListener");
    form.push(p("LoadBalancerArn", lb_arn));
    form.push(p("Protocol", "HTTP"));
    form.push(p("Port", "80"));
    form.push(p("DefaultActions.member.1.Type", "fixed-response"));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.StatusCode",
        &SERVED_STATUS.to_string(),
    ));
    form.push(p(
        "DefaultActions.member.1.FixedResponseConfig.MessageBody",
        SERVED_BODY,
    ));
    let reply = call(router, &form).await;
    assert_eq!(
        reply.status, 200,
        "setup: CreateListener must answer 200, got {} body {}",
        reply.status, reply.body
    );
}

/// One load balancer the clock already owes its move to `active`, carrying that listener, with
/// the rule group and the web ACL in front of it.
struct Deployed {
    router: MultiServiceRouter,
    dns_name: String,
    group_arn: String,
    web_acl: String,
}

/// Everything one scenario drives, named `name`: the load balancer, its listener, the rule group
/// and the web ACL in front of it. The logging destination is left to the scenario, because WHERE
/// the records go is what a scenario varies.
async fn deployed(name: &str) -> Deployed {
    let (router, engine) = emulator_with_waf_and_logs();
    let (lb_arn, dns_name) = create_load_balancer(&router, name).await;
    create_listener(&router, &lb_arn).await;
    settle(&engine, &lb_arn, SystemTime::now() - Duration::from_secs(1));
    let group_arn = rule_group(
        &router,
        &format!("{WAF_LOGS_PREFIX}{name}-group"),
        &[GroupRule {
            name: GROUP_RULE,
            probe: MATCHING_PROBE,
            action: json!({ "Block": {} }),
        }],
    )
    .await;
    let web_acl = web_acl_in_front(
        &router,
        name,
        json!([runs_the_group(RUNS_THE_GROUP, 0, &group_arn)]),
        &lb_arn,
    )
    .await;
    Deployed {
        router,
        dns_name,
        group_arn,
        web_acl,
    }
}

/// Which decision one request of a sequence is given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// A rule INSIDE the referenced rule group terminated the evaluation with a `Block`.
    BlockedByTheGroup,
    /// No rule matched, so the web ACL's `DefaultAction` allowed the request.
    AllowedByDefault,
}

impl Decision {
    /// The value of the probe header a request given this decision carries.
    fn probe(self) -> &'static str {
        match self {
            Self::BlockedByTheGroup => MATCHING_PROBE,
            Self::AllowedByDefault => UNMATCHED_PROBE,
        }
    }

    /// The status the client is answered under.
    fn status(self) -> u16 {
        match self {
            Self::BlockedByTheGroup => BLOCK_STATUS,
            Self::AllowedByDefault => SERVED_STATUS,
        }
    }

    /// The `action` the record names.
    fn action(self) -> &'static str {
        match self {
            Self::BlockedByTheGroup => "BLOCK",
            Self::AllowedByDefault => "ALLOW",
        }
    }

    /// The `terminatingRuleId` and `terminatingRuleType` the record names: the web ACL rule that
    /// ran the group for a request the group terminated, and the web ACL's own default action for
    /// a request no rule matched.
    fn terminating(self) -> (&'static str, &'static str) {
        match self {
            Self::BlockedByTheGroup => (RUNS_THE_GROUP, GROUP_TYPE),
            Self::AllowedByDefault => (DEFAULT_ACTION_RULE, REGULAR_TYPE),
        }
    }

    /// What the record's `ruleGroupList` entry for the referenced group must say its terminating
    /// rule was: the group's own rule for a request it terminated, and nothing for a request it
    /// left alone.
    fn terminating_rule_of_the_group(self) -> Value {
        match self {
            Self::BlockedByTheGroup => json!({ "ruleId": GROUP_RULE, "action": "BLOCK" }),
            Self::AllowedByDefault => Value::Null,
        }
    }
}

/// One row: a SEQUENCE of requests one web ACL evaluates for one load balancer, all logged into
/// one destination.
struct Row {
    case: &'static str,
    /// The name of this row's load balancer, its web ACL and its destination group.
    name: &'static str,
    /// The decision each request in the sequence is given, in the order they are sent.
    sequence: &'static [Decision],
}

/// Every row of the table.
fn rows() -> Vec<Row> {
    use Decision::{AllowedByDefault, BlockedByTheGroup};
    vec![
        // The recorded pair: one request a rule of the referenced rule group blocked and one the
        // same web ACL's default action allowed, both logged into the one destination.
        Row {
            case: "a rule-group-blocked request and a default-allowed one share the destination",
            name: "waf-logs-alb-pair",
            sequence: &[BlockedByTheGroup, AllowedByDefault],
        },
        // The same pair in the other order: which decision came first changes nothing about the
        // records the destination ends up holding.
        Row {
            case: "the same two decisions in the other order",
            name: "waf-logs-alb-reversed",
            sequence: &[AllowedByDefault, BlockedByTheGroup],
        },
        // One request on its own, of each decision.
        Row {
            case: "one rule-group-blocked request on its own",
            name: "waf-logs-alb-one-blocked",
            sequence: &[BlockedByTheGroup],
        },
        Row {
            case: "one default-allowed request on its own",
            name: "waf-logs-alb-one-allowed",
            sequence: &[AllowedByDefault],
        },
        // Repeats of each decision, interleaved: a record per evaluated request, not per distinct
        // decision the web ACL can make.
        Row {
            case: "repeated decisions, interleaved",
            name: "waf-logs-alb-repeats",
            sequence: &[
                AllowedByDefault,
                BlockedByTheGroup,
                BlockedByTheGroup,
                AllowedByDefault,
                BlockedByTheGroup,
            ],
        },
    ]
}

/// The path the request at position `index` of a sequence asks for, which is what its own record
/// is found by: every request of a row addresses its own path, so no record can stand in for
/// another's.
pub(crate) fn path_of(index: usize) -> String {
    format!("/probe-{index}")
}

/// The one record of `records` whose `httpRequest` describes the request for `path`.
pub(crate) fn record_for<'a>(case: &str, records: &'a [Value], path: &str) -> &'a Value {
    let matching: Vec<&Value> = records
        .iter()
        .filter(|record| record["httpRequest"]["uri"] == json!(path))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: the request for {path:?} must leave exactly one record, got {matching:?} \
         among {records:?}"
    );
    matching[0]
}

/// The value one header of the record's `httpRequest` shows, or `None` when the record carries no
/// entry for it.
fn header_shown(record: &Value, name: &str) -> Option<String> {
    record["httpRequest"]["headers"]
        .as_array()?
        .iter()
        .find(|entry| {
            entry["name"]
                .as_str()
                .is_some_and(|written| written.eq_ignore_ascii_case(name))
        })
        .and_then(|entry| entry["value"].as_str().map(str::to_string))
}

/// Assert everything the record of the request at `index` of `deployment`'s traffic must say
/// about the decision it was given and about the request it describes.
fn assert_record(
    case: &str,
    deployment: &Deployed,
    row_decision: Decision,
    index: usize,
    record: &Value,
) {
    let path = path_of(index);
    assert_eq!(
        record["webaclId"],
        json!(deployment.web_acl),
        "case {case:?}: the record of request {index} must name the web ACL that decided, got \
         {record}"
    );
    assert_eq!(
        record["action"],
        json!(row_decision.action()),
        "case {case:?}: expected request {index} to be recorded as {:?}, got {record}",
        row_decision.action()
    );
    let (terminating, terminating_type) = row_decision.terminating();
    assert_eq!(
        record["terminatingRuleId"],
        json!(terminating),
        "case {case:?}: expected request {index} to name the terminating rule {terminating:?}, \
         got {record}"
    );
    assert_eq!(
        record["terminatingRuleType"],
        json!(terminating_type),
        "case {case:?}: expected request {index} to name the terminating rule type \
         {terminating_type:?}, got {record}"
    );
    // The referenced rule group the web ACL ran, and what it did to this request: the one group
    // the web ACL's rules name, and the rule inside it that ended the evaluation where one did.
    // The entry's other members are each pinned by the suites of the service that composes a
    // record; what the rule here requires is that the terminating group is reported and named.
    let groups = record["ruleGroupList"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        groups.len(),
        1,
        "case {case:?}: expected request {index} to report the one rule group the web ACL ran, \
         got {record}"
    );
    assert_eq!(
        groups[0]["ruleGroupId"],
        json!(deployment.group_arn),
        "case {case:?}: expected request {index} to name the rule group the web ACL ran, got \
         {record}"
    );
    assert_eq!(
        groups[0]["terminatingRule"],
        row_decision.terminating_rule_of_the_group(),
        "case {case:?}: expected request {index} to report what that rule group did to it, got \
         {record}"
    );
    // And the request itself, as WAF inspected it.
    assert_eq!(
        record["httpRequest"]["uri"],
        json!(path),
        "case {case:?}: expected request {index} to be recorded for {path:?}, got {record}"
    );
    let shown = header_shown(record, PROBE_HEADER);
    assert_eq!(
        shown.as_deref(),
        Some(row_decision.probe()),
        "case {case:?}: expected request {index} to show the header it carried, got {record}"
    );
    // A record is of an INSTANT, which is what a reader orders the traffic by.
    let at = record["timestamp"].as_i64();
    assert!(
        at.is_some_and(|millis| millis > 0),
        "case {case:?}: expected request {index} to be recorded at an instant, got {record}"
    );
}

/// RULE: while the web ACL in front of a load balancer carries a logging configuration, every
/// request it evaluates leaves exactly one record in the destination that configuration names.
#[tokio::test]
async fn every_request_the_web_acl_evaluates_leaves_one_record_in_its_destination() {
    for row in rows() {
        let case = row.case;
        let deployment = deployed(row.name).await;
        let group = format!("{WAF_LOGS_PREFIX}{}", row.name);
        let destination = log_destination(&deployment.router, &group).await;
        log_to(&deployment.router, &deployment.web_acl, &destination).await;

        // One request at a time: after the nth, the destination holds n records and no more, so a
        // path that logs nothing fails here and so does one that logs twice.
        for (index, decision) in row.sequence.iter().copied().enumerate() {
            let path = path_of(index);
            let answered = send(
                &deployment.router,
                "GET",
                &deployment.dns_name,
                &path,
                &[(PROBE_HEADER, decision.probe())],
            )
            .await;
            assert_eq!(
                answered.status,
                decision.status(),
                "case {case:?}: expected request {index} to be answered {}, got {} with {:?}",
                decision.status(),
                answered.status,
                answered.body
            );
            if decision == Decision::AllowedByDefault {
                assert_eq!(
                    answered.body, SERVED_BODY,
                    "case {case:?}: expected request {index} to reach the listener, got {:?}",
                    answered.body
                );
            }

            let want = index + 1;
            let records = records_in(&deployment.router, case, &group, want).await;
            assert_eq!(
                records.len(),
                want,
                "case {case:?}: {want} evaluated requests are {want} records, got {records:?}"
            );
        }

        // Every record is the record of its own request, naming the decision that request was
        // given.
        let records = records_in(&deployment.router, case, &group, row.sequence.len()).await;
        for (index, decision) in row.sequence.iter().copied().enumerate() {
            let record = record_for(case, &records, &path_of(index));
            assert_record(case, &deployment, decision, index, record);
        }
    }
}

/// The destination is read PER REQUEST: pointing the same web ACL's logging at a second group
/// sends the records of the requests evaluated from then on there, and leaves the records already
/// delivered where they were.
#[tokio::test]
async fn the_destination_a_record_goes_to_is_the_one_configured_when_it_was_evaluated() {
    let case = "the destination replaced";
    let deployment = deployed("waf-logs-alb-moved").await;
    let first = format!("{WAF_LOGS_PREFIX}waf-logs-alb-moved-first");
    let second = format!("{WAF_LOGS_PREFIX}waf-logs-alb-moved-second");
    let first_destination = log_destination(&deployment.router, &first).await;
    let second_destination = log_destination(&deployment.router, &second).await;

    log_to(&deployment.router, &deployment.web_acl, &first_destination).await;
    let blocked = send(
        &deployment.router,
        "GET",
        &deployment.dns_name,
        &path_of(0),
        &[(PROBE_HEADER, Decision::BlockedByTheGroup.probe())],
    )
    .await;
    assert_eq!(
        blocked.status,
        BLOCK_STATUS,
        "case {case:?}: expected the web ACL to refuse the probed request, got {} with {:?}",
        blocked.status,
        blocked.body
    );
    let records = records_in(&deployment.router, case, &first, 1).await;
    assert_record(
        case,
        &deployment,
        Decision::BlockedByTheGroup,
        0,
        record_for(case, &records, &path_of(0)),
    );

    // The configuration now names the second group, and the next request is evaluated by the very
    // same web ACL.
    log_to(&deployment.router, &deployment.web_acl, &second_destination).await;
    let served = send(
        &deployment.router,
        "GET",
        &deployment.dns_name,
        &path_of(1),
        &[(PROBE_HEADER, Decision::AllowedByDefault.probe())],
    )
    .await;
    assert_eq!(
        (served.status, served.body.as_str()),
        (SERVED_STATUS, SERVED_BODY),
        "case {case:?}: expected the listener's own answer, got {} with {:?}",
        served.status,
        served.body
    );
    let moved = records_in(&deployment.router, case, &second, 1).await;
    assert_eq!(
        moved.len(),
        1,
        "case {case:?}: the destination put second holds the one request evaluated after it, got \
         {moved:?}"
    );
    assert_record(
        case,
        &deployment,
        Decision::AllowedByDefault,
        1,
        record_for(case, &moved, &path_of(1)),
    );
    let stayed = in_log_group(&deployment.router, &first).await;
    assert_eq!(
        stayed.len(),
        1,
        "case {case:?}: the destination put first keeps the record it already took and takes no \
         other, got {stayed:?}"
    );
}
