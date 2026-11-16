// SPDX-License-Identifier: CC0-1.0

//! What an accepted asset-model write SETTLES at, and which state a model has to
//! be in before a write to it is accepted at all.
//!
//! # The rule
//!
//! IoT SiteWise decides an asset-model write on two planes:
//!
//! * the SYNCHRONOUS plane answers the call — the request's own faults (a member
//!   outside its constraints, a quota, a name two definitions share, a formula
//!   that does not parse, a reference spelled in a form its scope does not accept,
//!   a resource the request names that does not exist) and the state the model is
//!   in are refused there, with `InvalidRequestException` and the rest of the
//!   operation's modelled errors;
//! * the ASYNCHRONOUS plane settles the model. A fault that is about the MODEL
//!   GRAPH — what another asset model exposes, and whether the hierarchies lead
//!   back to the model being written — does not refuse the call: the write is
//!   accepted, the model reports `CREATING` / `UPDATING` / `DELETING`, and when
//!   the transition that write owes falls due the model settles at `FAILED`
//!   carrying `assetModelStatus.error` = `ErrorDetails{VALIDATION_ERROR, <the rule
//!   that fired>}` rather than at `ACTIVE`.
//!
//! A `FAILED` model is a stored model like any other: it stays in the listing, it
//! stays describable with the definition the failed write stated, and a write that
//! states a definition the graph accepts moves it back to `ACTIVE` — which is why
//! the diagnosis is recorded on the record ([`AssetModelRecord::settle_failure`])
//! by the call that was accepted, rather than re-derived by a read.
//!
//! This module holds that plane: which definition settles at `FAILED` and with
//! which sentence, the delete that settles there instead of removing the model,
//! and the state gate every modification of a model is held to first.

use super::*;

/// The `ErrorCode` a settle-time diagnosis of a definition is reported under:
/// the request was understood and the definition is not one the service can
/// apply (`ErrorCode` is `[INTERNAL_FAILURE, VALIDATION_ERROR]`).
pub const VALIDATION_ERROR_CODE: &str = "VALIDATION_ERROR";

/// The diagnosis a formula variable reaching through a hierarchy at a property
/// the child asset model does not expose settles the model at.
///
/// Entirely fixed text: the sentence names no id, member or value of the request.
pub const MISSING_CHILD_PROPERTY_FAILURE: &str =
    "A formula property references a child asset model property that doesn't exist.";

/// The diagnosis a METRIC whose variable reaches through a hierarchy at a
/// property of the child model that is not itself aggregated settles the model
/// at: a metric maps "several values to a single value", so what it consumes from
/// the assets under a hierarchy is the child model's own metric, not a raw
/// measurement of it.
///
/// Entirely fixed text, like its sibling above: two sentences — the rule that
/// fired, then what a metric MAY consume — and no id, member or value of the
/// request. Distinct from [`MISSING_CHILD_PROPERTY_FAILURE`], which diagnoses a
/// reference that does not resolve at all.
pub const NON_METRIC_CHILD_PROPERTY_FAILURE: &str =
    "A metric property references a non-metric property from a child model. \
     Metrics can reference properties from the current model and metric properties \
     from child models.";

/// The diagnosis a hierarchy graph that leads back to the model being written
/// settles the model at.
///
/// Entirely fixed text, like its siblings above.
pub const CYCLICAL_HIERARCHY_FAILURE: &str = "A cyclical hierarchy definition exists. \
     Asset models can't contain cyclical hierarchy references.";

/// The diagnosis a delete of a model another model's hierarchy still names as its
/// child settles that model at. The model is NOT removed: the caller has to drop
/// the hierarchy reference and delete it again.
///
/// Entirely fixed text, like its siblings above.
pub const REFERENCED_BY_PARENT_HIERARCHY_FAILURE: &str =
    "Attempted to delete an asset model that is referenced by a hierarchy in a parent asset \
     model. You must remove hierarchy references before you can delete the asset model.";

/// The `ErrorDetails` a settle-time diagnosis is reported as: the validation
/// code, and the sentence naming the rule that fired.
///
/// ONE place, so every write that settles a model at `FAILED` reports the same
/// shape of diagnosis.
#[must_use]
pub fn validation_failure(message: &str) -> ErrorDetails {
    ErrorDetails {
        code: VALIDATION_ERROR_CODE.to_string(),
        message: message.to_string(),
        details: None,
    }
}

