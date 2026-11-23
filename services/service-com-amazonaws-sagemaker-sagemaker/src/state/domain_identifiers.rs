// SPDX-License-Identifier: CC0-1.0

//! The IDENTIFIER FORM a Studio domain's network members have to be written in, and the
//! refusal a request that writes one some other way is answered with.
//!
//! # The rule
//!
//! A VPC and a subnet are Amazon EC2's resources, and EC2 mints their identifiers in one
//! published form: the resource's own prefix, a hyphen, then at least eight lowercase
//! alphanumeric characters (`vpc-0a1b2c3d`, `subnet-0a1b2c3d4e5f60718`). SageMaker checks
//! the FORM of the identifiers a domain's placement names before it resolves that placement
//! against anything, so a value no EC2 identifier could be — a typo, or a template that
//! interpolated the wrong thing — is refused up front and no domain is created. That is a
//! narrower rule than the shapes' own modelled constraints, which admit any `[-0-9a-zA-Z]`
//! text inside their length bounds, so it is checked here rather than by the framework's
//! constraint walk.
//!
//! This is a refusal of the CALL, unlike the placement itself ([`super::domain_network`]): a
//! well-formed identifier that names a subnet of another VPC is a provisioning failure the
//! domain carries, while a value that is not an identifier at all is a bad request.
//!
//! # The refusal
//!
//! One clause per offending member, in the order the operation lists them, inside the
//! framework's own `N validation error(s) detected: ` envelope — the whole refusal is the
//! tree's shared rendering ([`::service_common::validation_message::envelope_over`]), so a
//! clause this module states reads exactly like one the constraint walk reports. A member
//! whose value is a LIST is stated at the list member, quoting the whole list the request
//! sent and nesting the element shape's constraint set, which is how this service states
//! every constraint a list's elements break (`crate::service::list_element_refusals`).

/// The prefix EC2 mints a VPC identifier with, i.e. the `vpc` in `vpc-0a1b2c3d`.
const VPC_ID_PREFIX: &str = "vpc";

/// The prefix EC2 mints a subnet identifier with.
const SUBNET_ID_PREFIX: &str = "subnet";

/// The characters an EC2 identifier carries after its prefix, and how few of them there can
/// be: at least eight lowercase alphanumerics, which is the form the refusal states and the
/// one the check applies.
const ID_SUFFIX_PATTERN: &str = "[a-z0-9]{8,}";

/// The member path `CreateDomain$VpcId` is stated at, i.e. its name as the wire spells it.
const VPC_ID_MEMBER: &str = "vpcId";

/// The member path `CreateDomain$SubnetIds` is stated at.
const SUBNET_IDS_MEMBER: &str = "subnetIds";

/// The expression a `VpcId` has to satisfy, as the refusal states it: the prefix, then
/// [`ID_SUFFIX_PATTERN`]. UNANCHORED, the way the service states the requirement; the check
/// itself is about the whole value ([`compiled`]).
#[must_use]
pub fn vpc_id_pattern() -> String {
    format!("{VPC_ID_PREFIX}-{ID_SUFFIX_PATTERN}")
}

/// The expression a `SubnetId` has to satisfy, worded like [`vpc_id_pattern`] for the other
/// half of the placement.
#[must_use]
pub fn subnet_id_pattern() -> String {
    format!("{SUBNET_ID_PREFIX}-{ID_SUFFIX_PATTERN}")
}

/// Whether `vpc_id` is written in the form EC2 mints a VPC identifier in.
///
/// The expression is compiled ONCE for the process, on first use: it is built from
/// constants, so compiling it per request would be work repeated for every create.
#[must_use]
pub fn well_formed_vpc_id(vpc_id: &str) -> bool {
    static FORM: ::std::sync::OnceLock<::regex::Regex> = ::std::sync::OnceLock::new();
    FORM.get_or_init(|| compiled(&vpc_id_pattern()))
        .is_match(vpc_id)
}

/// Whether `subnet_id` is written in the form EC2 mints a subnet identifier in, read the
/// same way [`well_formed_vpc_id`] reads a VPC's.
#[must_use]
pub fn well_formed_subnet_id(subnet_id: &str) -> bool {
    static FORM: ::std::sync::OnceLock<::regex::Regex> = ::std::sync::OnceLock::new();
    FORM.get_or_init(|| compiled(&subnet_id_pattern()))
        .is_match(subnet_id)
}

