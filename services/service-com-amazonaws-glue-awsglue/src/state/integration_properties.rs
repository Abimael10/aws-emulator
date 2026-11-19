// SPDX-License-Identifier: CC0-1.0

//! How the zero-ETL integration PROPERTY operations read their `ResourceArn`, and the
//! sentences they refuse one with.
//!
//! Six operations address ONE record by the ARN of the resource it belongs to: the three that
//! write, read and remove the processing properties of an integration's resource
//! (`CreateIntegrationResourceProperty`, `GetIntegrationResourceProperty`,
//! `DeleteIntegrationResourceProperty`) and the three that do the same for a replicated
//! table's override properties (`CreateIntegrationTableProperties`,
//! `GetIntegrationTableProperties`, `DeleteIntegrationTableProperties`). The reading of that
//! member and the wording of each refusal live here ONCE, so a refusal one of them makes
//! reads exactly like the one its sibling makes.
//!
//! Two rules order every one of those calls:
//!
//! * a rule decided on the REQUEST BODY alone — the two sides of an integration being
//!   exclusive, a target table name that is no identifier — is answered BEFORE `ResourceArn`
//!   is read at all, so a client whose payload is wrong is told about the payload instead of
//!   about a resource it never named wrongly;
//! * `ResourceArn` is then read in two steps that are reported DIFFERENTLY: a value this
//!   service cannot interpret as one of its resources is a validation failure naming that
//!   value, while a value it can interpret but holds nothing for is a missing record or a
//!   missing target resource.
//!
//! Each operation states the FIRST of those two in its own words — the create writes against
//! the integration's target and says so, the read names the resource it was asked to read, and
//! the delete carries the sentence the integration table property operations carry — so the
//! three sentences live beside each other here rather than one being borrowed for all three.
//!
//! A MISSING record is worded by the resolved ARN KIND, from a vocabulary of two sentences: the
//! miss of the stored RECORD, and the miss of the integration's TARGET RESOURCE. Which kinds
//! earn which is each SURFACE's own, because that is how each of them is recorded — the read of
//! a resource property gives the record sentence to the connection of the source alone
//! ([`read_resource_property_miss`]), while its delete and both table-property surfaces give the
//! target sentence to the Glue database of the target alone
//! ([`removed_resource_property_miss`], [`table_properties_miss`]). Within one surface nothing
//! but the kind chooses, which is what makes the read after a delete and the repeat delete of
//! one ARN agree.
//!
//! The file sits with the rest of this service's state; it is DECLARED from `src/service.rs`
//! (hence its module path), for the reason the declaration of
//! [`crate::service::partition_indexes`] gives there.

use crate::state::{
    arn_fields, integration_resource_reference, integration_table_reference, messages,
    parse_glue_arn, IntegrationTableResource, TaggedResource, TaggedResourceKind,
};

// ---------------------------------------------------------------------------
// The processing properties of an integration's resource
// ---------------------------------------------------------------------------

/// Why an integration resource property call will not take the `ResourceArn` it was given.
///
/// The two refusals are different ERROR CLASSES, which is what a client branches on: one says
/// the value can address nothing and will never work, the other says the resource it
/// addresses is not here — which creating that resource fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceArnRefusal {
    /// The value is no Glue ARN at all — another service's namespace, or not an ARN. This
    /// member is read as the ARN of one of GLUE's own resources, so a value outside that
    /// namespace is one the operation cannot interpret: its modelled `ValidationException`,
    /// naming the value it was handed.
    NotInterpretable(String),
    /// A Glue ARN that names no resource these properties could belong to — "the connection
    /// ARN of the source, or the database ARN of the target"
    /// (`CreateIntegrationResourcePropertyRequest$ResourceArn`). What is missing is the target
    /// resource, so this is the operation's modelled `EntityNotFoundException`.
    NoSuchTarget(String),
}

