// SPDX-License-Identifier: CC0-1.0

//! Whether the file system a function's configuration names can be REACHED by that
//! function at all.
//!
//! "Connection settings for an Amazon EFS file system" (`FileSystemConfigs`) names an
//! access point of an Amazon EFS file system (`FileSystemConfig$Arn`, whose pattern admits
//! `arn:aws…:elasticfilesystem:<region>:<account>:access-point/fsap-…` —
//! `smithy_query(query="shape",shape="FileSystemConfig",model_path="services/service-com-amazonaws-lambda-awsgirapiservice/smithy-model/com-amazonaws-lambda-awsgirapiservice.json")`).
//! An EFS file system is reachable only through a mount target inside a VPC — an NFS
//! endpoint with an address in one of that VPC's subnets — so the function that mounts it
//! has to have a network interface in that VPC itself, which is what "The VPC security
//! groups and subnets that are attached to a Lambda function" (`VpcConfig`) asks for.
//!
//! A function with no such configuration therefore describes a mount that could never be
//! made, so the question is settled by the call that WRITES the configuration, before
//! anything is persisted: the customer is told which access point is out of reach and what
//! the configuration is short of, instead of discovering it from an invocation whose
//! handler finds nothing at the mount path.
//!
//! The rule is about the NETWORK the file system sits on, so it is asked of exactly the
//! access points that need one: an Amazon S3 file system's access point
//! (`FileSystemConfig$S3FilesConfig`, "the configuration for how your function accesses
//! data on an Amazon S3 file system") is reached over its own service endpoint rather than
//! through a mount target in the customer's VPC, and needs no attachment.
//!
//! A pure DECISION over the request's own members: it reads no store and changes nothing.
//! The caller refuses with the modelled `InvalidParameterValueException` and persists
//! nothing, which is what makes the refusal and the stored configuration the only two
//! outcomes of such a request.

use crate::ops::types::{FileSystemConfig, VpcConfig};

/// The service part of an ARN that names an Amazon EFS resource, the one whose access
/// points are reached through a mount target in a VPC.
const ELASTIC_FILE_SYSTEM_SERVICE: &str = "elasticfilesystem";

/// The sentence Lambda refuses the pair (`file_system_configs`, `vpc_config`) with, because
/// the configuration mounts an Amazon EFS access point the function has no network path to —
/// or `None` when it mounts none of those, or when the function is attached to a VPC.
///
/// The FIRST such access point is the one the refusal names, since the member admits one
/// entry (`FileSystemConfigList`, `@length(min: 0, max: 1)`) and a request naming more is
/// refused by the constraint walk before this.
#[must_use]
pub fn unreachable_file_system_refusal(
    file_system_configs: Option<&[FileSystemConfig]>,
    vpc_config: Option<&VpcConfig>,
) -> Option<String> {
    if is_vpc_attached(vpc_config) {
        return None;
    }
    let unreachable = file_system_configs?
        .iter()
        .find(|config| names_an_elastic_file_system(&config.arn))?;
    Some(message(&unreachable.arn))
}

/// How Lambda words that refusal: fixed text apart from the access point the function
/// cannot reach, which is the ARN the request itself named.
fn message(access_point_arn: &str) -> String {
    format!(
        "Function must be configured to execute in a VPC to reference access point \
         {access_point_arn}. Please update the function configuration to include VPC subnets \
         and security groups."
    )
}

/// Whether `arn` names a resource of Amazon EFS: the VENDOR field of a well-formed
/// resource ARN, read through the tree's one ARN parse
/// ([`::service_common::arn::vendor`]) rather than by splitting the value again, so this
/// service cannot disagree with another about what an ARN is. The field is not a prefix of
/// the whole string: the partition ahead of it varies by Region.
fn names_an_elastic_file_system(arn: &str) -> bool {
    ::service_common::arn::vendor(arn) == Some(ELASTIC_FILE_SYSTEM_SERVICE)
}

/// Whether `vpc_config` puts the function INSIDE a VPC: it has to name a subnet for the
/// function's network interface to sit in and a security group to govern that interface's
/// traffic, which are the two halves the refusal above asks the caller to supply. "To
/// remove the function's VPC configuration, pass an empty list"
/// (`UpdateFunctionConfiguration`), so an empty list names nothing at all.
fn is_vpc_attached(vpc_config: Option<&VpcConfig>) -> bool {
    let named = |ids: Option<&Vec<String>>| ids.is_some_and(|ids| !ids.is_empty());
    vpc_config.is_some_and(|vpc_config| {
        named(vpc_config.subnet_ids.as_ref()) && named(vpc_config.security_group_ids.as_ref())
    })
}

#[cfg(test)]
mod tests {
    use super::{is_vpc_attached, names_an_elastic_file_system, unreachable_file_system_refusal};
    use crate::ops::types::{FileSystemConfig, VpcConfig};

    /// An access point ARN of Amazon EFS, in `partition`.
    fn efs_access_point(partition: &str) -> String {
        format!("arn:{partition}:elasticfilesystem:us-east-1:000000000000:access-point/fsap-0123456789abcdef0")
    }

    /// An access point ARN of an Amazon S3 file system, the other form the member's pattern
    /// admits.
    fn s3_files_access_point() -> String {
        "arn:aws:s3files:us-east-1:000000000000:file-system/fs-0123456789abcdef0\
         /access-point/fsap-0123456789abcdef0"
            .to_string()
    }

    /// A configuration mounting `arn`.
    fn mounting(arn: &str) -> Vec<FileSystemConfig> {
        vec![FileSystemConfig {
            arn: arn.to_string(),
            local_mount_path: "/mnt/data".to_string(),
            s3_files_config: None,
        }]
    }

