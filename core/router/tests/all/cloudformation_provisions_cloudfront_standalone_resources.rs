// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares one of the CloudFront resources whose create takes the
//! declaration's own properties — an `AWS::CloudFront::KeyValueStore`, an
//! `AWS::CloudFront::Function`, an `AWS::CloudFront::ConnectionFunction`, an
//! `AWS::CloudFront::ConnectionGroup` or an `AWS::CloudFront::MonitoringSubscription` — really
//! creates it in Amazon CloudFront, and the identity the stack publishes is the one that
//! service's own read answers to.
//!
//! The rule under test is the one
//! `cloudformation_provisions_cloudfront_policies_and_keys.rs` states for the types declared as
//! one configuration: **a resource type CloudFormation recognises and whose owning service this
//! emulator mounts is created through that service's own API, under the identity that service
//! mints — so the stack's `Ref`/`Fn::GetAtt` outputs carry a value that service's own read
//! resolves to the resource the template configured, and deleting the stack takes it away
//! there.** These are the types that reach it the other way round: the create's payload is the
//! operation's own request shape rather than a configuration the type publishes, and the
//! identity is an ARN, an id or the distribution the resource hangs off.
//!
//! The rows differ along what the rule generalises over: which resource type is declared,
//! whether the identity the stack publishes is an ARN (a key value store, a function, a
//! connection function), an id CloudFront minted (a connection group) or the id of the
//! resource it hangs off (a monitoring subscription), whether the declaration carries code to
//! be published and a list the model wraps in a `{Quantity, Items}` container (a function's key
//! value store associations), whether the template is written in JSON or YAML, and whether the
//! resource depends on another the same stack creates.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{elements, form_encode, opt_element, outputs_of};

/// How long a stack operation is given to reach its terminal status. GENEROUS: what is waited
/// for is the CONDITION (the status the stack reports), polled until it holds, so the only
/// thing this bound decides is when a stack that never settles is reported as a failure.
const SETTLES_WITHIN: Duration = Duration::from_secs(60);

/// How often the stack's status is read while waiting for it to settle.
const POLL_EVERY: Duration = Duration::from_millis(50);

/// The Region every row deploys in. CloudFront is a global service, and the Region is what the
/// credential scope of each call names.
const REGION: &str = "us-east-1";

/// The logical id each row declares the resource under test as.
const LOGICAL_ID: &str = "TheResource";

/// The emulator with the three services these rows exercise, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them.
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
        service_com_amazonaws_cloudfront_cloudfront2020_05_31::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
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
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one CloudFront read over its restXml wire.
async fn cloudfront(router: &MultiServiceRouter, path: &str) -> (u16, String) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", format!("cloudfront.{REGION}.amazonaws.com"))
        .header("authorization", authorization("cloudfront"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Read the stack's status until it is one `DescribeStacks` reports as terminal, or until
/// [`SETTLES_WITHIN`] has passed. The CONDITION is what is waited for, not a fixed span.
async fn settled_stack(router: &MultiServiceRouter, stack_name: &str, case: &str) -> String {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body \
             {described}"
        );
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if !reported.ends_with("_IN_PROGRESS") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack was still {reported:?} when the wait ran out"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// Read the stack until `DescribeStacks` no longer reports it at all, or until
/// [`SETTLES_WITHIN`] has passed — which is how a client watches a deletion through.
async fn deleted_stack(router: &MultiServiceRouter, stack_name: &str, case: &str) {
    let deadline = Instant::now() + SETTLES_WITHIN;
    loop {
        let (_, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        let reported = opt_element(&described, "StackStatus").unwrap_or_default();
        if reported == "DELETE_COMPLETE" || reported.is_empty() {
            return;
        }
        assert_ne!(
            reported, "DELETE_FAILED",
            "case {case:?}: expected the stack's resources to be deleted, got {described}"
        );
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack was still {reported:?} when the wait ran out"
        );
        tokio::time::sleep(POLL_EVERY).await;
    }
}

/// The source a row's function carries, which travels to CloudFront as the base64 a blob member
/// takes on the wire and comes back as the code the declaration stated.
const FUNCTION_CODE: &str =
    "function handler(event) { event.request.headers['x-estate'] = { value: '1' }; \
     return event.request; }";

/// One row of the table.
struct Row {
    case: &'static str,
    /// The template body, given the name the row's resources are declared under.
    template: fn(&str) -> String,
    /// The name the row declares its resource under.
    name: &'static str,
    /// The output keys the template declares. EVERY one must carry a value: a stack reported
    /// CREATE_COMPLETE has resolved each output it publishes.
    outputs: &'static [&'static str],
    /// The output whose value is the resource's identity in CloudFront, and the element the
    /// read below reports that identity under (`None` where the type publishes no identity of
    /// its own — a monitoring subscription is the distribution's).
    identity_output: &'static str,
    identity_element: Option<&'static str>,
    /// The resource's own read, as the path built from the stack's outputs and the row's name.
    read_path: fn(&BTreeMap<String, String>, &str) -> String,
    /// `(element, value)` pairs the read's answer must report: the declaration reaching
    /// CloudFront.
    reported: &'static [(&'static str, &'static str)],
}

/// The key-value-store row, in JSON.
fn key_value_store_template(name: &str) -> String {
    serde_json::json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::CloudFront::KeyValueStore",
                "Properties": {
                    "Name": name,
                    "Comment": "the store the stack declares"
                }
            }
        },
        "Outputs": {
            "StoreRef": {"Value": {"Ref": LOGICAL_ID}},
            "StoreArn": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Arn"]}},
            "StoreId": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Id"]}},
            "StoreStatus": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Status"]}}
        }
    })
    .to_string()
}

