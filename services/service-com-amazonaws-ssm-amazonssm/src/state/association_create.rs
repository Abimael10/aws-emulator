// SPDX-License-Identifier: CC0-1.0

//! What creating ONE State Manager association resolves, refuses and persists.
//!
//! Two operations create an association — one at a time (`CreateAssociation`) and in a
//! batch (`CreateAssociationBatch`, whose `Entries` are "one or more associations") — and
//! they create the same thing: the document is resolved against the store, the version it
//! names is selected from that document, the parameter values are held to the ones that
//! version declares, the schedule and the output location have to name a schedule and a
//! bucket, `Targets` and `TargetMaps` cannot both be named, a node already carrying the
//! document's association carries it once, and the Region's association limit still holds.
//! That reading lives HERE once, so the batch creates exactly what the single create does.
//!
//! What differs between the two operations is only how a refusal is REPORTED: a single
//! create answers the modelled error, while a batch reports the entry it could not create
//! in `Failed` ("Information about the associations that failed") beside the ones it did
//! create. [`AssociationCreateRejection`] is that shared refusal, with the description each
//! operation renders.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::AssociationDescription;

use super::associations::{
    AssociationRecord, AssociationVersionRecord, MAX_ASSOCIATIONS_PER_REGION,
    MAX_ASSOCIATIONS_TOTAL,
};
use super::ComAmazonawsSsmAmazonssmState;

/// The `FailedCreateAssociation$Fault` of an entry the REQUEST is at fault for — every
/// refusal below is a property of the entry rather than of the service. From the modelled
/// `Fault` enum (`crate::ops::types::FAULT_VALUES`).
pub const FAULT_CLIENT: &str = "Client";

/// The `FailedCreateAssociation$Fault` of an entry the SERVICE could not take — what is
/// left of that enum's two attributable values for a refusal that is no property of the
/// entry at all (`crate::ops::types::FAULT_VALUES`).
pub const FAULT_SERVER: &str = "Server";

/// How State Manager states that the document an association call named is not one this
/// endpoint holds.
///
/// `InvalidDocument` is "the specified SSM document doesn't exist" (its own documentation),
/// and the sentence the association operations answer with is fixed text FOLLOWED by the
/// name the request carried: `Document does not exist: <name>`. One function, so every call
/// that associates a document — a single create, an entry of a batch and an update — reports
/// a document it cannot resolve in the same words, with nothing but that name interpolated.
pub fn association_document_not_found(name: &str) -> String {
    format!("Document does not exist: {name}")
}

/// How this service states an output location that names no bucket.
/// `InvalidOutputLocation`'s own documentation is "The output location isn't valid or
/// doesn't exist.", and the error itself carries no message member, so this is the
/// description a batch reports for that entry.
pub const INVALID_OUTPUT_LOCATION: &str = "The output location isn't valid or doesn't exist.";

/// How this service states an association that is already there. `AssociationAlreadyExists`'s
/// own documentation is "The specified association already exists.", and it carries no
/// message member either.
pub const ASSOCIATION_ALREADY_EXISTS: &str = "The specified association already exists.";

/// How this service states the documented association limit:
/// `AssociationLimitExceeded`'s own documentation is "You can have at most 2,000 active
/// associations." ([`MAX_ASSOCIATIONS_PER_REGION`]), and it carries no message member.
pub const ASSOCIATION_LIMIT_EXCEEDED: &str = "You can have at most 2,000 active associations.";

/// How many entries ONE create of a batch of associations carries.
///
/// `CreateAssociationBatchRequestEntries` is modelled `len 1..` — a minimum with no
/// maximum — so nothing the model says bounds how many associations one call asks this
/// process to make. This is the emulator's own ceiling, and the documented limit is where
/// it sits: every entry asks for one association and a Region holds at most
/// [`MAX_ASSOCIATIONS_PER_REGION`] of them, so an entry past that many could not be
/// created whatever the store already held. A request carrying more is refused before
/// anything is read or created.
pub const MAX_BATCH_ENTRIES: usize = MAX_ASSOCIATIONS_PER_REGION;

