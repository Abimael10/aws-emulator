// SPDX-License-Identifier: CC0-1.0

//! The three tags CloudFormation records on a stack-created `AWS::Lambda::Function`, read
//! back through the owning service's own tag plane and through the tagging index.
//!
//! The rule under test: **a taggable resource CloudFormation provisions carries, in the
//! owning service's own tag state, the tags the template declares for it and the stack-level
//! tags propagated to it, PLUS three reserved keys CloudFormation writes itself —
//! `aws:cloudformation:stack-name` (the deployed stack's name),
//! `aws:cloudformation:stack-id` (that stack's id) and `aws:cloudformation:logical-id` (that
//! resource's logical id in the template) — whether or not the template declares any `Tags`
//! at all; those three are the provisioning service's to write, through no call a client
//! makes, so a caller removing its OWN tags leaves them in place.** The reserved `aws:`
//! namespace is refused to a caller, so three crates have to agree for that to hold
//! (CloudFormation computes and sends the three, Lambda stores and reports them, the Resource
//! Groups Tagging index reads the same tag set), which is why the suite lives here rather
//! than in any one crate.
//!
//! What a client gets from it: stack-scoped inventory. `GetResources` filtered on
//! `aws:cloudformation:stack-name` answers with every function the stack owns, and filtered
//! on `aws:cloudformation:logical-id` with the one resource the template declared under that
//! id — which is what ownership attribution, cost allocation and tag-based policies key on.
//!
//! The rows differ along what the rule generalises over: how many tags the resource's own
//! declaration asks for (one, none at all, two), its logical id in the template, and whether
//! the template NAMES the function or leaves CloudFormation to generate its name. Every
//! expected value is computed here from what the row declares plus the stack's own name and
//! the id `DescribeStacks` reports for it, never read back out of the answer being checked.

use std::collections::BTreeMap;
use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML readers this test crate writes ONCE
// (`tests/all/query_wire.rs`). `form_encode` is a percent-encoder for an unreserved-only
// component, which is what a `tagKeys` query value needs too.
use crate::query_wire::{form_encode, opt_element, outputs_of};
// Polling a stack until it first reports a status is what every suite deploying a resource
// its owning service reports as still coming up needs, and this crate already has it. The
// SigV4 header builder is the crate's one too, and it signs for the same Region the hosts
// below name.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::rds_docdb_neptune_signing_name_routing::authorization;

/// The Region every request here is signed for — the Region `wait_for_stack_status` polls in
/// — and so the Region the stack's resources belong to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The `StackStatus` the rule is stated about.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The tag keys CloudFormation records on every resource it provisions. Source: the
/// `aws:cloudformation:*` tags CloudFormation's own documentation names (this emulator's
/// `service-com-amazonaws-cloudformation-cloudformation`
/// `state::provisioning::{STACK_NAME_TAG, STACK_ID_TAG, LOGICAL_ID_TAG}`).
const STACK_NAME_TAG: &str = "aws:cloudformation:stack-name";
const STACK_ID_TAG: &str = "aws:cloudformation:stack-id";
const LOGICAL_ID_TAG: &str = "aws:cloudformation:logical-id";

/// The stack-level tag every case's stack operation supplies, which CloudFormation
/// propagates to the resources it creates in the stack. An ordinary key: it is a caller's
/// tag, not one of the three above.
const STACK_TAG: (&str, &str) = ("Team", "platform");

/// The execution role the template declares, and the principal its trust policy admits —
/// what Lambda has to be able to assume for a function to be created with it.
const EXEC_ROLE_LOGICAL_ID: &str = "ExecRole";
const EXEC_ROLE_NAME: &str = "blueprint-exec";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The `service:resourceType` key a qualified `ResourceTypeFilters` entry selects a Lambda
/// function by.
const FUNCTION_RESOURCE_TYPE: &str = "lambda:function";

/// One `AWS::Lambda::Function` the template declares.
struct Declared {
    case: &'static str,
    /// The logical id it is declared under — the value `aws:cloudformation:logical-id` must
    /// carry for this resource and no other.
    logical_id: &'static str,
    /// The `FunctionName` the template declares, or `None` for a resource whose name
    /// CloudFormation generates.
    name: Option<&'static str>,
    /// The resource's own `Tags` property, in the order the template writes it.
    tags: &'static [(&'static str, &'static str)],
}

