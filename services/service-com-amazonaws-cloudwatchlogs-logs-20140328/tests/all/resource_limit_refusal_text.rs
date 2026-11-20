// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the TEXT a request over a RESOURCE limit is refused with.
//!
//! RULE under test: a request that would take the account, or one log group, past the
//! resources it may hold at one time is refused with `LimitExceededException` carrying that
//! error class's own fixed sentence — it names neither the limit that was reached, nor the
//! resource whose slots are full, nor anything from the request — so every refusal of that
//! class reads identically.
//!
//! Each case drives the service the way a client does — `POST /` with
//! `X-Amz-Target: Logs_20140328.<Operation>` through the router that mounts it — so a
//! sentence a handler does not actually answer cannot pass.
//!
//! The table spans what the rule generalises over: WHICH resource runs out (an
//! account-scoped resource policy, the account's single integration, the metric filters one
//! log group can hold), whether the limit belongs to the account or to one log group, and
//! whether it is reached at one resource or at a hundred. The resource policy a report
//! happens to cite is one row of it.

use serde_json::{json, Value};

use crate::harness::Logs;

/// The code CloudWatch Logs answers when a request would take a resource past what it may
/// hold.
const LIMIT_EXCEEDED: &str = "LimitExceededException";

/// The whole sentence of that class, as AWS words it. Stated here independently of the crate
/// under test.
const LIMIT_TEXT: &str = "Resource limit exceeded.";

/// AWS's documented resource-policy quota, from `PutResourcePolicy`: "An account can have a
/// maximum of 10 policies without resourceARN and one per LogGroup resourceARN".
const MAX_ACCOUNT_POLICIES: usize = 10;

/// AWS's documented integration quota, from `PutIntegration`: "Currently you can have only
/// one integration in your account".
const MAX_INTEGRATIONS: usize = 1;

/// AWS's documented metric-filter quota, from `PutMetricFilter`: "The maximum number of
/// metric filters that can be associated with a log group is 100".
const MAX_METRIC_FILTERS_PER_GROUP: usize = 100;

/// The account every ARN this emulator mints carries, which the ARN members of a
/// `PutIntegration` request are judged against.
const ACCOUNT: &str = "000000000000";

/// The log group whose metric-filter slots one row fills.
const FILTER_GROUP: &str = "/limit/metric-filters";

/// A resource policy document of the form `PutResourcePolicy$policyDocument` documents: a
/// statement allowing a service principal an action the API supports.
fn policy_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "EmulatorLimitRow",
            "Effect": "Allow",
            "Principal": {"Service": ["route53.amazonaws.com"]},
            "Action": ["logs:PutLogEvents"],
            "Resource": "*",
        }]
    })
    .to_string()
}

/// A `PutIntegration` request for the one integration type the operation documents.
fn integration_body(name: &str) -> Value {
    json!({
        "integrationName": name,
        "integrationType": "OPENSEARCH",
        "resourceConfig": {
            "openSearchResourceConfig": {
                "dataSourceRoleArn": format!("arn:aws:iam::{ACCOUNT}:role/CloudWatchOpenSearch"),
                "dashboardViewerPrincipals": [
                    format!("arn:aws:iam::{ACCOUNT}:user/dashboard-viewer")
                ],
                // Required by the model; 7 is inside `CollectionRetentionDays`' 1..30.
                "retentionDays": 7,
            }
        },
    })
}

/// A `PutMetricFilter` request naming one metric transformation.
fn metric_filter_body(filter: &str) -> Value {
    json!({
        "logGroupName": FILTER_GROUP,
        "filterName": filter,
        "filterPattern": "ERROR",
        "metricTransformations": [{
            "metricName": filter,
            "metricNamespace": "EmulatorLimits",
            "metricValue": "1",
        }],
    })
}

/// One row of the limit table: the case, the operation whose limit is driven, the requests
/// that FILL that limit (each of which must be accepted) and the one made once it is full.
struct Row {
    case: &'static str,
    operation: &'static str,
    fill: Vec<Value>,
    over: Value,
}

/// The rows, with the log group the metric-filter row needs created first.
async fn rows(logs: &Logs) -> Vec<Row> {
    let document = policy_document();
    logs.ok("CreateLogGroup", &json!({"logGroupName": FILTER_GROUP}))
        .await;
    vec![
        Row {
            case: "one resource policy past the account's ten",
            operation: "PutResourcePolicy",
            fill: (0..MAX_ACCOUNT_POLICIES)
                .map(|index| {
                    json!({
                        "policyName": format!("limit-policy-{index:02}"),
                        "policyDocument": document,
                    })
                })
                .collect(),
            over: json!({"policyName": "limit-policy-over", "policyDocument": document}),
        },
        Row {
            case: "one integration past the account's one",
            operation: "PutIntegration",
            fill: (0..MAX_INTEGRATIONS)
                .map(|index| integration_body(&format!("limit-integration-{index}")))
                .collect(),
            over: integration_body("limit-integration-over"),
        },
        Row {
            case: "one metric filter past the log group's hundred",
            operation: "PutMetricFilter",
            fill: (0..MAX_METRIC_FILTERS_PER_GROUP)
                .map(|index| metric_filter_body(&format!("limit-filter-{index:03}")))
                .collect(),
            over: metric_filter_body("limit-filter-over"),
        },
    ]
}

/// RULE: a request over a resource limit is refused `LimitExceededException` with the fixed
/// text of that class, and that text names neither the limit nor the resource that ran out.
#[tokio::test]
async fn a_request_over_a_resource_limit_states_the_fixed_text_of_its_class() {
    let logs = Logs::new();
    // What a refusal of this class may NOT carry: the limit that was reached, or the kind of
    // resource whose slots are full.
    let withheld = [
        MAX_ACCOUNT_POLICIES.to_string(),
        MAX_INTEGRATIONS.to_string(),
        MAX_METRIC_FILTERS_PER_GROUP.to_string(),
        "policy".to_string(),
        "integration".to_string(),
        "filter".to_string(),
        FILTER_GROUP.to_string(),
    ];
    for row in rows(&logs).await {
        for (index, body) in row.fill.iter().enumerate() {
            let (status, answered) = logs.call(row.operation, body).await;
            assert_eq!(
                status, 200,
                "case {}: filling the limit is accepted, request {index} answered {status} \
                 with {answered}",
                row.case
            );
        }
        let (code, message) = logs.refusal(row.operation, &row.over).await;
        assert_eq!(
            code, LIMIT_EXCEEDED,
            "case {}: expected {LIMIT_EXCEEDED}, got {code} with {message:?}",
            row.case
        );
        assert_eq!(
            message, LIMIT_TEXT,
            "case {}: expected {LIMIT_TEXT:?}, got {message:?}",
            row.case
        );
        for named in &withheld {
            assert!(
                !message.to_lowercase().contains(&named.to_lowercase()),
                "case {}: the fixed text withholds the limit and the resource, got \
                 {message:?} carrying {named:?}",
                row.case
            );
        }
    }
}
