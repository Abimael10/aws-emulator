// SPDX-License-Identifier: CC0-1.0

//! The AWS-SPECIFIC parameter types: a declared `Type` that makes the parameter's value the
//! identifier of an existing resource of the account, and the resolution a stack operation
//! performs on it.
//!
//! ## The rule this module implements
//!
//! A template parameter declared with one of the AWS-specific types does not take any string:
//! its value names a resource that has to BE THERE — "AWS-specific parameter types […] help
//! ensure that the values are valid", and CloudFormation resolves such a value against the
//! resources of the account and Region the operation runs in. A value that names none is not a
//! value of that parameter, so the deployment cannot be carried out: the operation it belongs
//! to FAILS, naming the value and the parameter it was given for ([`does_not_exist`]), and the
//! stack rolls back.
//!
//! The question is put to the service that OWNS the resource, over that service's own wire, so
//! a value resolves only against resources that service really holds — exactly as the resource
//! EXISTENCE check does for a name a deployment would create
//! ([`super::super::early_validation`]). A read this emulator does not serve, or one that
//! failed inside it, establishes nothing: no parameter is failed because a read could not be
//! made.
//!
//! The types and their reads ([`AWS_SPECIFIC_TYPES`]) are the published catalogue, and a
//! `List<…>` of one of them is "an array of […] values" of that type, so every value of the
//! array is resolved the same way. An `AWS::SSM::Parameter::Value<…>` of one of them is
//! resolved too, against the value Parameter Store holds: the inner type is what the resolved
//! value is read AS ([`super::super::ssm_parameters`]), so it is that value — not the
//! parameter-store name addressing it — that has to name an existing resource.
//!
//! ## Bounds
//!
//! One read per DISTINCT value the operation's AWS-specifically typed parameters name, at most
//! [`MAX_RESOLVED_VALUES`] of them, so the calls one request can ask for are bounded by a
//! constant however many parameters a template declares and however many values one
//! list-typed entry carries.

use ::std::collections::BTreeSet;

use ::serde_json::Value;

use super::super::provisioning::ec2_vpc_network::set_entry;
use super::super::provisioning::{call_query, call_rest_xml, xml_scalar_children};
use super::super::StoredParameter;
use super::super::{ssm_parameters, template};

/// Amazon EC2's signing name and the API version a request names beside the action, from its
/// model (`sigv4Name: ec2`, `version: 2016-11-15`) — the pair every in-process call to that
/// service is made with.
const EC2_SIGNING_NAME: &str = "ec2";
const EC2_API_VERSION: &str = "2016-11-15";

/// Amazon Route 53's signing name, from its model, and the prefix its model binds every
/// operation's URI under (`/2013-04-01`).
const ROUTE53_SIGNING_NAME: &str = "route53";
const ROUTE53_URI_PREFIX: &str = "/2013-04-01";

/// The suffixes of the error codes an owning service answers ABOUT THE VALUE a read named: it
/// holds no such resource (`InvalidVpcID.NotFound`, `InvalidGroup.NotFound`,
/// `InvalidAMIID.NotFound`, `InvalidKeyPair.NotFound`), or the value is not an identifier of
/// that kind at all (`InvalidVpcID.Malformed`, `InvalidVolumeID.Malformed`). Either way the
/// account holds no resource under that value, which is what this stage asks
/// ([`Resolution::Absent`]); judged on the CODE, never on a message, exactly as
/// [`super::super::provisioning::ec2_vpc_network`]'s own forgiving reads are. Every other
/// refusal — an operation this emulator does not serve, a call that failed inside it, one the
/// stack's service role is denied — establishes nothing ([`Resolution::Unknown`]).
const VALUE_NOT_FOUND_SUFFIX: &str = ".NotFound";
const VALUE_MALFORMED_SUFFIX: &str = ".Malformed";

/// Amazon Route 53's own code for a hosted zone it does not hold, and Amazon EC2's for a
/// value that names no resource of a kind it keeps no `Invalid…` code for ("the value supplied
/// for a parameter was invalid" — which is what `DescribeAvailabilityZones` answers for a zone
/// name the Region does not have). Both are refusals ABOUT THE VALUE, so both say the account
/// holds no resource under it.
const NO_SUCH_HOSTED_ZONE: &str = "NoSuchHostedZone";
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// How a value of one AWS-specific type is resolved: the read that says whether the account
/// holds the resource it names.
#[derive(Debug, Clone, Copy)]
enum Read {
    /// An Amazon EC2 describe: the action, the `ec2Query` parameter the value travels as — the
    /// member's own `xmlName`, numbered from one, which is how that protocol spells a list —
    /// the result set the answer reports the resources in (that member's `xmlName`), and the
    /// member every entry of that set carries the resource's own identifier under, which is
    /// what says an entry was reported at all.
    Ec2 {
        action: &'static str,
        parameter: &'static str,
        set: &'static str,
        reported: &'static str,
    },
    /// Amazon Route 53's own read of ONE hosted zone, which its model binds to
    /// `GET /hostedzone/{Id}`.
    HostedZone,
}

