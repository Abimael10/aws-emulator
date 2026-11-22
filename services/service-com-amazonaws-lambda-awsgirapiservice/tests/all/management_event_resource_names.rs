// SPDX-License-Identifier: CC0-1.0

//! Which RESOURCE the record of a control-plane call of this front door names.
//!
//! RULE: a service that records a management call names, in that record's resource list,
//! each resource of its own the call acted on — by that resource's type and its
//! user-created name — so a lookup by `ResourceName` or `ResourceType` selects the call.
//! For this service the resource is the FUNCTION the request addressed, named by the
//! function's own name whichever of the documented reference forms the caller wrote and
//! whichever member carried it; a call that addresses no function of this service, and one
//! the service refused because the FUNCTION is not there, name none — while a refusal about
//! a sub-resource of a function that resolved acted on that function and names it.
//!
//! The rows below span what the rule generalises over rather than the create that motivated
//! it: a write that MAKES the function, a read of it, a grant on its policy, a write
//! addressing it by its ARN through a differently-named member, a listing that addresses no
//! function at all, the refusal of a read naming a function the account does not hold, and
//! the refusal of a read of a sub-resource the function itself does hold no instance of.
//!
//! Every call goes over the wire through the same router the production emulator uses (the
//! harness `function_lifecycle.rs` publishes); the records are read back out of the SHARED
//! log in the engine the service was built from — the one store CloudTrail's `LookupEvents`
//! reads, and the one its `ResourceName` lookup filters. The end-to-end leg through that
//! operation is `core/router/tests/all/cloudtrail_lambda_resource_name_lookup.rs`, which
//! needs CloudTrail mounted beside this service.

use std::sync::Arc;

use serde_json::{json, Value};

use service_com_amazonaws_lambda_awsgirapiservice::service::management_events::{
    published_event_name, FUNCTION_RESOURCE_TYPE,
};
use service_common::management_events::ManagementEventLog;

use crate::function_lifecycle::{
    create_function, function_arn, json_call, make_activation_due, make_update_due, package_bytes,
    router_on, zip_file, ROLE,
};

/// The function every call below addresses, and the name its records must report.
const FUNCTION: &str = "resource-named";

/// A function name the account holds nothing under, which is what makes the read of it a
/// refusal.
const ABSENT: &str = "resource-never-made";

/// The resources the one record of `operation` names, as `(type, name)` pairs.
fn named_resources(engine: &Arc<engine::InMemoryEngine>, operation: &str) -> Vec<(String, String)> {
    let published = published_event_name(operation);
    let log = engine.state::<ManagementEventLog>();
    let held = log.lock();
    let found: Vec<&service_common::management_events::ManagementEvent> = held
        .iter()
        .filter(|event| event.call.event_name == published)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {operation:?}: expected exactly one record under {published:?}, got {} \
         (every recorded name: {:?})",
        found.len(),
        held.iter()
            .map(|event| event.call.event_name.clone())
            .collect::<Vec<String>>()
    );
    found[0]
        .call
        .resources
        .iter()
        .map(|resource| {
            (
                resource.resource_type.clone(),
                resource.resource_name.clone(),
            )
        })
        .collect()
}

/// One call of the table: what it sends, what its caller sees, and the resources its record
/// must name.
struct Row {
    case: &'static str,
    /// The operation's modelled name — the catalog's key, not the published event name.
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
    status: u16,
    /// The resource names the record must carry, each of the function resource type.
    resources: Vec<String>,
}

#[tokio::test]
async fn a_management_record_names_the_function_the_call_acted_on() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let package = package_bytes(96);

    // The subject of the reads below, created through this service's own API — so its
    // create is itself one of the rows asserted on.
    let created = create_function(
        &router,
        &json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": zip_file(&package) },
        }),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "setup: CreateFunction must succeed, got {} with {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(&engine);

    let rows = vec![
        Row {
            case: "the create that made the function",
            operation: "CreateFunction",
            method: "GET",
            uri: String::new(),
            body: Value::Null,
            status: 201,
            resources: vec![FUNCTION.to_string()],
        },
        Row {
            case: "the read of that function",
            operation: "GetFunction",
            method: "GET",
            uri: format!("/2015-03-31/functions/{FUNCTION}"),
            body: Value::Null,
            status: 200,
            resources: vec![FUNCTION.to_string()],
        },
        Row {
            case: "a grant on the function's policy",
            operation: "AddPermission",
            method: "POST",
            uri: format!("/2015-03-31/functions/{FUNCTION}/policy"),
            body: json!({
                "StatementId": "named",
                "Action": "lambda:InvokeFunction",
                "Principal": "s3.amazonaws.com",
            }),
            status: 201,
            resources: vec![FUNCTION.to_string()],
        },
        Row {
            case: "a tagging write, which addresses the function by ARN at another member",
            operation: "TagResource",
            method: "POST",
            uri: format!("/2017-03-31/tags/{}", function_arn(FUNCTION)),
            body: json!({ "Tags": { "owner": "audit" } }),
            status: 204,
            resources: vec![FUNCTION.to_string()],
        },
        Row {
            case: "a list over the account, which addresses no function",
            operation: "ListFunctions",
            method: "GET",
            uri: "/2015-03-31/functions".to_string(),
            body: Value::Null,
            status: 200,
            resources: Vec::new(),
        },
        Row {
            case: "the read of a function the account does not hold, which is refused",
            operation: "GetFunctionConfiguration",
            method: "GET",
            uri: format!("/2015-03-31/functions/{ABSENT}/configuration"),
            body: Value::Null,
            status: 404,
            resources: Vec::new(),
        },
        Row {
            case: "the refused read of a sub-resource of a function that DID resolve",
            operation: "GetAlias",
            method: "GET",
            uri: format!("/2015-03-31/functions/{FUNCTION}/aliases/no-such-alias"),
            body: Value::Null,
            status: 404,
            resources: vec![FUNCTION.to_string()],
        },
    ];

    for row in &rows {
        // The create is the row made in the setup above, so it is asserted on and not
        // repeated: making it twice would be a second create of one function.
        if !row.uri.is_empty() {
            make_update_due(&engine);
            let answered = json_call(&router, row.method, &row.uri, &row.body).await;
            assert_eq!(
                answered.status, row.status,
                "case {:?}: expected HTTP {}, got {} with {}",
                row.case,
                row.status,
                answered.status,
                String::from_utf8_lossy(&answered.body)
            );
        }
        let actual = named_resources(&engine, row.operation);
        let expected: Vec<(String, String)> = row
            .resources
            .iter()
            .map(|name| (FUNCTION_RESOURCE_TYPE.to_string(), name.clone()))
            .collect();
        assert_eq!(
            actual, expected,
            "case {:?}: expected the record to name {expected:?}, got {actual:?}",
            row.case
        );
    }
}
