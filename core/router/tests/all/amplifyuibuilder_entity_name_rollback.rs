// SPDX-License-Identifier: CC0-1.0

//! An `AWS::AmplifyUIBuilder::{Component,Form,Theme}` whose declared `Name` Amplify UI
//! Builder does not accept for an entity FAILS its resource and rolls the stack back, and
//! the entity it would have created exists nowhere.
//!
//! The rule under test: **an entity name Amplify UI Builder refuses is that service's own
//! refusal of the create, so CloudFormation reports it on the resource that failed — a
//! `CREATE_FAILED` row whose `ResourceStatusReason` quotes the provider's answer — and
//! rolls the stack back, leaving no entity of that name in the app environment.** Three
//! crates have to agree for that to hold (CloudFormation submits the template and makes the
//! call, AWS Amplify serves the app the entity belongs to, and Amplify UI Builder refuses
//! the name), so the suite lives here: a crate-local test can only see one side of it.
//!
//! Each row is driven TWICE against the same app environment: once with a name that service
//! accepts, which is the control that must reach `CREATE_COMPLETE` and leave the entity
//! readable, and once with the SAME declaration under the same name with its initial
//! lower-cased, which must roll back. The rows differ along which of the three entity types
//! carries the name, so the rule is asserted where it holds rather than only for the type
//! the defect was found on.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The region every request here is signed for, and so the region the stacks belong to.
const REGION: &str = "us-east-1";

/// How long one poll of a stack's status waits before asking again, and how many times: a
/// deployment settles well inside this budget, which is generous so a loaded host does not
/// decide the outcome.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
const POLLS: usize = 60;

/// The backend environment of the app the entities below are created in.
const ENVIRONMENT: &str = "staging";

/// The `ResourceStatusReason` the failed resource carries, with the three per-run values
/// masked ([`masked_reason`]): the SDK's request id, the attempt count that SDK made and the
/// token of the resource request are a different value on every run, and everything else is
/// FIXED text — the sentence Amplify UI Builder words for a name it does not accept, the SDK
/// diagnostic its client appends, and the envelope CloudFormation quotes a resource
/// provider's answer in, closing with the `HandlerErrorCode` class that provider reports it
/// under.
const REFUSAL_REASON: &str = "Resource handler returned message: \"Entity name is not a \
                              valid name (Service: AmplifyUiBuilder, Status Code: 400, \
                              Request ID: <id>) (SDK Attempt Count: <n>)\" (RequestToken: \
                              <token>, HandlerErrorCode: InternalFailure)";

