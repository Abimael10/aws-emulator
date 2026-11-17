// SPDX-License-Identifier: CC0-1.0

//! WHICH layer refuses a value outside a modelled enum, and what that refusal SAYS.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the
//! router's `dispatch`, using the harness `tests/all/job_flow_lifecycle.rs` owns, so
//! these are the same bytes an SDK client sends.
//!
//! THE RULE every table below asserts: Amazon EMR checks the ENUM-valued members of
//! an operation's own input shape itself, ahead of the generic request validator, and
//! refuses a value outside such an enum with its own modelled client-input error
//! `InvalidRequestException` (HTTP 400) — where the operation's model declares that
//! error at all — naming, where the rejected values are elements of ONE
//! collection-valued filter member, those values in the request's own order. An enum
//! carried DEEPER, inside a nested request structure, is reached only by the generic
//! request validator, so it keeps that validator's `ValidationException`, worded in
//! EMR's own prose where EMR validates the structure the value sits in and in the
//! validator's member-path envelope otherwise. A refusal identity is therefore a
//! property of WHERE the offending value sits and of WHICH operation was served, not of
//! the service as a whole.
//!
//! The Studio/notebook surface is served by its OWN backend, which words every refusal it
//! raises its own way, so the rejected value of one of its enum members is stated there
//! rather than in the validator's envelope — `tests/all/editors_plane_enum_refusals.rs`
//! is that plane's own table, and the two rows here pin which plane each operation is on.
//!
//! The rows span what the rule generalises over: how deep the offending value sits
//! (a member of the input shape, an element of one of its collections, and two, three
//! and five structure steps down), whether the served operation's model declares EMR's
//! own client error, which enum and which operation, how many values the member carries
//! and which of them are outside the set, the order they were submitted in, and a
//! request that violates both classes at once. The admitted rows are the last part of
//! the rule: a value the model declares is never refused at the boundary, at any depth.

use serde_json::{json, Value};

use crate::input_refusals::error_message;
use crate::job_flow_lifecycle::{assert_refused, at, kept_alive_request, Emr};

/// The client-input error EMR's own model declares, which its own enum check answers on
/// an operation that declares it (`smithy_query(query="operation",
/// operation="ListClusters")` lists `InvalidRequestException`, HTTP 400).
const SELF_CHECKED: &str = "InvalidRequestException";

/// The generic request validator's code — what a caller sees for anything EMR does not
/// check itself, and for an operation whose model declares no client error at all.
/// Recorded for `PutAutoScalingPolicy`'s out-of-enum `ComparisonOperator` and for
/// `RunJobFlow`, which `smithy_query(query="digest")` shows declaring only
/// `InternalServerError`.
const GENERIC: &str = "ValidationException";

/// Every value `com.amazonaws.emr#ClusterState` declares
/// (`smithy_query(query="shape", shape="ClusterState")`).
const CLUSTER_STATES: [&str; 7] = [
    "STARTING",
    "BOOTSTRAPPING",
    "RUNNING",
    "WAITING",
    "TERMINATING",
    "TERMINATED",
    "TERMINATED_WITH_ERRORS",
];

/// Every value `com.amazonaws.emr#InstanceState` declares
/// (`smithy_query(query="shape", shape="InstanceState")`).
const INSTANCE_STATES: [&str; 5] = [
    "AWAITING_FULFILLMENT",
    "PROVISIONING",
    "BOOTSTRAPPING",
    "RUNNING",
    "TERMINATED",
];

/// Every value `com.amazonaws.emr#ComparisonOperator` declares
/// (`smithy_query(query="shape", shape="ComparisonOperator")`), reached five
/// structure steps down `AutoScalingPolicy`.
const COMPARISON_OPERATORS: [&str; 4] = [
    "GREATER_THAN_OR_EQUAL",
    "GREATER_THAN",
    "LESS_THAN",
    "LESS_THAN_OR_EQUAL",
];

