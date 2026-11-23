// SPDX-License-Identifier: CC0-1.0

//! The ADMISSION slice of this service's state: the questions a SageMaker create answers
//! about the identity, the references and the network a request names BEFORE it stores
//! anything, and the sentences it refuses each of them with.
//!
//! The three facts a create settles are stated once in this module so no two of them word
//! the same refusal differently:
//!
//! * the execution role the resource would RUN AS ([`execution_role_is_unassumable`]);
//! * the Amazon S3 location its work would READ, asked through [`crate::state::s3_access`]
//!   and worded per resource family ([`could_not_access_model_data`] for the hosting
//!   creates, [`no_s3_objects_found`] for the training ones);
//! * the network placement it asks for ([`SECURITY_GROUPS_NOT_SPECIFIED`]).
//!
//! One QUESTION about the role, several SENTENCES: the recorded refusals word an
//! unassumable role per resource family (the AI services, HyperPod, hosting, and the
//! generic sts:AssumeRole sentence the rest answer with), so each wording is stated here
//! once, beside the one reading they all share, rather than inline in a handler.
//!
//! Each refusal is 400 `ValidationException` — the class recorded SageMaker answers a
//! request it understood and will not accept — carried through
//! [`crate::scaffolded::Scaffolded::validation`], because none of these operations models
//! an error for a bad request.
//!
//! # Only what IAM ANSWERED is refused
//!
//! The role is a resource of ANOTHER service, reached the way any emulated service reaches
//! another: the AWS SDK over the emulator's OWN in-process callback, through the shared
//! reading in `service_common`. The question is not answered from a guess: an emulator with
//! no IAM mounted to ask, and any answer other than "this account holds no such role",
//! refuses NOTHING. It is awaited with NO store guard held.

/// The service principal SageMaker assumes a caller-supplied execution role as, from
/// `CreateNotebookInstanceInput$RoleArn`'s doc: "you must grant the SageMaker AI service
/// principal (sagemaker.amazonaws.com) permissions to assume this role".
pub const SERVICE_PRINCIPAL: &str = "sagemaker.amazonaws.com";

/// How the AutoML creates word a `RoleArn` SageMaker cannot assume: the role ARN AS THE
/// CALLER SENT IT, then the fixed sentence naming what the role has to allow.
///
/// Stated once here so every create of that family words the same fact the same way.
#[must_use]
pub fn could_not_assume_role(role_arn: &str) -> String {
    format!(
        "Could not assume role {role_arn}. Please ensure that the role exists and allows \
         principal '{SERVICE_PRINCIPAL}' to assume the role."
    )
}

/// How the AI-service creates word a `RoleArn` SageMaker cannot assume: a FIXED sentence
/// carrying nothing from the request, as that inner service states the fact.
///
/// Stated once here beside the other wordings so the operations of that family cannot word
/// it differently.
pub const ROLE_CANNOT_BE_ASSUMED: &str = "Unable to assume the provided RoleArn. Ensure the \
                                          role exists and its trust policy allows this \
                                          service to assume it.";

/// How the generic SageMaker create words a role it cannot assume: a FIXED sentence naming
/// the action the role has to allow and the principal it has to allow it for.
pub const UNABLE_TO_PERFORM_ASSUME_ROLE: &str =
    "SageMaker is unable to perform: sts:AssumeRole on the role. Check if you have a trust \
     relationship for 'sagemaker.amazonaws.com' to assume the role.";

/// How HyperPod words an execution role it cannot assume: the role ARN AS THE CALLER SENT
/// IT, then the fixed sentence naming the action and the principal it has to be allowed
/// for.
#[must_use]
pub fn hyperpod_cannot_assume_execution_role(role_arn: &str) -> String {
    format!(
        "SageMaker HyperPod cannot assume the execution role {role_arn}. Please ensure that \
         the execution role allows the action sts:AssumeRole for the service principal \
         {SERVICE_PRINCIPAL}."
    )
}