/// Every AWS-specific parameter type, against the read that resolves one of its values.
///
/// Source: the CloudFormation User Guide's "AWS-specific parameter types" list, each type
/// against the operation of the owning service that reports the resource it names
/// (`smithy_query(query="operation",operation="DescribeVpcs,DescribeSubnets,
/// DescribeSecurityGroups,DescribeVolumes,DescribeInstances,DescribeImages,DescribeKeyPairs,
/// DescribeAvailabilityZones")` for the Amazon EC2 ones, `GetHostedZone` for the Route 53 one).
/// A security group named by `GroupName` is read by that same describe's OTHER member, which is
/// the one that takes a name.
///
/// `AWS::SSM::Parameter::Name` is not among them: that type's value is a parameter KEY of
/// Parameter Store rather than an AWS resource identifier, and a `Ref` to it answers the name
/// as written.
const AWS_SPECIFIC_TYPES: &[(&str, Read)] = &[
    (
        "AWS::EC2::AvailabilityZone::Name",
        Read::Ec2 {
            action: "DescribeAvailabilityZones",
            parameter: "ZoneName.1",
            set: "availabilityZoneInfo",
            reported: "zoneName",
        },
    ),
    (
        "AWS::EC2::Image::Id",
        Read::Ec2 {
            action: "DescribeImages",
            parameter: "ImageId.1",
            set: "imagesSet",
            reported: "imageId",
        },
    ),
    (
        "AWS::EC2::Instance::Id",
        Read::Ec2 {
            action: "DescribeInstances",
            parameter: "InstanceId.1",
            set: "reservationSet",
            reported: "reservationId",
        },
    ),
    (
        "AWS::EC2::KeyPair::KeyName",
        Read::Ec2 {
            action: "DescribeKeyPairs",
            parameter: "KeyName.1",
            set: "keySet",
            reported: "keyName",
        },
    ),
    (
        "AWS::EC2::SecurityGroup::GroupName",
        Read::Ec2 {
            action: "DescribeSecurityGroups",
            parameter: "GroupName.1",
            set: "securityGroupInfo",
            reported: "groupId",
        },
    ),
    (
        "AWS::EC2::SecurityGroup::Id",
        Read::Ec2 {
            action: "DescribeSecurityGroups",
            parameter: "GroupId.1",
            set: "securityGroupInfo",
            reported: "groupId",
        },
    ),
    (
        "AWS::EC2::Subnet::Id",
        Read::Ec2 {
            action: "DescribeSubnets",
            parameter: "SubnetId.1",
            set: "subnetSet",
            reported: "subnetId",
        },
    ),
    (
        "AWS::EC2::Volume::Id",
        Read::Ec2 {
            action: "DescribeVolumes",
            parameter: "VolumeId.1",
            set: "volumeSet",
            reported: "volumeId",
        },
    ),
    (
        "AWS::EC2::VPC::Id",
        Read::Ec2 {
            action: "DescribeVpcs",
            parameter: "VpcId.1",
            set: "vpcSet",
            reported: "vpcId",
        },
    ),
    ("AWS::Route53::HostedZone::Id", Read::HostedZone),
];

/// How many values one stack operation resolves.
///
/// A list-typed parameter's value is an array the REQUEST sizes, and each value is one call to
/// another service, so the reads are held to a ceiling rather than to the request's own size.
/// Chosen: far more values than a template's AWS-specifically typed parameters are given in
/// practice, and a constant. A value past the ceiling is left to the resource that uses it
/// rather than resolved here.
pub const MAX_RESOLVED_VALUES: usize = 100;

/// The opening of a declared `Type` whose value is an ARRAY of values of the type inside the
/// brackets: "`List<AWS::EC2::VPC::Id>`: an array of VPC IDs".
const LIST_TYPE_OPEN: &str = "List<";

/// See [`LIST_TYPE_OPEN`].
const LIST_TYPE_CLOSE: &str = ">";

