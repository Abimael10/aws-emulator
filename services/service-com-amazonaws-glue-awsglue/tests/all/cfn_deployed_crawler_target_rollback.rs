// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares a Glue crawler whose role Glue cannot assume is ROLLED
//! BACK, and one whose role trusts the Glue service principal deploys.
//!
//! The rule under test: a crawler write resolves the `Role` the crawl would run as against this
//! emulator's own IAM and refuses the create when IAM answers that Glue cannot assume it
//! ([`crate::crawler_role_trust`] drives that rule over the class of trust documents), and a
//! declared resource whose own service refuses its create is what makes CloudFormation fail that
//! resource and unwind the operation — so the stack settles in a terminal rollback status, its
//! failure reason carries what Glue refused (the role's trust policy), and every resource the
//! create had already made — the `AWS::Glue::Database` beside the crawler — is gone again, which
//! a client observes as `GetDatabase` answering the catalog's own miss.
//!
//! WHY it matters: a deployment whose crawler cannot be created is reported to the deployer as a
//! healthy one if nothing refuses it, so the client discovers the mistake much later or never,
//! and a template written to be refused at deploy time cannot be exercised at all.
//!
//! Four crates have to agree for that to hold — CloudFormation makes every create the template
//! declares through the service that owns the resource, IAM holds the role the crawl would run
//! as, Amazon S3 holds the bucket behind the target, and this service decides the create — so
//! all four are mounted on ONE engine here, the way `core/router/src/lib.rs` mounts them, and
//! every call travels the real in-process callback.
//!
//! Nothing here sleeps for a span: the wait is for the stack's own STATUS, looked at again until
//! it is terminal or a generous deadline passes, and no assertion reads the wall clock.

use std::time::{Duration, Instant};

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call, error_code, error_message, ok, REGION};

/// How long the wait for a stack to settle lasts before it is given up on, and how often the
/// status is looked at. The deadline is generous — what is waited for is the condition, not a
/// span — so a loaded or a fast host reaches the same verdict.
const STACK_DEADLINE: Duration = Duration::from_secs(60);
/// See [`STACK_DEADLINE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(50);

/// The stack statuses a create settles in: the healthy one, and the two a failed create rests
/// in once the operation has unwound.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
/// See [`CREATE_COMPLETE`].
const CREATE_FAILED: &str = "CREATE_FAILED";
/// See [`CREATE_COMPLETE`].
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";

/// The statuses the wait below stops at: every one a create can come to rest in.
const TERMINAL_STACK_STATUSES: [&str; 4] = [
    CREATE_COMPLETE,
    CREATE_FAILED,
    ROLLBACK_COMPLETE,
    "ROLLBACK_FAILED",
];

/// The resource status a declared resource whose create its own service refused reports.
const RESOURCE_CREATE_FAILED: &str = "CREATE_FAILED";

/// An emulator with this service, CloudFormation, IAM and Amazon S3 on ONE shared engine.
fn emulator_with_a_stack() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = crate::wire::emulator_on(std::sync::Arc::clone(&engine));
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        std::sync::Arc::clone(&engine),
    ));
    router
}

