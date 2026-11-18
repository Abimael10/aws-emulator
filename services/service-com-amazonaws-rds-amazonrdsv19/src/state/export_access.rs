// SPDX-License-Identifier: CC0-1.0

//! The preconditions a snapshot or cluster export is accepted against: the IAM role that
//! writes the objects, and the KMS key they are encrypted under.
//!
//! "The provided IAM role must have access to the S3 bucket" (`StartExportTask`), and the
//! role Amazon RDS assumes to write an export is one that trusts the EXPORT principal
//! `export.rds.amazonaws.com` and carries a policy granting it the calls the export makes on
//! the destination ("Provide access to an Amazon S3 bucket using an IAM role", Exporting DB
//! snapshot data to Amazon S3). The key is "the ID of the Amazon Web Services KMS key to
//! use to encrypt the data exported to Amazon S3" (`KmsKeyId`), and the export cannot run
//! under a key that does not exist or is not enabled.
//!
//! Each question is answered against the service that OWNS the answer — IAM for the role
//! and its policies, Amazon S3 for the destination bucket's policy, KMS for the key — and
//! each refusal is grounded in what that service ANSWERED: an emulator with no IAM, S3 or
//! KMS beside this one establishes nothing, so it refuses nothing.

use super::export_objects::bucket_policy;

/// The IAM action the FIRST call an export makes on its destination is authorized by: before
/// anything is written, the export has to find out which Region the bucket is in, which is
/// the `s3:GetBucketLocation` the role's policy has to grant ("include the following required
/// actions ... s3:GetBucketLocation", `IamRoleArn`).
pub const EXPORT_BUCKET_LOCATION_ACTION: &str = "s3:GetBucketLocation";

/// The IAM action one export object write performs: it writes the exported file into the
/// destination bucket, which is the `s3:PutObject` the role's policy has to grant ("include
/// the following required actions to allow the transfer of files from Amazon RDS or Amazon
/// Aurora to an S3 bucket: s3:PutObject* ...", `IamRoleArn`).
pub const EXPORT_OBJECT_WRITE_ACTION: &str = "s3:PutObject";

/// What is wrong with the role an export was asked to run under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportRoleDefect {
    /// IAM answered that this account holds NO role under the value — it reported none under
    /// that name, or the value is an IAM role ARN of ANOTHER account, whose roles this
    /// emulator's IAM cannot resolve at all. The export's KMS grant is created FOR that role,
    /// so a principal that cannot be resolved
    /// is a grant that cannot be created, and the request is refused as the key fault that
    /// failed grant carries ([`super::export_key_grant_not_created_message`], which records
    /// the answer) rather than as a statement about the role.
    Absent,
    /// IAM holds the role, and the trust policy it carries does not let the export principal
    /// assume it: the modelled `IamRoleNotFoundFault`, which that one sentence reports as the
    /// principal not being allowed to assume the role or the role not existing.
    NotAssumable,
    /// The role is assumable, but the FIRST of the calls the export makes on the destination
    /// that its effective policy does not allow is this one — nothing grants it, or the
    /// destination bucket's own policy denies it: the modelled
    /// `IamRoleMissingPermissionsFault`, whose sentence names the call that failed
    /// ([`super::export_role_missing_permissions_message`]).
    MissingPermissions(&'static str),
}