/// What a refusal must say.
enum Says {
    /// EMR's own sentence, in full: the filter member's words and the rejected values.
    Exactly(String),
    /// The generic validator's envelope, which must name this member path.
    Names(&'static str),
    /// The Studio/notebook backend's sentence for the member, inside the service detail
    /// every refusal that backend raises carries.
    Editors(String),
}

/// One row: a request, the error code it must carry, and what its message must say.
struct Case {
    case: String,
    operation: &'static str,
    body: Value,
    code: &'static str,
    says: Says,
}

/// A refusal EMR's own enum check owns, whose message enumerates the rejected values
/// of one collection-valued filter member.
fn own_sentence(case: &str, operation: &'static str, body: Value, sentence: &str) -> Case {
    Case {
        case: case.to_string(),
        operation,
        body,
        code: SELF_CHECKED,
        says: Says::Exactly(sentence.to_string()),
    }
}

/// A refusal EMR's own enum check owns on a SINGLE-valued member: there is no set of
/// submitted values to enumerate, so the message stays the validator's envelope, which
/// names the member and the value it held.
fn own_envelope(case: &str, operation: &'static str, body: Value, path: &'static str) -> Case {
    Case {
        case: case.to_string(),
        operation,
        body,
        code: SELF_CHECKED,
        says: Says::Names(path),
    }
}

/// A refusal the Studio/notebook plane owns: EMR's own code, with the backend's sentence
/// for the member inside its service detail — `tests/all/editors_plane_enum_refusals.rs`
/// states that rule over the whole plane.
fn editors_sentence(case: &str, operation: &'static str, body: Value, sentence: &str) -> Case {
    Case {
        case: case.to_string(),
        operation,
        body,
        code: SELF_CHECKED,
        says: Says::Editors(sentence.to_string()),
    }
}

/// A refusal the GENERIC request validator owns: the offending enum sits inside a
/// nested request structure, which EMR's own check does not reach.
fn generic(case: &str, operation: &'static str, body: Value, path: &'static str) -> Case {
    Case {
        case: case.to_string(),
        operation,
        body,
        code: GENERIC,
        says: Says::Names(path),
    }
}

/// A refusal under the generic validator's code that EMR nevertheless words itself:
/// an undeclared value of one of a CloudWatch metric alarm's enum members, stated in
/// EMR's own prose for that member.
fn generic_sentence(case: &str, operation: &'static str, body: Value, sentence: &str) -> Case {
    Case {
        case: case.to_string(),
        operation,
        body,
        code: GENERIC,
        says: Says::Exactly(sentence.to_string()),
    }
}

/// One scaling rule of an `AutoScalingPolicy`, with each of its three nested enums
/// settable so a row can put the offending value at a different depth.
fn scaling_rule(name: &str, comparison: &str, statistic: &str, adjustment: &str) -> Value {
    json!({
        "Name": name,
        "Action": {
            "SimpleScalingPolicyConfiguration": {
                "AdjustmentType": adjustment,
                "ScalingAdjustment": 1,
                "CoolDown": 300,
            },
        },
        "Trigger": {
            "CloudWatchAlarmDefinition": {
                "ComparisonOperator": comparison,
                "EvaluationPeriods": 1,
                "MetricName": "YARNMemoryAvailablePercentage",
                "Namespace": "AWS/ElasticMapReduce",
                "Period": 300,
                "Threshold": 15.0,
                "Statistic": statistic,
                "Unit": "PERCENT",
            },
        },
    })
}

/// A `PutAutoScalingPolicy` body carrying `rules`. The cluster and group ids never
/// matter to these rows: a boundary refusal happens before the handler resolves them.
fn put_policy(cluster_id: &str, group_id: &str, rules: Value) -> Value {
    json!({
        "ClusterId": cluster_id,
        "InstanceGroupId": group_id,
        "AutoScalingPolicy": {
            "Constraints": { "MinCapacity": 1, "MaxCapacity": 2 },
            "Rules": rules,
        },
    })
}

/// A well-formed rule, used as the valid neighbour of an offending one.
fn good_rule(name: &str) -> Value {
    scaling_rule(name, COMPARISON_OPERATORS[1], "AVERAGE", "CHANGE_IN_CAPACITY")
}

/// Every row names a cluster that EXISTS, because a cluster-scoped request's cluster is
/// resolved BEFORE its other members are checked: an unresolvable id would be the
/// refusal, and these rows are about the enum value.
fn refused_cases(cluster: &str) -> Vec<Case> {
    let group = "ig-ABCDEFGHIJKLM";
    vec![
        // ---- EMR's own check, on a collection-valued filter member --------------
        own_sentence(
            "one ClusterStates entry outside ClusterState",
            "ListClusters",
            json!({ "ClusterStates": ["NOPE"] }),
            "Cluster states [NOPE] are not valid.",
        ),
        own_sentence(
            "a declared ClusterState followed by an undeclared one",
            "ListClusters",
            json!({ "ClusterStates": ["RUNNING", "NOPE"] }),
            "Cluster states [NOPE] are not valid.",
        ),
        own_sentence(
            "an undeclared ClusterState between two declared ones",
            "ListClusters",
            json!({ "ClusterStates": ["TERMINATED_WITH_ERRORS", "bogus", "STARTING"] }),
            "Cluster states [bogus] are not valid.",
        ),
        // Enum values are case-sensitive, so a declared value in lower case is
        // outside the set — and both rejected values are named, in the order the
        // request spelled them.
        own_sentence(
            "two undeclared ClusterStates, the second a declared value in lower case",
            "ListClusters",
            json!({ "ClusterStates": ["NOPE", "running"] }),
            "Cluster states [NOPE, running] are not valid.",
        ),
        own_sentence(
            "the same two undeclared ClusterStates in the other order",
            "ListClusters",
            json!({ "ClusterStates": ["running", "NOPE"] }),
            "Cluster states [running, NOPE] are not valid.",
        ),
        own_sentence(
            "every ClusterStates entry undeclared",
            "ListClusters",
            json!({ "ClusterStates": ["one", "two", "three"] }),
            "Cluster states [one, two, three] are not valid.",
        ),
        // The same rule on another operation's filter, so the sentence is built from
        // the member rather than written for ListClusters.
        own_sentence(
            "a StepStates entry outside StepState",
            "ListSteps",
            json!({ "ClusterId": cluster, "StepStates": ["NOT_A_STATE"] }),
            "Step states [NOT_A_STATE] are not valid.",
        ),
        own_sentence(
            "a SessionStates entry outside SessionState",
            "ListSessions",
            json!({ "ClusterId": cluster, "SessionStates": ["NOPE"] }),
            "Session states [NOPE] are not valid.",
        ),
        own_sentence(
            "an InstanceStates entry outside InstanceState",
            "ListInstances",
            json!({ "ClusterId": cluster, "InstanceStates": ["NOPE"] }),
            "Instance states [NOPE] are not valid.",
        ),
        // A filter member whose name is three words, so the sentence's wording is
        // derived from the member and not from a per-member table.
        own_sentence(
            "an InstanceGroupTypes entry outside InstanceGroupType",
            "ListInstances",
            json!({ "ClusterId": cluster, "InstanceGroupTypes": ["PRIMARY"] }),
            "Instance group types [PRIMARY] are not valid.",
        ),
        // A request violating the enum class AND another class at once is refused as
        // the enum class: that is the check EMR runs first.
        own_sentence(
            "an undeclared SessionStates entry beside a MaxResults out of range",
            "ListSessions",
            json!({
                "ClusterId": cluster,
                "SessionStates": ["NOPE"],
                "MaxResults": 101,
            }),
            "Session states [NOPE] are not valid.",
        ),
        // ---- EMR's own check, on a single-valued member of the input shape ------
        // The Studio/notebook plane's two filters: the backend fronting that plane words
        // the refusal itself, so neither the member path nor the enum's value set is what
        // the caller reads.
        editors_sentence(
            "a Status outside NotebookExecutionStatus",
            "ListNotebookExecutions",
            json!({ "Status": "NOT_A_REAL_STATUS" }),
            "Invalid Execution Status: NOT_A_REAL_STATUS",
        ),
        editors_sentence(
            "an IdentityType outside IdentityType",
            "ListStudioSessionMappings",
            json!({ "IdentityType": "PERSON" }),
            "IdentityType is not valid.",
        ),
        own_envelope(
            "an InstanceFleetType outside InstanceFleetType",
            "ListInstances",
            json!({ "ClusterId": cluster, "InstanceFleetType": "WORKER" }),
            "instanceFleetType",
        ),
        own_envelope(
            "a ProfilerType outside ProfilerType",
            "CreatePersistentAppUI",
            json!({
                "TargetResourceArn":
                    "arn:aws:elasticmapreduce:us-east-1:000000000000:cluster/j-ABCDEFGHIJKLM",
                "ProfilerType": "SPARKUI",
            }),
            "profilerType",
        ),
        // An undeclared value on a member of the input shape of an operation whose
        // model declares NO client error: `RunJobFlow` declares only
        // `InternalServerError` (HTTP 500), so EMR's own client-input error is an
        // identity it cannot answer and the generic one stands — and it stands on the
        // SHALLOWER violation, reported beside the one nested five steps down.
        generic(
            "an undeclared ScaleDownBehavior beside an undeclared nested Market",
            "RunJobFlow",
            json!({
                "Name": "both classes at once",
                "ReleaseLabel": "emr-6.15.0",
                "ScaleDownBehavior": "NOPE",
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceGroups": [{
                        "Name": "primary",
                        "InstanceRole": "MASTER",
                        "InstanceType": "m5.xlarge",
                        "InstanceCount": 1,
                        "Market": "CHEAP",
                    }],
                },
            }),
            "scaleDownBehavior",
        ),
        // A request whose input-member enum violations span TWO collection members has
        // no single member to word the sentence around, so the validator's envelope
        // stands — under EMR's own code, which the operation declares.
        own_envelope(
            "undeclared entries in two InstanceStates and InstanceGroupTypes at once",
            "ListInstances",
            json!({
                "ClusterId": cluster,
                "InstanceStates": ["NOPE"],
                "InstanceGroupTypes": ["PRIMARY"],
            }),
            "instanceStates",
        ),
        // ---- the generic validator's code, EMR's own prose ---------------------
        // The recorded case: five structure steps down PutAutoScalingPolicy's policy,
        // on the CloudWatch metric alarm EMR validates itself.
        generic_sentence(
            "an undeclared ComparisonOperator in the only scaling rule",
            "PutAutoScalingPolicy",
            put_policy(
                cluster,
                group,
                json!([scaling_rule(
                    "scale-out-bad-comparison-operator",
                    "NOT_A_REAL_COMPARISON_OPERATOR",
                    "AVERAGE",
                    "CHANGE_IN_CAPACITY",
                )]),
            ),
            "The supplied cloud watch comparison operator is invalid.",
        ),
        // The same depth reached through the SECOND rule, so the refusal is not keyed
        // to one position.
        generic_sentence(
            "an undeclared ComparisonOperator in the second scaling rule",
            "PutAutoScalingPolicy",
            put_policy(
                cluster,
                group,
                json!([
                    good_rule("scale-out"),
                    scaling_rule("scale-in", "NEARLY", "AVERAGE", "CHANGE_IN_CAPACITY"),
                ]),
            ),
            "The supplied cloud watch comparison operator is invalid.",
        ),
        // The alarm's other two enum members: the sentence names the member EMR
        // rejected, so it is built from the member rather than written for one of
        // them.
        generic_sentence(
            "an undeclared Statistic in a scaling rule",
            "PutAutoScalingPolicy",
            put_policy(
                cluster,
                group,
                json!([scaling_rule(
                    "scale-out",
                    COMPARISON_OPERATORS[0],
                    "MEDIAN",
                    "CHANGE_IN_CAPACITY",
                )]),
            ),
            "The supplied cloud watch statistic is invalid.",
        ),
        generic_sentence(
            "an undeclared Unit in a scaling rule",
            "PutAutoScalingPolicy",
            put_policy(
                cluster,
                group,
                json!([{
                    "Name": "scale-out",
                    "Action": {
                        "SimpleScalingPolicyConfiguration": {
                            "AdjustmentType": "CHANGE_IN_CAPACITY",
                            "ScalingAdjustment": 1,
                        },
                    },
                    "Trigger": {
                        "CloudWatchAlarmDefinition": {
                            "ComparisonOperator": COMPARISON_OPERATORS[1],
                            "MetricName": "YARNMemoryAvailablePercentage",
                            "Period": 300,
                            "Threshold": 15.0,
                            "Unit": "FURLONGS",
                        },
                    },
                }]),
            ),
            "The supplied cloud watch unit is invalid.",
        ),
        // ---- the generic validator, on an enum inside any other nested structure
        // The same depth on the OTHER branch of a scaling rule, which is no CloudWatch
        // alarm: nothing words it but the validator's envelope.
        generic(
            "an undeclared AdjustmentType in a scaling rule",
            "PutAutoScalingPolicy",
            put_policy(
                cluster,
                group,
                json!([scaling_rule(
                    "scale-out",
                    COMPARISON_OPERATORS[3],
                    "AVERAGE",
                    "CHANGE_IN_HEADCOUNT",
                )]),
            ),
            "autoScalingPolicy.rules.1.member.action.simpleScalingPolicyConfiguration.\
             adjustmentType",
        ),
        // Two structure steps down, through a list.
        generic(
            "an undeclared ActionOnFailure on a submitted step",
            "AddJobFlowSteps",
            json!({
                "JobFlowId": cluster,
                "Steps": [{
                    "Name": "a step",
                    "ActionOnFailure": "GIVE_UP",
                    "HadoopJarStep": { "Jar": "command-runner.jar" },
                }],
            }),
            "steps.1.member.actionOnFailure",
        ),
        // Two structure steps down, through a structure.
        generic(
            "an undeclared InstanceFleetType on a submitted fleet",
            "AddInstanceFleet",
            json!({
                "ClusterId": cluster,
                "InstanceFleet": {
                    "InstanceFleetType": "WORKER",
                    "TargetOnDemandCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
                },
            }),
            "instanceFleet.instanceFleetType",
        ),
        // Three structure steps down.
        generic(
            "an undeclared Market on a launched instance group",
            "RunJobFlow",
            json!({
                "Name": "an undeclared market",
                "ReleaseLabel": "emr-6.15.0",
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceGroups": [{
                        "Name": "primary",
                        "InstanceRole": "MASTER",
                        "InstanceType": "m5.xlarge",
                        "InstanceCount": 1,
                        "Market": "CHEAP",
                    }],
                },
            }),
            "instances.instanceGroups.1.member.market",
        ),
    ]
}

