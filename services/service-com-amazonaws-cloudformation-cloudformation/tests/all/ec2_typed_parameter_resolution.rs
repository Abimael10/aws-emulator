// SPDX-License-Identifier: CC0-1.0

//! The AWS-SPECIFIC parameter types a template declares, resolved against the resources of the
//! account.
//!
//! RULE: a parameter declared with one of the AWS-specific parameter types does not take any
//! string — its value names a resource that has to BE THERE, and CloudFormation resolves it
//! against the account and Region the operation runs in. A value that names a resource the
//! owning service really holds deploys; a value that names none is no value of that parameter,
//! so the operation fails with CloudFormation's own parameter-validation diagnosis and the
//! stack rolls back, having created nothing.
//!
//! The rows differ along what the rule generalises over: which AWS-specific type the parameter
//! is declared with (and so which service is asked and which resource kind is named), whether
//! the value is an identifier that service assigns or a name a caller chose, whether the
//! parameter is scalar or a `List<…>` of that type, and — the other side of the rule — a
//! parameter whose type is not an AWS-specific one, which takes a value naming nothing at all.
//! Where the value COMES FROM is the same rule read once more, which
//! `ec2_typed_parameters.rs` holds: a declared `Default` is a value in force too.
//!
//! Every type in the catalogue is covered, including ones the issue this suite was written for
//! never cited: the catalogue is the CloudFormation User Guide's own list of AWS-specific
//! parameter types.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::parameter_constraints::aws_specific_types::does_not_exist;

use crate::stack_lifecycle::{
    call, create_stack, ec2, member, router, settled_stack, xml_text, xml_texts,
};

/// The resources this suite creates OUTSIDE any stack, through each owning service's own wire,
/// so a row's "exists" value names a resource that service really holds.
struct Estate {
    vpc_id: String,
    subnet_id: String,
    group_id: String,
    volume_id: String,
    image_id: String,
    zone_name: String,
    key_name: String,
}

/// Create that estate in Amazon EC2.
async fn estate(router: &MultiServiceRouter) -> Estate {
    let (status, body) = ec2(router, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await;
    assert_eq!(status, 200, "CreateVpc must answer 200, got {status} {body}");
    let vpc_id = xml_text(&body, "vpcId").expect("the VPC's own id");
    let (status, body) = ec2(
        router,
        "DescribeAvailabilityZones",
        &[],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAvailabilityZones must answer 200, got {status} {body}"
    );
    let zone_name = xml_texts(&body, "zoneName")
        .into_iter()
        .next()
        .expect("a zone of the Region");
    let (status, body) = ec2(
        router,
        "CreateSubnet",
        &[
            ("VpcId", &vpc_id),
            ("CidrBlock", "10.90.1.0/24"),
            ("AvailabilityZone", &zone_name),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateSubnet must answer 200, got {status} {body}"
    );
    let subnet_id = xml_text(&body, "subnetId").expect("the subnet's own id");
    let (status, body) = ec2(
        router,
        "CreateSecurityGroup",
        &[
            ("GroupName", "the-estate-group"),
            ("GroupDescription", "a group of the estate"),
            ("VpcId", &vpc_id),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateSecurityGroup must answer 200, got {status} {body}"
    );
    let group_id = xml_text(&body, "groupId").expect("the group's own id");
    let (status, body) = ec2(
        router,
        "CreateVolume",
        &[("AvailabilityZone", &zone_name), ("Size", "1")],
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateVolume must answer 200, got {status} {body}"
    );
    let volume_id = xml_text(&body, "volumeId").expect("the volume's own id");
    let (status, body) = ec2(router, "CreateKeyPair", &[("KeyName", "the-estate-key")]).await;
    assert_eq!(
        status, 200,
        "CreateKeyPair must answer 200, got {status} {body}"
    );
    let key_name = xml_text(&body, "keyName").expect("the key pair's name");
    let (status, body) = ec2(router, "DescribeImages", &[]).await;
    assert_eq!(
        status, 200,
        "DescribeImages must answer 200, got {status} {body}"
    );
    let image_id = xml_texts(&body, "imageId")
        .into_iter()
        .next()
        .expect("an image of the Region");
    Estate {
        vpc_id,
        subnet_id,
        group_id,
        volume_id,
        image_id,
        zone_name,
        key_name,
    }
}

/// What a row declares and what it supplies for it.
struct TypedCase {
    case: &'static str,
    /// The `Type` the parameter is declared with.
    declared_type: &'static str,
    /// The value that names a resource of the estate, chosen from it by this closure-free
    /// selector.
    exists: fn(&Estate) -> String,
    /// A value of the same FORM that names no resource of the account.
    missing: &'static str,
}

/// Every AWS-specific parameter type, each with a value of the estate and a value of the same
/// form that names nothing.
///
/// Source: the CloudFormation User Guide's "AWS-specific parameter types" list. The missing
/// values are well-formed identifiers and names of the same kind — what makes them fail is
/// that no resource of the account carries them.
const TYPED_CASES: &[TypedCase] = &[
    TypedCase {
        case: "a VPC id",
        declared_type: "AWS::EC2::VPC::Id",
        exists: |estate| estate.vpc_id.clone(),
        missing: "vpc-0aaaaaaaaaaaaaaaa",
    },
    TypedCase {
        case: "a subnet id",
        declared_type: "AWS::EC2::Subnet::Id",
        exists: |estate| estate.subnet_id.clone(),
        missing: "subnet-0aaaaaaaaaaaaaaaa",
    },
    TypedCase {
        case: "a security group id",
        declared_type: "AWS::EC2::SecurityGroup::Id",
        exists: |estate| estate.group_id.clone(),
        missing: "sg-0aaaaaaaaaaaaaaaa",
    },
    TypedCase {
        case: "a security group name",
        declared_type: "AWS::EC2::SecurityGroup::GroupName",
        exists: |_| "the-estate-group".to_string(),
        missing: "no-such-group",
    },
    TypedCase {
        case: "a volume id",
        declared_type: "AWS::EC2::Volume::Id",
        exists: |estate| estate.volume_id.clone(),
        missing: "vol-0aaaaaaaaaaaaaaaa",
    },
    TypedCase {
        case: "an image id",
        declared_type: "AWS::EC2::Image::Id",
        exists: |estate| estate.image_id.clone(),
        missing: "ami-0aaaaaaaaaaaaaaaa",
    },
    TypedCase {
        case: "a key pair name",
        declared_type: "AWS::EC2::KeyPair::KeyName",
        exists: |estate| estate.key_name.clone(),
        missing: "no-such-key-pair",
    },
    TypedCase {
        case: "an availability zone name",
        declared_type: "AWS::EC2::AvailabilityZone::Name",
        exists: |estate| estate.zone_name.clone(),
        missing: "us-east-1zz",
    },
    TypedCase {
        case: "a list of subnet ids",
        declared_type: "List<AWS::EC2::Subnet::Id>",
        exists: |estate| estate.subnet_id.clone(),
        // The list is supplied as the good value followed by this one, so what fails the
        // deployment is one entry of the array rather than the whole value.
        missing: "subnet-00bbbbbbbbbbbbbbb",
    },
    // The other side of the rule: a type that is NOT an AWS-specific one constrains the value
    // to nothing of the sort, so a value naming no resource deploys.
    TypedCase {
        case: "a plain string",
        declared_type: "String",
        exists: |_| "anything at all".to_string(),
        missing: "vpc-0aaaaaaaaaaaaaaaa",
    },
];

/// The body a row deploys: one parameter of the declared type, and one resource that does not
/// read it — so what decides the outcome is the parameter's own value and nothing else.
fn body(declared_type: &str) -> String {
    format!(
        r#"{{"Parameters":{{"Named":{{"Type":"{declared_type}"}}}},
            "Resources":{{"Group":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
              "GroupDescription":"the group of the stack"}}}}}},
            "Outputs":{{"GroupId":{{"Value":{{"Ref":"Group"}}}}}}}}"#
    )
}