/// Judge the export role the way Amazon RDS does before it accepts an export: `None` for a
/// role the export can run under, and the defect that stops it otherwise.
///
/// `bucket` is the destination, whose own policy is the other half of every call's decision
/// and whose objects are the resource the object write is authorized on.
///
/// Nothing is refused on an answer this emulator could not get: a value naming no role of
/// this account, or an IAM that is not mounted beside this service, leaves the role unjudged.
/// Policies that could not be enumerated leave the IDENTITY half of each call's decision
/// untaken — they ground no refusal, and they withdraw none either: the destination's own
/// explicit deny is an answer the object store gave, so it still refuses the export. Only
/// what IAM and the object store ANSWERED grounds a defect.
pub async fn export_role_defect<Eng: Send + Sync + 'static>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
    region: &str,
    role_arn: &str,
    bucket: &str,
) -> Option<ExportRoleDefect> {
    // The EXISTENCE of the role is asked first and on its own, because the two answers are
    // reported as different faults: a role this account holds nothing under is a principal
    // the export's KMS grant cannot be created for, while a role it HOLDS that does not
    // trust the export principal is the role's own fault
    // ([`::service_common::iam_role::role_is_absent`] is stated for exactly this pair).
    if ::service_common::iam_role::role_is_absent(ctx, region, role_arn).await
        // A value that is an IAM role ARN of ANOTHER account is the SAME condition: the only
        // IAM this emulator can resolve a role in is this account's, and the export acts
        // under the role the request names rather than under the caller, so no grant can be
        // created for it here either. Answering it identically is also what keeps the KEY's
        // own policy from deciding a request whose principal was never resolved at all.
        || (super::is_iam_role_arn(role_arn)
            && ::service_common::iam_role::role_name(role_arn).is_none())
    {
        return Some(ExportRoleDefect::Absent);
    }
    if ::service_common::iam_role::held_role_is_unassumable(
        ctx,
        region,
        role_arn,
        super::EXPORT_SERVICE_PRINCIPAL,
    )
    .await
    {
        return Some(ExportRoleDefect::NotAssumable);
    }
    // The destination itself, which the bucket-level call is authorized on
    // ("arn:aws:s3:::your-s3-bucket", `IamRoleArn`), and the objects the export writes,
    // which the object write is ("arn:aws:s3:::your-s3-bucket/*").
    let destination = ::service_com_amazonaws_s3_amazons3::state::bucket_arn(bucket);
    let objects = ::service_com_amazonaws_s3_amazons3::state::object_arn(bucket, "*");
    let principals = export_principals(role_arn);
    let context = export_context(role_arn);
    let policy = bucket_policy(engine, bucket);
    // The role's own identity policies: a role carrying none of them can make none of the
    // calls below, which is what the missing-permissions fault reports. A set of documents
    // this emulator could not read establishes NOTHING about what the role may do, so that
    // half of the decision is simply not taken — it never turns into a refusal, and it never
    // withdraws the other half either.
    let documents = role_identity_documents(ctx, region, role_arn).await;
    // Each call the export makes on the destination, IN THE ORDER it makes them: the role is
    // judged call by call and the FIRST one it cannot make is the one reported, because that
    // is the call that failed — the export never gets to the next one.
    for (action, resource) in [
        (EXPORT_BUCKET_LOCATION_ACTION, destination.as_str()),
        (EXPORT_OBJECT_WRITE_ACTION, objects.as_str()),
    ] {
        let request =
            ::service_common::iam_authorization::Request::new(action, resource, &principals)
                .with_context(&context);
        // The bucket's own policy is read first: "an explicit deny in any policy overrides
        // any allow", so a bucket that denies the role this call refuses the export whatever
        // the role carries — and whether or not the role's own documents could be read, since
        // a deny does not depend on them.
        if let Some(policy) = &policy {
            if ::service_common::iam_authorization::resource_decision(policy, &request)
                == ::service_common::iam_authorization::Decision::Denied
            {
                return Some(ExportRoleDefect::MissingPermissions(action));
            }
        }
        if let Some(documents) = &documents {
            if ::service_common::iam_authorization::identity_decision(documents, &request)
                != ::service_common::iam_authorization::Decision::Allowed
            {
                return Some(ExportRoleDefect::MissingPermissions(action));
            }
        }
    }
    None
}

/// Every spelling of the role a resource-based policy may name: the role's own ARN, and the
/// account spellings a DENY of the account refuses each of its principals under
/// ([`::service_common::iam_authorization::account_principals`]).
fn export_principals(role_arn: &str) -> Vec<String> {
    let mut principals = vec![role_arn.to_string()];
    principals.extend(::service_common::iam_authorization::account_principals(
        role_arn,
    ));
    principals
}

/// The condition keys the calls an export makes carry: the export acts UNDER the role the
/// request named, so `aws:PrincipalArn` is that role's ARN — which is what a destination
/// policy that admits or refuses one principal is written over.
fn export_context(role_arn: &str) -> Vec<(String, String)> {
    vec![(PRINCIPAL_ARN_KEY.to_string(), role_arn.to_string())]
}

/// The IAM global condition key naming the identity whose policies are in force: "the ARN
/// of the principal that made the request", which for a request made under an assumed role
/// is that role's own ARN.
const PRINCIPAL_ARN_KEY: &str = "aws:PrincipalArn";

