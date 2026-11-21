// SPDX-License-Identifier: CC0-1.0

//! The FORM a caller-supplied identifier carries, read BEFORE anything is looked up.
//!
//! Two kinds of identifier reach this API from a caller: the id of a target
//! ([`check_target_id`]) and the ARN of a resource this API addresses
//! ([`check_resource_arn`], [`check_arn_format`]). Both are judged on
//! the value alone, so the judgement is the same whatever the store holds — and it is made
//! ahead of the store, because the two answers are not interchangeable to a caller: a
//! value that is not an identifier this API could ever address is a bad REQUEST
//! (`ValidationError`, permanent, caller-side), while a well-formed identifier nothing
//! holds is the operation's modelled `NotFound` (a resource that may yet appear, or that
//! was deleted). An API that answered the second for the first would tell a client to go
//! looking for a missing resource when what it has to fix is the argument it sent.
//!
//! An ARN this API could have minted names its own vendor
//! ([`super::SERVICE_NAMESPACE`]), the Region the request addresses — a regional endpoint
//! addresses the resources of its own Region, so an ARN of another one names nothing it
//! can reach — and a resource field of the form the create handlers in `src/ops/` mint for
//! that kind. A value breaking any of those is refused on the request.
//!
//! The two readers differ only in the WORDS the refusal carries, which Elastic Load
//! Balancing keys on the OPERATION rather than on what was wrong with the value. Most of
//! them name the kind of ARN the member takes ([`invalid_arn_message`] —
//! `DescribeLoadBalancers`, `DescribeTargetGroups`, `DescribeTargetHealth`), which is
//! therefore the reading every member takes unless its own refusal is recorded otherwise;
//! the two attribute reads state the FORM the value failed to take
//! ([`arn_format_message`] — `DescribeLoadBalancerAttributes`,
//! `DescribeTargetGroupAttributes`). Both are recorded behaviour: the model declares no
//! error shape for either, so the code is this service's own `ValidationError`
//! (`crate::scaffolded::Scaffolded::validation_error`).

use super::*;

/// The prefix every EC2 instance id carries, which is what a target of an `instance`
/// target group is written as.
const INSTANCE_ID_PREFIX: &str = "i-";

/// The lengths an EC2 instance id's hexadecimal part has: AWS writes an instance id as
/// `i-` and either 8 or 17 hexadecimal characters, and a value of any other length is no
/// instance id at all.
const INSTANCE_ID_DIGITS: [usize; 2] = [8, 17];

/// A Lambda function's ARN, which is what a target of a `lambda` target group is written
/// as (`TargetDescription$Id`: "if the target type is lambda, specify the ARN of the
/// Lambda function").
const LAMBDA_FUNCTION: ::service_common::arn::Requirement = ::service_common::arn::Requirement {
    vendor: "lambda",
    resource_type: Some("function"),
    label: "a Lambda function ARN",
};

/// An Application Load Balancer's ARN, which is what a target of an `alb` target group is
/// written as (`TargetDescription$Id`: "if the target type is alb, specify the ARN of the
/// Application Load Balancer target").
const LOAD_BALANCER_TARGET: ::service_common::arn::Requirement =
    ::service_common::arn::Requirement {
        vendor: SERVICE_NAMESPACE,
        resource_type: Some("loadbalancer"),
        label: "an Application Load Balancer ARN",
    };

