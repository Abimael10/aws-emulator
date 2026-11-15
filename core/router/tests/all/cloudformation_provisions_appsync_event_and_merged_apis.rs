// SPDX-License-Identifier: CC0-1.0

//! The resource types a CloudFormation template may DECLARE, at the assembled router: the
//! `AWS::AppSync::*` types that service publishes are accepted, and a type it publishes none
//! for is still refused.
//!
//! The rule under test: **CloudFormation deploys only the resource types it knows — a template
//! declaring a type that is not one Amazon Web Services publishes is refused when the template
//! is submitted, HTTP 400 `ValidationError` with `Template format error: Unrecognized resource
//! types: [<types>]`, and nothing is created — so every type the resource reference DOES publish
//! for a service this emulator mounts has to be accepted instead.** The AppSync types an Event
//! API and a Merged API's link are declared with are the ones this suite pins: a client's
//! `CreateStack` lands on the assembled router, and the service that owns those types has to be
//! mounted beside CloudFormation for the stack it admits to provision them, so the suite drives
//! the assembled router rather than one crate.
//!
//! The rows differ along what the rule generalises over: which AppSync types the template
//! declares (the Event API, its channel namespace, the source-API link), how many of them one
//! template declares at once, and — on the refusing side — a type under a namespace this
//! emulator serves that the reference publishes nothing for, and a misspelling of one it does.
//! What each resource of an admitted template then IS in AppSync is
//! `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/\
//! appsync_event_and_merged_api_resources.rs`.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{form_encode, opt_element};

/// The Region these requests are signed for, which is the Region the stacks live in.
const REGION: &str = "us-east-1";

/// The emulator with the services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them: a stack that declares an AppSync resource provisions it
/// by calling AppSync in process, so that service is mounted beside CloudFormation.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::EmulatedService::new(engine),
    );
    router
}

/// Call one CloudFormation operation over its awsQuery wire, signed for [`REGION`].
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/\
                 cloudformation/aws4_request, SignedHeaders=host, Signature=00000000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// What CloudFormation prefixes its refusal of a template declaring a type it does not know
/// with, as recorded.
const UNRECOGNIZED_PREFIX: &str = "Template format error: Unrecognized resource types: [";

/// How long one `DescribeStacks` poll waits before the next, and how many are made: a stack's
/// status transitions are owed on its clock, so the suite waits for the STATUS rather than for a
/// span, with a deadline generous enough for a loaded host running every suite of the workspace
/// beside this one.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 4_800;

/// Every template that declares only types the resource reference publishes is ADMITTED: the
/// stack is created, so nothing about these types is refused at submit time.
#[tokio::test]
async fn a_template_declaring_the_appsync_types_is_admitted() {
    let router = build_router();
    // (case, stack name, template body).
    let rows: &[(&str, &str, &str)] = &[
        (
            "an Event API alone",
            "appsync-api-admitted",
            concat!(
                r#"{"Resources":{"Events":{"Type":"AWS::AppSync::Api","Properties":{"#,
                r#""Name":"admitted-events","EventConfig":{"#,
                r#""AuthProviders":[{"AuthType":"API_KEY"}],"#,
                r#""ConnectionAuthModes":[{"AuthType":"API_KEY"}],"#,
                r#""DefaultPublishAuthModes":[{"AuthType":"API_KEY"}],"#,
                r#""DefaultSubscribeAuthModes":[{"AuthType":"API_KEY"}]}}}}}"#,
            ),
        ),
        (
            "an Event API and a channel namespace of it",
            "appsync-namespace-admitted",
            concat!(
                r#"{"Resources":{"Events":{"Type":"AWS::AppSync::Api","Properties":{"#,
                r#""Name":"admitted-namespaces","EventConfig":{"#,
                r#""AuthProviders":[{"AuthType":"API_KEY"}],"#,
                r#""ConnectionAuthModes":[{"AuthType":"API_KEY"}],"#,
                r#""DefaultPublishAuthModes":[{"AuthType":"API_KEY"}],"#,
                r#""DefaultSubscribeAuthModes":[{"AuthType":"API_KEY"}]}}},"#,
                r#""Default":{"Type":"AWS::AppSync::ChannelNamespace","Properties":{"#,
                r#""ApiId":{"Fn::GetAtt":["Events","ApiId"]},"Name":"default"}}}}"#,
            ),
        ),
        (
            "a source API linked into a Merged API",
            "appsync-link-admitted",
            concat!(
                r#"{"Resources":{"#,
                r#""Source":{"Type":"AWS::AppSync::GraphQLApi","Properties":{"#,
                r#""Name":"admitted-source","AuthenticationType":"API_KEY"}},"#,
                r#""Merged":{"Type":"AWS::AppSync::GraphQLApi","Properties":{"#,
                r#""Name":"admitted-merged","AuthenticationType":"API_KEY","#,
                r#""ApiType":"MERGED"}},"#,
                r#""Link":{"Type":"AWS::AppSync::SourceApiAssociation","Properties":{"#,
                r#""MergedApiIdentifier":{"Fn::GetAtt":["Merged","ApiId"]},"#,
                r#""SourceApiIdentifier":{"Fn::GetAtt":["Source","ApiId"]}}}}}"#,
            ),
        ),
    ];
    for (case, stack, template) in rows {
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", stack), ("TemplateBody", template)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the template to be admitted with HTTP 200, got {status} \
             with body {body}"
        );
        assert!(
            !body.contains(UNRECOGNIZED_PREFIX),
            "case {case:?}: expected no unrecognized-type refusal, got {body}"
        );
        assert!(
            opt_element(&body, "StackId").is_some_and(|id| id.contains(stack)),
            "case {case:?}: expected the created stack's id, got {body}"
        );
    }
}

