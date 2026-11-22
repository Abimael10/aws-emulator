// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation stack does with an `AWS::Glue::Crawler` whose Amazon S3 target names a
//! bucket that is not there, and with one whose declaration AWS Glue itself refuses.
//!
//! The rules under test:
//!
//! 1. **An `S3Target$Path` names the data a LATER crawl would walk — a reference to another
//!    service's store rather than a resource the create brings into being — so neither AWS Glue
//!    nor the deployment resolves it when the crawler is written: a declaration whose target
//!    names a bucket the object store does not hold is DEPLOYED, the stack reaches
//!    `CREATE_COMPLETE`, and the crawler is readable through AWS Glue's own API carrying exactly
//!    the targets that were declared.** A crawler is routinely declared before the bucket or the
//!    data behind it exists, and `CreateCrawler` models no not-found error for a target; whether
//!    a crawl can READ one is decided by the crawl, on the crawl
//!    ([`crate::cloudformation_deployed_crawler_crawls_its_s3_target`]).
//! 2. **A resource the owning service really does refuse is a resource CloudFormation reports as
//!    failed, so the stack does not reach `CREATE_COMPLETE`: it ends rolled back, the crawler is
//!    held by nobody, and every other resource that create had already made — here the stack's
//!    own `AWS::Glue::Database` — is released with it, so a read of that database answers
//!    `EntityNotFoundException` with `Database <name> not found.`.** The refusal that stands is
//!    about the REQUEST: "at least one crawl target must be specified, in the s3Targets field,
//!    the jdbcTargets field, or the DynamoDBTargets field" (`CreateCrawler`), which a declaration
//!    carrying an empty target list satisfies for none.
//!
//! Four crates have to agree for either to hold (CloudFormation makes the create calls and owns
//! the rollback, IAM answers for the crawl role, AWS Glue judges the declaration, and Amazon S3
//! is the store the declared target refers to), so the suite lives here: a crate-local test can
//! only ever see one side of it. The whole emulator is assembled, exactly as the production
//! entrypoint assembles it.
//!
//! What is asserted ELSEWHERE, and deliberately not repeated here:
//! `services/service-com-amazonaws-glue-awsglue/tests/all/crawler_s3_target_resolution.rs` pins
//! what AWS Glue's own `CreateCrawler`/`UpdateCrawler` store for each shape of target path, and
//! `core/router/tests/all/cloudformation_deployed_crawler_crawls_its_s3_target.rs` pins what a
//! crawler a stack deployed then crawls. What only this suite reaches is the deployment outcome:
//! the stack's own verdict, the per-resource reason a client reads it from, and the sibling
//! resource a rollback takes with it.
//!
//! The rows differ along what the rule generalises over: how an unresolvable path is written (a
//! prefix with a trailing separator, one without, a bucket root), whether the declaration also
//! names a target the store DOES hold, whether a held bucket's prefix carries any object at all,
//! and — for rule 2 — a declaration that names no crawl target at all.
//!
//! Neither clock is waited for: the stack's due instants are pulled into the past
//! ([`crate::cloudformation_stack_clock::settle_the_stacks`]), so every transition here is the
//! emulator's own settling rather than a race against the host.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_provisions_glue_jobs::{cloudformation, error_code, glue, TRUST_POLICY};
use crate::cloudformation_stack_clock::settle_the_stacks;
use crate::query_wire::{elements, members_of, opt_element, outputs_of};

/// The Region every stack here is deployed in, and the Region the catalog entities are created
/// in: a create is served by the endpoint it was called on.
const REGION: &str = "us-east-1";

/// The logical ids every row's template gives the three resources it declares.
const ROLE_LOGICAL_ID: &str = "CrawlerRole";
const DATABASE_LOGICAL_ID: &str = "CatalogDatabase";
const CRAWLER_LOGICAL_ID: &str = "CatalogCrawler";

/// The output keys every row's template publishes — `Ref` of the crawler and of the database,
/// which is how a client reaches what the stack deployed at all.
const CRAWLER_OUTPUT: &str = "CatalogCrawlerName";
const DATABASE_OUTPUT: &str = "CatalogDatabaseName";

/// How many settling rounds a stack is given to reach a terminal status. Generous: each round
/// materialises one transition of the stack's own clock (the create's conclusion, then the
/// rollback that conclusion makes due), and no round waits for the wall clock.
const SETTLING_ROUNDS: usize = 200;

/// The whole fixed text AWS Glue refuses a declaration naming no crawl target with, written out
/// here rather than read from that crate's constant so this suite pins the wording.
const TARGETS_NOT_DEFINED: &str = "Targets are not defined";