/// Whether `id` is written the way a target of a `target_type` target group is written,
/// and — when it is not — the rule it broke.
///
/// What a target id NAMES depends on the group's target type, and `TargetDescription$Id`
/// states the form for each: "if the target type of the target group is instance, specify
/// an instance ID. If the target type is ip, specify an IP address. If the target type is
/// lambda, specify the ARN of the Lambda function. If the target type is alb, specify the
/// ARN of the Application Load Balancer target." A value of none of those forms names no
/// target of ANY account — it is the request that has to be fixed rather than a target the
/// service could not find — so the operation that carries it refuses with this service's
/// `ValidationError`, before anything is resolved against the service that owns the
/// target.
///
/// Each class is worded the way Elastic Load Balancing words it: a verdict on the INSTANCE
/// ID for an `instance` group ([`invalid_instance_id_message`]) and the fixed sentence
/// [`INVALID_LAMBDA_FUNCTION_ARN_MESSAGE`] for a `lambda` one, both recorded behaviour.
///
/// An `ip` target's form is read by [`network::parse_ip_target`], which answers the
/// address itself, so it is not restated here; the model puts no form on an id this
/// service does not resolve at all.
pub fn check_target_id(target_type: &str, id: &str) -> Result<(), String> {
    match target_type {
        "instance" => {
            let digits = id.strip_prefix(INSTANCE_ID_PREFIX).unwrap_or_default();
            if !INSTANCE_ID_DIGITS.contains(&digits.len())
                || !digits
                    .chars()
                    .all(|digit| digit.is_ascii_digit() || ('a'..='f').contains(&digit))
            {
                return Err(invalid_instance_id_message(id));
            }
            Ok(())
        }
        "lambda" if !LAMBDA_FUNCTION.admits(id) => {
            Err(INVALID_LAMBDA_FUNCTION_ARN_MESSAGE.to_string())
        }
        "alb" if !LOAD_BALANCER_TARGET.admits(id) => Err(format!(
            "'{id}' in Targets is not {}",
            LOAD_BALANCER_TARGET.label
        )),
        _ => Ok(()),
    }
}

/// The sentence a target of a `lambda` target group that is no Lambda function ARN is
/// refused with.
///
/// Recorded behaviour, and ENTIRELY fixed text: Elastic Load Balancing states the form it
/// expected and quotes nothing of the request, so the value the caller sent appears
/// nowhere in it.
pub const INVALID_LAMBDA_FUNCTION_ARN_MESSAGE: &str = "Invalid Lambda function ARN";

/// The `TargetType` whose targets are named by an ADDRESS rather than by the id or ARN of
/// a resource, which is the one form this module reads through the parse that answers the
/// address itself ([`network::parse_ip_target`]).
const IP_TARGET_TYPE: &str = "ip";

/// Whether every target of a request is written the way a target of a `target_type` target
/// group is written, answering the ADDRESS each `ip` target names.
///
/// One reading of that rule for every operation that takes a `Targets` list: the
/// registration that adds them and the deregistration that removes them are judged alike,
/// because the form of an id does not depend on what is being asked of it — a value that is
/// no target of this kind of group names nothing to remove any more than it names something
/// to add, so it is refused rather than treated as the no-op a well-formed target that was
/// never registered earns.
///
/// The whole list is judged before anything is done with any of it, so a refusal leaves the
/// target group exactly as it was.
pub fn check_target_forms(
    target_type: &str,
    ip_address_type: Option<&str>,
    targets: &[crate::ops::types::TargetDescription],
) -> Result<Vec<std::net::IpAddr>, String> {
    let mut addresses = Vec::with_capacity(targets.len());
    for target in targets {
        check_target_id(target_type, &target.id)?;
        if target_type == IP_TARGET_TYPE {
            addresses.push(network::parse_ip_target(&target.id, ip_address_type)?);
        }
    }
    Ok(addresses)
}

/// The message a registration naming a value that is no INSTANCE ID is refused with: a
/// verdict on the instance id the request carried, which is the one thing the caller can
/// act on.
///
/// Elastic Load Balancing states this one by the kind of identifier it expected rather
/// than by the member that carried it, and states nothing about the target group, the
/// account or whether any instance with that id exists — the value is judged before
/// anything is asked of the service that owns the instance.
pub fn invalid_instance_id_message(id: &str) -> String {
    format!("Instance ID '{id}' is not valid")
}

/// The message a registration naming instance ids EC2 holds no instance for is refused
/// with: the ids it could not resolve, each quoted, in the order the request named them.
///
/// Elastic Load Balancing words this refusal about the TARGETS of the request — the ids are
/// what the caller corrects — and states nothing about the target group or the account.
pub fn invalid_instance_targets_message<'a>(ids: impl IntoIterator<Item = &'a str>) -> String {
    let quoted: Vec<String> = ids.into_iter().map(|id| format!("'{id}'")).collect();
    format!(
        "The following targets are not valid instances: {}",
        quoted.join(", ")
    )
}

