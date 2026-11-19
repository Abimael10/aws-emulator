// SPDX-License-Identifier: CC0-1.0

//! The in-place CHANGE of the stream an `AWS::KinesisFirehose::DeliveryStream` declares:
//! the destination the template now states, applied to the stream the stack already has.
//!
//! The create and delete arms of the type live beside the other data-service ones
//! ([`super::data_and_storage`]); this file holds the update, because applying one is a
//! concern of its own: Amazon Data Firehose changes a stream's destination through
//! `UpdateDestination`, which addresses the destination by the stream's CURRENT version and
//! the id that service gave the destination — neither of which the template states, so both
//! are read off the stream before the change is asked for.
//!
//! ## The rule this carries
//!
//! A destination configuration is not fixed when a Firehose stream is created: "Updates the
//! specified destination of the specified Firehose stream … The target Firehose stream
//! remains active while the configurations are updated" (`UpdateDestination`). So a
//! declaration that states a destination differently — another key `Prefix`, other
//! `BufferingHints`, another compression format, another destination type — is applied to
//! the stream the stack created, which keeps its name (the resource's physical id), its ARN
//! and the source it is fed by, rather than being deployed as a replacement or reported as
//! a change nothing applied.
//!
//! What reaches that call is the declared destination document itself: the type's
//! destination properties ARE `CreateDeliveryStream`'s own members
//! ([`super::data_and_storage::create_delivery_stream`]), and each of those members'
//! structures is the `*DestinationUpdate` of the same destination type member for member —
//! the one difference being the member a NESTED Amazon S3 location is carried under
//! ([`DESTINATION_UPDATES`]).
//!
//! A change to anything else the type declares is NOT one this call can make: the stream's
//! name, its type and the source it reads are what that stream IS, its encryption is turned
//! on and off by two calls of its own, and its tags travel through that service's own
//! tagging calls. Such a change is reported as not applied
//! ([`super::super::provisioning::reconcile`] says so), exactly as it was before this arm
//! existed — never as a success that dropped it.

use super::*;

/// Every destination property of `AWS::KinesisFirehose::DeliveryStream`, with the
/// `UpdateDestination` member its document is submitted at and the members that document
/// carries under ANOTHER name in that member's own structure.
///
/// The property names are the type's own, which are `CreateDeliveryStream`'s members; the
/// update members and the renames are
/// `smithy_query(query="operation",operation="UpdateDestination")` and the `*Update` shapes
/// it names. A nested Amazon S3 location is the one member whose name differs between a
/// destination's configuration and its update — `S3Configuration` against `S3Update`, and
/// an extended destination's backup location `S3BackupConfiguration` against
/// `S3BackupUpdate` — while the Apache Iceberg update keeps `S3Configuration`.
const DESTINATION_UPDATES: &[DestinationUpdate] = &[
    DestinationUpdate {
        property: "S3DestinationConfiguration",
        member: "S3DestinationUpdate",
        renames: &[],
    },
    DestinationUpdate {
        property: "ExtendedS3DestinationConfiguration",
        member: "ExtendedS3DestinationUpdate",
        renames: &[("S3BackupConfiguration", "S3BackupUpdate")],
    },
    DestinationUpdate {
        property: "RedshiftDestinationConfiguration",
        member: "RedshiftDestinationUpdate",
        renames: &[
            ("S3Configuration", "S3Update"),
            ("S3BackupConfiguration", "S3BackupUpdate"),
        ],
    },
    DestinationUpdate {
        property: "ElasticsearchDestinationConfiguration",
        member: "ElasticsearchDestinationUpdate",
        renames: &[("S3Configuration", "S3Update")],
    },
    DestinationUpdate {
        property: "AmazonopensearchserviceDestinationConfiguration",
        member: "AmazonopensearchserviceDestinationUpdate",
        renames: &[("S3Configuration", "S3Update")],
    },
    DestinationUpdate {
        property: "AmazonOpenSearchServerlessDestinationConfiguration",
        member: "AmazonOpenSearchServerlessDestinationUpdate",
        renames: &[("S3Configuration", "S3Update")],
    },
    DestinationUpdate {
        property: "SplunkDestinationConfiguration",
        member: "SplunkDestinationUpdate",
        renames: &[("S3Configuration", "S3Update")],
    },
    DestinationUpdate {
        property: "HttpEndpointDestinationConfiguration",
        member: "HttpEndpointDestinationUpdate",
        renames: &[("S3Configuration", "S3Update")],
    },
    DestinationUpdate {
        property: "SnowflakeDestinationConfiguration",
        member: "SnowflakeDestinationUpdate",
        renames: &[("S3Configuration", "S3Update")],
    },
    DestinationUpdate {
        property: "IcebergDestinationConfiguration",
        member: "IcebergDestinationUpdate",
        renames: &[],
    },
];

