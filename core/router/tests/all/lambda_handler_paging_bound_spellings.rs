// SPDX-License-Identifier: CC0-1.0

//! The SPELLING a running Lambda handler passes a paged read's bound in, end to end over the
//! assembled router.
//!
//! RULE: the AWS SDK a managed runtime supplies carries a call's whole-number member as the
//! value the handler passed, read in EITHER spelling that SDK takes it in — the number itself,
//! or the decimal text of it, which is how an SDK models a bound an operation binds to its
//! request's query string — so the two spellings ask for the SAME call and are answered with
//! the same page. A value that is no whole number the member can hold is a call the SDK makes
//! no request for at all, and a whole number the SERVICE itself does not accept is refused by
//! that service in its own error code.
//!
//! Only a cross-service suite can pin this: the page is CloudFront's own records, the handler
//! and the SDK seam its call travels over are Lambda's, and the execution role the call is
//! signed as is IAM's. The handler half that resolves a distribution and refreshes it is
//! `core/router/tests/all/lambda_handler_reads_its_distributions.rs`, which owns what the
//! distribution surface answers a handler; this suite owns the SPELLING the bound of such a
//! read may arrive in, which that suite passes in one spelling only.
//!
//! The rows differ along the spelling of the bound (a number, the decimal text of one),
//! its value (one distribution, every distribution the account holds), whether it is passed at
//! all, and whether it is a whole number the member can hold (text spelling no number, text
//! spelling a magnitude past the member's range, a fraction) or one the service refuses (a
//! negative count).
//!
//! Every read runs in ONE invocation — an invocation being the dearest thing this suite does —
//! and each row is reported under its own name, so one run answers the whole table.

use std::collections::BTreeMap;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, elements, form_encode, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// CloudFront's control-plane host, and the API version every route is under.
const CLOUDFRONT_HOST: &str = "cloudfront.amazonaws.com";
const API_VERSION: &str = "2020-05-31";

/// The distributions this suite creates, which is what a bounded page is cut out of.
const DISTRIBUTIONS: usize = 2;

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The state an invocation needs the function to have reached.
const ACTIVE: &str = "Active";

/// How long work the clock owes is waited for, and how often it is looked at. Generous: no row
/// here measures a deadline.
const WAIT: Duration = Duration::from_secs(60);
const LOOK: Duration = Duration::from_millis(50);

/// The stack, the role and the function this suite deploys.
const STACK: &str = "paging-bound-spellings-stack";
const ROLE: &str = "paging-bound-spellings-role";
const FUNCTION: &str = "paging-bound-spellings-reader";

/// The error code CloudFront refuses a bound no page can be taken by with
/// (`smithy_query(query="operation", operation="ListDistributions")` models `InvalidArgument`
/// as that operation's only error).
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The largest magnitude the `integer` shape `ListDistributions$MaxItems` targets can hold is
/// 32 bits wide, so this is one past it: a bound the member could not carry whatever spelling
/// it arrives in.
const PAST_THE_RANGE: &str = "2147483648";

/// The handler every row is answered by: it asks CloudFront for one page of the account's
/// distributions with the bound the row states — passed exactly as the row spells it, or not
/// passed at all — and reports either the page CloudFront answered, the refusal that service
/// stated, or the call its SDK made no request for.
const HANDLER: &str = r#"
import boto3
import botocore.exceptions


def lambda_handler(event, context):
    cloudfront = boto3.client("cloudfront")
    report = {}
    for row in event["rows"]:
        asked = {} if row["bound"] is None else {"MaxItems": row["bound"]}
        try:
            page = cloudfront.list_distributions(**asked)["DistributionList"]
        except botocore.exceptions.SdkCallNotMade as error:
            report[row["case"]] = {"not_made": str(error)}
            continue
        except botocore.exceptions.ClientError as error:
            report[row["case"]] = {
                "code": error.response["Error"]["Code"],
                "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
            }
            continue
        report[row["case"]] = {
            "max_items": page["MaxItems"],
            "quantity": page["Quantity"],
            "is_truncated": page["IsTruncated"],
            "next_marker": page.get("NextMarker", ""),
            "ids": [item["Id"] for item in page.get("Items", [])],
        }
    return report
