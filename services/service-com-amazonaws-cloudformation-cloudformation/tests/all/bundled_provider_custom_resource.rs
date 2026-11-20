// SPDX-License-Identifier: CC0-1.0

//! A custom resource whose provider is a BUNDLED one — a provider whose module body imports
//! the child-process entry point its SDK installer uses, which is the shape every provider a
//! CDK custom resource deploys takes — reaches a terminal state on that provider's own answer.
//!
//! The rule under test: **a stack's custom resource settles on the answer its provider sends
//! to the request's `ResponseURL`, and a provider written for a managed node runtime can send
//! one whatever module of that runtime's standard library its module body imports. A runtime
//! that has none of the host capability such a module needs refuses the CALL that needs it,
//! never the import, so the provider loads, falls back the way its own code says it falls
//! back, makes the API call the resource was declared for, and answers — on the create and on
//! the delete alike.**
//!
//! That is what the no-response sentence is the absence of: a provider whose module body
//! cannot be imported never runs, so nothing is ever PUT to the `ResponseURL` and
//! CloudFormation fails the resource with "CloudFormation did not receive a response from your
//! Custom Resource" — which this suite asserts is nowhere in the stack's own events.
//!
//! Every row drives the emulator through its own wire: the stack operations over
//! CloudFormation's awsQuery endpoint, the provider created in AWS Lambda by the stack itself,
//! the answer sent by the provider's own code, and the item the provider's API call wrote read
//! back through Amazon DynamoDB's own API — so a row whose provider answered without doing the
//! work fails here.
//!
//! The rows differ along what the rule generalises over: whether the provider is asked to
//! install the latest SDK (the property the CDK's own resource carries) and so whether it
//! reaches its fallback at all, how it loads the package (a `require` and a dynamic `import`),
//! and the type the custom resource is declared with.

use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use super::harness::{
    call, create_stack, member, router_with_engine, settled_stack, xml_text, xml_texts, REGION,
};
use crate::dynamodb_table_resources::dynamodb;

/// The sentence CloudFormation words a resource's failure with when no provider answer ever
/// arrives — the fixed text, whose one variable part is the request id. No row may produce it.
const NO_RESPONSE: &str = "CloudFormation did not receive a response from your Custom Resource.";

/// The provider every row deploys: the shape a bundled custom-resource provider has.
///
/// Its module body imports the child-process entry point its SDK installer uses — which is
/// where a runtime supplying no such module fails, before the handler runs at all — the
/// handler tries that install, falls back to the SDK the runtime itself supplies when the
/// install is refused, makes the call the resource's own properties declare, and PUTs the
/// response document to the request's `ResponseURL`.
const BUNDLED_PROVIDER: &str = r#"
'use strict';

const { execSync } = require('child_process');
const https = require('https');
const url = require('url');

// The SDK the call is made with: the latest release where it could be installed, and the one
// the runtime supplies otherwise.
async function loadAwsSdk(packageName, installLatestAwsSdk, loader) {
    const load = (specifier) => (loader === 'require'
        ? Promise.resolve(require(specifier))
        : import(specifier));
    try {
        if (installLatestAwsSdk !== 'true') {
            return { sdk: await load(packageName), installed: 'not-asked-for' };
        }
        execSync('HOME=/tmp npm install ' + packageName + ' --omit=dev --prefix /tmp');
        return { sdk: await load('/tmp/node_modules/' + packageName), installed: 'installed' };
    } catch (error) {
        return { sdk: await load(packageName), installed: String(error.code) };
    }
}

// One call of the SDK, named the way the resource's properties name it.
async function invoke(call, properties) {
    const packageName = '@aws-sdk/client-' + call.service.toLowerCase();
    const loaded = await loadAwsSdk(
        packageName,
        properties.InstallLatestAwsSdk,
        properties.Loader
    );
    const entries = Object.entries(loaded.sdk);
    const client = entries.find(([name]) => name.endsWith('Client') && name !== 'Client');
    const wanted = (call.action.charAt(0).toUpperCase() + call.action.slice(1) + 'Command')
        .toLowerCase();
    const command = entries.find(([name]) => name.toLowerCase() === wanted);
    const calling = new client[1]({ region: call.region || process.env.AWS_REGION });
    const answered = await calling.send(new command[1](call.parameters));
    return {
        Installed: loaded.installed,
        Region: await calling.config.region().catch(() => 'unresolved'),
        Status: String(answered.$metadata.httpStatusCode),
    };
}