/// The Glue resource whose processing properties a create writes, or the refusal its
/// `ResourceArn` earns.
///
/// Reads the member in the two steps above: Glue's own namespace first — a value outside it
/// is uninterpretable — and then the resource KIND, since only the two legs of an integration
/// carry these properties. Whether this account HOLDS that resource is a separate answer
/// ([`crate::state::ComAmazonawsGlueAwsglueState::resource_tags`]), as it is for a tagging
/// call's `ResourceArn`.
pub fn resource_property_target(
    resource_arn: &str,
) -> Result<TaggedResource, ResourceArnRefusal> {
    if parse_glue_arn(resource_arn).is_none() {
        return Err(ResourceArnRefusal::NotInterpretable(
            glue_target_arn_invalid(resource_arn),
        ));
    }
    integration_resource_reference(resource_arn).map_err(|_| {
        ResourceArnRefusal::NoSuchTarget(messages::target_resource_not_found(resource_arn))
    })
}

/// Why an UPDATE of an integration resource property will not take the `ResourceArn` it was
/// given.
///
/// The two refusals are different ERROR CLASSES, as they are for the create's reading
/// ([`ResourceArnRefusal`]), but this call draws the line in a different place: it reads the
/// member as an ARN first, so a value that is no ARN at all is refused before any resource is
/// named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatedResourceArnRefusal {
    /// The value is not an ARN at all, so there is nothing to read a resource out of: the
    /// operation's modelled `InvalidInputException`. RECORDED: that refusal carries the code
    /// alone and states no sentence, which is why this variant holds none.
    NotAnArn,
    /// An ARN that names neither leg of an integration — another service's namespace, or a
    /// Glue ARN of a resource type this API does not accept. RECORDED: the operation's
    /// modelled `ValidationException` carrying this service's general reading of that member
    /// ([`invalid_resource_arn`]), the sentence the delete and the three integration table
    /// property calls word it with.
    NamesNoLeg(String),
}

/// The Glue resource whose processing properties an UPDATE writes, or the refusal its
/// `ResourceArn` earns.
///
/// The same member and the same two legs as the create reads
/// ([`resource_property_target`]), but read as an ARN FIRST: RECORDED behaviour of this call
/// is that a value it cannot read as an ARN at all earns one error class and an ARN that
/// names neither leg earns another ([`UpdatedResourceArnRefusal`]), where the create words
/// both as the TARGET it is stating — which is why the two are separate functions over one
/// resolution. Whether this account HOLDS the leg is not asked here at all: this write
/// addresses the record namespace, and a leg with no catalog record still takes a record.
///
/// The ARN form is read through the crate's one split of it ([`arn_fields`]), which says
/// nothing about which service or resource the value names — that is what tells a client
/// whose value could never address anything from one whose value addresses the wrong thing.
pub fn updated_resource_property_target(
    resource_arn: &str,
) -> Result<TaggedResource, UpdatedResourceArnRefusal> {
    if arn_fields(resource_arn).is_none() {
        return Err(UpdatedResourceArnRefusal::NotAnArn);
    }
    integration_resource_reference(resource_arn)
        .map_err(|_| UpdatedResourceArnRefusal::NamesNoLeg(invalid_resource_arn(resource_arn)))
}

/// Whether `resource_arn` is an ARN of one of Glue's OWN resources, which is the whole of
/// what a read or a delete of an integration resource property needs of that member.
///
/// Neither call resolves the resource — the record is keyed by the ARN the create stored it
/// under, and that is the whole lookup — so every Glue ARN is a value a record CAN be held
/// under, and only a value outside Glue's namespace is refused ahead of the lookup, with the
/// operation's modelled `ValidationException`. Each of the two calls words that refusal its
/// own way ([`read_resource_arn_invalid`], [`invalid_resource_arn`]). A Glue ARN this account
/// holds no record for is [`read_resource_property_miss`] for the read and
/// [`removed_resource_property_miss`] for the delete.
pub fn names_a_glue_resource(resource_arn: &str) -> bool {
    parse_glue_arn(resource_arn).is_some()
}