/// The CloudFormation client this suite deploys through, over the emulator's own in-process
/// callback — the transport every emulated service reaches another through, so nothing outside
/// this process is addressed.
fn cloudformation_client(router: &MultiServiceRouter) -> aws_sdk_cloudformation::Client {
    let callback = smithy_server_runtime::EmbeddedEmulator::from_router(router.clone());
    let config = aws_sdk_cloudformation::Config::builder()
        .behavior_version(aws_sdk_cloudformation::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(aws_sdk_cloudformation::config::Region::new(REGION))
        .endpoint_url(format!("https://cloudformation.{REGION}.amazonaws.com"))
        .retry_config(aws_sdk_cloudformation::config::retry::RetryConfig::disabled())
        .timeout_config(aws_sdk_cloudformation::config::timeout::TimeoutConfig::disabled())
        .identity_cache(aws_sdk_cloudformation::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            aws_sdk_cloudformation::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    aws_sdk_cloudformation::Client::from_conf(config)
}

/// The whole `Message` Glue refuses a crawler whose role it cannot assume with, which is what
/// CloudFormation carries into the failed resource's `ResourceStatusReason`.
const ROLE_NOT_ASSUMABLE: &str =
    "Service is unable to assume provided role. Please verify role's TrustPolicy";

/// Every name one row's template declares, so what is asserted is computed from the declaration
/// rather than read back out of the answer being checked.
struct Names {
    /// The stack the row deploys.
    stack: String,
    /// The role the crawler would run as.
    role: String,
    /// The database the stack declares beside the crawler.
    database: String,
    /// The crawler itself.
    crawler: String,
    /// The bucket the target names, created by the stack.
    bucket: String,
}

impl Names {
    fn of(label: &str) -> Self {
        Self {
            stack: format!("cfn-glue-crawler-{label}"),
            role: format!("cfn-glue-crawler-role-{label}"),
            database: format!("cfn_glue_crawler_db_{label}"),
            crawler: format!("cfn-glue-crawler-{label}-crawler"),
            bucket: format!("cfn-glue-crawler-data-{label}"),
        }
    }

    /// The Amazon S3 target the declared crawler crawls.
    fn target_path(&self) -> String {
        format!("s3://{}/data/", self.bucket)
    }
}

/// The template one row deploys: the bucket the target names, the role the crawl would run as —
/// trusting the Glue service principal only where `glue_can_assume_the_role` — the database the
/// crawler writes into, and the crawler itself.
fn template(names: &Names, glue_can_assume_the_role: bool) -> String {
    let trusted_principal = if glue_can_assume_the_role {
        "glue.amazonaws.com"
    } else {
        // A service principal that is not this one: IAM holds the role, and answers that Glue
        // cannot assume it.
        "ec2.amazonaws.com"
    };
    let resources = json!({
        "Data": {
            "Type": "AWS::S3::Bucket",
            "Properties": { "BucketName": &names.bucket },
        },
        "Role": {
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": &names.role,
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": trusted_principal },
                        "Action": "sts:AssumeRole",
                    }],
                },
                // The crawl reads the target as this role, so the listing and the object
                // reads it makes are judged against what the role is granted here.
                "Policies": [{
                    "PolicyName": "read-the-target",
                    "PolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Action": ["s3:GetObject", "s3:ListBucket"],
                            "Resource": [
                                format!("arn:aws:s3:::{}", names.bucket),
                                format!("arn:aws:s3:::{}/*", names.bucket),
                            ],
                        }],
                    },
                }],
            },
        },
        "Catalog": {
            "Type": "AWS::Glue::Database",
            "Properties": {
                "DatabaseInput": { "Name": &names.database },
            },
        },
        "Crawler": {
            "Type": "AWS::Glue::Crawler",
            "DependsOn": ["Catalog", "Data"],
            "Properties": {
                "Name": &names.crawler,
                "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                "DatabaseName": &names.database,
                "Targets": { "S3Targets": [{ "Path": names.target_path() }] },
            },
        },
    });
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a Glue database and a crawler over one Amazon S3 target",
        "Resources": resources,
    })
    .to_string()
}

