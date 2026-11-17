// SPDX-License-Identifier: CC0-1.0

//! The IMPORT IDENTIFIERS of a resource type: what a client supplies to identify an EXISTING
//! resource of that type, so a resource already in the account can be brought into a stack.
//!
//! This is what `GetTemplateSummary` reports as
//! `ResourceIdentifierSummary$ResourceIdentifiers` — "the resource properties you can provide
//! during the import to identify your target resources. For example, BucketName is a possible
//! identifier property for an AWS::S3::Bucket resource" — and what the `ResourceIdentifier`
//! map of a `ResourceToImport` is keyed by ("the key is the name of one of the primary
//! identifiers for the resource … the value is the value of that primary identifier").
//!
//! ## What one reported entry is
//!
//! A resource type states its identifiers in its own resource provider schema, and each of them
//! is a SET of properties: `primaryIdentifier` is the properties whose values TOGETHER identify
//! one resource, and each `additionalIdentifiers` entry is another such set — an ALTERNATIVE
//! the type's own service can resolve a resource by. So one reported entry is one SET, which is
//! how a client reads which keys ONE `ResourceToImport` has to carry:
//!
//! * a type identified by a single property reports that property — `BucketName`;
//! * a type a client may identify EITHER way reports one entry per alternative — an Amazon S3
//!   Vectors vector bucket by its ARN or by its name;
//! * a type whose identifier is COMPOSITE reports the parts of it as ONE entry, joined by
//!   [`IDENTIFIER_SET_SEPARATOR`], because none of them identifies a resource on its own: an
//!   AWS Glue table is the catalog, the database and the table name together.
//!
//! They are a property of the TYPE, not of any resource, so they are decided here from the type
//! alone: a summary is given for a template whose resources nothing has created yet.
//!
//! ## The types that report none
//!
//! A type that identifies NO resource outside the stack — a custom resource, whose physical id
//! is its own provider's to mint, a record CloudFormation serves itself
//! ([`super::resource_types::BUILT_IN_RESOURCE_TYPES`]), a wait condition and its handle — has
//! no import identifier at all, and is therefore not summarised: there is no resource of it
//! anywhere for an import to adopt, so a client told to plan one would be planning an import
//! that cannot be made.
//!
//! Nothing here reads or writes stored state: every answer is a function of the type name.

use super::property_validation::declared_properties::DECLARED_PROPERTIES;
use super::template::{self, EC2_INSTANCE, GLUE_TABLE, S3VECTORS_VECTOR_BUCKET, WAIT_CONDITION};

/// What joins the properties of ONE identifier set in the entry reported for it: the parts of a
/// composite identifier are supplied together, so they are reported together.
pub const IDENTIFIER_SET_SEPARATOR: &str = ",";

