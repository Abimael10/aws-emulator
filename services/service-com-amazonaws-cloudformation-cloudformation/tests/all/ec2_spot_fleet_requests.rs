// SPDX-License-Identifier: CC0-1.0

//! The Spot Fleet request a stack declares, READ BACK from Amazon EC2 itself.
//!
//! RULE: a declared resource of a type whose owning service this emulator serves is created by
//! that service's own create call, so an `AWS::EC2::SpotFleet` is made by `RequestSpotFleet`:
//! the `PhysicalResourceId` the stack publishes is the `sfr-…` id Amazon EC2 assigned ("Ref
//! returns the Spot Fleet request ID"), that service reports the fleet under exactly that id
//! with the configuration the declaration stated — its `IamFleetRole` among it, which is how a
//! client enumerates the fleets one fleet role owns
//! (`SpotFleetRequestConfigData$IamFleetRole`) — and a configuration that service REFUSES fails
//! the resource instead of being reported created.
//!
//! Why it matters: a stack that recorded the resource without requesting anything publishes an
//! id `DescribeSpotFleetRequests` reports nothing for, so a client reconciling its fleets by
//! the role they act under (cleanup, cost attribution, drift detection) sees none of the fleets
//! its stacks deployed, and a declaration naming a role that could never act as a fleet deploys
//! as though it could.
//!
//! The rows differ along what the rule generalises over — how the fleet describes its launches
//! (its own `LaunchSpecifications`, and a `LaunchTemplateConfigs` naming the launch template
//! the same stack deployed, with the overrides it varies), the target capacity, the request
//! type, and whether the configuration carries tags of its own — plus the negative leg, where
//! the fleet role is one IAM holds but the Spot Fleet service cannot assume.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{physical_resource_id, settled_stack};
use crate::stack_lifecycle::{
    create_stack, ec2, ec2_tag_value, iam, member, output_value, router, xml_text, xml_texts,
    REGION,
};

/// The service principal a Spot Fleet assumes its fleet role as, as the Amazon EC2 crate states
/// it: the fleet acts under that role, so a role whose trust policy does not name this
/// principal is one the fleet could never act under.
const SPOT_FLEET_PRINCIPAL: &str =
    service_com_amazonaws_ec2_amazonec2::ops::request_spot_fleet::SPOT_FLEET_SERVICE_PRINCIPAL;

/// The prefix Amazon EC2 mints a Spot Fleet request id with, which is the form the type's `Ref`
/// returns (`sfr-73fbd2ce-aa30-494c-8788-1cee4ffcbc91`).
const FLEET_ID_PREFIX: &str = "sfr-";

/// The `ResourceType` a tag specification of the fleet REQUEST must name: "the value for
/// `ResourceType` must be `spot-fleet-request`, otherwise the Spot Fleet request fails"
/// (`SpotFleetRequestConfigData$TagSpecifications`).
const FLEET_REQUEST_RESOURCE_TYPE: &str = "spot-fleet-request";

/// The public Systems Manager parameter every row resolves its AMI from — a name in AWS's own
/// `/aws/service/` hierarchy, which the published catalog answers for, so no row names an image
/// id this suite invented.
const IMAGE_PARAMETER: &str =
    "/aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64";

/// The instance type every row's launch asks for, and the one an override varies to: both
/// published types of the catalog Amazon EC2 serves.
const INSTANCE_TYPE: &str = "t3.micro";
const OVERRIDE_INSTANCE_TYPE: &str = "t3.small";

/// How a row's fleet describes what it launches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Launches {
    /// The fleet's own `LaunchSpecifications`, which name the image and instance type
    /// themselves.
    Specifications,
    /// A `LaunchTemplateConfigs` naming the `AWS::EC2::LaunchTemplate` the same stack deployed,
    /// with an override that varies the instance type.
    TemplateConfigs,
}

/// One deployed fleet: the stack it is declared in, how it describes its launches, and what the
/// configuration asks for.
struct Row {
    case: &'static str,
    stack: &'static str,
    launches: Launches,
    target_capacity: u32,
    fleet_type: &'static str,
    /// The tags the configuration states on the fleet REQUEST itself, as (key, value).
    tags: &'static [(&'static str, &'static str)],
}

/// Every positive row: the fleet is requested, reported against its fleet role, and cancelled
/// with the stack.
const ROWS: &[Row] = &[
    Row {
        case: "a fleet that names its own launch specification",
        stack: "spot-fleet-specifications",
        launches: Launches::Specifications,
        target_capacity: 1,
        fleet_type: "request",
        tags: &[],
    },
    Row {
        case: "a fleet that launches from the stack's launch template, tagged",
        stack: "spot-fleet-template-configs",
        launches: Launches::TemplateConfigs,
        target_capacity: 2,
        fleet_type: "maintain",
        tags: &[("estate", "spot"), ("owner", "platform")],
    },
];

