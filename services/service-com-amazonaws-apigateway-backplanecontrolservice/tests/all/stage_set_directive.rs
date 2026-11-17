// SPDX-License-Identifier: CC0-1.0

//! The DIRECTIVES a mapping template is performed for, end to end through a deployed stage:
//! `#set` and the conditional family.
//!
//! RULE: a mapping template's `#set` directive binds a name to the value its expression
//! names — "the `#set` directive is used to set the value of a reference" — and the
//! references after it render what they reach through that name; a `#if`/`#elseif`/`#else`
//! chooses ONE piece of the template by a condition over those same values, and the pieces it
//! does not choose render nothing and resolve nothing. The directive itself renders nothing,
//! and so does the line break that closes its own line. Those are the directives a template
//! is PERFORMED for, because each states something about the call — a binding, a choice —
//! rather than work of its own; they are what lets a template name a SUB-DOCUMENT of the
//! payload, which is why every API Gateway mapping-template example opens with one
//! (`#set($inputRoot = $input.path('$'))`). A directive that is neither (`#foreach`,
//! `#macro`, a comment) is a statement this emulator does not perform, so a template carrying
//! one is answered through the api's `API_CONFIGURATION_ERROR` gateway response.
//!
//! The rows differ along what the rule generalises over: which side of the integration the
//! directive sits on (the request template, which for a `MOCK` names the status code that
//! selects the integration response, and the response template, which is the body), what is
//! bound (the whole payload, a sub-document of it, a method request parameter, a literal the
//! language interpolates, a number), how the bound value is read (member by member, by the
//! method a map documents, through a member it does not carry), which branch of a conditional
//! the call takes, which spelling the directive takes (`#set`, `#{set}`) and whether the line
//! it sits on carries text after it.
//!
//! Every case drives the real wire path — an unsigned request to the api's own invoke host
//! through the multi-service router, with the control plane driven through the same emulator
//! — so what is asserted is what a client of a published invoke URL sees.

use serde_json::{json, Value};

use crate::execute_api_data_plane::{
    integration_response, method_response, put_integration, put_method,
};
use crate::rest_api_lifecycle::{create_api, resource_under, string_member, Gateway};
use crate::stage_request_validation::{invoke, JSON, STAGE};

/// The payload every case POSTs, so a template that binds the method request's body has one
/// to bind.
const PAYLOAD: &str = r#"{"id":"a1","qty":2,"nested":{"deep":"value"}}"#;