/// How the hosting creates word an `ExecutionRoleArn` SageMaker cannot assume: the role ARN
/// AS THE CALLER SENT IT, quoted, then the fixed sentence naming what its trust
/// relationship has to allow.
#[must_use]
pub fn execution_role_arn_is_invalid(role_arn: &str) -> String {
    format!(
        "The execution role ARN \"{role_arn}\" is invalid. Please ensure that the role exists \
         and that its trust relationship policy allows the action \"sts:AssumeRole\" for the \
         service principal \"{SERVICE_PRINCIPAL}\"."
    )
}

/// How the hosting creates word an Amazon S3 location whose model artifact Amazon S3 answers
/// is NOT THERE: the location AS THE CALLER SENT IT, and nothing else — there is no
/// permission to check or Region to name when the object does not exist.
#[must_use]
pub fn could_not_find_model_data(s3_url: &str) -> String {
    format!("Could not find model data at {s3_url}.")
}

/// How the hosting creates word an Amazon S3 location whose model artifact SageMaker could
/// not READ AS the execution role: the location AS THE CALLER SENT IT, the role the request
/// supplied, and the Region the request addressed — the three things that decide whether the
/// artifact can be read.
///
/// The condition this states is a GRANT the artifact's own policies withhold, which is a
/// different fact from the object not being there ([`could_not_find_model_data`]): the caller
/// is told what has to hold for the read to be allowed.
#[must_use]
pub fn could_not_access_model_data(s3_url: &str, role_arn: &str, region: &str) -> String {
    format!(
        "Could not access model data at {s3_url}. Please ensure that the role \"{role_arn}\" \
         exists and that its trust relationship policy allows the action \"sts:AssumeRole\" \
         for the service principal \"{SERVICE_PRINCIPAL}\". Also ensure that the role has \
         \"s3:GetObject\" permissions and that the object is located in {region}. If your \
         Model uses multiple models or uncompressed models, please ensure that the role has \
         \"s3:ListBucket\" permission."
    )
}

/// How a DEPLOYMENT reports model data it could not download: the download that failed, the
/// location it was attempted on AS THE MODEL NAMES IT, and the two things that have to hold
/// for it to succeed — an object at that URL, and a role that may read it.
///
/// A different surface from the creates above, and so a different sentence: those REFUSE a
/// call, while this travels on the endpoint as its `FailureReason` once the fleet it was
/// standing up could not come up ("If any of the models hosted at this endpoint get model data
/// from an Amazon S3 location, SageMaker uses Amazon Web Services Security Token Service to
/// download model artifacts from the S3 path you provided", `CreateEndpoint`). One sentence
/// for the whole condition, because it names both of the things that can make the download
/// fail and the deployment reports the download rather than which of them it was.
#[must_use]
pub fn failed_to_download_model_data(s3_url: &str) -> String {
    format!(
        "Failed to download data. Cannot download {s3_url}. Please ensure that there is an \
         object located at the URL and that the role passed to CreateModel has permissions to \
         access the object."
    )
}

/// How a create whose resource's work would READ an Amazon S3 location words one that holds
/// no object at all: the location AS THE CALLER SENT IT, and the two facts that make it name
/// nothing — there is no such location, or there is nothing under it.
///
/// A DIFFERENT fact from a location whose listing was withheld from the role
/// ([`no_s3_objects_found`], whose sentence names the permission the listing needs): here
/// Amazon S3 ANSWERED, and what it answered is that the location holds nothing, so there is no
/// permission to name. `CreateTransformJob` tells its caller the two apart, the way the hosting
/// creates tell "could not find" and "could not access" apart
/// ([`could_not_find_model_data`], [`could_not_access_model_data`]).
#[must_use]
pub fn s3_location_holds_no_objects(s3_url: &str) -> String {
    format!("The S3 location \"{s3_url}\" does not exist or has no objects.")
}

