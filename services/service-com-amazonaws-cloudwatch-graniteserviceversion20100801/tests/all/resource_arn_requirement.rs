// SPDX-License-Identifier: CC0-1.0

//! How the operations that address a resource by ARN refuse a `ResourceARN` that names none.
//!
//! The rule under test: **a `ResourceARN` that is not the ARN of a resource this service tags
//! is refused HTTP 400 with the parameter-value fault the operation declares and the sentence
//! that states the REQUIREMENT the member carries — `ResourceARN must be valid and belong to a
//! CloudWatch resource: <value>` — whose only variable part is the value the request
//! supplied.** The sentence enumerates no resource KIND: which kinds are taggable is the
//! member's own documentation
//! (`smithy_query(query="doc",shape="TagResourceInput",member="ResourceARN")`), and a refusal
//! that lists them states more than the requirement a caller has to satisfy.
//!
//! The rule is the MEMBER's, not one operation's, so the three operations that take a
//! `ResourceARN` (`TagResource`, `UntagResource`, `ListTagsForResource`) answer one sentence.
//!
//! The rows differ along what the rule generalises over: HOW the value misses being such an ARN
//! (text that is no ARN at all, an ARN of another vendor's service, an ARN of this service
//! naming a resource kind it does not tag, one whose resource part is empty, one truncated
//! before the resource part) and which operation carried it — and the value is echoed, so each
//! row states its own.

use serde_json::json;

use crate::harness::{build_router, call, error_code, error_message};

/// The error SHAPE each of the three operations declares for a value it cannot use.
const PARAMETER_VALUE_SHAPE: &str = "InvalidParameterValueException";

/// The three operations that take a `ResourceARN`, with the rest of the request each needs.
const RESOURCE_ARN_OPERATIONS: [&str; 3] = ["TagResource", "UntagResource", "ListTagsForResource"];

/// The sentence a `ResourceARN` naming no resource of this service is refused with.
fn requirement(arn: &str) -> String {
    format!("ResourceARN must be valid and belong to a CloudWatch resource: {arn}")
}

#[tokio::test]
async fn a_resource_arn_naming_no_cloudwatch_resource_states_the_requirement() {
    // (case, the value the request carries)
    let rows: [(&str, &str); 6] = [
        ("text that is no ARN at all", "not-an-arn"),
        (
            "an ARN of another vendor's service",
            "arn:aws:logs:us-east-1:000000000000:log-group:/app",
        ),
        (
            "an ARN of this service naming a kind it does not tag",
            "arn:aws:cloudwatch:us-east-1:000000000000:anomaly-detector/one",
        ),
        (
            "an ARN of this service whose resource part is empty",
            "arn:aws:cloudwatch:us-east-1:000000000000:alarm:",
        ),
        (
            "an ARN truncated before the resource part",
            "arn:aws:cloudwatch:us-east-1:000000000000",
        ),
        ("an ARN of another partition's other service", "arn:aws-cn:sns:cn-north-1:000000000000:topic"),
    ];

    let router = build_router();
    for (case, arn) in rows {
        for operation in RESOURCE_ARN_OPERATIONS {
            let request = match operation {
                "TagResource" => json!({"ResourceARN": arn, "Tags": [{"Key": "Env", "Value": "prod"}]}),
                "UntagResource" => json!({"ResourceARN": arn, "TagKeys": ["Env"]}),
                _ => json!({"ResourceARN": arn}),
            };
            let (status, body) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&body).as_str()),
                (400, PARAMETER_VALUE_SHAPE),
                "case {case:?} ({operation}): expected HTTP 400 {PARAMETER_VALUE_SHAPE}, got \
                 {status} with {body}"
            );
            let expected = requirement(arn);
            let stated = error_message(&body);
            assert_eq!(
                stated, expected,
                "case {case:?} ({operation}): expected {expected:?}, got {stated:?}"
            );
        }
    }
}
