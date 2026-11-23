// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for how this service reads a BOOLEAN request parameter the caller writes
//! INSIDE a structure or an element of a list, driven over the wire it speaks: `ec2Query`, an
//! `application/x-www-form-urlencoded` POST answered with XML.
//!
//! The rule under test: EC2 reads a boolean parameter's value without regard to its ASCII
//! case, and WHERE in the parameter list the value sits is no part of that — a flag on an
//! element of a list, on a nested structure, or several structures deep is read exactly as one
//! the request writes on its own ([`super::boolean_parameter_case`] pins that half). So such a
//! request is SERVED and the boolean carries the value it spells, rather than being refused as
//! a body the service could not read.
//!
//! Three things follow, and each is a test below:
//!
//! 1. every spelling of a boolean, at every depth this model writes one at, is read as the
//!    value it spells and reaches the state the call wrote;
//! 2. the same holds for an operation whose boolean is the VALUE IT WRITES rather than a
//!    parameter of its own — an attribute write — so the rule is not about one shape, and a
//!    value that spells NEITHER boolean leaves it unset rather than refusing the request;
//! 3. a member that merely CARRIES text reading like a boolean is untouched: a tag value
//!    spelled `True` round-trips exactly as the request sent it.
//!
//! Every row is a distinct input along what the rule generalises over — which position, which
//! spelling, which operation — so the request any one recording made is one row rather than
//! the specification.

use smithy_server_runtime::MultiServiceRouter;

use super::ec2_query::{call, element, emulator, filters};
use super::wire::{child_text, document, set_items};

/// The Region every call of this suite is signed for.
const REGION: &str = "us-west-2";

/// An AMI id of the right FORM for the launch parameters every row carries: a launch template
/// stores its parameters without resolving them, so which image it names is not the subject.
const SOME_AMI: &str = "ami-0123456789abcdef0";

/// A `(name, value)` form field.
fn p(name: &str, value: &str) -> (String, String) {
    (name.to_string(), value.to_string())
}

/// The spellings of each boolean a row is driven with, and the value each one carries: the
/// wire's own spelling of both values and three other ASCII cases of each.
const SPELLINGS: [(&str, bool); 8] = [
    ("true", true),
    ("True", true),
    ("TRUE", true),
    ("tRuE", true),
    ("false", false),
    ("False", false),
    ("FALSE", false),
    ("fAlSe", false),
];

/// The positions a boolean of a launch template's stored parameters is written at: the case,
/// the request key the flag is written under, and the element the version data reports it as.
///
/// They span the depths this model writes a boolean at: a member of the launch parameters
/// themselves, one of a nested structure, one of an element of a list, and one three
/// structures deep.
const POSITIONS: [(&str, &str, &str); 4] = [
    (
        "a flag on the launch parameters themselves",
        "LaunchTemplateData.EbsOptimized",
        "ebsOptimized",
    ),
    (
        "a flag on a nested structure",
        "LaunchTemplateData.Monitoring.Enabled",
        "enabled",
    ),
    (
        "a flag on an element of a list of structures",
        "LaunchTemplateData.NetworkInterface.1.AssociatePublicIpAddress",
        "associatePublicIpAddress",
    ),
    (
        "a flag three structures deep",
        "LaunchTemplateData.BlockDeviceMapping.1.Ebs.DeleteOnTermination",
        "deleteOnTermination",
    ),
];

/// The value `element_name` carries in the stored parameters of the launch template `name`,
/// read back through `DescribeLaunchTemplateVersions` — so the assertion is about what the
/// call WROTE rather than about what it echoed.
async fn stored_flag(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    element_name: &str,
) -> Option<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeLaunchTemplateVersions",
        &[p("LaunchTemplateName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the stored parameters must read back, got {status} with body {body}"
    );
    element(&body, element_name)
}

/// RULE 1: every ASCII case of each boolean, at every depth, is read as the value it spells
/// and is what the stored launch parameters carry — and so is a value that spells neither,
/// which leaves the boolean unset instead of refusing the request.
#[tokio::test]
async fn every_case_of_a_nested_boolean_is_read_as_the_value_it_spells() {
    let (router, _engine) = emulator();

    for (position, key, element_name) in POSITIONS {
        let spellings = SPELLINGS
            .into_iter()
            .chain([("yes", false), ("1", false), ("", false), ("ture", false)]);
        for (spelling, expected) in spellings {
            let case = format!("{position} written {spelling:?}");
            let name = format!("flag-{element_name}-{spelling}");
            let (status, body) = call(
                &router,
                REGION,
                "CreateLaunchTemplate",
                &[
                    p("LaunchTemplateName", &name),
                    p("LaunchTemplateData.ImageId", SOME_AMI),
                    p(key, spelling),
                ],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: a boolean spelled in another case is read, not refused, so \
                 CreateLaunchTemplate answers 200, got {status} with body {body}"
            );
            let stored = stored_flag(&router, &case, &name, element_name).await;
            assert_eq!(
                stored.as_deref(),
                Some(if expected { "true" } else { "false" }),
                "case {case:?}: expected <{element_name}> {expected}, got {stored:?}"
            );
        }
    }
}

