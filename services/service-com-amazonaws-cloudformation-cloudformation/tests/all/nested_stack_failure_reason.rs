// SPDX-License-Identifier: CC0-1.0

//! The `ResourceStatusReason` a FAILED nested stack records, end to end over the awsQuery
//! wire.
//!
//! RULE: CloudFormation reports a failed resource's cause inside the resource-provider
//! envelope — `Resource handler returned message: "<the provider's message>" (RequestToken:
//! <token>, HandlerErrorCode: <class>)` — only where a registry resource PROVIDER answered for
//! the resource. A type CloudFormation deploys ITSELF has no provider answer to quote, so the
//! reason its failed resource carries is CloudFormation's own sentence as it reads: no quoting,
//! no request token and no handler class, none of which any provider supplied.
//!
//! `AWS::CloudFormation::Stack` is such a type — a nested stack is created by CloudFormation's
//! own `CreateStack`, so what fails the parent's resource is CloudFormation's own refusal of
//! the child's create, and that is exactly what the event must say. The other half of the rule
//! is asserted too: a type a resource provider DOES answer for still reports the envelope, so
//! what this suite pins is where the envelope belongs rather than that it never appears.
//!
//! Why it matters: the `CREATE_FAILED` reason is the only machine-readable account a deploy
//! pipeline has of why a nested stack failed. A client that matches CloudFormation's sentence
//! for a bad `TemplateURL`, or shows it to an operator, must not be handed an envelope naming a
//! provider that was never asked anything.
//!
//! The rows differ along the FORM of the refused reference — an Amazon S3 ARN, the direct
//! `s3://` spelling, plain http, a bucket's static website endpoint, an https URL carrying no
//! object — and along WHICH refusal CloudFormation gives: the scheme the documentation fixes,
//! the website endpoint it does not read templates from, a URL naming no object, and an object
//! the bucket does not hold.

use serde_json::json;

use crate::change_set_lifecycle::settle_the_store;
use crate::harness::{
    call, create_stack, failed_resource_reason, member, quoted_and_class, router_with_engine,
    xml_text, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::nested_stacks::{staged_url, STACK_RESOURCE_TYPE};
use crate::stack_lifecycle::{acknowledging_iam, iam};
use crate::template_by_reference::{create_staging_bucket, stage, STAGING_BUCKET};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The logical id the parent declares its nested stack under.
const NESTED: &str = "NestedStack";

/// The child template one row addresses correctly and the others address wrongly: one topic,
/// which the child stack really creates in Amazon SNS.
const CHILD_KEY: &str = "reason-child-template.json";

/// The sentences CloudFormation words a `TemplateURL` it will not read from with.
///
/// Source: `CreateStackInput$TemplateURL`
/// (`smithy_query(query="doc",shape="CreateStackInput",member="TemplateURL")`), which states
/// the two rules the first two of these answer — "the location for an Amazon S3 bucket must
/// start with `https://`. URLs from S3 static websites are not supported" — beside the
/// requirement that the URL point to a template in an Amazon S3 bucket, which is what the last
/// two report is not so. Each is CloudFormation's own words about the REQUEST, which is why
/// none of them belongs in a provider envelope.
const S3_LOCATION_SCHEME: &str = "The location for an Amazon S3 bucket must start with https://";
const S3_WEBSITE_URL: &str = "S3 static website URLs are not supported.";
const NOT_A_READABLE_OBJECT: &str =
    "TemplateURL must reference a valid S3 object to which you have access.";

/// The two halves of the envelope a provider-backed failure is reported in, which a nested
/// stack's reason must carry NEITHER of.
const ENVELOPE_OPENS: &str = "Resource handler returned message: \"";
const ENVELOPE_CLASS: &str = "HandlerErrorCode: ";

/// One row: a parent stack whose nested stack addresses a template CloudFormation will not
/// read, and the reason the parent's resource must record for it.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The `TemplateURL` the nested-stack resource declares.
    template_url: String,
    /// The `ResourceStatusReason` the `CREATE_FAILED` event must carry, exactly.
    reason: String,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape of the defect: an ARN names the object but is not a location.
            case: "an Amazon S3 ARN rather than an https URL",
            stack: "nested-reason-arn",
            template_url: format!("arn:aws:s3:::{STAGING_BUCKET}/{CHILD_KEY}"),
            reason: S3_LOCATION_SCHEME.to_string(),
        },
        Row {
            case: "the direct s3 spelling of the same object",
            stack: "nested-reason-s3-scheme",
            template_url: format!("s3://{STAGING_BUCKET}/{CHILD_KEY}"),
            reason: S3_LOCATION_SCHEME.to_string(),
        },
        Row {
            case: "the same object over plain http",
            stack: "nested-reason-http",
            template_url: format!("http://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{CHILD_KEY}"),
            reason: S3_LOCATION_SCHEME.to_string(),
        },
        Row {
            case: "the staging bucket's static website endpoint",
            stack: "nested-reason-website",
            template_url: format!(
                "http://{STAGING_BUCKET}.s3-website-{REGION}.amazonaws.com/{CHILD_KEY}"
            ),
            reason: S3_WEBSITE_URL.to_string(),
        },
        Row {
            case: "an https URL carrying no object at all",
            stack: "nested-reason-no-object",
            template_url: format!("https://{STAGING_BUCKET}.s3.{REGION}.amazonaws.com/"),
            reason: NOT_A_READABLE_OBJECT.to_string(),
        },
        Row {
            // A reference that is addressed correctly and resolves to nothing: the refusal is
            // Amazon S3's absence, reported in CloudFormation's own sentence about it.
            case: "a key the staging bucket does not hold",
            stack: "nested-reason-absent-key",
            template_url: staged_url("no-such-child-template.json"),
            reason: format!(
                "S3 object does not exist. Bucket: {STAGING_BUCKET}, Key: \
                 no-such-child-template.json. Error: NoSuchKey"
            ),
        },
    ]
}