/// The message a registration of targets that live OUTSIDE the target group's own VPC is
/// refused with: the VPC the group names, and the targets of the request that are not in it,
/// each quoted, in the order the request named them.
///
/// Elastic Load Balancing words this refusal about the TARGETS the caller has to correct and
/// about the ONE network the group is in — the group's own `VpcId`, read from the store — and
/// states nothing about where each target does live: the VPC a target of another network sits
/// in is not part of the verdict, which is that the group's VPC does not hold it. The sibling
/// of [`invalid_instance_targets_message`], and worded the same way: the offending ids as a
/// list, however many of them one request carries.
pub fn targets_not_in_target_group_vpc_message<'a>(
    vpc_id: &str,
    ids: impl IntoIterator<Item = &'a str>,
) -> String {
    let quoted: Vec<String> = ids.into_iter().map(|id| format!("'{id}'")).collect();
    format!(
        "The following targets are not in the target group VPC '{vpc_id}': {}",
        quoted.join(", ")
    )
}

/// Which resource of this service a caller-supplied ARN member names.
///
/// One variant per kind whose ARN a caller hands this API as the resource an operation
/// acts on or selects by. The kind decides the resource field the ARN must carry
/// ([`ArnKind::addresses`]) and the noun a refusal names ([`ArnKind::noun`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArnKind {
    /// `loadbalancer/<type-code>/<name>/<id>`, as `load_balancer_type_code` mints it.
    LoadBalancer,
    /// `targetgroup/<name>/<id>`, as `CreateTargetGroup` mints it.
    TargetGroup,
}

impl ArnKind {
    /// The noun Elastic Load Balancing names this kind with in a refusal.
    ///
    /// The prose spelling, not the ARN segment: a refusal is read by a person fixing a
    /// request.
    pub fn noun(self) -> &'static str {
        match self {
            Self::LoadBalancer => "load balancer",
            Self::TargetGroup => "target group",
        }
    }

    /// Whether `resource` is the resource field of an ARN this API mints for this kind.
    ///
    /// The forms are the ones the create handlers write and every read resolves against,
    /// so a value that is not one of them names no resource of this kind that could ever
    /// exist: the type segment AWS publishes for the kind, then the name rule that kind's
    /// own create enforces ([`check_load_balancer_name`], [`check_target_group_name`]),
    /// then the resource id every ARN of this service carries ([`is_resource_id`]).
    fn addresses(self, resource: &str) -> bool {
        match self {
            Self::LoadBalancer => {
                let Some(rest) = resource.strip_prefix("loadbalancer/") else {
                    return false;
                };
                let [type_code, name, id] = rest.split('/').collect::<Vec<&str>>()[..] else {
                    return false;
                };
                LOAD_BALANCER_TYPE_CODES.contains(&type_code)
                    && is_load_balancer_name(name)
                    && is_resource_id(id)
            }
            Self::TargetGroup => {
                let Some(rest) = resource.strip_prefix("targetgroup/") else {
                    return false;
                };
                let [name, id] = rest.split('/').collect::<Vec<&str>>()[..] else {
                    return false;
                };
                check_target_group_name(name).is_ok() && is_resource_id(id)
            }
        }
    }
}

/// Whether `value` is an ARN that names a resource of `kind` this API, addressed in
/// `region`, could hold.
///
/// Three facts about the value decide it, and none of them is the store: the ARN is this
/// service's own ([`SERVICE_NAMESPACE`]) — an ARN of another vendor names another
/// service's resource — it belongs to the Region the request addresses, because a regional
/// endpoint addresses the resources of its own Region alone, and its resource field is the
/// form this API mints for the kind, so an ARN of this service naming a resource of
/// ANOTHER kind (a target group where a load balancer belongs) is not one either.
///
/// The partition and the account are deliberately NOT read: an ARN of another account is
/// well formed and simply names no resource this store holds, which is the operation's own
/// `NotFound`.
pub fn is_resource_arn(kind: ArnKind, value: &str, region: &str) -> bool {
    ::service_common::arn::parse(value).is_some_and(|arn| {
        arn.vendor == SERVICE_NAMESPACE && arn.region == region && kind.addresses(arn.resource)
    })
}

/// The refusal the two attribute reads carry for a value that is not an ARN of the
/// resource they address: the value, and the form it failed to take.
///
/// Recorded behaviour of `DescribeLoadBalancerAttributes` and
/// `DescribeTargetGroupAttributes`: the fixed text states the FORM expected, and the one
/// variable part is the value the request carried, quoted verbatim, because that is what
/// the caller has to replace.
pub fn arn_format_message(value: &str) -> String {
    format!("'{value}' must be in ARN format")
}