/// THE RULE, on everything a caller gets wrong: the code and the message of an
/// out-of-enum refusal are the ones belonging to the layer that reaches the offending
/// value.
#[tokio::test]
async fn an_out_of_enum_value_is_refused_by_the_layer_that_reaches_it() {
    let emr = Emr::new();
    let cluster = emr.run_job_flow(kept_alive_request("filtered")).await;
    for case in refused_cases(&cluster) {
        let label = &case.case;
        let (status, payload) = emr.call(case.operation, case.body.clone()).await;
        assert_eq!(
            status, 400,
            "case {label:?}: expected HTTP 400, got {status} with {payload}"
        );
        let code = at(&payload, &["__type"]).as_str().unwrap_or_default().to_string();
        assert!(
            code.contains(case.code),
            "case {label:?}: expected error code {}, got {code:?} in {payload}",
            case.code
        );
        let message = error_message(&payload);
        match &case.says {
            Says::Exactly(sentence) => assert_eq!(
                &message, sentence,
                "case {label:?}: expected {sentence:?}, got {message:?}"
            ),
            Says::Names(path) => assert!(
                message.contains(path),
                "case {label:?}: the refusal must name the member {path:?}, got {message:?}"
            ),
            Says::Editors(sentence) => {
                let request_id = crate::refusal_messages::wrapped_request_id(label, &message);
                let expected = format!(
                    "{sentence} (Service: AWSEditors; Status Code: 400; Error Code: \
                     InvalidRequestException; Request ID: {request_id}; Proxy: null)"
                );
                assert_eq!(
                    message, expected,
                    "case {label:?}: expected {expected:?}, got {message:?}"
                );
            }
        }
    }
}