/// Create an IAM role carrying a trust policy that names `principal`, and answer the ARN IAM
/// minted for it — the value a declaration states as its `IamFleetRole`.
async fn role_trusting(router: &MultiServiceRouter, name: &str, principal: &str) -> String {
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (code, body) = iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("AssumeRolePolicyDocument", document.as_str()),
        ],
    )
    .await;
    assert_eq!(
        code, 200,
        "CreateRole({name}) must answer 200, got {code} {body}"
    );
    xml_text(&body, "Arn")
        .unwrap_or_else(|| panic!("CreateRole({name}) must report the role's ARN, got {body}"))
}

/// The configuration one row declares, as the `SpotFleetRequestConfigData` property document.
fn configuration(row: &Row, role_arn: &str) -> Value {
    let mut config = json!({
        "IamFleetRole": role_arn,
        "TargetCapacity": row.target_capacity,
        "Type": row.fleet_type,
        "TerminateInstancesWithExpiration": true,
    });
    match row.launches {
        Launches::Specifications => {
            config["LaunchSpecifications"] = json!([{
                "ImageId": { "Ref": "Image" },
                "InstanceType": INSTANCE_TYPE,
                "WeightedCapacity": 1,
                "Monitoring": { "Enabled": false },
            }]);
        }
        Launches::TemplateConfigs => {
            config["LaunchTemplateConfigs"] = json!([{
                "LaunchTemplateSpecification": {
                    "LaunchTemplateId": { "Ref": "Lt" },
                    "Version": { "Fn::GetAtt": ["Lt", "LatestVersionNumber"] },
                },
                "Overrides": [
                    { "InstanceType": INSTANCE_TYPE, "WeightedCapacity": 1 },
                    { "InstanceType": OVERRIDE_INSTANCE_TYPE, "WeightedCapacity": 1 },
                ],
            }]);
        }
    }
    if !row.tags.is_empty() {
        config["TagSpecifications"] = json!([{
            "ResourceType": FLEET_REQUEST_RESOURCE_TYPE,
            "Tags": row
                .tags
                .iter()
                .map(|(key, value)| json!({ "Key": key, "Value": value }))
                .collect::<Vec<Value>>(),
        }]);
    }
    config
}

/// The whole template one row deploys: the AMI parameter every launch resolves, the launch
/// template the template-config row launches from, the fleet itself, and the outputs that
/// publish what the type returns.
fn template_of(row: &Row, role_arn: &str) -> String {
    let mut resources = json!({
        "Fleet": {
            "Type": "AWS::EC2::SpotFleet",
            "Properties": { "SpotFleetRequestConfigData": configuration(row, role_arn) },
        },
    });
    if row.launches == Launches::TemplateConfigs {
        resources["Lt"] = json!({
            "Type": "AWS::EC2::LaunchTemplate",
            "Properties": {
                "LaunchTemplateData": {
                    "ImageId": { "Ref": "Image" },
                    "InstanceType": INSTANCE_TYPE,
                },
            },
        });
    }
    json!({
        "Parameters": {
            "Image": {
                "Type": "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
                "Default": IMAGE_PARAMETER,
            },
        },
        "Resources": resources,
        "Outputs": {
            "FleetRef": { "Value": { "Ref": "Fleet" } },
            "FleetId": { "Value": { "Fn::GetAtt": ["Fleet", "Id"] } },
        },
    })
    .to_string()
}

/// Every Spot Fleet request this account holds, as the (id, fleet role, state) triples
/// `DescribeSpotFleetRequests` reports — the very read a client enumerating one role's fleets
/// makes, taken with no id at all.
///
/// The three members are read as PARALLEL lists, which is sound because that call reports every
/// one of them on every fleet it describes: the id and the state are the fleet's own members and
/// `IamFleetRole` is a required member of the configuration it echoes.
async fn described_fleets(router: &MultiServiceRouter, case: &str) -> Vec<(String, String, String)> {
    let (code, body) = ec2(router, "DescribeSpotFleetRequests", &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeSpotFleetRequests must answer 200, got {code} {body}"
    );
    let ids = xml_texts(&body, "spotFleetRequestId");
    let roles = xml_texts(&body, "iamFleetRole");
    let states = xml_texts(&body, "spotFleetRequestState");
    assert_eq!(
        (ids.len(), roles.len()),
        (states.len(), states.len()),
        "case {case:?}: every described fleet reports its id, its fleet role and its state, \
         got {ids:?} / {roles:?} / {states:?}"
    );
    ids.into_iter()
        .zip(roles)
        .zip(states)
        .map(|((id, role), state)| (id, role, state))
        .collect()
}

