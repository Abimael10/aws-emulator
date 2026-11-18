// SPDX-License-Identifier: CC0-1.0

//! Which RESOURCES an action can be performed on at all, for the simulation that decides
//! one `(action, resource)` pair at a time.
//!
//! A statement's `Resource` element says which resources its grant covers, but a grant
//! covers a pair only where the ACTION applies to that resource in the first place. Every
//! action is published with the resource types it takes, and an action published with NONE
//! is performed on no resource: a policy can only permit it by covering every resource
//! (`"Resource": "*"`), and a request naming one concrete resource is not a request that
//! action could have been made as. So a simulation of such a pair reports that nothing
//! permitted it, however widely the documents in force are written — which is what tells a
//! reviewer that a token-minting grant has to stay scoped to `*` while the resource-scoped
//! actions of the same service do not.
//!
//! This is the reading this service already takes of its OWN account-wide actions, which
//! are decided against `*` because "the Service Authorization Reference lists no resource
//! type for such an IAM action" ([`super::authorization::ACCOUNT_WIDE_RESOURCE`]). The
//! question here is asked about ANOTHER service's action, which a simulation request names
//! itself, so the actions whose publication carries no resource type are held in one
//! catalog ([`ACTIONS_WITHOUT_RESOURCE_TYPES`]) rather than read off an operation of this
//! service.
//!
//! One concern, held in one place: the catalog, the lookup over it
//! ([`takes_a_named_resource`]) and the pair-level question the simulation asks
//! ([`applies_to_resource`]).

#[allow(unused_imports)]
use super::*;

/// Whether `action` can be performed on `resource` — the question a simulated
/// `(action, resource)` pair is read by BEFORE any document is evaluated for it.
///
/// The resource standing for every resource (`super::simulated_decisions::ANY_RESOURCE`,
/// which is also what a request naming no resource is decided about) is a resource every
/// action applies to: it is the resource element a policy has to use for an action that
/// takes no resource, and it covers the resources of every action that does. Any other
/// value names ONE resource, which only an action published with a resource type can be
/// performed on.
#[must_use]
pub fn applies_to_resource(action: &str, resource: &str) -> bool {
    resource == super::simulated_decisions::ANY_RESOURCE || takes_a_named_resource(action)
}

/// Whether `action` is published with any resource type at all, i.e. whether a policy can
/// scope it to a named resource.
///
/// Action names are not distinguished by case in the policy language, so the comparison is
/// case-folded; an action no catalog row names is one that takes a resource, because the
/// catalog records the exception rather than the rule — and because the catalog is an
/// excerpt, so resolving a gap in it the other way would withhold grants the documents
/// carry ([`ACTIONS_WITHOUT_RESOURCE_TYPES`]).
#[must_use]
pub fn takes_a_named_resource(action: &str) -> bool {
    !ACTIONS_WITHOUT_RESOURCE_TYPES
        .iter()
        .any(|published| published.eq_ignore_ascii_case(action))
}

/// The actions published with NO resource type, ascending by their `<namespace>:<Action>`
/// spelling.
///
/// SOURCE: the Service Authorization Reference ("Actions, resources, and condition keys for
/// Amazon Web Services services"), the same publication this crate reads a service's
/// namespace out of ([`super::PUBLISHED_SERVICES`]) and names each IAM action from
/// ([`super::authorization::actions`]): an action whose row lists no resource type is one
/// "that does not support resource-level permissions", which a statement must therefore
/// permit on `*`.
///
/// The rows span the services such an action belongs to — a registry's token mint, an
/// identity read, an account-wide listing, a key mint, a random-value generator — because
/// the property is the ACTION's, not one service's: each of them is performed on the
/// account rather than on anything a policy could name.
///
/// Ascending, so a new row is filed at its place rather than appended and a duplicate
/// cannot hide in the list (asserted below).
///
/// It is an EXCERPT of that publication rather than the whole of it, and the lookup over it
/// resolves an action no row names to the PERMISSIVE answer — such an action is decided from
/// the documents in force, against a named resource as against the wildcard, exactly as
/// before this catalog existed. So a simulation that reports an allow for an uncatalogued
/// action against a named resource says what the documents decide and nothing about the
/// action's own publication; a resourceless action is filed here, at its place, as the
/// behaviour that needs it arrives, the way a service is filed into
/// [`super::PUBLISHED_SERVICES`]. The permissive resolution is deliberate: denying on a gap
/// in this list would withhold a grant the documents DO carry, which is the answer a
/// least-privilege review would then have to work around.
pub const ACTIONS_WITHOUT_RESOURCE_TYPES: &[&str] = &[
    "cloudformation:ListStacks",
    "dynamodb:ListTables",
    "ec2:DescribeAvailabilityZones",
    "ec2:DescribeInstances",
    "ec2:DescribeRegions",
    "ecr-public:GetAuthorizationToken",
    "ecr:GetAuthorizationToken",
    "ecs:ListClusters",
    "iam:GenerateCredentialReport",
    "iam:GetAccountAuthorizationDetails",
    "iam:GetAccountSummary",
    "iam:GetCredentialReport",
    "iam:ListAccountAliases",
    "iam:ListGroups",
    "iam:ListPolicies",
    "iam:ListRoles",
    "iam:ListUsers",
    "iam:SimulateCustomPolicy",
    "kms:CreateKey",
    "kms:GenerateRandom",
    "kms:ListKeys",
    "lambda:ListFunctions",
    "s3:ListAllMyBuckets",
    "secretsmanager:GetRandomPassword",
    "secretsmanager:ListSecrets",
    "sns:ListTopics",
    "sqs:ListQueues",
    "ssm:DescribeParameters",
    "sts:DecodeAuthorizationMessage",
    "sts:GetCallerIdentity",
    "sts:GetSessionToken",
];