/// How `CreateTrainingJob` words an input data source it found no object under: the S3 URL AS
/// THE CALLER SENT IT, the Region the request addressed, the role it supplied and the bucket
/// that role has to be allowed to list — the three things that can make the location answer
/// nothing.
///
/// One sentence for the location, and a SECOND one for what Amazon S3 itself answered when
/// the listing was refused by AUTHORIZATION rather than merely coming back empty
/// ([`s3_listing_refusal`]): the two conditions are told apart by whether the sentence
/// naming the Amazon S3 answer is there, which is how a client distinguishes "the prefix is
/// empty" from "this role may not list the bucket". A listing that answered and held nothing
/// carries no such clause, because Amazon S3 refused nothing.
#[must_use]
pub fn no_s3_objects_found(
    s3_url: &str,
    region: &str,
    role_arn: &str,
    bucket: &str,
    s3_error: Option<&str>,
) -> String {
    let refused = s3_error.map(|error| format!(" Error message from S3: {error}"));
    format!(
        "No S3 objects found under S3 URL \"{s3_url}\" given in input data source. Please \
         ensure that the bucket exists in the selected region ({region}), that objects exist \
         under that S3 prefix, and that the role \"{role_arn}\" has \"s3:ListBucket\" \
         permissions on bucket \"{bucket}\".{}",
        refused.unwrap_or_default()
    )
}

/// The session name SageMaker assumes an execution role under, which is what the Amazon S3
/// refusal names the calling principal by: an assumed-role session ARN carries the role's own
/// name and then the session's.
pub const EXECUTION_ROLE_SESSION: &str = "SageMaker";

/// What Amazon S3 ANSWERS a listing it refuses on the identity-based question: the principal
/// the call was made by, the action it is not authorized to perform, the resource it was
/// attempted on, and the IAM rule that withheld it.
///
/// The principal is the assumed-role SESSION SageMaker reads the data under — not the role ARN
/// the request carried — because that is the identity the call is signed with
/// (`arn:<partition>:sts::<account>:assumed-role/<role name>/<session name>`), and the
/// resource is the BUCKET, quoted, which is what `s3:ListBucket` is granted on.
///
/// The sentence and its reason clause are the tree's ONE wording of an identity-based refusal
/// ([`::service_common::iam_authorization::not_authorized_by_identity_policy`]), so this states
/// only which principal, action and resource Amazon S3 names. Every variable part is taken from
/// the request and from the role the request named; nothing of any one run is written here.
#[must_use]
pub fn s3_listing_refusal(role_arn: &str, bucket_arn: &str, partition: &str, reason: &str) -> String {
    let parsed = ::service_common::arn::parse(role_arn);
    let account = parsed.map_or("", |role| role.account);
    let role_name = ::service_common::iam_role::role_name(role_arn)
        .unwrap_or_else(|| role_arn.rsplit('/').next().unwrap_or(role_arn));
    let session =
        format!("arn:{partition}:sts::{account}:assumed-role/{role_name}/{EXECUTION_ROLE_SESSION}");
    ::service_common::iam_authorization::not_authorized_by_identity_policy(
        &session,
        crate::state::LIST_BUCKET_ACTION,
        &format!("\"{bucket_arn}\""),
        reason,
    )
}

/// How a resource whose create was ACCEPTED with a role SageMaker cannot assume reports why
/// it never became usable: the `FailureReason` it carries once the clock settles it into its
/// modelled failed status, naming the role the request supplied.
///
/// Not every create resolves the role synchronously: a custom image is accepted and the
/// failure surfaces on the resource, so the sentence is a resource's own reason rather than
/// a refusal of the call.
#[must_use]
pub fn unable_to_assume_role_with_arn(role_arn: &str) -> String {
    format!("Unable to assume role with RoleArn {role_arn}")
}

