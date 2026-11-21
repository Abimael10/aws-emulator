// SPDX-License-Identifier: CC0-1.0

//! The execution-role policies AWS publishes for a Lambda function's EVENT SOURCES — the
//! family a client attaches to the role Lambda polls a source as — read and attached over the
//! awsQuery wire.
//!
//! RULE: a policy of AWS's own `arn:aws:iam::aws:policy/` namespace that AWS publishes exists
//! in every account: it is readable by ARN, its default version serves the document AWS
//! publishes for it, and it is attachable to a role — whose permissions then include exactly
//! the actions that document allows. So the policy AWS publishes for each event source a
//! function can be TRIGGERED by is one this service serves: the three stream and queue reads
//! Lambda makes on the function's behalf are granted by attaching the published policy for
//! that source, and nothing else is.
//!
//! The rows differ along what the rule generalises over: WHICH source's published policy
//! (the queue, both shard sources, the Kafka cluster) and the log-only policy every execution
//! role carries beside them — including the entries no recorded request cited — and what each
//! one's document must therefore allow and must not.
//!
//! The reads themselves are asserted over the WHOLE catalog by
//! `tests/all/aws_managed_policy_catalog.rs`; what is added here is the FAMILY: that the
//! published policy of every event source Lambda reads is in it, and that its document is the
//! grant that source's own reads are decided by. The companion
//! `tests/all/aws_managed_policy_arn_resolution.rs` owns the path rule and the subset caveat.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_iam_awsidentitymanagementv20100508::state;

use crate::user_lifecycle::{call, document_element, element, elements, router};

/// A trust policy letting the Lambda service assume the role, which is what makes a role one
/// a function executes as.
const TRUST_POLICY: &str = r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Principal":{"Service":"lambda.amazonaws.com"},"Action":"sts:AssumeRole"}]}"#;

/// The path AWS files an execution-role policy under.
const SERVICE_ROLE_PATH: &str = "/service-role/";

/// The log writes EVERY Lambda execution-role policy carries, whichever source it is for:
/// "the permissions to write logs to CloudWatch Logs" every function's role is given.
const LOG_WRITES: [&str; 3] = [
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// One row: the policy AWS publishes for one event source, the reads Lambda makes on that
/// source with it, and a read of ANOTHER source that this policy does not allow.
struct Row {
    /// The friendly name AWS publishes the policy under, which with the path above is its
    /// whole identifier.
    policy_name: &'static str,
    /// What Lambda does with this role, named for the assertion messages.
    serves: &'static str,
    /// The actions this policy's document must allow: the calls Lambda makes on the function's
    /// behalf for that source.
    ///
    /// SOURCE: the published document of each policy, which is also what this emulator's
    /// Lambda poller decides a mapping on that kind of source by
    /// (`services/service-com-amazonaws-lambda-awsgirapiservice/src/state/event_source_authorization.rs`).
    allows: &'static [&'static str],
    /// An action of ANOTHER source's policy, which this one must NOT allow — so a role
    /// carrying it is granted its own source and not every source.
    denies: &'static str,
}

/// The whole family: the published policy of every event source this emulator's Lambda poller
/// reads, plus the log-only policy a function with no event source runs with.
const ROWS: &[Row] = &[
    Row {
        policy_name: "AWSLambdaSQSQueueExecutionRole",
        serves: "an Amazon SQS queue event source",
        allows: &[
            "sqs:ReceiveMessage",
            "sqs:DeleteMessage",
            "sqs:GetQueueAttributes",
        ],
        denies: "kinesis:GetRecords",
    },
    Row {
        policy_name: "AWSLambdaDynamoDBExecutionRole",
        serves: "an Amazon DynamoDB stream event source",
        allows: &[
            "dynamodb:DescribeStream",
            "dynamodb:GetRecords",
            "dynamodb:GetShardIterator",
        ],
        denies: "sqs:ReceiveMessage",
    },
    Row {
        policy_name: "AWSLambdaKinesisExecutionRole",
        serves: "an Amazon Kinesis data stream event source",
        allows: &[
            "kinesis:DescribeStream",
            "kinesis:GetRecords",
            "kinesis:GetShardIterator",
        ],
        denies: "sqs:ReceiveMessage",
    },
    Row {
        policy_name: "AWSLambdaMSKExecutionRole",
        serves: "an Amazon MSK cluster event source",
        allows: &[
            "kafka:DescribeCluster",
            "kafka:DescribeClusterV2",
            "kafka:GetBootstrapBrokers",
        ],
        denies: "sqs:ReceiveMessage",
    },
    Row {
        policy_name: "AWSLambdaBasicExecutionRole",
        serves: "a function with no event source at all",
        allows: &[],
        denies: "sqs:ReceiveMessage",
    },
];