"#;

/// The `MaxItems` a `ListDistributions` that named none is served at: "CloudFront
/// ListDistributions ... if you don't specify MaxItems, the default is 100" — the page size the
/// service supplies for a request that asks for no bound, which the answer reports back.
const DEFAULT_PAGE_BOUND: usize = 100;

/// What one row expects of the handler's report.
enum Expected {
    /// A page of the first `taken` distributions, reporting `max_items` as the bound it was cut
    /// by — the bound the row passed, or the service's own ([`DEFAULT_PAGE_BOUND`]) where the
    /// row passed none.
    Page { max_items: usize, taken: usize },
    /// The SDK carried no request for this call at all.
    NotMade,
    /// CloudFront itself refused the call, with this error code and status.
    Refused(&'static str, u16),
}

/// One row: the `MaxItems` the handler passes, spelled as its SDK call carries it.
struct Row {
    case: &'static str,
    /// The value passed for the bound, or `Value::Null` for a call that passes none.
    bound: Value,
    expected: Expected,
}

/// The rows, differing along the spelling of the bound, its value, its absence, and whether it
/// is a whole number the member can hold at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a bound of one, as a number",
            bound: json!(1),
            expected: Expected::Page {
                max_items: 1,
                taken: 1,
            },
        },
        Row {
            case: "a bound of one, as the decimal text of that number",
            bound: json!("1"),
            expected: Expected::Page {
                max_items: 1,
                taken: 1,
            },
        },
        Row {
            case: "a bound covering every distribution, as a number",
            bound: json!(DISTRIBUTIONS),
            expected: Expected::Page {
                max_items: DISTRIBUTIONS,
                taken: DISTRIBUTIONS,
            },
        },
        Row {
            case: "a bound covering every distribution, as decimal text",
            bound: json!(DISTRIBUTIONS.to_string()),
            expected: Expected::Page {
                max_items: DISTRIBUTIONS,
                taken: DISTRIBUTIONS,
            },
        },
        Row {
            case: "no bound at all",
            bound: Value::Null,
            expected: Expected::Page {
                max_items: DEFAULT_PAGE_BOUND,
                taken: DISTRIBUTIONS,
            },
        },
        Row {
            case: "text that spells no number",
            bound: json!("one"),
            expected: Expected::NotMade,
        },
        Row {
            case: "text spelling a magnitude past the member's range",
            bound: json!(PAST_THE_RANGE),
            expected: Expected::NotMade,
        },
        Row {
            case: "a number that is no whole number",
            bound: json!(1.5),
            expected: Expected::NotMade,
        },
        Row {
            case: "a negative count, which names no page",
            bound: json!("-1"),
            expected: Expected::Refused(INVALID_ARGUMENT, 400),
        },
    ]
}

/// The emulator as it is ASSEMBLED, on one engine: the handler's own SDK call is dispatched
/// back into this same router, so the distributions it pages are the ones this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The suite exercises the services, not SigV4: each setup call carries a credential scope
    // so the router can name the target service, without a computed signature.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One response: the status and the body as text.
struct Answer {
    status: u16,
    body: String,
}