/// How work that had to be performed AS a role this account holds NOTHING under reports why
/// it could not be: the role ARN AS THE CALLER SENT IT, and the fact that no such role
/// exists — there is no trust document to describe when the role itself is absent.
///
/// The two halves are what the recorded step failure for a role nobody holds states: the
/// ARN the request supplied, and that it does not exist. The ARN is taken from the request,
/// so nothing of any one run is written here.
///
/// A role resolved when the work RUNS rather than when the resource was registered answers
/// on the work, so this is a step's own reason rather than a refusal of a call.
#[must_use]
pub fn role_does_not_exist(role_arn: &str) -> String {
    format!("Role {role_arn} does not exist.")
}

/// Why work performed AS `role_arn` could not be performed at all, as this emulator's own
/// IAM answers about that role — or `None` when IAM answered nothing against it, which is
/// every case the two readings above stay silent about.
///
/// The two answers are worded apart because they are different facts: a role this account
/// holds nothing under does not exist ([`role_does_not_exist`]), while a role it does hold
/// whose trust document withholds [`SERVICE_PRINCIPAL`] exists and cannot be assumed
/// ([`unable_to_assume_role_with_arn`]). Which of the two it is is settled by asking the
/// narrower reading only once the wider one has answered, so a role IAM is content with
/// costs ONE in-process `GetRole` and only the failing path costs a second.
///
/// This is the resolution for the operations AWS does not resolve the role at: the call is
/// accepted and the failure travels on the resource or the run the role was needed for.
pub async fn execution_role_failure_reason(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> Option<String> {
    if !execution_role_is_unassumable(ctx, region, role_arn).await {
        return None;
    }
    if held_execution_role_is_unassumable(ctx, region, role_arn).await {
        return Some(unable_to_assume_role_with_arn(role_arn));
    }
    Some(role_does_not_exist(role_arn))
}

/// The FIRST of `role_arns` this emulator's own IAM answers is unassumable
/// ([`execution_role_is_unassumable`]), or `None` when it answers that about none of them.
///
/// A create that takes SEVERAL roles — one per instance group of a cluster, one per training
/// job definition of a tuning job — settles them in the order the request carries them, so
/// the refusal names the same role whichever other roles the request also names. The roles
/// are resolved one after another and the first answer short-circuits, so a request whose
/// first role is unassumable asks IAM once; a role the request repeats is asked about once,
/// so the questions are bounded by the DISTINCT roles the request names rather than by how
/// many members carry one.
pub async fn first_unassumable_execution_role<'role>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arns: impl IntoIterator<Item = &'role str>,
) -> Option<&'role str> {
    let mut asked = ::std::collections::BTreeSet::new();
    for role_arn in role_arns {
        if !asked.insert(role_arn) {
            continue;
        }
        if execution_role_is_unassumable(ctx, region, role_arn).await {
            return Some(role_arn);
        }
    }
    None
}

/// Whether this emulator's own IAM ANSWERED that `role_arn` is a role SageMaker cannot
/// assume: this account holds no such role, or the trust policy it carries does not admit
/// [`SERVICE_PRINCIPAL`].
///
/// A job RUNS AS the role the request names, so a role SageMaker cannot assume backs no work
/// at all and that create is refused with [`could_not_assume_role`] before anything is
/// stored. The creates the recorded service ANSWERS instead — a notebook instance, a custom
/// image — ask the same question only to decide what the resource then fails with
/// ([`unable_to_assume_role_with_arn`]). The whole question — which role of
/// this account the value names, what IAM answers about it, and whether the trust document
/// admits a service principal — is the ONE reading every emulated service that resolves a
/// caller-supplied execution role shares
/// (`service_common::iam_role::role_is_unassumable`), so this module states only WHICH
/// principal has to be admitted and WHICH operations ask.
pub async fn execution_role_is_unassumable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    ::service_common::iam_role::role_is_unassumable(ctx, region, role_arn, SERVICE_PRINCIPAL).await
}

