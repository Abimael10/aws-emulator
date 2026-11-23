// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::ElasticBeanstalk::Environment` reports
//! `CREATE_COMPLETE` only once Elastic Beanstalk reports that environment `Ready`.
//!
//! The rule under test: **a resource whose owning service answers its create while the
//! resource is still coming up is reported as created only once that service reports it
//! ready, so the stack's own terminal status is a usable guarantee about it.** For an
//! Elastic Beanstalk environment that is `EnvironmentDescription$Status`: `CreateEnvironment`
//! "launches an environment for the specified application" and answers it `Launching`
//! ("Environment is in the process of initial deployment"), and only `Ready` is
//! "available to have an action performed on it, such as update or terminate". So at the
//! instant the stack reaches `CREATE_COMPLETE`, an independent `DescribeEnvironments` of
//! that environment reports `Ready` — and the CNAME prefix it holds is then taken, which
//! `CheckDNSAvailability` reports.
//!
//! Two crates have to agree for that to hold (CloudFormation opens the wait and makes the
//! calls, Elastic Beanstalk serves them and owns the launch), so the suite lives here
//! rather than in either of them.
//!
//! The rows differ along what the rule generalises over: whether the template names the
//! environment's `CNAMEPrefix` or leaves it to be generated, and the Region the stack
//! deploys in.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::json;
use service_com_amazonaws_elasticbeanstalk_awselasticbeanstalkservice as eb;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{element, elements, form_encode, opt_element, outputs_of};

/// Elastic Beanstalk's API version, as its model declares it (`version: 2010-12-01`).
const EB_API_VERSION: &str = "2010-12-01";

/// How long a poll below waits for a stack to reach a terminal status, and for the
/// environment behind it to settle: generous, so a loaded host is not what decides the
/// outcome. The spans themselves are each service's own and far shorter.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long a poll below waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(eb::EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one awsQuery operation of `service` in `region`, returning its status and body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    region: &str,
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
        .header("host", format!("{service}.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service, region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", region, "2010-05-15", action, members).await
}

/// Call one Elastic Beanstalk operation over its awsQuery wire, signed for `region`, failing
/// the named case unless it answered 200.
async fn elasticbeanstalk(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "elasticbeanstalk",
        region,
        EB_API_VERSION,
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {action}, got {status} with body {body}"
    );
    body
}

/// The solution stack the rows launch on: the first row of the service's OWN published
/// catalog (`eb::state::SOLUTION_STACKS`, what `ListAvailableSolutionStacks` reports).
fn first_solution_stack() -> String {
    eb::state::SOLUTION_STACKS
        .first()
        .expect("the service publishes at least one solution stack")
        .name()
}

/// One row: the Region the stack deploys in, and the `CNAMEPrefix` its environment declares
/// (or `None` for one that declares none).
struct Row {
    case: &'static str,
    region: &'static str,
    cname_prefix: Option<&'static str>,
}

/// The template one row deploys: an application and an environment on it, publishing the
/// names CloudFormation resolved for both.
fn template(cname_prefix: Option<&str>) -> String {
    let mut environment = json!({
        "ApplicationName": { "Ref": "EbApplication" },
        "SolutionStackName": first_solution_stack(),
        "OptionSettings": [{
            "Namespace": "aws:elasticbeanstalk:environment",
            "OptionName": "EnvironmentType",
            "Value": "SingleInstance"
        }]
    });
    if let Some(prefix) = cname_prefix {
        environment["CNAMEPrefix"] = json!(prefix);
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "EbApplication": { "Type": "AWS::ElasticBeanstalk::Application" },
            "EbEnvironment": {
                "Type": "AWS::ElasticBeanstalk::Environment",
                "Properties": environment
            }
        },
        "Outputs": {
            "EnvRef": { "Value": { "Ref": "EbEnvironment" } },
            "EnvEndpoint": { "Value": { "Fn::GetAtt": ["EbEnvironment", "EndpointURL"] } }
        }
    })
    .to_string()
}

/// Look again until the stack `stack_name` reaches a status that is not
/// `CREATE_IN_PROGRESS`, and answer the `DescribeStacks` body that reported it.
///
/// The CONDITION is what is waited on, never a fixed time: the spans both services owe are
/// their own, and this only fails where the stack never leaves that status at all.
async fn created(router: &MultiServiceRouter, case: &str, region: &str, stack_name: &str) -> String {
    let started = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let stack_status = opt_element(&described, "StackStatus").unwrap_or_default();
        if stack_status != "CREATE_IN_PROGRESS" {
            return described;
        }
        assert!(
            started.elapsed() < DEADLINE,
            "case {case:?}: the stack never left CREATE_IN_PROGRESS, last body {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE: at the instant a stack reports `CREATE_COMPLETE`, the environment it declared is
/// one Elastic Beanstalk reports `Ready` — the status that service documents as "available
/// to have an action performed on it" — and the CNAME prefix that environment holds is
/// taken, so `CheckDNSAvailability` reports it unavailable.
#[tokio::test]
async fn a_stack_is_complete_only_once_its_environment_is_ready() {
    let rows = [
        Row {
            case: "a declared CNAME prefix",
            region: "us-west-2",
            cname_prefix: Some("cfn-eb-ready"),
        },
        Row {
            case: "a generated CNAME prefix, in another Region",
            region: "eu-central-1",
            cname_prefix: None,
        },
    ];

    let router = build_router();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("eb-ready-stack-{index}");
        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template(row.cname_prefix)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let described = created(&router, case, region, &stack_name).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // THE defect this suite is about: the environment the completed stack created is
        // one that service itself reports ready to be acted on.
        let outputs = outputs_of(&described);
        let environment = outputs
            .get("EnvRef")
            .unwrap_or_else(|| panic!("case {case:?}: no EnvRef output in {described}"))
            .clone();
        let environments = elasticbeanstalk(
            &router,
            case,
            region,
            "DescribeEnvironments",
            &[("EnvironmentNames.member.1", &environment)],
        )
        .await;
        assert_eq!(
            elements(&environments, "Status"),
            vec![eb::state::STATUS_READY.to_string()],
            "case {case:?}: the environment of a CREATE_COMPLETE stack must be ready, got \
             {environments}"
        );

        // And it really holds its address: the prefix the published CNAME's first label
        // names is taken for as long as the environment carries it.
        let cname = element(&environments, "CNAME");
        let prefix = cname
            .split('.')
            .next()
            .unwrap_or_else(|| panic!("case {case:?}: no CNAME label in {environments}"));
        let checked = elasticbeanstalk(
            &router,
            case,
            region,
            "CheckDNSAvailability",
            &[("CNAMEPrefix", prefix)],
        )
        .await;
        assert_eq!(
            opt_element(&checked, "Available").as_deref(),
            Some("false"),
            "case {case:?}: the prefix {prefix:?} the stack's environment holds must not be \
             available, got {checked}"
        );
        if let Some(declared) = row.cname_prefix {
            assert_eq!(
                prefix, declared,
                "case {case:?}: the published CNAME must start at the declared prefix, got \
                 {cname:?}"
            );
        }
    }
}