/// Deploy `names.stack` and answer the status it comes to rest in, waiting for a TERMINAL
/// status rather than for a span.
async fn deployed_status(
    router: &MultiServiceRouter,
    case: &str,
    names: &Names,
    glue_can_assume_the_role: bool,
) -> String {
    let client = cloudformation_client(router);
    client
        .create_stack()
        .stack_name(&names.stack)
        .template_body(template(names, glue_can_assume_the_role))
        // "If your stack template contains IAM resources with custom names, you must specify
        // CAPABILITY_NAMED_IAM" — the stack names the crawler's role.
        .capabilities(aws_sdk_cloudformation::types::Capability::CapabilityNamedIam)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the stack must be accepted: {error:?}"));

    let deadline = Instant::now() + STACK_DEADLINE;
    loop {
        let described = client
            .describe_stacks()
            .stack_name(&names.stack)
            .send()
            .await
            .unwrap_or_else(|error| {
                panic!("case {case:?}: the stack must be describable: {error:?}")
            });
        let stack = described
            .stacks()
            .first()
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: DescribeStacks must report the stack"));
        let status = stack
            .stack_status()
            .map(|status| status.as_str().to_string())
            .unwrap_or_default();
        if TERMINAL_STACK_STATUSES.contains(&status.as_str()) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stack to settle, it reports {status:?} ({:?})",
            stack.stack_status_reason()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every `ResourceStatusReason` the stack's events carry for a resource whose create FAILED.
async fn failure_reasons(router: &MultiServiceRouter, case: &str, names: &Names) -> Vec<String> {
    let described = cloudformation_client(router)
        .describe_stack_events()
        .stack_name(&names.stack)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the events must be readable: {error:?}"));
    described
        .stack_events()
        .iter()
        .filter(|event| {
            event
                .resource_status()
                .map(|status| status.as_str())
                .is_some_and(|status| status == RESOURCE_CREATE_FAILED)
        })
        .filter_map(|event| event.resource_status_reason().map(str::to_string))
        .collect()
}

/// A stack whose crawler runs as a role trusting the Glue service principal deploys, and the
/// database and the crawler it declared are both readable afterwards.
#[tokio::test]
async fn a_stack_whose_crawler_role_trusts_glue_deploys() {
    let router = emulator_with_a_stack();
    let case = "the crawler's role trusts the Glue service principal";
    let names = Names::of("held");

    let status = deployed_status(&router, case, &names, true).await;
    assert_eq!(
        status, CREATE_COMPLETE,
        "case {case:?}: expected the stack to deploy, it settled {status:?} with the failures \
         {:?}",
        failure_reasons(&router, case, &names).await
    );

    let database = ok(
        &router,
        "GetDatabase",
        &json!({ "Name": names.database }),
    )
    .await;
    assert_eq!(
        database["Database"]["Name"],
        json!(names.database),
        "case {case:?}: the stack's database must be readable, got {database}"
    );
    let crawler = ok(&router, "GetCrawler", &json!({ "Name": names.crawler })).await;
    assert_eq!(
        crawler["Crawler"]["Targets"]["S3Targets"],
        json!([{ "Path": names.target_path(), "Exclusions": [] }]),
        "case {case:?}: the crawler must carry the declared target, got {crawler}"
    );
}

/// A stack whose crawler runs as a role Glue cannot assume does NOT deploy: it settles in a
/// terminal rollback status, its failure reason names the trust policy Glue refused, and the
/// database the stack had already created is rolled back with it.
#[tokio::test]
async fn a_stack_whose_crawler_role_glue_cannot_assume_is_rolled_back() {
    let router = emulator_with_a_stack();
    let case = "the crawler's role trusts another service only";
    let names = Names::of("unassumable");

    let status = deployed_status(&router, case, &names, false).await;
    assert!(
        status == ROLLBACK_COMPLETE || status == CREATE_FAILED,
        "case {case:?}: expected the stack to fail and unwind, it settled {status:?}"
    );

    let reasons = failure_reasons(&router, case, &names).await;
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains(ROLE_NOT_ASSUMABLE)),
        "case {case:?}: a failure reason must carry {ROLE_NOT_ASSUMABLE:?}, got {reasons:?}"
    );

    // The database the create had already made is gone again, which is what a client reads as
    // "the stack rolled back".
    let (status, body) = call(&router, "GetDatabase", &json!({ "Name": names.database })).await;
    assert_eq!(
        status, 400,
        "case {case:?}: the rolled-back database must be gone, got {status} with {body}"
    );
    assert!(
        error_code(&body).ends_with("EntityNotFoundException"),
        "case {case:?}: expected EntityNotFoundException, got {}",
        error_code(&body)
    );
    assert_eq!(
        error_message(&body),
        format!("Database {} not found.", names.database),
        "case {case:?}: the miss must name the database, got {:?}",
        error_message(&body)
    );

    // And the crawler itself was never created.
    let (crawler_status, crawler_body) =
        call(&router, "GetCrawler", &json!({ "Name": names.crawler })).await;
    assert_eq!(
        crawler_status, 400,
        "case {case:?}: no crawler must have been created, got {crawler_status} with \
         {crawler_body}"
    );
    assert!(
        error_code(&crawler_body).ends_with("EntityNotFoundException"),
        "case {case:?}: expected EntityNotFoundException for the crawler, got {}",
        error_code(&crawler_body)
    );
}
