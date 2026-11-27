// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack whose `AWS::Glue::Crawler` declares an Amazon S3 target in a bucket
//! that is not there is ROLLED BACK; one whose targets all resolve deploys.
//!
//! The rule under test: a crawler's `Targets` are data the declaration REFERENCES rather than
//! anything its own create brings into being — `S3Target$Path` is "the path to the Amazon S3
//! target" — so CloudFormation's resource handler resolves every declared target against the
//! account's own object store before it submits the crawler, and a target whose bucket that
//! store ANSWERED it holds none of fails the resource with the wrapped 404, which unwinds the
//! operation: the stack settles in a terminal rollback status, the failed resource's reason
//! names the S3 target and the status the object store answered, and every resource the create
//! had already made — the `AWS::Glue::Database` beside the crawler — is gone again, which a
//! client observes as `GetDatabase` answering the catalog's own miss. Only that answer refuses
//! anything: a target naming a bucket the stack itself declares, and one whose path addresses
//! no bucket at all, deploy.
//!
//! WHY it matters: a template with a typo'd or not-yet-created bucket in a crawler target is
//! reported to the deployer as a healthy deployment if nothing resolves it — the pipeline moves
//! on, and the breakage surfaces much later as a crawler that catalogues nothing — and the
//! resources AWS would have rolled back are left behind as orphans. It is the DEPLOYMENT that
//! resolves the reference: this service serves a `CreateCrawler` over a target it holds no data
//! under ([`crate::crawler_s3_target_resolution`] holds that rule), because a crawler is
//! routinely registered before the data exists and over buckets other accounts own.
//!
//! The rows differ along what the rule generalises over: how many targets a declaration states,
//! whether a target names a prefix or a whole bucket, WHICH of several targets is the one that
//! is not there, and the path forms that address no bucket to resolve.
//!
//! Four crates have to agree for this to hold — CloudFormation makes every create the template
//! declares through the service that owns the resource, IAM holds the role the crawl would run
//! as, Amazon S3 holds the buckets behind the targets, and this service takes the crawler — so
//! all four are mounted on ONE engine here ([`crate::cfn_deployed_crawler_target_rollback`]
//! assembles it), and every call travels the real in-process callback.
//!
//! Nothing here sleeps for a span: the wait is for the stack's own STATUS, looked at again
//! until it is terminal or a generous deadline passes, and no assertion reads the wall clock.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::cfn_deployed_crawler_target_rollback::{
    cloudformation_client, emulator_with_a_stack, failure_reasons, settled_status,
    CREATE_COMPLETE, CREATE_FAILED, ROLLBACK_COMPLETE,
};
use crate::wire::{call, error_code, error_message, ok};

/// The two fragments the failed resource's reason has to carry, which is what a client reading
/// a rolled-back crawler deployment branches on: WHAT was resolved and WHAT the object store
/// answered about it. Matched case-insensitively, as a reason is read.
const S3_TARGET_FRAGMENT: &str = "s3 target";
/// See [`S3_TARGET_FRAGMENT`]: the status Amazon S3 answers for a bucket it does not hold.
const ABSENT_STATUS_FRAGMENT: &str = "404";

/// Every name one row's template declares, so what is asserted is computed from the
/// declaration rather than read back out of the answer being checked.
struct Names {
    /// The stack the row deploys.
    stack: String,
    /// The role the crawler would run as.
    role: String,
    /// The database the stack declares beside the crawler.
    database: String,
    /// The crawler itself.
    crawler: String,
    /// The bucket the stack DECLARES, which its own create brings into being.
    bucket: String,
    /// A bucket name nothing creates anywhere: the reference a row points a target at to be
    /// resolved against an object store that holds no such bucket.
    absent_bucket: String,
}

impl Names {
    fn of(label: &str) -> Self {
        Self {
            stack: format!("cfn-crawler-target-{label}"),
            role: format!("cfn-crawler-target-role-{label}"),
            database: format!("cfn_crawler_target_db_{label}"),
            crawler: format!("cfn-crawler-target-{label}-crawler"),
            bucket: format!("cfn-crawler-target-data-{label}"),
            absent_bucket: format!("cfn-crawler-target-absent-{label}"),
        }
    }
}

/// The template one row deploys: the bucket the row's declared targets may name, the role the
/// crawl would run as (trusting the Glue service principal, so the role is never what fails
/// the resource), the database the crawler writes into, and the crawler over `targets`.
fn template(names: &Names, targets: &[String]) -> String {
    let paths: Vec<serde_json::Value> = targets
        .iter()
        .map(|path| json!({ "Path": path }))
        .collect();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a Glue database and a crawler over declared Amazon S3 targets",
        "Resources": {
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
                            "Principal": { "Service": "glue.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "Catalog": {
                "Type": "AWS::Glue::Database",
                "Properties": { "DatabaseInput": { "Name": &names.database } },
            },
            "Crawler": {
                "Type": "AWS::Glue::Crawler",
                "DependsOn": ["Catalog", "Data"],
                "Properties": {
                    "Name": &names.crawler,
                    "Role": { "Fn::GetAtt": ["Role", "Arn"] },
                    "DatabaseName": &names.database,
                    "Targets": { "S3Targets": paths },
                },
            },
        },
    })
    .to_string()
}

/// How one row writes the Amazon S3 targets its crawler declares, out of the row's own names
/// — so the declaration and the assertions are computed from one source.
type DeclaredTargets = fn(&Names) -> Vec<String>;