/// One destination type's row of [`DESTINATION_UPDATES`].
#[derive(Debug, Clone, Copy)]
struct DestinationUpdate {
    /// The property `AWS::KinesisFirehose::DeliveryStream` declares the destination under,
    /// which is `CreateDeliveryStream`'s own member for it.
    property: &'static str,
    /// The `UpdateDestination` member that destination's document is submitted at.
    member: &'static str,
    /// The members the update's structure carries under another name than the
    /// configuration's does, as `(configuration, update)` pairs.
    renames: &'static [(&'static str, &'static str)],
}

/// Apply a changed declaration to the stream a stack already has.
///
/// `Ok(Some(..))` is the change applied through `UpdateDestination`; the stream keeps the
/// physical id and the ARN it was created with, so nothing is reported moved.
/// `Ok(None)` is a declaration whose change is not a destination configuration at all,
/// which no call of that service makes to a stream that exists.
pub(super) async fn update_delivery_stream(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    resource: &StoredResource,
    declared: &Value,
) -> Result<Option<Created>, Refusal> {
    // Which properties this deployment states differently — the same reading every other
    // in-place arm makes ([`super::iam_identities::changed_properties`]).
    let changed = changed_properties(&resource.properties, declared);
    if changed.is_empty() {
        // The declaration says nothing new about the stream, so there is nothing to ask
        // Amazon Data Firehose: a deployment reaches here for a stack-level change too.
        return Ok(Some(Created::default()));
    }
    if changed
        .iter()
        .any(|property| destination_update(property).is_none())
    {
        return Ok(None);
    }
    // What the NEW declaration states is the configuration the stream is to carry, whether or
    // not the previous one was of the same type. EVERY destination it states is submitted
    // rather than one chosen here: "You must specify only one of the following destination
    // configuration parameters", and a request naming more than one is refused by Amazon Data
    // Firehose itself, with the same sentence it refuses a create naming more than one with.
    let destinations = declared_destinations(declared);
    if destinations.is_empty() {
        return Ok(None);
    }

    // The two values the call addresses the destination by are that service's, not the
    // template's: "Firehose uses CurrentDeliveryStreamVersionId to avoid race conditions and
    // conflicting merges", and the `DestinationId` is the one it gave the destination when
    // the stream was created. Both are read off the stream itself, through the one read of a
    // stream's description this module has ([`super::data_and_storage`]); a stream Firehose
    // no longer has refuses that read, and that refusal is what the deployment reports.
    let description =
        delivery_stream_description(ctx, region, &resource.physical_resource_id).await?;
    let version = description.get("VersionId").and_then(Value::as_str);
    let destination_id = description
        .get("Destinations")
        .and_then(Value::as_array)
        .and_then(|destinations| destinations.first())
        .and_then(|destination| destination.get("DestinationId"))
        .and_then(Value::as_str);
    // A stream reporting neither has no destination this call could address, so the change
    // is reported as not applied rather than asked for with a value invented here.
    let (Some(version), Some(destination_id)) = (version, destination_id) else {
        return Ok(None);
    };

    let mut request = Map::new();
    request.insert(
        "DeliveryStreamName".to_string(),
        Value::String(resource.physical_resource_id.clone()),
    );
    request.insert(
        "CurrentDeliveryStreamVersionId".to_string(),
        Value::String(version.to_string()),
    );
    request.insert(
        "DestinationId".to_string(),
        Value::String(destination_id.to_string()),
    );
    for (destination, document) in destinations {
        request.insert(
            destination.member.to_string(),
            as_destination_update(document, destination.renames),
        );
    }
    call_json_rpc(
        ctx,
        FIREHOSE_SIGNING_NAME,
        region,
        AWS_JSON_1_1,
        &format!("{FIREHOSE_TARGET_PREFIX}.UpdateDestination"),
        &Value::Object(request),
    )
    .await?;
    // The stream is the one that was there: its name, its ARN and the source it reads are
    // untouched, so the record keeps every value it published.
    Ok(Some(Created::default()))
}

/// The [`DESTINATION_UPDATES`] row for one declared PROPERTY, or `None` for a property that
/// is no destination configuration.
fn destination_update(property: &str) -> Option<DestinationUpdate> {
    DESTINATION_UPDATES
        .iter()
        .find(|row| row.property == property)
        .copied()
}

/// Every destination this declaration states: each one's row of [`DESTINATION_UPDATES`], and
/// the declared document itself.
fn declared_destinations(declared: &Value) -> Vec<(DestinationUpdate, &Value)> {
    DESTINATION_UPDATES
        .iter()
        .filter_map(|row| declared.get(row.property).map(|document| (*row, document)))
        .collect()
}