/// The resources one stack declares, spanning what the rule generalises over: a declaration
/// asking for ONE user tag, one asking for NONE at all, and one asking for two whose name
/// the template leaves to CloudFormation.
const DECLARED: &[Declared] = &[
    Declared {
        case: "a function whose declaration asks for one user tag",
        logical_id: "TaggedFunction",
        name: Some("blueprint-tagged"),
        tags: &[("lambda-console:blueprint", "true")],
    },
    Declared {
        case: "a function whose declaration asks for no Tags at all",
        logical_id: "PlainFunction",
        name: Some("blueprint-plain"),
        tags: &[],
    },
    Declared {
        case: "a function with two user tags whose name CloudFormation generates",
        logical_id: "ReportFunction",
        name: None,
        tags: &[("Owner", "reports"), ("Tier", "prod")],
    },
];

/// The emulator with the four services this suite exercises, mounted on ONE engine exactly
/// as `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way
/// (the credential scope is what routes it).
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
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_resourcegroupstaggingapi_resourcegroupstaggingapi_20170126::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// Call one CloudFormation operation over its awsQuery wire.
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

/// Call one Lambda operation over its restJson1 wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(document) => serde_json::to_vec(document).expect("the request body serializes"),
        None => Vec::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// Call one Resource Groups Tagging API operation over its awsJson1_1 wire.
async fn tagging(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("tagging.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("ResourceGroupsTaggingAPI_20170126.{operation}"),
        )
        .header("authorization", authorization("tagging"))
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let answered: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, answered)
}

/// The template every case deploys: the execution role, every resource of [`DECLARED`], and
/// one `Ref` and one `Fn::GetAtt … Arn` output per declared function.
fn template() -> String {
    let mut resources = Map::new();
    resources.insert(
        EXEC_ROLE_LOGICAL_ID.to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": EXEC_ROLE_NAME,
                "AssumeRolePolicyDocument": {
                    "Version": "2012-10-17",
                    "Statement": [{
                        "Effect": "Allow",
                        "Principal": { "Service": LAMBDA_PRINCIPAL },
                        "Action": "sts:AssumeRole"
                    }]
                }
            }
        }),
    );
    let mut outputs = Map::new();
    for declared in DECLARED {
        let mut properties = json!({
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Role": { "Fn::GetAtt": [EXEC_ROLE_LOGICAL_ID, "Arn"] },
            "Code": { "ZipFile": "def handler(event, context):\n    return {'ok': True}\n" },
        });
        if let Some(name) = declared.name {
            properties["FunctionName"] = json!(name);
        }
        if !declared.tags.is_empty() {
            properties["Tags"] = Value::Array(
                declared
                    .tags
                    .iter()
                    .map(|(key, value)| json!({ "Key": key, "Value": value }))
                    .collect(),
            );
        }
        resources.insert(
            declared.logical_id.to_string(),
            json!({ "Type": "AWS::Lambda::Function", "Properties": properties }),
        );
        outputs.insert(
            format!("{}Ref", declared.logical_id),
            json!({ "Value": { "Ref": declared.logical_id } }),
        );
        outputs.insert(
            format!("{}Arn", declared.logical_id),
            json!({ "Value": { "Fn::GetAtt": [declared.logical_id, "Arn"] } }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// What one deployed stack published: its id, and the `(function name, ARN)` the outputs
/// report for each declared resource, keyed by logical id.
struct Deployed {
    stack_name: String,
    stack_id: String,
    functions: BTreeMap<String, (String, String)>,
}

/// Deploy the template under `stack_name`, with [`STACK_TAG`] as the stack's own tag set,
/// and read back what the completed stack published.
async fn deploy(router: &MultiServiceRouter, stack_name: &str) -> Deployed {
    let body = template();
    let (status, created) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", &body),
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
            ("Tags.member.1.Key", STACK_TAG.0),
            ("Tags.member.1.Value", STACK_TAG.1),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateStack {stack_name:?}: expected HTTP 200, got {status} with body {created}"
    );

    let (reached, described) = wait_for_stack_status(router, stack_name, CREATE_COMPLETE).await;
    assert_eq!(
        reached, CREATE_COMPLETE,
        "stack {stack_name:?}: expected {CREATE_COMPLETE}, got {reached:?} from {described}"
    );

    let stack_id = opt_element(&described, "StackId")
        .unwrap_or_else(|| panic!("stack {stack_name:?}: no <StackId> in {described}"));
    let outputs = outputs_of(&described);
    let mut functions = BTreeMap::new();
    for declared in DECLARED {
        let case = declared.case;
        let reference = outputs
            .get(&format!("{}Ref", declared.logical_id))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no Ref output for {}: {described}",
                    declared.logical_id
                )
            })
            .clone();
        let arn = outputs
            .get(&format!("{}Arn", declared.logical_id))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: no Arn output for {}: {described}",
                    declared.logical_id
                )
            })
            .clone();
        if let Some(name) = declared.name {
            assert_eq!(
                reference, name,
                "case {case:?}: expected the declared function name {name:?}, got {reference:?}"
            );
        }
        assert_eq!(
            arn,
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{reference}"),
            "case {case:?}: expected the unqualified ARN of the published function name \
             {reference:?}, got {arn:?}"
        );
        functions.insert(declared.logical_id.to_string(), (reference, arn));
    }
    Deployed {
        stack_name: stack_name.to_string(),
        stack_id,
        functions,
    }
}