/// One resource type and the identifier sets an import of a resource of it takes.
#[derive(Debug, Clone, Copy)]
struct ImportIdentifiers {
    /// The resource type the row is about, as a template declares it.
    resource_type: &'static str,
    /// Each set of properties that TOGETHER identify one resource of the type, in the order the
    /// type's schema states them: its primary identifier first, then each additional identifier
    /// it publishes.
    identifier_sets: &'static [&'static [&'static str]],
}

/// The types whose import identifiers are RECORDED, because the identity rules below answer
/// something else for them.
///
/// Source for each row: the type's own resource provider schema — its `primaryIdentifier` and
/// `additionalIdentifiers` — which is what the type's entry in the "Resources that support
/// import operations" table of the CloudFormation User Guide publishes as the identifier(s) an
/// import of one takes, and which is the set of members the owning service's own read of one
/// resource takes.
///
/// A type NOT listed here is answered from the identity this emulator already records for it
/// ([`properties`]), which is the same property for the great majority of types: a bucket by
/// its `BucketName`, a table by its `TableName`, a role by its `RoleName`. A row is needed
/// where SEVERAL properties identify one resource together, where the type's schema publishes
/// an alternative identifier beside its primary one, or where the property the owning service
/// addresses a resource by is spelled unlike the type's own name.
const IMPORT_IDENTIFIERS: &[ImportIdentifiers] = &[
    // An AWS Glue table is addressed by the catalog it is in, the database it belongs to and
    // its own name TOGETHER — the three members `GetTable`, `UpdateTable` and `DeleteTable`
    // each take, none of which names a table on its own — so the three are one identifier set.
    // (The table's own name is `Name`, the member that API takes it as; the type carries it
    // inside its `TableInput`.)
    ImportIdentifiers {
        resource_type: GLUE_TABLE,
        identifier_sets: &[&["CatalogId", "DatabaseName", "Name"]],
    },
    // An Amazon S3 Vectors vector bucket is keyed by its own ARN (`GetVectorBucket` takes
    // `vectorBucketArn`), and that operation resolves one by `vectorBucketName` just as well:
    // the schema states the ARN as the primary identifier and the name as an additional one, so
    // an import takes EITHER — two sets of one property each.
    ImportIdentifiers {
        resource_type: S3VECTORS_VECTOR_BUCKET,
        identifier_sets: &[&["VectorBucketArn"], &["VectorBucketName"]],
    },
    // An Amazon EC2 instance is identified by the `InstanceId` Amazon EC2 minted for it — the
    // handle every Amazon EC2 call on an instance takes — and the type declares no name
    // property at all.
    ImportIdentifiers {
        resource_type: EC2_INSTANCE,
        identifier_sets: &[&["InstanceId"]],
    },
    // An Amazon RDS DB instance is identified by its `DBInstanceIdentifier`, the name every
    // Amazon RDS call on one takes; the type has no `DBInstanceName` whatsoever.
    ImportIdentifiers {
        resource_type: RDS_DB_INSTANCE,
        identifier_sets: &[&["DBInstanceIdentifier"]],
    },
];

/// The Amazon RDS DB instance type, whose identifier the row above records.
const RDS_DB_INSTANCE: &str = "AWS::RDS::DBInstance";

/// What an import may identify one resource of `resource_type` by: one entry per identifier
/// SET, in the order a summary reports them, each entry the set's properties joined by
/// [`IDENTIFIER_SET_SEPARATOR`]; the EMPTY list for a type an import can identify no resource
/// of, which is a type no summary is given for at all.
///
/// The reading, in order:
///
/// 1. a type that identifies no resource outside the stack has none
///    ([`identifies_no_resource_of_its_own`]);
/// 2. the identifier sets the type's schema states, where they are RECORDED
///    ([`IMPORT_IDENTIFIERS`]);
/// 3. the identity this emulator RECORDS for the type as ONE set — the parts of a composite
///    primary identifier, or the published return value that carries the resource's identity
///    ([`super::primary_identifier::recorded_identity`], the one reading of that question);
/// 4. otherwise the property the type takes its NAME from, where the type HAS such a property
///    ([`name_identifier`]), because a physical id that IS the resource's name is the value of
///    that property.
///
/// Step 3 is the record [`super::primary_identifier`] reports a SCANNED resource's identity
/// from and [`super::resource_schema`] declares as a type's `primaryIdentifier`: a resource is
/// identified by the same properties whether it was read or is about to be imported. What this
/// module adds is the import's own two questions — which types an import can identify nothing
/// of, and which ALTERNATIVE identifiers a type accepts beside its primary one.
pub fn properties(resource_type: &str) -> Vec<String> {
    identifier_sets(resource_type)
        .iter()
        .map(|set| set.join(IDENTIFIER_SET_SEPARATOR))
        .collect()
}

/// The identifier sets themselves, each the properties of one set in the order they are
/// reported in ([`properties`] is this reading as a summary states it).
fn identifier_sets(resource_type: &str) -> Vec<Vec<String>> {
    if identifies_no_resource_of_its_own(resource_type) {
        return Vec::new();
    }
    if let Some(row) = IMPORT_IDENTIFIERS
        .iter()
        .find(|row| row.resource_type == resource_type)
    {
        return row
            .identifier_sets
            .iter()
            .map(|set| set.iter().map(|property| (*property).to_string()).collect())
            .collect();
    }
    // The identity this emulator RECORDS for the type, which is what its schema declares as
    // the type's `primaryIdentifier`: one set, because every part of a composite identifier is
    // needed to name one resource.
    let recorded = super::primary_identifier::recorded_identity(resource_type);
    if !recorded.is_empty() {
        return vec![recorded];
    }
    name_identifier(resource_type)
        .map(|property| vec![vec![property]])
        .unwrap_or_default()
}

/// Whether a resource of `resource_type` exists only INSIDE the stack that declares it, so there
/// is no resource of the type anywhere for an import to adopt and the type takes no identifier.
///
/// Three such kinds, and what each of them is:
///
/// * a CUSTOM RESOURCE, whose physical id is "a required custom resource provider-defined
///   physical ID" its own provider mints — no property of the declaration names the resource,
///   which is why the naming rules give one no name property either
///   ([`template::name_property_candidates`]);
/// * a record CloudFormation SERVES ITSELF ([`super::resource_types::BUILT_IN_RESOURCE_TYPES`] —
///   the CDK's version report, whose resource IS the report);
/// * a WAIT CONDITION and its HANDLE, which are a stack's own synchronisation: the handle is the
///   pre-signed write target CloudFormation publishes for the stack, and the condition is the
///   wait on the signals written to it. Neither names a resource in any service, which is why
///   the handle's schema declares no property at all.
fn identifies_no_resource_of_its_own(resource_type: &str) -> bool {
    super::resource_types::is_custom_resource(resource_type)
        || super::resource_types::BUILT_IN_RESOURCE_TYPES.contains(&resource_type)
        || resource_type == WAIT_CONDITION
        || resource_type == super::provisioning::wait_condition_handle::WAIT_CONDITION_HANDLE
}

/// The property a client names one resource of `resource_type` by, for a type whose identity IS
/// its name; `None` for a type that declares no such property.
///
/// The candidates are the properties a resource of the type takes its physical name from
/// ([`template::name_property_candidates`]), most specific first — the type's own name property
/// where that is recorded, then the two conventions, then the Amazon RDS spelling. Which of them
/// the type HAS is decided from its recorded property set ([`DECLARED_PROPERTIES`], the complete
/// `properties` of the types whose schema this emulator carries in full): the identifier is the
/// first candidate that set states, and a type whose set states none of them names no resource
/// by any of them — a client given such a name would be sent at a property the type does not
/// have.
///
/// A type whose property set is NOT recorded is answered by the convention, which is the
/// identity this emulator records for it everywhere else.
fn name_identifier(resource_type: &str) -> Option<String> {
    let candidates = template::name_property_candidates(resource_type);
    match DECLARED_PROPERTIES
        .iter()
        .find(|row| row.resource_type == resource_type)
    {
        Some(row) => candidates
            .into_iter()
            .find(|candidate| row.declared.contains(&candidate.as_str())),
        None => candidates.into_iter().next(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: what a type reports is one entry per identifier SET its own schema states — the
    /// properties of a composite identifier joined into one entry, one entry per alternative a
    /// type publishes beside its primary identifier — and a type that identifies no resource
    /// outside the stack reports none.
    ///
    /// The rows span the catalogue and the rules below it, including types the report that led
    /// here did not cite.
    #[test]
    fn a_types_import_identifiers_are_the_sets_its_schema_states() {
        // (the type, the entries an import of one takes).
        let rows: &[(&str, &[&str])] = &[
            // Recorded rows: a composite identifier (one entry of three properties), an
            // alternative identifier beside the primary one (two entries), and two types whose
            // identifier property is spelled unlike the type's name.
            ("AWS::Glue::Table", &["CatalogId,DatabaseName,Name"]),
            (
                "AWS::S3Vectors::VectorBucket",
                &["VectorBucketArn", "VectorBucketName"],
            ),
            ("AWS::EC2::Instance", &["InstanceId"]),
            ("AWS::RDS::DBInstance", &["DBInstanceIdentifier"]),
            // A binding, identified by every part of what it binds — one entry, because
            // neither part names it on its own.
            ("AWS::WAFv2::WebACLAssociation", &["ResourceArn,WebACLArn"]),
            ("AWS::EKS::AccessEntry", &["ClusterName,PrincipalArn"]),
            // A type whose `Ref` returns a published identity rather than a name.
            ("AWS::SNS::Topic", &["TopicArn"]),
            ("AWS::SQS::Queue", &["QueueUrl"]),
            ("AWS::S3Tables::TableBucket", &["TableBucketARN"]),
            // A type whose recorded property set states which of the name candidates it has: a
            // parameter is identified by its `Name`, not by any `ParameterName`.
            ("AWS::SSM::Parameter", &["Name"]),
            // Types identified by the conventional name property.
            ("AWS::S3::Bucket", &["BucketName"]),
            ("AWS::Logs::LogGroup", &["LogGroupName"]),
            ("AWS::Glue::Database", &["DatabaseName"]),
            // The types that identify no resource outside the stack.
            ("AWS::CloudFormation::WaitConditionHandle", &[]),
            ("AWS::CloudFormation::WaitCondition", &[]),
            ("AWS::CDK::Metadata", &[]),
            ("Custom::Thing", &[]),
            ("AWS::CloudFormation::CustomResource", &[]),
        ];
        for (resource_type, expected) in rows {
            let reported = properties(resource_type);
            assert_eq!(
                reported,
                expected
                    .iter()
                    .map(|entry| (*entry).to_string())
                    .collect::<Vec<String>>(),
                "case {resource_type:?}: expected {expected:?}, got {reported:?}"
            );
        }
    }

    /// Every recorded row names at least one set, every set at least one property, and no set
    /// names a property twice: an entry a client reads is a list of keys it can supply.
    #[test]
    fn every_recorded_identifier_set_names_properties_once() {
        for row in IMPORT_IDENTIFIERS {
            assert!(
                !row.identifier_sets.is_empty(),
                "case {:?}: expected identifier sets, got none",
                row.resource_type
            );
            for set in row.identifier_sets {
                assert!(
                    !set.is_empty() && set.iter().all(|property| !property.is_empty()),
                    "case {:?}: expected named properties, got {set:?}",
                    row.resource_type
                );
                let unique: ::std::collections::BTreeSet<&&str> = set.iter().collect();
                assert_eq!(
                    unique.len(),
                    set.len(),
                    "case {:?}: expected each property once, got {set:?}",
                    row.resource_type
                );
            }
        }
    }
}
