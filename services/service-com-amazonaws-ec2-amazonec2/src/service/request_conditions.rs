// SPDX-License-Identifier: CC0-1.0

//! The condition keys an EC2 request's OWN parameters answer, read ahead of the
//! authorization decision.
//!
//! IAM decides a statement's `Condition` against the keys the request carries a value for,
//! and some of an EC2 request's keys are properties of the request itself rather than of the
//! caller: the instance type a launch asks for, say, which is what a policy that admits only
//! some instance types is written over. They are read here, once, out of the request's own
//! parameter list ([`super::request_parameters`]), so the gate decides such a clause instead
//! of leaving it undecided — and so the encoded authorization failure message a refusal
//! hands the caller reports "the values of condition keys in the context of the user's
//! request" (`sts:DecodeAuthorizationMessage`, the STS model) as the values this request
//! really carried.
//!
//! Only keys whose value IS a request parameter live here. A key about the caller, the
//! credential or the endpoint is not a property of the parameter list and is not read from
//! it.
//!
//! Some of those keys are typed ARN rather than string ("Condition keys for Amazon EC2"
//! gives `ec2:Subnet` and `ec2:Vpc` the type ARN), and an EC2 request names such a resource
//! by ID: their value is therefore the ARN of the resource the id names
//! ([`ARN_CONDITION_KEY_BY_RESOURCE_TYPE`]), built the one way this crate builds the ARN of
//! an addressed resource ([`super::addressed_resources`]) so a clause naming the subnet a
//! launch asks for is decided against the ARN the account really holds. Which parameter
//! carried the id does not matter — a launch spells its subnet on itself or on one of its
//! network interfaces, and both name the same subnet — so these are keyed by the resource
//! TYPE the id's own prefix fixes.
//!
//! An action that supports no resource-level permissions carries none of those keys: "the
//! Amazon EC2 API actions that begin with `Describe` do not support resource-level
//! permissions" (Amazon EC2 User Guide), the rule [`super::authorization`] takes the
//! decision for such an action under, so a resource the request merely NAMES decides nothing
//! about it and a clause over one of these keys stays undecided, granting nothing.

use std::collections::BTreeSet;

use ::aws_smithy_runtime_api::http::Request as RtRequest;

use super::addressed_resources::resource_type_of;
use super::request_parameters::{request_parameters, MAX_PARAMETERS_READ};

/// The EC2 condition key each request parameter answers, as the IAM Service Authorization
/// Reference names it ("Condition keys for Amazon EC2") beside the parameter an ec2Query
/// request carries the value under.
///
/// Compared without regard to ASCII case, the way the ec2Query key of a member is: that key
/// capitalises the first letter of either the member's name or its `@xmlName`.
pub const CONDITION_KEY_BY_PARAMETER: &[(&str, &str)] = &[
    // `ec2:InstanceType` — "the type of instance" a request asks for.
    ("InstanceType", "ec2:InstanceType"),
    // `ec2:AvailabilityZone` — the Availability Zone a launch's placement names.
    ("Placement.AvailabilityZone", "ec2:AvailabilityZone"),
    // `ec2:Tenancy` — "the tenancy of the VPC or instance" that placement asks for.
    ("Placement.Tenancy", "ec2:Tenancy"),
];

/// The EC2 condition key each resource a request NAMES BY ID answers with that resource's
/// ARN, keyed by the resource type the id's prefix fixes
/// ([`super::addressed_resources::RESOURCE_TYPE_BY_ID_PREFIX`]).
///
/// Both are typed ARN by the IAM Service Authorization Reference ("Condition keys for Amazon
/// EC2"): `ec2:Subnet` is "the ARN of the subnet" a request names — the key a policy that
/// admits a launch only into one subnet is written over — and `ec2:Vpc` "the ARN of the VPC".
pub const ARN_CONDITION_KEY_BY_RESOURCE_TYPE: &[(&str, &str)] =
    &[("subnet", "ec2:Subnet"), ("vpc", "ec2:Vpc")];