/// A parent template declaring ONE nested stack at `template_url`.
fn parent_template(template_url: &str) -> String {
    json!({
        "Resources": {
            NESTED: {
                "Type": STACK_RESOURCE_TYPE,
                "Properties": {"TemplateURL": template_url}
            }
        }
    })
    .to_string()
}

/// A child template declaring one topic, staged so the rows differ only in how they ADDRESS
/// it.
fn child_template() -> String {
    json!({
        "Resources": {
            "Topic": {"Type": "AWS::SNS::Topic", "Properties": {"TopicName": "reason-child-topic"}}
        }
    })
    .to_string()
}

/// Poll `stack` until it reports a status no operation is still in flight in, making every
/// transition the store already owes due on each look.
///
/// What is waited FOR is the CONDITION — a settled status — under the generous deadline this
/// target shares, so a loaded host cannot decide a case.
async fn settled_status(
    case: &str,
    router: &smithy_server_runtime::MultiServiceRouter,
    engine: &std::sync::Arc<engine::InMemoryEngine>,
    stack: &str,
) -> String {
    let deadline = tokio::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        settle_the_store(router, engine).await;
        let (code, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: DescribeStacks({stack}) must answer 200, got {code} {body}"
        );
        let reported = xml_text(&body, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return reported;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: {stack} never settled, last reported {reported:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

#[tokio::test]
async fn a_failed_nested_stack_records_cloudformations_own_reason() {
    let (router, engine) = router_with_engine();
    create_staging_bucket(&router).await;
    stage(&router, CHILD_KEY, &child_template()).await;

    for row in rows() {
        let case = row.case;
        let (code, body) = create_stack(
            &router,
            row.stack,
            &parent_template(&row.template_url),
            &[],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the parent must be accepted, got {code} {body}"
        );
        let settled = settled_status(case, &router, &engine, row.stack).await;
        assert_eq!(
            settled,
            status::ROLLBACK_COMPLETE,
            "case {case:?}: expected the parent to roll back, got {settled:?}"
        );
        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack,
            NESTED,
            resource_status::CREATE_FAILED,
        )
        .await;
        assert_eq!(
            reason, row.reason,
            "case {case:?}: expected {NESTED} to record CloudFormation's own reason {:?}, got \
             {reason:?}",
            row.reason
        );
        assert!(
            !reason.contains(ENVELOPE_OPENS) && !reason.contains(ENVELOPE_CLASS),
            "case {case:?}: expected no resource-provider envelope around a nested stack's \
             reason, got {reason:?}"
        );
    }
}

/// The other half of the rule: a type a registry resource PROVIDER answers for still reports
/// that provider's message inside the envelope, under the class its refusal falls in.
///
/// The collision is an IAM role NAME a client took outside any stack, as
/// `failed_create_rollback.rs` takes it: the create then really reaches the service that owns
/// the type and is refused there, which is the one case the envelope exists for.
#[tokio::test]
async fn a_failed_provider_backed_resource_still_reports_the_handler_envelope() {
    const CASE: &str = "a role name IAM already holds";
    const ROLE: &str = "nested-reason-taken-role";
    const LOGICAL_ID: &str = "ZRole";
    let (router, engine) = router_with_engine();
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "glue.amazonaws.com"},
            "Action": "sts:AssumeRole"
        }]
    });
    let (code, body) = iam(
        &router,
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust.to_string()),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {CASE:?}: the client's own CreateRole must succeed, got {code} {body}"
    );

    let stack = "nested-reason-provider";
    let template = json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": {"RoleName": ROLE, "AssumeRolePolicyDocument": trust}
            }
        }
    })
    .to_string();
    let (code, body) = create_stack(
        &router,
        stack,
        &template,
        // The pre-deployment existence check would refuse this deployment before any owning
        // service was asked anything, and then there would be no provider refusal to report:
        // off, exactly as `failed_create_rollback.rs` turns it off, so the create really
        // reaches the service that owns the type.
        &acknowledging_iam(&[member("DisableValidation", "true")]),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {CASE:?}: the stack must be accepted, got {code} {body}"
    );
    let settled = settled_status(CASE, &router, &engine, stack).await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case {CASE:?}: expected the stack to roll back, got {settled:?}"
    );
    let reason = failed_resource_reason(
        CASE,
        &router,
        REGION,
        stack,
        LOGICAL_ID,
        resource_status::CREATE_FAILED,
    )
    .await;
    let (quoted, class) = quoted_and_class(CASE, &reason);
    assert!(
        quoted.contains(ROLE),
        "case {CASE:?}: expected the provider's own message about {ROLE}, got {reason:?}"
    );
    assert!(
        !class.is_empty(),
        "case {CASE:?}: expected the class the refusal falls in, got {reason:?}"
    );
}
