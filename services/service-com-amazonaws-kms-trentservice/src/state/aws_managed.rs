// SPDX-License-Identifier: CC0-1.0

//! What an Amazon Web Services managed key carries that the model does not type: the
//! default-key `Description` KMS gives the key behind a predefined `alias/aws/<service>`
//! alias.
//!
//! One concern of [`super`]'s store, in its own file: the helpers here are reached as
//! `crate::state::…` exactly as they were before, because the parent re-exports them.
//!
//! KMS seeds every Amazon Web Services managed key with one fixed sentence — "Default
//! key that protects my <resources> when no other key is defined" — whose only variable
//! part names the resources the service owning the alias protects with it. The sentence
//! is the same for every service; what differs is that noun phrase, so it is the only
//! thing resolved per alias here.

/// The fixed sentence every Amazon Web Services managed key's description is, with
/// `{resources}` standing for the resources the owning service protects with it.
fn default_key_description(resources: &str) -> String {
    format!("Default key that protects my {resources} when no other key is defined")
}

/// The `Description` of the Amazon Web Services managed key behind the predefined
/// alias `alias_name` (`alias/aws/<service-name>`).
///
/// The service-name segment of the alias is what selects the noun phrase: a service
/// whose wording [`RESOURCES_BY_SERVICE`] records gets that wording, and every other
/// service gets the segment's own name followed by `data`, which is how KMS words a
/// managed key whose service has no resource noun of its own (`aws/backup` →
/// "Backup data").
///
/// An alias that is not a predefined Amazon Web Services alias has no managed key, so
/// there is no description for it.
pub fn aws_managed_key_description(alias_name: &str) -> Option<String> {
    if !super::is_predefined_aws_alias(alias_name) {
        return None;
    }
    let service_name = alias_name.strip_prefix(super::ALIAS_PREFIX_AWS_MANAGED)?;
    Some(default_key_description(&resources_protected_by(
        service_name,
    )))
}

/// The noun phrase naming what the service behind `alias/aws/<service_name>` protects
/// with its managed key.
fn resources_protected_by(service_name: &str) -> String {
    if let Some((_, resources)) = RESOURCES_BY_SERVICE
        .iter()
        .find(|(service, _)| *service == service_name)
    {
        return (*resources).to_string();
    }
    format!("{} data", capitalized(service_name))
}

/// `service_name` with its first character upper-cased, which is how the fallback
/// renders a service that names no resource of its own.
fn capitalized(service_name: &str) -> String {
    let mut chars = service_name.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

/// The services whose managed key names a resource of its own rather than the
/// service's `data`, keyed by the service-name segment of its predefined alias.
///
/// The Smithy model types `KeyMetadata.Description` and documents no value for it
/// (`smithy_query(query="doc",shape="KeyMetadata",member="Description")`), so this is
/// the wording KMS is observed to seed each managed key with, as the Key Management
/// Service Developer Guide's default-key descriptions show it. A service absent here
/// falls back to the `<Service> data` form.
const RESOURCES_BY_SERVICE: &[(&str, &str)] = &[
    ("dynamodb", "DynamoDB tables"),
    ("ebs", "EBS volumes"),
    ("elasticfilesystem", "EFS filesystems"),
    ("lambda", "Lambda functions"),
    ("rds", "RDS database volumes"),
    ("redshift", "Redshift clusters"),
    ("s3", "S3 objects"),
    ("secretsmanager", "Secrets Manager secrets"),
    ("sqs", "SQS queues"),
];
