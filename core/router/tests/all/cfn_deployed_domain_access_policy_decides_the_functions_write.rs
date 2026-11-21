// SPDX-License-Identifier: CC0-1.0

//! The deploy-then-restrict-then-restore trajectory of an Amazon OpenSearch Service domain's
//! own access policy, run end to end over a CloudFormation-deployed estate.
//!
//! RULE: a domain's `AccessPolicies` document is a RESOURCE-based policy, so for a principal
//! that carries no identity-based grant of this service it is the whole of what decides a
//! SigV4-signed data-plane request — the request is served while a statement of the document
//! in force covers the principal that signed, the `es:ESHttp*` action its HTTP method asks
//! for and the domain path it addresses; replacing the document through `UpdateDomainConfig`
//! with one that no longer covers it refuses the identical request `403` with AWS's own
//! sentence and stores NOTHING, so the id the refused write addressed is still absent; and
//! putting the covering document back serves the same request again.
//!
//! Its second half is what the deployment owes for that trajectory to be reachable at all:
//! a stack whose resources span four services settles in `CREATE_COMPLETE`, each resource
//! really exists in the service that OWNS it, and the domain publishes the endpoint the
//! function signs its request to — so a client gets as far as its first signed write instead
//! of polling a stack that never settles.
//!
//! Six crates have to agree for that to hold (CloudFormation deploys the estate, IAM holds
//! the execution role, Amazon S3 holds the bucket beside it, Lambda runs the handler, STS
//! mints the session the handler's own request is signed as, and Amazon OpenSearch Service
//! takes the decision and serves the write), so the suite lives here rather than in any one
//! service's crate.
//!
//! The rows differ along WHICH element of the statement stops covering the function's
//! request — the `Principal` list it is no longer in, the `Action` narrowed past the one its
//! method names, the `Resource` narrowed to another index's paths, and an explicit `Deny`
//! added beside an `Allow` that still names it — over two of the methods this surface reads
//! an action from, so the action and the rule in the refusal are the request's own and not a
//! fixed string. The recorded case — the policy narrowed to the deployer alone, read through
//! a `PUT /<index>/_doc/<id>` — is one row of that table.
//!
//! Every wait here is for a CONDITION and never for a span: the stack reaching a terminal
//! status, the function reaching `Active`, the domain reporting nothing in flight, and Lambda
//! having room to serve a probe's invocation at all. Each is looked at again until it holds or
//! a generous deadline passes, so which writes the domain admits cannot be decided by how
//! fast or how loaded this host happens to be.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element, outputs_of};

/// The Region every request here is signed for, and so the Region every resource of the
/// deployed stack and every ARN below belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The one stack the whole estate is deployed as.
const STACK: &str = "osap-trajectory-stack";

/// The domain the stack declares, inside `DomainName`'s own `^[a-z][a-z0-9\-]+$` and
/// 3..28 characters.
const DOMAIN: &str = "osap-trajectory";

/// The bucket the stack declares beside it — the estate the recorded trajectory deploys
/// carries one, and a stack that reports it created is only right if Amazon S3 really holds
/// it.
const BUCKET: &str = "osap-trajectory-landing";

/// The execution role the stack declares. It carries NO identity-based grant of this service,
/// which is the position a role reaching a domain through that domain's access policy alone is
/// in: the document under test is the whole of what covers its requests.
const ROLE: &str = "osap-trajectory-indexer";

/// The function the stack declares, which is also the name Lambda opens the execution role's
/// session under — so the principal the domain decides about is
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
const FUNCTION: &str = "osap-trajectory-writer";

/// The index every write of this suite addresses.
const INDEX: &str = "catalog";

/// Another index of the same domain, which one row's `Resource` names instead.
const OTHER_INDEX: &str = "archive";

/// The engine release the deployed domain runs: one the owning service's catalogue offers, so
/// the only thing a row can fail on is the access policy under test.
const ENGINE_VERSION: &str = "OpenSearch_2.11";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function has to be in to take an invocation: the model documents the
/// provisioning window a newly created function passes through first, and `Invoke` refuses a
/// call that arrives inside it.
const STATE_ACTIVE: &str = "Active";

