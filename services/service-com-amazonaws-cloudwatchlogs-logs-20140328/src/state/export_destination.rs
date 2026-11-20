// SPDX-License-Identifier: CC0-1.0

//! Resolving the Amazon S3 bucket an export task writes to, against the object store that
//! holds it.
//!
//! `CreateExportTask` "creates an export task so that you can efficiently export data from
//! a log group to an Amazon S3 bucket", and its `destination` is "the name of S3 bucket for
//! the exported log data" — a resource of ANOTHER service, which this service therefore
//! resolves where that service holds it rather than assuming. S3 is reached the way the
//! framework documents a handler reaching another emulated service
//! ([`smithy_server_runtime::OperationContext::callback`]): an AWS SDK client over the
//! in-process callback's HTTP client and credentials, so nothing here opens a socket or
//! leaves the host. The question is put with `HeadBucket`, the operation S3 documents for
//! it ("You can use this operation to determine if a bucket exists").
//!
//! Resolving the bucket is TWO questions, because an export needs two things of its
//! destination and the create answers for both before it mints a task id: that the object
//! store holds the bucket at all ([`bucket_is_absent`]), and that the bucket's own policy lets
//! this service make the two calls an export makes on it ([`resolve_destination`]). The second
//! is the setup `CreateExportTask` documents — "you must use credentials that have permission
//! to write to the S3 bucket that you specify as the destination" — and the export establishes
//! it, as the CloudWatch Logs service principal, by READING the destination bucket's ACL and
//! then WRITING its own check object under the destination prefix, which is why those are the
//! two calls resolved here and why a refusal names the one that was withheld. A service
//! principal holds no identity-based policies, so the bucket's own policy is the only document
//! that can grant it either call; the decision itself is the workspace's ONE evaluation of a
//! resource policy ([`service_common::iam_authorization::resource_decision`]), so `Principal`,
//! `Resource`, `Condition` and an explicit `Deny` all count here exactly as they count
//! everywhere else.
//!
//! The condition context this hop carries is stated in one place ([`call_context`]): the three
//! keys a documented destination policy scopes its grant by. The shared evaluation reads a
//! statement conditioned on a key the request carries no value for as granting nothing, so a
//! destination whose grant is conditioned on a key beyond those — a transport or
//! server-side-encryption clause, say — is refused here even where the object store would allow
//! the call; the stated limit is that this hop publishes those keys and no others, and the
//! answer to widening it is to carry the further key here rather than to read the grant more
//! loosely.
//!
//! The one caller-supplied value that reaches the client's AUTHORITY is the signing Region,
//! and it is narrowed before it gets there ([`service_common::arn::region_label`]), so no
//! request can re-point this client at a host of its own choosing: it reaches the emulator's
//! own Amazon S3 and nothing else, whatever bucket a caller names. The client itself is the
//! shared callback client every call to another service of this emulator is made through
//! ([`crate::state::callback_client`]).

use ::service_common::iam_authorization::{resource_decision, Decision, Request};

use super::callback_client::callback_client;

/// Whether Amazon S3 reports that it holds no bucket under `bucket`.
///
/// `false` means the export may be created. That INCLUDES every answer that is not S3
/// reporting the bucket absent — an emulator assembled with no object store to ask, or any
/// other failure to ask it: nothing is then established about the bucket, and a refusal has
/// to be grounded in what S3 answered rather than assumed.
pub async fn bucket_is_absent(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bucket: &str,
) -> bool {
    if bucket.is_empty() {
        return false;
    }
    held_bucket(&s3(ctx, region), bucket).await == Held::No
}