/// THE RULE's other half: a value the model declares is never refused at the
/// boundary, whatever depth it sits at — it reaches the handler, which answers for
/// itself. Naming the status per row is what keeps a re-scaffolded operation from
/// passing here silently: `ListInstances` and `ListSessions` are implemented, so the
/// declared values reach their handlers, which then refuse the CLUSTER this suite names
/// and holds none of.
#[tokio::test]
async fn a_declared_enum_value_reaches_the_handler_at_every_depth() {
    let emr = Emr::new();
    let cluster = "j-ABCDEFGHIJKLM";
    let rows: Vec<(&str, &'static str, Value, u16)> = vec![
        (
            "one declared ClusterState",
            "ListClusters",
            json!({ "ClusterStates": ["RUNNING"] }),
            200,
        ),
        (
            "every declared ClusterState at once",
            "ListClusters",
            json!({ "ClusterStates": CLUSTER_STATES }),
            200,
        ),
        ("no filter at all", "ListClusters", json!({}), 200),
        (
            "a declared NotebookExecutionStatus",
            "ListNotebookExecutions",
            json!({ "Status": "FINISHED" }),
            200,
        ),
        (
            "a declared IdentityType",
            "ListStudioSessionMappings",
            json!({ "IdentityType": "USER" }),
            200,
        ),
        (
            "every declared InstanceState at once",
            "ListInstances",
            json!({ "ClusterId": cluster, "InstanceStates": INSTANCE_STATES }),
            400,
        ),
        (
            // `ListSessions` is implemented: the declared values reach the handler, which
            // then refuses the CLUSTER this suite names and holds none of.
            "declared SessionStates",
            "ListSessions",
            json!({ "ClusterId": cluster, "SessionStates": ["IDLE", "BUSY"] }),
            400,
        ),
    ];
    for (label, operation, body, expected) in rows {
        let (status, payload) = emr.call(operation, body).await;
        assert_eq!(
            status, expected,
            "case {label:?}: a value the model declares must reach the handler and be \
             answered {expected}, got {status} with {payload}"
        );
    }
}

