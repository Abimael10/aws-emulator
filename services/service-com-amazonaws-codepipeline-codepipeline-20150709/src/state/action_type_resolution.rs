// SPDX-License-Identifier: CC0-1.0

//! Resolving the action type a declared action names, and what that resolution decides
//! about the action's own declaration.
//!
//! Split out of [`super`] so one file holds one concern (and so neither file grows past
//! the project's file-length limit); every name stays re-exported from `crate::state`,
//! which is where the handlers reach it.
//!
//! Two rules, both applied by the ONE declaration validator every operation that takes a
//! `PipelineDeclaration` runs ([`super::validate_declaration`]):
//!
//! * an `actionTypeId` whose `owner` is `AWS` names an action type the SERVICE publishes,
//!   and that set is closed — [`super::BUILT_IN_ACTION_TYPES`] is the catalog
//!   `ListActionTypes` reports for that owner, and it IS the published set but for the few
//!   identities `ActionType`'s required artifact counts cannot carry
//!   ([`AWS_ACTION_TYPES_WITHOUT_DETAIL`]) — so a `category`/`provider`/`version`
//!   combination neither of them carries names no action type at all and the declaration
//!   cannot be run. (`Custom` and `ThirdParty` ids are
//!   resolved against registrations this account or a partner makes, which a declaration
//!   may name before the registration is read back, so an id of either owner the catalog
//!   does not carry is not refused here.)
//! * an action takes and produces the artifacts its action type publishes bounds for:
//!   `ArtifactDetails$maximumCount` is "the maximum number of artifacts allowed for the
//!   action type", so an `inputArtifacts` or `outputArtifacts` list longer than that bound
//!   describes artifacts the action can never consume or produce — which is what an
//!   `Approval` action, whose action type publishes a maximum of zero of both, declares
//!   with any artifact at all.
//!
//! Both are defects of ONE action declaration rather than of the stage holding it or of
//! the pipeline as a whole, so both are refused as `InvalidActionDeclarationException`
//! (`super::DeclarationProblem::Action`).

use super::*;

/// The catalog entry the four `ActionTypeId` members name, or `None` when the service
/// publishes no action type with that identity.
pub fn built_in_action_type(id: &ActionTypeId) -> Option<&'static BuiltInActionType> {
    BUILT_IN_ACTION_TYPES.iter().find(|entry| {
        entry.category == id.category
            && entry.owner == id.owner
            && entry.provider == id.provider
            && entry.version == id.version
    })
}

/// The `ActionOwner` value of an action type the SERVICE itself publishes — the one owner
/// whose catalog is closed, so an id carrying it either names a published action type or
/// names nothing.
pub const ACTION_OWNER_AWS: &str = "AWS";

/// The version every action type the service publishes carries: `1`, the only version
/// these actions publish ([`BUILT_IN_ACTION_TYPES`] states the same for its entries).
const BUILT_IN_ACTION_TYPE_VERSION: &str = "1";

/// The `AWS`-owned `(category, provider)` pairs the documented provider list names that
/// [`BUILT_IN_ACTION_TYPES`] cannot hold an entry for.
///
/// Source: the page `ActionTypeId$provider` and `ActionTypeId$owner` both link to, "Valid
/// action types and providers in CodePipeline"
/// (<https://docs.aws.amazon.com/codepipeline/latest/userguide/reference-pipeline-structure.html#actions-valid-providers>),
/// which is where the catalog's own entries come from.
///
/// ONE bound decides what is here rather than in the catalog: `ActionType` declares
/// `inputArtifactDetails` and `outputArtifactDetails` as REQUIRED members whose counts are
/// modelled `@range(min: 0, max: 5)` (`smithy_query query="shape"
/// shape="ActionType,ArtifactDetails"`), and each provider here documents a count above it
/// — the CloudFormation deploy action's ten input artifacts, and the `Commands` action's,
/// in each of the three categories it is published in. The bound a declared action is
/// judged against is the catalog's own ([`validate_action_type`]), so an entry for one of
/// these would publish a maximum that refuses a declaration the real service accepts.
/// Published by no page and refused by no bound is the lesser of the two wrongs, so these
/// identities are published here: a declaration naming one names a real action type, and
/// this service holds its artifact lists to no bound of its own.
///
/// Every OTHER published identity is a catalog entry, including the providers whose
/// documented parameter table is longer than `ActionConfigurationPropertyList` can carry:
/// that member is optional, so such an entry reports its identity and its artifact bounds
/// (see [`super::built_in_catalog`]'s note on both bounds). The two tables are disjoint,
/// which [`action_type_is_published`] does not depend on and this crate's declaration suite
/// asserts.
pub const AWS_ACTION_TYPES_WITHOUT_DETAIL: [(&str, &str); 4] = [
    ("Build", "Commands"),
    ("Compute", "Commands"),
    ("Deploy", "CloudFormation"),
    ("Test", "Commands"),
];