/// `pattern` anchored over the WHOLE value and compiled: the expression the refusal states
/// is unanchored, as the service states it, while the check is about the whole identifier.
fn compiled(pattern: &str) -> ::regex::Regex {
    ::regex::Regex::new(&format!("^{pattern}$")).expect("the identifier form compiles")
}

/// Why the network identifiers a `CreateDomain` names are not ones EC2 could have minted, or
/// `None` when every identifier the request carried is written in the published form.
///
/// The `VpcId` is weighed first and the `SubnetIds` after it, so a request that gets both
/// wrong is answered with both clauses in the order the operation lists its members. A
/// member the request did not carry has nothing to check; an EMPTY `VpcId` carries no
/// identifier either, which the length bound its own shape declares already admits, so it is
/// left to that bound rather than refused here as a malformed one.
///
/// `input_schema` is the operation's own input shape, which the element constraints of
/// `SubnetIds` are read off: the clause states the element shape's whole declared set beside
/// the form, so the length bounds come from the MODEL rather than from a second copy here.
#[must_use]
pub fn domain_network_identifier_refusal(
    input_schema: &'static ::aws_smithy_schema::Schema,
    vpc_id: Option<&str>,
    subnet_ids: &[String],
) -> Option<String> {
    let mut clauses: Vec<String> = Vec::new();
    let malformed_vpc_id =
        vpc_id.filter(|vpc_id| !vpc_id.is_empty() && !well_formed_vpc_id(vpc_id));
    if let Some(vpc_id) = malformed_vpc_id {
        clauses.push(malformed_vpc_id_clause(vpc_id));
    }
    if subnet_ids
        .iter()
        .any(|subnet_id| !well_formed_subnet_id(subnet_id))
    {
        clauses.push(malformed_subnet_ids_clause(input_schema, subnet_ids));
    }
    ::service_common::validation_message::envelope_over(&clauses)
}

/// The clause a malformed `VpcId` is stated with: the value AS THE REQUEST SENT IT, the
/// member it was sent at, and the form it had to be written in.
///
/// The framework's own clause ([`::aws_smithy_schema_server::Violation::message`]) over a
/// pattern violation, so a requirement this module states reads exactly like one the
/// constraint walk reports.
fn malformed_vpc_id_clause(vpc_id: &str) -> String {
    ::aws_smithy_schema_server::Violation {
        path: VPC_ID_MEMBER.to_string(),
        kind: ::aws_smithy_schema_server::ViolationKind::Pattern(vpc_id_pattern()),
        value: Some(vpc_id.to_string()),
        sensitive: false,
    }
    .message()
}