/// How often to look while waiting for work the clock owes, and how long to keep looking
/// before a wait is a failure. The spans waited on are those services' own, so nothing here
/// sleeps for a fixed span expecting work to be done, and the deadline is generous so a loaded
/// host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const DEADLINE: Duration = Duration::from_secs(240);

/// The statuses Lambda answers an invocation it has NOT served with, each of which says "not
/// now" and carries no decision about the write: no room among the account's concurrent
/// executions (`TooManyRequestsException`, `@httpError(429)`), or the function not being
/// `Active` yet (`ResourceConflictException`, `@httpError(409)`).
///
/// Whether one arrives is decided by what the rest of this process is running at that instant
/// and by nothing a row varies, so a probe LOOKS AGAIN on one instead of reading an
/// authorization decision out of a run that never happened. Nothing is tolerated by looking
/// again: a refusal that still stands at the deadline fails the row.
const UNSERVED: &[u16] = &[409, 429];

/// The handler every probe runs: it SIGNS a document write of its own with the credentials the
/// runtime hands it and reports the status the domain answered together with the body, so a
/// refusal is the probe's answer rather than an exception.
const WRITER_HANDLER: &str = r#"
import json
import os
import urllib.error
import urllib.request

import botocore.session
from botocore.auth import SigV4Auth
from botocore.awsrequest import AWSRequest


def handler(event, context):
    endpoint = os.environ["SEARCH_ENDPOINT"]
    url = "https://" + endpoint + "/" + event["index"] + "/_doc/" + event["id"]
    body = json.dumps(event["document"]).encode("utf-8")
    signed = AWSRequest(
        method=event["method"],
        url=url,
        data=body,
        headers={"Content-Type": "application/json"},
    )
    credentials = botocore.session.get_session().get_credentials()
    SigV4Auth(credentials, "es", os.environ["AWS_REGION"]).add_auth(signed)
    sending = urllib.request.Request(signed.url, data=body, method=signed.method)
    for name, value in signed.headers.items():
        sending.add_header(name, value)
    try:
        with urllib.request.urlopen(sending) as answered:
            return {"status": answered.status, "body": answered.read().decode("utf-8")}
    except urllib.error.HTTPError as refused:
        return {"status": refused.code, "body": refused.read().decode("utf-8")}
"#;

/// The emulator as it is ASSEMBLED, on one engine: the request the handler signs is dispatched
/// back into this same router, so the domain it writes into is the domain this suite deployed.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
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
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
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

/// One Amazon OpenSearch Service control-plane call over its `restJson1` wire.
async fn opensearch(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    dispatched(router, "es", method, path, body).await
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, http::HeaderMap, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    let payload = match body {
        Some(document) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(document.to_string())
        }
        None => Bytes::new(),
    };
    let request = builder.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One request against the service that answers for `signing_name` at its regional host.
async fn dispatched(
    router: &MultiServiceRouter,
    signing_name: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization(signing_name));
    let payload = match body {
        Some(document) => {
            builder = builder.header("content-type", "application/json");
            Bytes::from(document.to_string())
        }
        None => Bytes::new(),
    };
    let request = builder.body(payload).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One request to the address the DOMAIN publishes, dialled the way a client dials it — the
/// deployer's own credential, which every document in force here still covers, so what this
/// reports is the state of the index and never a verdict about the reader.
async fn at_address(
    router: &MultiServiceRouter,
    address: &str,
    method: &str,
    path: &str,
) -> (u16, Value) {
    let (host, prefix) = match address.split_once('/') {
        Some((host, under)) => (host.to_string(), format!("/{under}")),
        None => (address.to_string(), String::new()),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{prefix}{path}"))
        .header("host", host)
        .header("authorization", authorization("es"))
        .header("content-type", "application/json")
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, body)
}

