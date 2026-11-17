// SPDX-License-Identifier: CC0-1.0

//! How `DescribeAlarms` refuses a RELATIONSHIP request that also carries a selector.
//!
//! "If you specify `ChildrenOfAlarmName`, you cannot specify any other parameters in the
//! request except for `MaxRecords` and `NextToken`. If you do so, you receive a validation
//! error." (`smithy_query(query="doc",shape="DescribeAlarmsInput",member="ChildrenOfAlarmName")`,
//! and the same sentence on `ParentsOfAlarmName`.)
//!
//! The refusal's TEXT is part of its contract — `ValidationError` answers dozens of unrelated
//! CloudWatch refusals, so the message is the only thing that tells one from another — and it
//! enumerates the whole set of selectors such a request EXCLUDES, in one alphabetical list,
//! rather than describing the two parameters it still allows. The list is fixed: it is the set
//! the rule excludes, not the subset the refused request happened to carry, so a caller is told
//! every parameter it has to drop.
//!
//! Each rule is asserted over a table whose rows differ along what it generalises over: which
//! relationship member was used, which selector was carried beside it, and how many were
//! carried — plus the paging members, which the rule explicitly admits, and the relationship
//! members on their own, which are served.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{build_router, call, error_code, error_message, metric_alarm_request, ok};

/// The error class a refused request carries, as the body of the answer names it: the
/// request-validation class, whose legacy query code (`ValidationError`) travels in the
/// `x-amzn-query-error` header instead.
const REFUSAL_CODE: &str = "ValidationException";

/// The two members that state a composite-alarm relationship
/// (`smithy_query(query="operation",operation="DescribeAlarms")`).
const RELATIONSHIP_MEMBERS: [&str; 2] = ["ChildrenOfAlarmName", "ParentsOfAlarmName"];

/// The whole excluded-selector list every such refusal names, in the order it names them: every
/// input member of this read other than the two relationship members and the two paging members
/// `MaxRecords`/`NextToken` the rule admits
/// (`smithy_query(query="operation",operation="DescribeAlarms")`).
const EXCLUDED: &str = "ActionPrefix, AlarmNamePrefix, AlarmNames, AlarmTypes, StateValue";

/// A composite alarm named `parent` over a metric alarm named `child`, so a relationship
/// request has a real relationship to walk.
async fn given_a_parent_and_child(router: &MultiServiceRouter, parent: &str, child: &str) {
    ok(router, "PutMetricAlarm", &metric_alarm_request(child)).await;
    ok(
        router,
        "PutCompositeAlarm",
        &json!({"AlarmName": parent, "AlarmRule": format!("ALARM(\"{child}\")")}),
    )
    .await;
}

#[tokio::test]
async fn a_relationship_request_carrying_a_selector_is_refused_naming_every_excluded_parameter() {
    // (case, the selector the request carries beside the relationship member)
    //
    // Rows differ along WHICH selector is carried — each of the five the rule excludes, a
    // selector carrying a list rather than a scalar, two at once, and all five at once — and
    // the whole table is run for each of the two relationship members, so the message's only
    // request-derived part is exercised both ways.
    let selectors: [(&str, Value); 8] = [
        ("an alarm type", json!({"AlarmTypes": ["MetricAlarm"]})),
        ("a name prefix", json!({"AlarmNamePrefix": "sim2real-"})),
        ("a list of names", json!({"AlarmNames": ["anything"]})),
        ("a state value", json!({"StateValue": "ALARM"})),
        ("an action prefix", json!({"ActionPrefix": "arn:aws:sns:"})),
        (
            "a name prefix and a state value",
            json!({"AlarmNamePrefix": "sim2real-", "StateValue": "OK"}),
        ),
        (
            "every excluded selector at once",
            json!({
                "ActionPrefix": "arn:aws:sns:",
                "AlarmNamePrefix": "sim2real-",
                "AlarmNames": ["anything"],
                "AlarmTypes": ["CompositeAlarm"],
                "StateValue": "INSUFFICIENT_DATA",
            }),
        ),
        (
            "a selector beside the paging members the rule admits",
            json!({"AlarmTypes": ["MetricAlarm"], "MaxRecords": 10}),
        ),
    ];

    let router = build_router();
    given_a_parent_and_child(&router, "exclusivity-parent", "exclusivity-child").await;

    for member in RELATIONSHIP_MEMBERS {
        // Each relationship member names the alarm whose relationships it walks: the composite
        // alarm's children, or the metric alarm's parents.
        let named = if member == "ChildrenOfAlarmName" {
            "exclusivity-parent"
        } else {
            "exclusivity-child"
        };
        let expected =
            format!("{member} requests cannot be used with the following parameters: {EXCLUDED}");
        for (case, selector) in &selectors {
            let case = format!("{member} with {case}");
            let mut request = json!({member: named});
            for (name, value) in selector.as_object().expect("the row's selectors") {
                request[name.as_str()] = value.clone();
            }
            let (status, body) = call(&router, "DescribeAlarms", &request).await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected the request to be refused with HTTP 400, got {status} \
                 with body {body}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, REFUSAL_CODE,
                "case {case:?}: expected the error code {REFUSAL_CODE}, got {code:?} in {body}"
            );
            let message = error_message(&body);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal {expected:?}, got {message:?}"
            );
        }
    }
}

#[tokio::test]
async fn a_relationship_request_carrying_only_the_paging_members_is_served() {
    // The rule admits `MaxRecords` and `NextToken`, so the same relationship request carrying
    // either of them — or neither — is answered rather than refused, and answers with the
    // relationship it was asked for.
    //
    // (case, the paging members the request carries, the alarm the answer must report)
    let rows: [(&str, Value); 3] = [
        ("no paging member", json!({})),
        ("a page size", json!({"MaxRecords": 5})),
        (
            "a page size and a token",
            json!({"MaxRecords": 5, "NextToken": ""}),
        ),
    ];

    let router = build_router();
    given_a_parent_and_child(&router, "paging-parent", "paging-child").await;

    for (case, paging) in rows {
        for (member, named, reported, member_of_answer) in [
            (
                "ChildrenOfAlarmName",
                "paging-parent",
                "paging-child",
                "MetricAlarms",
            ),
            (
                "ParentsOfAlarmName",
                "paging-child",
                "paging-parent",
                "CompositeAlarms",
            ),
        ] {
            let case = format!("{member} with {case}");
            let mut request = json!({member: named});
            for (name, value) in paging.as_object().expect("the row's paging members") {
                request[name.as_str()] = value.clone();
            }
            let (status, body) = call(&router, "DescribeAlarms", &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the relationship request to be served with HTTP 200, \
                 got {status} with body {body}"
            );
            let names: Vec<&str> = body
                .get(member_of_answer)
                .and_then(Value::as_array)
                .map(|alarms| {
                    alarms
                        .iter()
                        .filter_map(|alarm| alarm.get("AlarmName").and_then(Value::as_str))
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                names,
                vec![reported],
                "case {case:?}: expected the answer to report {reported:?} under \
                 {member_of_answer}, got {body}"
            );
        }
    }
}
