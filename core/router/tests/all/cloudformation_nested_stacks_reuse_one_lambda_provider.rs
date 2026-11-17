// SPDX-License-Identifier: CC0-1.0

//! Nested stacks that REUSE one Lambda-backed custom-resource provider, and the outputs the
//! parent publishes from what those children published.
//!
//! The rule under test: **a stack reports every output its template declares with the
//! `OutputValue` the deployment resolved for it — including an output whose value is read out
//! of ANOTHER stack's published output (an `AWS::CloudFormation::Stack` resource's
//! `Outputs.<key>` attribute), and including when the value that child published is itself a
//! custom resource's provider-reported `PhysicalResourceId` or `Data`.**
//!
//! Three crates have to agree for that to hold — CloudFormation creates the child stacks and
//! sends the provider its requests, AWS Lambda runs the provider and answers at the request's
//! `ResponseURL`, Amazon S3 holds the child template the parent points at, and IAM answers for
//! the execution role — so the suite lives here.
//!
//! The rows differ along what the rule generalises over: how many children read the one
//! provider, which SPELLING of `Fn::GetAtt` reads a child's output (the dotted short form and
//! the two-part array), and what KIND of value the child published (the provider's `Data`, the
//! `PhysicalResourceId` a `Ref` resolves to, and a value of the parent's own that reaches no
//! child).
//!
//! Nothing here sleeps for a span the emulator owes: every wait is a poll for the CONDITION
//! with a deadline far longer than the spans this emulator gives these transitions.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, members_of, opt_element};

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The terminal status a deployment that worked reaches.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long a condition the clock owes is waited for, and how often it is looked at.
const SETTLE_DEADLINE: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

/// The bucket the child templates are staged in, and the stack that declares the provider.
const STAGING_BUCKET: &str = "nested-provider-reuse-templates";

/// The provider: it answers CloudFormation at the request's `ResponseURL` with the marker its
/// properties carry as `Data` and a physical id of its own — the shape of every provider that
/// hands a value back to the template that declared it.
const PROVIDER_SOURCE: &str = r#"
import json
import urllib.request


def handler(event, context):
    properties = event.get("ResourceProperties", {})
    body = json.dumps(
        {
            "Status": "SUCCESS",
            "PhysicalResourceId": "probe-" + properties.get("Marker", ""),
            "StackId": event["StackId"],
            "RequestId": event["RequestId"],
            "LogicalResourceId": event["LogicalResourceId"],
            "Data": {"Echo": properties.get("Marker", "")},
        }
    ).encode("utf-8")
    request = urllib.request.Request(
        event["ResponseURL"],
        data=body,
        headers={"content-type": ""},
        method="PUT",
    )
    with urllib.request.urlopen(request) as answered:
        answered.read()
    return {}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the provider CloudFormation invokes is the
/// function the stack created, and the answer it PUTs is served by this same router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request and what each handler reads its Region from.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One awsQuery call of the service `signing_name`.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call (API version 2010-05-15).
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> (u16, String) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .header("content-type", "application/octet-stream")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Stage one child template in Amazon S3, which is where a nested stack's `TemplateURL` points.
async fn stage(router: &MultiServiceRouter, key: &str, body: &str) {
    let (status, answered) = s3(router, "PUT", &format!("/{STAGING_BUCKET}/{key}"), body).await;
    assert!(
        (200..300).contains(&status),
        "setup: staging {key:?} must be accepted, got {status} with {answered}"
    );
}

/// The URL a staged child template is addressed by, path-style.
fn staged_url(key: &str) -> String {
    format!("https://s3.{REGION}.amazonaws.com/{STAGING_BUCKET}/{key}")
}

/// The child template every row stages: ONE custom resource backed by the provider its
/// `ServiceToken` parameter names, publishing both the id the provider minted and the `Data` it
/// reported.
fn child_template() -> String {
    json!({
        "Parameters": {
            "ProviderToken": {"Type": "String"},
            "Marker": {"Type": "String"},
        },
        "Resources": {
            "Thing": {
                "Type": "Custom::Probe",
                "Properties": {
                    "ServiceToken": {"Ref": "ProviderToken"},
                    "Marker": {"Ref": "Marker"},
                },
            },
        },
        "Outputs": {
            "Echo": {"Value": {"Fn::GetAtt": ["Thing", "Echo"]}},
            "ThingId": {"Value": {"Ref": "Thing"}},
        },
    })
    .to_string()
}