/// The `HandlerErrorCode` class CloudFormation reports a resource its owning service refused
/// the REQUEST for under, which is what tells a client the declaration — and not a passing
/// condition — is what failed the resource.
const REFUSED_REQUEST_CLASS: &str = "HandlerErrorCode: InvalidRequest";

/// The terminal statuses a create whose resource was refused comes to rest in: AWS rolls the
/// stack back, and a stack whose rollback itself has nothing left to undo reports the create's
/// own failure.
const ROLLED_BACK: [&str; 2] = ["ROLLBACK_COMPLETE", "CREATE_FAILED"];

/// One row: the buckets the object store holds before the deploy, the Amazon S3 targets the
/// declaration names, and whether the declaration is one AWS Glue refuses.
struct Row {
    case: &'static str,
    /// The buckets created before the deploy (no underscore: `CreateBucket` refuses one).
    held_buckets: &'static [&'static str],
    /// The objects staged in those buckets, as `(bucket, key, body)`.
    objects: &'static [(&'static str, &'static str, &'static str)],
    /// The `S3Targets[].Path` values the declaration carries, in order. Empty for the row whose
    /// declaration names no crawl target at all.
    paths: &'static [&'static str],
}

impl Row {
    /// Whether AWS Glue refuses this declaration: the one refusal about the REQUEST that stands
    /// here is a `Targets` naming no data store, which is a declaration carrying no path.
    fn is_refused(&self) -> bool {
        self.paths.is_empty()
    }
}

/// The rows: three shapes of path in a bucket nobody created, one declaration mixing such a path
/// with one the store holds, two declarations every target of which the store holds, and one
/// declaration naming no crawl target at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one target prefix in a bucket nobody created",
            held_buckets: &[],
            objects: &[],
            paths: &["s3://cfn-crawler-rollback-absent/data/"],
        },
        Row {
            case: "a target with no trailing separator in a bucket nobody created",
            held_buckets: &[],
            objects: &[],
            paths: &["s3://cfn-crawler-rollback-absent-two/data"],
        },
        Row {
            case: "the root of a bucket nobody created",
            held_buckets: &[],
            objects: &[],
            paths: &["s3://cfn-crawler-rollback-absent-three"],
        },
        Row {
            case: "a target the store holds beside one in a bucket nobody created",
            held_buckets: &["cfn-crawler-rollback-held"],
            objects: &[(
                "cfn-crawler-rollback-held",
                "data/sample.csv",
                "id,name\n1,ada\n",
            )],
            paths: &[
                "s3://cfn-crawler-rollback-held/data/",
                "s3://cfn-crawler-rollback-absent-four/other/",
            ],
        },
        Row {
            // A crawler is routinely declared before the data it crawls is written.
            case: "a prefix of a held bucket that carries no object yet",
            held_buckets: &["cfn-crawler-rollback-empty"],
            objects: &[],
            paths: &["s3://cfn-crawler-rollback-empty/not-written-yet/"],
        },
        Row {
            case: "a prefix of a held bucket that carries data",
            held_buckets: &["cfn-crawler-rollback-data"],
            objects: &[(
                "cfn-crawler-rollback-data",
                "data/sample.csv",
                "id,name\n1,ada\n",
            )],
            paths: &["s3://cfn-crawler-rollback-data/data/"],
        },
        Row {
            // Rule 2: the declaration names no data store to crawl, which AWS Glue refuses.
            case: "a declaration naming no crawl target at all",
            held_buckets: &[],
            objects: &[],
            paths: &[],
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: the stack's calls, the object store the
/// declared targets refer to and the catalog it writes all land in the same store the production
/// entrypoint serves from.
fn assembled_emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(Arc::clone(&engine));
    router.set_request_authentication(false);
    (router, engine)
}

/// A SigV4 `Authorization` header whose credential scope names Amazon S3 in [`REGION`], which is
/// what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn s3_authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/s3/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Amazon S3 request against the Region's path-style endpoint, which is the endpoint a
/// crawl of a declared target would read it through.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("s3.{REGION}.amazonaws.com"))
        .header("authorization", s3_authorization())
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.as_bytes().to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Create the row's buckets and stage its objects, so the store really holds what the row says it
/// holds — and nothing else.
async fn stage(router: &MultiServiceRouter, row: &Row) {
    let case = row.case;
    for bucket in row.held_buckets {
        let (status, body) = s3(router, "PUT", &format!("/{bucket}"), "").await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: CreateBucket({bucket:?}) must succeed, got {status} with {body}"
        );
    }
    for (bucket, key, content) in row.objects {
        let (status, body) = s3(router, "PUT", &format!("/{bucket}/{key}"), content).await;
        assert!(
            (200..300).contains(&status),
            "case {case:?}: PutObject({bucket:?}, {key:?}) must succeed, got {status} with {body}"
        );
    }
}

