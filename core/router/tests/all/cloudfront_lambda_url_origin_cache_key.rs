// SPDX-License-Identifier: CC0-1.0

//! What a distribution's CACHE KEY is made of over a LAMBDA FUNCTION URL origin, end to end
//! over HTTP across the services on one engine: a viewer dials the address the distribution
//! published, the edge fetches a running function's URL, and the function COUNTS every origin
//! request it serves in its own Amazon DynamoDB table.
//!
//! RULE: the cache key of a request carries exactly the query strings the matched cache
//! behaviour's CACHE POLICY names — `CachePolicyQueryStringsConfig$QueryStringBehavior`
//! (`smithy_query(query="shape",shape="CachePolicyQueryStringsConfig")`): `none` names none of
//! them, `whitelist` the listed names, `allExcept` every name but the listed ones, `all` the
//! query string whole — so two viewer requests that differ only in a query string the policy
//! names are two objects, each fetched from the origin and answered
//! `X-Cache: Miss from cloudfront`, while two that differ only in a query string the policy
//! does NOT name are one object, the second answered from the cache
//! (`X-Cache: Hit from cloudfront`) with the first one's bytes and no further origin request.
//! And the ORIGIN request carries the union of the query strings the cache key names and the
//! ones the behaviour's ORIGIN REQUEST POLICY forwards
//! (`OriginRequestPolicyQueryStringsConfig$QueryStringBehavior` — "Even when this field is set
//! to `none`, any query strings that are listed in a `CachePolicy` *are* included in origin
//! requests"), and nothing else: a parameter the cache key EXCLUDES still reaches the origin
//! where the origin request policy names it, and still cannot separate two cache entries.
//!
//! Only a cross-service suite can pin that over this topology: the behaviour and its two
//! policies are CloudFront's, the origin is a RUNNING Lambda function published at a function
//! URL, and "the origin ran for this answer" is a fact of the counters the function itself
//! bumps in DynamoDB — so a hit that silently re-fetched the origin, and a miss that never
//! reached it, both fail here. The cloudfront crate's own `edge_request_shaping` suite pins the
//! same rule against an origin this emulator's control plane serves, where no origin-side
//! record of the fetch exists.
//!
//! Each of the two rules is one test with its own staged origin and its own distribution — a
//! cache behaviour per row, each on its own path — so a rule is asserted over many
//! configurations while the services are staged once per rule and the two run side by side. The
//! rows differ along what the rules generalise over: every value
//! `CachePolicyQueryStringBehavior` admits, both directions a value decides between (a repeat
//! differing in a parameter the policy names, one differing in a parameter it does not), the
//! ORDER the same parameters were sent in (which `all` keys on), every value
//! `OriginRequestPolicyQueryStringBehavior` admits, and a behaviour that names no origin request
//! policy at all.
//!
//! The staging mirrors [`super::cloudfront_edge_generated_headers`]'s function-URL half (a
//! CloudFormation stack declaring the counter table, the execution role and the python handler,
//! then the function URL and the grant that makes it public); the wire harness itself is
//! [`super::cloudfront_apigateway_origin_edge`]'s, imported rather than repeated.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::cloudfront_apigateway_origin_edge::{
    authorization, cloudfront, dispatch, member, view, Answer, API_VERSION, REGION,
};
use crate::query_wire::form_encode;

/// `X-Cache` for an answer the origin produced for this request, and for one the edge cache
/// held.
const MISS: &str = "Miss from cloudfront";
const HIT: &str = "Hit from cloudfront";

/// The stack that declares the origin, the function it declares, the role that function runs
/// as and the table it counts its origin requests in.
const STACK: &str = "cloudfront-cache-key-origin-stack";
const FUNCTION: &str = "cloudfront-cache-key-origin";
const ROLE: &str = "cloudfront-cache-key-origin-role";
const TABLE: &str = "cloudfront-cache-key-origin-counter";