/// The function row, in YAML: a key value store and the function that reads through it, with
/// the code the stack publishes to the LIVE stage.
fn function_template(name: &str) -> String {
    [
        "Resources:",
        "  Store:",
        "    Type: AWS::CloudFront::KeyValueStore",
        "    Properties:",
        &format!("      Name: {name}-store"),
        &format!("  {LOGICAL_ID}:"),
        "    Type: AWS::CloudFront::Function",
        "    Properties:",
        &format!("      Name: {name}"),
        "      AutoPublish: true",
        &format!("      FunctionCode: \"{FUNCTION_CODE}\""),
        "      FunctionConfig:",
        "        Comment: the function the stack declares",
        "        Runtime: cloudfront-js-2.0",
        "        KeyValueStoreAssociations:",
        "          - KeyValueStoreARN: !GetAtt Store.Arn",
        "Outputs:",
        "  FunctionRef:",
        &format!("    Value: !Ref {LOGICAL_ID}"),
        "  FunctionArn:",
        &format!("    Value: !GetAtt {LOGICAL_ID}.FunctionARN"),
        "  FunctionMetadataArn:",
        &format!("    Value: !GetAtt {LOGICAL_ID}.FunctionMetadata.FunctionARN"),
        "  FunctionStage:",
        &format!("    Value: !GetAtt {LOGICAL_ID}.Stage"),
    ]
    .join("\n")
}

/// The connection-function row, in JSON.
fn connection_function_template(name: &str) -> String {
    serde_json::json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::CloudFront::ConnectionFunction",
                "Properties": {
                    "Name": name,
                    "AutoPublish": true,
                    "ConnectionFunctionCode": FUNCTION_CODE,
                    "ConnectionFunctionConfig": {
                        "Comment": "the connection function the stack declares",
                        "Runtime": "cloudfront-js-2.0"
                    }
                }
            }
        },
        "Outputs": {
            "FunctionRef": {"Value": {"Ref": LOGICAL_ID}},
            "FunctionArn": {
                "Value": {"Fn::GetAtt": [LOGICAL_ID, "ConnectionFunctionArn"]}
            },
            "FunctionId": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Id"]}},
            "FunctionStage": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Stage"]}},
            "FunctionStatus": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Status"]}}
        }
    })
    .to_string()
}