    /// A VPC configuration naming `subnets` subnets and `groups` security groups.
    fn vpc(subnets: usize, groups: usize) -> VpcConfig {
        VpcConfig {
            subnet_ids: Some((0..subnets).map(|index| format!("subnet-{index:017x}")).collect()),
            security_group_ids: Some((0..groups).map(|index| format!("sg-{index:017x}")).collect()),
            ipv6_allowed_for_dual_stack: None,
        }
    }

    /// The service field of the ARN decides, in every partition AWS publishes an EFS ARN in,
    /// and the other access point form the member admits is not one of them.
    #[test]
    fn only_an_elastic_file_system_arn_needs_the_attachment() {
        // (case, the ARN, whether it names an Amazon EFS resource)
        let rows: Vec<(String, String, bool)> = vec![
            ("the commercial partition".to_string(), efs_access_point("aws"), true),
            ("the GovCloud partition".to_string(), efs_access_point("aws-us-gov"), true),
            ("an isolated partition".to_string(), efs_access_point("aws-iso-b"), true),
            ("the China partition".to_string(), efs_access_point("aws-cn"), true),
            (
                "an Amazon S3 file system's access point".to_string(),
                s3_files_access_point(),
                false,
            ),
            (
                "a service whose name merely starts the same way".to_string(),
                "arn:aws:elasticfilesystemx:us-east-1:000000000000:access-point/fsap-0123456789abcdef0"
                    .to_string(),
                false,
            ),
            (
                "a bare id rather than an ARN".to_string(),
                "fsap-0123456789abcdef0".to_string(),
                false,
            ),
            (
                // Three fields: too few separators for any service to read the value as an
                // ARN, which is what the shared parse requires of one.
                "the vendor field with no resource behind it".to_string(),
                "arn:aws:elasticfilesystem".to_string(),
                false,
            ),
            (
                "the vendor field left empty".to_string(),
                "arn:aws::us-east-1:000000000000:access-point/fsap-0123456789abcdef0".to_string(),
                false,
            ),
            ("no value at all".to_string(), String::new(), false),
        ];
        for (case, arn, expected) in rows {
            let named = names_an_elastic_file_system(&arn);
            assert_eq!(
                named, expected,
                "case {case:?}: expected it to name Amazon EFS: {expected}, got {named} for \
                 {arn:?}"
            );
        }
    }

    /// A function is inside a VPC only when the configuration names BOTH halves of the
    /// interface it would sit on, however the request spells "none".
    #[test]
    fn only_a_configuration_naming_a_subnet_and_a_group_attaches_the_function() {
        // (case, the configuration, whether the function is inside a VPC)
        let rows: Vec<(&str, Option<VpcConfig>, bool)> = vec![
            ("no configuration at all", None, false),
            ("no member at all", Some(VpcConfig::default()), false),
            ("both lists empty", Some(vpc(0, 0)), false),
            ("subnets but no group", Some(vpc(2, 0)), false),
            ("a group but no subnet", Some(vpc(0, 1)), false),
            ("one subnet and one group", Some(vpc(1, 1)), true),
            ("three subnets and two groups", Some(vpc(3, 2)), true),
        ];
        for (case, vpc_config, expected) in rows {
            let attached = is_vpc_attached(vpc_config.as_ref());
            assert_eq!(
                attached, expected,
                "case {case:?}: expected the function to be inside a VPC: {expected}, got \
                 {attached}"
            );
        }
    }

    /// The decision over the PAIR: an Amazon EFS mount asked for by a function that is not
    /// inside a VPC is the only combination refused, and the sentence names the access point
    /// the request itself gave.
    #[test]
    fn an_efs_mount_is_refused_only_when_the_function_is_outside_a_vpc() {
        let efs = efs_access_point("aws");
        // (case, the mounts, the VPC configuration, whether it is refused)
        let rows: Vec<(&str, Vec<FileSystemConfig>, Option<VpcConfig>, bool)> = vec![
            ("no mount and no VPC", Vec::new(), None, false),
            ("an EFS mount with no VPC configuration", mounting(&efs), None, true),
            (
                "an EFS mount with the configuration removed",
                mounting(&efs),
                Some(vpc(0, 0)),
                true,
            ),
            (
                "an EFS mount with subnets but no group",
                mounting(&efs),
                Some(vpc(1, 0)),
                true,
            ),
            (
                "an EFS mount inside a VPC",
                mounting(&efs),
                Some(vpc(1, 1)),
                false,
            ),
            (
                "an Amazon S3 file system mount with no VPC configuration",
                mounting(&s3_files_access_point()),
                None,
                false,
            ),
        ];
        for (case, configs, vpc_config, refused) in rows {
            let worded = unreachable_file_system_refusal(Some(&configs), vpc_config.as_ref());
            assert_eq!(
                worded.is_some(),
                refused,
                "case {case:?}: expected a refusal: {refused}, got {worded:?}"
            );
            if refused {
                // The sentence written out here rather than taken from the formatter under
                // test, so the WORDING is pinned and not just the ARN it names.
                let expected = format!(
                    "Function must be configured to execute in a VPC to reference access point \
                     {efs}. Please update the function configuration to include VPC subnets and \
                     security groups."
                );
                assert_eq!(
                    worded.as_deref(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the sentence {expected:?}, got {worded:?}"
                );
            }
        }
        // A request carrying no member at all is the same as one carrying no mount.
        assert_eq!(
            unreachable_file_system_refusal(None, None),
            None,
            "a request naming no file system mounts nothing, so nothing is refused"
        );
    }
}