/// The template one row deploys: the crawl role, the catalog database the crawler would write
/// into, and the crawler itself over the row's targets — with both names published as outputs.
fn template(row: &Row, database: &str, crawler: &str) -> String {
    let targets: Vec<Value> = row.paths.iter().map(|path| json!({ "Path": path })).collect();
    let trust: Value = serde_json::from_str(TRUST_POLICY).expect("the trust policy is a document");
    json!({
        "Description": "a Glue Data Catalog database and a crawler over Amazon S3",
        "Resources": {
            ROLE_LOGICAL_ID: {
                "Type": "AWS::IAM::Role",
                "Properties": { "AssumeRolePolicyDocument": trust }
            },
            DATABASE_LOGICAL_ID: {
                "Type": "AWS::Glue::Database",
                "Properties": { "DatabaseInput": { "Name": database } }
            },
            CRAWLER_LOGICAL_ID: {
                "Type": "AWS::Glue::Crawler",
                "Properties": {
                    "Name": crawler,
                    "Role": { "Fn::GetAtt": [ROLE_LOGICAL_ID, "Arn"] },
                    "DatabaseName": { "Ref": DATABASE_LOGICAL_ID },
                    "Targets": { "S3Targets": targets }
                }
            }
        },
        "Outputs": {
            CRAWLER_OUTPUT: { "Value": { "Ref": CRAWLER_LOGICAL_ID } },
            DATABASE_OUTPUT: { "Value": { "Ref": DATABASE_LOGICAL_ID } }
        }
    })
    .to_string()
}