/// The connection-group row, in JSON.
fn connection_group_template(name: &str) -> String {
    serde_json::json!({
        "Resources": {
            LOGICAL_ID: {
                "Type": "AWS::CloudFront::ConnectionGroup",
                "Properties": {
                    "Name": name,
                    "Enabled": true,
                    "Ipv6Enabled": true,
                    "Tags": [{"Key": "Estate", "Value": "storefront"}]
                }
            }
        },
        "Outputs": {
            "GroupRef": {"Value": {"Ref": LOGICAL_ID}},
            "GroupArn": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Arn"]}},
            "GroupId": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Id"]}},
            "GroupEndpoint": {
                "Value": {"Fn::GetAtt": [LOGICAL_ID, "RoutingEndpoint"]}
            },
            "GroupStatus": {"Value": {"Fn::GetAtt": [LOGICAL_ID, "Status"]}}
        }
    })
    .to_string()
}

/// The monitoring-subscription row, in JSON: a distribution and the additional metrics the
/// stack turns on for it, so the subscription's own identity is that distribution's id.
fn monitoring_subscription_template(name: &str) -> String {
    serde_json::json!({
        "Resources": {
            "Distribution": {
                "Type": "AWS::CloudFront::Distribution",
                "Properties": {"DistributionConfig": {
                    "Enabled": true,
                    "Comment": name,
                    "Origins": [{
                        "Id": "the-origin",
                        "DomainName": "origin.example.com",
                        "CustomOriginConfig": {
                            "HTTPPort": 80,
                            "HTTPSPort": 443,
                            "OriginProtocolPolicy": "https-only"
                        }
                    }],
                    "DefaultCacheBehavior": {
                        "TargetOriginId": "the-origin",
                        "ViewerProtocolPolicy": "allow-all"
                    }
                }}
            },
            LOGICAL_ID: {
                "Type": "AWS::CloudFront::MonitoringSubscription",
                "Properties": {
                    "DistributionId": {"Ref": "Distribution"},
                    "MonitoringSubscription": {
                        "RealtimeMetricsSubscriptionConfig": {
                            "RealtimeMetricsSubscriptionStatus": "Enabled"
                        }
                    }
                }
            }
        },
        "Outputs": {
            "SubscriptionRef": {"Value": {"Ref": LOGICAL_ID}},
            "DistributionId": {"Value": {"Ref": "Distribution"}}
        }
    })
    .to_string()
}

fn key_value_store_read(_outputs: &BTreeMap<String, String>, name: &str) -> String {
    format!("/2020-05-31/key-value-store/{name}")
}

fn function_read(_outputs: &BTreeMap<String, String>, name: &str) -> String {
    // The LIVE stage, which is the one `AutoPublish` takes the function to and the one a
    // distribution may associate; a read that names no stage reports the DEVELOPMENT stage the
    // create left behind.
    format!("/2020-05-31/function/{name}/describe?Stage=LIVE")
}

fn connection_function_read(outputs: &BTreeMap<String, String>, _name: &str) -> String {
    // The LIVE stage, for the reason [`function_read`] names it.
    format!(
        "/2020-05-31/connection-function/{}/describe?Stage=LIVE",
        outputs.get("FunctionId").cloned().unwrap_or_default()
    )
}

fn connection_group_read(outputs: &BTreeMap<String, String>, _name: &str) -> String {
    format!(
        "/2020-05-31/connection-group/{}",
        outputs.get("GroupId").cloned().unwrap_or_default()
    )
}

fn monitoring_subscription_read(outputs: &BTreeMap<String, String>, _name: &str) -> String {
    format!(
        "/2020-05-31/distributions/{}/monitoring-subscription",
        outputs.get("DistributionId").cloned().unwrap_or_default()
    )
}

