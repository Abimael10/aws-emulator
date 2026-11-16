// SPDX-License-Identifier: CC0-1.0

//! The questions a DOMAIN create settles about its request BEFORE it stores anything, and
//! the sentences it refuses each of them with.
//!
//! Two facts a domain create settles here, in request order, so neither is worded twice:
//!
//! * the members the service itself requires, which the model leaves optional
//!   ([`required_member_message`]);
//! * the IAM roles the domain would be RUN AS, which the service has to assume
//!   ([`first_unassumable_role`]).
//!
//! # Only what IAM ANSWERED is refused
//!
//! A role is a resource of ANOTHER service, reached the way any emulated service reaches
//! another: over this emulator's own in-process callback, through the shared reading in
//! `service_common`. Nothing is guessed — an emulator with no IAM mounted to ask, and any
//! answer other than "this account holds no such role" or "the trust document it holds
//! withholds this service", refuses NOTHING. It is awaited with NO store guard held.

/// The service principal DataZone assumes a domain's caller-supplied roles as.
///
/// A domain's execution role is "created in the Amazon Web Services account that houses the
/// Amazon DataZone domain"
/// (`smithy_query(query="doc",shape="CreateDomainInput",member="domainExecutionRole")`) and is
/// taken on BY this service, so the principal whose assume has to be admitted is the service
/// itself.
pub const DATAZONE_SERVICE_PRINCIPAL: &str = "datazone.amazonaws.com";

/// The `domainExecutionRole` member name, as the wire and a refusal naming it spell it.
pub const DOMAIN_EXECUTION_ROLE_MEMBER: &str = "domainExecutionRole";

/// How a request that names NO value for a member this service requires is refused: the AWS
/// `ValidationException` envelope over one violation, naming the member and nothing else.
///
/// The envelope and the clause are the framework's own rendering of an absent required
/// member (`::aws_smithy_schema_server::missing_member_message`), which is what the request
/// boundary answers for a member the MODEL marks `@required` — so a member the SERVICE
/// requires is refused in exactly the same words, and the only variable part is the member's
/// name.
#[must_use]
pub fn required_member_message(member: &str) -> String {
    ::aws_smithy_schema_server::missing_member_message(member)
}

/// How a domain create words a role it cannot assume: the role ARN AS THE CALLER SENT IT,
/// quoted, and nothing else — there is no trust document to describe when the role is not
/// there at all.
#[must_use]
pub fn unable_to_assume_role_message(role_arn: &str) -> String {
    format!("Unable to assume role '{role_arn}'")
}

/// The FIRST of `role_arns` this emulator's own IAM answers DataZone cannot assume, or
/// `None` when it answers that about none of them.
///
/// A request that names SEVERAL roles — a domain's execution role and its service role —
/// settles them in the order the request carries them, so the refusal names the same role
/// whichever other roles the request also names. The roles are resolved one after another
/// and the first answer short-circuits, so a request whose first role is unassumable asks
/// IAM once; a role the request repeats is asked about once.
///
/// The whole question — which role of this account the ARN names, what IAM answers about it,
/// and whether the trust document admits a service principal — is the ONE reading every
/// emulated service that resolves a caller-supplied role shares
/// (`service_common::iam_role::role_is_unassumable`), so this module states only WHICH
/// principal has to be admitted.
pub async fn first_unassumable_role<'role>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arns: impl IntoIterator<Item = &'role str>,
) -> Option<&'role str> {
    let mut asked = ::std::collections::BTreeSet::new();
    for role_arn in role_arns {
        if !asked.insert(role_arn) {
            continue;
        }
        if ::service_common::iam_role::role_is_unassumable(
            ctx,
            region,
            role_arn,
            DATAZONE_SERVICE_PRINCIPAL,
        )
        .await
        {
            return Some(role_arn);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{required_member_message, unable_to_assume_role_message};

    /// RULE: the refusal of an absent required member is the AWS validation envelope over
    /// one `Member must not be null` clause naming that member — so the member name is the
    /// only part that varies.
    #[test]
    fn an_absent_required_member_is_refused_in_the_aws_validation_envelope() {
        for member in ["domainExecutionRole", "serviceRole", "name"] {
            let message = required_member_message(member);
            let expected = format!(
                "1 validation error detected: Value null at '{member}' failed to satisfy \
                 constraint: Member must not be null"
            );
            assert_eq!(
                message, expected,
                "case {member:?}: expected {expected:?}, got {message:?}"
            );
        }
    }

    /// RULE: a role this service cannot assume is reported by the ARN the request sent,
    /// quoted, with nothing else of the request or the store in the sentence.
    #[test]
    fn an_unassumable_role_is_reported_by_the_arn_the_request_sent() {
        for role in [
            "arn:aws:iam::000000000000:role/exec",
            "arn:aws:iam::000000000000:role/service-role/nested/exec",
            "arn:aws-us-gov:iam::123456789012:role/exec+name=x",
        ] {
            let message = unable_to_assume_role_message(role);
            let expected = format!("Unable to assume role '{role}'");
            assert_eq!(
                message, expected,
                "case {role:?}: expected {expected:?}, got {message:?}"
            );
        }
    }
}