/// The tag set the rule says one declared resource carries: the three CloudFormation records
/// itself, the stack-level tag it propagates, and the resource's own declaration.
fn expected_tags(declared: &Declared, deployed: &Deployed) -> BTreeMap<String, String> {
    let mut expected: BTreeMap<String, String> = declared
        .tags
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    expected.insert(STACK_TAG.0.to_string(), STACK_TAG.1.to_string());
    expected.insert(STACK_NAME_TAG.to_string(), deployed.stack_name.clone());
    expected.insert(STACK_ID_TAG.to_string(), deployed.stack_id.clone());
    expected.insert(LOGICAL_ID_TAG.to_string(), declared.logical_id.to_string());
    expected
}

/// The three CloudFormation records itself, for one declared resource.
fn system_tags(declared: &Declared, deployed: &Deployed) -> BTreeMap<String, String> {
    [
        (STACK_NAME_TAG.to_string(), deployed.stack_name.clone()),
        (STACK_ID_TAG.to_string(), deployed.stack_id.clone()),
        (LOGICAL_ID_TAG.to_string(), declared.logical_id.to_string()),
    ]
    .into_iter()
    .collect()
}

/// The tag map Lambda's own `ListTags` reports for one function ARN.
async fn listed_tags(
    case: &str,
    router: &MultiServiceRouter,
    arn: &str,
) -> BTreeMap<String, String> {
    let (status, listed) = lambda(router, "GET", &format!("/2017-03-31/tags/{arn}"), None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListTags on {arn}, got {status} with body {listed}"
    );
    listed
        .get("Tags")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("case {case:?}: no 'Tags' map in {listed}"))
        .iter()
        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
        .collect()
}

/// `UntagResource` for `keys` on `arn`: `tagKeys` is an `@httpQuery` list, so each key
/// travels as its own repeated `tagKeys=` parameter.
async fn untag(case: &str, router: &MultiServiceRouter, arn: &str, keys: &[&str]) {
    let query: Vec<String> = keys
        .iter()
        .map(|key| format!("tagKeys={}", form_encode(key)))
        .collect();
    let uri = format!("/2017-03-31/tags/{arn}?{}", query.join("&"));
    let (status, answered) = lambda(router, "DELETE", &uri, None).await;
    assert_eq!(
        status, 204,
        "case {case:?}: expected HTTP 204 from UntagResource {keys:?}, got {status} with body \
         {answered}"
    );
}

