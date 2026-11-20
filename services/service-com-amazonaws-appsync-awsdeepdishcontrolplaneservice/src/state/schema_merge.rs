// SPDX-License-Identifier: CC0-1.0

//! The merge of a source API's schema into the Merged API it is associated with, and the
//! withdrawal of that schema when the association goes away.
//!
//! An association's merge is "the merge of the source API's schema into the Merged API"
//! (`StartSchemaMerge`, `SourceApiAssociation$lastSuccessfulMergeDate`), so what a merge
//! that has been PERFORMED leaves behind is the source API's types on the Merged API:
//! that is what makes the Merged API's own schema readable and its types addressable,
//! and a merge that moved only a status would report success while the Merged API held
//! nothing.
//!
//! A Merged API's schema is the composition of the schemas of EVERY source API associated
//! with it, so it is not written type by type as each merge arrives: each association
//! holds the snapshot its last performed merge carried
//! ([`super::SourceApiAssociationRecord::merged_types`]) and the Merged API's own type
//! system is RECOMPOSED from the associations it has, every time that set or one of those
//! snapshots changes ([`ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState::recompose_merged_types`]).
//! Composing rather than overwriting is what lets two source APIs both declare one type
//! name — the root operation types every source API declares its own fields on — and
//! recomposing is what withdraws an association's types when the association is deleted:
//! a schema no source API contributes any longer is a schema the Merged API no longer
//! holds.
//!
//! The merge DEPENDS on the source API's schema: a creation the clock still owes on the
//! source API has not installed its types yet, so a merge scheduled while one is owed is
//! scheduled to come due AFTER it — a composite reaches its terminal state only once what
//! it depends on is itself ready. The dependency is read when the merge is scheduled
//! ([`super::merge_due_instant`]), so the due instant never moves afterwards and schema
//! activity that arrives later cannot leave an association non-terminal for as long as it
//! keeps arriving. That is why the dues are applied together, deletions and creations
//! first
//! ([`super::ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState::settle_schema_creations`]).

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use super::{
    arn_region, combine_type_definitions, type_resource,
    ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState, TypeConflict, TypeDefinition,
    TypeRecord, ARN_SERVICE, DEFAULT_REGION,
};

impl ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState {
    /// Materialise the deletion of every association the CLOCK already made due at `now`.
    ///
    /// Owed by the clock alone: the due instant was recorded by the call that scheduled
    /// the deletion, so the same transition is applied whether or not anybody read the
    /// association in between. An association that is gone contributes nothing to the
    /// Merged API's schema any more, so each Merged API a deletion came due on is
    /// recomposed from the associations it has left — the types that association
    /// contributed leave the Merged API with it.
    pub(super) fn settle_due_disassociations(&mut self, now: SystemTime) {
        let merged_apis: BTreeSet<String> = self
            .source_api_associations
            .values()
            .filter(|association| association.deletion_due(now))
            .map(|association| association.merged_api_id.clone())
            .collect();
        if merged_apis.is_empty() {
            return;
        }
        self.source_api_associations
            .retain(|_, association| !association.deletion_due(now));
        for merged_api_id in merged_apis {
            self.settle_composition(&merged_api_id);
        }
    }

    /// Perform every merge the CLOCK already made due at `now`.
    ///
    /// Owed by the clock alone: the due instant was recorded by the call that scheduled
    /// the merge — including the wait for the source API's own schema creation
    /// ([`super::merge_due_instant`]) — so the same transition is applied whether or not
    /// anybody read either API in between.
    ///
    /// Several merges that came due together are performed in the order they came due
    /// (the order they were scheduled in, for two scheduled at one instant), never in the
    /// order the store happens to be walked in: which merge is performed first decides
    /// which of two declarations that cannot be combined is the one that is not performed,
    /// so it cannot depend on an opaque `associationId`.
    pub(super) fn settle_due_merges(&mut self, now: SystemTime) {
        let mut due: Vec<(SystemTime, u64, String)> = self
            .source_api_associations
            .values()
            .filter_map(|association| {
                association
                    .merge_due_at
                    .filter(|at| now >= *at)
                    .map(|at| (at, association.seq, association.association_id.clone()))
            })
            .collect();
        due.sort();
        for (due_at, _, association_id) in due {
            self.perform_merge(&association_id, due_at);
        }
    }