/// The fleets `DescribeSpotFleetRequests` reports for `role_arn`, in the order it reports them:
/// the positive leg of the enumeration a client reconciles its fleets with.
async fn fleet_ids_with_role(
    router: &MultiServiceRouter,
    case: &str,
    role_arn: &str,
) -> Vec<String> {
    described_fleets(router, case)
        .await
        .into_iter()
        .filter(|(_, role, _)| role == role_arn)
        .map(|(id, _, _)| id)
        .collect()
}

/// The state `DescribeSpotFleetRequests` reports for one fleet, or `None` where it reports no
/// fleet for the id at all.
async fn fleet_state(router: &MultiServiceRouter, case: &str, fleet_id: &str) -> Option<String> {
    described_fleets(router, case)
        .await
        .into_iter()
        .find(|(id, _, _)| id == fleet_id)
        .map(|(_, _, state)| state)
}

/// What Amazon EC2 reports for ONE fleet, read by the id the stack published.
async fn describe_fleet(router: &MultiServiceRouter, case: &str, fleet_id: &str) -> String {
    let (code, body) = ec2(
        router,
        "DescribeSpotFleetRequests",
        &[("SpotFleetRequestId.1", fleet_id)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeSpotFleetRequests({fleet_id}) must answer 200, got {code} {body}"
    );
    body
}

/// Whether `value` is an id of the form Amazon EC2 mints a Spot Fleet request in: the `sfr-`
/// prefix and a UUID in the 8-4-4-4-12 grouping, which is the shape that service's own ids
/// carry and not a name CloudFormation computed.
fn is_a_fleet_id(value: &str) -> bool {
    let Some(uuid) = value.strip_prefix(FLEET_ID_PREFIX) else {
        return false;
    };
    let groups: Vec<&str> = uuid.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(width, group)| {
                group.len() == *width && group.chars().all(|digit| digit.is_ascii_hexdigit())
            })
}

