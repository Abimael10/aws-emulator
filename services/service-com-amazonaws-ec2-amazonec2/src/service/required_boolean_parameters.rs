// SPDX-License-Identifier: CC0-1.0

//! The `@required` BOOLEAN members of this model, and the refusal a request that names no
//! value for one already gets.
//!
//! A boolean is the one member type whose absence a HANDLER cannot see: `false` is what a
//! deserialized input carries both for a request that named `false` and for one that named
//! nothing at all, so a handler reading such a member reads a value it cannot tell from a
//! default. What makes that safe is the framework's own reader: a `@required` member with no
//! value is `aws_smithy_schema_server::missing_member`, which travels out of the read as a
//! refusal this service words in EC2's own vocabulary — `MissingParameter` with the sentence
//! the member's SURFACE words ([`crate::state::absent_required_member_message`], reached
//! through [`super::parameter_refusals`]) — before any handler runs.
//!
//! So this front door states no refusal of its own for that class, and the handlers that read
//! a required boolean ([`crate::ops::update_capacity_manager_organizations_access`] among
//! them) rest on the reader above. This module is where that rule is pinned, over the members
//! of this model it governs, so a change to the reader cannot silently turn an absent
//! parameter into a `false` the caller never sent.

#[cfg(test)]
mod tests {
    use ::aws_smithy_schema::serde::SerdeError;
    use ::aws_smithy_schema_server::{DeserializableStruct, QueryShapeDeserializer};

    use crate::ops::types::{
        CreateNetworkAclEntryInput, DeleteNetworkAclEntryInput, ModifyIdFormatInput,
        ModifyIdentityIdFormatInput, UpdateCapacityManagerOrganizationsAccessInput,
    };

    /// Read `body` — an ec2Query parameter list — into `T`, the way the one place this service
    /// reads a request does ([`super::super::Ec2QueryEnvelope::deserialize_input`]).
    fn read<T: DeserializableStruct>(body: &str) -> Result<T, SerdeError> {
        let mut deserializer = QueryShapeDeserializer::new(body.as_bytes(), true);
        T::deserialize(&mut deserializer)
    }

    /// The member path a missing-member refusal carries, or `None` for a body that was read.
    fn absent_member<T: ::std::fmt::Debug + DeserializableStruct>(
        case: &str,
        body: &str,
    ) -> Option<String> {
        match read::<T>(body) {
            Ok(_) => None,
            Err(SerdeError::MissingMember { member_name }) => Some(member_name),
            Err(other) => panic!(
                "case {case:?}: expected the read to refuse the absent member or succeed, \
                 got {other:?}"
            ),
        }
    }

    /// A request that names no value for a `@required` boolean is refused, naming that member
    /// — and the same request with the member named is read.
    ///
    /// One row per surface, each naming the members the model declares `@required` on that
    /// operation's input
    /// (`smithy_query(query="operation",operation="<Operation>",model_path="smithy-model/com-amazonaws-ec2-amazonec2.json")`):
    /// every one of them but the boolean is given, so the refusal is about the boolean alone.
    #[test]
    fn a_required_boolean_the_request_does_not_name_is_refused() {
        /// Read one row's body with the type its operation declares, answering the member the
        /// read refused (`None` when it was read).
        type Read = fn(&str, &str) -> Option<String>;

        // `(case, the body without the boolean, the member the refusal must name, the
        // parameter that names the boolean, the reader of that operation's input)`.
        let rows: [(&str, &str, &str, &str, Read); 4] = [
            (
                "UpdateCapacityManagerOrganizationsAccess",
                "",
                "organizationsAccess",
                "OrganizationsAccess=false",
                absent_member::<UpdateCapacityManagerOrganizationsAccessInput>,
            ),
            (
                "ModifyIdFormat",
                "Resource=instance",
                "useLongIds",
                "UseLongIds=true",
                absent_member::<ModifyIdFormatInput>,
            ),
            (
                "ModifyIdentityIdFormat",
                "Resource=instance&PrincipalArn=arn:aws:iam::000000000000:role/r",
                "useLongIds",
                "UseLongIds=true",
                absent_member::<ModifyIdentityIdFormatInput>,
            ),
            (
                "DeleteNetworkAclEntry",
                "NetworkAclId=acl-00000000&RuleNumber=120",
                "egress",
                "Egress=false",
                absent_member::<DeleteNetworkAclEntryInput>,
            ),
        ];

        for (case, without, member, names_it, read_input) in rows {
            let refused = read_input(case, without);
            assert_eq!(
                refused.as_deref(),
                Some(member),
                "case {case:?}: expected the read to refuse the absent {member:?}, got {refused:?}"
            );

            let with = if without.is_empty() {
                names_it.to_string()
            } else {
                format!("{without}&{names_it}")
            };
            let read = read_input(case, &with);
            assert_eq!(
                read, None,
                "case {case:?}: expected a body naming {names_it:?} to be read, got {read:?}"
            );
        }
    }

    /// The refusal is about the member the request left out, not about the operation: an input
    /// whose other `@required` members are also absent is refused for one of them, so a row
    /// above that gave them all is what isolates the boolean.
    #[test]
    fn a_request_naming_nothing_is_refused_for_a_required_member() {
        let refused = absent_member::<CreateNetworkAclEntryInput>("CreateNetworkAclEntry", "");
        assert!(
            refused.is_some(),
            "case \"CreateNetworkAclEntry with nothing at all\": expected a refusal, got \
             {refused:?}"
        );

        // Every `@required` member but the boolean given: the refusal names the boolean.
        let refused = absent_member::<CreateNetworkAclEntryInput>(
            "CreateNetworkAclEntry without Egress",
            "NetworkAclId=acl-00000000&RuleNumber=120&Protocol=6&RuleAction=allow",
        );
        assert_eq!(
            refused.as_deref(),
            Some("egress"),
            "case \"CreateNetworkAclEntry without Egress\": expected the refusal to name \
             egress, got {refused:?}"
        );
    }
}