    /// Carry the source API's schema into the Merged API for the association
    /// `association_id`, whose merge came due at `due_at`, and leave the association in
    /// the status that merge reached.
    ///
    /// The merge takes the source API's type system as it stands at the due instant and
    /// records it as this association's contribution, in the ORDER this merge was
    /// performed in ([`super::SourceApiAssociationRecord::merge_order`]), then recomposes
    /// the Merged API's schema from every contribution it now holds.
    ///
    /// A contribution that cannot be composed with the rest is not installed at all: the
    /// association reports [`super::MERGE_FAILED`] with the conflict, and what its LAST
    /// PERFORMED merge carried goes back as its contribution — a merge that failed carries
    /// no schema, so the Merged API keeps exactly what it held.
    fn perform_merge(&mut self, association_id: &str, due_at: SystemTime) {
        let Some(association) = self.source_api_associations.get(association_id) else {
            return;
        };
        let source_api_id = association.source_api_id.clone();
        let merged_api_id = association.merged_api_id.clone();
        // What this association contributed until now, held onto until the contribution
        // this merge carries is known to compose.
        let performed = (association.merged_types.clone(), association.merge_order);
        let source_types = self
            .graphql_apis
            .get(&source_api_id)
            .map(|api| api.types.clone())
            .unwrap_or_default();
        let order = self.next_seq();

        if let Some(association) = self.source_api_associations.get_mut(association_id) {
            association.merged_types = source_types;
            association.merge_order = order;
            association.merge_due_at = None;
        }

        if !self.settle_composition(&merged_api_id).contains(association_id) {
            if let Some(association) = self.source_api_associations.get_mut(association_id) {
                association.merge_performed(due_at);
            }
            return;
        }
        // Refused: the status the composition left stands, and the schema this association
        // had already merged goes back where it was.
        if performed.0.is_empty() {
            return;
        }
        if let Some(association) = self.source_api_associations.get_mut(association_id) {
            association.merged_types = performed.0;
            association.merge_order = performed.1;
        }
        self.settle_composition(&merged_api_id);
    }

    /// Recompose the Merged API `merged_api_id` and leave every association whose
    /// contribution cannot be composed into its schema in [`super::MERGE_FAILED`], with
    /// what the two declarations disagree about; answer which associations those were.
    ///
    /// ONE recomposition is enough, so this costs one pass however many contributions
    /// disagree: a contribution that does not compose is left out of that same pass, so
    /// what the pass installed already excludes it and clearing it off the refused
    /// association changes nothing the Merged API holds. The Merged API therefore holds
    /// exactly what the merges that were PERFORMED carried, and no association reports a
    /// merge whose schema the Merged API does not hold.
    pub(super) fn settle_composition(&mut self, merged_api_id: &str) -> BTreeSet<String> {
        let conflicts = self.recompose_merged_types(merged_api_id);
        let refused: BTreeSet<String> = conflicts.keys().cloned().collect();
        for (association_id, conflict) in conflicts {
            if let Some(association) = self.source_api_associations.get_mut(&association_id) {
                association.merged_types.clear();
                association.merge_failed(&conflict.conflict());
            }
        }
        refused
    }

