// SPDX-License-Identifier: CC0-1.0

//! The resources an environment's configuration POINTS AT in other services, resolved
//! against the services that own them before the configuration is stored.
//!
//! Three members of an environment's configuration are references to resources Amazon MWAA
//! does not own: `ExecutionRoleArn` is "the Amazon Resource Name (ARN) of the execution role
//! in IAM that allows MWAA to access Amazon Web Services resources in your environment",
//! `SourceBucketArn` is "the ARN of the Amazon S3 bucket where your DAG code and supporting
//! files are stored", and `NetworkConfiguration.SecurityGroupIds` is "a list of security
//! group IDs" of the account's VPC. Each member's modelled `@pattern` settles only the SHAPE
//! of such a value — whether the resource it names exists, and whether it can be used the
//! way the configuration asks, is the OWNING service's to answer — so the question is put to
//! that service before an environment is left pointing at it:
//!
//! * the execution role is resolved against this account's IAM through the tree's one
//!   reading of a caller-supplied role
//!   ([`service_common::iam_role::held_role_is_unassumable_by_every`]): a role IAM HOLDS
//!   whose trust policy admits none of the service principals MWAA assumes it under
//!   ([`EXECUTION_ROLE_SERVICE_PRINCIPALS`]) is one this service could never run the
//!   environment as;
//! * the DAG source bucket is resolved against Amazon S3 with `GetBucketLocation`, which
//!   answers both halves of the question in one call: whether the store holds a bucket of
//!   that name at all, and which Region it is in — "the bucket must be in the same Region as
//!   the environment";
//! * the security groups are resolved against Amazon EC2 with `DescribeSecurityGroups`,
//!   selected by the `group-id` filter, so an identifier the account has no group for is an
//!   absent row rather than a failure of the whole read.
//!
//! Every call is made over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), which dispatches to a service
//! registered in THIS process: no socket is opened, no name is resolved, and the only Region
//! text that reaches an endpoint authority is a Region label
//! ([`service_common::arn::region_label`]).
//!
//! Nothing is refused on evidence this emulator does not have. This service keeps only the
//! identifiers a request carried — it reports no VPC, no bucket Region and no role of its
//! own read off those answers — so it GATES on what the owning service said and refuses
//! nothing on silence: an emulator with no IAM, no Amazon S3 or no Amazon EC2 mounted, a hop
//! that did not complete, an answer that could not be read, and a role this account's IAM
//! holds nothing under (an execution role is routinely provisioned out of band) all leave
//! the request served, which is the reading this project's own contract for a resolved
//! reference states ([`service_common::network`]).
//!
//! The question is stated ONCE here for every operation that stores a configuration, so
//! `CreateEnvironment` and `UpdateEnvironment` resolve the three references the same way,
//! with the same grounds and the same refusals. A refused request writes nothing, so the
//! environment is left exactly as it was.

use ::smithy_server_runtime::OperationContext;

/// The service principals an Amazon MWAA execution role's trust policy names: the ones the
/// documented execution-role trust policy of an environment admits
/// (`airflow-env.amazonaws.com` for the environment's own components, and the service's own
/// principal over its ARN namespace [`crate::state::ARN_SERVICE`]).
///
/// A role is unusable only when its trust policy admits NEITHER: each of them names this
/// same service, so a document naming either one is a role MWAA can assume.
pub const EXECUTION_ROLE_SERVICE_PRINCIPALS: [&str; 2] =
    ["airflow-env.amazonaws.com", "airflow.amazonaws.com"];

/// The error code Amazon S3 answers a read of a bucket it does not hold with, and the ONE
/// answer a source bucket is refused on.
///
/// Matched exactly, because it is what tells S3's own answer apart from a failure to ask it:
/// a deployment with no object store mounted answers the read with something that carries no
/// such code, and that establishes nothing about the bucket.
const NO_SUCH_BUCKET_CODE: &str = "NoSuchBucket";

/// The Region a bucket whose location constraint is absent is in: "Buckets in Region
/// us-east-1 have a LocationConstraint of null"
/// (`GetBucketLocationOutput$LocationConstraint`).
const UNCONSTRAINED_BUCKET_REGION: &str = "us-east-1";

/// The `DescribeSecurityGroups` filter that selects groups BY ID (`group-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter rather than `GroupIds`, so an identifier the account has no group for is an
/// absent row in an answer rather than a failure of the whole read: every identifier the
/// request named is still resolved against what EC2 does hold.
const GROUP_ID_FILTER: &str = "group-id";