/// THE RULE at the far end of the nesting, proved end to end: each declared
/// `ComparisonOperator` passes the boundary, is PERSISTED on the instance group the
/// request names, and reads back through `ListInstanceGroups` — so the narrowed
/// refusal path did not start refusing what the model admits.
#[tokio::test]
async fn every_declared_comparison_operator_is_admitted_and_persisted() {
    let emr = Emr::new();
    let cluster = emr
        .run_job_flow(json!({
            "Name": "nested enum round trip",
            "ReleaseLabel": "emr-6.15.0",
            "JobFlowRole": crate::job_flow_lifecycle::INSTANCE_PROFILE, "ServiceRole": crate::job_flow_lifecycle::SERVICE_ROLE,
            "Instances": {
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    { "Name": "primary", "InstanceRole": "MASTER",
                      "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                    { "Name": "core", "InstanceRole": "CORE",
                      "InstanceType": "m5.xlarge", "InstanceCount": 2 },
                ],
            },
        }))
        .await;
    let groups = emr
        .ok("ListInstanceGroups", json!({ "ClusterId": &cluster }))
        .await;
    let core = groups["InstanceGroups"]
        .as_array()
        .expect("a group listing")
        .iter()
        .find(|group| group["InstanceGroupType"] == json!("CORE"))
        .map(|group| group["Id"].as_str().expect("a group id").to_string())
        .expect("the CORE instance group");

    for comparison in COMPARISON_OPERATORS {
        let label = format!("ComparisonOperator {comparison}");
        let (status, payload) = emr
            .call(
                "PutAutoScalingPolicy",
                put_policy(
                    &cluster,
                    &core,
                    json!([scaling_rule(
                        "scale-out",
                        comparison,
                        "AVERAGE",
                        "CHANGE_IN_CAPACITY"
                    )]),
                ),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {label:?}: a declared ComparisonOperator must be accepted, got {status} \
             with {payload}"
        );

        let groups = emr
            .ok("ListInstanceGroups", json!({ "ClusterId": &cluster }))
            .await;
        let stored = groups["InstanceGroups"]
            .as_array()
            .expect("a group listing")
            .iter()
            .find(|group| group["Id"] == json!(&core))
            .cloned()
            .expect("the CORE instance group");
        let read_back = at(
            &stored,
            &[
                "AutoScalingPolicy",
                "Rules",
            ],
        )
        .as_array()
        .and_then(|rules| rules.first().cloned())
        .unwrap_or(Value::Null);
        assert_eq!(
            at(
                &read_back,
                &["Trigger", "CloudWatchAlarmDefinition", "ComparisonOperator"]
            ),
            &json!(comparison),
            "case {label:?}: the persisted policy must carry the submitted operator, got \
             {stored}"
        );
    }
}

