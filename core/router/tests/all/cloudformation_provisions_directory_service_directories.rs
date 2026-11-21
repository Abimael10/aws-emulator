// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::DirectoryService::MicrosoftAD` or an
//! `AWS::DirectoryService::SimpleAD` deploys: the directory is created in Directory Service's
//! own control plane, the stack publishes the `d-…` id that service minted and what it reports
//! for the directory, the stack reaches `CREATE_COMPLETE` only once that service reports the
//! directory `Active`, deleting the stack takes the directory away — and a declaration that
//! service REFUSES fails the resource with its own refusal and rolls the stack back.
//!
//! The rule under test: **a resource type AWS publishes is one a template may declare, and a
//! declaration of it is deployed by calling the service that OWNS the type — so what the stack
//! publishes is what that service answered, and what that service refuses is a FAILED RESOURCE
//! rather than a template-format error.** Three crates have to agree for that to hold
//! (CloudFormation submits the template and makes the call, Directory Service serves it, and
//! Amazon EC2 owns the VPC and the subnets the directory's servers are placed in), so the
//! suite lives here: a crate-local test can only ever see one side of it. The catalogue half
//! this rule rests on — that a body declaring either type is ADMITTED — is pinned beside the
//! CloudFormation crate itself
//! (`services/service-com-amazonaws-cloudformation-cloudformation/tests/all/\
//! directory_service_resource_types.rs`).
//!
//! The rows of the deploy table differ along what the rule generalises over: which of the two
//! types the stack declares (and a stack that declares one of each), the properties the
//! declaration carries (the required three alone, those plus the short name and the edition,
//! those plus the alias and single sign-on flags, a Simple AD's own size and description),
//! the template spelling (JSON with long-form intrinsics, YAML with the short-form tags), and
//! which published return value the outputs read (`Ref`, `Fn::GetAtt Alias`,
//! `Fn::GetAtt DirectoryId`, and the LIST-valued `Fn::GetAtt DnsIpAddresses` read through
//! `Fn::Join`).
//!
//! The refusal rows differ along the CLASS of declaration Directory Service refuses: a
//! placement naming a subnet the account does not hold, and one naming two subnets in the SAME
//! Availability Zone. Each is the owning service's own refusal, reported on the resource that
//! failed — never the `Template format error: Unrecognized resource types` a type
//! CloudFormation does not know is refused with, which no row of either table may ever see.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// The Region every request here is signed for, and so the Region the stacks, the directories
/// and the network they sit on belong to.
const REGION: &str = "us-west-2";

/// The EC2 API version a client sends in an ec2Query body.
const EC2_API_VERSION: &str = "2016-11-15";

/// The `X-Amz-Target` prefix Directory Service names an operation under: its service shape's
/// name.
const DS_TARGET_PREFIX: &str = "DirectoryService_20150416";

/// A password the modelled `Password` pattern admits.
const PASSWORD: &str = "Passw0rd!2345";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which is what the defect this suite pins answered for a type AWS publishes.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// How long a polled read may take to reach the condition it waits for before a case fails:
/// far longer than any span either service publishes, so a loaded or faster host changes
/// nothing about the outcome.
const SETTLE_DEADLINE: Duration = Duration::from_secs(90);

/// How often such a read asks again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// The stage Directory Service reports a directory that is SERVING in.
const STAGE_ACTIVE: &str = "Active";

/// CloudFormation, Directory Service and Amazon EC2 over ONE shared engine, as the production
/// router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_ec2_amazonec2::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_directoryservice_directoryservice_20150416::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is what
/// routes the request to that service; the signature is never checked.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}\
         /aws4_request, SignedHeaders=host, Signature=0000"
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

