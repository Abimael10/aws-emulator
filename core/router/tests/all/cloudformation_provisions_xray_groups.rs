// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an `AWS::XRay::Group` really creates a group in AWS
//! X-Ray, and the ARN the stack's `Outputs` hand back is the one that service resolves.
//!
//! The rule under test: **a resource of a type this emulator provisions is created in the
//! service that OWNS it, through that service's own public API, under the configuration the
//! template declared — so once the stack reports `CREATE_COMPLETE` the owning service's own
//! reads resolve the resource, every `Ref`/`Fn::GetAtt` the template publishes as an output
//! carries the identifier that service minted for it, and deleting the stack removes it
//! there.** Two crates have to agree for that to hold (CloudFormation makes the calls, X-Ray
//! serves them and mints the `GroupARN`), so the suite lives here rather than in either
//! crate.
//!
//! The rows differ along what the rule generalises over: whether the template NAMES the group
//! or leaves CloudFormation to generate its name, whether it declares a `FilterExpression`
//! and which form of the expression language it is written in, whether it declares an
//! `InsightsConfiguration` (and with notifications on), whether it carries `Tags`, how many
//! outputs it publishes over the resource and with which intrinsic (`Fn::GetAtt` of the
//! published attribute and `Ref` of the resource itself), whether it is written in JSON or
//! YAML, and the Region it deploys in.
//!
//! Every expected value is computed here from what the row declares, never read back out of
//! the answer being checked — except the GENERATED name of the row that declares none, and
//! the per-group suffix X-Ray draws for the ARN, which are those services' to decide: what is
//! checked about them is that the ARN the stack publishes is a group ARN of that name in that
//! Region, that the name is inside the member's published bound, and that X-Ray's own reads
//! resolve it.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsQuery form encoder and the XML element readers this crate writes ONCE
// (`tests/all/query_wire.rs`): "Nothing new should write another one — call this module."
use super::query_wire::{elements, form_encode, opt_element, outputs_of, physical_ids_of};

/// How long a stack is given to reach a terminal status, and how long between two looks.
///
/// What is waited for is the CONDITION — the stack out of its `_IN_PROGRESS` statuses — and
/// the deadline is only a safety net so a wait that will never be satisfied still ends. It is
/// far above the clock-owed span a stack transition takes, so a loaded host cannot decide the
/// outcome.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// What a stack reports when CloudFormation refuses a deployment at one of its pre-deployment
/// validations: the count of errors that stage found and where their detail is read. Source:
/// `state::early_validation::validation_failure_summary`.
const VALIDATION_SUMMARY: &str = "Validation failed with 1 error(s). Call DescribeEvents to \
                                  retrieve the full list of issues with resource and property \
                                  details, resolve each error, then retry the operation.";

/// The stack-level tag every row's `CreateStack` states, which CloudFormation propagates to
/// the resources the stack creates.
const STACK_TAG_KEY: &str = "Stack";
const STACK_TAG_VALUE: &str = "xray-group-suite";

/// The longest a group's name may be: `CreateGroup$GroupName` is `@length(min: 1, max: 32)`
/// in X-Ray's own model, which is the bound a name CloudFormation generates has to be inside.
const GROUP_NAME_MAX: usize = 32;

/// How many hex digits the per-group segment of a group ARN carries
/// (`arn:aws:xray:<region>:<account>:group/<GroupName>/<12 hex digits>`): the value X-Ray
/// draws per group, which is why no rule in CloudFormation could compute the ARN.
const GROUP_ARN_SUFFIX_CHARS: usize = 12;

/// The emulator with the two services this suite exercises, mounted on ONE engine exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here; the in-process callback CloudFormation makes signs its request either way (the
/// credential scope is what routes it).
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_xray_awsxray::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in `region`, which
/// is what routes the request and what each handler reads its Region from. The signature is
/// never checked: this router runs with request authentication off.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// Call one CloudFormation operation over its awsQuery wire, signed for `region`.
async fn cloudformation(
    router: &MultiServiceRouter,
    region: &str,
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
        .header("host", format!("cloudformation.{region}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation", region))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = String::from_utf8_lossy(response.body()).to_string();
    (status, payload)
}