// ---------------------------------------------------------------------------
// The ORDER a constraint clause lists an enum's admissible value set in
// ---------------------------------------------------------------------------

/// One enum, as the MODEL declares its values and as this service LISTS them in a
/// constraint clause.
///
/// `declared` is read off the model (`smithy_query(query="shape", shape="<Enum>")`, cited
/// per row); `listed` is the order a refusal states that set in, which is the walk of the
/// front door's own table of those values and so neither the declaration order nor a sort
/// of it. The recorded instance behind the rule is one row — `PlacementGroupStrategy`,
/// declared `[SPREAD, PARTITION, CLUSTER, NONE]` and stated
/// `[PARTITION, SPREAD, NONE, CLUSTER]`.
struct ValueSet {
    declared: &'static [&'static str],
    listed: &'static [&'static str],
}

/// `com.amazonaws.emr#PlacementGroupStrategy` (`smithy_query(query="shape",
/// shape="PlacementGroupStrategy")`): the recorded row.
const PLACEMENT_GROUP_STRATEGY: ValueSet = ValueSet {
    declared: &["SPREAD", "PARTITION", "CLUSTER", "NONE"],
    listed: &["PARTITION", "SPREAD", "NONE", "CLUSTER"],
};

/// `com.amazonaws.emr#MarketType` (`smithy_query(query="shape", shape="MarketType")`).
const MARKET_TYPE: ValueSet = ValueSet {
    declared: &["ON_DEMAND", "SPOT"],
    listed: &["SPOT", "ON_DEMAND"],
};

/// `com.amazonaws.emr#ActionOnFailure` (`smithy_query(query="shape",
/// shape="ActionOnFailure")`).
const ACTION_ON_FAILURE: ValueSet = ValueSet {
    declared: &[
        "TERMINATE_JOB_FLOW",
        "TERMINATE_CLUSTER",
        "CANCEL_AND_WAIT",
        "CONTINUE",
    ],
    listed: &[
        "CANCEL_AND_WAIT",
        "CONTINUE",
        "TERMINATE_JOB_FLOW",
        "TERMINATE_CLUSTER",
    ],
};

/// `com.amazonaws.emr#ProfilerType` (`smithy_query(query="shape", shape="ProfilerType")`).
const PROFILER_TYPE: ValueSet = ValueSet {
    declared: &["SHS", "TEZUI", "YTS"],
    listed: &["YTS", "TEZUI", "SHS"],
};

/// `com.amazonaws.emr#InstanceFleetType` (`smithy_query(query="shape",
/// shape="InstanceFleetType")`).
const INSTANCE_FLEET_TYPE: ValueSet = ValueSet {
    declared: &["MASTER", "CORE", "TASK"],
    listed: &["MASTER", "TASK", "CORE"],
};