/// The members of a configuration that name a resource of another service, exactly as ONE
/// request carried them.
///
/// `None` is "this request says nothing about that member": an update that names no
/// execution role leaves the one the environment already runs as, and nothing is resolved
/// for it. A create fills every member it stores.
#[derive(Debug, Default, Clone, Copy)]
pub struct RequestedDependencies<'a> {
    /// `ExecutionRoleArn`, the IAM role the environment runs as.
    pub execution_role_arn: Option<&'a str>,
    /// `NetworkConfiguration.SecurityGroupIds`, the EC2 security groups that guard the
    /// environment's network interfaces.
    pub security_group_ids: Option<&'a [String]>,
    /// `SourceBucketArn`, the Amazon S3 bucket the environment loads its DAG code from.
    pub source_bucket_arn: Option<&'a str>,
}

/// The sentence the request is refused with when a reference it carries is one the owning
/// service answered is unusable, or `None` when every reference it carries resolves — which
/// includes every case where no owning service answered at all (see the module docs).
///
/// The references are resolved in the order the input shapes declare them, so one call
/// reports one fault, deterministically.
///
/// Called with NO store guard held: each of these is a call into another service of this
/// same emulator.
pub async fn unmet_dependency(
    ctx: &OperationContext<'_>,
    region: &str,
    requested: RequestedDependencies<'_>,
) -> Option<String> {
    if let Some(role_arn) = requested.execution_role_arn {
        if ::service_common::iam_role::held_role_is_unassumable_by_every(
            ctx,
            region,
            role_arn,
            &EXECUTION_ROLE_SERVICE_PRINCIPALS,
        )
        .await
        {
            return Some(unassumable_execution_role_message(role_arn));
        }
    }
    if let Some(group_ids) = requested.security_group_ids {
        if let Some(absent) = absent_security_group(ctx, region, group_ids).await {
            return Some(absent_security_group_message(&absent));
        }
    }
    if let Some(bucket_arn) = requested.source_bucket_arn {
        if let Some(refusal) = source_bucket_refusal(ctx, region, bucket_arn).await {
            return Some(refusal);
        }
    }
    None
}

/// What Amazon S3 answered about the bucket `bucket_arn` names, as the sentence this service
/// refuses the request with — or `None` when the store holds that bucket in this Region, and
/// whenever nothing was established about it.
async fn source_bucket_refusal(
    ctx: &OperationContext<'_>,
    region: &str,
    bucket_arn: &str,
) -> Option<String> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    // An `S3BucketArn` carries the bucket name as its whole resource field
    // (`^arn:aws(-[a-z]+)?:s3:::[a-z0-9.\-]+$`), read through the tree's one ARN parse. A
    // value that is no such ARN is the boundary validator's to refuse, not this reading's.
    let bucket = ::service_common::arn::parse_of(bucket_arn, "s3")?.resource;
    if bucket.is_empty() {
        return None;
    }
    // Caller-supplied Region text reaches the endpoint authority of the hop below, so it
    // travels only as a Region label; a request that carries none asks nothing.
    let region = ::service_common::arn::region_label(region)?;
    let answer = ::service_common::s3_client::s3_client(ctx.callback(), region)
        .get_bucket_location()
        .bucket(bucket.to_string())
        .send()
        .await;
    match answer {
        Ok(reported) => {
            let bucket_region = reported
                .location_constraint()
                .map(|constraint| constraint.as_str())
                .filter(|constraint| !constraint.is_empty())
                .unwrap_or(UNCONSTRAINED_BUCKET_REGION);
            (bucket_region != region)
                .then(|| cross_region_source_bucket_message(bucket_arn, bucket_region, region))
        }
        // The store itself says it holds no such bucket: the refusal is grounded in its
        // answer.
        Err(error) if error.code() == Some(NO_SUCH_BUCKET_CODE) => {
            Some(absent_source_bucket_message(bucket_arn))
        }
        // Anything else is this emulator failing to ASK rather than Amazon S3 reporting a
        // bucket it does not hold, so it grounds no refusal — and it is worth a warning,
        // because a deployment with no object store to ask is one where this gate does not
        // run. The caller's own value is rendered in its escaped `Debug` form, so a value
        // carrying a newline cannot forge a log line.
        Err(error) => {
            ::tracing::warn!(
                bucket = ?bucket,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "an environment's source bucket could not be resolved against Amazon S3"
            );
            None
        }
    }
}