/// The refusal an ARN member carries for a value that is not an ARN of `kind`: the value,
/// and the kind of ARN the member takes.
///
/// Recorded behaviour of the reads that name a kind (`DescribeLoadBalancers`'
/// `LoadBalancerArns`, `DescribeTargetGroups`' `LoadBalancerArn` and `TargetGroupArns`,
/// `DescribeTargetHealth`'s `TargetGroupArn`): the fixed text names the kind
/// ([`ArnKind::noun`]), and the one variable part is the offending value from the request.
pub fn invalid_arn_message(kind: ArnKind, value: &str) -> String {
    format!("'{value}' is not a valid {} ARN", kind.noun())
}

/// Judge an ARN member of an operation whose refusal states the FORM the value failed to
/// take, as [`arn_format_message`] words it.
pub fn check_arn_format(kind: ArnKind, value: &str, region: &str) -> Result<(), String> {
    match is_resource_arn(kind, value, region) {
        true => Ok(()),
        false => Err(arn_format_message(value)),
    }
}

/// Judge an ARN member of an operation whose refusal names the KIND of ARN it takes, as
/// [`invalid_arn_message`] words it — the reading every member takes unless its own
/// refusal is recorded otherwise.
pub fn check_resource_arn(kind: ArnKind, value: &str, region: &str) -> Result<(), String> {
    match is_resource_arn(kind, value, region) {
        true => Ok(()),
        false => Err(invalid_arn_message(kind, value)),
    }
}

/// How a refusal names the subject of a target group NAME clause.
///
/// As with a load balancer's name (`super::LOAD_BALANCER_NAME_SUBJECT`), the create that
/// takes a `Name` states `Target group name '<value>' …` (recorded behaviour), and a read
/// that SELECTS by name states the same clause with the definite subject below.
pub const TARGET_GROUP_NAME_SUBJECT: &str = "Target group name";

/// The subject a read that selects by NAME states the clauses with — see
/// [`TARGET_GROUP_NAME_SUBJECT`].
pub const SELECTED_TARGET_GROUP_NAME_SUBJECT: &str = "The target group name";

/// Whether `name` is a target group name this service accepts and — when it is not — the
/// clause it broke, stated with the subject the create uses
/// ([`TARGET_GROUP_NAME_SUBJECT`]).
pub fn check_target_group_name(name: &str) -> Result<(), String> {
    check_target_group_name_as(TARGET_GROUP_NAME_SUBJECT, name)
}