/// Whether the service publishes an action type with this identity at all — the catalog
/// entry, or one of the published identities it carries no detail for.
pub fn action_type_is_published(id: &ActionTypeId) -> bool {
    built_in_action_type(id).is_some()
        || (id.owner == ACTION_OWNER_AWS
            && id.version == BUILT_IN_ACTION_TYPE_VERSION
            && AWS_ACTION_TYPES_WITHOUT_DETAIL
                .iter()
                .any(|(category, provider)| {
                    *category == id.category && *provider == id.provider
                }))
}

/// The identity of an action type, as a refusal states it: the four `ActionTypeId`
/// members, each quoted under the name the model gives it, in the order CodePipeline
/// states them (`Category`, `Provider`, `Owner`, `Version`).
pub fn action_type_identity(id: &ActionTypeId) -> String {
    format!(
        "ActionType (Category: '{}', Provider: '{}', Owner: '{}', Version: '{}')",
        id.category, id.provider, id.owner, id.version
    )
}

/// An AWS Region as a refusal NAMES it: the Region enum spelling, which is the region
/// token upper-cased with its separators as underscores (`us-west-2` -> `US_WEST_2`).
///
/// Derived from the region the request was signed for, never from a table.
pub fn region_identity(region: &str) -> String {
    region.to_uppercase().replace('-', "_")
}

/// The message `InvalidActionDeclarationException` carries for an action naming an action
/// type the service does not publish.
///
/// An action type is published per Region, so the refusal states the identity the request
/// named, the action that named it and the Region it was not available in — the Region the
/// request was signed for.
pub fn unknown_action_type_message(
    id: &ActionTypeId,
    action_name: &str,
    region: &str,
) -> String {
    format!(
        "{} in action '{action_name}' is not available in region '{}'",
        action_type_identity(id),
        region_identity(region)
    )
}

/// The message `InvalidActionDeclarationException` carries for an action declaring more
/// artifacts than its action type takes: the action that declared them, WHICH artifacts
/// they are (the ones the action takes in or the ones it produces), the count the request
/// declared and the bound that action type publishes.
///
/// The subject is the ACTION, named unquoted, and the two counts are stated declared-then-
/// maximum, with the maximum parenthesised: the defect is an artifact COUNT over a bound,
/// so the sentence names the action to fix and the two numbers to compare, and restates
/// neither the `actionTypeId` (which resolved) nor the JSON member name.
pub fn artifact_count_message(
    action_name: &str,
    artifacts: &str,
    declared: usize,
    maximum: i32,
) -> String {
    format!(
        "Action {action_name} declares {declared} {artifacts} artifacts which is more than \
         the maximum count ({maximum})"
    )
}

/// The two artifact-list members of an action declaration: the model's name for each, and
/// the kind of artifact a refusal states it as.
const ARTIFACT_MEMBERS: [(&str, &str); 2] = [("inputArtifacts", "input"), ("outputArtifacts", "output")];

/// Refuse an action whose `actionTypeId` names no action type the service publishes in
/// `region`, or which declares more artifacts than that action type takes.
///
/// `Ok(())` for an action this file's rules say nothing about: an id of an owner whose
/// catalog is open (see the module docs), whose artifact lists are then bounded by nothing
/// this file knows.
pub fn validate_action_type(
    action: &ActionDeclaration,
    region: &str,
) -> Result<(), DeclarationProblem> {
    let id = &action.action_type_id;
    let Some(action_type) = built_in_action_type(id) else {
        if id.owner == ACTION_OWNER_AWS && !action_type_is_published(id) {
            return Err(DeclarationProblem::Action(unknown_action_type_message(
                id,
                &action.name,
                region,
            )));
        }
        return Ok(());
    };
    let declared = [
        action.input_artifacts.as_ref().map_or(0, Vec::len),
        action.output_artifacts.as_ref().map_or(0, Vec::len),
    ];
    let bounds = [action_type.input_artifacts.1, action_type.output_artifacts.1];
    for (((_, artifacts), declared), published) in
        ARTIFACT_MEMBERS.iter().zip(declared).zip(bounds)
    {
        // Clamped ONCE, so the figure the refusal states is the figure it was judged
        // against: a catalog bound below zero would describe no list a declaration could
        // carry, and is read as "none" rather than printed back at the caller.
        let maximum = published.max(0);
        if declared > maximum as usize {
            return Err(DeclarationProblem::Action(artifact_count_message(
                &action.name,
                artifacts,
                declared,
                maximum,
            )));
        }
    }
    Ok(())
}

/// The message `InvalidStageDeclarationException` carries when a declaration names two
/// stages alike.
///
/// Built from the request: a stage is addressed by its name (`GetPipelineState`'s
/// `stageStates[].stageName`, `DisableStageTransition$stageName`), so the refusal names
/// the stage whose name the declaration repeats and nothing else — worded like the
/// repeated-action-name refusal ([`super::duplicate_action_name_message`]), because it is
/// the same law one level up.
pub fn duplicate_stage_name_message(stage_name: &str) -> String {
    format!("Stage name '{stage_name}' is used more than once")
}