/// The status a resource its provider refused carries.
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The emulator with the three services this suite exercises, mounted on ONE engine exactly
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
    router.register_service(service_com_amazonaws_amplify_amplify::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_amplifyuibuilder_amplifyuibuilder::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request. The signature is never checked: request authentication is off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation action over its awsQuery wire.
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
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Every entity NAME Amplify UI Builder lists for one app environment's collection, which
/// is what says whether the rolled-back entity is there.
async fn listed_names(
    router: &MultiServiceRouter,
    collection: &str,
    app_id: &str,
    environment: &str,
) -> Vec<String> {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!(
            "/app/{app_id}/environment/{environment}/{collection}"
        ))
        .header("host", format!("amplifyuibuilder.{REGION}.amazonaws.com"))
        .header("authorization", authorization("amplifyuibuilder"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let document: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    document
        .get("entities")
        .and_then(Value::as_array)
        .map(|entities| {
            entities
                .iter()
                .filter_map(|entity| entity.get("name").and_then(Value::as_str))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Submit a template and poll the stack until it first reports `wanted`; answers the
/// `DescribeStacks` body it was last read from.
///
/// The completion is owed from the instant the resources settle, which no fixed sleep can
/// name, so the condition is polled rather than waited out.
async fn deploy(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let mut last = String::new();
    for _ in 0..POLLS {
        let (status, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {body}"
        );
        let reported = opt_element(&body, "StackStatus").unwrap_or_default();
        last = body;
        if !reported.ends_with("_IN_PROGRESS") {
            return last;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    last
}

/// `reason` with its three per-run values masked, so a row compares the FIXED text around
/// them against [`REFUSAL_REASON`].
///
/// A value that is EMPTY is left empty rather than masked, so a reason that states no
/// request id, no attempt count or no request token fails the comparison instead of passing
/// it.
fn masked_reason(reason: &str) -> String {
    let mut masked = String::new();
    let mut rest = reason;
    for (label, placeholder, terminator) in [
        ("Request ID: ", "<id>", ")"),
        ("(SDK Attempt Count: ", "<n>", ")"),
        ("(RequestToken: ", "<token>", ","),
    ] {
        let Some((before, after)) = rest.split_once(label) else {
            return reason.to_string();
        };
        let Some((value, tail)) = after.split_once(terminator) else {
            return reason.to_string();
        };
        masked.push_str(before);
        masked.push_str(label);
        if !value.is_empty() {
            masked.push_str(placeholder);
        }
        masked.push_str(terminator);
        rest = tail;
    }
    masked.push_str(rest);
    masked
}

/// One entity type, with the properties its declaration carries beside the app environment
/// and the `Name` the row supplies.
struct Row {
    case: &'static str,
    type_name: &'static str,
    /// The collection that type's entities are listed under.
    collection: &'static str,
    /// The name the control deploys under; the refused deploy lower-cases its initial.
    accepted_name: &'static str,
    /// The members of the declaration beyond `AppId`, `EnvironmentName` and `Name`.
    members: fn() -> Value,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a form",
            type_name: "AWS::AmplifyUIBuilder::Form",
            collection: "forms",
            accepted_name: "PostCreateForm",
            members: || {
                json!({
                    "DataType": {"DataSourceType": "DataStore", "DataTypeName": "Post"},
                    "FormActionType": "create",
                    "SchemaVersion": "1.0",
                    "Fields": {
                        "title": {"Label": "Title", "InputType": {"Type": "TextField"}}
                    },
                    "SectionalElements": {"Heading": {"Type": "Heading", "Level": 1}},
                    "Style": {"HorizontalGap": {"Value": "16px"}}
                })
            },
        },
        Row {
            case: "a component",
            type_name: "AWS::AmplifyUIBuilder::Component",
            collection: "components",
            accepted_name: "HeroCard",
            members: || {
                json!({
                    "ComponentType": "Button",
                    "SchemaVersion": "1.0",
                    "Properties": {"label": {"Value": "Save"}},
                    "Overrides": {},
                    "BindingProperties": {},
                    "Variants": []
                })
            },
        },
        Row {
            case: "a theme",
            type_name: "AWS::AmplifyUIBuilder::Theme",
            collection: "themes",
            accepted_name: "StudioTheme",
            members: || json!({"Values": [{"Key": "primaryColor", "Value": {"Value": "#047d95"}}]}),
        },
    ]
}

/// The same name with its initial character lower-cased: the one difference between the
/// control deploy and the refused one.
fn lower_initial(name: &str) -> String {
    let mut characters = name.chars();
    match characters.next() {
        Some(first) => first.to_lowercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// The control template: the app, and the entity under a name Amplify UI Builder accepts.
fn control_template(row: &Row) -> String {
    let mut properties = json!({
        "AppId": {"Fn::GetAtt": ["App", "AppId"]},
        "EnvironmentName": ENVIRONMENT,
        "Name": row.accepted_name,
    });
    merge(&mut properties, (row.members)());
    json!({
        "Resources": {
            "App": {"Type": "AWS::Amplify::App", "Properties": {"Name": "uibuilder-entity-name"}},
            "Entity": {"Type": row.type_name, "Properties": properties}
        },
        "Outputs": {
            "AppIdAtt": {"Value": {"Fn::GetAtt": ["App", "AppId"]}},
            "EntityId": {"Value": {"Fn::GetAtt": ["Entity", "Id"]}}
        }
    })
    .to_string()
}

/// The refused template: the SAME entity, in the SAME app environment, under the same name
/// with its initial lower-cased — and nothing else, so the stack's whole outcome is that one
/// resource's.
fn refused_template(row: &Row, app_id: &str, name: &str) -> String {
    let mut properties = json!({
        "AppId": app_id,
        "EnvironmentName": ENVIRONMENT,
        "Name": name,
    });
    merge(&mut properties, (row.members)());
    json!({"Resources": {"Entity": {"Type": row.type_name, "Properties": properties}}}).to_string()
}

/// Copy every member of `extra` into `document`.
fn merge(document: &mut Value, extra: Value) {
    let (Some(target), Value::Object(extra)) = (document.as_object_mut(), extra) else {
        return;
    };
    for (member, value) in extra {
        target.insert(member, value);
    }
}

#[tokio::test]
async fn an_entity_name_amplify_ui_builder_refuses_rolls_the_stack_back() {
    for row in rows() {
        let case = row.case;
        let router = build_router();

        // The control: the same declaration under a name that service accepts.
        let control = deploy(
            &router,
            case,
            "uibuilder-entity-name-control",
            &control_template(&row),
        )
        .await;
        assert_eq!(
            opt_element(&control, "StackStatus").unwrap_or_default(),
            "CREATE_COMPLETE",
            "case {case:?}: the control stack must reach CREATE_COMPLETE, got {control}"
        );
        let outputs = outputs_of(&control);
        let app_id = outputs
            .get("AppIdAtt")
            .unwrap_or_else(|| panic!("case {case:?}: no AppIdAtt output in {control}"))
            .clone();
        assert!(
            listed_names(&router, row.collection, &app_id, ENVIRONMENT)
                .await
                .contains(&row.accepted_name.to_string()),
            "case {case:?}: the control entity must be listed under {:?}, got {:?}",
            row.accepted_name,
            listed_names(&router, row.collection, &app_id, ENVIRONMENT).await
        );

        // The refused deploy: the one difference is the name's initial character.
        let refused_name = lower_initial(row.accepted_name);
        let rolled_back = deploy(
            &router,
            case,
            "uibuilder-entity-name-refused",
            &refused_template(&row, &app_id, &refused_name),
        )
        .await;
        assert_eq!(
            opt_element(&rolled_back, "StackStatus").unwrap_or_default(),
            "ROLLBACK_COMPLETE",
            "case {case:?}: a stack whose only resource that service refuses must end \
             ROLLBACK_COMPLETE, got {rolled_back}"
        );

        // The failure is reported on the RESOURCE, in the envelope CloudFormation quotes a
        // resource provider's answer in.
        let (status, events) = cloudformation(
            &router,
            "DescribeStackEvents",
            &[("StackName", "uibuilder-entity-name-refused")],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with \
             {events}"
        );
        assert!(
            elements(&events, "ResourceStatus").contains(&CREATE_FAILED.to_string()),
            "case {case:?}: expected a {CREATE_FAILED} resource event, got {events}"
        );
        let reasons: Vec<String> = elements(&events, "ResourceStatusReason")
            .into_iter()
            .map(|reason| masked_reason(&reason))
            .collect();
        assert!(
            reasons.iter().any(|reason| reason == REFUSAL_REASON),
            "case {case:?}: expected a failure reason that masks to {REFUSAL_REASON:?}, got \
             {reasons:?}"
        );

        // And the entity the rolled-back stack would have created exists nowhere: the
        // control's is still the only one of its collection.
        let names = listed_names(&router, row.collection, &app_id, ENVIRONMENT).await;
        assert_eq!(
            names,
            vec![row.accepted_name.to_string()],
            "case {case:?}: a rolled-back stack must leave no entity named {refused_name:?}, \
             got {names:?}"
        );
    }
}