/// `com.amazonaws.emr#ScaleDownBehavior` (`smithy_query(query="shape",
/// shape="ScaleDownBehavior")`): the control on the rule — a set whose stated order and
/// declaration order AGREE, so the order is shown to be computed from the values rather
/// than a blanket reversal or sort of whatever the model declared.
const SCALE_DOWN_BEHAVIOR: ValueSet = ValueSet {
    declared: &["TERMINATE_AT_INSTANCE_HOUR", "TERMINATE_AT_TASK_COMPLETION"],
    listed: &["TERMINATE_AT_INSTANCE_HOUR", "TERMINATE_AT_TASK_COMPLETION"],
};

/// The whole message a refusal of ONE value outside an enum states: the framework's
/// counted envelope and clause, over the value set as `set` says this service lists it.
fn one_enum_error(submitted: &str, path: &str, set: &ValueSet) -> String {
    format!(
        "1 validation error detected: Value '{submitted}' at '{path}' failed to satisfy \
         constraint: Member must satisfy enum value set: [{}]",
        set.listed.join(", ")
    )
}

/// A `RunJobFlow` body carrying the members that operation requires, plus `extra`.
fn launch(name: &str, extra: Value) -> Value {
    let mut body = json!({
        "Name": name,
        "ReleaseLabel": "emr-6.15.0",
        "Instances": { "InstanceCount": 1, "MasterInstanceType": "m5.xlarge" },
    });
    if let Some(members) = extra.as_object() {
        for (key, value) in members {
            body[key] = value.clone();
        }
    }
    body
}