/// One declared destination document as the `*DestinationUpdate` of its type spells it:
/// every member it states, under the name that structure carries it by.
///
/// Only the names differ, and only for a nested Amazon S3 location: the value of each member
/// is what the declaration stated, byte for byte, so the destination the stream ends up with
/// is the one the template declares.
fn as_destination_update(document: &Value, renames: &[(&str, &str)]) -> Value {
    let Some(stated) = document.as_object() else {
        return document.clone();
    };
    let mut update = Map::new();
    for (member, value) in stated {
        let renamed = renames
            .iter()
            .find(|(configuration, _)| *configuration == member.as_str())
            .map(|(_, update_member)| *update_member)
            .unwrap_or(member.as_str());
        update.insert(renamed.to_string(), value.clone());
    }
    Value::Object(update)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{as_destination_update, declared_destinations, destination_update};

    /// Every destination property is submitted at the update member of its own type, and a
    /// property that is no destination has no update member at all.
    ///
    /// The rows span the two Amazon S3 spellings, a destination whose nested location is
    /// renamed, the one whose nested location is NOT, and the properties that are no
    /// destination (the stream's identity, its source, its encryption and its tags).
    #[test]
    fn a_destination_property_names_its_own_update_member() {
        let rows: &[(&str, Option<&str>)] = &[
            ("S3DestinationConfiguration", Some("S3DestinationUpdate")),
            (
                "ExtendedS3DestinationConfiguration",
                Some("ExtendedS3DestinationUpdate"),
            ),
            (
                "SplunkDestinationConfiguration",
                Some("SplunkDestinationUpdate"),
            ),
            (
                "IcebergDestinationConfiguration",
                Some("IcebergDestinationUpdate"),
            ),
            ("DeliveryStreamName", None),
            ("DeliveryStreamType", None),
            ("KinesisStreamSourceConfiguration", None),
            ("DeliveryStreamEncryptionConfigurationInput", None),
            ("Tags", None),
        ];
        for (property, expected) in rows {
            let member = destination_update(property).map(|row| row.member);
            assert_eq!(
                member, *expected,
                "case {property:?}: expected the update member {expected:?}, got {member:?}"
            );
        }
    }

    /// A declared destination document reaches the call with every member it states, under
    /// the name the `*DestinationUpdate` of its type carries that member by — so only a
    /// NESTED Amazon S3 location is renamed, and only where that structure renames it.
    #[test]
    fn a_declared_destination_is_submitted_as_the_update_of_its_type() {
        let s3 = json!({
            "BucketARN": "arn:aws:s3:::bucket",
            "RoleARN": "arn:aws:iam::000000000000:role/delivery",
            "Prefix": "after/",
        });

        let rows: &[(&str, serde_json::Value, serde_json::Value)] = &[
            (
                "a plain Amazon S3 destination, whose members the update spells identically",
                json!({ "S3DestinationConfiguration": s3 }),
                json!({
                    "BucketARN": "arn:aws:s3:::bucket",
                    "RoleARN": "arn:aws:iam::000000000000:role/delivery",
                    "Prefix": "after/",
                }),
            ),
            (
                "an extended destination, whose backup location is renamed",
                json!({
                    "ExtendedS3DestinationConfiguration": {
                        "BucketARN": "arn:aws:s3:::bucket",
                        "S3BackupMode": "Enabled",
                        "S3BackupConfiguration": s3,
                    }
                }),
                json!({
                    "BucketARN": "arn:aws:s3:::bucket",
                    "S3BackupMode": "Enabled",
                    "S3BackupUpdate": s3,
                }),
            ),
            (
                "a Splunk destination, whose nested location is renamed",
                json!({
                    "SplunkDestinationConfiguration": {
                        "HECEndpoint": "https://splunk.example:8088",
                        "S3Configuration": s3,
                    }
                }),
                json!({
                    "HECEndpoint": "https://splunk.example:8088",
                    "S3Update": s3,
                }),
            ),
            (
                "an Apache Iceberg destination, whose update keeps S3Configuration",
                json!({
                    "IcebergDestinationConfiguration": {
                        "RoleARN": "arn:aws:iam::000000000000:role/delivery",
                        "S3Configuration": s3,
                    }
                }),
                json!({
                    "RoleARN": "arn:aws:iam::000000000000:role/delivery",
                    "S3Configuration": s3,
                }),
            ),
        ];

        for (case, declared, expected) in rows {
            let stated = declared_destinations(declared);
            assert_eq!(
                stated.len(),
                1,
                "case {case:?}: the declaration states exactly one destination, got {}",
                stated.len()
            );
            let (destination, document) = stated[0];
            let submitted = as_destination_update(document, destination.renames);
            assert_eq!(
                &submitted, expected,
                "case {case:?}: expected the update document {expected}, got {submitted}"
            );
        }
    }

    /// A declaration stating SEVERAL destinations hands every one of them to
    /// `UpdateDestination`, which is what refuses them — nothing here picks one.
    #[test]
    fn a_declaration_stating_several_destinations_submits_all_of_them() {
        let declared = json!({
            "DeliveryStreamType": "DirectPut",
            "S3DestinationConfiguration": { "BucketARN": "arn:aws:s3:::bucket" },
            "SplunkDestinationConfiguration": { "HECEndpoint": "https://splunk.example:8088" },
        });
        let stated = declared_destinations(&declared);
        let members: Vec<&str> = stated.iter().map(|(row, _)| row.member).collect();
        assert_eq!(
            members,
            vec!["S3DestinationUpdate", "SplunkDestinationUpdate"],
            "expected every declared destination to be submitted, got {members:?}"
        );
    }
}