/// The miss a READ of an integration resource property answers for a `ResourceArn` this
/// account holds no record for.
///
/// RECORDED behaviour: two sentences, chosen by the resolved ARN KIND. The SOURCE of an
/// integration is addressed by its Glue connection's ARN, and that miss names the RECORD the
/// lookup did not find ([`resource_property_not_found`]); EVERY OTHER kind this read resolves —
/// the Glue database of the target, and a Data Catalog table's own ARN, which is recorded
/// answering this sentence too — names the target RESOURCE
/// ([`messages::target_resource_not_found`], the sentence this service answers wherever a
/// target does not resolve). Each is a template whose one variable part is the ARN the request
/// addressed, taken from `ResourceArn`.
///
/// The kind SET each sentence covers is this READ's own: the delete of the same record covers a
/// table ARN with the other sentence ([`removed_resource_property_miss`]), which is what each of
/// the two is recorded answering, so the split is per surface and deliberate rather than one of
/// them being wrong.
pub fn read_resource_property_miss(resource_arn: &str) -> String {
    let addresses_the_source = integration_resource_reference(resource_arn)
        .is_ok_and(|resource| matches!(resource.kind, TaggedResourceKind::Connection));
    if addresses_the_source {
        resource_property_not_found(resource_arn)
    } else {
        messages::target_resource_not_found(resource_arn)
    }
}

/// The miss a DELETE of an integration resource property answers for a `ResourceArn` this
/// account holds no record for.
///
/// RECORDED behaviour: the same two sentences the read above chooses between, chosen by the
/// resolved ARN KIND, over a different kind set. The integration's TARGET is addressed by the
/// Glue DATABASE ARN it writes into ([`addresses_the_integration_target`]), and that miss names
/// the target RESOURCE ([`messages::target_resource_not_found`]); every other Glue ARN — the
/// connection ARN of the source, and a Data Catalog table's own ARN, which this surface is
/// recorded answering with the record sentence where the read answers with the target one —
/// earns the miss of the RECORD the delete was asked to remove
/// ([`resource_property_not_found`]). Each is a template whose one variable part is the ARN the
/// request addressed, taken from `ResourceArn`.
///
/// So WITHIN one surface it is the kind and nothing else that chooses, which is what makes the
/// read after a delete and the repeat delete of the same ARN agree; WHICH kinds earn which
/// sentence is each surface's own, as recorded.
pub fn removed_resource_property_miss(resource_arn: &str) -> String {
    if addresses_the_integration_target(resource_arn) {
        messages::target_resource_not_found(resource_arn)
    } else {
        resource_property_not_found(resource_arn)
    }
}

/// Whether `resource_arn` addresses the integration's TARGET: the Glue database an
/// integration replicates INTO.
///
/// "The connection ARN of the source, or the database ARN of the target"
/// (`CreateIntegrationResourcePropertyRequest$ResourceArn`) — so of the ARN kinds these
/// operations resolve, the Glue DATABASE is the target leg, and a refusal that reports the
/// TARGET RESOURCE rather than the record is the one a request addressing that leg earns.
fn addresses_the_integration_target(resource_arn: &str) -> bool {
    integration_resource_reference(resource_arn)
        .is_ok_and(|resource| matches!(resource.kind, TaggedResourceKind::Database))
}

/// The miss an operation that addresses ONE integration TABLE PROPERTY record answers for a
/// `(ResourceArn, TableName)` pair this account holds no record for — the read
/// (`GetIntegrationTableProperties`) and the delete (`DeleteIntegrationTableProperties`), which
/// word it identically.
///
/// RECORDED behaviour: the same choice by resolved ARN kind
/// ([`removed_resource_property_miss`]) — a request addressing the integration's TARGET, the
/// Glue database it replicates into, is answered with the target RESOURCE not being found,
/// while every other ARN kind earns the miss of the stored record, named by the two members
/// the request addressed it by ([`messages::integration_table_properties_not_found`]). The ONE
/// reading of that rule, so the read and the delete of one record cannot disagree about the
/// miss a request earns.
pub fn table_properties_miss(resource_arn: &str, table_name: &str) -> String {
    if addresses_the_integration_target(resource_arn) {
        messages::target_resource_not_found(resource_arn)
    } else {
        messages::integration_table_properties_not_found(resource_arn, table_name)
    }
}