/// One child a row declares: its logical id, the marker it passes down, and the key its
/// template is staged under.
struct Child {
    logical_id: &'static str,
    marker: &'static str,
    key: &'static str,
}

/// One row: the parent stack, the children it declares, the outputs it publishes and what each
/// of those must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    children: &'static [Child],
    /// The parent's own outputs: the key, and the expression that reads a child's output.
    outputs: &'static [(&'static str, &'static str)],
    /// What each output must report, as `(key, value)`.
    expected: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        case: "one child, the dotted short form",
        stack: "nested-provider-one",
        children: &[Child {
            logical_id: "First",
            marker: "first",
            key: "one-child.json",
        }],
        outputs: &[("FirstEcho", "First.Outputs.Echo")],
        expected: &[("FirstEcho", "first")],
    },
    Row {
        case: "two children reusing the one provider, both kinds of published value",
        stack: "nested-provider-two",
        children: &[
            Child {
                logical_id: "First",
                marker: "left",
                key: "two-children-left.json",
            },
            Child {
                logical_id: "Second",
                marker: "right",
                key: "two-children-right.json",
            },
        ],
        outputs: &[
            ("FirstEcho", "First.Outputs.Echo"),
            ("FirstThingId", "First.Outputs.ThingId"),
            ("SecondEcho", "Second.Outputs.Echo"),
            ("SecondThingId", "Second.Outputs.ThingId"),
        ],
        expected: &[
            ("FirstEcho", "left"),
            ("FirstThingId", "probe-left"),
            ("SecondEcho", "right"),
            ("SecondThingId", "probe-right"),
        ],
    },
];

/// The parent's template: the execution role, the ONE provider function every child reuses, one
/// nested stack per child, and the outputs the row reads off them — plus one output of the
/// parent's own, which reaches no child at all.
fn parent_template(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Action": "sts:AssumeRole",
                        "Effect": "Allow",
                        "Principal": {"Service": "lambda.amazonaws.com"},
                    }],
                },
            },
        }),
    );
    resources.insert(
        "Provider".to_string(),
        json!({
            "Type": "AWS::Lambda::Function",
            "Properties": {
                "Code": {"ZipFile": PROVIDER_SOURCE},
                "Handler": "index.handler",
                "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                "Runtime": "python3.12",
                "Timeout": 30,
            },
        }),
    );
    for child in row.children {
        resources.insert(
            child.logical_id.to_string(),
            json!({
                "Type": "AWS::CloudFormation::Stack",
                "Properties": {
                    "TemplateURL": staged_url(child.key),
                    "Parameters": {
                        "ProviderToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                        "Marker": child.marker,
                    },
                },
            }),
        );
    }
    let mut outputs = serde_json::Map::new();
    for (index, (key, read)) in row.outputs.iter().enumerate() {
        // The two documented spellings of the same read, alternating over the row's outputs:
        // the dotted short form and the two-part array form.
        let value = match index % 2 {
            0 => json!({"Fn::GetAtt": read}),
            _ => {
                let (resource, attribute) = read.split_once('.').expect("a child's output read");
                json!({"Fn::GetAtt": [resource, attribute]})
            }
        };
        outputs.insert((*key).to_string(), json!({"Value": value}));
    }
    // An output of the PARENT's own, so a row asserts the parent's outputs are reported whether
    // or not they read a child.
    outputs.insert(
        "ProviderArn".to_string(),
        json!({"Value": {"Fn::GetAtt": ["Provider", "Arn"]}}),
    );
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// Every `Output` one `DescribeStacks` answer reports, as `(OutputKey, Option<OutputValue>)`.
///
/// Read entry by entry rather than as two parallel lists: whether an entry carries an
/// `OutputValue` at all is the very thing this suite is about, and a missing value would shift
/// a parallel pairing instead of reading as the absence it is.
fn reported_outputs(body: &str) -> Vec<(String, Option<String>)> {
    let Some(block) = crate::query_wire::block_of(body, "Outputs") else {
        return Vec::new();
    };
    members_of(block)
        .into_iter()
        .filter_map(|member| {
            let key = opt_element(member, "OutputKey")?;
            Some((key, opt_element(member, "OutputValue")))
        })
        .collect()
}

