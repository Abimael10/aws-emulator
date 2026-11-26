// SPDX-License-Identifier: CC0-1.0

//! An `AWS::ApiGatewayV2::Model` a CloudFormation stack declares reaches API Gateway v2 with
//! the schema the template stated, whichever FORM the template stated it in.
//!
//! The rule under test:
//!
//! > **A CloudFormation property whose owning API member is a STRING carrying a DOCUMENT —
//! > `CreateModel$Schema` is a `StringWithLengthBetween0And32K` holding "the schema for the
//! > model. For application/json models, this should be JSON schema draft 4 model" — may be
//! > declared in the template as that document itself, and the deployment sends the
//! > document's own JSON TEXT: nothing inside it is read as a modelled member name, so no key
//! > it carries is renamed, re-shaped or refused; a property declared as text travels exactly
//! > as written.**
//!
//! Two crates have to agree for that to hold (CloudFormation makes the create, API Gateway v2
//! serves it and reads the model back), so the suite lives here rather than in either of them.
//! `cloudformation_websocket_action_dispatch.rs` owns the one row where the schema is declared
//! as PRE-SERIALIZED text inside a whole WebSocket estate; what this suite adds is the form a
//! template normally writes a schema in — the document, inline — which is what no stack could
//! deploy: the resource was refused and the refusal rolled the whole stack back.
//!
//! The rows differ along what the rule generalises over: the FORM the schema is declared in
//! (an inline document against pre-serialized text), the document's size and nesting (an empty
//! schema, a flat one, one nesting an array of a type), the draft-04 keywords it carries
//! (`$schema`, `title`, `type`, `properties`, `required`, `items`), and the CASE of the names
//! the schema gives its OWN properties — upper camel, which a walk over modelled member names
//! would have re-cased. Every expected value is computed here from what the row declares,
//! never from the answer being checked.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_apigatewayv2_resources::{
    apigatewayv2, build_router, cloudformation, create_stack, describe_stack, member,
};
use crate::cloudformation_provisions_stack_resources::{outputs_of, xml_text, xml_texts};

/// The statuses a stack that created, and then updated, everything it declared reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const UPDATE_COMPLETE: &str = "UPDATE_COMPLETE";

/// The content type every row's model is the schema FOR: the one API Gateway's own
/// documentation gives for a model (`Model$ContentType`: "for example, \"application/json\"").
const CONTENT_TYPE: &str = "application/json";

/// How often the deploy asks the stack whether it has settled, and how long it goes on
/// asking. The wait is on the CONDITION — the stack reporting the status asked for — and the
/// deadline is only there so the wait terminates; nothing here is timed.
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// One row: the model the template declares and the schema that model must be stored with.
struct Row {
    /// What the row is called in every assertion of it.
    case: &'static str,
    /// The `Name` the model is declared under, which is also the logical id its declaration
    /// and its `Outputs` entry are keyed by. "Must be alphanumeric" (`CreateModel$Name`), so
    /// every row's name is.
    name: &'static str,
    /// The `Schema` property, exactly as the template declares it: an inline DOCUMENT for
    /// every row but the one that states pre-serialized text.
    declared: Value,
}

/// The rows, each a `Schema` of a different form, size, nesting and key casing.
///
/// Source for the keywords each document carries: JSON Schema draft-04, the dialect
/// `CreateModel$Schema` names ("this should be JSON schema draft 4 model") — `$schema`,
/// `title`, `type`, `properties`, `required` and `items`.
fn rows() -> Vec<Row> {
    vec![
        // The form a template normally writes a schema in: the draft-04 object, inline, with
        // the dialect it declares and a title of its own.
        Row {
            case: "a draft-04 object schema declared inline",
            name: "DraftFourObject",
            declared: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "title": "DraftFourObject",
                "type": "object",
                "properties": {"action": {"type": "string"}}
            }),
        },
        // The smallest schema there is: `{}` matches everything, and is a legitimate document.
        Row {
            case: "the empty schema, which matches everything",
            name: "EmptyObject",
            declared: json!({}),
        },
        // A document that NESTS: an array of a type, the list of names the schema requires,
        // and an object inside the object.
        Row {
            case: "a nested schema with a required list and an array of a type",
            name: "NestedArrays",
            declared: json!({
                "$schema": "http://json-schema.org/draft-04/schema#",
                "type": "object",
                "required": ["id", "tags"],
                "properties": {
                    "id": {"type": "integer"},
                    "tags": {"type": "array", "items": {"type": "string"}},
                    "nested": {
                        "type": "object",
                        "properties": {"depth": {"type": "number"}}
                    }
                }
            }),
        },
        // The row that pins what the document's keys ARE: a schema whose own property names
        // are UPPER camel, which a walk over modelled member names would have lower-cased
        // into names the template never declared.
        Row {
            case: "a schema whose own property names are upper camel",
            name: "UpperCamelProperties",
            declared: json!({
                "type": "object",
                "properties": {
                    "Action": {"type": "string"},
                    "RequestId": {"type": "string"},
                    "Payload": {"type": "object", "properties": {"Body": {"type": "string"}}}
                }
            }),
        },
        // The other form the same property is stated in: text the template serialized itself,
        // which travels exactly as written.
        Row {
            case: "a schema declared as pre-serialized text",
            name: "PreSerializedText",
            declared: Value::String(
                r#"{"$schema":"http://json-schema.org/draft-04/schema#","type":"object","properties":{"action":{"type":"string"}}}"#
                    .to_string(),
            ),
        },
    ]
}