// The response document, PUT to the address the request carried.
function respond(event, status, reason, physicalResourceId, data) {
    const document = JSON.stringify({
        Status: status,
        Reason: reason,
        PhysicalResourceId: physicalResourceId,
        StackId: event.StackId,
        RequestId: event.RequestId,
        LogicalResourceId: event.LogicalResourceId,
        NoEcho: false,
        Data: data,
    });
    const parsed = url.parse(event.ResponseURL);
    return new Promise((resolve, reject) => {
        try {
            const request = https.request({
                hostname: parsed.hostname,
                path: parsed.path,
                method: 'PUT',
                headers: {
                    'content-type': '',
                    'content-length': Buffer.byteLength(document, 'utf8'),
                },
            }, () => resolve());
            request.on('error', reject);
            request.write(document);
            request.end();
        } catch (error) {
            reject(error);
        }
    });
}

exports.handler = async (event, context) => {
    const properties = event.ResourceProperties || {};
    const stated = properties[event.RequestType];
    const call = stated === undefined ? null : JSON.parse(stated);
    const physicalResourceId = (call && call.physicalResourceId && call.physicalResourceId.id)
        || event.PhysicalResourceId
        || event.LogicalResourceId;
    let status = 'SUCCESS';
    let reason = 'OK';
    let data = {};
    try {
        if (call) {
            data = await invoke(call, properties);
        }
    } catch (error) {
        status = 'FAILED';
        reason = error.message;
    }
    await respond(event, status, reason, physicalResourceId, data);
    return {};
};
"#;

/// One row: the stack and the table it deploys, the custom resource's declared type, and what
/// the provider is asked to do.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    table_name: &'static str,
    resource_type: &'static str,
    /// The property the CDK's own resource carries, as a template states a boolean.
    install_latest: &'static str,
    /// How the provider loads the SDK package.
    loader: &'static str,
    /// What the install attempt must have come to, which the provider answers as `Data`.
    installed: &'static str,
    /// The item the provider's call writes.
    item_key: &'static str,
    item_status: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        // The CDK's own default: the provider is asked to install the latest SDK, cannot — an
        // invocation of this emulator starts no child process — and falls back to the SDK the
        // runtime supplies, which is the path its own code takes.
        Row {
            case: "a provider asked to install the latest SDK, loading with require",
            stack_name: "bundled-provider-install",
            table_name: "bundled-provider-install-items",
            resource_type: "Custom::AWS",
            install_latest: "true",
            loader: "require",
            installed: "ENOSYS",
            item_key: "user-001",
            item_status: "active",
        },
        // The same provider on the pre-installed SDK, loaded with a dynamic import — the other
        // form a bundled provider loads a package in.
        Row {
            case: "a provider on the pre-installed SDK, loading with a dynamic import",
            stack_name: "bundled-provider-preinstalled",
            table_name: "bundled-provider-preinstalled-items",
            resource_type: "AWS::CloudFormation::CustomResource",
            install_latest: "false",
            loader: "import",
            installed: "not-asked-for",
            item_key: "user-005",
            item_status: "pending",
        },
    ]
}

/// The template one row deploys: the execution role the provider assumes, with the grant its
/// own call needs; the table its call writes; the provider itself; and the custom resource
/// that provider backs, whose properties declare the call the way the CDK's own resource
/// declares it — one JSON document per request type.
fn template(row: &Row) -> String {
    let create = json!({
        "service": "DynamoDB",
        "action": "putItem",
        "region": REGION,
        "physicalResourceId": {"id": "populate-items"},
        "parameters": {
            "TableName": row.table_name,
            "Item": {
                "id": {"S": row.item_key},
                "status": {"S": row.item_status},
            },
        },
    })
    .to_string();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Role": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "AssumeRolePolicyDocument": {
                        "Version": "2012-10-17",
                        "Statement": [{
                            "Action": "sts:AssumeRole",
                            "Effect": "Allow",
                            "Principal": {"Service": "lambda.amazonaws.com"},
                        }],
                    },
                    "Policies": [{
                        "PolicyName": "PopulateItems",
                        "PolicyDocument": {
                            "Version": "2012-10-17",
                            "Statement": [{
                                "Action": "dynamodb:*",
                                "Effect": "Allow",
                                "Resource": "*",
                            }],
                        },
                    }],
                },
            },
            "Table": {
                "Type": "AWS::DynamoDB::Table",
                "Properties": {
                    "TableName": row.table_name,
                    "KeySchema": [{"AttributeName": "id", "KeyType": "HASH"}],
                    "AttributeDefinitions": [{"AttributeName": "id", "AttributeType": "S"}],
                    "BillingMode": "PAY_PER_REQUEST",
                },
            },
            "Provider": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "Code": {"ZipFile": BUNDLED_PROVIDER},
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Runtime": "nodejs22.x",
                    "Timeout": 60,
                },
            },
            "PopulateItems": {
                "Type": row.resource_type,
                // The call writes the table, so the resource is deployed after it: a write to
                // a table that is still coming up is refused by DynamoDB itself.
                "DependsOn": "Table",
                "Properties": {
                    "ServiceToken": {"Fn::GetAtt": ["Provider", "Arn"]},
                    "InstallLatestAwsSdk": row.install_latest,
                    "Loader": row.loader,
                    "Create": create,
                },
            },
        },
        "Outputs": {
            "ResourceRef": {"Value": {"Ref": "PopulateItems"}},
            "Installed": {"Value": {"Fn::GetAtt": ["PopulateItems", "Installed"]}},
            "CallRegion": {"Value": {"Fn::GetAtt": ["PopulateItems", "Region"]}},
            "CallStatus": {"Value": {"Fn::GetAtt": ["PopulateItems", "Status"]}},
        },
    })
    .to_string()
}

