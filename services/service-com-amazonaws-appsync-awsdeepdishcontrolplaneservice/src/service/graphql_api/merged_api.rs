// SPDX-License-Identifier: CC0-1.0

//! Whether a MERGED API may EXECUTE a field one of its source APIs contributed.
//!
//! `GraphqlApi$mergedApiExecutionRoleArn` is "the Identity and Access Management service role
//! ARN for a merged API. The AppSync service assumes this role on behalf of the Merged API to
//! validate access to source APIs at runtime", so a request on a Merged API's `GRAPHQL`
//! endpoint does not run a merged field under the caller's own permissions: it runs it as a
//! session of that role, and the role has to be allowed the `appsync:SourceGraphQL` action on
//! the field it is resolving — the source API's own resource, down to the type and the field
//! ([`field_resource`]).
//!
//! A field the role may not execute is an error OF THAT FIELD: the value is `null` and the
//! answer carries the refusal under the field's path, as every other field-level refusal of
//! this plane does ([`super::field_auth`]). It is not a refusal of the request: the rest of
//! the selection resolves.
//!
//! The reading of what IAM holds about the role is the one every role of this plane is judged
//! by ([`super::data_source_role`]): a role this account's IAM HOLDS is the authority on what
//! may be done under it, so the action has to be allowed and an implicit deny — a policy set
//! that allows it nowhere — withholds the field; a value IAM holds no role under says nothing
//! about the action, since such a role is routinely provisioned out of band, so only an
//! explicit deny withholds it. Nothing here writes.

use super::PlaneContext;

/// The source API a merged field is resolved from, as a resolution carries it: what the field
/// is authorized against.
#[derive(Debug, Clone)]
pub struct MergedSource {
    /// The ARN of the source API whose merge carried the field, which the resource the action
    /// is weighed on is built from.
    pub source_api_arn: String,
    /// The Merged API's own `mergedApiExecutionRoleArn`, or `None` for one carrying none —
    /// which names no role to weigh anything on.
    pub execution_role_arn: Option<String>,
}

/// The IAM action an execution of a source API's field performs, which the AppSync developer
/// guide's merged-API execution role is granted on its source APIs.
pub const SOURCE_GRAPHQL_ACTION: &str = "appsync:SourceGraphQL";

/// The resource that action is performed on: the field being resolved, under the SOURCE API's
/// own ARN — the API that contributed it — as AppSync names a field of a type of an API.
#[must_use]
pub fn field_resource(source_api_arn: &str, type_name: &str, field_name: &str) -> String {
    format!("{source_api_arn}/types/{type_name}/fields/{field_name}")
}

/// The sentence AppSync refuses such a field with: the action it could not perform and the
/// resource it would have performed it on.
#[must_use]
pub fn not_authorized_message(resource: &str) -> String {
    format!("{NOT_AUTHORIZED_TO_PERFORM} {SOURCE_GRAPHQL_ACTION} on resource {resource}")
}

/// The fixed opening of that sentence.
const NOT_AUTHORIZED_TO_PERFORM: &str = "Not authorized to perform:";

/// The refusal the permissions in force on the Merged API's execution role withhold
/// `field_name` of `type_name` with, or `None` where nothing is known against executing it.
///
/// Everything that is not an answer withholds nothing: a Merged API carrying no execution
/// role, a value naming no role of this account, no IAM to ask and a failed hop.
pub async fn withheld(
    context: &PlaneContext,
    source: &MergedSource,
    type_name: &str,
    field_name: &str,
) -> Option<String> {
    let role_arn = source.execution_role_arn.as_deref()?;
    let resource = field_resource(&source.source_api_arn, type_name, field_name);
    let decision = ::service_common::iam_role::role_action_decision_through(
        &context.callback,
        &context.region,
        role_arn,
        SOURCE_GRAPHQL_ACTION,
        &resource,
        &[],
    )
    .await?;
    match decision {
        ::service_common::iam_authorization::Decision::Allowed => None,
        ::service_common::iam_authorization::Decision::Denied => {
            Some(not_authorized_message(&resource))
        }
        // Nothing in force states this execution, which is an answer about the role only
        // where IAM holds the role whose policies were weighed.
        ::service_common::iam_authorization::Decision::NotStated => {
            match ::service_common::iam_role::role_is_held_through(
                &context.callback,
                &context.region,
                role_arn,
            )
            .await
            {
                true => Some(not_authorized_message(&resource)),
                false => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the action is performed on the FIELD being resolved, under the source API's own
    /// ARN. The rows differ along the type and the field, including a field of a root
    /// operation type other than the query root.
    #[test]
    fn the_resource_names_the_field_under_the_source_apis_arn() {
        let arn = "arn:aws:appsync:us-west-2:123456789012:apis/abcdefghijklmnopqrstuvwxyz";
        for (case, type_name, field_name, expected) in [
            (
                "a field of the query root",
                "Query",
                "greeting",
                format!("{arn}/types/Query/fields/greeting"),
            ),
            (
                "a field of the mutation root",
                "Mutation",
                "putNote",
                format!("{arn}/types/Mutation/fields/putNote"),
            ),
            (
                "a field of an object type",
                "Note",
                "author",
                format!("{arn}/types/Note/fields/author"),
            ),
        ] {
            let resource = field_resource(arn, type_name, field_name);
            assert_eq!(
                resource, expected,
                "case {case:?}: expected {expected:?}, got {resource:?}"
            );
        }
    }

    /// RULE: the refusal states the action that was not allowed and the resource it was
    /// weighed on, with the fixed text AppSync words it with.
    #[test]
    fn the_refusal_states_the_action_and_the_resource() {
        let resource = "arn:aws:appsync:eu-west-1:123456789012:apis/source/types/Query/fields/x";
        let message = not_authorized_message(resource);
        let expected =
            format!("Not authorized to perform: appsync:SourceGraphQL on resource {resource}");
        assert_eq!(
            message, expected,
            "case {:?}: expected {expected:?}, got {message:?}",
            "a field of a source API"
        );
    }
}
