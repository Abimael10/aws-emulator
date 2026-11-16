// SPDX-License-Identifier: CC0-1.0

//! What a web ACL's log record says about the rule GROUPS it ran, end to end over the
//! wire.
//!
//! The rule under test: a log record reports, for every rule GROUP the web ACL ran the
//! request against, which group it was and what that group's own rules did to the
//! request — the group's rule that ended the evaluation and the group's rules that only
//! counted it, each by the group-local name that group declares it under, plus the rules
//! the web ACL took out of force and left counting. A group whose rules all left the
//! request alone is listed too, with nothing under it, because the group still ran; the
//! member is always present, and empty only for a request no rule group was run against.
//! The web ACL rule that RUNS the group is what the record names as the rule that
//! terminated the evaluation, so without the per-group entry a reader cannot tell WHICH
//! rule of the group refused the request.
//!
//! The rows differ along what the rule generalises over: what the group's rule did to the
//! request (ended the evaluation, counted it, matched nothing), what the web ACL put in
//! the place of that rule's own action (nothing, the whole group counted by
//! `OverrideAction`, the rule excluded by `ExcludedRules`, the rule overridden to count by
//! `RuleActionOverrides`), which KIND of group ran it (one this account owns, named by its
//! ARN, and a vendor's managed group, named by its vendor and its own name), a group the
//! request never reached at all because the reference's `ScopeDownStatement` does not
//! match, and a rule of the web ACL itself, which lives in no group.
//!
//! Every row drives the request through a protected resource's own endpoint — an Amazon
//! Cognito user pool, one of the resource kinds `AssociateWebACL` takes — and reads the
//! record back out of the CloudWatch Logs log group the web ACL's
//! `LoggingConfiguration` names, through that service's own reader. So what each row
//! observes is the record as a client reads it, not a value computed in-process. Each row
//! also re-checks that the record identifies the resource the request arrived at the way
//! the service holding it does: the account and the resource's own id (the per-ARN-form
//! catalogue of that identity is pinned in `service::logs`'s own tests).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call is signed for and every ARN below names.
const REGION: &str = "us-west-2";

/// The `Scope` a user pool's web ACL lives in: a user pool is a REGIONAL resource.
const SCOPE: &str = "REGIONAL";

/// The header every request carries, and the two values a row drives it with: one the
/// rule group's rule matches, one it does not.
const PROBE_HEADER: &str = "x-waf-group-probe";
const MATCHING_PROBE: &str = "deny-me";
const UNMATCHED_PROBE: &str = "leave-me";

/// The rule INSIDE the rule group this account owns, which blocks what it matches.
const GROUP_RULE: &str = "deny-the-probe";

/// The web ACL rule that RUNS that group.
const RUNS_THE_GROUP: &str = "run-the-group";

/// The web ACL rule that runs the vendor's managed group.
const RUNS_THE_MANAGED_GROUP: &str = "run-the-managed-group";

/// A web ACL rule of its own, which blocks what it matches and lives in no group.
const OWN_RULE: &str = "block-the-probe";

/// The `terminatingRuleId` of a request no rule terminated.
const DEFAULT_ACTION_RULE: &str = "Default_Action";

/// The vendor of the managed rule groups this emulator publishes.
const VENDOR: &str = "AWS";

/// The managed rule group one row runs, and the rule of it that decides the row's
/// request: a request carrying no `User-Agent` header. Both are read back from the
/// service's OWN declaration of that group (`DescribeManagedRuleGroup`, whose
/// `RuleSummary` list is "high-level information about a Rule") before the row runs, so a
/// name written here that the group does not declare fails the case.
const MANAGED_GROUP: &str = "AWSManagedRulesCommonRuleSet";
const MANAGED_RULE: &str = "NoUserAgent_HEADER";

/// The status a request the web ACL did not let through is answered with.
const FORBIDDEN_STATUS: u16 = 403;