#[cfg(test)]
mod tests {
    use super::*;

    /// The catalog is ordered and every row is one action name: strictly ascending (so no row
    /// is duplicated) and each spelled `<namespace>:<Action>` with both halves non-empty,
    /// which is the form a request names an action in.
    #[test]
    fn the_catalog_is_ordered_and_every_row_names_an_action() {
        for pair in ACTIONS_WITHOUT_RESOURCE_TYPES.windows(2) {
            assert!(
                pair[0] < pair[1],
                "the catalog must ascend strictly: {:?} is not before {:?}",
                pair[0],
                pair[1]
            );
        }
        for action in ACTIONS_WITHOUT_RESOURCE_TYPES {
            let named = action
                .split_once(':')
                .is_some_and(|(service, operation)| !service.is_empty() && !operation.is_empty());
            assert!(
                named,
                "every catalog row names a service and an action, got {action:?}"
            );
        }
    }

    /// RULE: a catalogued action applies to the wildcard resource alone, while every other
    /// action applies to a named resource as well.
    #[test]
    fn only_a_catalogued_action_is_confined_to_the_wildcard() {
        let any = super::super::simulated_decisions::ANY_RESOURCE;
        // The rows differ along what the rule generalises over: whether the action is
        // published with a resource type, the service it belongs to, the casing it is
        // written in, and the resource it is asked about (the wildcard, and an ARN of its
        // own service).
        let rows: [(&str, &str, &str, bool); 10] = [
            (
                "a registry token mint, on the wildcard",
                "ecr:GetAuthorizationToken",
                any,
                true,
            ),
            (
                "the same action on a repository",
                "ecr:GetAuthorizationToken",
                "arn:aws:ecr:us-west-2:000000000000:repository/app",
                false,
            ),
            (
                "a repository read on the same repository",
                "ecr:BatchGetImage",
                "arn:aws:ecr:us-west-2:000000000000:repository/app",
                true,
            ),
            (
                "an identity read, on an identity's ARN",
                "sts:GetCallerIdentity",
                "arn:aws:iam::000000000000:user/builder",
                false,
            ),
            (
                "an account-wide listing, on a bucket",
                "s3:ListAllMyBuckets",
                "arn:aws:s3:::reports",
                false,
            ),
            (
                "an object read, on an object",
                "s3:GetObject",
                "arn:aws:s3:::reports/june.csv",
                true,
            ),
            (
                "a key listing, on a key",
                "kms:ListKeys",
                "arn:aws:kms:eu-west-1:000000000000:key/3f6c7a4e",
                false,
            ),
            (
                "a key use, on the same key",
                "kms:Decrypt",
                "arn:aws:kms:eu-west-1:000000000000:key/3f6c7a4e",
                true,
            ),
            (
                "a catalogued action in another casing",
                "ECR:getauthorizationtoken",
                "arn:aws:ecr:us-west-2:000000000000:repository/app",
                false,
            ),
            (
                "an action of a service no row names",
                "sqs:SendMessage",
                "arn:aws:sqs:us-west-2:000000000000:orders",
                true,
            ),
        ];

        for (case, action, resource, expected) in rows {
            let applies = applies_to_resource(action, resource);
            assert_eq!(
                applies, expected,
                "case {case:?}: expected {action} on {resource} to apply? {expected}, got \
                 {applies}"
            );
        }
    }
}