/// One ec2Query action that must succeed, answered with its XML body.
async fn ec2_ok(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={EC2_API_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ec2.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("ec2"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with {text}"
    );
    text
}

/// One Directory Service control-plane call: its status and its parsed body.
async fn ds(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{DS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("ds"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One subnet of the account, as EC2 reported it when it minted it.
#[derive(Debug, Clone)]
struct Subnet {
    id: String,
    zone: String,
}

/// The network a row's directories are placed in: the VPC and the subnets EC2 minted in it.
#[derive(Debug, Clone)]
struct Network {
    vpc_id: String,
    subnets: Vec<Subnet>,
}

/// Build one VPC of the account carrying one subnet per `(block, zone)` entry, through EC2's
/// own operations — so every identifier below is one EC2 handed out.
async fn network(
    router: &MultiServiceRouter,
    vpc_block: &str,
    specs: &[(&str, &str)],
) -> Network {
    let created = ec2_ok(router, "CreateVpc", &[("CidrBlock", vpc_block)]).await;
    let vpc_id = element(&created, "vpcId");
    let mut subnets = Vec::new();
    for (block, zone) in specs {
        let answer = ec2_ok(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc_id.as_str()),
                ("CidrBlock", block),
                ("AvailabilityZone", zone),
            ],
        )
        .await;
        subnets.push(Subnet {
            id: element(&answer, "subnetId"),
            zone: element(&answer, "availabilityZone"),
        });
    }
    Network { vpc_id, subnets }
}

/// Whether `id` has the form Directory Service's own `DirectoryId` takes
/// (`^d-[0-9a-f]{10}$`) — which is what distinguishes an id that service MINTED from a name
/// the template or CloudFormation chose.
fn is_directory_id(id: &str) -> bool {
    let Some(digits) = id.strip_prefix("d-") else {
        return false;
    };
    digits.len() == 10 && digits.chars().all(|digit| digit.is_ascii_hexdigit() && !digit.is_ascii_uppercase())
}

/// The `DescribeStacks` body once the stack rests in a status that is not in progress, or the
/// last one read when the deadline passes. Waits for the CONDITION, looking again until the
/// stack has settled — every read of it is also what drives the waits the deployment opened.
async fn settled_stack(router: &MultiServiceRouter, case: &str, stack_name: &str) -> String {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with body {body}"
        );
        let settled = opt_element(&body, "StackStatus")
            .is_some_and(|stack_status| !stack_status.ends_with("_IN_PROGRESS"));
        if settled || Instant::now() >= deadline {
            return body;
        }
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Submit one body and answer the `DescribeStacks` body the stack settled in.
async fn deploy(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack_name), ("TemplateBody", template)],
    )
    .await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    settled_stack(router, case, stack_name).await
}

/// The description Directory Service reports for `directory_id`, or `None` where it reports
/// none — which is what a finished teardown looks like.
async fn described(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Option<Value> {
    let (status, body) = ds(
        router,
        "DescribeDirectories",
        json!({ "DirectoryIds": [directory_id] }),
    )
    .await;
    if status != 200 {
        return None;
    }
    let _ = case;
    body["DirectoryDescriptions"]
        .as_array()
        .and_then(|descriptions| descriptions.first().cloned())
}

/// The description Directory Service reports for a directory the stack says it created, which
/// it must hold.
async fn held(router: &MultiServiceRouter, directory_id: &str, case: &str) -> Value {
    described(router, directory_id, case)
        .await
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected Directory Service to hold the directory \
                 {directory_id:?} the stack published, it holds none"
            )
        })
}