/// How CloudFormation words the failure of an operation whose AWS-specifically typed parameter
/// was given a value naming no resource of the account.
///
/// The fixed text is CloudFormation's own; the variable parts are the value the operation was
/// given and the parameter it was given for. The rollback clause the stack's own event carries
/// is appended where every other failure's is
/// ([`super::super::rollback_requested`]).
pub fn does_not_exist(key: &str, value: &str) -> String {
    format!(
        "Parameter validation failed: parameter value {value} for parameter name {key} does \
         not exist."
    )
}

/// `Some(reason)` when a value in force for an AWS-specifically typed parameter of
/// `template_body` names no resource the owning service holds — the failure the stack operation
/// that submitted it then carries; `None` when every such value resolves, and for a template
/// that declares no AWS-specifically typed parameter at all (which costs no call).
///
/// The parameters are walked in the order the template DECLARES them and each value is read
/// once, so one body and one set of values always answer the same sentence. The values are the
/// ones IN FORCE — what the request supplied, the declaration's own `Default`, or the value
/// Parameter Store holds for an SSM-typed parameter — so a declared default that names nothing
/// fails the deployment exactly as a supplied value does. Past [`MAX_RESOLVED_VALUES`] distinct
/// values the walk STOPS rather than reading more: the values beyond the ceiling are left to
/// the resources that use them, which fail on their own owning service's refusal.
///
/// No store lock is held while this runs: every call reaches the service that owns the
/// resource, exactly as the existence check does.
pub async fn deployment_failure(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    template_body: &str,
    parameters: &[StoredParameter],
) -> Option<String> {
    let declared = template::parse(template_body)?;
    let mut read: BTreeSet<String> = BTreeSet::new();
    for parameter in &declared.parameters {
        // A declaration stating no `Type` at all states no AWS-specific one either, and a
        // parameter of any other type carries its own value: neither is resolved, and neither
        // ends the walk over the parameters AFTER it.
        let Some(declared_type) = parameter.declared_type.as_deref() else {
            continue;
        };
        let Some(resolved) = resolved_by(declared_type) else {
            continue;
        };
        let in_force = parameters
            .iter()
            .find(|held| held.key == parameter.key)
            .map(|held| {
                // An SSM-typed parameter carries the NAME of a Parameter Store parameter and
                // the VALUE the store holds for it: the value is what the template's
                // expressions carry, so that is what has to name an existing resource.
                held.resolved_value
                    .clone()
                    .unwrap_or_else(|| held.value.clone())
            })
            .or_else(|| parameter.default.clone());
        // A declared parameter with no value in force at all is the parameter SET's own
        // refusal, which the submitting operation has already made: there is no value here to
        // resolve.
        let Some(in_force) = in_force else {
            continue;
        };
        for value in stated_values(declared_type, &in_force) {
            if value.is_empty() || !read.insert(value.to_string()) {
                continue;
            }
            if read.len() > MAX_RESOLVED_VALUES {
                return None;
            }
            if resolution(ctx, region, resolved, value).await == Resolution::Absent {
                return Some(does_not_exist(&parameter.key, value));
            }
        }
    }
    None
}

/// The read that resolves a value of `declared_type`, or `None` for a type that is not an
/// AWS-specific one — such a parameter takes any value its other constraints admit, and
/// nothing is read for it.
///
/// A `List<…>` of an AWS-specific type is an array of values OF that type, and an
/// `AWS::SSM::Parameter::Value<…>` of one resolves to a value of it, so both are read as the
/// type inside their brackets; the two compose, `List<…>` inside the SSM brackets included.
fn resolved_by(declared_type: &str) -> Option<Read> {
    let of = ssm_parameters::resolved_as(declared_type).unwrap_or(declared_type);
    let element = of
        .strip_prefix(LIST_TYPE_OPEN)
        .and_then(|inner| inner.strip_suffix(LIST_TYPE_CLOSE))
        .unwrap_or(of);
    AWS_SPECIFIC_TYPES
        .iter()
        .find(|(named, _)| *named == element)
        .map(|(_, read)| *read)
}

/// The values one entry puts in force: the whole text for a scalar parameter, and each
/// comma-separated entry for a list-typed one, whose value IS that array — read the one way
/// this crate reads a list-typed value ([`super::stated_values`] holds the same rule for the
/// constraints a declaration states).
///
/// At most [`MAX_RESOLVED_VALUES`] of them, clamped BEFORE the enumeration allocates: how many
/// values one entry carries is the REQUEST's to choose, so the work and the allocation this
/// reader can be asked for are bounded by the same constant that bounds the reads below it,
/// exactly as the sibling reader is bounded by its own.
fn stated_values<'a>(declared_type: &str, value: &'a str) -> Vec<&'a str> {
    let of = ssm_parameters::resolved_as(declared_type).unwrap_or(declared_type);
    if of.starts_with(LIST_TYPE_OPEN) {
        return value
            .split(',')
            .map(str::trim)
            .take(MAX_RESOLVED_VALUES)
            .collect();
    }
    vec![value.trim()]
}

