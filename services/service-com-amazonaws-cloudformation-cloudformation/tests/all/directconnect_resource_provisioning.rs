// SPDX-License-Identifier: CC0-1.0

//! The AWS Direct Connect estate a stack declares, end to end: the gateway and the resiliency
//! group exist in that service itself, under the identities it minted, a changed declaration
//! reaches them, deleting the stack takes them away — and a declaration carrying a resiliency
//! model outside the modelled enum fails the RESOURCE and rolls the stack back.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of it
//! is DEPLOYED by calling the service that owns the type — so the handle the stack publishes
//! (`Ref`) resolves in that service, the values the type publishes are the ones that service
//! answered, and a declaration that stage refuses is a FAILED RESOURCE rather than a
//! template-format error. The `Template format error: Unrecognized resource types` refusal a
//! type CloudFormation does not know is answered with may appear in no row of any table here.
//!
//! The deploy table's rows differ along what the rule generalises over: which of the two types
//! is declared, whether the stack declares one resource or both, whether the gateway's
//! Amazon-side ASN is omitted (so the documented default stands), stated as a NUMBER or stated
//! as the TEXT a resolved parameter reference yields, where that ASN sits inside the ranges
//! `CreateDirectConnectGatewayRequest$amazonSideAsn` publishes (each range's own bounds),
//! which of the three `ResiliencyModel` values the group is created to meet — including the
//! ones the report that led here did not cite — and whether the declaration carries the type's
//! own `Tags`.
//!
//! The refusal table's rows differ along the FORM of a resiliency model outside that enum: a
//! value naming no model, one differing from a member only in case, one spelled with a space,
//! and — as the control — a member of the set, which deploys.
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs`
//! builds: the CloudFormation calls over its awsQuery wire and the Direct Connect calls over
//! that service's own `awsJson1_1` wire, so what is asserted is what a client that deploys the
//! template and then calls Direct Connect observes. Nothing waits out a span: every stack is
//! polled to its terminal status and every resource to the state its owning service reports it
//! ready in.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    call, create_stack, member, router, settled_stack, settled_stack_status, stack_event_reasons,
    xml_blocks, xml_text, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::output_value;
use service_com_amazonaws_cloudformation_cloudformation::state::property_validation::enum_properties::{
    enum_failure_reason, RESILIENCY_MODEL_VALUES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::provisioning::{
    LOGICAL_ID_TAG, STACK_NAME_TAG,
};
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The two resource types under test.
const GATEWAY_TYPE: &str = "AWS::DirectConnect::DirectConnectGateway";
const GROUP_TYPE: &str = "AWS::DirectConnect::ResiliencyGroup";

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of any table here may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The validation CloudFormation names for the check that holds a declaration's properties to
/// its type's schema, and the `EventType` its result is reported under. Source:
/// `state::property_validation::PROPERTY_VALIDATION` and the model's own `EventType`.
const PROPERTY_VALIDATION: &str = "PROPERTY_VALIDATION";
const VALIDATION_ERROR: &str = "VALIDATION_ERROR";

/// `OperationEvent$ValidationStatus` for a validation the declaration did not pass, and
/// `OperationEvent$ValidationFailureMode` for one whose failure was handled by FAILING the
/// operation.
const FAILED: &str = "FAILED";
const FAIL: &str = "FAIL";

/// `StackEvent$DetailedStatus` for an operation a pre-deployment validation failed.
const VALIDATION_FAILED: &str = "VALIDATION_FAILED";

/// The state either Direct Connect resource is ready for use in ("available: The Direct
/// Connect gateway is ready for use"), and the TERMINAL state a torn-down one is reported in.
const AVAILABLE: &str = "available";
const DELETED: &str = "deleted";

/// The one `ResiliencyGroupType` the model declares: "The valid value is `Managed`."
const MANAGED: &str = "Managed";

/// The `amazonSideAsn` a gateway declared without one gets:
/// `CreateDirectConnectGatewayRequest$amazonSideAsn` documents "The default is 64512."
const DEFAULT_AMAZON_SIDE_ASN: i64 = 64_512;

/// The bounds of the ranges that same member publishes: "The ASN must be in the private range
/// of 64,512 to 65,534 or 4,200,000,000 to 4,294,967,294."
const PRIVATE_ASN_BOUNDS: [i64; 4] = [64_512, 65_534, 4_200_000_000, 4_294_967_294];

/// The account every ARN this emulator mints carries.
const ACCOUNT_ID: &str = "000000000000";

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates to
/// every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// One AWS Direct Connect call over the `awsJson1_1` wire its model declares, under the
/// `X-Amz-Target` its service shape and the operation build, signed for `directconnect` — that
/// model's own `sigv4Name`.
async fn direct_connect(
    router: &MultiServiceRouter,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let host = format!("directconnect.{REGION}.amazonaws.com");
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/directconnect/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0123456789abcdef"
    );
    let built = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("OvertureService.{operation}"))
        .header("authorization", authorization)
        .body(bytes::Bytes::from(request.to_string()))
        .expect("a well-formed Direct Connect request");
    let response = router.dispatch(built).await;
    let code = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (code, parsed)
}

/// That service's own record of ONE Direct Connect gateway, by the id it minted — the listing
/// `DescribeDirectConnectGateways` narrows by `directConnectGatewayId`, searched for the id
/// asked about.
async fn gateway(router: &MultiServiceRouter, gateway_id: &str) -> Option<Value> {
    let (code, answered) = direct_connect(
        router,
        "DescribeDirectConnectGateways",
        &json!({"directConnectGatewayId": gateway_id}),
    )
    .await;
    assert_eq!(
        code, 200,
        "expected HTTP 200 from DescribeDirectConnectGateways({gateway_id}), got {code} with \
         {answered}"
    );
    answered
        .get("directConnectGateways")
        .and_then(Value::as_array)
        .and_then(|listed| {
            listed
                .iter()
                .find(|held| held.get("directConnectGatewayId").and_then(Value::as_str)
                    == Some(gateway_id))
                .cloned()
        })
}

/// That service's own read of ONE resiliency group, by the id it minted; `None` where it holds
/// no group under that id, which is the one client exception this service answers.
async fn resiliency_group(router: &MultiServiceRouter, group_id: &str) -> Option<Value> {
    let (code, answered) = direct_connect(
        router,
        "GetResiliencyGroup",
        &json!({"resiliencyGroupId": group_id}),
    )
    .await;
    if code != 200 {
        return None;
    }
    answered.get("resiliencyGroup").cloned()
}

/// Every resiliency group that service lists for the account, as `ListResiliencyGroups` reports
/// them.
async fn listed_groups(router: &MultiServiceRouter) -> Vec<Value> {
    let (code, answered) = direct_connect(router, "ListResiliencyGroups", &json!({})).await;
    assert_eq!(
        code, 200,
        "expected HTTP 200 from ListResiliencyGroups, got {code} with {answered}"
    );
    answered
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The tags that service holds on one resource, by its ARN — `DescribeTags`' own answer, read
/// as the map a client compares.
async fn described_tags(router: &MultiServiceRouter, resource_arn: &str) -> BTreeMap<String, String> {
    let (code, answered) = direct_connect(
        router,
        "DescribeTags",
        &json!({"resourceArns": [resource_arn]}),
    )
    .await;
    assert_eq!(
        code, 200,
        "expected HTTP 200 from DescribeTags({resource_arn}), got {code} with {answered}"
    );
    answered
        .get("resourceTags")
        .and_then(Value::as_array)
        .and_then(|held| {
            held.iter()
                .find(|entry| entry.get("resourceArn").and_then(Value::as_str) == Some(resource_arn))
                .cloned()
        })
        .and_then(|entry| entry.get("tags").and_then(Value::as_array).cloned())
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    Some((
                        tag.get("key").and_then(Value::as_str)?.to_string(),
                        tag.get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ARN a Direct Connect resource of `path` and `identifier` is published under: this
/// service's namespace, NO Region — which is what `ResiliencyGroupArn`'s own pattern states —
/// the account and the `<path>/<identifier>` resource path.
fn direct_connect_arn(path: &str, identifier: &str) -> String {
    format!("arn:aws:directconnect::{ACCOUNT_ID}:{path}/{identifier}")
}

/// Read one gateway until that service reports it `available`, looking again until the
/// harness's own generous deadline passes: the state is one the CLOCK makes due, so what is
/// waited for is the CONDITION rather than a span of wall clock.
async fn available_gateway(router: &MultiServiceRouter, case: &str, gateway_id: &str) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = gateway(router, gateway_id).await;
        let reported = held
            .as_ref()
            .and_then(|held| held.get("directConnectGatewayState"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if reported == AVAILABLE {
            return held.expect("a reported gateway carries a state");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected AWS Direct Connect to report the gateway {gateway_id} \
             {AVAILABLE}, last read {held:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// The same wait for one resiliency group, read through that service's own `GetResiliencyGroup`.
async fn available_group(router: &MultiServiceRouter, case: &str, group_id: &str) -> Value {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let held = resiliency_group(router, group_id).await;
        let reported = held
            .as_ref()
            .and_then(|held| held.get("state"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if reported == AVAILABLE {
            return held.expect("a reported group carries a state");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected AWS Direct Connect to report the group {group_id} \
             {AVAILABLE}, last read {held:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// Deploy `template` as `stack_name` under the stack's own tag, assert the type was recognised
/// and the create completed, and answer the settled `DescribeStacks` body.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (code, created) = create_stack(
        router,
        stack_name,
        template,
        &[
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert!(
        !created.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected a type Amazon Web Services publishes to be recognised, got \
         {created}"
    );
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {code} with {created}"
    );
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached,
        status::CREATE_COMPLETE,
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    settled_stack(case, router, REGION, stack_name).await
}

/// What one row expects of the gateway its template declares.
struct DeclaredGateway {
    /// The `DirectConnectGatewayName` the declaration states.
    name: &'static str,
    /// The `AmazonSideAsn` the declaration states, exactly as it writes it — a JSON number, the
    /// text a resolved parameter reference yields, or nothing at all.
    declared_asn: Option<Value>,
    /// The ASN that service must then report, which is the declared one or the documented
    /// default.
    reported_asn: i64,
    /// The type's own `Tags` the declaration states.
    tags: &'static [(&'static str, &'static str)],
}

/// What one row expects of the resiliency group its template declares.
struct DeclaredGroup {
    name: &'static str,
    /// The `IntendedResiliencyModel` the declaration states, which must be one of
    /// `RESILIENCY_MODEL_VALUES`.
    model: &'static str,
    tags: &'static [(&'static str, &'static str)],
}

/// One row of the deploy table.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    gateway: Option<DeclaredGateway>,
    group: Option<DeclaredGroup>,
}

/// The template one row deploys: the resources it declares, and one output per published
/// return value — the `Ref` of each resource and the identifier its type publishes.
fn template_of(row: &Row) -> String {
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    if let Some(declared) = &row.gateway {
        let mut properties = json!({"DirectConnectGatewayName": declared.name});
        if let Some(asn) = &declared.declared_asn {
            properties["AmazonSideAsn"] = asn.clone();
        }
        if !declared.tags.is_empty() {
            properties["Tags"] = tag_list(declared.tags);
        }
        resources.insert(
            "Dgw".to_string(),
            json!({"Type": GATEWAY_TYPE, "Properties": properties}),
        );
        outputs.insert("DgwRef".to_string(), json!({"Value": {"Ref": "Dgw"}}));
        outputs.insert(
            "DgwId".to_string(),
            json!({"Value": {"Fn::GetAtt": ["Dgw", "DirectConnectGatewayId"]}}),
        );
    }
    if let Some(declared) = &row.group {
        let mut properties = json!({
            "ResiliencyGroupName": declared.name,
            "IntendedResiliencyModel": declared.model,
        });
        if !declared.tags.is_empty() {
            properties["Tags"] = tag_list(declared.tags);
        }
        resources.insert(
            "Rg".to_string(),
            json!({"Type": GROUP_TYPE, "Properties": properties}),
        );
        outputs.insert("RgRef".to_string(), json!({"Value": {"Ref": "Rg"}}));
        outputs.insert(
            "RgId".to_string(),
            json!({"Value": {"Fn::GetAtt": ["Rg", "ResiliencyGroupId"]}}),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
        "Outputs": Value::Object(outputs),
    })
    .to_string()
}

/// A declaration's own `Tags`, in the `[{Key, Value}]` spelling the types publish.
fn tag_list(tags: &[(&str, &str)]) -> Value {
    Value::Array(
        tags.iter()
            .map(|(key, value)| json!({"Key": key, "Value": value}))
            .collect(),
    )
}

fn rows() -> Vec<Row> {
    vec![
        // The issue's own stack: a gateway and a group in ONE template, every published return
        // value read back, the gateway's ASN at the low bound of the first documented range.
        Row {
            case: "a gateway and a resiliency group in one stack",
            stack_name: "dx-estate-round-trip",
            gateway: Some(DeclaredGateway {
                name: "dx-estate-gateway",
                declared_asn: Some(json!(PRIVATE_ASN_BOUNDS[0])),
                reported_asn: PRIVATE_ASN_BOUNDS[0],
                tags: &[("sim2real-dx", "round-trip")],
            }),
            group: Some(DeclaredGroup {
                name: "dx-estate-group",
                model: "basic-resiliency",
                tags: &[("sim2real-dx", "round-trip")],
            }),
        },
        // A gateway alone, declaring no ASN at all: the documented default stands.
        Row {
            case: "a gateway that declares no Amazon-side ASN",
            stack_name: "dx-gateway-default-asn",
            gateway: Some(DeclaredGateway {
                name: "dx-default-asn-gateway",
                declared_asn: None,
                reported_asn: DEFAULT_AMAZON_SIDE_ASN,
                tags: &[],
            }),
            group: None,
        },
        // A gateway whose ASN is the TEXT a resolved parameter reference yields, at the high
        // bound of the first documented range.
        Row {
            case: "a gateway whose Amazon-side ASN is stated as text at the first range's top",
            stack_name: "dx-gateway-text-asn",
            gateway: Some(DeclaredGateway {
                name: "dx-text-asn-gateway",
                declared_asn: Some(json!(PRIVATE_ASN_BOUNDS[1].to_string())),
                reported_asn: PRIVATE_ASN_BOUNDS[1],
                tags: &[("asn", "text")],
            }),
            group: None,
        },
        // A gateway at each bound of the SECOND documented range, which is four-byte.
        Row {
            case: "a gateway at the low bound of the four-byte range",
            stack_name: "dx-gateway-four-byte-low",
            gateway: Some(DeclaredGateway {
                name: "dx-four-byte-low-gateway",
                declared_asn: Some(json!(PRIVATE_ASN_BOUNDS[2])),
                reported_asn: PRIVATE_ASN_BOUNDS[2],
                tags: &[],
            }),
            group: None,
        },
        Row {
            case: "a gateway at the high bound of the four-byte range",
            stack_name: "dx-gateway-four-byte-high",
            gateway: Some(DeclaredGateway {
                name: "dx-four-byte-high-gateway",
                declared_asn: Some(json!(PRIVATE_ASN_BOUNDS[3])),
                reported_asn: PRIVATE_ASN_BOUNDS[3],
                tags: &[],
            }),
            group: None,
        },
        // A group for the OTHER two resiliency models the enum publishes — neither of them the
        // value the report that led here cited — each in a stack of its own.
        Row {
            case: "a resiliency group meeting the maximum-resiliency model",
            stack_name: "dx-group-maximum",
            gateway: None,
            group: Some(DeclaredGroup {
                name: "dx_maximum_group",
                model: RESILIENCY_MODEL_VALUES[0],
                tags: &[("model", "maximum")],
            }),
        },
        Row {
            case: "a resiliency group meeting the high-resiliency model",
            stack_name: "dx-group-high",
            gateway: None,
            group: Some(DeclaredGroup {
                name: "dx-high-group",
                model: RESILIENCY_MODEL_VALUES[1],
                tags: &[],
            }),
        },
    ]
}

/// Every row: the declaration is ADMITTED, deployed into AWS Direct Connect's own control
/// plane, published under the ARN that service's identifier sits inside — and deleting the
/// stack leaves each resource describable in the terminal `deleted` state that service reports
/// a torn-down one in.
#[tokio::test]
async fn a_declared_direct_connect_estate_is_deployed_read_back_and_taken_away() {
    for row in rows() {
        let case = row.case;
        // A router of its own per row: what each case is about is its own declaration, not
        // what another row left behind.
        let router = router();
        let described = deployed(&router, case, row.stack_name, &template_of(&row)).await;

        let mut gateway_id = String::new();
        if let Some(declared) = &row.gateway {
            gateway_id = output_value(&described, "DgwId").unwrap_or_default();
            let published = output_value(&described, "DgwRef").unwrap_or_default();
            assert_eq!(
                published,
                direct_connect_arn("dx-gateway", &gateway_id),
                "case {case:?}: expected Ref to publish the gateway's ARN around the id that \
                 service minted, got {published:?} for {gateway_id:?}"
            );

            let held = available_gateway(&router, case, &gateway_id).await;
            assert_eq!(
                (
                    held.get("directConnectGatewayName").and_then(Value::as_str),
                    held.get("amazonSideAsn").and_then(Value::as_i64),
                    held.get("ownerAccount").and_then(Value::as_str),
                ),
                (
                    Some(declared.name),
                    Some(declared.reported_asn),
                    Some(ACCOUNT_ID),
                ),
                "case {case:?}: expected the declared gateway in Direct Connect, got {held}"
            );

            let tags = described_tags(&router, &published).await;
            for (key, value) in declared.tags {
                assert_eq!(
                    tags.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?} on the gateway, got \
                     {tags:?}"
                );
            }
            assert_eq!(
                (
                    tags.get(STACK_NAME_TAG).map(String::as_str),
                    tags.get(LOGICAL_ID_TAG).map(String::as_str),
                ),
                (Some(row.stack_name), Some("Dgw")),
                "case {case:?}: expected CloudFormation's own tags on the gateway, got {tags:?}"
            );
        }

        let mut group_id = String::new();
        if let Some(declared) = &row.group {
            group_id = output_value(&described, "RgId").unwrap_or_default();
            let published = output_value(&described, "RgRef").unwrap_or_default();
            assert_eq!(
                published,
                direct_connect_arn("dx-resiliency-group", &group_id),
                "case {case:?}: expected Ref to publish the group's ARN around the id that \
                 service minted, got {published:?} for {group_id:?}"
            );

            let held = available_group(&router, case, &group_id).await;
            assert_eq!(
                (
                    held.get("resiliencyGroupName").and_then(Value::as_str),
                    held.get("resiliencyGroupArn").and_then(Value::as_str),
                    held.get("resiliencyGroupType").and_then(Value::as_str),
                    held.get("ownerAccount").and_then(Value::as_str),
                ),
                (
                    Some(declared.name),
                    Some(published.as_str()),
                    Some(MANAGED),
                    Some(ACCOUNT_ID),
                ),
                "case {case:?}: expected the declared group in Direct Connect, got {held}"
            );

            // The account's own LISTING reports the group the stack created, which is the other
            // read that service publishes for one.
            let listed = listed_groups(&router).await;
            assert_eq!(
                listed
                    .iter()
                    .filter_map(|held| held.get("resiliencyGroupId").and_then(Value::as_str))
                    .collect::<Vec<&str>>(),
                vec![group_id.as_str()],
                "case {case:?}: expected the listing to report the group the stack created, got \
                 {listed:?}"
            );

            let tags = described_tags(&router, &published).await;
            for (key, value) in declared.tags {
                assert_eq!(
                    tags.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?} on the group, got {tags:?}"
                );
            }
            assert_eq!(
                (
                    tags.get(STACK_NAME_TAG).map(String::as_str),
                    tags.get(LOGICAL_ID_TAG).map(String::as_str),
                ),
                (Some(row.stack_name), Some("Rg")),
                "case {case:?}: expected CloudFormation's own tags on the group, got {tags:?}"
            );
        }

        // Deleting the stack takes the resources away in that service's own words: each stays
        // describable, in the terminal state it reports a torn-down resource in.
        let (code, deleted) =
            call(&router, "DeleteStack", &[member("StackName", row.stack_name)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {code} with {deleted}"
        );
        settle_the_deletion(&router, case, &gateway_id, &group_id).await;
    }
}

/// Wait until AWS Direct Connect reports every resource the stack created in the terminal
/// `deleted` state, looking again until the harness's own generous deadline passes.
async fn settle_the_deletion(
    router: &MultiServiceRouter,
    case: &str,
    gateway_id: &str,
    group_id: &str,
) {
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut still_up: Vec<String> = Vec::new();
        if !gateway_id.is_empty() {
            let held = gateway(router, gateway_id).await;
            let state = held
                .as_ref()
                .and_then(|held| held.get("directConnectGatewayState"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if state != DELETED {
                still_up.push(format!("gateway {gateway_id}: {state:?}"));
            }
        }
        if !group_id.is_empty() {
            let held = resiliency_group(router, group_id).await;
            let state = held
                .as_ref()
                .and_then(|held| held.get("state"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if state != DELETED {
                still_up.push(format!("group {group_id}: {state:?}"));
            }
        }
        if still_up.is_empty() {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected every resource the stack created to be reported {DELETED}, \
             these are not: {still_up:?}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }
}

/// One refusal row: the resiliency model a declaration states, and whether the enum admits it.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    model: &'static str,
    /// `true` for the control row, whose model IS a member of the set.
    admitted: bool,
}

/// What a stack reports for a failure of any of CloudFormation's pre-deployment validations:
/// the count of errors the stage found and where their detail is read. Source:
/// `state::early_validation::validation_failure_summary`.
fn summary(errors: usize) -> String {
    format!(
        "Validation failed with {errors} error(s). Call DescribeEvents to retrieve the full \
         list of issues with resource and property details, resolve each error, then retry \
         the operation."
    )
}

/// The validation RESULTS `DescribeEvents` reports for `stack`: one tuple per event carrying a
/// `ValidationName`.
async fn validation_results(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String, String, String, String, String, String)> {
    let (code, body) = call(router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| {
            (
                xml_text(event, "LogicalResourceId").unwrap_or_default(),
                xml_text(event, "EventType").unwrap_or_default(),
                xml_text(event, "ResourceType").unwrap_or_default(),
                xml_text(event, "ValidationName").unwrap_or_default(),
                xml_text(event, "ValidationStatus").unwrap_or_default(),
                xml_text(event, "ValidationStatusReason").unwrap_or_default(),
                xml_text(event, "ValidationPath").unwrap_or_default(),
            )
        })
        .collect()
}

/// `(DetailedStatus, ResourceStatusReason)` of every stack-level `CREATE_FAILED` event of
/// `stack`.
async fn stack_failure_events(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String)> {
    let (code, body) = call(router, "DescribeStackEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the stack's events must be readable, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .filter(|event| {
            xml_text(event, "LogicalResourceId").as_deref() == Some(stack)
                && xml_text(event, "ResourceStatus").as_deref()
                    == Some(resource_status::CREATE_FAILED)
        })
        .map(|event| {
            (
                xml_text(event, "DetailedStatus").unwrap_or_default(),
                xml_text(event, "ResourceStatusReason").unwrap_or_default(),
            )
        })
        .collect()
}

/// A resiliency model outside the enum the type's schema states fails the RESOURCE, not the
/// template: `CreateStack` is accepted, the stack rolls back with CloudFormation's own summary
/// over the stage's one error, `DescribeEvents` carries the per-property diagnosis at the
/// property's own path — and nothing at all is created in AWS Direct Connect. A model the set
/// admits deploys.
#[tokio::test]
async fn a_resiliency_model_outside_the_enum_fails_the_resource_and_rolls_the_stack_back() {
    let rows = [
        Refused {
            case: "a value naming no resiliency model at all",
            stack_name: "dx-group-unknown-model",
            model: "nonexistent-resiliency-model",
            admitted: false,
        },
        Refused {
            case: "a value differing from a member only in case",
            stack_name: "dx-group-cased-model",
            model: "Basic-Resiliency",
            admitted: false,
        },
        Refused {
            case: "a value spelled with a space where a member has a hyphen",
            stack_name: "dx-group-spaced-model",
            model: "basic resiliency",
            admitted: false,
        },
        Refused {
            case: "the member of the set the refused rows differ from",
            stack_name: "dx-group-admitted-model",
            model: RESILIENCY_MODEL_VALUES[2],
            admitted: true,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = router();
        let template = json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                "Rg": {
                    "Type": GROUP_TYPE,
                    "Properties": {
                        "ResiliencyGroupName": "dx-enum-group",
                        "IntendedResiliencyModel": row.model,
                    },
                },
            },
        })
        .to_string();

        let (code, created) = create_stack(&router, row.stack_name, &template, &[]).await;
        assert!(
            !created.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: the TYPE is recognised whatever the property says, got {created}"
        );
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {code} with {created}"
        );

        let described = settled_stack(case, &router, REGION, row.stack_name).await;
        let settled = xml_text(&described, "StackStatus").unwrap_or_default();
        let expected_status = if row.admitted {
            status::CREATE_COMPLETE
        } else {
            status::ROLLBACK_COMPLETE
        };
        assert_eq!(
            settled, expected_status,
            "case {case:?}: expected the stack to settle {expected_status}, got {settled:?} with \
             reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );

        let failed = stack_failure_events(&router, case, row.stack_name).await;
        let expected_failed: Vec<(String, String)> = if row.admitted {
            Vec::new()
        } else {
            vec![(VALIDATION_FAILED.to_string(), summary(1))]
        };
        assert_eq!(
            failed, expected_failed,
            "case {case:?}: expected the stack's own failure event {expected_failed:?}, got \
             {failed:?}"
        );

        let results = validation_results(&router, case, row.stack_name).await;
        let expected_results: Vec<(String, String, String, String, String, String, String)> =
            if row.admitted {
                Vec::new()
            } else {
                vec![(
                    "Rg".to_string(),
                    VALIDATION_ERROR.to_string(),
                    GROUP_TYPE.to_string(),
                    PROPERTY_VALIDATION.to_string(),
                    FAILED.to_string(),
                    enum_failure_reason(row.model, RESILIENCY_MODEL_VALUES),
                    "/Resources/Rg/Properties/IntendedResiliencyModel".to_string(),
                )]
            };
        assert_eq!(
            results, expected_results,
            "case {case:?}: expected the validation results {expected_results:?}, got {results:?}"
        );

        // What the owning service holds is the proof: a refused declaration left no group
        // behind at all, and an admitted one created exactly the group it declared.
        let listed = listed_groups(&router).await;
        assert_eq!(
            listed.len(),
            usize::from(row.admitted),
            "case {case:?}: expected AWS Direct Connect to hold {} group(s), got {listed:?}",
            usize::from(row.admitted)
        );
    }
}

/// A changed NAME is applied to the resources the stack already has: either `Update…` of AWS
/// Direct Connect renames one by the identifier that service minted, so the stack keeps the
/// identity it published and that service holds the new name — rather than the resource being
/// replaced or the change being dropped.
#[tokio::test]
async fn a_changed_name_is_applied_to_the_estate_the_stack_has() {
    let case = "an update that renames the gateway and the group";
    let router = router();
    let stack_name = "dx-estate-renamed";
    let template_of = |suffix: &str| {
        json!({
            "AWSTemplateFormatVersion": "2010-09-09",
            "Resources": {
                "Dgw": {
                    "Type": GATEWAY_TYPE,
                    "Properties": {
                        "DirectConnectGatewayName": format!("dx-renamed-gateway-{suffix}"),
                        "AmazonSideAsn": PRIVATE_ASN_BOUNDS[0],
                    },
                },
                "Rg": {
                    "Type": GROUP_TYPE,
                    "Properties": {
                        "ResiliencyGroupName": format!("dx_renamed_group_{suffix}"),
                        "IntendedResiliencyModel": RESILIENCY_MODEL_VALUES[2],
                    },
                },
            },
            "Outputs": {
                "DgwRef": {"Value": {"Ref": "Dgw"}},
                "DgwId": {"Value": {"Fn::GetAtt": ["Dgw", "DirectConnectGatewayId"]}},
                "RgRef": {"Value": {"Ref": "Rg"}},
                "RgId": {"Value": {"Fn::GetAtt": ["Rg", "ResiliencyGroupId"]}}
            }
        })
        .to_string()
    };

    let described = deployed(&router, case, stack_name, &template_of("before")).await;
    let gateway_ref = output_value(&described, "DgwRef").unwrap_or_default();
    let gateway_id = output_value(&described, "DgwId").unwrap_or_default();
    let group_ref = output_value(&described, "RgRef").unwrap_or_default();
    let group_id = output_value(&described, "RgId").unwrap_or_default();
    available_gateway(&router, case, &gateway_id).await;
    available_group(&router, case, &group_id).await;

    let (code, updated) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &template_of("after")),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {code} with {updated}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );

    let described = settled_stack(case, &router, REGION, stack_name).await;
    assert_eq!(
        (
            output_value(&described, "DgwRef"),
            output_value(&described, "DgwId"),
            output_value(&described, "RgRef"),
            output_value(&described, "RgId"),
        ),
        (
            Some(gateway_ref),
            Some(gateway_id.clone()),
            Some(group_ref),
            Some(group_id.clone()),
        ),
        "case {case:?}: expected both resources to keep the identities AWS Direct Connect \
         minted, got {described}"
    );

    // The change reached the resources THAT service holds, not just the stack's record.
    let held_gateway = gateway(&router, &gateway_id).await;
    assert_eq!(
        held_gateway
            .as_ref()
            .and_then(|held| held.get("directConnectGatewayName"))
            .and_then(Value::as_str),
        Some("dx-renamed-gateway-after"),
        "case {case:?}: expected the re-declared gateway name, got {held_gateway:?}"
    );
    let held_group = resiliency_group(&router, &group_id).await;
    assert_eq!(
        held_group
            .as_ref()
            .and_then(|held| held.get("resiliencyGroupName"))
            .and_then(Value::as_str),
        Some("dx_renamed_group_after"),
        "case {case:?}: expected the re-declared group name, got {held_group:?}"
    );
    let listed = listed_groups(&router).await;
    assert_eq!(
        listed.len(),
        1,
        "case {case:?}: expected the rename to leave the ONE group the stack created, got \
         {listed:?}"
    );
}

/// The FAILURE MODE the validation's own result reports, which is read beside the rows above
/// rather than in them: the stage FAILS the operation, which is what makes the stack roll back
/// rather than carry on.
#[tokio::test]
async fn the_refused_resiliency_model_is_reported_as_a_failing_validation() {
    let case = "a value naming no resiliency model at all";
    let router = router();
    let stack = "dx-group-failure-mode";
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Rg": {
                "Type": GROUP_TYPE,
                "Properties": {
                    "ResiliencyGroupName": "dx-failure-mode-group",
                    "IntendedResiliencyModel": "no-such-resiliency",
                },
            },
        },
    })
    .to_string();

    let (code, created) = create_stack(&router, stack, &template, &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {code} with {created}"
    );
    let settled = settled_stack_status(case, &router, REGION, stack).await;
    assert_eq!(
        settled,
        status::ROLLBACK_COMPLETE,
        "case {case:?}: expected the stack to roll back, got {settled:?}"
    );

    let (code, body) = call(&router, "DescribeEvents", &[member("StackName", stack)]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: the operation's events must be readable, got {code} {body}"
    );
    let modes: Vec<String> = xml_blocks(&body, "member")
        .iter()
        .filter(|event| xml_text(event, "ValidationName").is_some())
        .map(|event| xml_text(event, "ValidationFailureMode").unwrap_or_default())
        .collect();
    assert_eq!(
        modes,
        vec![FAIL.to_string()],
        "case {case:?}: expected the one validation result to report a failing mode, got \
         {modes:?}"
    );
}