/// The text `CreateModel$Schema` takes a declared `Schema` as: a document's own compact JSON,
/// and a declared string exactly as written.
fn expected_schema_text(declared: &Value) -> String {
    match declared {
        Value::String(text) => text.clone(),
        document => document.to_string(),
    }
}

/// The DOCUMENT a declared `Schema` is, whichever form it was declared in: what a client that
/// parses the stored text must get back.
fn expected_schema_document(case: &str, declared: &Value) -> Value {
    match declared {
        Value::String(text) => serde_json::from_str(text)
            .unwrap_or_else(|failure| panic!("case {case:?}: the row's own schema text: {failure}")),
        document => document.clone(),
    }
}

/// The template the stack is created from: one WEBSOCKET api — the protocol a model belongs
/// to — and one `AWS::ApiGatewayV2::Model` per row in `rows`, each publishing its `Ref` (the
/// model id) as an output.
fn template(rows: &[&Row]) -> String {
    let mut resources = json!({
        "WsApi": {
            "Type": "AWS::ApiGatewayV2::Api",
            "Properties": {
                "Name": "cfn-model-schema-api",
                "ProtocolType": "WEBSOCKET",
                "RouteSelectionExpression": "$request.body.action"
            }
        }
    });
    let mut outputs = json!({"ApiId": {"Value": {"Ref": "WsApi"}}});
    for row in rows {
        resources[row.name] = json!({
            "Type": "AWS::ApiGatewayV2::Model",
            "Properties": {
                "ApiId": {"Ref": "WsApi"},
                "Name": row.name,
                "ContentType": CONTENT_TYPE,
                "Schema": row.declared.clone()
            }
        });
        outputs[row.name] = json!({"Value": {"Ref": row.name}});
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": resources,
        "Outputs": outputs
    })
    .to_string()
}

/// Poll `stack` until it reports `wanted`, and answer the status it last reported with the
/// `DescribeStacks` body it reported it in. The wait is on the STATUS, not on the clock: the
/// deadline only makes it terminate.
async fn settled(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    let giving_up_at = Instant::now() + SETTLE_DEADLINE;
    let mut body = describe_stack(router, case, stack).await;
    while xml_text(&body, "StackStatus").as_deref() != Some(wanted) && Instant::now() < giving_up_at
    {
        tokio::time::sleep(POLL_INTERVAL).await;
        body = describe_stack(router, case, stack).await;
    }
    (xml_text(&body, "StackStatus").unwrap_or_default(), body)
}