/// The clause a malformed `SubnetIds` is stated with: the WHOLE list the request sent, at the
/// list member, and the element shape's constraint set with the identifier form standing in
/// for the expression the model declares.
///
/// So a list with two malformed entries is ONE validation error about one member, and a
/// caller is told everything an entry has to satisfy rather than the one requirement that
/// happened to fail — the rule this service states every element constraint with
/// (`crate::service::list_element_refusals`). The length bounds are read off the model; the
/// one expression the clause states is the identifier FORM, which is the narrower of the two
/// and the only one this refusal is about: a value outside the element shape's own modelled
/// pattern never reaches here, because the framework's constraint walk refuses it first with
/// that pattern's own clause.
fn malformed_subnet_ids_clause(
    input_schema: &'static ::aws_smithy_schema::Schema,
    subnet_ids: &[String],
) -> String {
    let mut declared = ::service_common::validation_message::element_constraint_set(
        &[input_schema],
        SUBNET_IDS_MEMBER,
    )
    .unwrap_or_default();
    declared.retain(|kind| {
        !matches!(
            kind,
            ::aws_smithy_schema_server::ViolationKind::Pattern(_)
        )
    });
    declared.push(::aws_smithy_schema_server::ViolationKind::Pattern(
        subnet_id_pattern(),
    ));
    let requirements: Vec<String> = declared
        .into_iter()
        .map(::service_common::validation_message::requirement)
        .collect();
    format!(
        "Value '[{}]' at '{SUBNET_IDS_MEMBER}' failed to satisfy constraint: {}",
        subnet_ids.join(", "),
        ::service_common::validation_message::element_set_requirement(&requirements)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        domain_network_identifier_refusal, subnet_id_pattern, vpc_id_pattern,
        well_formed_subnet_id, well_formed_vpc_id,
    };

    /// RULE: a value is in the published identifier form only when it is the prefix, a
    /// hyphen and at least eight lowercase alphanumerics, all of it.
    #[test]
    fn the_identifier_form_admits_what_ec2_mints_and_nothing_else() {
        for (case, vpc_id, admitted) in [
            ("the shortest VPC id EC2 mints", "vpc-0a1b2c3d", true),
            ("a longer VPC id", "vpc-0a1b2c3d4e5f60718", true),
            ("one character short", "vpc-0a1b2c", false),
            ("an uppercase suffix", "vpc-0A1B2C3D", false),
            ("no prefix at all", "not-a-real-vpc-id", false),
            ("a subnet id at the VPC member", "subnet-0a1b2c3d", false),
            ("trailing text", "vpc-0a1b2c3d!", false),
            ("an empty value", "", false),
        ] {
            let written = well_formed_vpc_id(vpc_id);
            assert!(
                written == admitted,
                "case {case:?}: expected admitted={admitted} for {vpc_id:?} against {:?}, \
                 got {written}",
                vpc_id_pattern()
            );
        }
        for (case, subnet_id, admitted) in [
            ("the shortest subnet id EC2 mints", "subnet-0a1b2c3d", true),
            ("a longer subnet id", "subnet-0a1b2c3d4e5f60718", true),
            ("one character short", "subnet-0a1b2c", false),
            ("no prefix at all", "not-a-real-subnet-id", false),
            ("a VPC id at the subnet member", "vpc-0a1b2c3d", false),
        ] {
            let written = well_formed_subnet_id(subnet_id);
            assert!(
                written == admitted,
                "case {case:?}: expected admitted={admitted} for {subnet_id:?} against {:?}, \
                 got {written}",
                subnet_id_pattern()
            );
        }
    }

    /// RULE: one clause per offending member, in the order the operation lists them, inside
    /// the framework's envelope — and no refusal at all for identifiers in the form.
    #[test]
    fn the_refusal_states_one_clause_per_offending_member() {
        let schema = crate::ops::types::CREATE_DOMAIN_INPUT_SCHEMA;
        // The element shape's own `@length` bounds, read off the model by the shared
        // element-set reading, with the identifier form standing in for the expression the
        // model declares.
        let subnet_set = "Member must satisfy constraint: [Member must have length less than \
                          or equal to 32, Member must have length greater than or equal to 0, \
                          Member must satisfy regular expression pattern: subnet-[a-z0-9]{8,}]";
        for (case, vpc_id, subnet_ids, expected) in [
            ("neither member carried", None, Vec::<String>::new(), None),
            (
                "both members in the published form",
                Some("vpc-0a1b2c3d"),
                vec!["subnet-0a1b2c3d".to_string()],
                None,
            ),
            (
                "a malformed VPC id alone",
                Some("not-a-real-vpc-id"),
                vec!["subnet-0a1b2c3d".to_string()],
                Some(format!(
                    "1 validation error detected: Value 'not-a-real-vpc-id' at 'vpcId' failed \
                     to satisfy constraint: Member must satisfy regular expression pattern: {}",
                    vpc_id_pattern()
                )),
            ),
            (
                "one malformed entry among well-formed ones",
                Some("vpc-0a1b2c3d"),
                vec!["subnet-0a1b2c3d".to_string(), "subnet-1".to_string()],
                Some(format!(
                    "1 validation error detected: Value '[subnet-0a1b2c3d, subnet-1]' at \
                     'subnetIds' failed to satisfy constraint: {subnet_set}"
                )),
            ),
            (
                "both members malformed",
                Some("not-a-real-vpc-id"),
                vec!["not-a-real-subnet-id".to_string()],
                Some(format!(
                    "2 validation errors detected: Value 'not-a-real-vpc-id' at 'vpcId' failed \
                     to satisfy constraint: Member must satisfy regular expression pattern: \
                     {}; Value '[not-a-real-subnet-id]' at 'subnetIds' failed to satisfy \
                     constraint: {subnet_set}",
                    vpc_id_pattern()
                )),
            ),
        ] {
            let stated = domain_network_identifier_refusal(schema, vpc_id, &subnet_ids);
            assert!(
                stated == expected,
                "case {case:?}: expected {expected:?}, got {stated:?}"
            );
        }
    }
}
