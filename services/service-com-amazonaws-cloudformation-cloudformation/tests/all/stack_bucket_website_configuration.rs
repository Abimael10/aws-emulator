// SPDX-License-Identifier: CC0-1.0

//! A bucket's STATIC-WEBSITE configuration as a stack declares it, read back through Amazon
//! S3.
//!
//! RULE: a resource CloudFormation provisions carries, in the service that OWNS it, every
//! sub-resource configuration its declaration states — each applied by the one API call that
//! service publishes for it, since none of them is a member of the create. For an
//! `AWS::S3::Bucket` the whole `WebsiteConfiguration` is ONE `PutBucketWebsite`, so a bucket a
//! template declares one for reads back through `GetBucketWebsite` exactly like a bucket
//! configured through that call — rather than answering `NoSuchWebsiteConfiguration`, which is
//! the state every bucket is CREATED in and so is indistinguishable from a declaration that
//! never reached S3.
//!
//! The template's spelling is not the API's, and the translation is part of the rule: the
//! template states `IndexDocument` and `ErrorDocument` as the object key itself where the API
//! nests each in a container (`IndexDocument.Suffix`, `ErrorDocument.Key`), and names a routing
//! rule's halves `RedirectRule` and `RoutingRuleCondition` where the API names them `Redirect`
//! and `Condition`.
//!
//! The rows differ along what the rule generalises over: which MEMBERS the configuration states
//! (an index document alone, an index and an error document, a redirect-everything container,
//! and an index document with a conditional routing rule), a bucket that declares NO website
//! configuration at all, and the template SPELLING (JSON and YAML). A second case drives the
//! UPDATE half: a changed declaration is applied to the bucket that exists, and a declaration
//! that drops the property leaves it with no website configuration at all.
//!
//! Every expected value is computed from what the row declares, never read back out of the
//! answer under test.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{call, create_stack, describe_stack, member, router, xml_text};
use crate::stack_resource_provisioning::s3;
use service_com_amazonaws_cloudformation_cloudformation::state::status;

/// The longest this suite waits for a stack to reach its terminal status, and how often it
/// looks. Generous: the wait is for a CONDITION the emulator settles on its own clock, so the
/// deadline only has to outlast a loaded machine, and nothing here asserts on elapsed time.
const DEPLOY_DEADLINE: Duration = Duration::from_secs(60);
const DEPLOY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The code Amazon S3 answers `GetBucketWebsite` with for a bucket carrying no website
/// configuration.
const NO_SUCH_WEBSITE_CONFIGURATION: &str = "NoSuchWebsiteConfiguration";

/// Deploy a template and answer with the `DescribeStacks` body it completed with, looking again
/// until the stack reports a status other than the in-progress one or [`DEPLOY_DEADLINE`]
/// passes.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (http_status, created) = create_stack(router, stack, template, &[]).await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: CreateStack must answer 200, got {http_status} {created}"
    );
    settled(
        router,
        case,
        stack,
        status::CREATE_IN_PROGRESS,
        status::CREATE_COMPLETE,
    )
    .await;
}

/// `UpdateStack` with a new revision of the same template, waited out the same way.
async fn update(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) {
    let (http_status, body) = call(
        router,
        "UpdateStack",
        &[
            member("StackName", stack),
            member("TemplateBody", template),
        ],
    )
    .await;
    assert_eq!(
        http_status, 200,
        "case {case:?}: UpdateStack must answer 200, got {http_status} {body}"
    );
    settled(
        router,
        case,
        stack,
        status::UPDATE_IN_PROGRESS,
        status::UPDATE_COMPLETE,
    )
    .await;
}

/// Look at the stack again until it reports something other than `in_progress`, then assert it
/// reached `expected`.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    in_progress: &str,
    expected: &str,
) {
    let started = Instant::now();
    let mut described = String::new();
    while started.elapsed() < DEPLOY_DEADLINE {
        let (http_status, body) = describe_stack(router, stack).await;
        assert_eq!(
            http_status, 200,
            "case {case:?}: DescribeStacks must answer 200, got {http_status} {body}"
        );
        described = body;
        let reported = xml_text(&described, "StackStatus");
        if reported.as_deref() != Some(in_progress) && reported.is_some() {
            break;
        }
        tokio::time::sleep(DEPLOY_POLL_INTERVAL).await;
    }
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some(expected),
        "case {case:?}: the stack must reach {expected}, got {described}"
    );
}

/// What `GetBucketWebsite` reports for one bucket: the elements this suite reads, or the error
/// code for a bucket that has no website configuration.
struct Reported {
    status: u16,
    body: String,
}