/// The `DnsIpAddrs` a description reports, in the order it reports them.
fn dns_ip_addrs(description: &Value) -> Vec<String> {
    description["DnsIpAddrs"]
        .as_array()
        .map(|addresses| {
            addresses
                .iter()
                .filter_map(|address| address.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// What one row expects of one directory the stack declares.
struct Declared {
    /// The logical id the declaration gives it, which `DescribeStackResources` reports the
    /// minted id under.
    logical_id: &'static str,
    /// The output key whose value is the `Ref` of the resource.
    ref_output: &'static str,
    /// The output key whose value is its `Alias`, where the row reads one.
    alias_output: Option<&'static str>,
    /// The output key whose value is its `DnsIpAddresses`, joined — the LIST-valued return
    /// value, read the way `Fn::GetAtt`'s own documentation says a list is read.
    dns_output: Option<&'static str>,
    /// The fully qualified name the declaration states, which the described directory must
    /// report.
    name: &'static str,
    /// Whether the declaration asks CloudFormation to assign the directory an alias of its
    /// own, and whether it asks for single sign-on.
    create_alias: bool,
    enable_sso: bool,
}

/// One row: the template, the network it is placed in and the directories it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    /// `(the VPC block, one (block, zone) per subnet)`.
    vpc_block: &'static str,
    subnets: &'static [(&'static str, &'static str)],
    /// The template, built once the network's identifiers are known.
    template: fn(&Network) -> String,
    declared: Vec<Declared>,
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: one Managed Microsoft AD with the required properties alone,
        // with every published return value read back, in JSON with long-form intrinsics.
        Row {
            case: "a Managed Microsoft AD with the required properties, every return value read",
            stack_name: "ds-managed-round-trip",
            vpc_block: "10.10.0.0/16",
            subnets: &[("10.10.1.0/24", "us-west-2a"), ("10.10.2.0/24", "us-west-2b")],
            template: |net| {
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Resources": {
                        "Directory": {
                            "Type": "AWS::DirectoryService::MicrosoftAD",
                            "Properties": {
                                "Name": "corp.example.com",
                                "Password": PASSWORD,
                                "VpcSettings": {
                                    "VpcId": net.vpc_id,
                                    "SubnetIds": [net.subnets[0].id, net.subnets[1].id],
                                },
                            },
                        }
                    },
                    "Outputs": {
                        "DirectoryRef": {"Value": {"Ref": "Directory"}},
                        "Alias": {"Value": {"Fn::GetAtt": ["Directory", "Alias"]}},
                        "DnsAddresses": {
                            "Value": {
                                "Fn::Join": [",", {"Fn::GetAtt": ["Directory", "DnsIpAddresses"]}]
                            }
                        }
                    }
                })
                .to_string()
            },
            declared: vec![Declared {
                logical_id: "Directory",
                ref_output: "DirectoryRef",
                alias_output: Some("Alias"),
                dns_output: Some("DnsAddresses"),
                name: "corp.example.com",
                create_alias: false,
                enable_sso: false,
            }],
        },
        // The same type carrying the optional properties it publishes, in YAML with
        // short-form tags: the NetBIOS short name, the `Standard` edition, and the two flags
        // CloudFormation applies with calls of that service's own once the directory serves.
        Row {
            case: "a Managed Microsoft AD with a short name, an edition and both flags, YAML",
            stack_name: "ds-managed-aliased",
            vpc_block: "10.20.0.0/16",
            subnets: &[("10.20.4.0/22", "us-west-2b"), ("10.20.8.0/28", "us-west-2c")],
            template: |net| {
                format!(
                    "\
Resources:
  Directory:
    Type: AWS::DirectoryService::MicrosoftAD
    Properties:
      Name: example.corp.example.com
      ShortName: EXAMPLE
      Password: \"{PASSWORD}\"
      Edition: Standard
      CreateAlias: true
      EnableSso: true
      VpcSettings:
        VpcId: {vpc}
        SubnetIds:
          - {first}
          - {second}
Outputs:
  DirectoryRef:
    Value: !Ref Directory
  Alias:
    Value: !GetAtt Directory.Alias
",
                    vpc = net.vpc_id,
                    first = net.subnets[0].id,
                    second = net.subnets[1].id,
                )
            },
            declared: vec![Declared {
                logical_id: "Directory",
                ref_output: "DirectoryRef",
                alias_output: Some("Alias"),
                dns_output: None,
                name: "example.corp.example.com",
                create_alias: true,
                enable_sso: true,
            }],
        },
        // The OTHER type of the namespace, sized by the `Size` a Simple AD publishes where a
        // Managed Microsoft AD publishes an `Edition`, with the `DirectoryId` only that type
        // publishes read back beside its alias.
        Row {
            case: "a Simple AD with its size and description, DirectoryId read back",
            stack_name: "ds-simple-round-trip",
            vpc_block: "10.30.0.0/16",
            subnets: &[("10.30.0.0/25", "us-west-2a"), ("10.30.1.0/26", "us-west-2c")],
            template: |net| {
                json!({
                    "Resources": {
                        "Directory": {
                            "Type": "AWS::DirectoryService::SimpleAD",
                            "Properties": {
                                "Name": "simple.example.com",
                                "Password": PASSWORD,
                                "Size": "Small",
                                "Description": "a simple directory",
                                "VpcSettings": {
                                    "VpcId": net.vpc_id,
                                    "SubnetIds": [net.subnets[0].id, net.subnets[1].id],
                                },
                            },
                        }
                    },
                    "Outputs": {
                        "DirectoryRef": {"Value": {"Ref": "Directory"}},
                        "DirectoryId": {"Value": {"Fn::GetAtt": ["Directory", "DirectoryId"]}},
                        "DnsAddresses": {
                            "Value": {
                                "Fn::Join": [",", {"Fn::GetAtt": ["Directory", "DnsIpAddresses"]}]
                            }
                        }
                    }
                })
                .to_string()
            },
            declared: vec![Declared {
                logical_id: "Directory",
                ref_output: "DirectoryRef",
                alias_output: None,
                dns_output: Some("DnsAddresses"),
                name: "simple.example.com",
                create_alias: false,
                enable_sso: false,
            }],
        },
        // One directory of EACH type in one stack, placed in the same two subnets: each gets
        // its own minted id, and both are read back.
        Row {
            case: "one directory of each type in one stack",
            stack_name: "ds-directory-pair",
            vpc_block: "10.40.0.0/16",
            subnets: &[("10.40.16.0/20", "us-west-2a"), ("10.40.32.0/24", "us-west-2c")],
            template: |net| {
                json!({
                    "Resources": {
                        "Managed": {
                            "Type": "AWS::DirectoryService::MicrosoftAD",
                            "Properties": {
                                "Name": "first.example.com",
                                "Password": PASSWORD,
                                "Edition": "Enterprise",
                                "VpcSettings": {
                                    "VpcId": net.vpc_id,
                                    "SubnetIds": [net.subnets[0].id, net.subnets[1].id],
                                },
                            },
                        },
                        "Simple": {
                            "Type": "AWS::DirectoryService::SimpleAD",
                            "Properties": {
                                "Name": "second.example.com",
                                "Password": PASSWORD,
                                "Size": "Large",
                                "VpcSettings": {
                                    "VpcId": net.vpc_id,
                                    "SubnetIds": [net.subnets[1].id, net.subnets[0].id],
                                },
                            },
                        }
                    },
                    "Outputs": {
                        "ManagedRef": {"Value": {"Ref": "Managed"}},
                        "ManagedAlias": {"Value": {"Fn::GetAtt": ["Managed", "Alias"]}},
                        "SimpleRef": {"Value": {"Ref": "Simple"}},
                        "SimpleAlias": {"Value": {"Fn::GetAtt": ["Simple", "Alias"]}}
                    }
                })
                .to_string()
            },
            declared: vec![
                Declared {
                    logical_id: "Managed",
                    ref_output: "ManagedRef",
                    alias_output: Some("ManagedAlias"),
                    dns_output: None,
                    name: "first.example.com",
                    create_alias: false,
                    enable_sso: false,
                },
                Declared {
                    logical_id: "Simple",
                    ref_output: "SimpleRef",
                    alias_output: Some("SimpleAlias"),
                    dns_output: None,
                    name: "second.example.com",
                    create_alias: false,
                    enable_sso: false,
                },
            ],
        },
    ]
}