/// The ARN of the identity this suite DEPLOYS with, as `sts:GetCallerIdentity` reports it —
/// the principal the recorded trajectory's access policy keeps naming while it narrows the
/// function's role away.
///
/// Read from the token service rather than written here: a statement naming the account's own
/// root would be a statement about every principal of the account, so it would still cover the
/// function's session and no narrowing could be read through it.
async fn deployer_arn(router: &MultiServiceRouter) -> String {
    let (status, body) = query(router, "sts", "2011-06-15", "GetCallerIdentity", &[]).await;
    let arn = opt_element(&body, "Arn").unwrap_or_default();
    assert!(
        status == 200 && arn.starts_with("arn:"),
        "setup: GetCallerIdentity must report the deploying identity, got {status} with {body}"
    );
    arn
}

/// The ARN IAM mints for the execution role the stack declares.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// The session the handler's own request is signed as, which is the principal the domain takes
/// its decision about: Lambda opens the execution role's session under the function's name.
fn session_arn() -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{FUNCTION}")
}

/// The domain's ARN, which every statement below names a path under.
fn domain_arn() -> String {
    format!("arn:aws:es:{REGION}:{ACCOUNT}:domain/{DOMAIN}")
}

/// A statement covering `principals` for `action` on `resource`.
fn statement(effect: &str, principals: Value, action: &str, resource: &str) -> Value {
    json!({
        "Effect": effect,
        "Principal": { "AWS": principals },
        "Action": action,
        "Resource": resource,
    })
}

/// One document of the whole policy, as `UpdateDomainConfig` carries it: the member is modelled
/// a string (`smithy_query(query="operation", operation="UpdateDomainConfig")`), so what is
/// sent is the TEXT of the document.
fn document(statements: Vec<Value>) -> String {
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
}

/// The access policy the domain is deployed with, and the one every restore puts back: ONE
/// `Allow` statement naming BOTH the deployer and the execution role for every data-plane
/// action on every path of the domain.
fn full_policy(deployer: &str) -> String {
    document(vec![statement(
        "Allow",
        json!([deployer, role_arn()]),
        "es:ESHttp*",
        &format!("{}/*", domain_arn()),
    )])
}

/// The statement that keeps the DEPLOYER covered, which every narrowed document below carries:
/// what a row narrows is the coverage of the function's role and nothing else, so a refusal
/// that arrives is about the principal the row dropped.
fn deployer_statement(deployer: &str) -> Value {
    statement(
        "Allow",
        json!(deployer),
        "es:ESHttp*",
        &format!("{}/*", domain_arn()),
    )
}

/// Which of IAM's rules refuses a row's write, which is what the refusal's sentence names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refused {
    /// No statement of the document in force covers the request, and the domain DOES carry an
    /// access policy — so what would have had to allow it is that document.
    NoResourcePolicyAllows,
    /// A statement of the document in force explicitly denies it, which wins over every allow
    /// whichever side states it.
    ExplicitDeny,
}

impl Refused {
    /// The clause the refusal's sentence ends with.
    fn clause(self, action: &str) -> String {
        match self {
            Self::NoResourcePolicyAllows => {
                format!("because no resource-based policy allows the {action} action")
            }
            Self::ExplicitDeny => "with an explicit deny in a resource-based policy".to_string(),
        }
    }
}

/// One row: how the document stops covering the function's write, and the write it is read
/// through.
struct Row {
    case: &'static str,
    /// The method the write is sent with, which is where its action comes from.
    method: &'static str,
    /// The `es:ESHttp*` action that method names, as the refusal spells it.
    action: &'static str,
    /// The stem of the document ids this row writes, so each row's legs are its own.
    id: &'static str,
    /// The whole document in force while the row's write must be refused, built around the
    /// identity this suite deploys with — the one principal every narrowing keeps covered.
    narrowed: fn(&str) -> String,
    /// Which of IAM's rules refuses it.
    refused: Refused,
}

