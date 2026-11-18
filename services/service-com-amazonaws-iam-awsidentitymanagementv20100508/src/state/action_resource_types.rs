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
//! An action published WITH a resource type is performed on the resources of ONE service —
//! its own, or the service whose resources it acts on — so a pair naming a resource of
//! another service is likewise a request that action could not have been made as
//! ([`names_a_resource_of_the_action`]), and the TYPE the action is performed on is what the
//! action's own result names the resource it was simulated against by
//! ([`resource_arn_template`]).
//!
//! One concern, held in one place: the catalogs, the lookups over them
//! ([`takes_a_named_resource`], [`resource_service`]) and the pair-level question the
//! simulation asks ([`applies_to_resource`]).

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
/// performed on — and only where that resource is one of the resources the action is
/// published to take ([`names_a_resource_of_the_action`]).
#[must_use]
pub fn applies_to_resource(action: &str, resource: &str) -> bool {
    if resource == super::simulated_decisions::ANY_RESOURCE {
        return true;
    }
    takes_a_named_resource(action) && names_a_resource_of_the_action(action, resource)
}

/// Whether `resource` names a resource `action` is published to take, which is a resource of
/// the service the action acts on ([`resource_service`]): an action of one service performed
/// on a resource of another is a request that action could not have been made as, however
/// widely the documents in force are written.
///
/// The question is asked of the ARN's service segment, which is what an ARN names its
/// resource's service by; a value this cannot read as an ARN, or an action not spelled
/// `<namespace>:<Action>`, is resolved to the PERMISSIVE answer, the way an action no catalog
/// row names is — such a pair is decided from the documents in force, exactly as before this
/// question was asked.
#[must_use]
pub fn names_a_resource_of_the_action(action: &str, resource: &str) -> bool {
    match (resource_service(action), arn_service(resource)) {
        (Some(acts_on), Some(named)) => named.eq_ignore_ascii_case(acts_on),
        _ => true,
    }
}

/// The service whose resources `action` is performed on: the action's own namespace, or the
/// namespace [`ACTIONS_ON_ANOTHER_SERVICE`] records when the action acts on another service's
/// resources. `None` for a value not spelled `<namespace>:<Action>`.
#[must_use]
pub fn resource_service(action: &str) -> Option<&str> {
    let (namespace, operation) = action.split_once(':')?;
    if namespace.is_empty() || operation.is_empty() {
        return None;
    }
    let acts_on = ACTIONS_ON_ANOTHER_SERVICE
        .iter()
        .find(|(published, _)| published.eq_ignore_ascii_case(action))
        .map(|(_, service)| *service);
    Some(acts_on.unwrap_or(namespace))
}

/// The service segment of `resource` read as an ARN (`arn:<partition>:<service>:…`), or
/// `None` for a value that is no ARN.
fn arn_service(resource: &str) -> Option<&str> {
    let mut segments = resource.split(':');
    if segments.next()? != "arn" {
        return None;
    }
    // The partition, which this question is not about.
    segments.next()?;
    let service = segments.next()?;
    (!service.is_empty()).then_some(service)
}

/// The ARN TEMPLATE of the resource type `action` is performed on: "the ARN template for the
/// simulated resource type (for example, `arn:${Partition}:s3:::${BucketName}/${KeyName}`),
/// or `*` if no ARN format is defined for the action. This is not a specific
/// customer-provided resource ARN" (`EvaluationResult$EvalResourceName`).
///
/// So the value is a fact about the ACTION and the service whose resources it acts on
/// ([`resource_service`]) rather than about any resource a request named: it is the ARN
/// format that service publishes its resources under ([`SERVICE_ARN_TEMPLATES`], and
/// otherwise the general form of an ARN), and the wildcard for an action no ARN format is
/// defined for — one published with no resource type at all
/// ([`ACTIONS_WITHOUT_RESOURCE_TYPES`]) or named in a spelling that is no action.
#[must_use]
pub fn resource_arn_template(action: &str) -> String {
    let wildcard = || super::simulated_decisions::ANY_RESOURCE.to_string();
    if !takes_a_named_resource(action) {
        return wildcard();
    }
    let Some(service) = resource_service(action) else {
        return wildcard();
    };
    let service = service.to_ascii_lowercase();
    match SERVICE_ARN_TEMPLATES
        .iter()
        .find(|(published, _)| *published == service)
    {
        Some((_, template)) => (*template).to_string(),
        // The general form of an ARN, which is the form the services that carry both a region
        // and an owning account publish their resources under.
        None => format!("arn:aws:{service}:${{Region}}:${{Account}}:${{ResourceType}}/${{ResourcePath}}"),
    }
}

/// The services whose published ARN format is NOT the general one every segment of an ARN is
/// filled in, ascending by namespace.
///
/// SOURCE: the ARN format each service publishes in the Service Authorization Reference
/// ("Resource types defined by …"), the same publication the catalog below is an excerpt of.
/// A service global to a partition carries no region, and one whose resources are not owned
/// by an account carries neither region nor account — which is why the general form cannot
/// stand for them. Every other service's resources are named
/// `arn:aws:<service>:<region>:<account>:<type>/<path>`, which [`resource_arn_template`]
/// writes as the template directly.
const SERVICE_ARN_TEMPLATES: &[(&str, &str)] = &[
    // IAM is global to its partition, so its resources carry an owning account and no region.
    ("iam", "arn:aws:iam::${Account}:${ResourceType}/${ResourceName}"),
    // An S3 bucket's name is global, so neither the region nor the owning account is written
    // into the ARN of a bucket or of an object in one — the example
    // `EvaluationResult$EvalResourceName`'s own documentation gives.
    ("s3", "arn:aws:s3:::${BucketName}/${KeyName}"),
];