/// ...and a type the reference publishes none of is still refused, naming it: the catalogue is a
/// list of the types CloudFormation knows, not a shape test over `AWS::<x>::<y>`.
#[tokio::test]
async fn a_template_declaring_an_unpublished_type_is_still_refused() {
    let router = build_router();
    // (case, stack name, the declared type) — a namespace that publishes no resource type at
    // all, and a misspelling of one of the types above.
    let rows: &[(&str, &str, &str)] = &[
        ("a namespace with no published types", "appsync-swf", "AWS::SWF::Domain"),
        (
            "a misspelled Event API type",
            "appsync-misspelled-api",
            "AWS::AppSync::EventApi",
        ),
        (
            "a misspelled channel namespace type",
            "appsync-misspelled-namespace",
            "AWS::AppSync::ChannelNameSpace",
        ),
    ];
    for (case, stack, resource_type) in rows {
        let template =
            format!(r#"{{"Resources":{{"R":{{"Type":"{resource_type}","Properties":{{}}}}}}}}"#);
        let (status, body) = cloudformation(
            &router,
            "CreateStack",
            &[("StackName", stack), ("TemplateBody", &template)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with body {body}"
        );
        assert_eq!(
            opt_element(&body, "Code").as_deref(),
            Some("ValidationError"),
            "case {case:?}: expected the ValidationError code, got {body}"
        );
        let message = opt_element(&body, "Message").unwrap_or_default();
        assert_eq!(
            message,
            format!("{UNRECOGNIZED_PREFIX}{resource_type}]"),
            "case {case:?}: expected the refusal to name the type it does not know, got {body}"
        );
    }
}

/// ...and an admitted template really PROVISIONS what it declared: the stack reaches
/// `CREATE_COMPLETE` and the Event API it declared is one AWS AppSync holds, under the name the
/// template stated. (Every value such a stack publishes for its resources is pinned by
/// `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/\
/// appsync_event_and_merged_api_resources.rs`, which deploys the whole estate.)
#[tokio::test]
async fn an_admitted_event_api_template_creates_the_api_in_appsync() {
    let router = build_router();
    let case = "an Event API a stack declared";
    let stack = "appsync-api-provisioned";
    let name = "provisioned-events";
    let template = concat!(
        r#"{"Resources":{"Events":{"Type":"AWS::AppSync::Api","Properties":{"#,
        r#""Name":"provisioned-events","EventConfig":{"#,
        r#""AuthProviders":[{"AuthType":"API_KEY"}],"#,
        r#""ConnectionAuthModes":[{"AuthType":"API_KEY"}],"#,
        r#""DefaultPublishAuthModes":[{"AuthType":"API_KEY"}],"#,
        r#""DefaultSubscribeAuthModes":[{"AuthType":"API_KEY"}]}}}}}"#,
    );
    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let mut described = String::new();
    let mut reported = String::new();
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(&router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        reported = opt_element(&body, "StackStatus").unwrap_or_default();
        described = body;
        if reported == "CREATE_COMPLETE" {
            break;
        }
        assert!(
            !reported.ends_with("_FAILED") && !reported.starts_with("ROLLBACK"),
            "case {case:?}: expected the stack to be created, got {reported:?} with answer \
             {described}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to be created, got {described}"
    );

    // The API the stack declared is one AppSync holds, read over that service's own wire.
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://appsync.{REGION}.amazonaws.com/v2/apis"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/appsync/\
                 aws4_request, SignedHeaders=host, Signature=00000000"
            ),
        )
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let listed = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListApis, got {status} with body {listed}"
    );
    assert!(
        listed.contains(name),
        "case {case:?}: expected AppSync to hold the API the stack declared ({name:?}), got \
         {listed}"
    );
}