/// THE RULE: a refusal of a value outside a modelled enum lists that enum's admissible
/// values in the order this service's front door walks its own table of them — ascending
/// bucket of each value's string hash — and NOT in the order the model declares them, so
/// one enum is listed the same way whichever member carried the value, whichever operation
/// was served, and however deep the value sat. Everything else about the clause stays the
/// framework's: the counted envelope, the echoed value, the member path and the
/// requirement's wording.
///
/// Rows span what the rule generalises over: six enums (two, three and four values), the
/// two error identities EMR answers a boundary refusal under, a value on the operation's
/// own input shape and values two, three and four steps down, two positions of the same
/// list, and — as the control — a set whose stated order the declaration order agrees
/// with.
#[tokio::test]
async fn an_enum_value_set_is_listed_in_the_order_this_service_states_it() {
    let emr = Emr::new();
    // `AddInstanceFleet` resolves its cluster BEFORE the request's other members, so the
    // row below names a cluster this emulator holds; the rest resolve nothing.
    let cluster = emr
        .run_job_flow(json!({
            "Name": "stated enum value sets",
            "ReleaseLabel": "emr-6.15.0",
            "JobFlowRole": crate::job_flow_lifecycle::INSTANCE_PROFILE,
            "ServiceRole": crate::job_flow_lifecycle::SERVICE_ROLE,
            "Instances": {
                "KeepJobFlowAliveWhenNoSteps": true,
                "InstanceGroups": [
                    { "Name": "primary", "InstanceRole": "MASTER",
                      "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                ],
            },
        }))
        .await;

    let rows: Vec<(&str, &str, Value, &str, &str, &str, &ValueSet)> = vec![
        // The recorded case: the strategy of the FIRST placement group configuration.
        (
            "RunJobFlow with a lower-cased PlacementStrategy",
            "RunJobFlow",
            launch(
                "placement strategy out of its set",
                json!({ "PlacementGroupConfigs": [
                    { "InstanceRole": "MASTER", "PlacementStrategy": "spread" },
                ] }),
            ),
            GENERIC,
            "spread",
            "placementGroupConfigs.1.member.placementStrategy",
            &PLACEMENT_GROUP_STRATEGY,
        ),
        // The same enum reached through the SECOND element, so neither the set nor the
        // clause is keyed to one position.
        (
            "RunJobFlow with the second PlacementStrategy mis-cased",
            "RunJobFlow",
            launch(
                "second placement strategy out of its set",
                json!({ "PlacementGroupConfigs": [
                    { "InstanceRole": "MASTER", "PlacementStrategy": "SPREAD" },
                    { "InstanceRole": "CORE", "PlacementStrategy": "Cluster" },
                ] }),
            ),
            GENERIC,
            "Cluster",
            "placementGroupConfigs.2.member.placementStrategy",
            &PLACEMENT_GROUP_STRATEGY,
        ),
        // A member of the operation's OWN input shape, on an operation that cannot answer
        // EMR's client error: the control on the rule's order half.
        (
            "RunJobFlow with an undeclared ScaleDownBehavior",
            "RunJobFlow",
            launch(
                "scale down behavior out of its set",
                json!({ "ScaleDownBehavior": "TERMINATE_WHENEVER" }),
            ),
            GENERIC,
            "TERMINATE_WHENEVER",
            "scaleDownBehavior",
            &SCALE_DOWN_BEHAVIOR,
        ),
        // Four steps down, through a list inside a nested structure.
        (
            "RunJobFlow with an undeclared Market on a launched group",
            "RunJobFlow",
            json!({
                "Name": "market out of its set",
                "ReleaseLabel": "emr-6.15.0",
                "Instances": {
                    "KeepJobFlowAliveWhenNoSteps": true,
                    "InstanceGroups": [{
                        "Name": "primary",
                        "InstanceRole": "MASTER",
                        "InstanceType": "m5.xlarge",
                        "InstanceCount": 1,
                        "Market": "CHEAP",
                    }],
                },
            }),
            GENERIC,
            "CHEAP",
            "instances.instanceGroups.1.member.market",
            &MARKET_TYPE,
        ),
        // Two steps down, through a list.
        (
            "AddJobFlowSteps with an undeclared ActionOnFailure",
            "AddJobFlowSteps",
            json!({
                "JobFlowId": "j-ABCDEFGHIJKLM",
                "Steps": [{
                    "Name": "a step",
                    "ActionOnFailure": "GIVE_UP",
                    "HadoopJarStep": { "Jar": "command-runner.jar" },
                }],
            }),
            GENERIC,
            "GIVE_UP",
            "steps.1.member.actionOnFailure",
            &ACTION_ON_FAILURE,
        ),
        // Two steps down, through a structure, on a cluster-scoped operation.
        (
            "AddInstanceFleet with an undeclared InstanceFleetType",
            "AddInstanceFleet",
            json!({
                "ClusterId": &cluster,
                "InstanceFleet": {
                    "InstanceFleetType": "WORKER",
                    "TargetOnDemandCapacity": 1,
                    "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
                },
            }),
            GENERIC,
            "WORKER",
            "instanceFleet.instanceFleetType",
            &INSTANCE_FLEET_TYPE,
        ),
        // A member of the operation's own input shape, answered under EMR's OWN client
        // error: one order, whichever identity carries the clause.
        (
            "CreatePersistentAppUI with an undeclared ProfilerType",
            "CreatePersistentAppUI",
            json!({
                "TargetResourceArn":
                    "arn:aws:elasticmapreduce:us-east-1:000000000000:cluster/j-ABCDEFGHIJKLM",
                "ProfilerType": "SPARKUI",
            }),
            SELF_CHECKED,
            "SPARKUI",
            "profilerType",
            &PROFILER_TYPE,
        ),
    ];

    for (case, operation, body, code, submitted, path, set) in rows {
        let (status, payload) = emr.call(operation, body).await;
        assert_refused(case, status, &payload, code);
        let expected = one_enum_error(submitted, path, set);
        let said = error_message(&payload);
        assert_eq!(
            said, expected,
            "case {case:?}: expected {expected:?}, got {said:?}"
        );
        // The values the model declares are exactly the values stated, so the order is
        // all that moved: nothing is added to the set and nothing is dropped from it.
        let mut declared: Vec<&str> = set.declared.to_vec();
        let mut listed: Vec<&str> = set.listed.to_vec();
        declared.sort_unstable();
        listed.sort_unstable();
        assert_eq!(
            listed, declared,
            "case {case:?}: the stated set must hold exactly the declared values, got \
             {listed:?} against {declared:?}"
        );
    }
}

/// THE RULE's teeth: the recorded enum is NOT listed in the order its model declares it,
/// and the declaration order never reaches the wire.
#[tokio::test]
async fn the_recorded_enum_value_set_is_not_listed_in_the_models_order() {
    let emr = Emr::new();
    let (status, payload) = emr
        .call(
            "RunJobFlow",
            launch(
                "placement strategy out of its set",
                json!({ "PlacementGroupConfigs": [
                    { "InstanceRole": "MASTER", "PlacementStrategy": "spread" },
                ] }),
            ),
        )
        .await;
    assert_refused("the recorded placement strategy", status, &payload, GENERIC);
    let said = error_message(&payload);
    let declared = format!("[{}]", PLACEMENT_GROUP_STRATEGY.declared.join(", "));
    assert!(
        !said.contains(&declared),
        "case {:?}: the declaration order must not reach the wire, got {said:?}",
        "the recorded placement strategy"
    );
    let listed = format!("[{}]", PLACEMENT_GROUP_STRATEGY.listed.join(", "));
    assert!(
        said.ends_with(&listed),
        "case {:?}: the clause must end with {listed:?}, got {said:?}",
        "the recorded placement strategy"
    );
}