/// Call one X-Ray operation over its restJson1 wire, signed for `region`.
async fn xray(router: &MultiServiceRouter, region: &str, uri: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", format!("xray.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("xray", region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// The stack's `StackStatus` once it is out of its `_IN_PROGRESS` statuses, with the whole
/// `DescribeStacks` answer it was read from.
///
/// Waits for the CONDITION, looking again every [`POLL_INTERVAL`] until it holds or
/// [`TERMINAL_DEADLINE`] passes, and answers the last status it read either way — so a stack
/// that never settles fails on the status the case asserts rather than on a measured span.
async fn settled_stack(
    router: &MultiServiceRouter,
    region: &str,
    stack: &str,
) -> (String, String) {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let (_, described) =
            cloudformation(router, region, "DescribeStacks", &[("StackName", stack)]).await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if !status.ends_with("_IN_PROGRESS") || Instant::now() >= deadline {
            return (status, described);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// X-Ray's own read of ONE group by the ARN the stack published: the `Group` document, or
/// `None` where that service holds no such group (which it answers under its one client-error
/// class).
async fn read_group(router: &MultiServiceRouter, region: &str, arn: &str) -> Option<Value> {
    let (status, answered) = xray(router, region, "/GetGroup", &json!({ "GroupARN": arn })).await;
    if status != 200 {
        return None;
    }
    answered.get("Group").cloned()
}

/// Every group X-Ray reports for the account, walked over every page of `GetGroups` the way a
/// client reading the whole listing does.
async fn listed_groups(router: &MultiServiceRouter, region: &str) -> Vec<Value> {
    let mut groups = Vec::new();
    let mut token: Option<String> = None;
    // Far above the number of groups a row creates, so the walk ends on the listing's own
    // last page rather than on this bound.
    for _ in 0..16 {
        let mut body = json!({});
        if let Some(token) = &token {
            body["NextToken"] = json!(token);
        }
        let (status, answered) = xray(router, region, "/Groups", &body).await;
        assert_eq!(
            status, 200,
            "GetGroups must answer 200, got {status} with body {answered}"
        );
        groups.extend(
            answered
                .get("Groups")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
        );
        match answered.get("NextToken").and_then(Value::as_str) {
            Some(next) => token = Some(next.to_string()),
            None => break,
        }
    }
    groups
}

/// The tags X-Ray reports for a resource ARN, as the map a client reads them as.
async fn resource_tags(
    router: &MultiServiceRouter,
    region: &str,
    arn: &str,
) -> BTreeMap<String, String> {
    let (status, listed) = xray(
        router,
        region,
        "/ListTagsForResource",
        &json!({ "ResourceARN": arn }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListTagsForResource must answer 200, got {status} with body {listed}"
    );
    listed
        .get("Tags")
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("no 'Tags' array in {listed}"))
        .iter()
        .filter_map(|tag| {
            Some((
                tag.get("Key").and_then(Value::as_str)?.to_string(),
                tag.get("Value").and_then(Value::as_str)?.to_string(),
            ))
        })
        .collect()
}

/// One row of the table: a template, and what the rule says the group it declares must then
/// be.
struct Row {
    case: &'static str,
    /// The Region the row deploys in, which is the Region the group is created in and the one
    /// its ARN carries.
    region: &'static str,
    /// The template body, in the form the row writes it (JSON or YAML).
    template: fn() -> String,
    /// The name the template declares, or `None` for a template that names no group — whose
    /// group carries the name CloudFormation generated.
    declared_name: Option<&'static str>,
    /// The `FilterExpression` the template declares, which X-Ray must report for the group,
    /// or `None` for a template that declares none.
    declared_filter: Option<&'static str>,
    /// The `InsightsConfiguration` the template declares, which X-Ray must report for the
    /// group, or `None` for a template that declares none.
    declared_insights: Option<fn() -> Value>,
    /// The tags X-Ray must report for the group's ARN, beside the stack's own.
    expected_tags: &'static [(&'static str, &'static str)],
    /// The output keys whose value must be the group's ARN — every one of them, since both
    /// `Ref` and `Fn::GetAtt GroupARN` of this type resolve to it.
    arn_outputs: &'static [&'static str],
}

/// The rows. Written as a function rather than a `const` because each template is built here.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a named, tagged group with a comparison filter, read with Fn::GetAtt and Ref",
            region: "us-east-1",
            template: || {
                json!({
                    "AWSTemplateFormatVersion": "2010-09-09",
                    "Description": "an X-Ray group",
                    "Resources": {
                        "Group": {
                            "Type": "AWS::XRay::Group",
                            "Properties": {
                                "GroupName": "orders-faults",
                                "FilterExpression": "fault = true",
                                "Tags": [
                                    { "Key": "Owner", "Value": "orders" },
                                    { "Key": "Tier", "Value": "prod" }
                                ]
                            }
                        }
                    },
                    "Outputs": {
                        "GroupARN": { "Value": { "Fn::GetAtt": ["Group", "GroupARN"] } },
                        "GroupRef": { "Value": { "Ref": "Group" } }
                    }
                })
                .to_string()
            },
            declared_name: Some("orders-faults"),
            declared_filter: Some("fault = true"),
            declared_insights: None,
            expected_tags: &[("Owner", "orders"), ("Tier", "prod")],
            arn_outputs: &["GroupARN", "GroupRef"],
        },
        Row {
            case: "a group with insights and notifications on, filtered by a service call",
            region: "eu-west-1",
            template: || {
                json!({
                    "Resources": {
                        "Group": {
                            "Type": "AWS::XRay::Group",
                            "Properties": {
                                "GroupName": "checkout-insights",
                                "FilterExpression": "service(\"checkout\")",
                                "InsightsConfiguration": {
                                    "InsightsEnabled": true,
                                    "NotificationsEnabled": true
                                }
                            }
                        }
                    },
                    "Outputs": {
                        "GroupARN": { "Value": { "Fn::GetAtt": ["Group", "GroupARN"] } }
                    }
                })
                .to_string()
            },
            declared_name: Some("checkout-insights"),
            declared_filter: Some("service(\"checkout\")"),
            declared_insights: Some(|| {
                json!({ "InsightsEnabled": true, "NotificationsEnabled": true })
            }),
            expected_tags: &[],
            arn_outputs: &["GroupARN"],
        },
        Row {
            case: "a group in YAML with a unary filter and insights off",
            region: "ap-southeast-2",
            template: || {
                [
                    "Resources:",
                    "  Group:",
                    "    Type: AWS::XRay::Group",
                    "    Properties:",
                    "      GroupName: slow-calls",
                    "      FilterExpression: responsetime > 2",
                    "      InsightsConfiguration:",
                    "        InsightsEnabled: false",
                    "        NotificationsEnabled: false",
                    "Outputs:",
                    "  GroupARN:",
                    "    Value: !GetAtt Group.GroupARN",
                    "",
                ]
                .join("\n")
            },
            declared_name: Some("slow-calls"),
            declared_filter: Some("responsetime > 2"),
            declared_insights: Some(|| {
                json!({ "InsightsEnabled": false, "NotificationsEnabled": false })
            }),
            expected_tags: &[],
            arn_outputs: &["GroupARN"],
        },
        Row {
            case: "a group the template does not name, so CloudFormation names it",
            region: "us-west-2",
            template: || {
                json!({
                    "Resources": {
                        "Group": {
                            "Type": "AWS::XRay::Group",
                            "Properties": {
                                "FilterExpression": "ok",
                                "Tags": [{ "Key": "Owner", "Value": "platform" }]
                            }
                        }
                    },
                    "Outputs": {
                        "GroupARN": { "Value": { "Fn::GetAtt": ["Group", "GroupARN"] } }
                    }
                })
                .to_string()
            },
            declared_name: None,
            declared_filter: Some("ok"),
            declared_insights: None,
            expected_tags: &[("Owner", "platform")],
            arn_outputs: &["GroupARN"],
        },
    ]
}