/// The sentence a miss of the RECORD carries: the lookup this surface makes in the
/// per-resource namespace it keeps, named as the service names it.
///
/// RECORDED template: the fixed text around its one variable part, the ARN the request
/// addressed the record by, taken from `ResourceArn`.
fn resource_property_not_found(resource_arn: &str) -> String {
    format!("Failed to find the resource property with arn {resource_arn} in the table")
}

/// The refusal a create carries when ONE request states BOTH sides of the integration.
///
/// The model marks each side optional on its own, so the rule is the service's: "To set both
/// source and target properties the same API needs to be invoked with the Glue connection ARN
/// as ResourceArn with SourceProcessingProperties and the Glue database ARN as ResourceArn
/// with TargetProcessingProperties respectively" (`CreateIntegrationResourceProperty`) — one
/// call writes the properties of the ONE resource its `ResourceArn` names, and no resource is
/// both the source and the target. RECORDED constant: the whole sentence is fixed text naming
/// the two sides in prose, interpolating neither the resource nor a member path.
pub const PROCESSING_PROPERTIES_SIDES_EXCLUSIVE: &str =
    "Both source processing properties and target processing properties cannot be provided in \
     the same request.";

/// The sentence a refusal carries for a `ResourceArn` outside Glue's own namespace when the
/// call is STATING the integration's target: `CreateIntegrationResourceProperty`, which
/// writes the properties of the resource that ARN names.
///
/// RECORDED template: the fixed text around its one variable part, the value the request sent,
/// which the service states as the GLUE TARGET ARN it was asked to write against.
fn glue_target_arn_invalid(resource_arn: &str) -> String {
    format!("The glue target ARN {resource_arn} is invalid.")
}

/// The same refusal as [`glue_target_arn_invalid`] for the call that READS one record,
/// `GetIntegrationResourceProperty`.
///
/// RECORDED template, and a DIFFERENT sentence from the create's: the read is not stating a
/// target, so it names the GLUE RESOURCE ARN it was asked to read, around the same one
/// variable part — the value the request sent.
pub fn read_resource_arn_invalid(resource_arn: &str) -> String {
    format!("The glue resource ARN {resource_arn} is invalid.")
}

/// The sentence a refusal carries for a `ResourceArn` this service can interpret as none of
/// its resources, for the calls of this family that word it as this service's general reading
/// of that member: `DeleteIntegrationResourceProperty` and all three integration table
/// property operations.
///
/// RECORDED template: the fixed text plus its one variable part, the value the request sent in
/// `ResourceArn`.
pub fn invalid_resource_arn(resource_arn: &str) -> String {
    format!("Encountered invalid resource arn {resource_arn}")
}

// ---------------------------------------------------------------------------
// The override properties of a replicated table
// ---------------------------------------------------------------------------

/// The resource an integration table property call's `ResourceArn` names, or the sentence a
/// refusal carries when the operation can interpret the value as none of them.
///
/// One reading for the write, the read and the delete
/// ([`crate::state::integration_table_reference`]), so they cannot disagree about which values
/// address a resource at all. RECORDED behaviour: the refusal is the operation's modelled
/// `ValidationException` carrying [`invalid_resource_arn`].
pub fn table_property_target(resource_arn: &str) -> Result<IntegrationTableResource, String> {
    integration_table_reference(resource_arn).map_err(|_| invalid_resource_arn(resource_arn))
}

/// The refusal a write of integration table properties carries for a target table name that
/// is not a table identifier.
///
/// RECORDED constant, entirely fixed text: it names neither the member it read nor the value
/// it refused.
pub const TARGET_TABLE_NAME_INVALID_CHARACTERS: &str =
    "Target Table Name contains invalid characters.";

/// Whether `name` is a plain table identifier, the form a TARGET table name takes.
///
/// `TargetTableConfig$TargetTableName` is "the optional name of a target table": the name the
/// replicated table is created UNDER on the integration's target, so it carries what a table
/// identifier carries — ASCII letters, digits and `_`. The name of the table the request
/// replicates (`TableName`, "the name of the table to be replicated") is NOT held to this: it
/// names an entity on the SOURCE, which spells its names its own way (the `/` of a SAP
/// `EntityOf` entity), and the model constrains it by length alone.
pub fn is_target_table_identifier(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}