/// One emulator with WAFv2, the service whose resource the web ACL sits in front of, and
/// the service that holds the logging destination, all on one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // The suite exercises the services, not SigV4: every call carries a credential
        // scope so the router can name the target service, and no signature is verified.
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_wafv2_awswaf_20190729::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what
    /// routes the request. The signature itself is never checked here.
    fn authorization(signing_name: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/\
             aws4_request, SignedHeaders=host, Signature=0000"
        )
    }

    /// Dispatch one request as it arrives on the wire, answering its status and body bytes.
    async fn dispatch(
        &self,
        host: &str,
        headers: &[(&str, &str)],
        body: Bytes,
    ) -> (u16, Vec<u8>) {
        let mut builder = http::Request::builder()
            .method("POST")
            .uri(format!("https://{host}/"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let response = self
            .router
            .dispatch(builder.body(body).expect("a well-formed request"))
            .await;
        (response.status().as_u16(), response.into_body().to_vec())
    }

    /// One awsJson1_1 call of the service `signing_name`, whose `X-Amz-Target` prefix is
    /// `target_prefix`.
    async fn json_rpc(
        &self,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: Value,
    ) -> (u16, Value) {
        let (status, bytes) = self
            .dispatch(
                &format!("{signing_name}.{REGION}.amazonaws.com"),
                &[
                    ("content-type", "application/x-amz-json-1.1"),
                    ("authorization", &Self::authorization(signing_name)),
                    ("x-amz-target", &format!("{target_prefix}.{operation}")),
                ],
                Bytes::from(body.to_string()),
            )
            .await;
        let parsed = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, parsed)
    }

    /// One WAFv2 call that has to succeed.
    async fn waf_ok(&self, operation: &str, body: Value) -> Value {
        let (status, parsed) = self
            .json_rpc("wafv2", "AWSWAF_20190729", operation, body)
            .await;
        assert_eq!(
            status, 200,
            "setup {operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// One Amazon Cognito call that has to succeed.
    async fn cognito_ok(&self, operation: &str, body: Value) -> Value {
        let (status, parsed) = self
            .json_rpc(
                "cognito-idp",
                "AWSCognitoIdentityProviderService",
                operation,
                body,
            )
            .await;
        assert_eq!(
            status, 200,
            "setup {operation}: expected HTTP 200, got {status} with {parsed}"
        );
        parsed
    }

    /// Create the log group `name` through CloudWatch Logs' own wire, so the destination
    /// the configuration names is a group the account really holds.
    async fn given_log_group(&self, name: &str) {
        let (status, payload) = self
            .json_rpc(
                "logs",
                "Logs_20140328",
                "CreateLogGroup",
                json!({ "logGroupName": name }),
            )
            .await;
        assert_eq!(
            status, 200,
            "setup CreateLogGroup({name}): expected HTTP 200, got {status} with {payload}"
        );
    }

    /// The records `FilterLogEvents` reports for `group`, each parsed as the JSON document
    /// a WAF log record is.
    async fn in_log_group(&self, group: &str) -> Vec<Value> {
        let (status, body) = self
            .json_rpc(
                "logs",
                "Logs_20140328",
                "FilterLogEvents",
                json!({ "logGroupName": group }),
            )
            .await;
        assert_eq!(
            status, 200,
            "FilterLogEvents({group}): expected HTTP 200, got {status} with {body}"
        );
        body["events"]
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
}

/// A `VisibilityConfig` for a dimension, which every rule, rule group and web ACL needs.
fn visibility(metric: &str) -> Value {
    json!({
        "SampledRequestsEnabled": true,
        "CloudWatchMetricsEnabled": true,
        "MetricName": metric,
    })
}

/// A `Blob` member travels base64-encoded in an awsJson body, which is what
/// `ByteMatchStatement$SearchString` is.
fn base64_of(value: &str) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(value.as_bytes())
}

/// The statement every blocking rule here carries: the probe header holding the value the
/// row that is meant to match sends.
fn probe_statement() -> Value {
    json!({
        "ByteMatchStatement": {
            "SearchString": base64_of(MATCHING_PROBE),
            "FieldToMatch": { "SingleHeader": { "Name": PROBE_HEADER } },
            "TextTransformations": [{ "Priority": 0, "Type": "NONE" }],
            "PositionalConstraint": "EXACTLY",
        }
    })
}

/// A web ACL rule that runs the rule group `arn` this account owns, under
/// `override_action`, with `statement_members` (`ExcludedRules`, `RuleActionOverrides`)
/// beside the reference's own `ARN`.
fn group_reference(arn: &str, statement_members: Value, override_action: Value) -> Value {
    let mut statement = json!({ "RuleGroupReferenceStatement": { "ARN": arn } });
    for (member, value) in statement_members.as_object().into_iter().flatten() {
        statement["RuleGroupReferenceStatement"][member] = value.clone();
    }
    json!([{
        "Name": RUNS_THE_GROUP,
        "Priority": 0,
        "Statement": statement,
        "OverrideAction": override_action,
        "VisibilityConfig": visibility(RUNS_THE_GROUP),
    }])
}

/// The web ACL's `Rules` for the row that runs the vendor's managed group.
fn managed_reference(_arn: &str) -> Value {
    json!([{
        "Name": RUNS_THE_MANAGED_GROUP,
        "Priority": 0,
        "Statement": {
            "ManagedRuleGroupStatement": { "VendorName": VENDOR, "Name": MANAGED_GROUP }
        },
        "OverrideAction": { "None": {} },
        "VisibilityConfig": visibility(RUNS_THE_MANAGED_GROUP),
    }])
}

/// The web ACL's `Rules` for the row whose managed group is NOT run: its
/// `ScopeDownStatement` narrows the group to requests carrying the matching probe, and
/// that row's request carries the other one. "Requests are only evaluated by the rule
/// group reference statement when they match the scope-down statement"
/// (`ManagedRuleGroupStatement$ScopeDownStatement`).
fn managed_scoped_down(_arn: &str) -> Value {
    json!([{
        "Name": RUNS_THE_MANAGED_GROUP,
        "Priority": 0,
        "Statement": {
            "ManagedRuleGroupStatement": {
                "VendorName": VENDOR,
                "Name": MANAGED_GROUP,
                "ScopeDownStatement": probe_statement(),
            }
        },
        "OverrideAction": { "None": {} },
        "VisibilityConfig": visibility(RUNS_THE_MANAGED_GROUP),
    }])
}

/// The web ACL's `Rules` for the row whose blocking rule is the web ACL's own.
fn own_rule(_arn: &str) -> Value {
    json!([{
        "Name": OWN_RULE,
        "Priority": 0,
        "Statement": probe_statement(),
        "Action": { "Block": {} },
        "VisibilityConfig": visibility(OWN_RULE),
    }])
}

/// One `ruleGroupList` entry, as a row states what it expects.
fn group_entry(id: &str, terminating: Value, non_terminating: Value, excluded: Value) -> Value {
    json!({
        "ruleGroupId": id,
        "terminatingRule": terminating,
        "nonTerminatingMatchingRules": non_terminating,
        "excludedRules": excluded,
    })
}

/// The user pool and app client one row's requests are made against: a pool is one of the
/// resource kinds `AssociateWebACLRequest$ResourceArn` takes, and the governed half of
/// that pool's API names its pool by the app client the call is made through.
async fn pool_with_a_client(emulator: &Emulator, name: &str) -> (String, String) {
    let created = emulator
        .cognito_ok("CreateUserPool", json!({ "PoolName": name }))
        .await;
    let id = created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let arn = created["UserPool"]["Arn"]
        .as_str()
        .expect("the pool's ARN")
        .to_string();
    let client = emulator
        .cognito_ok(
            "CreateUserPoolClient",
            json!({ "UserPoolId": id, "ClientName": format!("{name}-client") }),
        )
        .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    (arn, client_id)
}

/// The rule group this account owns, whose one rule BLOCKS the probed request; answers its
/// ARN.
async fn blocking_rule_group(emulator: &Emulator, name: &str) -> String {
    let created = emulator
        .waf_ok(
            "CreateRuleGroup",
            json!({
                "Name": name,
                "Scope": SCOPE,
                "Capacity": 100,
                "VisibilityConfig": visibility(name),
                "Rules": [{
                    "Name": GROUP_RULE,
                    "Priority": 0,
                    "Statement": probe_statement(),
                    "Action": { "Block": {} },
                    "VisibilityConfig": visibility(GROUP_RULE),
                }],
            }),
        )
        .await;
    created["Summary"]["ARN"]
        .as_str()
        .expect("Summary.ARN")
        .to_string()
}

/// The group-local rule names the managed group `group` DECLARES, read over the wire.
async fn declared_managed_rules(emulator: &Emulator, group: &str) -> Vec<String> {
    let described = emulator
        .waf_ok(
            "DescribeManagedRuleGroup",
            json!({ "VendorName": VENDOR, "Name": group, "Scope": SCOPE }),
        )
        .await;
    described["Rules"]
        .as_array()
        .map(|rules| {
            rules
                .iter()
                .filter_map(|rule| rule["Name"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// Create the web ACL `name` with `rules` and the default `Allow`, put it in front of
/// `resource_arn`, point its logging at `destination`, and answer its ARN.
async fn web_acl_in_front(
    emulator: &Emulator,
    name: &str,
    rules: Value,
    resource_arn: &str,
    destination: &str,
) -> String {
    let created = emulator
        .waf_ok(
            "CreateWebACL",
            json!({
                "Name": name,
                "Scope": SCOPE,
                "DefaultAction": { "Allow": {} },
                "VisibilityConfig": visibility(name),
                "Rules": rules,
            }),
        )
        .await;
    let arn = created["Summary"]["ARN"]
        .as_str()
        .expect("Summary.ARN")
        .to_string();
    emulator
        .waf_ok(
            "AssociateWebACL",
            json!({ "WebACLArn": arn, "ResourceArn": resource_arn }),
        )
        .await;
    emulator
        .waf_ok(
            "PutLoggingConfiguration",
            json!({
                "LoggingConfiguration": {
                    "ResourceArn": arn,
                    "LogDestinationConfigs": [destination],
                }
            }),
        )
        .await;
    arn
}

/// Drive ONE request the web ACL inspects: a governed operation of the pool's own API,
/// carrying the probe header. Answers the status the request was answered with, which is
/// the web ACL's refusal for a request it did not let through.
async fn one_inspected_request(emulator: &Emulator, client_id: &str, probe: &str) -> u16 {
    let (status, _) = emulator
        .dispatch(
            &format!("cognito-idp.{REGION}.amazonaws.com"),
            &[
                ("content-type", "application/x-amz-json-1.1"),
                (
                    "authorization",
                    &Emulator::authorization("cognito-idp"),
                ),
                (
                    "x-amz-target",
                    "AWSCognitoIdentityProviderService.ForgotPassword",
                ),
                (PROBE_HEADER, probe),
            ],
            Bytes::from(
                json!({ "ClientId": client_id, "Username": "group-activity-probe" }).to_string(),
            ),
        )
        .await;
    status
}

/// One row: the evaluation the request is driven through, and what the record it leaves
/// must say about the rule groups the web ACL ran.
struct Row {
    case: &'static str,
    /// The name every resource of the row is created under.
    name: &'static str,
    /// The web ACL's `Rules`, given the ARN of the rule group this account owns.
    rules: fn(&str) -> Value,
    /// The value the probe header carries.
    probe: &'static str,
    /// The `action` the record names.
    action: &'static str,
    /// The `terminatingRuleId` the record names.
    terminating: &'static str,
    /// What `ruleGroupList` must be, given that same ARN.
    rule_group_list: fn(&str) -> Value,
}

/// The rows: every way a rule group can act on a request, a group that acts on none, and a
/// rule that lives in no group.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a rule of a group this account owns ends the evaluation",
            name: "aws-waf-logs-group-blocked",
            rules: |arn| group_reference(arn, json!({}), json!({ "None": {} })),
            probe: MATCHING_PROBE,
            action: "BLOCK",
            terminating: RUNS_THE_GROUP,
            rule_group_list: |arn| {
                json!([group_entry(
                    arn,
                    json!({ "ruleId": GROUP_RULE, "action": "BLOCK" }),
                    json!([]),
                    Value::Null,
                )])
            },
        },
        Row {
            case: "the same group with the web ACL rule's OverrideAction counting it whole",
            name: "aws-waf-logs-group-counted",
            rules: |arn| group_reference(arn, json!({}), json!({ "Count": {} })),
            probe: MATCHING_PROBE,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            rule_group_list: |arn| {
                json!([group_entry(
                    arn,
                    Value::Null,
                    json!([{ "ruleId": GROUP_RULE, "action": "COUNT" }]),
                    Value::Null,
                )])
            },
        },
        Row {
            case: "the group's rule the web ACL excluded by name",
            name: "aws-waf-logs-group-excluded",
            rules: |arn| {
                group_reference(
                    arn,
                    json!({ "ExcludedRules": [{ "Name": GROUP_RULE }] }),
                    json!({ "None": {} }),
                )
            },
            probe: MATCHING_PROBE,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            rule_group_list: |arn| {
                json!([group_entry(
                    arn,
                    Value::Null,
                    json!([]),
                    json!([{ "exclusionType": "EXCLUDED_AS_COUNT", "ruleId": GROUP_RULE }]),
                )])
            },
        },
        Row {
            case: "the group's rule the web ACL overrode to count",
            name: "aws-waf-logs-group-overridden",
            rules: |arn| {
                group_reference(
                    arn,
                    json!({
                        "RuleActionOverrides": [{
                            "Name": GROUP_RULE,
                            "ActionToUse": { "Count": {} },
                        }]
                    }),
                    json!({ "None": {} }),
                )
            },
            probe: MATCHING_PROBE,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            rule_group_list: |arn| {
                json!([group_entry(
                    arn,
                    Value::Null,
                    json!([{ "ruleId": GROUP_RULE, "action": "COUNT" }]),
                    Value::Null,
                )])
            },
        },
        Row {
            case: "a group none of whose rules match the request, which the record lists \
                   with nothing under it",
            name: "aws-waf-logs-group-untouched",
            rules: |arn| group_reference(arn, json!({}), json!({ "None": {} })),
            probe: UNMATCHED_PROBE,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            rule_group_list: |arn| {
                json!([group_entry(arn, Value::Null, json!([]), Value::Null)])
            },
        },
        Row {
            case: "a rule of a VENDOR's managed group ends the evaluation, and that group \
                   is named by its vendor and its own name",
            name: "aws-waf-logs-managed-blocked",
            rules: managed_reference,
            probe: UNMATCHED_PROBE,
            action: "BLOCK",
            terminating: RUNS_THE_MANAGED_GROUP,
            rule_group_list: |_| {
                json!([group_entry(
                    &format!("{VENDOR}#{MANAGED_GROUP}"),
                    json!({ "ruleId": MANAGED_RULE, "action": "BLOCK" }),
                    json!([]),
                    Value::Null,
                )])
            },
        },
        Row {
            case: "a managed group the request never reached, because its scope-down \
                   statement does not match",
            name: "aws-waf-logs-managed-scoped-down",
            rules: managed_scoped_down,
            probe: UNMATCHED_PROBE,
            action: "ALLOW",
            terminating: DEFAULT_ACTION_RULE,
            rule_group_list: |_| json!([]),
        },
        Row {
            case: "a rule of the web ACL itself, which lives in no group",
            name: "aws-waf-logs-no-group",
            rules: own_rule,
            probe: MATCHING_PROBE,
            action: "BLOCK",
            terminating: OWN_RULE,
            rule_group_list: |_| json!([]),
        },
    ]
}

/// Every record a web ACL leaves names the rule groups it ran the request against, the
/// rule of each group that decided or counted it, and the group's rules the web ACL left
/// counting — and names no group for a request no group was run against.
#[tokio::test]
async fn a_record_names_the_rule_groups_that_acted_and_what_their_rules_did() {
    for row in rows() {
        let case = row.case;
        let emulator = Emulator::new();
        // The managed row's expectation names a rule by hand: it has to be one the
        // service's own declaration of that group carries.
        if row.name.contains("managed") {
            let declared = declared_managed_rules(&emulator, MANAGED_GROUP).await;
            assert!(
                declared.iter().any(|name| name == MANAGED_RULE),
                "case {case:?}: expected {MANAGED_GROUP} to declare the rule \
                 {MANAGED_RULE:?}, got {declared:?}"
            );
        }
        emulator.given_log_group(row.name).await;
        let destination = format!(
            "arn:aws:logs:{REGION}:{}:log-group:{}",
            engine::emulator_account_id(),
            row.name
        );
        let group_arn = blocking_rule_group(&emulator, row.name).await;
        let (pool_arn, client_id) = pool_with_a_client(&emulator, row.name).await;
        let web_acl = web_acl_in_front(
            &emulator,
            row.name,
            (row.rules)(&group_arn),
            &pool_arn,
            &destination,
        )
        .await;

        let status = one_inspected_request(&emulator, &client_id, row.probe).await;
        let refused = row.action != "ALLOW";
        assert_eq!(
            status == FORBIDDEN_STATUS,
            refused,
            "case {case:?}: expected the request to be refused={refused} by the web ACL, \
             got HTTP {status}"
        );

        let delivered = emulator.in_log_group(row.name).await;
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: one inspected request is one record, got {delivered:?}"
        );
        let record = &delivered[0];
        assert_eq!(
            record["webaclId"],
            json!(web_acl),
            "case {case:?}: the record must name the web ACL that decided, got {record}"
        );
        assert_eq!(
            record["action"],
            json!(row.action),
            "case {case:?}: expected the action {:?}, got {record}",
            row.action
        );
        assert_eq!(
            record["terminatingRuleId"],
            json!(row.terminating),
            "case {case:?}: expected the terminating rule {:?}, got {record}",
            row.terminating
        );
        let expected = (row.rule_group_list)(&group_arn);
        assert_eq!(
            record["ruleGroupList"], expected,
            "case {case:?}: expected the rule groups the web ACL ran {expected}, got \
             {record}"
        );
        // And the resource the request arrived at, as the service that took it identifies
        // that resource: the account that holds the pool and the pool's own id, which is
        // what its ARN states after the resource type.
        let expected_source = pool_arn
            .rsplit_once('/')
            .map(|(_, pool_id)| {
                format!("{}-{pool_id}", engine::emulator_account_id())
            })
            .unwrap_or_default();
        assert_eq!(
            record["httpSourceId"],
            json!(expected_source),
            "case {case:?}: expected the source id {expected_source:?}, got {record}"
        );
    }
}