/// RULE 2: the same reading on an operation whose nested boolean is the VALUE IT WRITES — a
/// subnet attribute — so the rule is about the TYPE of the parameter rather than about one
/// shape or one operation.
///
/// The rows are every spelling of each boolean, and then the values that spell NEITHER of them
/// — a word, a digit, an empty value — which leave the attribute unset and are SERVED rather
/// than refusing the body. The attribute is read back through `DescribeSubnets`, which is the
/// state the write changed.
#[tokio::test]
async fn every_case_of_an_attribute_value_is_read_as_the_value_it_spells() {
    let (router, _engine) = emulator();

    let spellings = SPELLINGS
        .into_iter()
        .chain([("yes", false), ("1", false), ("", false), ("ture", false)]);
    let subnet_id = a_subnet_of_the_region(&router).await;
    for (spelling, expected) in spellings {
        let case = format!("MapPublicIpOnLaunch.Value written {spelling:?}");
        let (status, body) = call(
            &router,
            REGION,
            "ModifySubnetAttribute",
            &[
                p("SubnetId", &subnet_id),
                p("MapPublicIpOnLaunch.Value", spelling),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from ModifySubnetAttribute, got {status} with \
             body {body}"
        );
        let reported =
            reported_subnet_flag(&router, &case, &subnet_id, "mapPublicIpOnLaunch").await;
        assert_eq!(
            reported.as_deref(),
            Some(if expected { "true" } else { "false" }),
            "case {case:?}: expected mapPublicIpOnLaunch {expected}, got {reported:?}"
        );
    }
}

/// RULE 3: only a BOOLEAN position is read without regard to case. A tag value that reads like
/// a boolean — a string nested in an element of a list of structures, the same position shape
/// as a nested flag — round-trips exactly as the request sent it.
#[tokio::test]
async fn a_nested_value_that_is_no_boolean_is_untouched() {
    let (router, _engine) = emulator();

    for value in ["True", "TRUE", "tRuE", "true"] {
        let case = format!("a tag value of {value:?}");
        let (status, body) = call(
            &router,
            REGION,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", &format!("tag-{value}")),
                p("LaunchTemplateData.ImageId", SOME_AMI),
                p("TagSpecification.1.ResourceType", "launch-template"),
                p("TagSpecification.1.Tag.1.Key", "reads-like-a-flag"),
                p("TagSpecification.1.Tag.1.Value", value),
            ],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateLaunchTemplate, got {status} with \
             body {body}"
        );
        let parsed = document(&case, &body);
        let template = parsed
            .descendants()
            .find(|node| node.is_element() && child_text(*node, "launchTemplateId").is_some())
            .unwrap_or_else(|| panic!("case {case:?}: no launch template in {body}"));
        let tags: Vec<(String, String)> = set_items(template, "tagSet")
            .into_iter()
            .map(|item| {
                (
                    child_text(item, "key").unwrap_or_default(),
                    child_text(item, "value").unwrap_or_default(),
                )
            })
            .collect();
        assert_eq!(
            tags,
            vec![("reads-like-a-flag".to_string(), value.to_string())],
            "case {case:?}: a tag value is no boolean, so it round-trips as sent, got {tags:?}"
        );
    }
}

/// One subnet of the Region, read off the account's own `DescribeSubnets`: the Region's
/// default network already holds one per zone, so nothing here creates a subnet of its own.
async fn a_subnet_of_the_region(router: &MultiServiceRouter) -> String {
    let (status, body) = call(router, REGION, "DescribeSubnets", &[]).await;
    assert_eq!(
        status, 200,
        "the Region's subnets must read back, got {status} with body {body}"
    );
    element(&body, "subnetId")
        .unwrap_or_else(|| panic!("the Region must hold a subnet to write an attribute of: {body}"))
}

/// The value the subnet `subnet_id` reports for `member`, narrowed to that one subnet.
async fn reported_subnet_flag(
    router: &MultiServiceRouter,
    case: &str,
    subnet_id: &str,
    member: &str,
) -> Option<String> {
    let (status, body) = call(
        router,
        REGION,
        "DescribeSubnets",
        &filters(&[("subnet-id", &[subnet_id])]),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the subnet must read back, got {status} with body {body}"
    );
    element(&body, member)
}