/// The first identifier of `group_ids` Amazon EC2 reports NO security group for, in the
/// order the request listed them — or `None` when EC2 reports one for every identifier, and
/// whenever EC2 answered nothing at all.
async fn absent_security_group(
    ctx: &OperationContext<'_>,
    region: &str,
    group_ids: &[String],
) -> Option<String> {
    if group_ids.is_empty() {
        return None;
    }
    // `SecurityGroupList` is `@length(1, 5)`, so the selection one request asks about is
    // bounded by the boundary validator rather than by what the caller sends.
    let filter = ::aws_sdk_ec2::types::Filter::builder()
        .name(GROUP_ID_FILTER)
        .set_values(Some(group_ids.to_vec()))
        .build();
    let answer = ec2_client(ctx, region)?
        .describe_security_groups()
        .filters(filter)
        .send()
        .await;
    match answer {
        Ok(reported) => {
            let held: Vec<&str> = reported
                .security_groups()
                .iter()
                .filter_map(::aws_sdk_ec2::types::SecurityGroup::group_id)
                .collect();
            group_ids
                .iter()
                .find(|group_id| !held.contains(&group_id.as_str()))
                .cloned()
        }
        // No answer, so nothing about the account's security groups was established — see
        // [`source_bucket_refusal`] for why that refuses nothing.
        Err(error) => {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the security groups a request named could not be resolved against Amazon EC2"
            );
            None
        }
    }
}

/// An Amazon EC2 client over the in-process callback into this same emulator, or `None` when
/// the request carries no Region label to address it in.
///
/// The callback is the framework's seam for calling another emulated service: it dispatches
/// in process, so the endpoint URL below is addressing for that transport rather than a host
/// anything connects to — which is also why there is nothing to retry, time out, watch for a
/// stalled stream or cache an identity for.
fn ec2_client(ctx: &OperationContext<'_>, region: &str) -> Option<::aws_sdk_ec2::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// How this service words a configuration naming an execution role it cannot run an
/// environment as: the member at fault and the value the REQUEST carried.
#[must_use]
pub fn unassumable_execution_role_message(role_arn: &str) -> String {
    format!("ExecutionRoleArn {role_arn} is not an IAM role Amazon MWAA can assume.")
}

/// How this service words a configuration naming a DAG source bucket Amazon S3 does not
/// hold.
#[must_use]
pub fn absent_source_bucket_message(bucket_arn: &str) -> String {
    format!("SourceBucketArn {bucket_arn} does not name an Amazon S3 bucket.")
}

/// How this service words a configuration naming a DAG source bucket Amazon S3 holds in
/// ANOTHER Region than the environment's: the member at fault, the value the request
/// carried, the Region S3 reported for the bucket and the Region the environment lives in.
#[must_use]
pub fn cross_region_source_bucket_message(
    bucket_arn: &str,
    bucket_region: &str,
    environment_region: &str,
) -> String {
    format!(
        "SourceBucketArn {bucket_arn} names an Amazon S3 bucket in {bucket_region}. The \
         bucket must be in the same Region as the environment, {environment_region}."
    )
}

/// How this service words a configuration naming a security group Amazon EC2 has none of.
#[must_use]
pub fn absent_security_group_message(group_id: &str) -> String {
    format!(
        "NetworkConfiguration.SecurityGroupIds names the security group {group_id}, which \
         does not exist."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: each refusal names the member at fault and the value the REQUEST carried, so a
    /// caller sees which reference of its configuration could not be resolved.
    ///
    /// The rows differ along which member is at fault and which form its value takes — a role
    /// under a path, a bucket name with a dot, a group id, and a bucket in another Region.
    #[test]
    fn every_refusal_names_the_member_at_fault_and_the_value_the_request_carried() {
        let rows: &[(&str, String, &str, &str)] = &[
            (
                "a role of this account",
                unassumable_execution_role_message("arn:aws:iam::000000000000:role/mwaa"),
                "ExecutionRoleArn",
                "arn:aws:iam::000000000000:role/mwaa",
            ),
            (
                "a role under a path",
                unassumable_execution_role_message(
                    "arn:aws:iam::000000000000:role/service-role/mwaa",
                ),
                "ExecutionRoleArn",
                "arn:aws:iam::000000000000:role/service-role/mwaa",
            ),
            (
                "a bucket name carrying a dot",
                absent_source_bucket_message("arn:aws:s3:::dags.example"),
                "SourceBucketArn",
                "arn:aws:s3:::dags.example",
            ),
            (
                "a bucket in another Region",
                cross_region_source_bucket_message(
                    "arn:aws:s3:::dags",
                    "eu-west-1",
                    "us-east-1",
                ),
                "SourceBucketArn",
                "eu-west-1",
            ),
            (
                "a security group id",
                absent_security_group_message("sg-0a1b2c3d"),
                "NetworkConfiguration.SecurityGroupIds",
                "sg-0a1b2c3d",
            ),
        ];
        for (case, message, member, value) in rows {
            assert!(
                message.contains(member),
                "case {case:?}: expected the refusal to name {member:?}, got {message:?}"
            );
            assert!(
                message.contains(value),
                "case {case:?}: expected the refusal to carry {value:?}, got {message:?}"
            );
        }
    }
}