/// The group name inside a group ARN: the segment between `group/` and the per-group suffix
/// X-Ray drew for it.
fn name_in(arn: &str) -> String {
    arn.rsplit('/')
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_stack_provisions_the_xray_group_its_template_declares() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let stack_name = format!("xray-group-stack-{index}");
        let template = (row.template)();

        let (status, body) = cloudformation(
            &router,
            region,
            "CreateStack",
            &[
                ("StackName", &stack_name),
                ("TemplateBody", &template),
                // A stack-level tag, which CloudFormation propagates to the resources it
                // creates — so the group has to end up carrying it beside its own.
                ("Tags.member.1.Key", STACK_TAG_KEY),
                ("Tags.member.1.Value", STACK_TAG_VALUE),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
        );

        let (stack_status, described) = settled_stack(&router, region, &stack_name).await;
        assert_eq!(
            stack_status, "CREATE_COMPLETE",
            "case {case:?}: expected the stack to reach CREATE_COMPLETE, got {described}"
        );

        let (status, resources) = cloudformation(
            &router,
            region,
            "DescribeStackResources",
            &[("StackName", &stack_name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeStackResources, got {status} with \
             body {resources}"
        );
        let physical_id = physical_ids_of(&resources)
            .get("Group")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: no physical id for Group in {resources}"));

        // The type's identity is the `GroupARN` X-Ray minted, which carries the group's name:
        // the one the template asked for, or the one CloudFormation generated for a template
        // that named none.
        let name = match row.declared_name {
            Some(declared) => declared.to_string(),
            None => name_in(&physical_id),
        };
        let prefix = format!("arn:aws:xray:{region}:{ACCOUNT}:group/{name}/");
        let suffix = physical_id.strip_prefix(&prefix).unwrap_or_default();
        assert!(
            !suffix.is_empty()
                && suffix.len() == GROUP_ARN_SUFFIX_CHARS
                && suffix.chars().all(|digit| digit.is_ascii_hexdigit()),
            "case {case:?}: expected the physical id to be a group ARN {prefix}<{GROUP_ARN_SUFFIX_CHARS} hex>, \
             got {physical_id:?}"
        );
        assert!(
            !name.is_empty() && name.len() <= GROUP_NAME_MAX,
            "case {case:?}: expected a group name inside the published 1..{GROUP_NAME_MAX}, got \
             {name:?}"
        );

        // Every output the template declares carries a VALUE, and every output over this
        // resource carries the ARN X-Ray minted — both `Fn::GetAtt GroupARN` and `Ref`.
        let outputs = outputs_of(&described);
        assert_eq!(
            outputs.len(),
            elements(&described, "OutputKey").len(),
            "case {case:?}: expected every declared Output to carry an OutputValue, got \
             {described}"
        );
        for key in row.arn_outputs {
            assert_eq!(
                outputs.get(*key),
                Some(&physical_id),
                "case {case:?}: expected output {key:?} to be the group ARN {physical_id:?}, got \
                 {:?}",
                outputs.get(*key)
            );
        }
        assert_eq!(
            outputs.len(),
            row.arn_outputs.len(),
            "case {case:?}: expected exactly the {} output(s) the template declares, got \
             {outputs:?}",
            row.arn_outputs.len()
        );

        // X-Ray itself reports the group, under the name the template declared and carrying
        // every member it declared.
        let reported = read_group(&router, region, &physical_id)
            .await
            .unwrap_or_else(|| panic!("case {case:?}: GetGroup resolves no group at {physical_id}"));
        assert_eq!(
            reported.get("GroupARN").and_then(Value::as_str),
            Some(physical_id.as_str()),
            "case {case:?}: expected the group to carry the ARN {physical_id}, got {reported}"
        );
        assert_eq!(
            reported.get("GroupName").and_then(Value::as_str),
            Some(name.as_str()),
            "case {case:?}: expected the group to be named {name:?}, got {reported}"
        );
        assert_eq!(
            reported.get("FilterExpression").and_then(Value::as_str),
            row.declared_filter,
            "case {case:?}: expected the group's FilterExpression to be the declared {:?}, got \
             {reported}",
            row.declared_filter
        );
        match row.declared_insights {
            Some(declared) => {
                let declared = declared();
                assert_eq!(
                    reported.get("InsightsConfiguration"),
                    Some(&declared),
                    "case {case:?}: expected the group's InsightsConfiguration to be the declared \
                     {declared}, got {reported}"
                );
            }
            None => assert!(
                reported.get("InsightsConfiguration").is_none_or(Value::is_null),
                "case {case:?}: expected no InsightsConfiguration on a group that declares none, \
                 got {reported}"
            ),
        }

        // And the account's own listing holds it, so a client walking `GetGroups` finds the
        // group the stack created.
        assert!(
            listed_groups(&router, region)
                .await
                .iter()
                .any(|group| group.get("GroupARN").and_then(Value::as_str)
                    == Some(physical_id.as_str())),
            "case {case:?}: expected GetGroups to report the group at {physical_id}"
        );

        // The ARN the stack published is a handle X-Ray takes where a group is NAMED: a
        // service graph scoped to it is derived rather than refused.
        let (status, graph) = xray(
            &router,
            region,
            "/ServiceGraph",
            &json!({ "StartTime": 0.0_f64, "EndTime": 1_800.0_f64, "GroupARN": physical_id }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected GetServiceGraph scoped to the published ARN to answer 200, \
             got {status} with body {graph}"
        );

        // The tags the declaration asked for are on the group, beside the stack's own, and
        // reachable through the ARN the stack published. The three `aws:cloudformation:*`
        // tags are NOT: X-Ray refuses that namespace to a caller, and CloudFormation must not
        // fail a stack over tags it adds itself.
        let tags = resource_tags(&router, region, &physical_id).await;
        for (key, value) in row.expected_tags {
            assert_eq!(
                tags.get(*key).map(String::as_str),
                Some(*value),
                "case {case:?}: expected tag {key:?} to be {value:?}, got {tags:?}"
            );
        }
        assert_eq!(
            tags.get(STACK_TAG_KEY).map(String::as_str),
            Some(STACK_TAG_VALUE),
            "case {case:?}: expected the stack's own tag to be propagated, got {tags:?}"
        );
        assert!(
            !tags.keys().any(|key| key.starts_with("aws:")),
            "case {case:?}: expected no reserved tag key to be sent to X-Ray, got {tags:?}"
        );
        assert_eq!(
            tags.len(),
            row.expected_tags.len() + 1,
            "case {case:?}: expected exactly the {} tag(s) the template declared beside the \
             stack's own, got {tags:?}",
            row.expected_tags.len()
        );

        // Deleting the stack deletes the group X-Ray holds for it.
        let (status, body) =
            cloudformation(&router, region, "DeleteStack", &[("StackName", &stack_name)]).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
        );
        assert!(
            read_group(&router, region, &physical_id).await.is_none(),
            "case {case:?}: expected GetGroup to resolve nothing at {physical_id} once the stack \
             was deleted"
        );
    }
}

/// A stack whose template declares a group that is ALREADY there does not report a resource
/// nothing created: the stack fails before anything is provisioned — CloudFormation's own
/// pre-deployment validation establishes that the name is held — and the group a client already
/// had is untouched.
///
/// The name is one the account already holds, which `CreateGroup` would reject rather than
/// redefining, so the deployment could not have been carried out either way.
#[tokio::test]
async fn an_xray_group_the_owning_service_refuses_fails_the_stack() {
    let router = build_router();
    let case = "a template naming a group a client already created";
    let region = "us-east-2";
    let name = "already-there";

    let (status, created) = xray(
        &router,
        region,
        "/CreateGroup",
        &json!({ "GroupName": name, "FilterExpression": "fault = true" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the client's own CreateGroup to succeed, got {status} with body \
         {created}"
    );
    let arn = created
        .get("Group")
        .and_then(|group| group.get("GroupARN"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no GroupARN in {created}"))
        .to_string();

    let template = json!({
        "Resources": {
            "Group": {
                "Type": "AWS::XRay::Group",
                "Properties": {
                    "GroupName": name,
                    "FilterExpression": "error = true"
                }
            }
        }
    })
    .to_string();
    let (status, body) = cloudformation(
        &router,
        region,
        "CreateStack",
        &[("StackName", "group-clash"), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );

    let (stack_status, described) = settled_stack(&router, region, "group-clash").await;
    assert_eq!(
        stack_status, "ROLLBACK_COMPLETE",
        "case {case:?}: expected the failed create to be rolled back, got {described}"
    );
    let reason = opt_element(&described, "StackStatusReason").unwrap_or_default();
    // The name was taken before anything was provisioned, so CloudFormation refused the
    // deployment at its pre-deployment validation: the stack carries the summary over that
    // stage's errors, and which resource collided with what is the detail on the validation
    // result `DescribeEvents` reports.
    assert_eq!(
        reason, VALIDATION_SUMMARY,
        "case {case:?}: expected the pre-deployment validation summary, got {reason:?}"
    );

    // The group the client created is still exactly as it was.
    let held = read_group(&router, region, &arn)
        .await
        .unwrap_or_else(|| panic!("case {case:?}: the client's own group must still resolve"));
    assert_eq!(
        held.get("FilterExpression").and_then(Value::as_str),
        Some("fault = true"),
        "case {case:?}: expected the client's own configuration to be untouched, got {held}"
    );
}