/// Whether the type is a `List<…>` one, whose value is an array of values.
fn is_list(declared_type: &str) -> bool {
    declared_type.starts_with("List<")
}

#[tokio::test]
async fn a_value_naming_an_existing_resource_deploys() {
    let router = router();
    let estate = estate(&router).await;
    for (index, row) in TYPED_CASES.iter().enumerate() {
        let case = row.case;
        let value = (row.exists)(&estate);
        let stack = format!("typed-resolves-{index}");
        let (status, answered) = create_stack(
            &router,
            &stack,
            &body(row.declared_type),
            &[
                member("Parameters.member.1.ParameterKey", "Named"),
                member("Parameters.member.1.ParameterValue", &value),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} with {answered}"
        );
        let described = settled_stack(&router, case, &stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the value {value:?} to resolve and the create to \
             complete, got {described}"
        );
    }
}

#[tokio::test]
async fn a_value_naming_no_resource_rolls_the_stack_back() {
    let router = router();
    let estate = estate(&router).await;
    for (index, row) in TYPED_CASES.iter().enumerate() {
        let case = row.case;
        let stack = format!("typed-missing-{index}");
        // A list-typed parameter carries the array of both values, so what fails the
        // deployment is the entry that names nothing rather than the whole entry.
        let supplied = if is_list(row.declared_type) {
            format!("{}, {}", (row.exists)(&estate), row.missing)
        } else {
            row.missing.to_string()
        };
        let (status, answered) = create_stack(
            &router,
            &stack,
            &body(row.declared_type),
            &[
                member("Parameters.member.1.ParameterKey", "Named"),
                member("Parameters.member.1.ParameterValue", &supplied),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} with {answered}"
        );
        let described = settled_stack(&router, case, &stack).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        let reason = xml_text(&described, "StackStatusReason").unwrap_or_default();
        if row.declared_type == "String" {
            assert_eq!(
                reported, "CREATE_COMPLETE",
                "case {case:?}: a type that is not an AWS-specific one constrains the value \
                 to no resource, so expected CREATE_COMPLETE, got {reported:?}: {reason:?}"
            );
            continue;
        }
        assert_eq!(
            reported, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the value {:?} to fail the create and roll the stack \
             back, got {reported:?}: {reason:?}",
            row.missing
        );
        assert_eq!(
            reason,
            does_not_exist("Named", row.missing),
            "case {case:?}: expected the parameter-validation diagnosis, got {reason:?}"
        );
        // Nothing of the stack was created: the deployment was refused before any owning
        // service was asked, so the stack publishes no resource identifier at all.
        let (status, resources) = call(
            &router,
            "DescribeStackResources",
            &[member("StackName", &stack)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStackResources answered {status} with {resources}"
        );
        assert!(
            xml_texts(&resources, "PhysicalResourceId")
                .iter()
                .all(String::is_empty),
            "case {case:?}: expected no resource to have been created, got {resources}"
        );
    }
}