/// Whether the key `key_id`'s own policy withholds either of the two KMS operations the
/// export has to perform on it under the role `role_arn`.
///
/// "The caller of this operation must be authorized to run the following operations. These
/// can be set in the Amazon Web Services KMS key policy: kms:CreateGrant kms:DescribeKey"
/// (`KmsKeyId`), and in KMS "no AWS principal has any permissions to a KMS key unless the
/// key policy allows it" — so the question is what the KEY's own policy says about the role
/// the export runs under, and a policy that delegates to IAM by granting the account (the
/// default key policy) authorizes it.
///
/// WHICH of the two is withheld is not reported, because it is not reported to the caller
/// either: a key the export cannot use is answered with the one sentence the export's key
/// fault carries ([`super::export_kms_key_not_accessible_message`]), which names neither the
/// role nor the action.
///
/// `false` for everything that is not an answer: no KMS beside this service, a key it would
/// not report a policy for, or a document that could not be read. A refusal has to be
/// grounded in what KMS said.
pub async fn export_key_withholds_a_required_action(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    key_id: &str,
    role_arn: &str,
) -> bool {
    let Some(kms) = super::managed_master_secret::kms_client(ctx, region) else {
        return false;
    };
    let Some(policy) = kms
        .get_key_policy()
        .key_id(key_id)
        .send()
        .await
        .ok()
        .and_then(|answer| answer.policy)
        .filter(|document| !document.is_empty())
    else {
        return false;
    };
    let principals = export_principals(role_arn);
    let context = export_context(role_arn);
    EXPORT_KEY_ACTIONS.into_iter().any(|action| {
        let request = ::service_common::iam_authorization::Request::new(
            action,
            // The key itself is the resource those actions are taken on; a key policy
            // names its own key as `"*"`, which covers it.
            key_id,
            &principals,
        )
        .with_context(&context);
        ::service_common::iam_authorization::resource_decision(&policy, &request)
            != ::service_common::iam_authorization::Decision::Allowed
    })
}

/// The KMS operations an export has to be authorized to run on the key its data is
/// encrypted under, as `KmsKeyId` lists them.
const EXPORT_KEY_ACTIONS: [&str; 2] = ["kms:CreateGrant", "kms:DescribeKey"];

/// Every identity-based policy document IAM holds for the role `role_arn` names — the
/// documents of the managed policies attached to it, and its inline policies — or `None`
/// when this emulator could not read them at all (there is no IAM to ask, the value names no
/// role of this account, or an answer could not be read), which grounds no refusal.
///
/// An enumeration that succeeds and reports NO policy is a role that carries none, which is
/// an answer rather than a silence: it is reported as the empty set, and a role with no
/// policy grants nothing.
async fn role_identity_documents(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    role_arn: &str,
) -> Option<Vec<String>> {
    let role_name = ::service_common::iam_role::role_name(role_arn)?;
    let iam = ::service_common::iam_client::iam_client(ctx, region);
    let mut documents = Vec::new();

    let attached = iam
        .list_attached_role_policies()
        .role_name(role_name)
        .send()
        .await
        .ok()?;
    for policy in attached.attached_policies() {
        let policy_arn = policy.policy_arn()?;
        let described = iam.get_policy().policy_arn(policy_arn).send().await.ok()?;
        let version_id = described.policy()?.default_version_id()?;
        let version = iam
            .get_policy_version()
            .policy_arn(policy_arn)
            .version_id(version_id)
            .send()
            .await
            .ok()?;
        documents.push(version.policy_version()?.document()?.to_string());
    }

    let inline = iam
        .list_role_policies()
        .role_name(role_name)
        .send()
        .await
        .ok()?;
    for policy_name in inline.policy_names() {
        let policy = iam
            .get_role_policy()
            .role_name(role_name)
            .policy_name(policy_name)
            .send()
            .await
            .ok()?;
        documents.push(policy.policy_document().to_string());
    }

    Some(documents)
}

/// Whether KMS ANSWERED that the export cannot be encrypted under `key_id`: it holds no
/// such key, or the key it holds is not enabled.
///
/// `false` for everything that is not such an answer — no KMS mounted beside this service,
/// a signing scope that names no Region to ask it in, or any other failure to ask — because
/// a refusal has to be grounded in what KMS said. The client is the crate's ONE KMS client
/// ([`super::managed_master_secret::kms_client`]), which reaches this emulator's own KMS over
/// the in-process callback: no socket is opened and nothing leaves the process.
pub async fn kms_key_is_inaccessible(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    key_id: &str,
) -> bool {
    let Some(kms) = super::managed_master_secret::kms_client(ctx, region) else {
        return false;
    };
    let answer = kms.describe_key().key_id(key_id).send().await;
    match answer {
        // KMS resolved the key: the export can use it unless the key itself is not enabled,
        // which is the other half of what the fault reports.
        Ok(described) => described
            .key_metadata()
            .is_some_and(|metadata| !metadata.enabled()),
        // KMS answered that this account holds no such key, or that the identifier is not
        // one it can resolve: the refusal is grounded in its answer.
        Err(error) => error.as_service_error().is_some_and(|service| {
            matches!(
                service,
                ::aws_sdk_kms::operation::describe_key::DescribeKeyError::NotFoundException(_)
                    | ::aws_sdk_kms::operation::describe_key::DescribeKeyError::InvalidArnException(
                        _
                    )
            )
        }),
    }
}