/// Whether `name` is a target group name this service accepts and — when it is not — the
/// clause it broke, so a refusal can name the value and the rule the caller has to
/// satisfy.
///
/// Every clause is one of `CreateTargetGroupInput$Name`'s own: "This name must be unique
/// per region and cannot be changed after creation. A maximum of 32 alphanumeric
/// characters including hyphens are allowed, but the name must not begin or end with a
/// hyphen." Uniqueness is the only one that needs the store, so it stays with the create
/// (a name already taken is `DuplicateTargetGroupName`); the rest are read off the value
/// alone, which is why they are stated here once — for the create that takes a name, for
/// the read that selects by one, and for the ARN that carries one
/// ([`ArnKind::addresses`]). The clauses are read in the order AWS reports them: the
/// LENGTH before the character set.
///
/// `subject` is how the operation names the subject of the clause
/// ([`TARGET_GROUP_NAME_SUBJECT`]).
pub fn check_target_group_name_as(subject: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{subject} cannot be empty"));
    }
    if name.chars().count() > TARGET_GROUP_NAME_MAX_LENGTH {
        return Err(format!(
            "{subject} '{name}' cannot be longer than '{TARGET_GROUP_NAME_MAX_LENGTH}' \
             characters"
        ));
    }
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(format!(
            "{subject} '{name}' can only contain characters that are alphanumeric \
             characters or hyphens"
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(format!("{subject} '{name}' cannot begin or end with a hyphen"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Region every ARN below belongs to, which is the Region the request addresses.
    const REGION: &str = "us-west-2";

    /// An ARN of this service in `REGION` whose resource field is `resource`.
    fn arn(resource: &str) -> String {
        format!("arn:aws:elasticloadbalancing:{REGION}:000000000000:{resource}")
    }

    #[test]
    fn a_resource_arn_is_this_vendor_this_region_and_the_form_this_api_mints() {
        // (case, the value, the kind it is read as, whether it names a resource of it)
        let rows: &[(&str, String, ArnKind, bool)] = &[
            (
                "an application load balancer ARN",
                arn("loadbalancer/app/probe-lb/0123456789abcdef"),
                ArnKind::LoadBalancer,
                true,
            ),
            (
                "a network load balancer ARN",
                arn("loadbalancer/net/probe-lb/fedcba9876543210"),
                ArnKind::LoadBalancer,
                true,
            ),
            (
                "a gateway load balancer ARN",
                arn("loadbalancer/gwy/probe-lb/0000000000000000"),
                ArnKind::LoadBalancer,
                true,
            ),
            (
                "a target group ARN",
                arn("targetgroup/probe-tg/0123456789abcdef"),
                ArnKind::TargetGroup,
                true,
            ),
            (
                "a load balancer ARN of another Region",
                "arn:aws:elasticloadbalancing:eu-west-1:000000000000:loadbalancer/app/elsewhere\
                 /0123456789abcdef"
                    .to_string(),
                ArnKind::LoadBalancer,
                false,
            ),
            (
                "a load balancer ARN of another vendor",
                "arn:aws:elasticloadbalancingv2:us-west-2:000000000000:loadbalancer/app/other\
                 /0123456789abcdef"
                    .to_string(),
                ArnKind::LoadBalancer,
                false,
            ),
            (
                "a target group ARN where a load balancer belongs",
                arn("targetgroup/probe-tg/0123456789abcdef"),
                ArnKind::LoadBalancer,
                false,
            ),
            (
                "a load balancer ARN where a target group belongs",
                arn("loadbalancer/app/probe-lb/0123456789abcdef"),
                ArnKind::TargetGroup,
                false,
            ),
            (
                "a load balancer ARN with an unknown type code",
                arn("loadbalancer/web/probe-lb/0123456789abcdef"),
                ArnKind::LoadBalancer,
                false,
            ),
            (
                "a load balancer ARN whose id is not a resource id",
                arn("loadbalancer/app/probe-lb/1"),
                ArnKind::LoadBalancer,
                false,
            ),
            (
                "a load balancer ARN whose name breaks the name rule",
                arn("loadbalancer/app/internal-probe/0123456789abcdef"),
                ArnKind::LoadBalancer,
                false,
            ),
            (
                "a target group ARN carrying a type code it has none of",
                arn("targetgroup/app/probe-tg/0123456789abcdef"),
                ArnKind::TargetGroup,
                false,
            ),
            (
                "a bare name",
                "probe-tg".to_string(),
                ArnKind::TargetGroup,
                false,
            ),
            (
                "a value with too few ARN fields",
                "arn:aws:elasticloadbalancing".to_string(),
                ArnKind::LoadBalancer,
                false,
            ),
        ];
        for (case, value, kind, names_one) in rows {
            assert_eq!(
                is_resource_arn(*kind, value, REGION),
                *names_one,
                "case {case:?}: expected is_resource_arn={names_one} for {value:?} as a \
                 {:?} ARN",
                kind
            );
        }
    }

    #[test]
    fn the_two_refusals_quote_the_value_and_differ_only_in_what_they_state() {
        let value = "not-an-arn";
        assert_eq!(
            check_arn_format(ArnKind::LoadBalancer, value, REGION),
            Err("'not-an-arn' must be in ARN format".to_string()),
            "case an attribute read's member: unexpected refusal"
        );
        assert_eq!(
            check_resource_arn(ArnKind::LoadBalancer, value, REGION),
            Err("'not-an-arn' is not a valid load balancer ARN".to_string()),
            "case a member that names its kind: unexpected refusal"
        );
        assert_eq!(
            check_resource_arn(ArnKind::TargetGroup, value, REGION),
            Err("'not-an-arn' is not a valid target group ARN".to_string()),
            "case a member of the other kind: unexpected refusal"
        );
    }

    #[test]
    fn an_instance_target_id_outside_its_form_is_refused_as_an_instance_id() {
        for id in ["10.152.1.77", "not-an-instance", "i-123", "i-0123456789abcdefg"] {
            assert_eq!(
                check_target_id("instance", id),
                Err(format!("Instance ID '{id}' is not valid")),
                "case {id:?}: unexpected refusal"
            );
        }
        for id in ["i-01234567", "i-0123456789abcdef0"] {
            assert_eq!(
                check_target_id("instance", id),
                Ok(()),
                "case {id:?}: an instance id of the documented form must be accepted"
            );
        }
    }
}