/// What Amazon S3 answered about holding the bucket `bucket` — the ONE statement of that
/// question in this module, so the existence resolution and the whole destination resolution
/// cannot come to differ about which answer means what.
///
/// The question is put with `HeadBucket`, the operation S3 documents for exactly it ("you can
/// use this operation to determine if a bucket exists"), and only S3 ANSWERING that it holds no
/// such bucket is [`Held::No`]: any other failure is this emulator not reaching an object store
/// at all, which establishes nothing — and is worth a warning, because a deployment with no
/// object store to ask is one where this resolution does not run.
async fn held_bucket(client: &::aws_sdk_s3::Client, bucket: &str) -> Held {
    match client.head_bucket().bucket(bucket).send().await {
        Ok(_) => Held::Yes,
        Err(error) if reported_absent(&error) => Held::No,
        Err(error) => {
            unresolved(bucket, &error);
            Held::Unanswered
        }
    }
}

/// What [`held_bucket`] established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Held {
    /// S3 holds the bucket.
    Yes,
    /// S3 answered that it holds no bucket under that name.
    No,
    /// S3 could not be asked, so nothing is established.
    Unanswered,
}

/// The Amazon S3 client this resolution is put through, addressed to the Region the request
/// was signed for.
///
/// Which Region the bucket is looked up in decides nothing here — the emulator serves one
/// object store over the in-process callback — so a scope that carries no Region label falls
/// back to the default rather than putting caller text in the authority. The emulator serves
/// one endpoint for every service, so the bucket travels in the path rather than in a
/// per-bucket host the router could not resolve.
fn s3(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> ::aws_sdk_s3::Client {
    let region = signing_region(region);
    callback_client!(aws_sdk_s3, ctx.callback(), "s3", region, force_path_style(true))
}

/// The Region label a hop to Amazon S3 is signed for: the request's own when it carries one
/// this service can read as a Region, and the default otherwise.
fn signing_region(region: &str) -> &str {
    ::service_common::arn::region_label(region).unwrap_or(crate::state::DEFAULT_REGION)
}

/// What asking Amazon S3 about an export task's destination established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationResolution {
    /// S3 holds the bucket, and the bucket's own policy lets this service make both of the
    /// calls the export makes on it.
    Admitted,
    /// S3 answered, and it holds no bucket under that name.
    BucketAbsent,
    /// S3 holds the bucket, and its policy does not let this service read the bucket's ACL —
    /// which includes a bucket carrying no policy at all, since nothing else can grant a
    /// service principal that read.
    AclReadWithheld,
    /// S3 holds the bucket and its policy admits the ACL read, but not the export's own write
    /// into the destination prefix.
    ObjectWriteWithheld,
    /// Nothing was established: there is no object store to ask (an emulator assembled
    /// without one), or its answer could not be read. A refusal has to be grounded in what S3
    /// answered rather than assumed, so this admits the export.
    Unresolved,
}

/// What Amazon S3 says about exporting a log group's events into the bucket `bucket` under
/// `destination_prefix`.
///
/// Call it with NO store guard held — it awaits. Two questions, each put with the operation
/// S3 documents for it: whether it holds the bucket at all (`HeadBucket`, "you can use this
/// operation to determine if a bucket exists"), and what the bucket's own policy says
/// (`GetBucketPolicy`). `log_group_arn` is the resource the export is made FOR, which is the
/// value of `aws:SourceArn` a grant to this service's principal is scoped by.
pub async fn resolve_destination(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    bucket: &str,
    destination_prefix: &str,
    log_group_arn: &str,
) -> DestinationResolution {
    if bucket.is_empty() {
        return DestinationResolution::Unresolved;
    }
    let client = s3(ctx, region);
    match held_bucket(&client, bucket).await {
        Held::Yes => {}
        Held::No => return DestinationResolution::BucketAbsent,
        Held::Unanswered => return DestinationResolution::Unresolved,
    }
    let policy = match client.get_bucket_policy().bucket(bucket).send().await {
        Ok(answer) => answer.policy().map(str::to_string).unwrap_or_default(),
        // S3 answers `NoSuchBucketPolicy` for a bucket that carries no policy at all, which
        // is an answer to this question: nothing grants the export, and an empty document
        // states exactly that.
        Err(error) if carries_no_policy(&error) => String::new(),
        Err(error) => {
            unresolved(bucket, &error);
            return DestinationResolution::Unresolved;
        }
    };
    match withheld_call(&policy, signing_region(region), bucket, destination_prefix, log_group_arn)
    {
        Some(ExportCall::AclRead) => DestinationResolution::AclReadWithheld,
        Some(ExportCall::ObjectWrite) => DestinationResolution::ObjectWriteWithheld,
        None => DestinationResolution::Admitted,
    }
}