/// Deploy `body` as `stack_name` and answer the `DescribeStacks` XML of the stack once it has
/// reached a terminal status — which it reaches on its own clock, pulled into the past rather
/// than waited for.
async fn deploy(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    stack_name: &str,
    body: &str,
) -> String {
    let (status, answer) = cloudformation(
        router,
        REGION,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the template must be ADMITTED and any refusal be the resource's, got \
         {status} with {answer}"
    );

    for _ in 0..SETTLING_ROUNDS {
        settle_the_stacks(router, engine, case).await;
        let (_, described) =
            cloudformation(router, REGION, "DescribeStacks", &[("StackName", stack_name)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported.ends_with("_COMPLETE") || reported.ends_with("_FAILED") {
            return described;
        }
    }
    panic!("case {case:?}: the stack never reached a terminal status");
}

/// Every `ResourceStatusReason` the stack recorded for the crawler resource, in the order
/// `DescribeStackEvents` reports them.
async fn crawler_failure_reasons(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
) -> Vec<String> {
    let (status, events) = cloudformation(
        router,
        REGION,
        "DescribeStackEvents",
        &[("StackName", stack_name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with {events}"
    );
    members_of(&events)
        .into_iter()
        .filter(|event| {
            elements(event, "LogicalResourceId")
                .iter()
                .any(|id| id == CRAWLER_LOGICAL_ID)
        })
        .flat_map(|event| elements(event, "ResourceStatusReason"))
        .collect()
}

/// What AWS Glue answers for a crawler of `name`.
async fn read_crawler(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    glue(router, REGION, "GetCrawler", &json!({ "Name": name })).await
}

/// What AWS Glue answers for a database of `name`.
async fn read_database(router: &MultiServiceRouter, name: &str) -> (u16, Value) {
    glue(router, REGION, "GetDatabase", &json!({ "Name": name })).await
}

/// The `message` an awsJson error answer carries.
fn error_message(payload: &Value) -> String {
    payload
        .get("message")
        .or_else(|| payload.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Assert the stack ended rolled back over the crawler resource, with the per-resource reason a
/// client diagnoses it from: AWS Glue's own fixed refusal and the class CloudFormation reports a
/// refused request under.
fn assert_rolled_back(case: &str, described: &str, reasons: &[String]) {
    let reported = opt_element(described, "StackStatus").unwrap_or_default();
    assert!(
        ROLLED_BACK.contains(&reported.as_str()),
        "case {case:?}: expected the failed create to end in one of {ROLLED_BACK:?}, got \
         {reported:?} in {described}"
    );
    let stack_reason = opt_element(described, "StackStatusReason").unwrap_or_default();
    assert!(
        stack_reason.contains(CRAWLER_LOGICAL_ID),
        "case {case:?}: expected the stack's failure to name the resource, got {stack_reason:?}"
    );
    assert!(
        !reasons.is_empty(),
        "case {case:?}: expected the failure to be recorded on {CRAWLER_LOGICAL_ID}'s own event, \
         got no reason on it in {described}"
    );
    let carried = reasons.iter().any(|reason| {
        [TARGETS_NOT_DEFINED, REFUSED_REQUEST_CLASS]
            .iter()
            .all(|held| reason.contains(held))
    });
    assert!(
        carried,
        "case {case:?}: expected a reason on {CRAWLER_LOGICAL_ID} carrying \
         {TARGETS_NOT_DEFINED:?} and {REFUSED_REQUEST_CLASS:?}, got {reasons:?}"
    );
}

// ---------------------------------------------------------------------------
// What the deployment of such a declaration DOES
// ---------------------------------------------------------------------------

/// A stack whose `AWS::Glue::Crawler` names an Amazon S3 target in a bucket the object store does
/// not hold DEPLOYS: the crawler is stored with the declared targets and both entities are
/// readable through AWS Glue's own API. A declaration AWS Glue really does refuse — one naming no
/// crawl target — fails its resource instead, and neither the crawler nor the database the stack
/// would have owned is left behind.
#[tokio::test]
async fn a_crawler_over_a_bucket_nobody_created_still_deploys() {
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let (router, engine) = assembled_emulator();
        stage(&router, &row).await;

        // Lower case with no separator: AWS Glue folds a database name, so the name a client
        // reads back — and the one a refusal states — is the name as declared.
        let database = format!("crawlerrollbackdb{index}");
        let crawler = format!("crawler-rollback-{index}");
        let stack = format!("crawler-rollback-stack-{index}");
        let body = template(&row, &database, &crawler);
        let described = deploy(&router, &engine, case, &stack, &body).await;

        if !row.is_refused() {
            // Rule 1: the declared target is a reference nothing resolves at create time, so the
            // stack deployed and both handles it publishes are the names AWS Glue answers for.
            assert_eq!(
                opt_element(&described, "StackStatus").as_deref(),
                Some("CREATE_COMPLETE"),
                "case {case:?}: expected the stack to deploy, got {described}"
            );
            let outputs = outputs_of(&described);
            assert_eq!(
                (
                    outputs.get(CRAWLER_OUTPUT).map(String::as_str),
                    outputs.get(DATABASE_OUTPUT).map(String::as_str)
                ),
                (Some(crawler.as_str()), Some(database.as_str())),
                "case {case:?}: expected the declared names to be published, got {described}"
            );
            let (status, read) = read_crawler(&router, &crawler).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the deployed crawler to resolve, got {status} with {read}"
            );
            assert_eq!(
                read.pointer("/Crawler/DatabaseName").and_then(Value::as_str),
                Some(database.as_str()),
                "case {case:?}: the crawler writes into the declared database, got {read}"
            );
            let declared: Vec<&str> = row.paths.to_vec();
            let reported: Vec<String> = read
                .pointer("/Crawler/Targets/S3Targets")
                .and_then(Value::as_array)
                .map(|targets| {
                    targets
                        .iter()
                        .filter_map(|target| target.get("Path").and_then(Value::as_str))
                        .map(ToString::to_string)
                        .collect()
                })
                .unwrap_or_default();
            assert_eq!(
                reported, declared,
                "case {case:?}: expected the declared targets to be held, got {read}"
            );
            let (status, read) = read_database(&router, &database).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the deployed database to resolve, got {status} with \
                 {read}"
            );
            continue;
        }

        // Rule 2: the declaration is one the owning service refuses, so the resource failed.
        let reasons = crawler_failure_reasons(&router, case, &stack).await;
        assert_rolled_back(case, &described, &reasons);

        // Nothing the failed create made is left behind: the crawler was never stored, and the
        // database the rollback took with it is held by nobody.
        let (status, read) = read_crawler(&router, &crawler).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected no crawler to be left behind, got {status} with {read}"
        );
        assert_eq!(
            error_code(&read),
            "EntityNotFoundException",
            "case {case:?}: expected EntityNotFoundException for the refused crawler, got {read}"
        );
        let (status, read) = read_database(&router, &database).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the rolled-back database to be gone, got {status} with {read}"
        );
        assert_eq!(
            error_code(&read),
            "EntityNotFoundException",
            "case {case:?}: expected EntityNotFoundException for the rolled-back database, got \
             {read}"
        );
        assert_eq!(
            error_message(&read),
            format!("Database {database} not found."),
            "case {case:?}: expected the miss to name the database it could not resolve, got \
             {read}"
        );
    }
}