impl Reported {
    /// The first value of `tag` the body carries, or `None` where it carries none.
    fn text(&self, tag: &str) -> Option<String> {
        xml_text(&self.body, tag)
    }

    /// The `Code` of the error envelope the answer carries, or the empty string for an answer
    /// that is no refusal.
    fn error_code(&self) -> String {
        xml_text(&self.body, "Code").unwrap_or_default()
    }
}

/// `GET /<bucket>?website` — `GetBucketWebsite`, as a client of the bucket reads it.
async fn website(router: &MultiServiceRouter, bucket: &str) -> Reported {
    let answer = s3(router, "GET", &format!("/{bucket}?website"), b"").await;
    Reported {
        status: answer.status,
        body: answer.body,
    }
}

/// One row of the create table: the case, the bucket the declaration names, and the
/// `(element, value)` pairs `GetBucketWebsite` must report — empty for a bucket that must have
/// no website configuration at all.
struct Row {
    case: &'static str,
    bucket: &'static str,
    reported: Vec<(&'static str, &'static str)>,
}

/// The stack whose buckets declare each form of website configuration, in JSON.
const JSON_TEMPLATE: &str = r#"{"Resources":{
     "IndexBucket":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"stack-website-index",
       "WebsiteConfiguration":{"IndexDocument":"index.html"}}},
     "DocumentsBucket":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"stack-website-documents",
       "WebsiteConfiguration":{"IndexDocument":"home.html","ErrorDocument":"oops.html"}}},
     "RedirectBucket":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"stack-website-redirect",
       "WebsiteConfiguration":{
         "RedirectAllRequestsTo":{"HostName":"example.com","Protocol":"https"}}}},
     "RoutedBucket":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"stack-website-routed",
       "WebsiteConfiguration":{"IndexDocument":"index.html","RoutingRules":[
         {"RoutingRuleCondition":{"KeyPrefixEquals":"docs/"},
          "RedirectRule":{"ReplaceKeyPrefixWith":"documents/","HttpRedirectCode":"301"}}]}}},
     "PlainBucket":{"Type":"AWS::S3::Bucket","Properties":{
       "BucketName":"stack-website-plain"}}}}"#;

/// The same rule in YAML, with an index and an error document: the template SPELLING is one of
/// the dimensions the rule generalises over.
const YAML_TEMPLATE: &str = concat!(
    "Resources:\n",
    "  ArchiveBucket:\n",
    "    Type: AWS::S3::Bucket\n",
    "    Properties:\n",
    "      BucketName: stack-website-archive\n",
    "      WebsiteConfiguration:\n",
    "        IndexDocument: start.html\n",
    "        ErrorDocument: missing.html\n",
);