#[tokio::test]
async fn a_deployed_stack_publishes_the_directory_service_minted() {
    for row in rows() {
        let router = emulator();
        let case = row.case;
        let net = network(&router, row.vpc_block, row.subnets).await;
        let template = (row.template)(&net);
        let body = deploy(&router, case, row.stack_name, &template).await;
        assert_eq!(
            opt_element(&body, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {body}"
        );
        let outputs: BTreeMap<String, String> = outputs_of(&body);

        let (status, resources) = cloudformation(
            &router,
            "DescribeStackResources",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             {resources}"
        );
        let physical = physical_ids_of(&resources);

        for declared in &row.declared {
            let logical_id = declared.logical_id;
            let directory_id = physical.get(logical_id).unwrap_or_else(|| {
                panic!("case {case:?}: no physical id for {logical_id} in {resources}")
            });
            assert!(
                is_directory_id(directory_id),
                "case {case:?}: expected the resource's physical id to be a directory id of \
                 the modelled d-<10 hex> form, got {directory_id:?}"
            );
            assert_eq!(
                outputs.get(declared.ref_output).map(String::as_str),
                Some(directory_id.as_str()),
                "case {case:?}: expected Ref to publish the minted directory id, got {outputs:?}"
            );

            // The identifier the stack published is one the OWNING service resolves, and the
            // directory it resolves to is the one the template declared — serving, in the
            // placement the declaration named.
            let description = held(&router, directory_id, case).await;
            assert_eq!(
                description["Stage"].as_str(),
                Some(STAGE_ACTIVE),
                "case {case:?}: expected the directory the stack completed on to be serving, \
                 got {description}"
            );
            assert_eq!(
                description["Name"].as_str(),
                Some(declared.name),
                "case {case:?}: expected the declared directory name, got {description}"
            );
            assert_eq!(
                description["VpcSettings"]["VpcId"].as_str(),
                Some(net.vpc_id.as_str()),
                "case {case:?}: expected the directory to sit in the declared VPC, got \
                 {description}"
            );
            let placed: Vec<&str> = description["VpcSettings"]["SubnetIds"]
                .as_array()
                .map(|ids| ids.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            for subnet in &net.subnets {
                assert!(
                    placed.contains(&subnet.id.as_str()),
                    "case {case:?}: expected the directory to be placed in {} (zone {}), got \
                     {placed:?}",
                    subnet.id,
                    subnet.zone
                );
            }

            // An alias the declaration did not ask for is the directory id itself, which is
            // what that service reports for a directory no alias has been assigned to; one it
            // DID ask for is an alias of its own, assigned by a call of that service.
            let reported_alias = description["Alias"].as_str().unwrap_or_default();
            if declared.create_alias {
                assert_ne!(
                    reported_alias, directory_id,
                    "case {case:?}: expected a declaration asking for an alias to get one of \
                     its own, got {description}"
                );
                assert!(
                    !reported_alias.is_empty(),
                    "case {case:?}: expected an assigned alias, got {description}"
                );
            } else {
                assert_eq!(
                    reported_alias, directory_id,
                    "case {case:?}: expected the alias of a directory with none assigned to be \
                     the directory id, got {description}"
                );
            }
            if let Some(key) = declared.alias_output {
                assert_eq!(
                    outputs.get(key).map(String::as_str),
                    Some(reported_alias),
                    "case {case:?}: expected the published alias to be the one Directory \
                     Service reports, got {outputs:?}"
                );
            }
            assert_eq!(
                description["SsoEnabled"].as_bool().unwrap_or(false),
                declared.enable_sso,
                "case {case:?}: expected single sign-on to be {}, got {description}",
                declared.enable_sso
            );

            // The LIST-valued return value: one address per subnet the servers were placed in,
            // each the address that service reports its own DNS server answers on.
            let addresses = dns_ip_addrs(&description);
            assert_eq!(
                addresses.len(),
                net.subnets.len(),
                "case {case:?}: expected one DNS address per placed subnet, got {addresses:?}"
            );
            if let Some(key) = declared.dns_output {
                assert_eq!(
                    outputs.get(key).map(String::as_str),
                    Some(addresses.join(",").as_str()),
                    "case {case:?}: expected the published DnsIpAddresses to be the addresses \
                     Directory Service reports, got {outputs:?}"
                );
            }

            // A Simple AD publishes its own `DirectoryId` beside the alias.
            if let Some(published) = outputs.get("DirectoryId") {
                assert_eq!(
                    published, directory_id,
                    "case {case:?}: expected the published DirectoryId to be the minted id, \
                     got {outputs:?}"
                );
            }
        }

        // Deleting the stack takes every directory it created away: the delete is held open
        // until the owning service no longer holds it.
        let (status, deleted) = cloudformation(
            &router,
            "DeleteStack",
            &[("StackName", row.stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with {deleted}"
        );
        let deadline = Instant::now() + SETTLE_DEADLINE;
        loop {
            // Reading the stack is what drives the teardown the delete opened.
            let _ = cloudformation(
                &router,
                "DescribeStackEvents",
                &[("StackName", row.stack_name)],
            )
            .await;
            let mut gone = true;
            for declared in &row.declared {
                let directory_id = physical.get(declared.logical_id).expect("a physical id");
                if described(&router, directory_id, case).await.is_some() {
                    gone = false;
                }
            }
            if gone {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected Directory Service to hold no directory of the deleted \
                 stack"
            );
            tokio::time::sleep(LOOK_AGAIN_AFTER).await;
        }
    }
}

/// A declaration the OWNING service refuses fails its resource with that service's own
/// refusal and rolls the stack back — never the template-format error a type CloudFormation
/// does not know is refused with.
///
/// The rows differ along the class of placement Directory Service refuses: subnets the account
/// does not hold, and two subnets in ONE Availability Zone.
#[tokio::test]
async fn a_declaration_the_owning_service_refuses_fails_its_resource() {
    let router = emulator();
    let net = network(
        &router,
        "10.50.0.0/16",
        &[("10.50.1.0/24", "us-west-2a"), ("10.50.2.0/24", "us-west-2a")],
    )
    .await;
    // (case, the stack, the subnets the declaration names)
    let refused: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "subnets the account does not hold",
            "ds-unheld-subnets",
            vec![
                "subnet-0123456789abcdef0".to_string(),
                "subnet-0123456789abcdef1".to_string(),
            ],
        ),
        (
            "two subnets in one Availability Zone",
            "ds-one-zone",
            vec![net.subnets[0].id.clone(), net.subnets[1].id.clone()],
        ),
    ];
    for (case, stack_name, subnet_ids) in refused {
        let template = json!({
            "Resources": {
                "Directory": {
                    "Type": "AWS::DirectoryService::MicrosoftAD",
                    "Properties": {
                        "Name": "refused.example.com",
                        "Password": PASSWORD,
                        "VpcSettings": { "VpcId": net.vpc_id, "SubnetIds": subnet_ids },
                    },
                }
            }
        })
        .to_string();
        let body = deploy(&router, case, stack_name, &template).await;
        let stack_status = opt_element(&body, "StackStatus").unwrap_or_default();
        assert!(
            stack_status.ends_with("ROLLBACK_COMPLETE"),
            "case {case:?}: expected the stack to roll back on a refused resource, got \
             {stack_status:?} in {body}"
        );
        let (status, events) =
            cloudformation(&router, "DescribeStackEvents", &[("StackName", stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackEvents, got {status} with \
             {events}"
        );
        assert!(
            !events.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected the owning service's own refusal, got the \
             template-format error in {events}"
        );
        let reasons = elements(&events, "ResourceStatusReason").join(" | ");
        assert!(
            !reasons.is_empty(),
            "case {case:?}: expected the failed resource to carry the refusal Directory \
             Service worded, got {events}"
        );
    }
}