/// One request against a service's own host, with `signing_name` in the credential scope.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Answer {
    let request = ::http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/xml")
        .body(Bytes::from(body.to_owned()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One CloudFront control-plane call.
async fn cloudfront(router: &MultiServiceRouter, method: &str, path: &str, body: &str) -> Answer {
    call(router, "cloudfront", CLOUDFRONT_HOST, method, path, body).await
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: String::from_utf8_lossy(response.body()).into_owned(),
    }
}

/// One Lambda `restJson1` call, answered with the status and the body as JSON.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = match &body {
        Some(body) => Bytes::from(body.to_string()),
        None => Bytes::new(),
    };
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = ::http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", &host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create one distribution serving `index`'s own origin, answered with the id CloudFront minted.
async fn create_distribution(router: &MultiServiceRouter, index: usize) -> String {
    let origin = format!("paging-bound-origin-{index}.s3.{REGION}.amazonaws.com");
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>paging-bound-spellings-{index}</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin><Id>origin-{index}</Id>\
         <DomainName>{origin}</DomainName>\
         <S3OriginConfig><OriginAccessIdentity></OriginAccessIdentity></S3OriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>origin-{index}</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <ForwardedValues><QueryString>false</QueryString>\
         <Cookies><Forward>none</Forward></Cookies></ForwardedValues>\
         <MinTTL>0</MinTTL></DefaultCacheBehavior>\
         <Comment>the distribution a bounded page is cut out of</Comment>\
         <Enabled>true</Enabled></DistributionConfig>"
    );
    let created = cloudfront(router, "POST", &format!("/{API_VERSION}/distribution"), &body).await;
    assert_eq!(
        created.status, 201,
        "setup: CreateDistribution({index}) must succeed, got {} with {}",
        created.status, created.body
    );
    let id = element(&created.body, "Id");
    assert!(
        !id.is_empty(),
        "setup: the created distribution must report an id, got {}",
        created.body
    );
    id
}

/// The distributions `created` names, in the order `ListDistributions` reports them — the order
/// a bounded page is cut out of, read through CloudFront's own API rather than assumed.
///
/// A summary carries the `Id` of its origins beside the distribution's own, so only the ids
/// this suite created are kept, each once.
async fn listing_order(router: &MultiServiceRouter, created: &[String]) -> Vec<String> {
    let listed = cloudfront(router, "GET", &format!("/{API_VERSION}/distribution"), "").await;
    assert_eq!(
        listed.status, 200,
        "setup: ListDistributions must succeed, got {} with {}",
        listed.status, listed.body
    );
    let mut order: Vec<String> = Vec::new();
    for id in elements(&listed.body, "Id") {
        if created.contains(&id) && !order.contains(&id) {
            order.push(id);
        }
    }
    order
}

/// The stack that declares the execution role and the function: the role trusts Lambda and is
/// granted the listing, which is what the handler's own call is judged against.
fn handler_stack() -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ReaderRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                            "Action": "sts:AssumeRole",
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "page-the-distributions",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["cloudfront:ListDistributions"],
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Reader": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["ReaderRole", "Arn"]},
                    "Timeout": 30,
                    "Code": {"ZipFile": HANDLER},
                },
            },
        },
        "Outputs": {
            "FunctionName": {"Value": {"Ref": "Reader"}},
        },
    })
    .to_string()
}

/// Create the stack, wait for `CREATE_COMPLETE` and answer its outputs.
async fn deployed(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = handler_stack();
    let created = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        created.status, 200,
        "setup: CreateStack must be accepted, got {} with {}",
        created.status, created.body
    );
    let looks = WAIT.as_millis() / LOOK.as_millis();
    let mut described = Answer {
        status: 0,
        body: String::new(),
    };
    for _ in 0..looks {
        described = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        if element(&described.body, "StackStatus") == CREATE_COMPLETE {
            return outputs_of(&described.body);
        }
        ::tokio::time::sleep(LOOK).await;
    }
    panic!(
        "setup: the stack must reach {CREATE_COMPLETE}, last reported {}",
        described.body
    );
}

/// Wait until Lambda reports the function `Active` — the state an invocation needs. The span a
/// created function spends coming up is that service's own, so nothing here sleeps a fixed span
/// on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let looks = WAIT.as_millis() / LOOK.as_millis();
    let mut state = String::new();
    for _ in 0..looks {
        let (_status, body) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        state = body["State"].as_str().unwrap_or_default().to_string();
        if state == ACTIVE {
            return;
        }
        ::tokio::time::sleep(LOOK).await;
    }
    panic!("setup: the function {name:?} must reach {ACTIVE}, last reported {state:?}");
}