/// How this service states a batch carrying more entries than it serves.
pub fn batch_entry_limit_exceeded() -> String {
    format!("A CreateAssociationBatch request carries at most {MAX_BATCH_ENTRIES} entries")
}

/// How many associations a create may still add, counted ONCE per request.
///
/// The association limit is a COUNT over the store, so counting it again for every entry
/// of a batch would make one call cost the product of its entries and the associations the
/// store holds. The counts are read once
/// ([`ComAmazonawsSsmAmazonssmState::association_capacity`]) and carried through the
/// creates of one request, each of which reports what it added
/// ([`AssociationCapacity::created_one`]) — so every single insertion is still gated by the
/// same two limits, at the cost of one count per request rather than one per entry.
#[derive(Debug, Clone, Copy)]
pub struct AssociationCapacity {
    /// How many associations the Region this request was signed for holds.
    in_region: usize,
    /// How many this store holds, across every Region.
    in_store: usize,
}

impl AssociationCapacity {
    /// Why no further association can be added, or `None` when one can.
    ///
    /// "You can have at most 2,000 active associations" (`AssociationLimitExceeded`) is
    /// measured over the Region; the total is this emulator's own guard
    /// ([`MAX_ASSOCIATIONS_TOTAL`]), which keeps the process finite however many Regions a
    /// caller signs for.
    pub fn rejection(&self) -> Option<AssociationCreateRejection> {
        if self.in_region >= MAX_ASSOCIATIONS_PER_REGION {
            return Some(AssociationCreateRejection::AssociationLimitExceeded);
        }
        if self.in_store >= MAX_ASSOCIATIONS_TOTAL {
            return Some(AssociationCreateRejection::StoreFull);
        }
        None
    }

    /// Record that one association was added, so the next create of the same request is
    /// gated against what this one left.
    fn created_one(&mut self) {
        self.in_region += 1;
        self.in_store += 1;
    }
}

/// Why one association could not be created.
///
/// Every variant is a refusal of the ENTRY: each maps to the modelled error a single
/// create answers with, and carries what that error's description names (the document, the
/// version, the parameter that was wrong), so the same refusal reads alike whether it
/// travels as an error or as a `Failed` entry of a batch.
#[derive(Debug, Clone)]
pub enum AssociationCreateRejection {
    /// "The specified SSM document doesn't exist" (`InvalidDocument`), for the name that
    /// resolved to no document reachable through this endpoint.
    InvalidDocument { name: String },
    /// "The document version isn't valid or doesn't exist" (`InvalidDocumentVersion`), for
    /// a version the resolved document does not hold.
    InvalidDocumentVersion { document: String, version: String },
    /// "You must specify values for all required parameters … You can only supply values to
    /// parameters defined in the SSM document" (`InvalidParameters`).
    InvalidParameters { why: String },
    /// "The schedule is invalid" (`InvalidSchedule`), carrying that error's own wording
    /// ([`super::associations::schedule_rejection`]).
    InvalidSchedule { why: &'static str },
    /// "The output location isn't valid or doesn't exist" (`InvalidOutputLocation`).
    InvalidOutputLocation,
    /// "Both Targets and TargetMaps can't be specified together"
    /// (`CreateAssociationRequest$TargetMaps`), which `InvalidTargetMaps` reports.
    InvalidTargetMaps,
    /// "The specified association already exists" (`AssociationAlreadyExists`).
    AssociationAlreadyExists,
    /// "You can have at most 2,000 active associations" (`AssociationLimitExceeded`),
    /// measured over the Region the request was signed for.
    AssociationLimitExceeded,
    /// This emulator's own total guard across every Region
    /// ([`MAX_ASSOCIATIONS_TOTAL`]) — not a modelled failure of the service, so the
    /// operations refuse it as a bad request rather than as one of the modelled errors.
    StoreFull,
}

impl AssociationCreateRejection {
    /// How this refusal is described: the message member of the modelled error that carries
    /// one, and the `FailedCreateAssociation$Message` ("A description of the failure") of a
    /// batch entry either way.
    pub fn message(&self) -> String {
        match self {
            Self::InvalidDocument { name } => association_document_not_found(name),
            Self::InvalidDocumentVersion { document, version } => {
                format!("Document {document} has no version '{version}'")
            }
            Self::InvalidParameters { why } => why.clone(),
            Self::InvalidSchedule { why } => (*why).to_string(),
            Self::InvalidOutputLocation => INVALID_OUTPUT_LOCATION.to_string(),
            Self::InvalidTargetMaps => super::associations::TARGETS_AND_TARGET_MAPS.to_string(),
            Self::AssociationAlreadyExists => ASSOCIATION_ALREADY_EXISTS.to_string(),
            Self::AssociationLimitExceeded => ASSOCIATION_LIMIT_EXCEEDED.to_string(),
            Self::StoreFull => format!(
                "This emulator holds at most {MAX_ASSOCIATIONS_TOTAL} associations across \
                 every Region"
            ),
        }
    }