/// One case: the templates the route carries, the query the request sends, and what the stage
/// must answer.
struct Row {
    case: &'static str,
    /// The integration's own request template, which for a `MOCK` is both the integration
    /// request and the payload its response template reads.
    request_template: &'static str,
    /// The integration response's template, which is the body.
    response_template: &'static str,
    query: &'static str,
    status: u16,
    body: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the preamble every mapping-template example opens with, on both sides",
        request_template: "#set($root = $input.path('$'))\n\
                           {\"statusCode\": 200, \"id\": \"$root.id\", \"qty\": $root.qty}",
        response_template: "#set($answer = $input.path('$'))\n\
                            {\"id\":\"$answer.id\",\"qty\":$answer.qty}",
        query: "",
        status: 200,
        body: r#"{"id":"a1","qty":2}"#,
    },
    Row {
        case: "a bound SUB-document, which no reference renders on its own",
        request_template: r#"{"statusCode": 200, "nested": $input.json('$.nested')}"#,
        response_template: "#set($answer = $input.path('$'))\n\
                            {\"deep\":\"$answer.nested.deep\"}",
        query: "",
        status: 200,
        body: r#"{"deep":"value"}"#,
    },
    Row {
        case: "the directive with the body on the same line, whose space is that line's text",
        request_template: r#"{"statusCode": 200, "id": "a1"}"#,
        response_template: "#set($answer = $input.path('$')) {\"id\":\"$answer.id\"}",
        query: "",
        status: 200,
        body: r#" {"id":"a1"}"#,
    },
    Row {
        case: "the formal spelling, read by the method a map documents",
        request_template: r#"{"statusCode": 200, "id": "a1"}"#,
        response_template: "#{set}($answer = $input.path('$'))\n{\"id\":\"$answer.get('id')\"}",
        query: "",
        status: 200,
        body: r#"{"id":"a1"}"#,
    },
    Row {
        case: "a bound parameter and a bound literal the language interpolates",
        request_template: r#"{"statusCode": 200}"#,
        response_template: "#set($tenant = $input.params('tenant'))\
                            #set($label = \"tenant $tenant\")\n{\"label\":\"$label\"}",
        query: "?tenant=acme",
        status: 200,
        body: r#"{"label":"tenant acme"}"#,
    },
    Row {
        case: "a member the bound value does not carry, which renders as nothing",
        request_template: r#"{"statusCode": 200, "id": "a1"}"#,
        response_template: "#set($answer = $input.path('$'))\n{\"absent\":\"$answer.missing\"}",
        query: "",
        status: 200,
        body: r#"{"absent":""}"#,
    },
    Row {
        case: "a bound number, which selects the integration response of that status",
        request_template: "#set($code = 200)\n{\"statusCode\": $code, \"id\": \"a1\"}",
        response_template: "#set($answer = $input.path('$'))\n{\"id\":\"$answer.id\"}",
        query: "",
        status: 200,
        body: r#"{"id":"a1"}"#,
    },
    // The conditional family, written the way the recorded custom-resource fixture writes it:
    // bind a member of the payload, then choose the piece of the document to render by
    // comparing it.
    Row {
        case: "a conditional whose ELSE branch this call takes",
        request_template: "#set($d = $input.path('$.qty'))\n\
                           {\"statusCode\": 200, #if($d < 0)\"low\": true#else\"id\": \"a1\"#end}",
        response_template: "#set($answer = $input.path('$'))\n{\"id\":\"$answer.id\"}",
        query: "",
        status: 200,
        body: r#"{"id":"a1"}"#,
    },
    Row {
        case: "a conditional whose IF branch this call takes",
        request_template: "#set($d = $input.path('$.qty'))\n\
                           {\"statusCode\": 200, #if($d > 0)\"id\": \"$d\"#else\"low\": true#end}",
        response_template: "#set($answer = $input.path('$'))\n{\"id\":\"$answer.id\"}",
        query: "",
        status: 200,
        body: r#"{"id":"2"}"#,
    },
    Row {
        case: "a conditional on a parameter, whose other branch names nothing of this call",
        request_template: r#"{"statusCode": 200}"#,
        response_template: "#if($input.params('tenant') == 'acme')\n\
                            {\"ours\":true}\n#else\n{\"who\":\"$context.status\"}\n#end",
        query: "?tenant=acme",
        status: 200,
        body: "{\"ours\":true}\n",
    },
    // The boundary: a directive that is neither a binding nor a choice is a statement this
    // emulator does not perform, so the call is answered through the api's own
    // configuration-error gateway response.
    Row {
        case: "a directive this emulator does not perform",
        request_template: r#"{"statusCode": 200}"#,
        response_template: "#foreach($i in $input.path('$.ids'))\n{\"a\":1}\n#end",
        query: "",
        status: 500,
        body: r#"{"message":"Internal server error"}"#,
    },
];

/// The resource path of the `index`-th row.
fn route_of(index: usize) -> String {
    format!("set-{index}")
}