/// The table: each of the three elements that decide whether a statement covers a request —
/// `Principal`, `Action`, `Resource` — narrowed so it no longer does, plus the explicit `Deny`
/// that refuses a request a statement still covers, over two of the methods this surface reads
/// an action from.
const ROWS: &[Row] = &[
    Row {
        // The recorded form: the document narrowed to the deployer alone.
        case: "a Principal list the function's role is no longer in",
        method: "PUT",
        action: "es:ESHttpPut",
        id: "principal-narrowed",
        narrowed: |deployer| document(vec![deployer_statement(deployer)]),
        refused: Refused::NoResourcePolicyAllows,
    },
    Row {
        case: "an Action narrowed past the one the write's method names",
        method: "POST",
        action: "es:ESHttpPost",
        id: "action-narrowed",
        narrowed: |deployer| {
            document(vec![
                deployer_statement(deployer),
                statement(
                    "Allow",
                    json!(role_arn()),
                    "es:ESHttpGet",
                    &format!("{}/*", domain_arn()),
                ),
            ])
        },
        refused: Refused::NoResourcePolicyAllows,
    },
    Row {
        case: "a Resource naming another index's paths",
        method: "PUT",
        action: "es:ESHttpPut",
        id: "resource-narrowed",
        narrowed: |deployer| {
            document(vec![
                deployer_statement(deployer),
                statement(
                    "Allow",
                    json!(role_arn()),
                    "es:ESHttp*",
                    &format!("{}/{OTHER_INDEX}/*", domain_arn()),
                ),
            ])
        },
        refused: Refused::NoResourcePolicyAllows,
    },
    Row {
        case: "an explicit Deny beside an Allow that still names the role",
        method: "PUT",
        action: "es:ESHttpPut",
        id: "explicit-deny",
        narrowed: |deployer| {
            document(vec![
                statement(
                    "Allow",
                    json!([deployer, role_arn()]),
                    "es:ESHttp*",
                    &format!("{}/*", domain_arn()),
                ),
                statement(
                    "Deny",
                    json!(role_arn()),
                    "es:ESHttp*",
                    &format!("{}/*", domain_arn()),
                ),
            ])
        },
        refused: Refused::ExplicitDeny,
    },
];

/// The whole estate as ONE template: the execution role, the bucket beside it, the domain whose
/// access policy names both the deployer and that role, and the function configured with the
/// address the domain publishes.
///
/// The policy is declared the way every policy document is declared — as the document itself,
/// with `Fn::GetAtt` and `Fn::Sub` inside it — so what the deployment has to resolve is the
/// same thing the recorded template asks it to.
fn template(deployer: &str) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the estate the access-policy trajectory runs in",
        "Resources": {
            "IndexerRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": ROLE,
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "Service": "lambda.amazonaws.com" },
                            "Action": "sts:AssumeRole",
                        }],
                    },
                },
            },
            "LandingBucket": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": BUCKET },
            },
            "SearchDomain": {
                "Type": "AWS::OpenSearchService::Domain",
                "DependsOn": ["IndexerRole"],
                "Properties": {
                    "DomainName": DOMAIN,
                    "EngineVersion": ENGINE_VERSION,
                    "ClusterConfig": { "InstanceType": "t3.small.search", "InstanceCount": 1 },
                    "EBSOptions": { "EBSEnabled": true, "VolumeType": "gp3", "VolumeSize": 10 },
                    "DomainEndpointOptions": { "EnforceHTTPS": true },
                    "AccessPolicies": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Effect": "Allow",
                            "Principal": { "AWS": [
                                deployer,
                                { "Fn::GetAtt": ["IndexerRole", "Arn"] },
                            ]},
                            "Action": "es:ESHttp*",
                            "Resource": { "Fn::Sub": format!(
                                "arn:${{AWS::Partition}}:es:${{AWS::Region}}:${{AWS::AccountId}}\
                                 :domain/{DOMAIN}/*"
                            )},
                        }],
                    },
                },
            },
            "Indexer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": { "Fn::GetAtt": ["IndexerRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": {
                        "SEARCH_ENDPOINT": { "Fn::GetAtt": ["SearchDomain", "DomainEndpoint"] },
                        "LANDING_BUCKET": { "Ref": "LandingBucket" },
                    }},
                    "Code": { "ZipFile": WRITER_HANDLER },
                },
            },
        },
        "Outputs": {
            "DomainEndpoint": { "Value": { "Fn::GetAtt": ["SearchDomain", "DomainEndpoint"] } },
            "DomainArn": { "Value": { "Fn::GetAtt": ["SearchDomain", "Arn"] } },
            "DomainName": { "Value": { "Ref": "SearchDomain" } },
            "RoleArn": { "Value": { "Fn::GetAtt": ["IndexerRole", "Arn"] } },
            "BucketName": { "Value": { "Ref": "LandingBucket" } },
            "FunctionName": { "Value": { "Ref": "Indexer" } },
        },
    })
    .to_string()
}