/// The condition keys `request` carries a value for, as `(key, value)` pairs in the order
/// its parameters name them.
///
/// `operation` is the operation the request named, which decides whether the keys about the
/// resources it names are part of its context at all, and `region` is the Region it was
/// signed for, which is the Region those resources are in — the two the ARN of a named
/// resource is built from.
///
/// A key a request names SEVERAL times is passed as one pair per value, which is how the
/// shared evaluator reads a multi-valued key
/// (`::service_common::iam_authorization::Request::context`). A value a request repeats —
/// the one subnet a launch spells on itself and on its primary interface — is ONE value of
/// the key, not two, so it is named once: the same reading that names an addressed resource
/// once however often the request carries its id.
#[must_use]
pub fn request_conditions(
    request: &RtRequest,
    operation: &str,
    region: &str,
) -> Vec<(String, String)> {
    // Whether the keys about the resources the request names belong to its context: an
    // action that supports no resource-level permissions is decided over every resource, so
    // none of them do (see the module docs).
    let names_resources = !super::authorization::judged_over_every_resource(operation);
    let mut context: Vec<(String, String)> = Vec::new();
    // The ARN-typed keys already answered, so a resource a request names twice is named once
    // without the reading growing with the SQUARE of the ids one request can carry.
    let mut answered = BTreeSet::new();
    for (name, value) in request_parameters(request, MAX_PARAMETERS_READ) {
        if let Some((_, key)) = CONDITION_KEY_BY_PARAMETER
            .iter()
            .find(|(parameter, _)| parameter.eq_ignore_ascii_case(&name))
        {
            context.push(((*key).to_string(), value.into_owned()));
            continue;
        }
        if !names_resources {
            continue;
        }
        let Some(resource_type) = resource_type_of(&value) else {
            continue;
        };
        let Some((_, key)) = ARN_CONDITION_KEY_BY_RESOURCE_TYPE
            .iter()
            .find(|(named, _)| *named == resource_type)
        else {
            continue;
        };
        let pair = (
            (*key).to_string(),
            ::engine::emulator_arn(
                super::authorization::ACTION_PREFIX,
                region,
                &format!("{resource_type}/{value}"),
            ),
        );
        if answered.insert(pair.clone()) {
            context.push(pair);
        }
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A request carrying `parameters` as the `application/x-www-form-urlencoded` body an
    /// ec2Query client posts.
    fn posted(parameters: &[(&str, &str)]) -> RtRequest {
        let body: String = ::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(parameters.iter().copied())
            .finish();
        let mut request = RtRequest::new(::aws_smithy_types::body::SdkBody::from(body));
        request
            .set_uri("https://ec2.us-west-2.amazonaws.com/")
            .expect("a well-formed URI");
        request
    }

    /// One row of the table below: the case, the operation the request named, the
    /// parameters posted, and the condition keys they answer.
    type ConditionRow<'a> = (
        &'static str,
        &'static str,
        &'static [(&'static str, &'static str)],
        Vec<(&'a str, String)>,
    );

    /// The Region every row below is signed for, and so the Region the resources its
    /// parameters name are in.
    const REGION: &str = "us-west-2";

    /// RULE: a request answers the EC2 condition key each of its own parameters carries the
    /// value of, in the order the parameters name them — a key typed ARN with the ARN of the
    /// resource the parameter's id names — and answers nothing for a key no parameter
    /// carries.
    ///
    /// The rows span what the rule generalises over: which key the request carries (an
    /// instance type, a placement's Availability Zone, a placement's tenancy, the subnet a
    /// launch names, the subnet one of its interfaces names, the VPC a create names, several
    /// at once, none), the parameter's own casing, a parameter naming one key twice, the same
    /// resource named twice, and an action that supports no resource-level permissions.
    #[test]
    fn a_request_answers_the_keys_its_own_parameters_carry() {
        let subnet = "subnet-0123456789abcdef0";
        let other_subnet = "subnet-1a2b3c4d";
        let vpc = "vpc-0123456789abcdef0";
        let subnet_arn = ::engine::emulator_arn("ec2", REGION, &format!("subnet/{subnet}"));
        let other_subnet_arn =
            ::engine::emulator_arn("ec2", REGION, &format!("subnet/{other_subnet}"));
        let vpc_arn = ::engine::emulator_arn("ec2", REGION, &format!("vpc/{vpc}"));
        // (case, the operation named, the parameters posted, the keys they answer)
        let rows: Vec<ConditionRow<'_>> = vec![
            (
                "a launch naming an instance type",
                "RunInstances",
                &[
                    ("Action", "RunInstances"),
                    ("ImageId", "ami-0123456789abcdef0"),
                    ("InstanceType", "t3.micro"),
                ],
                vec![("ec2:InstanceType", "t3.micro".to_string())],
            ),
            (
                "a placement's Availability Zone and tenancy",
                "RunInstances",
                &[
                    ("Action", "RunInstances"),
                    ("Placement.AvailabilityZone", "us-west-2b"),
                    ("Placement.Tenancy", "dedicated"),
                ],
                vec![
                    ("ec2:AvailabilityZone", "us-west-2b".to_string()),
                    ("ec2:Tenancy", "dedicated".to_string()),
                ],
            ),
            (
                "the parameter in another ASCII case",
                "RunInstances",
                &[("action", "RunInstances"), ("instancetype", "m5.large")],
                vec![("ec2:InstanceType", "m5.large".to_string())],
            ),
            (
                "one key named twice, which travels as one pair per value",
                "RunInstances",
                &[
                    ("InstanceType", "t3.micro"),
                    ("InstanceType", "t3.small"),
                ],
                vec![
                    ("ec2:InstanceType", "t3.micro".to_string()),
                    ("ec2:InstanceType", "t3.small".to_string()),
                ],
            ),
            (
                "a launch naming the subnet it lands in, beside its type",
                "RunInstances",
                &[
                    ("Action", "RunInstances"),
                    ("ImageId", "ami-0123456789abcdef0"),
                    ("SubnetId", "subnet-0123456789abcdef0"),
                    ("InstanceType", "t3.micro"),
                ],
                vec![
                    ("ec2:Subnet", subnet_arn.clone()),
                    ("ec2:InstanceType", "t3.micro".to_string()),
                ],
            ),
            (
                // Whichever member carried the id, it is the same subnet the launch names.
                "a launch naming one subnet on itself and the same one on its interface",
                "RunInstances",
                &[
                    ("SubnetId", "subnet-0123456789abcdef0"),
                    ("NetworkInterface.1.SubnetId", "subnet-0123456789abcdef0"),
                ],
                vec![("ec2:Subnet", subnet_arn.clone())],
            ),
            (
                "a launch whose interfaces name two different subnets",
                "RunInstances",
                &[
                    ("NetworkInterface.1.SubnetId", "subnet-0123456789abcdef0"),
                    ("NetworkInterface.2.SubnetId", "subnet-1a2b3c4d"),
                ],
                vec![
                    ("ec2:Subnet", subnet_arn.clone()),
                    ("ec2:Subnet", other_subnet_arn),
                ],
            ),
            (
                "a create naming the VPC it goes in",
                "CreateSecurityGroup",
                &[
                    ("Action", "CreateSecurityGroup"),
                    ("GroupName", "a-group"),
                    ("VpcId", "vpc-0123456789abcdef0"),
                ],
                vec![("ec2:Vpc", vpc_arn)],
            ),
            (
                // An id of a kind no ARN-typed key is published for answers nothing.
                "a request naming a security group",
                "AuthorizeSecurityGroupIngress",
                &[("GroupId", "sg-0123456789abcdef0")],
                Vec::new(),
            ),
            (
                // "The Amazon EC2 API actions that begin with Describe do not support
                // resource-level permissions", so a resource such a request names decides
                // nothing about it.
                "a read naming a subnet and a VPC of the account",
                "DescribeSubnets",
                &[
                    ("SubnetId.1", "subnet-0123456789abcdef0"),
                    ("Filter.1.Name", "vpc-id"),
                    ("Filter.1.Value.1", "vpc-0123456789abcdef0"),
                ],
                Vec::new(),
            ),
            (
                "a request carrying none of them",
                "DescribeInstances",
                &[("Action", "DescribeInstances"), ("MaxResults", "5")],
                Vec::new(),
            ),
        ];
        for (case, operation, parameters, expected) in &rows {
            let carried = request_conditions(&posted(parameters), operation, REGION);
            let expected: Vec<(String, String)> = expected
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect();
            assert_eq!(
                carried, expected,
                "case {case:?}: expected {expected:?}, got {carried:?}"
            );
        }
    }
}