    /// "The source of the failure" (`FailedCreateAssociation$Fault`): every refusal above
    /// but the store's own guard is a property of the entry the caller sent, and that one
    /// is a property of this emulator.
    pub fn fault(&self) -> &'static str {
        match self {
            Self::StoreFull => FAULT_SERVER,
            _ => FAULT_CLIENT,
        }
    }
}

/// What ONE create of an association supplies — the same three things whether it arrived on
/// its own or as an entry of a batch.
#[derive(Debug, Clone)]
pub struct AssociationCreate<'a> {
    /// "The managed node ID" a call that named one through the deprecated `InstanceId`
    /// member gives.
    pub instance_id: Option<&'a str>,
    /// The tag set the create persists on the association. A batch entry carries none:
    /// `Tags` is a member of the single create only.
    pub tags: BTreeMap<String, String>,
    /// The FIRST version of the association, holding everything the call set
    /// ([`AssociationVersionRecord`]) — including the document it names, in whichever
    /// spelling the call named it by
    /// ([`AssociationVersionRecord::document_name`]); the create resolves that spelling to
    /// the name the document is held under before anything is persisted.
    pub version: AssociationVersionRecord,
}

impl ComAmazonawsSsmAmazonssmState {
    /// How many associations `region` and this store hold, as the capacity one request's
    /// creates are gated against ([`AssociationCapacity`]). Counted once: this is the only
    /// walk of the store the limits cost, however many associations one call creates.
    pub fn association_capacity(&self, region: &str) -> AssociationCapacity {
        AssociationCapacity {
            in_region: self.associations_in_region_count(region),
            in_store: self.associations.len(),
        }
    }