/// RULE: every declared `AWS::EC2::SpotFleet` is a Spot Fleet request Amazon EC2 really holds,
/// under the id the stack published, reported against the fleet role the declaration stated —
/// and the cancellation that takes the stack down is what takes that fleet out of the
/// enumeration again.
#[tokio::test]
async fn a_stacks_declared_spot_fleet_is_requested_and_reported_in_amazon_ec2() {
    let router = router();
    let role_arn = role_trusting(&router, "stack-spot-fleet-role", SPOT_FLEET_PRINCIPAL).await;

    let mut deployed: Vec<(&Row, String)> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let (code, body) =
            create_stack(&router, row.stack, &template_of(row, &role_arn), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: CreateStack must answer 200, got {code} {body}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let status = xml_text(&described, "StackStatus").unwrap_or_default();
        assert_eq!(
            status, "CREATE_COMPLETE",
            "case {case:?}: a declared Spot Fleet must deploy, got {status}: {described}"
        );

        // The identity in force is the one Amazon EC2 assigned, and it is what both return
        // values the type publishes carry.
        let fleet_id = physical_resource_id(&router, case, row.stack, "Fleet").await;
        assert!(
            is_a_fleet_id(&fleet_id),
            "case {case:?}: expected an sfr- id carrying a UUID, got {fleet_id:?}"
        );
        for key in ["FleetRef", "FleetId"] {
            let reported = output_value(&described, key).unwrap_or_default();
            assert_eq!(
                reported, fleet_id,
                "case {case:?}: the {key} output must carry the id Amazon EC2 assigned, \
                 got {reported:?}"
            );
        }

        // The fleet is really there, under that id, with the configuration the declaration
        // stated — and it is `active`, which is the state the stack held its creation open for.
        let reported = describe_fleet(&router, case, &fleet_id).await;
        let state = xml_text(&reported, "spotFleetRequestState").unwrap_or_default();
        assert_eq!(
            state, "active",
            "case {case:?}: a fleet of a completed stack is holding capacity, got {state:?}"
        );
        let role = xml_text(&reported, "iamFleetRole").unwrap_or_default();
        assert_eq!(
            role, role_arn,
            "case {case:?}: the fleet must report the fleet role the declaration stated, \
             got {role:?}"
        );
        let capacity = xml_text(&reported, "targetCapacity").unwrap_or_default();
        assert_eq!(
            capacity,
            row.target_capacity.to_string(),
            "case {case:?}: the target capacity must round-trip, got {capacity:?}"
        );
        let fleet_type = xml_text(&reported, "type").unwrap_or_default();
        assert_eq!(
            fleet_type, row.fleet_type,
            "case {case:?}: the request type must round-trip, got {fleet_type:?}"
        );
        match row.launches {
            Launches::Specifications => {
                let instance_type = xml_text(&reported, "instanceType").unwrap_or_default();
                assert_eq!(
                    instance_type, INSTANCE_TYPE,
                    "case {case:?}: the launch specification's instance type must round-trip, \
                     got {instance_type:?}"
                );
            }
            Launches::TemplateConfigs => {
                let launch_template =
                    physical_resource_id(&router, case, row.stack, "Lt").await;
                let named = xml_text(&reported, "launchTemplateId").unwrap_or_default();
                assert_eq!(
                    named, launch_template,
                    "case {case:?}: the fleet must launch from the template the stack deployed, \
                     got {named:?}"
                );
                let varied = xml_texts(&reported, "instanceType");
                assert_eq!(
                    varied,
                    vec![
                        INSTANCE_TYPE.to_string(),
                        OVERRIDE_INSTANCE_TYPE.to_string()
                    ],
                    "case {case:?}: every override the declaration stated must round-trip"
                );
            }
        }
        for (key, value) in row.tags {
            let reported_tag = ec2_tag_value(&reported, key);
            assert_eq!(
                reported_tag.as_deref(),
                Some(*value),
                "case {case:?}: the fleet's {key:?} tag must round-trip, got {reported_tag:?}"
            );
        }

        // The enumeration a client reconciles with: the fleet role answers for exactly the
        // fleets deployed under it so far.
        let listed = fleet_ids_with_role(&router, case, &role_arn).await;
        let mut expected: Vec<String> = deployed
            .iter()
            .map(|(_, id): &(&Row, String)| id.clone())
            .collect();
        expected.push(fleet_id.clone());
        expected.sort();
        let mut found = listed.clone();
        found.sort();
        assert_eq!(
            found, expected,
            "case {case:?}: the fleet role must answer for every fleet requested under it, \
             got {listed:?}"
        );
        deployed.push((row, fleet_id));
    }

    // Taking the stacks down cancels the fleets, so the enumeration no longer reports them
    // holding capacity.
    for (row, fleet_id) in &deployed {
        let case = row.case;
        let (code, body) = crate::stack_lifecycle::call(
            &router,
            "DeleteStack",
            &[member("StackName", row.stack)],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: DeleteStack must answer 200, got {code} {body}"
        );
        let described = settled_stack(case, &router, REGION, row.stack).await;
        let status = xml_text(&described, "StackStatus").unwrap_or_default();
        assert!(
            status.is_empty() || status == "DELETE_COMPLETE",
            "case {case:?}: the stack must delete, got {status:?}: {described}"
        );
        let state = fleet_state(&router, case, fleet_id).await.unwrap_or_default();
        assert!(
            state.starts_with("cancelled"),
            "case {case:?}: the deleted stack's fleet must be cancelled in Amazon EC2, \
             got {state:?}"
        );
    }
}

/// RULE, negative leg: the fleet acts UNDER the role the declaration names, so a role the Spot
/// Fleet service cannot assume is a configuration Amazon EC2 refuses — which fails the resource
/// and rolls the stack back, leaving that role answering for no fleet at all.
#[tokio::test]
async fn a_spot_fleet_whose_role_the_service_cannot_assume_fails_its_stack() {
    let case = "a held role that trusts another service only";
    let router = router();
    let role_arn = role_trusting(&router, "stack-not-a-fleet-role", "ecs.amazonaws.com").await;
    let row = Row {
        case,
        stack: "spot-fleet-wrong-trust-policy",
        launches: Launches::Specifications,
        target_capacity: 1,
        fleet_type: "request",
        tags: &[],
    };

    let (code, body) = create_stack(&router, row.stack, &template_of(&row, &role_arn), &[]).await;
    assert_eq!(
        code, 200,
        "case {case:?}: CreateStack must answer 200, got {code} {body}"
    );
    let described = settled_stack(case, &router, REGION, row.stack).await;
    let status = xml_text(&described, "StackStatus").unwrap_or_default();
    assert_eq!(
        status, "ROLLBACK_COMPLETE",
        "case {case:?}: a fleet Amazon EC2 refuses must roll the stack back, \
         got {status:?}: {described}"
    );
    let listed = fleet_ids_with_role(&router, case, &role_arn).await;
    assert!(
        listed.is_empty(),
        "case {case:?}: a refused request creates no fleet, got {listed:?}"
    );
}