/// Why the stack did not settle where it was expected to: every reason its own events carry,
/// which is what names the resource that was refused and the refusal it answered.
async fn failure_reasons(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let (_, body) = cloudformation(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    xml_texts(&body, "ResourceStatusReason")
}

/// The models API Gateway v2 holds for `api_id`, keyed by the `Name` each is called by.
async fn models_of(
    router: &MultiServiceRouter,
    case: &str,
    api_id: &str,
) -> BTreeMap<String, Value> {
    let (status, listed) =
        apigatewayv2(router, "GET", &format!("/v2/apis/{api_id}/models"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from GetModels for {api_id:?}, got {status} {listed}"
    );
    listed["items"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|item| (member(case, item, "name"), item.clone()))
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declared_model_schema_reaches_api_gateway_as_the_document_the_template_stated() {
    let case_set = "every form of a declared Schema";
    let router = build_router();
    let stack = "cfn-apigwv2-model-schemas";
    let declared = rows();
    let every_row: Vec<&Row> = declared.iter().collect();
    create_stack(&router, case_set, stack, &template(&every_row)).await;
    let (reported, described) = settled(&router, case_set, stack, CREATE_COMPLETE).await;
    if reported != CREATE_COMPLETE {
        let reasons = failure_reasons(&router, stack).await;
        panic!(
            "case {case_set:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?} \
             with resource failures {reasons:?}"
        );
    }
    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .unwrap_or_else(|| panic!("case {case_set:?}: no ApiId output in {outputs:?}"))
        .clone();
    let held = models_of(&router, case_set, &api_id).await;
    assert_eq!(
        held.len(),
        declared.len(),
        "case {case_set:?}: expected the {} models the template declared, got {held:?}",
        declared.len()
    );

    for row in &declared {
        let case = row.case;
        let expected_text = expected_schema_text(&row.declared);

        // The id the stack published for the model, which is the id that service MINTED: the
        // template states no id at all, so a `Ref` resolving to anything the template carries
        // would not address the model.
        let model_id = outputs
            .get(row.name)
            .unwrap_or_else(|| panic!("case {case:?}: no {} output in {outputs:?}", row.name))
            .clone();
        assert_ne!(
            model_id, row.name,
            "case {case:?}: expected Ref to resolve to the minted model id, got the declared \
             name {model_id:?}"
        );

        // The model as the api's own LISTING reports it, found by the name the row declared.
        let listed = held
            .get(row.name)
            .unwrap_or_else(|| panic!("case {case:?}: GetModels reported no {}", row.name));
        assert_eq!(
            member(case, listed, "modelId"),
            model_id,
            "case {case:?}: expected the listed modelId to be the published one {model_id:?}, \
             got {listed}"
        );
        assert_eq!(
            member(case, listed, "contentType"),
            CONTENT_TYPE,
            "case {case:?}: expected the declared ContentType, got {listed}"
        );
        assert_eq!(
            member(case, listed, "schema"),
            expected_text,
            "case {case:?}: expected the schema text {expected_text:?}, got {listed}"
        );

        // And the same model read back ONE at a time, by the published id — the round trip a
        // client makes with what the stack handed it.
        let (status, read) = apigatewayv2(
            &router,
            "GET",
            &format!("/v2/apis/{api_id}/models/{model_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetModel for {model_id:?}, got {status} {read}"
        );
        let answered = member(case, &read, "schema");
        assert_eq!(
            answered, expected_text,
            "case {case:?}: expected GetModel to answer the schema text {expected_text:?}, got \
             {answered:?}"
        );

        // The schema is a DOCUMENT, so what a client parses out of the text it was stored as
        // is the very document the template declared — every key it carries, spelled as
        // declared.
        let parsed: Value = serde_json::from_str(&answered).unwrap_or_else(|failure| {
            panic!(
                "case {case:?}: GetModel answered a schema that is no JSON: {failure} in \
                 {answered:?}"
            )
        });
        let expected_document = expected_schema_document(case, &row.declared);
        assert_eq!(
            parsed, expected_document,
            "case {case:?}: expected the declared schema document {expected_document}, got \
             {parsed}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stack_update_that_drops_the_model_removes_it_from_the_api() {
    let case = "an update that drops the declared Model";
    let router = build_router();
    let stack = "cfn-apigwv2-model-removal";
    // One row is enough to pin the REMOVAL: which form its schema was declared in is what the
    // suite's other test spans, and nothing about a delete depends on it.
    let declared = rows();
    let row = &declared[0];
    create_stack(&router, case, stack, &template(&[row])).await;
    let (reported, described) = settled(&router, case, stack, CREATE_COMPLETE).await;
    if reported != CREATE_COMPLETE {
        let reasons = failure_reasons(&router, stack).await;
        panic!(
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, got {reported:?} with \
             resource failures {reasons:?}"
        );
    }
    let outputs = outputs_of(&described);
    let api_id = outputs
        .get("ApiId")
        .unwrap_or_else(|| panic!("case {case:?}: no ApiId output in {outputs:?}"))
        .clone();
    let model_id = outputs
        .get(row.name)
        .unwrap_or_else(|| panic!("case {case:?}: no {} output in {outputs:?}", row.name))
        .clone();

    // The revision declares the api and no model at all, so the model the stack created is
    // the resource the update REMOVES.
    let (status, body) = cloudformation(
        &router,
        "UpdateStack",
        &[("StackName", stack), ("TemplateBody", &template(&[]))],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with body {body}"
    );
    let (reported, _) = settled(&router, case, stack, UPDATE_COMPLETE).await;
    if reported != UPDATE_COMPLETE {
        let reasons = failure_reasons(&router, stack).await;
        panic!(
            "case {case:?}: expected the update to reach {UPDATE_COMPLETE}, got {reported:?} \
             with resource failures {reasons:?}"
        );
    }

    // The api is still there and holds no model: the removal reached API Gateway v2 rather
    // than being recorded by the stack alone.
    let held = models_of(&router, case, &api_id).await;
    assert!(
        held.is_empty(),
        "case {case:?}: expected the api to hold no model after the update, got {held:?}"
    );
    let (status, read) = apigatewayv2(
        &router,
        "GET",
        &format!("/v2/apis/{api_id}/models/{model_id}"),
        None,
    )
    .await;
    assert_eq!(
        status, 404,
        "case {case:?}: expected 404 from GetModel for the removed {model_id:?}, got {status} \
         {read}"
    );
    assert_eq!(
        member(case, &read, "message"),
        format!("Invalid model identifier specified: {model_id}"),
        "case {case:?}: expected the refusal to name the model id the request carried, got {read}"
    );
}