/// The ARN AWS publishes `policy_name` at.
fn published_arn(policy_name: &str) -> String {
    format!("arn:aws:iam::aws:policy{SERVICE_ROLE_PATH}{policy_name}")
}

/// Create the role `role_name`, which an attachment needs first.
async fn create_role(router: &MultiServiceRouter, case: &str, role_name: &str) {
    let (status, body) = call(
        router,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", TRUST_POLICY),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateRole {role_name:?} must succeed, got {status}: {body}"
    );
}

/// RULE: every execution-role policy AWS publishes for an event source is a policy of this
/// account's view of IAM — readable by its published ARN, serving the document AWS publishes,
/// and attachable to a role whose permissions then allow exactly that source's own reads.
#[tokio::test]
async fn every_published_event_source_execution_role_policy_is_served_and_attachable() {
    let router = router();

    for (index, row) in ROWS.iter().enumerate() {
        let arn = published_arn(row.policy_name);
        let case = format!("{} ({})", row.policy_name, row.serves);

        // The catalog is what a read resolves the ARN against, so the entry has to be there
        // at all: a policy this service does not carry is reported missing to every client.
        let entry = state::aws_managed_policy(&arn).unwrap_or_else(|| {
            panic!(
                "case {case:?}: AWS publishes this policy at {arn}, so this service must carry \
                 it"
            )
        });
        assert_eq!(
            entry.path, SERVICE_ROLE_PATH,
            "case {case:?}: AWS files an execution-role policy under {SERVICE_ROLE_PATH}, got \
             {:?}",
            entry.path
        );

        let (status, body) = call(&router, "GetPolicy", &[("PolicyArn", arn.as_str())]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPolicy({arn}) must be served, got {status}: {body}"
        );
        assert_eq!(
            element(&body, "PolicyName"),
            row.policy_name,
            "case {case:?}: the read must report the published friendly name: {body}"
        );
        let default_version = element(&body, "DefaultVersionId");

        let (status, body) = call(
            &router,
            "GetPolicyVersion",
            &[
                ("PolicyArn", arn.as_str()),
                ("VersionId", default_version.as_str()),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetPolicyVersion({arn}, {default_version:?}) must be served, got \
             {status}: {body}"
        );
        let document = document_element(&body, "Document");
        for action in LOG_WRITES.iter().chain(row.allows) {
            assert!(
                document.contains(action),
                "case {case:?}: the published document must allow {action:?}, got {document}"
            );
        }
        assert!(
            !document.contains(row.denies),
            "case {case:?}: the published document must NOT allow {:?}, which belongs to \
             another source's policy, got {document}",
            row.denies
        );

        // A role of its own per row, so one row's attachment is never read as another's.
        let role_name = format!("event-source-role-{index}");
        create_role(&router, &case, &role_name).await;
        let (status, body) = call(
            &router,
            "AttachRolePolicy",
            &[("RoleName", role_name.as_str()), ("PolicyArn", arn.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the published policy must attach to an execution role, got \
             {status}: {body}"
        );
        let (status, body) = call(
            &router,
            "ListAttachedRolePolicies",
            &[("RoleName", role_name.as_str())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListAttachedRolePolicies must be served, got {status}: {body}"
        );
        assert_eq!(
            elements(&body, "PolicyArn"),
            vec![arn.clone()],
            "case {case:?}: the role must carry exactly the ARN it was attached with: {body}"
        );
    }
}

/// RULE (the other half): each of those policies is a DIFFERENT grant, so no two of them
/// carry the same document and each one's id and ARN are its own — a client attaching the
/// policy published for its source gets that source's reads rather than a shared grant.
#[tokio::test]
async fn each_published_event_source_policy_is_its_own_grant() {
    let mut documents: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    let mut ids: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();

    for row in ROWS {
        let arn = published_arn(row.policy_name);
        let entry = state::aws_managed_policy(&arn)
            .unwrap_or_else(|| panic!("case {:?}: the policy must be carried", row.policy_name));
        assert!(
            documents.insert(entry.document),
            "case {:?}: each published policy carries its own document, but this one repeats \
             another's: {}",
            row.policy_name,
            entry.document
        );
        assert!(
            ids.insert(entry.policy_id),
            "case {:?}: each published policy carries its own PolicyId, got {} twice",
            row.policy_name,
            entry.policy_id
        );
    }
}
