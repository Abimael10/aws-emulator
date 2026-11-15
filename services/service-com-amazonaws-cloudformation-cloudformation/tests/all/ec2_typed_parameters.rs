// SPDX-License-Identifier: CC0-1.0

//! Where the value of an AWS-specifically typed parameter COMES FROM: the request's own value,
//! or the declaration's `Default`.
//!
//! RULE: the value held to a parameter's declaration is the one IN FORCE — "a value of the
//! appropriate type for the template to use if no value is specified" is the `Default`'s own
//! documentation — so a parameter declared with an AWS-specific type is resolved against the
//! account's resources whichever of the two carries its value: a stack operation that supplies
//! none is held to the template's own default exactly as one that supplies a value is held to
//! what it supplied.
//!
//! The rows differ along where the value comes from and whether it names a resource: a default
//! naming a resource the owning service holds deploys, and one naming none fails the create and
//! rolls the stack back with CloudFormation's own parameter-validation diagnosis. Which TYPES
//! are resolved, and against which service each is asked, is the same rule's other half, in
//! `ec2_typed_parameter_resolution.rs`.

use service_com_amazonaws_cloudformation_cloudformation::state::parameter_constraints::aws_specific_types::does_not_exist;

use crate::stack_lifecycle::{create_stack, ec2, router, settled_stack, xml_text};

/// The body a case deploys: one parameter declared with an AWS-specific type and a `Default`,
/// and one resource that does not read it — so what decides the outcome is the value in force
/// for that parameter and nothing else.
fn body(declared_type: &str, default: &str) -> String {
    format!(
        r#"{{"Parameters":{{"Named":{{"Type":"{declared_type}","Default":"{default}"}}}},
            "Resources":{{"Group":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
              "GroupDescription":"the group of the stack"}}}}}},
            "Outputs":{{"GroupId":{{"Value":{{"Ref":"Group"}}}}}}}}"#
    )
}

#[tokio::test]
async fn a_parameter_of_another_type_does_not_stop_the_resolution() {
    let router = router();
    // One parameter of a type that is resolved against nothing, declared BEFORE the
    // AWS-specifically typed one: the walk is over every declared parameter, so a type that
    // carries its own value does not leave the one after it unresolved.
    let template = r#"{"Parameters":{
            "Plain":{"Type":"String","Default":"anything at all"},
            "Named":{"Type":"AWS::EC2::VPC::Id","Default":"vpc-0dddddddddddddddd"}},
          "Resources":{"Group":{"Type":"AWS::EC2::SecurityGroup","Properties":{
            "GroupDescription":"the group of the stack"}}}}"#;
    let (status, answered) =
        create_stack(&router, "typed-after-a-plain-type", template, &[]).await;
    assert_eq!(
        status, 200,
        "CreateStack answered {status} with {answered}"
    );
    let case = "an AWS-specific type declared after a plain one";
    let described = settled_stack(&router, case, "typed-after-a-plain-type").await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("ROLLBACK_COMPLETE"),
        "case {case:?}: expected the stack to roll back, got {described}"
    );
    assert_eq!(
        xml_text(&described, "StackStatusReason").as_deref(),
        Some(does_not_exist("Named", "vpc-0dddddddddddddddd").as_str()),
        "case {case:?}: expected the parameter-validation diagnosis, got {described}"
    );
}

#[tokio::test]
async fn a_declared_default_is_resolved_like_a_supplied_value() {
    let router = router();
    let (status, created) = ec2(&router, "CreateVpc", &[("CidrBlock", "10.91.0.0/16")]).await;
    assert_eq!(
        status, 200,
        "CreateVpc must answer 200, got {status} {created}"
    );
    let vpc_id = xml_text(&created, "vpcId").expect("the VPC's own id");

    let rows: &[(&str, &str, &str, Option<&str>)] = &[
        (
            "a default naming a VPC of the account",
            "typed-default-resolves",
            vpc_id.as_str(),
            None,
        ),
        (
            "a default naming no VPC at all",
            "typed-default-missing",
            "vpc-0cccccccccccccccc",
            Some("vpc-0cccccccccccccccc"),
        ),
    ];
    for (case, stack, default, missing) in rows {
        let (status, answered) = create_stack(
            &router,
            stack,
            &body("AWS::EC2::VPC::Id", default),
            // No `Parameters` at all: the value in force is the declaration's own default.
            &[],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateStack answered {status} with {answered}"
        );
        let described = settled_stack(&router, case, stack).await;
        let reported = xml_text(&described, "StackStatus").unwrap_or_default();
        let reason = xml_text(&described, "StackStatusReason").unwrap_or_default();
        match missing {
            None => assert_eq!(
                reported, "CREATE_COMPLETE",
                "case {case:?}: expected the default {default:?} to resolve and the create to \
                 complete, got {reported:?}: {reason:?}"
            ),
            Some(value) => {
                assert_eq!(
                    reported, "ROLLBACK_COMPLETE",
                    "case {case:?}: expected the create to fail and the stack to roll back, \
                     got {reported:?}: {reason:?}"
                );
                assert_eq!(
                    reason,
                    does_not_exist("Named", value),
                    "case {case:?}: expected the parameter-validation diagnosis, got {reason:?}"
                );
            }
        }
    }
}