#[tokio::test]
async fn a_set_directive_binds_what_the_templates_after_it_render() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "mapping-template-set").await;
    for (index, row) in ROWS.iter().enumerate() {
        let resource = resource_under(&gateway, &api, &root, &route_of(index)).await;
        put_method(
            &gateway,
            &api,
            &resource,
            "POST",
            json!({ "authorizationType": "NONE" }),
        )
        .await;
        put_integration(
            &gateway,
            &api,
            &resource,
            "POST",
            json!({ "type": "MOCK", "requestTemplates": { JSON: row.request_template } }),
        )
        .await;
        method_response(&gateway, &api, &resource, "POST", "200", json!({})).await;
        integration_response(
            &gateway,
            &api,
            &resource,
            "POST",
            "200",
            json!({ "responseTemplates": { JSON: row.response_template } }),
        )
        .await;
    }
    let deployment = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
        )
        .await;
    assert_eq!(
        deployment.status, 201,
        "setup CreateDeployment: expected 201, got {} with {:?}",
        deployment.status, deployment.body
    );
    let _ = string_member(&deployment.body, "id", "CreateDeployment");

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let answered = invoke(
            &gateway,
            &api,
            "POST",
            &format!("/{STAGE}/{}{}", route_of(index), row.query),
            &[("content-type", JSON)],
            PAYLOAD,
        )
        .await;
        assert_eq!(
            (answered.status, answered.body.as_str()),
            (row.status, row.body),
            "case {case:?}: expected {} {:?}, got {} {:?}",
            row.status,
            row.body,
            answered.status,
            answered.body
        );
    }
}

/// RULE: the same binding is performed for an integration that CALLS another service — the
/// request template that addresses it and the response template that renders its answer are
/// the same language — so a `#set` is what lets such a template read inside the document the
/// service answered.
#[tokio::test]
async fn a_set_directive_is_performed_for_a_service_integrations_templates() {
    let gateway = Gateway::new();
    let (api, root) = create_api(&gateway, "mapping-template-set-service").await;
    let resource = resource_under(&gateway, &api, &root, "capacity").await;
    put_method(
        &gateway,
        &api,
        &resource,
        "POST",
        json!({ "authorizationType": "NONE" }),
    )
    .await;
    // A `MOCK` stands in for the service here: what this pins is the RENDERING of the two
    // templates, which is the same for every integration type — the suite that drives a real
    // DynamoDB-backed endpoint through these templates is
    // `core/router/tests/all/application_autoscaling_custom_resource_target_tracking.rs`.
    put_integration(
        &gateway,
        &api,
        &resource,
        "POST",
        json!({
            "type": "MOCK",
            "requestTemplates": {
                JSON: "#set($root = $input.path('$'))\n\
                       {\"statusCode\": 200, \"Item\": {\"desired\": {\"N\": \"$root.qty\"}}}",
            },
        }),
    )
    .await;
    method_response(&gateway, &api, &resource, "POST", "200", json!({})).await;
    integration_response(
        &gateway,
        &api,
        &resource,
        "POST",
        "200",
        json!({
            "responseTemplates": {
                JSON: "#set($i = $input.path('$.Item'))\n\
                       {\"actualCapacity\":$i.desired.N,\"desiredCapacity\":$i.desired.N}",
            },
        }),
    )
    .await;
    let deployment = gateway
        .call(
            "POST",
            &format!("/restapis/{api}/deployments"),
            Some(json!({ "stageName": STAGE })),
        )
        .await;
    assert_eq!(
        deployment.status, 201,
        "setup CreateDeployment: expected 201, got {} with {:?}",
        deployment.status, deployment.body
    );

    let answered = invoke(
        &gateway,
        &api,
        "POST",
        &format!("/{STAGE}/capacity"),
        &[("content-type", JSON)],
        PAYLOAD,
    )
    .await;
    let read: Value = serde_json::from_str(&answered.body).unwrap_or(Value::Null);
    assert_eq!(
        read,
        json!({"actualCapacity": 2, "desiredCapacity": 2}),
        "case \"the capacity document a bound sub-document renders\": expected the two \
         capacities the item carries, got {} {:?}",
        answered.status,
        answered.body
    );
}