/// A stack's reported outputs, as the key/value map a client reads.
fn outputs_of(xml: &str) -> std::collections::BTreeMap<String, String> {
    xml_texts(xml, "OutputKey")
        .into_iter()
        .zip(xml_texts(xml, "OutputValue"))
        .collect()
}

/// Every `ResourceStatusReason` the stack's own events carry, so a row can assert that the
/// no-response sentence is not among them.
async fn event_reasons(router: &MultiServiceRouter, stack_name: &str) -> Vec<String> {
    let (status_code, body) =
        call(router, "DescribeStackEvents", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status_code, 200,
        "expected DescribeStackEvents to answer for {stack_name:?}, got {status_code} with {body}"
    );
    xml_texts(&body, "ResourceStatusReason")
}

#[tokio::test]
async fn a_bundled_provider_answers_its_request_and_the_resource_settles_on_it() {
    for row in rows() {
        let case = row.case;
        let (router, _engine) = router_with_engine();
        let (status_code, created) = create_stack(
            &router,
            row.stack_name,
            &template(&row),
            &[member("Capabilities.member.1", "CAPABILITY_NAMED_IAM")],
        )
        .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: expected CreateStack to be accepted, got {status_code} with {created}"
        );
        let settled = settled_stack(case, &router, REGION, row.stack_name).await;
        let reasons = event_reasons(&router, row.stack_name).await;
        assert!(
            !reasons.iter().any(|reason| reason.contains(NO_RESPONSE)),
            "case {case:?}: no event may report the no-response sentence, got {reasons:?}"
        );
        assert_eq!(
            xml_text(&settled, "StackStatus").unwrap_or_default(),
            "CREATE_COMPLETE",
            "case {case:?}: expected the stack to complete on its provider's answer, got \
             {settled} with {reasons:?}"
        );
        // The provider's answer IS the resource: the id it reported is what `Ref` resolves to
        // and the `Data` it published is what `Fn::GetAtt` resolves against.
        let outputs = outputs_of(&settled);
        assert_eq!(
            outputs.get("ResourceRef").map(String::as_str),
            Some("populate-items"),
            "case {case:?}: expected Ref to resolve to the answered id, got {outputs:?}"
        );
        assert_eq!(
            outputs.get("Installed").map(String::as_str),
            Some(row.installed),
            "case {case:?}: expected the install attempt to come to {:?}, got {outputs:?}",
            row.installed
        );
        assert_eq!(
            outputs.get("CallRegion").map(String::as_str),
            Some(REGION),
            "case {case:?}: expected the client's own Region provider to have answered the \
             Region the call was made in, got {outputs:?}"
        );
        assert_eq!(
            outputs.get("CallStatus").map(String::as_str),
            Some("200"),
            "case {case:?}: expected the provider's API call to have been answered, got \
             {outputs:?}"
        );
        // What the TABLE holds, read through DynamoDB's own API: the write the provider's call
        // made is really there.
        let (read_status, read) = dynamodb(
            &router,
            "GetItem",
            &json!({
                "TableName": row.table_name,
                "Key": {"id": {"S": row.item_key}},
            }),
        )
        .await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected GetItem to answer, got {read_status} with {read}"
        );
        assert_eq!(
            read["Item"]["status"],
            json!({"S": row.item_status}),
            "case {case:?}: expected the item the provider's call wrote to be on the table, got \
             {read}"
        );

        // ...and the DELETE is answered too, so the stack a client stood up comes down: the
        // provider is sent the `Delete` request and answers it the same way.
        let (deleted_code, deleted) =
            call(&router, "DeleteStack", &[member("StackName", row.stack_name)]).await;
        assert_eq!(
            deleted_code, 200,
            "case {case:?}: expected DeleteStack to be accepted, got {deleted_code} with {deleted}"
        );
        let after = settled_stack(case, &router, REGION, row.stack_name).await;
        let status_after = if after.is_empty() {
            // A completed deletion is a stack this service no longer reports by name.
            "DELETE_COMPLETE".to_string()
        } else {
            xml_text(&after, "StackStatus").unwrap_or_default()
        };
        assert_eq!(
            status_after, "DELETE_COMPLETE",
            "case {case:?}: expected the delete to settle on the provider's answer, got \
             {status_after:?} with {after}"
        );
    }
}