/// The message a modification of an asset model that has not settled yet is
/// refused with: the two states a model may be modified in, then the model's id,
/// its name and the state it is in.
///
/// A model whose owed transition is still pending is mid-write, so a second write
/// to it is refused rather than queued — the caller retries once the model
/// reports `ACTIVE` or `FAILED`.
#[must_use]
pub fn asset_model_not_modifiable_message(
    asset_model_id: &str,
    asset_model_name: &str,
    state: &str,
) -> String {
    format!(
        "This asset model must be {STATE_ACTIVE} or {STATE_FAILED} before you can modify it. \
         AssetModelID: {asset_model_id} AssetModelName: {asset_model_name} \
         AssetModelState: {state}"
    )
}

/// Refuse a modification of `record` while the model has not settled: only a
/// model that reports [`STATE_ACTIVE`] or [`STATE_FAILED`] may be modified.
///
/// The `Err` carries [`asset_model_not_modifiable_message`], which the calling
/// operation lifts into its own modelled `InvalidRequestException` (HTTP 400).
/// Shared, so every operation that gates on the state states one rule in one
/// wording.
pub fn check_asset_model_modifiable(
    record: &AssetModelRecord,
    now: SystemTime,
) -> Result<(), String> {
    let state = record.state(now);
    if state == STATE_ACTIVE || state == STATE_FAILED {
        return Ok(());
    }
    Err(asset_model_not_modifiable_message(
        &record.id,
        &record.name,
        state,
    ))
}

impl ComAmazonawsIotsitewiseAwsiotsitewiseState {
    /// What the transition an accepted write to the asset model `asset_model_id`
    /// owes settles at: `None` for a definition the model graph accepts — the
    /// model settles `ACTIVE` — and the diagnosis to report otherwise.
    ///
    /// The graph-scoped half of an asset-model definition's validation, asked of
    /// the same [`DefinitionUnderWrite`] the synchronous half
    /// ([`ComAmazonawsIotsitewiseAwsiotsitewiseState::check_asset_model_definition`])
    /// is asked of, so every operation that states a definition settles it by one
    /// rule: a hierarchy graph leading back to this model, and a formula variable
    /// reaching through a hierarchy at a property the child model does not expose
    /// or at a raw one where the formula aggregates.
    pub fn asset_model_settle_failure(
        &self,
        definition: &DefinitionUnderWrite,
        asset_model_id: &str,
    ) -> Option<ErrorDetails> {
        if self.hierarchies_reach(&definition.hierarchies, asset_model_id) {
            return Some(validation_failure(CYCLICAL_HIERARCHY_FAILURE));
        }
        // The scope each declared hierarchy resolves in is indexed ONCE, as the
        // synchronous half indexes its own scopes, so resolving a variable is a map
        // lookup however many properties the child models hold and however many
        // variables the definition declares.
        let mut scopes: HashMap<&str, HashMap<String, bool>> = HashMap::new();
        for hierarchy in &definition.hierarchies {
            let child = hierarchy.child_asset_model_id.as_str();
            if !scopes.contains_key(child) {
                scopes.insert(child, self.child_property_scope(child));
            }
        }
        definition
            .properties
            .iter()
            .chain(definition.composite_property_lists.iter().flatten())
            .find_map(|property| self.cross_hierarchy_failure(property, definition, &scopes))
            .map(validation_failure)
    }

    /// Whether the hierarchies `hierarchies` declares lead back to the asset model
    /// `target`, following each child model's own hierarchies.
    ///
    /// A hierarchy names "an asset model whose assets can be children of any other
    /// assets created from this asset model", so the `childAssetModelId` edges are
    /// a directed graph and a path from the written definition back to the model
    /// being written is a cycle in it. Each model is visited once, so a graph that
    /// already contains a cycle elsewhere cannot spin here.
    fn hierarchies_reach(&self, hierarchies: &[AssetModelHierarchy], target: &str) -> bool {
        let mut seen: HashSet<String> = HashSet::new();
        let mut pending: Vec<String> = hierarchies
            .iter()
            .map(|hierarchy| hierarchy.child_asset_model_id.clone())
            .collect();
        while let Some(child) = pending.pop() {
            if child == target {
                return true;
            }
            if !seen.insert(child.clone()) {
                continue;
            }
            if let Some(model) = self.asset_models.get(&child) {
                pending.extend(
                    model
                        .hierarchies
                        .iter()
                        .map(|hierarchy| hierarchy.child_asset_model_id.clone()),
                );
            }
        }
        false
    }