/// The table's key attribute and the attribute holding a path's count: the function counts
/// per `rawPath`, so each row's own path carries its own count.
const PATH_ATTRIBUTE: &str = "path";
const COUNT_ATTRIBUTE: &str = "seen";

/// The marker every answer of the function's own carries, so an answer that carries it came
/// from the origin (or from the cache holding the origin's bytes) rather than from CloudFront.
const MARKER: &str = "CACHE-KEY-ECHO";

/// The `StackStatus` the setup waits for.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because nothing here measures how fast the interpreter gets through the
/// handler.
const FUNCTION_TIMEOUT: u64 = 300;

/// The TTLs every cache policy below publishes. Long enough that no answer can expire between
/// the requests of a row, so what decides a hit is the cache KEY and never the clock — and
/// nothing waits for this span to pass.
const CACHE_TTL: u64 = 3600;

/// The `OriginReadTimeout` the origin carries: `CustomOriginConfig$OriginReadTimeout`'s own
/// documented maximum ("the minimum timeout is 1 second, the maximum is 120 seconds"), so how
/// fast the host runs the origin's handler does not decide whether the edge got an answer.
const ORIGIN_READ_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// `CREATE_COMPLETE`, a function reaching `Active`, a distribution reaching `Deployed` — and
/// how many looks to take. The spans are those services' own, so no fixed sleep names the
/// instant one ends, and the deadline is generous enough that a loaded host cannot decide the
/// outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 2_000;

/// The handler behind the function URL: it COUNTS the origin request it is serving under the
/// `rawPath` it arrived on, with the AWS SDK its runtime supplies, and reports the path and the
/// RAW QUERY STRING that request carried — which is what says which parameters reached the
/// origin at all.
const ECHO_HANDLER: &str = r#"
import json
import os

import boto3

CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    path = event.get("rawPath", "")
    CLIENT.update_item(
        TableName=os.environ["TABLE"],
        Key={"__PATH_ATTRIBUTE__": {"S": path}},
        UpdateExpression="ADD __COUNT_ATTRIBUTE__ :one",
        ExpressionAttributeValues={":one": {"N": "1"}},
    )
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "marker": "__MARKER__",
                "path": path,
                "query": event.get("rawQueryString", ""),
            }
        ),
    }
"#;

/// The WHOLE emulator as it is assembled, on one engine: CloudFormation, IAM, Lambda,
/// DynamoDB and CloudFront all have to agree here, and the function the edge fetches is
/// invoked over this same in-process transport.
fn assembled() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in the sibling suites; the credential scope is
    // what routes a request.
    router.set_request_authentication(false);
    router
}

/// One awsQuery call of `service`, answered with the status and the XML body.
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
    dispatch(
        router,
        "POST",
        &host,
        &format!("https://{host}/"),
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization(service)),
        ],
        Bytes::from(body),
    )
    .await
}