/// The same, for a row that also states WHICH of the targets it declares is the one the object
/// store holds no bucket for.
type DeclaredTargetsWithAbsent = fn(&Names) -> (Vec<String>, String);

/// Deploy one row's stack and answer the status it comes to rest in.
async fn deployed_status(
    router: &MultiServiceRouter,
    case: &str,
    names: &Names,
    targets: &[String],
) -> String {
    cloudformation_client(router)
        .create_stack()
        .stack_name(&names.stack)
        .template_body(template(names, targets))
        // "If your stack template contains IAM resources with custom names, you must specify
        // CAPABILITY_NAMED_IAM" — the stack names the crawler's role.
        .capabilities(aws_sdk_cloudformation::types::Capability::CapabilityNamedIam)
        .send()
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the stack must be accepted: {error:?}"));
    settled_status(router, case, &names.stack).await
}

/// A stack every one of whose declared targets resolves DEPLOYS, and the crawler it declared
/// carries the targets as written.
#[tokio::test]
async fn a_stack_whose_crawler_targets_all_resolve_deploys() {
    let router = emulator_with_a_stack();
    // (case, label, the targets the crawler declares, written from the row's own names)
    let rows: &[(&str, &str, DeclaredTargets)] = &[
        (
            "one target at a prefix of the bucket the stack declares",
            "prefix",
            |names| vec![format!("s3://{}/data/", names.bucket)],
        ),
        (
            "a target naming the whole declared bucket, with no prefix at all",
            "whole",
            |names| vec![format!("s3://{}", names.bucket)],
        ),
        (
            "two targets at two prefixes of the declared bucket",
            "two-prefixes",
            |names| {
                vec![
                    format!("s3://{}/in/", names.bucket),
                    format!("s3://{}/out/", names.bucket),
                ]
            },
        ),
        (
            "a target whose path addresses no Amazon S3 bucket at all",
            "no-bucket",
            |_| vec!["/mnt/data/".to_string()],
        ),
    ];
    for (case, label, targets) in rows {
        let names = Names::of(label);
        let targets = targets(&names);
        let status = deployed_status(&router, case, &names, &targets).await;
        assert_eq!(
            status, CREATE_COMPLETE,
            "case {case:?}: expected the stack to deploy, it settled {status:?} with the \
             failures {:?}",
            failure_reasons(&router, case, &names.stack).await
        );

        let crawler = ok(&router, "GetCrawler", &json!({ "Name": names.crawler })).await;
        let reported: Vec<String> = crawler["Crawler"]["Targets"]["S3Targets"]
            .as_array()
            .map(|targets| {
                targets
                    .iter()
                    .map(|target| target["Path"].as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            reported, targets,
            "case {case:?}: the deployed crawler must carry the declared targets, got {crawler}"
        );
        let database = ok(&router, "GetDatabase", &json!({ "Name": names.database })).await;
        assert_eq!(
            database["Database"]["Name"],
            json!(names.database),
            "case {case:?}: the stack's database must be readable, got {database}"
        );
    }
}

/// A stack one of whose declared targets names a bucket the object store holds none of does NOT
/// deploy: it settles in a terminal rollback status, the failed resource's reason names the S3
/// target and the 404 that store answered, and the database the create had already made is
/// rolled back with it.
#[tokio::test]
async fn a_stack_whose_crawler_target_names_an_absent_bucket_is_rolled_back() {
    let router = emulator_with_a_stack();
    // (case, label, the targets the crawler declares, the target expected to be reported)
    let rows: &[(&str, &str, DeclaredTargetsWithAbsent)] = &[
        (
            "the one target names a bucket nothing created",
            "absent",
            |names| {
                let path = format!("s3://{}/data/", names.absent_bucket);
                (vec![path.clone()], path)
            },
        ),
        (
            "the SECOND of two targets names a bucket nothing created",
            "second-absent",
            |names| {
                let absent = format!("s3://{}/data/", names.absent_bucket);
                (
                    vec![format!("s3://{}/data/", names.bucket), absent.clone()],
                    absent,
                )
            },
        ),
        (
            "a target naming a whole bucket nothing created",
            "absent-whole",
            |names| {
                let path = format!("s3://{}", names.absent_bucket);
                (vec![path.clone()], path)
            },
        ),
    ];
    for (case, label, targets) in rows {
        let names = Names::of(label);
        let (targets, absent_target) = targets(&names);
        let status = deployed_status(&router, case, &names, &targets).await;
        assert!(
            status == ROLLBACK_COMPLETE || status == CREATE_FAILED,
            "case {case:?}: expected the stack to fail and unwind, it settled {status:?}"
        );

        let reasons = failure_reasons(&router, case, &names.stack).await;
        let names_the_target = reasons.iter().any(|reason| {
            let lowered = reason.to_lowercase();
            lowered.contains(S3_TARGET_FRAGMENT)
                && reason.contains(ABSENT_STATUS_FRAGMENT)
                && reason.contains(&absent_target)
        });
        assert!(
            names_the_target,
            "case {case:?}: a failure reason must name the target {absent_target:?}, \
             {S3_TARGET_FRAGMENT:?} and {ABSENT_STATUS_FRAGMENT:?}, got {reasons:?}"
        );

        // The database the create had already made is gone again, which is what a client reads
        // as "the stack rolled back".
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

        // And no crawler was created over the target that is not there.
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
}