    /// The diagnosis ONE property definition's cross-hierarchy references settle
    /// the model at, or `None` when every one of them resolves to a property the
    /// formula may consume.
    ///
    /// Only the references that reach ANOTHER model are asked about here: a
    /// variable with no `hierarchyId` resolves in the model being written, which
    /// the synchronous plane already resolved it in. A variable naming a hierarchy
    /// the definition does not declare was refused there too, so one reaching here
    /// names a declared hierarchy and its child model is the scope it resolves in.
    ///
    /// A `metric` "maps several values to a single value": what it consumes from
    /// the assets under a hierarchy is an aggregate of them, so a variable of one
    /// has to name a metric of the child model, while a `transform`'s reference
    /// only has to resolve.
    fn cross_hierarchy_failure(
        &self,
        property: &AssetModelProperty,
        definition: &DefinitionUnderWrite,
        scopes: &HashMap<&str, HashMap<String, bool>>,
    ) -> Option<&'static str> {
        let aggregating = property.r#type.metric.is_some();
        for variable in formula_variables(property) {
            let Some(hierarchy_label) = variable.value.hierarchy_id.as_deref() else {
                continue;
            };
            let Some(property_label) = variable.value.property_id.as_deref() else {
                continue;
            };
            let Some(hierarchy) = definition
                .hierarchies
                .iter()
                .find(|hierarchy| names_hierarchy(hierarchy, hierarchy_label))
            else {
                continue;
            };
            let scope = scopes.get(hierarchy.child_asset_model_id.as_str());
            match scope.and_then(|scope| scope.get(property_label)) {
                None => return Some(MISSING_CHILD_PROPERTY_FAILURE),
                Some(false) if aggregating => return Some(NON_METRIC_CHILD_PROPERTY_FAILURE),
                Some(_) => {}
            }
        }
        None
    }

    /// Every label naming a property the asset model `child_asset_model_id`
    /// exposes, mapped to whether that property is AGGREGATED — a `metric`.
    ///
    /// The scope is every property that model exposes — its own definitions and
    /// the ones its composite models contribute
    /// ([`ComAmazonawsIotsitewiseAwsiotsitewiseState::exposed_asset_model_properties`]),
    /// the same scope the synchronous plane indexes — under the two spellings a
    /// reference into another model carries: the property's id and its
    /// `externalId:` alias. A model this store does not hold exposes nothing.
    fn child_property_scope(&self, child_asset_model_id: &str) -> HashMap<String, bool> {
        let mut scope = HashMap::new();
        let Some(child) = self.asset_models.get(child_asset_model_id) else {
            return scope;
        };
        for property in self.exposed_asset_model_properties(child, &HashSet::new()) {
            let aggregated = property.r#type.metric.is_some();
            if let Some(id) = property.id.as_deref().filter(|id| !id.is_empty()) {
                scope.insert(id.to_string(), aggregated);
            }
            if let Some(external_id) = property.external_id.as_deref() {
                scope.insert(format!("{EXTERNAL_ID_PREFIX}{external_id}"), aggregated);
            }
        }
        scope
    }

    /// Whether another asset model declares a hierarchy whose `childAssetModelId`
    /// is `asset_model_id`: the reference a delete of that model settles `FAILED`
    /// on, because the parent's hierarchy would otherwise name a model this
    /// service no longer holds.
    ///
    /// A hierarchy of the model ITSELF is not such a reference: it goes away with
    /// the model that declares it.
    #[must_use]
    pub fn referenced_by_parent_hierarchy(&self, asset_model_id: &str) -> bool {
        self.asset_models
            .values()
            .filter(|holder| holder.id != asset_model_id)
            .any(|holder| {
                holder
                    .hierarchies
                    .iter()
                    .any(|hierarchy| hierarchy.child_asset_model_id == asset_model_id)
            })
    }
}