/// The text values of a JSON array, in order.
fn texts(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| item.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Every row of the table, answered in ONE invocation of the handler.
#[tokio::test]
async fn a_handlers_paging_bound_is_read_in_either_spelling() {
    let router = build_router();
    let mut created: Vec<String> = Vec::new();
    for index in 0..DISTRIBUTIONS {
        created.push(create_distribution(&router, index).await);
    }
    let held = listing_order(&router, &created).await;
    assert_eq!(
        held.len(),
        DISTRIBUTIONS,
        "setup: expected {DISTRIBUTIONS} distributions on the listing, got {held:?}"
    );

    let outputs = deployed(&router).await;
    let function = outputs
        .get("FunctionName")
        .cloned()
        .unwrap_or_else(|| FUNCTION.to_string());
    wait_until_active(&router, &function).await;

    let table = rows();
    let asked: Vec<Value> = table
        .iter()
        .map(|row| json!({"case": row.case, "bound": row.bound}))
        .collect();
    let (status, report) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        Some(json!({"rows": asked})),
    )
    .await;
    assert_eq!(
        status, 200,
        "the invocation must be served, got {status} with {report}"
    );
    assert!(
        report.get("errorMessage").is_none() && report.get("errorType").is_none(),
        "the handler must not fault, got {report}"
    );

    for row in &table {
        let case = row.case;
        let answered = &report[case];
        assert!(
            answered.is_object(),
            "case {case:?}: expected the handler to report this row, got {report}"
        );
        match row.expected {
            Expected::Page { max_items, taken } => {
                let wanted: Vec<String> = held.iter().take(taken).cloned().collect();
                assert_eq!(
                    texts(&answered["ids"]),
                    wanted,
                    "case {case:?}: expected the page to hold {wanted:?}, got {answered}"
                );
                assert_eq!(
                    answered["quantity"],
                    json!(taken),
                    "case {case:?}: expected {taken} on the page, got {answered}"
                );
                let truncated = taken < held.len();
                assert_eq!(
                    answered["is_truncated"],
                    json!(truncated),
                    "case {case:?}: expected IsTruncated {truncated}, got {answered}"
                );
                let next_marker = if truncated {
                    wanted.last().cloned().unwrap_or_default()
                } else {
                    String::new()
                };
                assert_eq!(
                    answered["next_marker"],
                    json!(next_marker),
                    "case {case:?}: expected NextMarker {next_marker:?}, got {answered}"
                );
                // The bound the page reports is the one it was cut by: the row's own where it
                // passed one, and the size the service pages at where it passed none.
                assert_eq!(
                    answered["max_items"],
                    json!(max_items),
                    "case {case:?}: expected the page to report MaxItems {max_items}, got \
                     {answered}"
                );
            }
            Expected::NotMade => {
                assert!(
                    answered["not_made"].is_string(),
                    "case {case:?}: expected a call the SDK made no request for, got {answered}"
                );
            }
            Expected::Refused(code, status) => {
                assert_eq!(
                    answered["code"],
                    json!(code),
                    "case {case:?}: expected the service's own {code}, got {answered}"
                );
                assert_eq!(
                    answered["status"],
                    json!(status),
                    "case {case:?}: expected HTTP {status}, got {answered}"
                );
            }
        }
    }

    // The INVARIANT the rule states, over the pairs that spell one bound two ways: the number
    // and its decimal text ask for the same call, so the page answered is the same page.
    for (number, text) in [
        (
            "a bound of one, as a number",
            "a bound of one, as the decimal text of that number",
        ),
        (
            "a bound covering every distribution, as a number",
            "a bound covering every distribution, as decimal text",
        ),
    ] {
        assert_eq!(
            report[number], report[text],
            "case {number:?} against {text:?}: expected one page for both spellings, got {} and \
             {}",
            report[number], report[text]
        );
    }
}