/// Whether a stack status is one a deployment stops in, so polling it again tells a client
/// nothing new.
fn is_terminal(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// Deploy the estate and answer the body the stack was last described with, once it reached a
/// terminal status — the condition a client polls for, never a fixed stretch of time.
async fn deployed(router: &MultiServiceRouter, deployer: &str) -> String {
    let body = template(deployer);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
            ("Capabilities.member.2", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", STACK)]).await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks({STACK:?}) must be served, got {status} with {described}"
        );
        if is_terminal(&opt_element(&described, "StackStatus").unwrap_or_default()) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the stack must reach a terminal status, last described {described}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Every `ResourceStatusReason` the stack's events carry, which is where a resource an owning
/// service refused reports why.
async fn resource_reasons(router: &MultiServiceRouter) -> Vec<String> {
    let (_, events) =
        cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
    crate::query_wire::elements(&events, "ResourceStatusReason")
}

/// Wait until Lambda reports the deployed function [`STATE_ACTIVE`], which is the condition a
/// probe's invocation really needs rather than the one the stack's own status states.
async fn activated_function(router: &MultiServiceRouter) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, _, read) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if read["State"].as_str() == Some(STATE_ACTIVE) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the deployed function must reach {STATE_ACTIVE}, last read answered {status} \
             with {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Wait until the domain reports itself created with nothing in flight, and answer the
/// `DomainStatus` that read saw — the condition a client polls for after a configuration
/// change, never a fixed stretch of time.
async fn settled_domain(router: &MultiServiceRouter, case: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, read) = opensearch(
            router,
            "GET",
            &format!("/2021-01-01/opensearch/domain/{DOMAIN}"),
            None,
        )
        .await;
        let reported = read["DomainStatus"].clone();
        if reported["Created"].as_bool() == Some(true)
            && reported["Processing"].as_bool() == Some(false)
        {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the domain never reported itself settled, last read {reported}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Put `policy` in force as the domain's access policy through `UpdateDomainConfig`, and wait
/// for the change it starts to land.
async fn given_access_policy(router: &MultiServiceRouter, case: &str, policy: &str) {
    let (status, answered) = opensearch(
        router,
        "POST",
        &format!("/2021-01-01/opensearch/domain/{DOMAIN}/config"),
        Some(json!({ "AccessPolicies": policy })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from UpdateDomainConfig, got {status} with {answered}"
    );
    let settled = settled_domain(router, case).await;
    assert_eq!(
        settled["AccessPolicies"].as_str(),
        Some(policy),
        "case {case:?}: the document in force must be the one the update carried, got {settled}"
    );
}

/// Invoke the function and answer what the handler reported: the status the domain gave its
/// signed write, the body it answered with, and the whole payload for a failure message.
///
/// Looks again for as long as Lambda has not SERVED the invocation ([`UNSERVED`]), so what this
/// answers is always a decision the domain took about a handler that ran. Every other answer is
/// asserted on exactly as it was.
async fn probed_write(
    router: &MultiServiceRouter,
    case: &str,
    row: &Row,
    id: &str,
) -> (u64, Value, Value) {
    let event = json!({
        "method": row.method,
        "index": INDEX,
        "id": id,
        "document": { "id": id, "case": case },
    });
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, headers, payload) = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
            Some(event.clone()),
        )
        .await;
        if UNSERVED.contains(&status) {
            assert!(
                Instant::now() < deadline,
                "case {case:?}: Lambda must serve this probe's invocation, last answered {status} \
                 with {payload}"
            );
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must be served, got {status} with {payload}"
        );
        let faulted = headers
            .get("x-amz-function-error")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        assert_eq!(
            faulted, None,
            "case {case:?}: the handler must run to the end, got {faulted:?} with {payload}"
        );
        let answered = payload["status"].as_u64().unwrap_or_default();
        let body: Value = payload["body"]
            .as_str()
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_else(|| payload["body"].clone());
        return (answered, body, payload);
    }
}

/// Assert the function's signed write is SERVED, and that what it wrote is readable at the
/// address the domain published.
async fn assert_write_served(
    router: &MultiServiceRouter,
    address: &str,
    row: &Row,
    id: &str,
    leg: &str,
) {
    let case = row.case;
    let (answered, body, payload) = probed_write(router, case, row, id).await;
    assert!(
        (200..=201).contains(&answered),
        "case {case:?} ({leg}): a write the document in force covers must be served, got \
         {answered} with {payload}"
    );
    assert_eq!(
        body["result"].as_str(),
        Some(if answered == 201 { "created" } else { "updated" }),
        "case {case:?} ({leg}): expected the engine's own write answer, got {body}"
    );
    let (status, read) = at_address(router, address, "GET", &format!("/{INDEX}/_doc/{id}")).await;
    assert_eq!(
        (status, read["_source"]["case"].as_str()),
        (200, Some(case)),
        "case {case:?} ({leg}): the written document must be readable, got {status} with {read}"
    );
}

/// RULE: a domain's access policy is the whole of what decides a signed data-plane request from
/// a principal carrying no identity grant — so narrowing that document away from the deployed
/// function's role refuses its identical write `403` with AWS's own sentence and stores nothing,
/// while putting the document back serves the same write again; the deployer, whom every
/// document in force still covers, is served throughout, so neither verdict is the one an inert
/// policy or a blanket refusal would give.
#[tokio::test]
async fn a_narrowed_domain_access_policy_refuses_the_deployed_functions_write() {
    let router = build_router();
    let (address, _, deployer) = given_estate(&router).await;

    for row in ROWS {
        let case = row.case;
        let covered = format!("{}-covered", row.id);
        let withheld = format!("{}-withheld", row.id);

        // While the document names both principals, the function's write is served.
        given_access_policy(&router, case, &full_policy(&deployer)).await;
        assert_write_served(&router, &address, row, &covered, "the covering document").await;

        // The one change: the document narrowed so it no longer covers that write.
        given_access_policy(&router, case, &(row.narrowed)(&deployer)).await;
        let (answered, body, payload) = probed_write(&router, case, row, &withheld).await;
        assert_eq!(
            answered, 403,
            "case {case:?}: a write the document in force does not cover must be refused 403, \
             got {answered} with {payload}"
        );
        let sentence = format!(
            "User: {} is not authorized to perform: {} {}",
            session_arn(),
            row.action,
            row.refused.clause(row.action)
        );
        assert_eq!(
            body["Message"].as_str(),
            Some(sentence.as_str()),
            "case {case:?}: expected AWS's own refusal of {}, got {body}",
            row.action
        );

        // The refused write stored NOTHING: the id it addressed is still absent to the deployer,
        // whom the narrowed document still covers.
        let (status, absent) = at_address(
            &router,
            &address,
            "GET",
            &format!("/{INDEX}/_doc/{withheld}"),
        )
        .await;
        assert_eq!(
            (status, absent["found"].as_bool()),
            (404, Some(false)),
            "case {case:?}: the refused write must have stored nothing, got {status} with {absent}"
        );

        // And the narrowed document is not a blanket refusal: the document the covered leg wrote
        // is still readable while it is in force.
        let (status, still) = at_address(
            &router,
            &address,
            "GET",
            &format!("/{INDEX}/_doc/{covered}"),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the narrowed document must leave the deployer served, got {status} \
             with {still}"
        );

        // Putting the covering document back serves the identical write it refused.
        given_access_policy(&router, case, &full_policy(&deployer)).await;
        assert_write_served(&router, &address, row, &withheld, "the document restored").await;
    }
}

/// RULE (the half the deployment owes): a stack whose resources span four services settles in
/// `CREATE_COMPLETE`, every resource it reports created really exists in the service that OWNS
/// it, and the domain publishes the endpoint the function was configured with — so the
/// trajectory above is reachable at all.
#[tokio::test]
async fn the_deployed_estate_settles_and_every_resource_it_reports_exists() {
    let router = build_router();
    let (address, outputs, _) = given_estate(&router).await;

    // The address the stack published is the one the owning service itself reports, and the one
    // the deployed function was configured with: three readers of one value, which is what a
    // client needs for the function to reach the domain at all.
    let settled = settled_domain(&router, "the deployed domain").await;
    assert_eq!(
        settled["Endpoint"].as_str(),
        Some(address.as_str()),
        "the stack's output must be the address the owning service publishes, got {settled}"
    );
    let (status, _, configuration) = lambda(
        &router,
        "GET",
        &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
        None,
    )
    .await;
    assert_eq!(
        (status, configuration["Environment"]["Variables"]["SEARCH_ENDPOINT"].as_str()),
        (200, Some(address.as_str())),
        "the deployed function must carry the address the domain publishes, got {configuration}"
    );
    assert_eq!(
        configuration["Environment"]["Variables"]["LANDING_BUCKET"].as_str(),
        Some(BUCKET),
        "the deployed function must carry the bucket of its own stack, got {configuration}"
    );

    // Each identifier the stack publishes is the one the owning service minted.
    assert_eq!(
        outputs.get("DomainArn").map(String::as_str),
        Some(domain_arn().as_str()),
        "the stack must publish the domain ARN its owning service minted, got {outputs:?}"
    );
    assert_eq!(
        outputs.get("RoleArn").map(String::as_str),
        Some(role_arn().as_str()),
        "the stack must publish the role ARN IAM minted, got {outputs:?}"
    );

    // And the resources it reports created really exist where they are OWNED: IAM holds the
    // role, Amazon S3 holds the bucket, Lambda holds the function, and the domain's own data
    // plane answers at the address it published.
    let (status, role) = query(
        &router,
        "iam",
        "2010-05-08",
        "GetRole",
        &[("RoleName", ROLE)],
    )
    .await;
    assert_eq!(
        (status, opt_element(&role, "Arn")),
        (200, Some(role_arn())),
        "IAM must hold the role the stack reports created, got {status} with {role}"
    );
    let (status, bucket) = dispatched(&router, "s3", "HEAD", &format!("/{BUCKET}"), None).await;
    assert_eq!(
        status, 200,
        "Amazon S3 must hold the bucket the stack reports created, got {status} with {bucket}"
    );
    let (status, banner) = at_address(&router, &address, "GET", "/").await;
    assert_eq!(
        status, 200,
        "the address the domain published must answer, got {status} with {banner}"
    );
}

/// Deploy the estate once, assert it settled, and answer the address the domain publishes, the
/// stack's outputs, and the ARN of the identity it was deployed with.
async fn given_estate(
    router: &MultiServiceRouter,
) -> (String, BTreeMap<String, String>, String) {
    let deployer = deployer_arn(router).await;
    let described = deployed(router, &deployer).await;
    assert_eq!(
        opt_element(&described, "StackStatus").unwrap_or_default(),
        CREATE_COMPLETE,
        "setup: the estate must deploy, got {described} with reasons {:?}",
        resource_reasons(router).await
    );
    let outputs = outputs_of(&described);
    let address = outputs
        .get("DomainEndpoint")
        .cloned()
        .unwrap_or_else(|| panic!("setup: the stack must publish its domain's address"));
    assert!(
        !address.is_empty(),
        "setup: the domain must publish an address once it is up, got {address:?}"
    );
    activated_function(router).await;
    (address, outputs, deployer)
}