/// The actions performed on the resources of ANOTHER service than their own namespace, with
/// that service's namespace, ascending by action.
///
/// SOURCE: the Service Authorization Reference's resource-type column for each action, as
/// [`ACTIONS_WITHOUT_RESOURCE_TYPES`] is read from its resource-type column too: a token
/// mint of the token service is performed on the IAM role it mints a session of, which is a
/// resource of IAM. An action no row names is performed on its own service's resources,
/// which is the rule the rows are the exception to.
const ACTIONS_ON_ANOTHER_SERVICE: &[(&str, &str)] = &[
    ("sts:AssumeRole", "iam"),
    ("sts:AssumeRoleWithSAML", "iam"),
    ("sts:AssumeRoleWithWebIdentity", "iam"),
    ("sts:SetSourceIdentity", "iam"),
    ("sts:TagSession", "iam"),
];

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

    /// Every catalog is ordered and every row of one names an action or a service: strictly
    /// ascending (so no row is duplicated) and each action spelled `<namespace>:<Action>`
    /// with both halves non-empty, which is the form a request names an action in.
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
        for pair in ACTIONS_ON_ANOTHER_SERVICE.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "the cross-service catalog must ascend strictly: {:?} is not before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
        for pair in SERVICE_ARN_TEMPLATES.windows(2) {
            assert!(
                pair[0].0 < pair[1].0,
                "the template catalog must ascend strictly: {:?} is not before {:?}",
                pair[0].0,
                pair[1].0
            );
        }
        let actions = ACTIONS_WITHOUT_RESOURCE_TYPES
            .iter()
            .copied()
            .chain(ACTIONS_ON_ANOTHER_SERVICE.iter().map(|(action, _)| *action));
        for action in actions {
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
    /// action applies to a named resource of the service it acts on.
    #[test]
    fn only_a_catalogued_action_is_confined_to_the_wildcard() {
        let any = super::super::simulated_decisions::ANY_RESOURCE;
        // The rows differ along what the rule generalises over: whether the action is
        // published with a resource type, the service it belongs to, the casing it is
        // written in, and the resource it is asked about (the wildcard, an ARN of its own
        // service, an ARN of another service, and a value that is no ARN).
        let rows: [(&str, &str, &str, bool); 15] = [
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
            (
                "a task run, on a resource of its own service",
                "ecs:RunTask",
                "arn:aws:ecs:us-west-2:000000000000:task-definition/reports:1",
                true,
            ),
            (
                "the same task run, on a resource of ANOTHER service",
                "ecs:RunTask",
                "arn:aws:iam::000000000000:role/task-execution",
                false,
            ),
            (
                "a session mint, on the role it mints a session of",
                "sts:AssumeRole",
                "arn:aws:iam::000000000000:role/deployer",
                true,
            ),
            (
                "the same session mint, on a queue",
                "sts:AssumeRole",
                "arn:aws:sqs:us-west-2:000000000000:orders",
                false,
            ),
            (
                "an object read, on a value that is no ARN at all",
                "s3:GetObject",
                "reports/june.csv",
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

    /// RULE: the ARN template of an action is the ARN format the service it acts on publishes
    /// its resources under, and the wildcard for an action no ARN format is defined for.
    #[test]
    fn an_actions_template_is_the_arn_format_of_the_service_it_acts_on() {
        let any = super::super::simulated_decisions::ANY_RESOURCE;
        // The rows differ along what the rule generalises over: whether the service is
        // regional, global to its partition or global outright, whether the action acts on
        // another service's resources, whether it is published with a resource type at all,
        // and the casing the action is written in.
        let rows: &[(&str, &str, &str)] = &[
            (
                "a task run, on a regional service's resources",
                "ecs:RunTask",
                "arn:aws:ecs:${Region}:${Account}:${ResourceType}/${ResourcePath}",
            ),
            (
                "a log write, on another regional service's resources",
                "logs:PutLogEvents",
                "arn:aws:logs:${Region}:${Account}:${ResourceType}/${ResourcePath}",
            ),
            (
                "a role handover, on a service global to its partition",
                "iam:PassRole",
                "arn:aws:iam::${Account}:${ResourceType}/${ResourceName}",
            ),
            (
                "an object read, on a service whose names are global",
                "s3:GetObject",
                "arn:aws:s3:::${BucketName}/${KeyName}",
            ),
            (
                "a session mint, on the service whose resources it acts on",
                "sts:AssumeRole",
                "arn:aws:iam::${Account}:${ResourceType}/${ResourceName}",
            ),
            (
                "an action published with no resource type",
                "ec2:DescribeInstances",
                any,
            ),
            ("a value that is no action", "ListUsers", any),
            (
                "an action in another casing",
                "ECS:runtask",
                "arn:aws:ecs:${Region}:${Account}:${ResourceType}/${ResourcePath}",
            ),
        ];

        for (case, action, expected) in rows {
            let template = resource_arn_template(action);
            assert_eq!(
                template, *expected,
                "case {case:?}: expected the template {expected:?} for {action}, got \
                 {template:?}"
            );
        }
    }
}