    /// Recompose the type system of the Merged API `merged_api_id` from the contributions
    /// of the associations it holds, and report the contributions that could not be
    /// composed, keyed by the association that carries each.
    ///
    /// The Merged API's schema is a FUNCTION of what its source APIs contributed — it is
    /// not accumulated as merges arrive — so this is the one writer of a Merged API's
    /// merged types and the whole account of what it holds:
    ///
    /// * the types the API holds in its own right (the names no merge installed) are the
    ///   baseline every contribution is composed onto;
    /// * each association's contribution is composed in the order its merge was PERFORMED
    ///   ([`super::SourceApiAssociationRecord::merge_order`]), so a type name several
    ///   source APIs declare carries what all of them declare under it
    ///   ([`combine_type_definitions`]), and a declaration that cannot be composed is the
    ///   one the LATEST merge carried rather than whichever association happens to be read
    ///   first;
    /// * a contribution that DISAGREES with what is already composed is left out whole
    ///   and reported, because a schema the Merged API cannot hold is not installed in
    ///   part;
    /// * a name the API already holds keeps its place in the listing and its ARN, and a
    ///   name no association contributes any more is gone — which is how a deleted
    ///   association's types are withdrawn.
    ///
    /// A `merged_api_id` this account does not hold has no schema to compose, which
    /// leaves nothing to do and no conflict to report.
    pub(super) fn recompose_merged_types(
        &mut self,
        merged_api_id: &str,
    ) -> BTreeMap<String, TypeConflict> {
        let Some(merged) = self.graphql_apis.get(merged_api_id) else {
            return BTreeMap::new();
        };
        // The Merged API is reported in the Region it was created in, so a type of it is
        // named under that Region rather than under the source API's.
        let region = arn_region(&merged.arn)
            .unwrap_or(DEFAULT_REGION)
            .to_string();
        // The names the API holds at all, and the baseline the composition starts from:
        // the types it holds in its own right, i.e. the names no merge installed.
        let held: BTreeSet<String> = merged.types.keys().cloned().collect();
        let mut composed: BTreeMap<String, TypeDefinition> = merged
            .types
            .iter()
            .filter(|(name, _)| !merged.merged_type_names.contains(*name))
            .map(|(name, record)| (name.clone(), record.parsed.clone()))
            .collect();
        let baseline: BTreeSet<String> = composed.keys().cloned().collect();

        let mut contributions: Vec<&super::SourceApiAssociationRecord> = self
            .source_api_associations
            .values()
            .filter(|association| {
                association.merged_api_id == merged_api_id && !association.merged_types.is_empty()
            })
            .collect();
        contributions.sort_by_key(|association| (association.merge_order, association.seq));

        let mut conflicts: BTreeMap<String, TypeConflict> = BTreeMap::new();
        for association in contributions {
            // One undo entry per name this contribution writes — the definition that stood
            // there, or nothing — so a contribution that turns out not to compose is rolled
            // back without copying the whole composition: what one recomposition costs
            // stays proportional to the types the contributions declare rather than to
            // their product.
            let mut undo: Vec<(String, Option<TypeDefinition>)> = Vec::new();
            let mut refused: Option<TypeConflict> = None;
            for (name, record) in &association.merged_types {
                let combined = match composed.get(name) {
                    Some(already) => match combine_type_definitions(already, &record.parsed) {
                        Ok(combined) => combined,
                        Err(conflict) => {
                            refused = Some(conflict);
                            break;
                        }
                    },
                    None => record.parsed.clone(),
                };
                let previous = composed.insert(name.clone(), combined);
                undo.push((name.clone(), previous));
            }
            if let Some(conflict) = refused {
                for (name, previous) in undo.into_iter().rev() {
                    match previous {
                        Some(definition) => composed.insert(name, definition),
                        None => composed.remove(&name),
                    };
                }
                conflicts.insert(association.association_id.clone(), conflict);
            }
        }

        // A name the API does not hold yet is a type of it from now on, so it is given its
        // place in the listing and its ARN under the MERGED API's own path — drawn before
        // the records are rewritten, because the id counter and the type map are the same
        // store.
        let mut minted: BTreeMap<String, (u64, String)> = BTreeMap::new();
        for name in composed.keys().filter(|name| !held.contains(*name)) {
            let arn = ::engine::emulator_arn(
                ARN_SERVICE,
                &region,
                &type_resource(merged_api_id, name),
            );
            minted.insert(name.clone(), (self.next_seq(), arn));
        }

        let Some(merged) = self.graphql_apis.get_mut(merged_api_id) else {
            return conflicts;
        };
        // The records the API held, so a name it keeps keeps its own record: its place in
        // the listing, its ARN, and — while the composition left its definition alone —
        // the SDL text that record was written with ([`TypeRecord::definition`] is the text
        // the call that stated it carried).
        let mut previous = std::mem::take(&mut merged.types);
        let mut types: BTreeMap<String, TypeRecord> = BTreeMap::new();
        for (name, parsed) in composed {
            let record = match previous.remove(&name) {
                Some(mut record) => {
                    if record.parsed != parsed {
                        // The composition says something else under this name than the
                        // record did, and no request stated the composed definition, so the
                        // text beside it is the service's own printing of what it now holds.
                        record.definition = parsed.render_sdl();
                        record.parsed = parsed;
                    }
                    record
                }
                // Unreachable: every name the API does not hold was minted above.
                None => match minted.remove(&name) {
                    Some((seq, arn)) => TypeRecord {
                        seq,
                        arn,
                        definition: parsed.render_sdl(),
                        parsed,
                    },
                    None => continue,
                },
            };
            types.insert(name, record);
        }
        merged.merged_type_names = types
            .keys()
            .filter(|name| !baseline.contains(*name))
            .cloned()
            .collect();
        merged.types = types;
        conflicts
    }
}