/// The calls an export makes on its destination bucket, in the order it makes them — which is
/// the order a refusal names them in, since the first one withheld is the one reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportCall {
    /// The read of the bucket's own ACL, made on the bucket (`arn:aws:s3:::<bucket>`).
    AclRead,
    /// The write of the export's own check object, made on that object under the task's
    /// destination prefix ([`super::export_worker::WRITE_TEST_OBJECT_NAME`]).
    ObjectWrite,
}

impl ExportCall {
    /// The IAM action this call needs.
    fn action(self) -> &'static str {
        match self {
            Self::AclRead => "s3:GetBucketAcl",
            Self::ObjectWrite => "s3:PutObject",
        }
    }
}

/// The calls an export makes, in the order it makes them.
const EXPORT_CALLS: [ExportCall; 2] = [ExportCall::AclRead, ExportCall::ObjectWrite];

/// The FIRST call of an export that `policy` — the destination bucket's own policy — does not
/// allow this service, or `None` when it allows every one of them.
///
/// The decision is the shared IAM evaluation's; what this supplies is who is asking, on what,
/// and in what context: the principals are every spelling the export is made as
/// ([`export_principals`]), the resource is the one that call is made on
/// ([`call_resource`]), and the context is what a documented destination policy conditions its
/// grant on — the account the export is made in, the log group it is made for, and, for the
/// write, the canned ACL the export writes its objects with. A statement conditioned on any
/// other key cannot be decided, and the shared evaluation reads an undecided statement as
/// granting nothing.
fn withheld_call(
    policy: &str,
    region: &str,
    bucket: &str,
    destination_prefix: &str,
    log_group_arn: &str,
) -> Option<ExportCall> {
    let principals = export_principals(region);
    let account = ::engine::emulator_account_id().to_string();
    EXPORT_CALLS.into_iter().find(|call| {
        let resource = call_resource(*call, bucket, destination_prefix);
        let context = call_context(*call, &account, log_group_arn);
        let request =
            Request::new(call.action(), &resource, &principals).with_context(&context);
        resource_decision(policy, &request) != Decision::Allowed
    })
}

/// The condition keys this hop carries for `call`: the account the export is made in and the
/// log group it is made for, and — for the write — the canned ACL the export writes its objects
/// with. The keys a documented destination policy scopes its grant by, and the whole of what
/// this hop publishes (see the module docs).
fn call_context(call: ExportCall, account: &str, log_group_arn: &str) -> Vec<(String, String)> {
    let mut context = vec![
        (SOURCE_ACCOUNT_KEY.to_string(), account.to_string()),
        (SOURCE_ARN_KEY.to_string(), log_group_arn.to_string()),
    ];
    if call == ExportCall::ObjectWrite {
        context.push((
            OBJECT_ACL_KEY.to_string(),
            BUCKET_OWNER_FULL_CONTROL.to_string(),
        ));
    }
    context
}

/// The ARN of the resource `call` is made on: the bucket itself for the ACL read, and the
/// export's own check object under the destination prefix for the write — the first object an
/// export puts in the bucket, so a grant scoped to the prefix the export writes under covers
/// it.
fn call_resource(call: ExportCall, bucket: &str, destination_prefix: &str) -> String {
    let bucket_arn = format!("arn:aws:{S3_ARN_NAMESPACE}:::{bucket}");
    match call {
        ExportCall::AclRead => bucket_arn,
        ExportCall::ObjectWrite => format!(
            "{bucket_arn}/{}",
            super::export_worker::prefixed_key(
                destination_prefix,
                super::export_worker::WRITE_TEST_OBJECT_NAME
            )
        ),
    }
}