/// One AWS Lambda `restJson1` call that has to be accepted with `expected`.
async fn lambda(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
        router,
        method,
        &host,
        &format!("https://{host}{path}"),
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One Amazon DynamoDB `awsJson1_0` call.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: Value) -> Answer {
    let host = format!("dynamodb.{REGION}.amazonaws.com");
    dispatch(
        router,
        "POST",
        &host,
        &format!("https://{host}/"),
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("authorization", &authorization("dynamodb")),
            ("x-amz-target", &format!("DynamoDB_20120810.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// How many origin requests the function has counted under `path`, read through DynamoDB's own
/// strongly consistent `GetItem` — `0` for a path the origin was never asked for.
async fn origin_requests(router: &MultiServiceRouter, case: &str, path: &str) -> u64 {
    let read = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": TABLE,
            "Key": { PATH_ATTRIBUTE: { "S": path } },
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetItem on the origin's counter must succeed, got {} with {:?}",
        read.status,
        read.text()
    );
    let answered = read.json();
    // A path the origin was never asked for carries no item at all, which is zero; an item
    // whose count cannot be read as a number is a different problem and says so.
    let Some(counter) = answered.get("Item").and_then(|item| item.get(COUNT_ATTRIBUTE)) else {
        return 0;
    };
    counter
        .get("N")
        .and_then(Value::as_str)
        .and_then(|counted| counted.parse::<u64>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the origin's counter for {path:?} is no number: {counter}")
        })
}

/// The HOST of a PUBLIC function URL over [`ECHO_HANDLER`]: the stack that declares the table,
/// the execution role and the function, then the URL and the grant that admits the anonymous
/// caller an unsigned origin fetch is.
async fn staged_function_url(router: &MultiServiceRouter) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Counter": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": TABLE,
                    "AttributeDefinitions": [
                        {"AttributeName": PATH_ATTRIBUTE, "AttributeType": "S"},
                    ],
                    "KeySchema": [{"AttributeName": PATH_ATTRIBUTE, "KeyType": "HASH"}],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            },
            "EchoRole": {
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
                        "PolicyName": "count-its-own-origin-requests",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Effect": "Allow",
                                "Action": ["dynamodb:UpdateItem"],
                                "Resource": {"Fn::GetAtt": ["Counter", "Arn"]},
                            }],
                        },
                    }],
                },
            },
            "Echo": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": {"Fn::GetAtt": ["EchoRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {"TABLE": {"Ref": "Counter"}}},
                    "Code": {"ZipFile": ECHO_HANDLER
                        .replace("__PATH_ATTRIBUTE__", PATH_ATTRIBUTE)
                        .replace("__COUNT_ATTRIBUTE__", COUNT_ATTRIBUTE)
                        .replace("__MARKER__", MARKER)},
                },
            },
        },
    })
    .to_string();
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
        created.status,
        200,
        "setup: CreateStack must be accepted, got {} with {:?}",
        created.status,
        created.text()
    );
    // The stack's completion is owed from the instant its resources are created, which no
    // fixed sleep names: this looks until it is reported.
    let mut reported = String::new();
    for _ in 0..LOOKS {
        let described = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStacks",
            &[("StackName", STACK)],
        )
        .await;
        reported = described.element("StackStatus");
        if reported == CREATE_COMPLETE {
            break;
        }
        ::tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the origin's stack must reach {CREATE_COMPLETE}, last reported {reported:?}"
    );
    // An invocation needs the function Active, which is another span the service owes.
    let mut state = String::new();
    for _ in 0..LOOKS {
        let configuration = lambda(
            router,
            "the origin function",
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
            200,
        )
        .await;
        state = configuration["State"].as_str().unwrap_or_default().to_string();
        if state == "Active" {
            break;
        }
        ::tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        state, "Active",
        "setup: the origin function must reach Active, last reported {state:?}"
    );
    let url = lambda(
        router,
        "the origin's function URL",
        "POST",
        &format!("/2021-10-31/functions/{FUNCTION}/url"),
        Some(json!({ "AuthType": "NONE" })),
        201,
    )
    .await;
    // `AuthType=NONE` leaves IAM out of it; the function's own resource policy is what admits
    // the anonymous caller the edge's unsigned origin fetch is.
    lambda(
        router,
        "the grant on the function URL",
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        })),
        201,
    )
    .await;
    let endpoint = member("CreateFunctionUrlConfig", &url, "FunctionUrl");
    endpoint
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string()
}

/// The `QueryStrings` element a policy carries for `listed`, and nothing at all for a
/// behaviour that lists no name.
fn query_string_names(listed: &[&str]) -> String {
    if listed.is_empty() {
        return String::new();
    }
    let items: String = listed.iter().map(|name| format!("<Name>{name}</Name>")).collect();
    format!(
        "<QueryStrings><Quantity>{}</Quantity><Items>{items}</Items></QueryStrings>",
        listed.len()
    )
}