#[tokio::test]
async fn a_bucket_a_stack_declares_carries_the_website_configuration_it_declared() {
    let router = router();
    deploy(
        &router,
        "five buckets, in JSON: each form of declared website configuration",
        "stack-website-json",
        JSON_TEMPLATE,
    )
    .await;
    deploy(
        &router,
        "one bucket, in YAML: an index and an error document",
        "stack-website-yaml",
        YAML_TEMPLATE,
    )
    .await;

    let rows: [Row; 6] = [
        Row {
            case: "an index document alone",
            bucket: "stack-website-index",
            reported: vec![("Suffix", "index.html")],
        },
        Row {
            case: "an index document and an error document",
            bucket: "stack-website-documents",
            reported: vec![("Suffix", "home.html"), ("Key", "oops.html")],
        },
        Row {
            case: "a redirect-everything container",
            bucket: "stack-website-redirect",
            reported: vec![("HostName", "example.com"), ("Protocol", "https")],
        },
        Row {
            case: "an index document with a conditional routing rule",
            bucket: "stack-website-routed",
            reported: vec![
                ("Suffix", "index.html"),
                ("KeyPrefixEquals", "docs/"),
                ("ReplaceKeyPrefixWith", "documents/"),
                ("HttpRedirectCode", "301"),
            ],
        },
        Row {
            case: "no website configuration declared at all",
            bucket: "stack-website-plain",
            reported: Vec::new(),
        },
        Row {
            case: "an index and an error document declared in YAML",
            bucket: "stack-website-archive",
            reported: vec![("Suffix", "start.html"), ("Key", "missing.html")],
        },
    ];

    for Row {
        case,
        bucket,
        reported,
    } in &rows
    {
        let answer = website(&router, bucket).await;
        if reported.is_empty() {
            // A bucket that declared none keeps the state every bucket is created in.
            assert_eq!(
                answer.status, 404,
                "case {case:?}: GetBucketWebsite({bucket:?}) must answer 404, got {} {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                NO_SUCH_WEBSITE_CONFIGURATION,
                "case {case:?}: expected {NO_SUCH_WEBSITE_CONFIGURATION:?}, got {:?} ({})",
                answer.error_code(),
                answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 200,
            "case {case:?}: GetBucketWebsite({bucket:?}) must answer 200, got {} {}",
            answer.status, answer.body
        );
        for (element, expected) in reported {
            assert_eq!(
                answer.text(element).as_deref(),
                Some(*expected),
                "case {case:?}: {bucket:?} must report {element} {expected:?}, got {}",
                answer.body
            );
        }
        // A configuration that states an index document states no redirect-everything
        // container, and the other way round: the two shapes are exclusive, so what the row
        // did NOT declare must not be reported either.
        let declares_index = reported.iter().any(|(element, _)| *element == "Suffix");
        let unexpected = if declares_index { "HostName" } else { "Suffix" };
        if !reported.iter().any(|(element, _)| element == &unexpected) {
            assert_eq!(
                answer.text(unexpected),
                None,
                "case {case:?}: {bucket:?} must report no {unexpected}, got {}",
                answer.body
            );
        }
    }
}

/// The bucket every revision of the update case declares.
const UPDATED_BUCKET: &str = "stack-website-updated";

/// The template one revision of the update case deploys: the bucket under its fixed name, with
/// the website configuration the revision declares (and none at all for `None`).
fn revision(declared: Option<Value>) -> String {
    let mut properties = json!({"BucketName": UPDATED_BUCKET});
    if let (Some(target), Some(configuration)) = (properties.as_object_mut(), declared) {
        target.insert("WebsiteConfiguration".to_string(), configuration);
    }
    json!({"Resources": {"Site": {"Type": "AWS::S3::Bucket", "Properties": properties}}})
        .to_string()
}

/// One revision of the update case: the case, the website configuration it declares (`None`
/// for a revision that declares none at all), and the `(element, value)` pairs
/// `GetBucketWebsite` must report afterwards.
type Revision = (&'static str, Option<Value>, Vec<(&'static str, &'static str)>);

#[tokio::test]
async fn a_changed_website_declaration_is_applied_to_the_bucket_that_exists() {
    let router = router();
    let stack = "stack-website-updates";
    deploy(
        &router,
        "the first revision: an index document",
        stack,
        &revision(Some(json!({"IndexDocument": "index.html"}))),
    )
    .await;

    // The rows differ along what CHANGED: the index document's value, an error document added
    // beside it, the whole configuration swapped for the exclusive redirect-everything shape,
    // and the property dropped altogether.
    let revisions: [Revision; 4] = [
        (
            "the index document changed",
            Some(json!({"IndexDocument": "home.html"})),
            vec![("Suffix", "home.html")],
        ),
        (
            "an error document added beside it",
            Some(json!({"IndexDocument": "home.html", "ErrorDocument": "oops.html"})),
            vec![("Suffix", "home.html"), ("Key", "oops.html")],
        ),
        (
            "swapped for a redirect-everything container",
            Some(json!({"RedirectAllRequestsTo": {"HostName": "elsewhere.example"}})),
            vec![("HostName", "elsewhere.example")],
        ),
        (
            "the website configuration dropped",
            None,
            Vec::new(),
        ),
    ];

    for (case, declared, reported) in revisions {
        update(&router, case, stack, &revision(declared)).await;
        let answer = website(&router, UPDATED_BUCKET).await;
        if reported.is_empty() {
            assert_eq!(
                answer.status, 404,
                "case {case:?}: GetBucketWebsite must answer 404, got {} {}",
                answer.status, answer.body
            );
            assert_eq!(
                answer.error_code(),
                NO_SUCH_WEBSITE_CONFIGURATION,
                "case {case:?}: expected {NO_SUCH_WEBSITE_CONFIGURATION:?}, got {:?} ({})",
                answer.error_code(),
                answer.body
            );
            continue;
        }
        assert_eq!(
            answer.status, 200,
            "case {case:?}: GetBucketWebsite must answer 200, got {} {}",
            answer.status, answer.body
        );
        for (element, expected) in &reported {
            assert_eq!(
                answer.text(element).as_deref(),
                Some(*expected),
                "case {case:?}: expected {element} {expected:?}, got {}",
                answer.body
            );
        }
    }
}