/// The table: one row per resource type.
const ROWS: &[Row] = &[
    Row {
        case: "a key value store, identified by the ARN its data plane takes",
        template: key_value_store_template,
        name: "stack-key-value-store",
        outputs: &["StoreRef", "StoreArn", "StoreId", "StoreStatus"],
        identity_output: "StoreRef",
        identity_element: Some("ARN"),
        read_path: key_value_store_read,
        reported: &[
            ("Name", "stack-key-value-store"),
            ("Comment", "the store the stack declares"),
        ],
    },
    Row {
        case: "a function reading through the stack's own key value store, in YAML",
        template: function_template,
        name: "stack-function",
        outputs: &[
            "FunctionRef",
            "FunctionArn",
            "FunctionMetadataArn",
            "FunctionStage",
        ],
        identity_output: "FunctionRef",
        identity_element: Some("FunctionARN"),
        read_path: function_read,
        reported: &[
            ("Name", "stack-function"),
            ("Comment", "the function the stack declares"),
            ("Runtime", "cloudfront-js-2.0"),
            // `AutoPublish: true` is what takes the function to the LIVE stage, which is the
            // stage a distribution may associate.
            ("Stage", "LIVE"),
        ],
    },
    Row {
        case: "a connection function, identified by the ARN CloudFront built for it",
        template: connection_function_template,
        name: "stack-connection-function",
        outputs: &[
            "FunctionRef",
            "FunctionArn",
            "FunctionId",
            "FunctionStage",
            "FunctionStatus",
        ],
        identity_output: "FunctionRef",
        identity_element: Some("ConnectionFunctionArn"),
        read_path: connection_function_read,
        reported: &[
            ("Name", "stack-connection-function"),
            ("Comment", "the connection function the stack declares"),
            ("Stage", "LIVE"),
        ],
    },
    Row {
        case: "a connection group, identified by the id CloudFront minted",
        template: connection_group_template,
        name: "stack-connection-group",
        outputs: &[
            "GroupRef",
            "GroupArn",
            "GroupId",
            "GroupEndpoint",
            "GroupStatus",
        ],
        identity_output: "GroupRef",
        identity_element: Some("Id"),
        read_path: connection_group_read,
        reported: &[("Name", "stack-connection-group")],
    },
    Row {
        case: "a monitoring subscription, identified by the distribution it is made on",
        template: monitoring_subscription_template,
        name: "stack-monitoring-subscription",
        outputs: &["SubscriptionRef", "DistributionId"],
        identity_output: "SubscriptionRef",
        identity_element: None,
        read_path: monitoring_subscription_read,
        reported: &[("RealtimeMetricsSubscriptionStatus", "Enabled")],
    },
];

/// RULE: the resource a stack declares of one of these types is created in CloudFront itself —
/// readable there under the identity the stack's outputs report, configured as the template
/// declared, and taken away when the stack is deleted.
#[tokio::test]
async fn a_declared_cloudfront_resource_is_created_in_cloudfront_itself() {
    for row in ROWS {
        let case = row.case;
        let router = build_router();
        let stack_name = format!("cloudfront-{}", row.name);
        let (status, created) = cloudformation(
            &router,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &(row.template)(row.name)),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {created}"
        );
        let described = settled_stack(&router, &stack_name, case).await;
        assert_eq!(
            opt_element(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        // Every output the template declares carries a VALUE. `outputs_of` reads the pair out
        // of the entry that carries both, so an entry serialized without its `OutputValue`
        // contributes nothing and is reported here as a missing key.
        let outputs = outputs_of(&described);
        for key in row.outputs {
            assert!(
                outputs.contains_key(*key),
                "case {case:?}: expected the output {key:?} to carry a value, got {outputs:?} \
                 in {described}"
            );
        }
        let identity = outputs
            .get(row.identity_output)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected the output {:?}, got {outputs:?}",
                    row.identity_output
                )
            })
            .clone();

        // CloudFront's own read resolves that identity to the resource the template declared.
        let path = (row.read_path)(&outputs, row.name);
        let (status, read) = cloudfront(&router, &path).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 reading {path}, got {status} with body {read}"
        );
        if let Some(element) = row.identity_element {
            assert_eq!(
                elements(&read, element).first().map(String::as_str),
                Some(identity.as_str()),
                "case {case:?}: expected CloudFront to report the identity {identity:?} the \
                 stack published as its {element}, got {read}"
            );
        }
        for (element, expected) in row.reported {
            assert_eq!(
                elements(&read, element).first().map(String::as_str),
                Some(*expected),
                "case {case:?}: expected {element} {expected:?}, got {read}"
            );
        }

        // Deleting the stack takes the resource away in CloudFront.
        let (status, deleted) =
            cloudformation(&router, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
        );
        deleted_stack(&router, &stack_name, case).await;
        let (status, gone) = cloudfront(&router, &path).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected the deleted stack to leave no resource at {path}, got \
             {status} with body {gone}"
        );
    }
}