/// The service principal an export's own calls on the destination bucket are made as, in BOTH
/// spellings a destination bucket's policy may name this service under
/// ([`crate::state::SERVICE_PRINCIPAL`]): the REGIONAL one a policy written for one Region's
/// resources names, and the partition-wide one beside it. A document naming either names this
/// service, so both are offered to the shared evaluation — the same pair the other gates that
/// read another service's policy for this one offer ([`super::delivery_role`],
/// [`super::kms_key_grant`]).
fn export_principals(region: &str) -> [String; 2] {
    [
        super::regional_service_principal(region),
        super::subscription_delivery::SERVICE_PRINCIPAL.to_string(),
    ]
}

/// The condition keys a call this service makes as its own service principal carries: the
/// account the export is made in, and the ARN of the log group it is made for — the two keys
/// a destination bucket's policy scopes its grant by. IAM compares a condition key's name
/// case-insensitively, so one spelling of each serves a policy written in any of them.
const SOURCE_ACCOUNT_KEY: &str = "aws:SourceAccount";
/// See [`SOURCE_ACCOUNT_KEY`].
const SOURCE_ARN_KEY: &str = "aws:SourceArn";

/// The canned ACL the export writes each of its objects with, so the objects it puts in the
/// destination belong to that bucket's owner, and the condition key a destination policy
/// states it in.
const BUCKET_OWNER_FULL_CONTROL: &str = "bucket-owner-full-control";
/// See [`BUCKET_OWNER_FULL_CONTROL`].
const OBJECT_ACL_KEY: &str = "s3:x-amz-acl";

/// Amazon S3's ARN vendor: `arn:aws:s3:::<bucket>` IS the bucket, S3 being a service whose
/// ARNs carry neither Region nor account.
const S3_ARN_NAMESPACE: &str = "s3";

/// Whether this refusal of the policy read is Amazon S3 saying the bucket carries NO policy,
/// rather than a failure to ask it at all.
fn carries_no_policy<E: ::aws_sdk_s3::error::ProvideErrorMetadata>(error: &E) -> bool {
    ::aws_sdk_s3::error::ProvideErrorMetadata::code(error) == Some(NO_SUCH_BUCKET_POLICY_CODE)
}

/// The error code Amazon S3 answers a policy read of a bucket that carries no policy with,
/// which its own documentation names.
const NO_SUCH_BUCKET_POLICY_CODE: &str = "NoSuchBucketPolicy";

/// Trace a destination this emulator could not ask Amazon S3 about, which is a deployment
/// with no such service mounted rather than an answer about the bucket.
///
/// The caller's own bucket name is rendered in its escaped `Debug` form, so a bucket name
/// carrying a newline cannot forge a log line.
fn unresolved<E: ::std::error::Error + 'static>(bucket: &str, error: &E) {
    ::tracing::warn!(
        bucket = ?bucket,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(error),
        "an export task's destination bucket could not be resolved against Amazon S3"
    );
}

/// Whether an S3 `HeadBucket` failure is S3 reporting that the bucket does not exist
/// (`NotFound`, the only error that operation models), as opposed to the call not reaching
/// S3 at all.
fn reported_absent(
    error: &::aws_sdk_s3::error::SdkError<
        ::aws_sdk_s3::operation::head_bucket::HeadBucketError,
        ::aws_smithy_runtime_api::client::orchestrator::HttpResponse,
    >,
) -> bool {
    error
        .as_service_error()
        .is_some_and(::aws_sdk_s3::operation::head_bucket::HeadBucketError::is_not_found)
}