/// Poll `stack` until it reports a status that is not `*_IN_PROGRESS`, and answer that status
/// with the answer it was read from.
async fn settled(router: &MultiServiceRouter, case: &str, stack: &str) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (code, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected DescribeStacks to answer for {stack:?}, got {code} {body}"
        );
        let status = opt_element(&body, "StackStatus").unwrap_or_default();
        if !status.is_empty() && !status.ends_with("_IN_PROGRESS") {
            return (status, body);
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: expected {stack:?} to settle, last was {status:?} with {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Every resource the stack reports a FAILED status for, with the reason it gave — quoted when
/// a deployment did not complete.
async fn reported_failures(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (code, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    if code != 200 {
        return Vec::new();
    }
    members_of(&body)
        .into_iter()
        .filter(|member| {
            opt_element(member, "ResourceStatus")
                .is_some_and(|status| status.contains("FAILED"))
        })
        .map(|member| {
            format!(
                "{}={}",
                opt_element(member, "LogicalResourceId").unwrap_or_default(),
                opt_element(member, "ResourceStatusReason").unwrap_or_default()
            )
        })
        .collect()
}

#[tokio::test]
async fn a_parent_publishes_what_its_children_published_from_one_shared_provider() {
    let router = build_router();
    let (code, body) = s3(&router, "PUT", &format!("/{STAGING_BUCKET}"), "").await;
    assert!(
        (200..300).contains(&code),
        "setup: the staging bucket must be created, got {code} with {body}"
    );

    for row in ROWS {
        let case = row.case;
        for child in row.children {
            stage(&router, child.key, &child_template()).await;
        }
        let (code, body) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &parent_template(row)),
                ("Capabilities.member.1", "CAPABILITY_IAM"),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {code} with {body}"
        );

        let (status, body) = settled(&router, case, row.stack).await;
        assert_eq!(
            status,
            CREATE_COMPLETE,
            "case {case:?}: expected the parent to reach {CREATE_COMPLETE}, got {status:?} with \
             {body}; failures: {:?}",
            reported_failures(&router, row.stack).await
        );

        // The parent has reported its create COMPLETE, so every stack it created has settled:
        // what the children published is read from the children themselves, with no further
        // wait, because a parent that reports a create complete while a child is still being
        // created is exactly what this suite is about.
        for child in row.children {
            let (code, resource) = cloudformation(
                &router,
                "DescribeStackResource",
                &[
                    ("StackName", row.stack),
                    ("LogicalResourceId", child.logical_id),
                ],
            )
            .await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the child resource {:?} to be reported, got {code} \
                 {resource}",
                child.logical_id
            );
            let child_stack = opt_element(&resource, "PhysicalResourceId").unwrap_or_default();
            let (code, child_body) =
                cloudformation(&router, "DescribeStacks", &[("StackName", &child_stack)]).await;
            assert_eq!(
                code, 200,
                "case {case:?}: expected the child stack {child_stack:?} to be described, got \
                 {code} {child_body}"
            );
            assert_eq!(
                opt_element(&child_body, "StackStatus").as_deref(),
                Some(CREATE_COMPLETE),
                "case {case:?}: the parent reported {CREATE_COMPLETE}, so the child {:?} it \
                 created must have settled too, got {child_body}",
                child.logical_id
            );
            let published: BTreeMap<String, Option<String>> =
                reported_outputs(&child_body).into_iter().collect();
            assert_eq!(
                published.get("Echo").cloned().flatten().as_deref(),
                Some(child.marker),
                "case {case:?}: expected the child {:?} to publish the provider's Data, got \
                 {published:?}",
                child.logical_id,
            );
        }

        let reported: BTreeMap<String, Option<String>> =
            reported_outputs(&body).into_iter().collect();
        for (key, value) in row.expected {
            let actual = reported.get(*key).cloned().flatten();
            assert_eq!(
                actual.as_deref(),
                Some(*value),
                "case {case:?}: expected the output {key:?} to report {value:?}, got {actual:?} \
                 in {reported:?}"
            );
        }
        // The parent's own output is reported with its value too, and that value is the ARN of
        // the function AWS Lambda really holds.
        let provider_arn = reported
            .get("ProviderArn")
            .cloned()
            .flatten()
            .unwrap_or_default();
        assert!(
            provider_arn.starts_with("arn:aws:lambda:"),
            "case {case:?}: expected the parent's own output to carry the provider's ARN, got \
             {provider_arn:?} in {reported:?}"
        );
    }
}