/// Create a cache policy whose query-string behaviour is `behavior` over `listed`, with every
/// TTL at [`CACHE_TTL`] and no header or cookie in the key, and answer its minted id.
async fn create_cache_policy(
    router: &MultiServiceRouter,
    name: &str,
    behavior: &str,
    listed: &[&str],
) -> String {
    let body = format!(
        "<CachePolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name><DefaultTTL>{CACHE_TTL}</DefaultTTL><MaxTTL>{CACHE_TTL}</MaxTTL>\
         <MinTTL>{CACHE_TTL}</MinTTL>\
         <ParametersInCacheKeyAndForwardedToOrigin>\
         <EnableAcceptEncodingGzip>false</EnableAcceptEncodingGzip>\
         <HeadersConfig><HeaderBehavior>none</HeaderBehavior></HeadersConfig>\
         <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>{behavior}</QueryStringBehavior>{}\
         </QueryStringsConfig>\
         </ParametersInCacheKeyAndForwardedToOrigin>\
         </CachePolicyConfig>",
        query_string_names(listed)
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/cache-policy"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateCachePolicy({name:?}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let id = created.element("Id");
    assert!(
        !id.is_empty(),
        "setup: the created cache policy {name:?} reports no Id: {:?}",
        created.text()
    );
    id
}

/// Create an origin request policy whose query-string behaviour is `behavior` over `listed`,
/// and answer its minted id.
async fn create_origin_request_policy(
    router: &MultiServiceRouter,
    name: &str,
    behavior: &str,
    listed: &[&str],
) -> String {
    let body = format!(
        "<OriginRequestPolicyConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <Name>{name}</Name>\
         <HeadersConfig><HeaderBehavior>none</HeaderBehavior></HeadersConfig>\
         <CookiesConfig><CookieBehavior>none</CookieBehavior></CookiesConfig>\
         <QueryStringsConfig><QueryStringBehavior>{behavior}</QueryStringBehavior>{}\
         </QueryStringsConfig>\
         </OriginRequestPolicyConfig>",
        query_string_names(listed)
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/origin-request-policy"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateOriginRequestPolicy({name:?}) must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let id = created.element("Id");
    assert!(
        !id.is_empty(),
        "setup: the created origin request policy {name:?} reports no Id: {:?}",
        created.text()
    );
    id
}

/// One ordered cache behaviour of the distribution the tables drive: the path pattern it
/// matches, the cache policy it keys on and the origin request policy it forwards through
/// (`None` for a behaviour that names none).
struct Behavior {
    pattern: String,
    cache_policy: String,
    origin_request_policy: Option<String>,
}

/// One distribution over the function URL `origin`, with one ordered cache behaviour per row
/// of the two tables — so every row has its own path, its own policies and its own cache
/// entries, and one deployment serves them all. Answers the `DomainName` it published.
async fn deployed_distribution(
    router: &MultiServiceRouter,
    origin: &str,
    default_cache_policy: &str,
    behaviors: &[Behavior],
) -> String {
    let items: String = behaviors
        .iter()
        .map(|behavior| {
            let forwarding = behavior
                .origin_request_policy
                .as_ref()
                .map(|id| format!("<OriginRequestPolicyId>{id}</OriginRequestPolicyId>"))
                .unwrap_or_default();
            format!(
                "<CacheBehavior><PathPattern>{}</PathPattern>\
                 <TargetOriginId>function-url</TargetOriginId>\
                 <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
                 <CachePolicyId>{}</CachePolicyId>{forwarding}</CacheBehavior>",
                behavior.pattern, behavior.cache_policy
            )
        })
        .collect();
    let body = format!(
        "<DistributionConfig xmlns=\"http://cloudfront.amazonaws.com/doc/{API_VERSION}/\">\
         <CallerReference>cloudfront-cache-key-over-a-function-url</CallerReference>\
         <Origins><Quantity>1</Quantity><Items><Origin>\
         <Id>function-url</Id><DomainName>{origin}</DomainName>\
         <CustomOriginConfig><HTTPPort>80</HTTPPort><HTTPSPort>443</HTTPSPort>\
         <OriginProtocolPolicy>https-only</OriginProtocolPolicy>\
         <OriginReadTimeout>{ORIGIN_READ_TIMEOUT}</OriginReadTimeout>\
         </CustomOriginConfig>\
         </Origin></Items></Origins>\
         <DefaultCacheBehavior><TargetOriginId>function-url</TargetOriginId>\
         <ViewerProtocolPolicy>allow-all</ViewerProtocolPolicy>\
         <CachePolicyId>{default_cache_policy}</CachePolicyId></DefaultCacheBehavior>\
         <CacheBehaviors><Quantity>{}</Quantity><Items>{items}</Items></CacheBehaviors>\
         <Comment>a function url origin behind a cache key</Comment>\
         <Enabled>true</Enabled>\
         </DistributionConfig>",
        behaviors.len()
    );
    let created = cloudfront(
        router,
        "POST",
        &format!("/{API_VERSION}/distribution"),
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateDistribution must succeed, got {} with {:?}",
        created.status,
        created.text()
    );
    let (id, domain) = (created.element("Id"), created.element("DomainName"));
    assert!(
        !id.is_empty() && !domain.is_empty(),
        "setup: the created distribution reports no Id or DomainName: {:?}",
        created.text()
    );
    // A configuration that has not reached the edge locations serves no viewer: look until
    // CloudFront reports it Deployed, which is a span the service owes.
    let mut reported = String::new();
    for _ in 0..LOOKS {
        let read = cloudfront(
            router,
            "GET",
            &format!("/{API_VERSION}/distribution/{id}"),
            Bytes::new(),
        )
        .await;
        reported = read.element("Status");
        if reported == "Deployed" {
            break;
        }
        ::tokio::time::sleep(LOOK).await;
    }
    assert_eq!(
        reported, "Deployed",
        "setup: the distribution must reach Deployed, last reported {reported:?}"
    );
    domain
}

/// Read the RAW QUERY STRING the function reported for the origin request it served, holding
/// the answer to being the origin's own document asked for `path`.
fn reported_query(case: &str, leg: &str, path: &str, answer: &Answer) -> String {
    let body = answer.json();
    assert_eq!(
        body.get("marker").and_then(Value::as_str),
        Some(MARKER),
        "case {case:?} ({leg}): expected the origin's own document, got {:?}",
        answer.text()
    );
    assert_eq!(
        body.get("path").and_then(Value::as_str),
        Some(path),
        "case {case:?} ({leg}): expected the origin to be asked for {path:?}, got {:?}",
        answer.text()
    );
    body.get("query")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `name=value` pairs of a raw query string, sorted — so an assertion is about WHICH
/// parameters arrived and not about the order they were written in.
fn pairs(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = raw
        .trim_start_matches('?')
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(str::to_string)
        .collect();
    out.sort();
    out
}

/// The pairs of `viewer_query` whose NAME is one of `forwarded`: what the origin must receive
/// of a viewer's query string, taken from the request rather than written out per row. The
/// order is [`pairs`]'s.
fn only_named(viewer_query: &str, forwarded: &[&str]) -> Vec<String> {
    pairs(viewer_query)
        .into_iter()
        .filter(|pair| {
            let name = pair.split('=').next().unwrap_or_default();
            forwarded.contains(&name)
        })
        .collect()
}

/// One row of the cache-key table: the cache policy's behaviour, the two viewer requests, and
/// what the second of them must be answered with.
struct KeyRow {
    case: &'static str,
    behavior: &'static str,
    listed: &'static [&'static str],
    first: &'static str,
    second: &'static str,
    /// The `X-Cache` the SECOND request must carry.
    second_x_cache: &'static str,
    /// How many origin requests the function must have counted for this row's path once both
    /// requests have been served.
    origin_requests: u64,
}

/// Every value `CachePolicyQueryStringBehavior` admits, each over the two directions that value
/// decides between: a repeat differing in a parameter the policy names (another object) and one
/// differing in a parameter it does not (the same object). `all` is asked the question its own
/// wording raises, the same parameters in another order.
const KEY_ROWS: &[KeyRow] = &[
    KeyRow {
        case: "none keys on no query string, so another value is the same object",
        behavior: "none",
        listed: &[],
        first: "?a=1",
        second: "?a=2",
        second_x_cache: HIT,
        origin_requests: 1,
    },
    KeyRow {
        case: "whitelist keys on a listed parameter",
        behavior: "whitelist",
        listed: &["a"],
        first: "?a=1&b=1",
        second: "?a=2&b=1",
        second_x_cache: MISS,
        origin_requests: 2,
    },
    KeyRow {
        case: "allExcept keys on nothing it lists",
        behavior: "allExcept",
        listed: &["b"],
        first: "?a=1&b=1",
        second: "?a=1&b=2",
        second_x_cache: HIT,
        origin_requests: 1,
    },
    KeyRow {
        // The query string takes part in the key as TEXT, in the order the viewer sent it:
        // the origin reads it as text, and a reordered one is another request to anything
        // that parses it positionally. The crate's own `edge/forwarding.rs` keys it the same
        // way (`PairOrder::AsSent`), and the cloudfront crate's `edge_request_shaping` suite
        // states the same row, so a future reader has two statements of it rather than one.
        case: "all keys on the query string as the viewer sent it, order and all",
        behavior: "all",
        listed: &[],
        first: "?a=1&b=2",
        second: "?b=2&a=1",
        second_x_cache: MISS,
        origin_requests: 2,
    },
];

/// The parameter the forwarding table's cache policy keys on, and the one it EXCLUDES from the
/// cache key — the parameter whose forwarding is what each row differs about.
const KEYED: &str = "sku";
const EXCLUDED: &str = "token";

/// One row of the forwarding table: the origin request policy the behaviour names (`None` for
/// one that names none) and the query-string NAMES the origin must then receive.
struct ForwardRow {
    case: &'static str,
    /// The origin request policy's behaviour and the names it lists.
    policy: Option<(&'static str, &'static [&'static str])>,
    forwarded: &'static [&'static str],
    /// Whether this row also asks the other direction: a request differing in the parameter the
    /// cache key NAMES, which is another object and reaches the origin again. One row carries
    /// it, so the table states it without an extra origin fetch per row.
    asks_another_cache_key: bool,
}

/// Every value `OriginRequestPolicyQueryStringBehavior` admits, and the behaviour that names no
/// origin request policy at all — against one cache policy that keys on everything but
/// [`EXCLUDED`], so what a row differs about is the forwarding alone.
const FORWARD_ROWS: &[ForwardRow] = &[
    ForwardRow {
        case: "a behaviour naming no origin request policy sends only the cache key's own",
        policy: None,
        forwarded: &[KEYED],
        asks_another_cache_key: true,
    },
    ForwardRow {
        case: "whitelist forwards the parameter it lists, which the cache key excludes",
        policy: Some(("whitelist", &[EXCLUDED])),
        forwarded: &[KEYED, EXCLUDED],
        asks_another_cache_key: false,
    },
    ForwardRow {
        case: "all forwards every parameter the viewer sent",
        policy: Some(("all", &[])),
        forwarded: &[KEYED, EXCLUDED],
        asks_another_cache_key: false,
    },
    ForwardRow {
        case: "allExcept withholds the parameter it lists, leaving the cache key's own",
        policy: Some(("allExcept", &[EXCLUDED])),
        forwarded: &[KEYED],
        asks_another_cache_key: false,
    },
    ForwardRow {
        case: "none forwards none of its own, leaving the cache key's own",
        policy: Some(("none", &[])),
        forwarded: &[KEYED],
        asks_another_cache_key: false,
    },
];

/// RULE, the first half of this suite's header: the cache key carries exactly the query strings
/// the matched behaviour's CACHE POLICY names, so a repeat the policy says is the same object is
/// served from the cache with the first answer's bytes and no origin request, and one it says is
/// another object is fetched from the origin again.
#[tokio::test]
async fn the_cache_policys_query_strings_decide_a_function_url_origins_cache_key() {
    let router = assembled();
    let origin = staged_function_url(&router).await;

    // One behaviour per row, each on its own path and its own cache policy, so no row reads
    // another row's cached answer or another row's counter.
    let mut behaviors = Vec::new();
    for (index, row) in KEY_ROWS.iter().enumerate() {
        let policy = create_cache_policy(
            &router,
            &format!("cache-key-{index}"),
            row.behavior,
            row.listed,
        )
        .await;
        behaviors.push(Behavior {
            pattern: format!("/key-{index}/*"),
            cache_policy: policy,
            origin_request_policy: None,
        });
    }
    // The default behaviour no row's path matches, which every distribution carries.
    let default_policy = create_cache_policy(&router, "cache-key-default", "none", &[]).await;
    let domain = deployed_distribution(&router, &origin, &default_policy, &behaviors).await;

    for (index, row) in KEY_ROWS.iter().enumerate() {
        let case = row.case;
        let path = format!("/key-{index}/object");

        let first = view(
            &router,
            "https",
            &domain,
            &format!("{path}{}", row.first),
            &[],
        )
        .await;
        assert_eq!(
            (first.status, first.header("x-cache")),
            (200, MISS.to_string()),
            "case {case:?}: the first request must be fetched from the origin, got {} with \
             X-Cache {:?} ({:?})",
            first.status,
            first.header("x-cache"),
            first.text()
        );
        let _ = reported_query(case, "the first request", &path, &first);
        let after_the_first = origin_requests(&router, case, &path).await;
        assert_eq!(
            after_the_first, 1,
            "case {case:?}: the origin must have counted exactly the one request it served, got \
             {after_the_first}"
        );

        let second = view(
            &router,
            "https",
            &domain,
            &format!("{path}{}", row.second),
            &[],
        )
        .await;
        assert_eq!(
            (second.status, second.header("x-cache")),
            (200, row.second_x_cache.to_string()),
            "case {case:?}: {:?} after {:?} expected X-Cache {:?}, got {} with {:?} ({:?})",
            row.second,
            row.first,
            row.second_x_cache,
            second.status,
            second.header("x-cache"),
            second.text()
        );
        let counted = origin_requests(&router, case, &path).await;
        assert_eq!(
            counted, row.origin_requests,
            "case {case:?}: expected the origin to have served {} request(s) after both, got \
             {counted}",
            row.origin_requests
        );
        if row.second_x_cache == HIT {
            // A hit is served the bytes the origin produced for the FIRST request.
            assert_eq!(
                second.text(),
                first.text(),
                "case {case:?}: a hit must carry the answer the origin produced for {:?}, got \
                 {:?}",
                row.first,
                second.text()
            );
        } else {
            let _ = reported_query(case, "the second request", &path, &second);
        }
    }
}

/// RULE, the second half of this suite's header: the origin request carries the union of the
/// query strings the cache key names and the ones the behaviour's ORIGIN REQUEST POLICY
/// forwards, and nothing else — while a parameter the cache key excludes still cannot separate
/// two cache entries, whatever that policy forwards.
#[tokio::test]
async fn an_origin_request_policy_forwards_what_a_function_url_origins_cache_key_excludes() {
    let router = assembled();
    let origin = staged_function_url(&router).await;

    // ONE cache policy for every row: it keys on everything but EXCLUDED, so what a row differs
    // about is the forwarding alone. One behaviour per row, each on its own path.
    let forwarding_key =
        create_cache_policy(&router, "forwarding-key", "allExcept", &[EXCLUDED]).await;
    let mut behaviors = Vec::new();
    for (index, row) in FORWARD_ROWS.iter().enumerate() {
        let policy = match row.policy {
            Some((behavior, listed)) => Some(
                create_origin_request_policy(
                    &router,
                    &format!("forwarding-{index}"),
                    behavior,
                    listed,
                )
                .await,
            ),
            None => None,
        };
        behaviors.push(Behavior {
            pattern: format!("/fwd-{index}/*"),
            cache_policy: forwarding_key.clone(),
            origin_request_policy: policy,
        });
    }
    let default_policy = create_cache_policy(&router, "forwarding-default", "none", &[]).await;
    let domain = deployed_distribution(&router, &origin, &default_policy, &behaviors).await;

    // The viewer requests of a row: one, a repeat differing only in the EXCLUDED parameter
    // (one object with the first, so no origin request), and — for the row that asks it — one
    // differing in the KEYED parameter, which is another object.
    let first_query = format!("?{KEYED}=1&{EXCLUDED}=A");
    let repeat_query = format!("?{KEYED}=1&{EXCLUDED}=B");
    let other_query = format!("?{KEYED}=2&{EXCLUDED}=B");

    for (index, row) in FORWARD_ROWS.iter().enumerate() {
        let case = row.case;
        let path = format!("/fwd-{index}/object");

        let first = view(
            &router,
            "https",
            &domain,
            &format!("{path}{first_query}"),
            &[],
        )
        .await;
        assert_eq!(
            (first.status, first.header("x-cache")),
            (200, MISS.to_string()),
            "case {case:?}: the first request must be fetched from the origin, got {} with \
             X-Cache {:?} ({:?})",
            first.status,
            first.header("x-cache"),
            first.text()
        );
        let served = reported_query(case, "the first request", &path, &first);
        assert_eq!(
            pairs(&served),
            only_named(&first_query, row.forwarded),
            "case {case:?}: expected the origin to receive {:?}, got {:?}",
            only_named(&first_query, row.forwarded),
            pairs(&served)
        );
        let after_the_first = origin_requests(&router, case, &path).await;
        assert_eq!(
            after_the_first, 1,
            "case {case:?}: the origin must have counted exactly the one request it served, got \
             {after_the_first}"
        );

        // The parameter the cache key excludes cannot separate two entries, whatever the
        // origin request policy forwards: this repeat is the FIRST answer, cache-held.
        let repeat = view(
            &router,
            "https",
            &domain,
            &format!("{path}{repeat_query}"),
            &[],
        )
        .await;
        assert_eq!(
            (repeat.status, repeat.header("x-cache")),
            (200, HIT.to_string()),
            "case {case:?}: {repeat_query:?} differs from {first_query:?} only in the \
             parameter the cache key excludes, so it must be served from the cache, got {} \
             with X-Cache {:?} ({:?})",
            repeat.status,
            repeat.header("x-cache"),
            repeat.text()
        );
        assert_eq!(
            repeat.text(),
            first.text(),
            "case {case:?}: the cached answer must be the one the origin produced for \
             {first_query:?}, got {:?}",
            repeat.text()
        );
        let after_the_hit = origin_requests(&router, case, &path).await;
        assert_eq!(
            after_the_hit, 1,
            "case {case:?}: a cache hit must make no origin request, got {after_the_hit} counted"
        );

        // One row also asks the other direction: the parameter the cache key DOES name is
        // another object, fetched from the origin under the same forwarding union.
        if row.asks_another_cache_key {
            let other = view(
                &router,
                "https",
                &domain,
                &format!("{path}{other_query}"),
                &[],
            )
            .await;
            assert_eq!(
                (other.status, other.header("x-cache")),
                (200, MISS.to_string()),
                "case {case:?}: {other_query:?} differs in the parameter the cache key names, \
                 so it must be fetched from the origin, got {} with X-Cache {:?} ({:?})",
                other.status,
                other.header("x-cache"),
                other.text()
            );
            let served = reported_query(case, "the request on another cache key", &path, &other);
            assert_eq!(
                pairs(&served),
                only_named(&other_query, row.forwarded),
                "case {case:?}: expected the origin to receive {:?}, got {:?}",
                only_named(&other_query, row.forwarded),
                pairs(&served)
            );
            let after_the_other = origin_requests(&router, case, &path).await;
            assert_eq!(
                after_the_other, 2,
                "case {case:?}: the origin must have served the second cache key's request \
                 too, got {after_the_other} counted"
            );
        }
    }
}