    /// Create ONE association in `region`, or report why the request names none that can be
    /// created.
    ///
    /// The association is dispatched to the managed nodes its targets select right away,
    /// because "State Manager … applies the configuration when the association is created",
    /// and the run finishes on the clock rather than on this call
    /// ([`super::associations::ASSOCIATION_EXECUTION_DELAY`]).
    ///
    /// Nothing is persisted when the request is refused, so a batch entry that fails leaves
    /// the store as it was for the entries beside it.
    pub fn create_one_association(
        &mut self,
        create: AssociationCreate<'_>,
        capacity: &mut AssociationCapacity,
        region: &str,
        now: SystemTime,
    ) -> Result<AssociationDescription, AssociationCreateRejection> {
        let AssociationCreate {
            instance_id,
            tags,
            mut version,
        } = create;
        // "You can specify Amazon Web Services-predefined documents, documents you created,
        // or a document that is shared with you from another Amazon Web Services account…
        // you must specify the complete SSM document ARN" (`Name`), so either spelling names
        // the same document.
        let name = version.document_name.clone();
        let Some(document) = self.resolve_document_for_request(&name, region, now) else {
            return Err(AssociationCreateRejection::InvalidDocument { name });
        };
        // Whichever spelling named the document, the version applies the document by the
        // name it is held under: `AssociationDescription$Name` is "the name of the SSM
        // document", and the version in force is where the association reads it from.
        version.document_name = document.name.clone();
        // "The document version you want to associate with the targets. Can be a specific
        // version or the default version" (`DocumentVersion`): each names one of the
        // versions the document holds, and a number it does not hold names none. A request
        // that names none associates the document's default version.
        let requested_version = super::non_empty(version.document_version.as_deref())
            .unwrap_or(super::DOCUMENT_VERSION_DEFAULT)
            .to_string();
        let Some(document_version) = document.select(&requested_version) else {
            return Err(AssociationCreateRejection::InvalidDocumentVersion {
                document: document.name.clone(),
                version: requested_version,
            });
        };
        // "You must specify values for all required parameters in the Amazon Web Services
        // Systems Manager document (SSM document). You can only supply values to parameters
        // defined in the SSM document" (`InvalidParameters`) — the one reading of a
        // document's parameters this service has.
        if let Some(why) = super::commands::parameter_rejection(
            &document.name,
            &document_version.parsed.parameters,
            &version.parameters,
        ) {
            return Err(AssociationCreateRejection::InvalidParameters { why });
        }
        // "A cron expression when the association will be applied to the targets"
        // (`ScheduleExpression`): a value in neither of the two forms such an expression is
        // written in names no schedule at all.
        if let Some(expression) = super::non_empty(version.schedule_expression.as_deref()) {
            if let Some(why) = super::associations::schedule_rejection(expression) {
                return Err(AssociationCreateRejection::InvalidSchedule { why });
            }
        }
        // "An Amazon Simple Storage Service (Amazon S3) bucket where you want to store the
        // output details of the request" (`OutputLocation`): a location that names no bucket
        // is no output location.
        if super::associations::names_no_output_bucket(version.output_location.as_ref()) {
            return Err(AssociationCreateRejection::InvalidOutputLocation);
        }
        // "Both Targets and TargetMaps can't be specified together" (`TargetMaps`).
        if !version.targets.is_empty() && !version.target_maps.is_empty() {
            return Err(AssociationCreateRejection::InvalidTargetMaps);
        }
        // "The specified association already exists" (`AssociationAlreadyExists`): a node
        // named through the deprecated `InstanceId` member carries the association of one
        // document once, so a second create of the same pair names the association that is
        // already there.
        let instance_id = super::non_empty(instance_id);
        if let Some(instance_id) = instance_id {
            if self
                .association_of_node(region, &document.name, instance_id)
                .is_some()
            {
                return Err(AssociationCreateRejection::AssociationAlreadyExists);
            }
        }
        // The documented per-Region limit and the store's own total guard, read from the
        // counts this request carries rather than counted again here: EVERY create of a
        // request passes this gate, including each entry of a batch, because the capacity
        // records what the creates before it added.
        if let Some(rejection) = capacity.rejection() {
            return Err(rejection);
        }

        // The association is pinned to the document version this call RESOLVED, which is the
        // default version where the request named none: "Can be a specific version or the
        // default version" (`DocumentVersion`), so what the association holds is the selector
        // it tracks rather than the absence of one, and every read of it reports which version
        // it was pinned to.
        version.document_version = Some(requested_version);
        // The version the association is created with is also the version it is DESCRIBED
        // from, so it is read here rather than selected back out of the record: a create
        // reports the version it minted, never none.
        let first = version.clone();
        let mut record = AssociationRecord::new(
            super::new_uuid(),
            region.to_string(),
            instance_id.map(str::to_string),
            now,
            version,
            tags,
        );
        // An association that selects no registered node reaches none, which is why the
        // nodes are resolved here rather than assumed — and a create that asks for the
        // schedule to be the only trigger dispatches nothing at all
        // ([`super::associations::AssociationRecord::runs_when_applied`]).
        if record.runs_when_applied() {
            let nodes = self.association_run_nodes(&record);
            record.start_run(nodes, now);
        }
        let description = record.to_association_description(&first, now);
        self.insert_association(record);
        capacity.created_one();
        Ok(description)
    }
}