/// The ARNs `GetResources` selects for one request, sorted, with the whole answer for a
/// failure message.
async fn selected(
    case: &str,
    router: &MultiServiceRouter,
    request: &Value,
) -> (Vec<String>, Value) {
    let (status, body) = tagging(router, "GetResources", request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from GetResources {request}, got {status} with body \
         {body}"
    );
    let mut arns: Vec<String> = body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no ResourceTagMappingList in {body}"))
        .iter()
        .map(|mapping| {
            mapping["ResourceARN"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a mapping with no ResourceARN: {body}"))
                .to_string()
        })
        .collect();
    arns.sort();
    (arns, body)
}

/// The tag map one `GetResources` entry reports for `arn`.
fn indexed_tags(case: &str, body: &Value, arn: &str) -> BTreeMap<String, String> {
    let mapping = body["ResourceTagMappingList"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no ResourceTagMappingList in {body}"))
        .iter()
        .find(|mapping| mapping["ResourceARN"] == json!(arn))
        .unwrap_or_else(|| panic!("case {case:?}: {arn} is not in the page {body}"));
    mapping["Tags"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a mapping with no Tags: {mapping}"))
        .iter()
        .filter_map(|tag| {
            Some((
                tag["Key"].as_str()?.to_string(),
                tag["Value"].as_str()?.to_string(),
            ))
        })
        .collect()
}

/// Every function the stack provisioned carries the three tags CloudFormation records about
/// it, beside the stack's own tag and whatever its declaration asked for — and the tagging
/// index reads that same set, so a filter on the stack name answers with the whole estate and
/// one on a logical id with the single resource declared under it.
#[tokio::test]
async fn a_stack_stamps_its_three_tags_on_every_function_it_provisions() {
    let router = build_router();
    let deployed = deploy(&router, "blueprint-provenance").await;

    for declared in DECLARED {
        let case = declared.case;
        let (_, arn) = &deployed.functions[declared.logical_id];
        let expected = expected_tags(declared, &deployed);
        let reported = listed_tags(case, &router, arn).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected ListTags to report exactly {expected:?}, got {reported:?}"
        );

        // The same tag set through the tagging index, selected by the resource's own logical
        // id: the three are ordinary entries of the map every reader sees.
        let request = json!({
            "TagFilters": [{ "Key": LOGICAL_ID_TAG, "Values": [declared.logical_id] }],
            "ResourceTypeFilters": [FUNCTION_RESOURCE_TYPE],
        });
        let (selected_arns, body) = selected(case, &router, &request).await;
        assert_eq!(
            selected_arns,
            vec![arn.clone()],
            "case {case:?}: expected a filter on {LOGICAL_ID_TAG}={:?} to select {arn} alone, \
             got {selected_arns:?}",
            declared.logical_id
        );
        let indexed = indexed_tags(case, &body, arn);
        assert_eq!(
            indexed, expected,
            "case {case:?}: expected the index entry to carry exactly {expected:?}, got \
             {indexed:?}"
        );
    }

    // Stack-scoped inventory: the stack-name tag selects every function the stack owns.
    let case = "every function of the stack, by stack name";
    let mut whole_estate: Vec<String> = DECLARED
        .iter()
        .map(|declared| deployed.functions[declared.logical_id].1.clone())
        .collect();
    whole_estate.sort();
    let request = json!({
        "TagFilters": [{ "Key": STACK_NAME_TAG, "Values": [deployed.stack_name.clone()] }],
        "ResourceTypeFilters": [FUNCTION_RESOURCE_TYPE],
    });
    let (selected_arns, _) = selected(case, &router, &request).await;
    assert_eq!(
        selected_arns, whole_estate,
        "case {case:?}: expected a filter on {STACK_NAME_TAG}={:?} to select every function the \
         stack created {whole_estate:?}, got {selected_arns:?}",
        deployed.stack_name
    );
}

/// A caller removing the tags that are ITS own — the stack-level tag and whatever the
/// template declared — leaves the three CloudFormation wrote in place, so what the stack owns
/// is still attributable afterwards.
#[tokio::test]
async fn a_callers_untag_of_its_own_tags_leaves_the_three_in_place() {
    let router = build_router();
    let deployed = deploy(&router, "blueprint-untagged").await;

    for declared in DECLARED {
        let case = declared.case;
        let (_, arn) = &deployed.functions[declared.logical_id];
        let mut caller_keys: Vec<&str> = declared.tags.iter().map(|(key, _)| *key).collect();
        caller_keys.push(STACK_TAG.0);
        untag(case, &router, arn, &caller_keys).await;

        let expected = system_tags(declared, &deployed);
        let reported = listed_tags(case, &router, arn).await;
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the three CloudFormation tags {expected:?} to survive an \
             untag of {caller_keys:?}, got {reported:?}"
        );

        // The index reads that same set: the user tags are gone from it, the three are not.
        let request = json!({
            "TagFilters": [{ "Key": LOGICAL_ID_TAG, "Values": [declared.logical_id] }],
            "ResourceTypeFilters": [FUNCTION_RESOURCE_TYPE],
        });
        let (selected_arns, body) = selected(case, &router, &request).await;
        assert_eq!(
            selected_arns,
            vec![arn.clone()],
            "case {case:?}: expected {arn} to still be selected by {LOGICAL_ID_TAG} after the \
             untag, got {selected_arns:?}"
        );
        let indexed = indexed_tags(case, &body, arn);
        assert_eq!(
            indexed, expected,
            "case {case:?}: expected the index entry to carry exactly {expected:?} after the \
             untag, got {indexed:?}"
        );

        for (key, _) in declared.tags {
            let request = json!({
                "TagFilters": [{ "Key": key }],
                "ResourceTypeFilters": [FUNCTION_RESOURCE_TYPE],
            });
            let (still_selected, _) = selected(case, &router, &request).await;
            assert!(
                !still_selected.contains(arn),
                "case {case:?}: expected the removed key {key:?} to select {arn} no longer, got \
                 {still_selected:?}"
            );
        }
    }
}