/// What a read established about one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolution {
    /// The owning service reported a resource for it.
    Reported,
    /// It names no resource that service holds: the read answered with none, or that service
    /// refused the value outright.
    Absent,
    /// Nothing was established: the read is one this emulator does not serve, one that failed
    /// inside it, or a refusal that is not about the value. No parameter is failed on this.
    Unknown,
}

/// Resolve ONE value against the service that owns the resource it names.
///
/// A value is [`Resolution::Reported`] only on a resource that was really read back — the
/// identifier that service reports for the entry, read off the answer's own result set with
/// the reader every describe of a stack's resources is read with
/// ([`set_entry`]). A refusal ABOUT THE VALUE — that service holds no such resource, or the
/// value is no identifier of that kind at all — is that service saying the account has no
/// resource under it ([`VALUE_NOT_FOUND_SUFFIX`], [`VALUE_MALFORMED_SUFFIX`],
/// [`NO_SUCH_HOSTED_ZONE`], [`INVALID_PARAMETER_VALUE`]). Every OTHER answer establishes nothing: a read this emulator does
/// not serve, one that failed inside it, one the stack's own service role is denied, and any
/// refusal that is not about the value leave the parameter unresolved rather than failed.
async fn resolution(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    read: Read,
    value: &str,
) -> Resolution {
    let answered = match read {
        Read::Ec2 {
            action,
            parameter,
            set,
            reported,
        } => call_query(
            ctx,
            EC2_SIGNING_NAME,
            region,
            EC2_API_VERSION,
            action,
            &[(parameter.to_string(), value.to_string())],
        )
        .await
        .map(|document| set_entry(&document, set, reported).is_some()),
        Read::HostedZone => call_rest_xml(
            ctx,
            ROUTE53_SIGNING_NAME,
            region,
            "GET",
            &format!(
                "{ROUTE53_URI_PREFIX}/hostedzone/{}",
                super::super::provisioning::percent_encoded(value)
            ),
            None,
            &[],
            &[],
        )
        .await
        // "Gets information about a specified hosted zone": the zone travels as the answer's
        // own `<HostedZone>` structure, whose `Id` is the identifier Route 53 holds it under.
        .map(|(document, _headers)| {
            xml_scalar_children(&document, "HostedZone")
                .get("Id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
        }),
    };
    match answered {
        Ok(true) => Resolution::Reported,
        Ok(false) => Resolution::Absent,
        Err(refusal)
            if refusal.code.ends_with(VALUE_NOT_FOUND_SUFFIX)
                || refusal.code.ends_with(VALUE_MALFORMED_SUFFIX)
                || refusal.code == NO_SUCH_HOSTED_ZONE
                || refusal.code == INVALID_PARAMETER_VALUE =>
        {
            Resolution::Absent
        }
        Err(_) => Resolution::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a declaration's `Type` decides whether its value has to name an existing
    /// resource, and which service is asked — for the type itself, for a `List<…>` of it and
    /// for an `AWS::SSM::Parameter::Value<…>` that resolves to one.
    #[test]
    fn an_aws_specific_type_names_the_read_that_resolves_its_value() {
        let rows: &[(&str, &str, Option<&str>)] = &[
            ("a VPC id", "AWS::EC2::VPC::Id", Some("DescribeVpcs")),
            (
                "a list of subnet ids",
                "List<AWS::EC2::Subnet::Id>",
                Some("DescribeSubnets"),
            ),
            (
                "a security group id",
                "AWS::EC2::SecurityGroup::Id",
                Some("DescribeSecurityGroups"),
            ),
            (
                "a security group name",
                "AWS::EC2::SecurityGroup::GroupName",
                Some("DescribeSecurityGroups"),
            ),
            (
                "a key pair name",
                "AWS::EC2::KeyPair::KeyName",
                Some("DescribeKeyPairs"),
            ),
            (
                "an image id read out of Parameter Store",
                "AWS::SSM::Parameter::Value<AWS::EC2::Image::Id>",
                Some("DescribeImages"),
            ),
            (
                "a list of volume ids read out of Parameter Store",
                "AWS::SSM::Parameter::Value<List<AWS::EC2::Volume::Id>>",
                Some("DescribeVolumes"),
            ),
            (
                "an availability zone name",
                "AWS::EC2::AvailabilityZone::Name",
                Some("DescribeAvailabilityZones"),
            ),
            (
                "an instance id",
                "AWS::EC2::Instance::Id",
                Some("DescribeInstances"),
            ),
            // The one AWS-specific type whose resource another service owns, read over that
            // service's own wire rather than Amazon EC2's.
            (
                "a hosted zone id",
                "AWS::Route53::HostedZone::Id",
                Some("GetHostedZone"),
            ),
            ("a plain string", "String", None),
            ("a number", "Number", None),
            ("a comma-delimited list", "CommaDelimitedList", None),
            ("a list of strings", "List<String>", None),
            // A parameter-store KEY is not a resource identifier: nothing is read for it.
            ("a parameter store name", "AWS::SSM::Parameter::Name", None),
            (
                "a string read out of Parameter Store",
                "AWS::SSM::Parameter::Value<String>",
                None,
            ),
        ];
        for (case, declared_type, expected) in rows {
            let action = resolved_by(declared_type).map(|read| match read {
                Read::Ec2 { action, .. } => action,
                Read::HostedZone => "GetHostedZone",
            });
            assert_eq!(
                action, *expected,
                "case {case:?}: expected {expected:?}, got {action:?}"
            );
        }
    }

    /// RULE: a list-typed value is an ARRAY of values, every one of which is resolved; a
    /// scalar one is the single value it is.
    #[test]
    fn every_value_of_a_list_typed_entry_is_resolved() {
        let rows: &[(&str, &str, &str, Vec<&str>)] = &[
            (
                "a scalar id",
                "AWS::EC2::VPC::Id",
                "vpc-0123456789abcdef0",
                vec!["vpc-0123456789abcdef0"],
            ),
            (
                "two ids in one entry",
                "List<AWS::EC2::Subnet::Id>",
                "subnet-0123456789abcdef0, subnet-00112233445566778",
                vec!["subnet-0123456789abcdef0", "subnet-00112233445566778"],
            ),
            (
                "a list read out of Parameter Store",
                "AWS::SSM::Parameter::Value<List<AWS::EC2::Volume::Id>>",
                "vol-0123456789abcdef0,vol-00112233445566778",
                vec!["vol-0123456789abcdef0", "vol-00112233445566778"],
            ),
        ];
        for (case, declared_type, value, expected) in rows {
            let values = stated_values(declared_type, value);
            assert_eq!(
                values, *expected,
                "case {case:?}: expected {expected:?}, got {values:?}"
            );
        }
    }

    /// RULE: the failure names the value the operation was given and the parameter it was
    /// given for, in the words CloudFormation words it in.
    #[test]
    fn the_failure_names_the_value_and_the_parameter() {
        let reason = does_not_exist("VpcId", "vpc-0123456789abcdef0");
        let expected = "Parameter validation failed: parameter value vpc-0123456789abcdef0 \
                        for parameter name VpcId does not exist.";
        assert_eq!(
            reason, expected,
            "case \"a VPC id that names nothing\": expected {expected:?}, got {reason:?}"
        );
    }

    /// RULE: a value is resolved only on a resource the owning service really reported, so an
    /// answer that reports an empty result set — or one whose set carries no identifier for an
    /// entry — is a value naming nothing.
    #[test]
    fn only_a_reported_resource_resolves_a_value() {
        let rows: &[(&str, &str, &str, &str, bool)] = &[
            (
                "a VPC the service reported",
                "<DescribeVpcsResponse><vpcSet><item><vpcId>vpc-0123456789abcdef0</vpcId>\
                 </item></vpcSet></DescribeVpcsResponse>",
                "vpcSet",
                "vpcId",
                true,
            ),
            (
                "an empty result set",
                "<DescribeVpcsResponse><vpcSet/></DescribeVpcsResponse>",
                "vpcSet",
                "vpcId",
                false,
            ),
            (
                "an answer carrying no such set",
                "<DescribeVpcsResponse><nextToken/></DescribeVpcsResponse>",
                "vpcSet",
                "vpcId",
                false,
            ),
            // The id is read off the SET's own first entry, so an element of that name
            // somewhere else in the answer is not a resource this read reported.
            (
                "an id outside the result set",
                "<DescribeVpcsResponse><requestedVpc><vpcId>vpc-0123456789abcdef0</vpcId>\
                 </requestedVpc><vpcSet/></DescribeVpcsResponse>",
                "vpcSet",
                "vpcId",
                false,
            ),
        ];
        for (case, document, set, reported, expected) in rows {
            let resolved = set_entry(document, set, reported).is_some();
            assert_eq!(
                resolved, *expected,
                "case {case:?}: expected {expected:?}, got {resolved:?}"
            );
        }
    }
}
