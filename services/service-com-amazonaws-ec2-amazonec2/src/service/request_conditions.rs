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

use ::aws_smithy_runtime_api::http::Request as RtRequest;

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

/// The condition keys `request` carries a value for, as `(key, value)` pairs in the order
/// its parameters name them.
///
/// A key a request names SEVERAL times is passed as one pair per value, which is how the
/// shared evaluator reads a multi-valued key
/// (`::service_common::iam_authorization::Request::context`).
#[must_use]
pub fn request_conditions(request: &RtRequest) -> Vec<(String, String)> {
    let mut context = Vec::new();
    for (name, value) in request_parameters(request, MAX_PARAMETERS_READ) {
        let Some((_, key)) = CONDITION_KEY_BY_PARAMETER
            .iter()
            .find(|(parameter, _)| parameter.eq_ignore_ascii_case(&name))
        else {
            continue;
        };
        context.push(((*key).to_string(), value.into_owned()));
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

    /// One row of the table below: the case, the parameters posted, and the condition keys
    /// they answer.
    type ConditionRow = (
        &'static str,
        &'static [(&'static str, &'static str)],
        &'static [(&'static str, &'static str)],
    );

    /// RULE: a request answers the EC2 condition key each of its own parameters carries the
    /// value of, in the order the parameters name them, and answers nothing for a key no
    /// parameter carries.
    ///
    /// The rows span what the rule generalises over: which key the request carries (an
    /// instance type, a placement's Availability Zone, a placement's tenancy, several at
    /// once, none), the parameter's own casing, and a parameter naming one key twice.
    #[test]
    fn a_request_answers_the_keys_its_own_parameters_carry() {
        // (case, the parameters posted, the keys they answer)
        let rows: &[ConditionRow] = &[
            (
                "a launch naming an instance type",
                &[
                    ("Action", "RunInstances"),
                    ("ImageId", "ami-0123456789abcdef0"),
                    ("InstanceType", "t3.micro"),
                ],
                &[("ec2:InstanceType", "t3.micro")],
            ),
            (
                "a placement's Availability Zone and tenancy",
                &[
                    ("Action", "RunInstances"),
                    ("Placement.AvailabilityZone", "us-west-2b"),
                    ("Placement.Tenancy", "dedicated"),
                ],
                &[
                    ("ec2:AvailabilityZone", "us-west-2b"),
                    ("ec2:Tenancy", "dedicated"),
                ],
            ),
            (
                "the parameter in another ASCII case",
                &[("action", "RunInstances"), ("instancetype", "m5.large")],
                &[("ec2:InstanceType", "m5.large")],
            ),
            (
                "one key named twice, which travels as one pair per value",
                &[
                    ("InstanceType", "t3.micro"),
                    ("InstanceType", "t3.small"),
                ],
                &[
                    ("ec2:InstanceType", "t3.micro"),
                    ("ec2:InstanceType", "t3.small"),
                ],
            ),
            (
                "a request carrying none of them",
                &[("Action", "DescribeInstances"), ("MaxResults", "5")],
                &[],
            ),
        ];
        for (case, parameters, expected) in rows {
            let carried = request_conditions(&posted(parameters));
            let expected: Vec<(String, String)> = expected
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect();
            assert_eq!(
                carried, expected,
                "case {case:?}: expected {expected:?}, got {carried:?}"
            );
        }
    }
}