/// Whether this emulator's own IAM ANSWERED that `role_arn` is a role it HOLDS and
/// SageMaker cannot assume: this account holds a role under that name and the trust policy
/// that stored role carries does not admit [`SERVICE_PRINCIPAL`].
///
/// The same question [`execution_role_is_unassumable`] answers, for the operations whose
/// recorded behaviour SERVES a request naming a role this account's IAM holds nothing
/// under: the hosting creates are recorded registering a resource around a role not held
/// here, and refusing one whose stored trust document admits another service only. Which of
/// the two readings an operation asks is therefore decided by what the recordings show that
/// operation doing, never by taste — and the shared reading
/// (`service_common::iam_role::held_role_is_unassumable`) is the same one every emulated
/// service with that recorded behaviour takes.
pub async fn held_execution_role_is_unassumable(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> bool {
    ::service_common::iam_role::held_role_is_unassumable(ctx, region, role_arn, SERVICE_PRINCIPAL)
        .await
}

/// How SageMaker words a request that places a resource in a VPC subnet and names no
/// security group for it: a FIXED sentence carrying nothing from the request.
///
/// `SecurityGroupIds`' doc: "The VPC security group IDs, in the form sg-xxxxxxxx. The
/// security groups must be for the same VPC as specified in the subnet" — a placement in a
/// subnet is incomplete without one, and the member is modelled optional because the
/// placement itself is, so the pair is this service's own rule.
pub const SECURITY_GROUPS_NOT_SPECIFIED: &str = "Check if the security groups are specified";

/// Why the network placement `subnet_id` / `security_group_ids` asks for is one SageMaker
/// will not accept, or `None` when it is complete.
///
/// A request that names NO subnet asks for no VPC placement at all, so it needs no
/// security group; one that names a subnet needs at least one.
#[must_use]
pub fn vpc_placement_refusal(
    subnet_id: Option<&str>,
    security_group_ids: Option<&[String]>,
) -> Option<&'static str> {
    let placed_in_a_subnet = subnet_id.is_some_and(|subnet| !subnet.is_empty());
    let named_a_group = security_group_ids.is_some_and(|groups| !groups.is_empty());
    (placed_in_a_subnet && !named_a_group).then_some(SECURITY_GROUPS_NOT_SPECIFIED)
}

#[cfg(test)]
mod tests {
    use super::vpc_placement_refusal;

    /// One placement a request can ask for, and whether this service refuses it.
    struct Row {
        case: &'static str,
        subnet: Option<&'static str>,
        groups: Option<Vec<String>>,
        refused: bool,
    }

    /// RULE: a VPC placement is refused exactly when a subnet is named and no security
    /// group is.
    #[test]
    fn a_subnet_placement_needs_a_security_group() {
        let group = || Some(vec!["sg-0123456789abcdef0".to_string()]);
        let rows = vec![
            Row {
                case: "no VPC placement at all",
                subnet: None,
                groups: None,
                refused: false,
            },
            Row {
                case: "a subnet and a group",
                subnet: Some("subnet-1"),
                groups: group(),
                refused: false,
            },
            Row {
                case: "a subnet and no group member",
                subnet: Some("subnet-1"),
                groups: None,
                refused: true,
            },
            Row {
                case: "a subnet and an empty group list",
                subnet: Some("subnet-1"),
                groups: Some(Vec::new()),
                refused: true,
            },
            Row {
                case: "groups but no subnet",
                subnet: None,
                groups: group(),
                refused: false,
            },
            Row {
                case: "an empty subnet id",
                subnet: Some(""),
                groups: None,
                refused: false,
            },
        ];

        for row in rows {
            let refused = vpc_placement_refusal(row.subnet, row.groups.as_deref()).is_some();
            assert!(
                refused == row.refused,
                "case {:?}: expected refused={}, got {refused}",
                row.case,
                row.refused
            );
        }
    }
}
