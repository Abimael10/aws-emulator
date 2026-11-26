// SPDX-License-Identifier: CC0-1.0

//! @op GetPersonalizedRanking
//!
//! ## How this handler answers
//!
//! "Re-ranks a list of recommended items for the given user. The first item in the
//! list is deemed the most likely item to be of interest to the user." So the
//! response is the REQUEST'S OWN `inputList`, re-ordered — never a list this handler
//! chooses:
//!
//! 1. What the REQUEST alone settles is checked FIRST, before anything is resolved:
//!    `inputList`'s length against the bound the same request fixes ("If you are
//!    including metadata in recommendations, the maximum is 50. Otherwise, the maximum
//!    is 500." — a longer list is 400 `Length of 'inputList' cannot be greater than
//!    <max>`), the `@required` `userId`, and the `metadataColumns` rules that read
//!    nothing else ("The map key is `ITEMS`", "The maximum number of columns you can
//!    provide is 10" — see [`crate::state::requested_metadata_columns`]). The column cap
//!    is settled in front of `inputList`'s bound, because that bound is read off the same
//!    map: a column list the cap refuses never lowers it.
//! 2. `campaignArn` is resolved against the registry in [`crate::state`] (400 "does
//!    not exist or is not active yet" when it resolves to nothing — see
//!    [`crate::state::Denied::unresolvable`]), and the campaign must deploy a
//!    `PERSONALIZED_RANKING` solution
//!    version ("The solution backing the campaign must have been created using a
//!    recipe of type PERSONALIZED_RANKING") — otherwise 400.
//! 3. Each `inputList` entry is looked up in the campaign's STORED Items catalog. An
//!    entry is an `ItemID`, bounded only by a `@length` that starts at 0, so an entry
//!    that is the empty string identifies no item and is read as the entry not being
//!    supplied (`crate::state::present`, the rule every `UserID`/`ItemID` of this
//!    service is read by): it names neither a catalog record nor an item to append, so
//!    it adds nothing to the ranking. An entry the catalog holds is scored by
//!    [`crate::state::scoring::Seed::relevance`] from
//!    its stored `base_score` and the request's `userId`/`context`; an entry the
//!    catalog does not hold "is appended to the end of the reranked list", with no
//!    relevance score for the user. The reported score is the entry's share of the
//!    certainty over the WHOLE stored catalog scored for this request, so an item
//!    carries the same score however long `inputList` is and whatever `filterArn`
//!    removed — a ranking of part of the catalog simply sums to less than 1.0.
//! 4. With no `filterArn`, the result is therefore a PERMUTATION of the DISTINCT ids
//!    `inputList` names, ordered by score descending. `InputList` carries no
//!    `@uniqueItems`, so a caller may list an id twice; both occurrences name the one
//!    catalog entry, whose share of the certainty the ranking already reports, so the
//!    repeat adds no entry (a repeated id reported twice would report its share twice
//!    and the reported scores would sum past the one unit of certainty the scored
//!    catalog apportions). With a `filterArn` ("a filter you created to include items
//!    or exclude items from recommendations for a given user") the entries the filter
//!    removes are not in the ranking.
//! 5. `metadataColumns` is served from the stored item attributes, for EVERY entry
//!    the response reports: with no `metadataColumns` the `metadata` member is
//!    omitted entirely, and with `metadataColumns` it is present on every entry,
//!    holding only the requested columns that entry stores — so an appended entry,
//!    which stores none, reports an empty map rather than no member.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `GetPersonalizedRanking`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-personalizeruntime-amazonpersonalizeruntime/smithy-model/com-amazonaws-personalizeruntime-amazonpersonalizeruntime.json")
//! ```
//!
//! Re-ranks a list of recommended items for the given user. The first item in the list is
//! deemed the most likely item to be of interest to the user. The solution backing the
//! campaign must have been created using a recipe of type PERSONALIZED_RANKING.
//!
//! ## Input
//!
//! * `campaignArn` (Arn, len 0..256, pattern ^arn:([a-z\d-]+):personalize:.*:.*:.+$)
//!   **required** — The Amazon Resource Name (ARN) of the campaign to use for generating the
//!   personalized ranking.
//! * `context` (Context, len 0..150) — The contextual metadata to use when getting
//!   recommendations. Contextual metadata includes any interaction information that might be
//!   relevant when getting a user's recommendations, such as the user's current location or
//!   device type.
//! * `filterArn` (Arn, len 0..256, pattern ^arn:([a-z\d-]+):personalize:.*:.*:.+$) — The
//!   Amazon Resource Name (ARN) of a filter you created to include items or exclude items
//!   from recommendations for a given user. For more information, see Filtering
//!   Recommendations.
//! * `filterValues` (FilterValues, len 0..25) — The values to use when filtering
//!   recommendations. For each placeholder parameter in your filter expression, provide the
//!   parameter name (in matching case) as a key and the filter value(s) as the corresponding
//!   value. Separate multiple values for one parameter with a comma. For filter expressions
//!   that use an INCLUDE element to include items, you must provide values for all parameters
//!   that are defined in the expression. For filters with expressions that use an EXCLUDE
//!   element to exclude items, you can omit the filter-values.In this case, Amazon
//!   Personalize doesn't use that portion of the expression to filter recommendations. For
//!   more information, see Filtering Recommendations.
//! * `inputList` (InputList) **required** — A list of items (by itemId) to rank. If an item
//!   was not included in the training dataset, the item is appended to the end of the
//!   reranked list. If you are including metadata in recommendations, the maximum is 50.
//!   Otherwise, the maximum is 500.
//! * `metadataColumns` (MetadataColumns, len 0..1) — If you enabled metadata in
//!   recommendations when you created or updated the campaign, specify metadata columns from
//!   your Items dataset to include in the personalized ranking. The map key is ITEMS and the
//!   value is a list of column names from your Items dataset. The maximum number of columns
//!   you can provide is 10. For information about enabling metadata for a campaign, see
//!   Enabling metadata in recommendations for a campaign.
//! * `userId` (UserID, len 0..256) **required** — The user for which you want the campaign to
//!   provide a personalized ranking.
//!
//! ## Output
//!
//! * `personalizedRanking` (ItemList) — A list of items in order of most likely interest to
//!   the user. The maximum is 500.
//! * `recommendationId` (RecommendationID) — The ID of the recommendation.
//!
//! ## Errors
//!
//! * `InvalidInputException` (HTTP 400) — Provide a valid value for the field or parameter.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource does not exist.

#[allow(unused_imports)]
use super::types::*;

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use crate::state::{self as registry, scoring, Denied, Entity, RecipeType};

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `GetPersonalizedRankingError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-personalizeruntime-amazonpersonalizeruntime.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `GetPersonalizedRankingError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.personalizeruntime#GetPersonalizedRanking")]
#[derive(Debug)]
pub enum GetPersonalizedRankingErrorModeled {
    InvalidInputException(InvalidInputException),
    ResourceNotFoundException(ResourceNotFoundException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::get_personalized_ranking::GetPersonalizedRankingError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type GetPersonalizedRankingError = crate::scaffolded::Scaffolded<GetPersonalizedRankingErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<GetPersonalizedRankingErrorModeled> for GetPersonalizedRankingError {
    fn from(modeled: GetPersonalizedRankingErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `GetPersonalizedRanking` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct GetPersonalizedRanking<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> GetPersonalizedRanking<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for GetPersonalizedRanking<Eng>
{
    type Input = GetPersonalizedRankingInput;
    type Output = GetPersonalizedRankingOutput;
    type Error = GetPersonalizedRankingError;

    const NAME: &'static str = "GetPersonalizedRanking";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_PERSONALIZED_RANKING_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::GET_PERSONALIZED_RANKING_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        // 0. Whether this CALLER may make the call at all — the decision AWS takes in front
        //    of the handler, on the action this operation names and on every resource ARN
        //    the request names ([`crate::state::authorization`]). It is taken before
        //    anything is read, so a refused request consults no registry entry and tells
        //    its caller nothing about one.
        if let Some(refusal) = crate::state::authorization_refusal(
            &*self.engine,
            ctx,
            Self::NAME,
            &referenced_resources(&input),
        ) {
            return Err(GetPersonalizedRankingError::access_denied(refusal));
        }

        let state = crate::state::service_state(&*self.engine)
            .ok_or_else(|| registry::registry_unavailable("GetPersonalizedRanking"))?;
        let now = SystemTime::now();
        let mut guard = state.lock();

        // 1. What the REQUEST alone settles, before the campaign is resolved.
        //
        //    `inputList`'s length is a fact about the REQUEST — "If you are including
        //    metadata in recommendations, the maximum is 50. Otherwise, the maximum is
        //    500." — so it is checked BEFORE the campaign is resolved: a list past the
        //    bound is refused whatever the campaign turns out to be. Which of the two
        //    maxima applies is the `metadataColumns` map read as the model states it
        //    ("The map key is `ITEMS`"), and the column cap is applied in front of the
        //    bound it would otherwise lower ([`crate::state::metadata_bounded_max`]) — so
        //    a map naming no column of the Items dataset gets the "Otherwise" maximum, and
        //    the lower bound is never taken from a column list this handler is about to
        //    refuse.
        let max_input_list =
            registry::metadata_bounded_max(input.metadata_columns.as_ref()).map_err(deny)?;
        if input.input_list.len() > max_input_list {
            return Err(deny(registry::too_long("inputList", max_input_list)));
        }
        //    `userId` is a fact about the request too: this operation serves
        //    `PERSONALIZED_RANKING` campaigns only, so the personalization input it
        //    needs never depends on what the ARN resolves to.
        if input.user_id.is_empty() {
            return Err(invalid(
                "userId is required: it is the user the campaign provides a personalized ranking \
                 for.",
            ));
        }
        //    The remaining request-only `metadataColumns` rule is the map key, which a
        //    map breaking asks for no metadata at all: it leaves the bound above at the
        //    "Otherwise" maximum, so nothing it decides is measured before it.
        let requested_metadata =
            registry::requested_metadata_columns(input.metadata_columns.as_ref()).map_err(deny)?;

        // 2. The campaign, and the recipe type this operation serves. `campaignArn` is
        //    `@required` and targets `Arn`, whose `@pattern` no empty or blank string
        //    satisfies, so the boundary has already refused anything that names no
        //    campaign at all: every value reaching this line is an ARN the boundary
        //    admitted, which the registry either resolves or reports unresolvable.
        //    WHICH operation serves a campaign is the registry's rule, stated once in
        //    [`RecipeType::operation`] and asked here with this operation's own name
        //    ([`RecipeType::served_by`]).
        let campaign = guard.campaign(&input.campaign_arn).map_err(deny)?.clone();
        if !campaign.recipe_type.served_by(Self::NAME) {
            return Err(invalid(format!(
                "Campaign {} deploys a solution version trained with the {} recipe type; the \
                 solution backing a GetPersonalizedRanking campaign must have been created using \
                 a recipe of type {}.",
                campaign.arn,
                campaign.recipe_type.as_str(),
                RecipeType::PersonalizedRanking.as_str()
            )));
        }

        // 3. The catalog this call ranks over: the campaign's stored catalog is what its
        //    solution version was FITTED to, and the ingestion data plane holds what the
        //    same dataset group has taken since (see [`crate::state::ingested`]). Metadata
        //    is then validated against the resolved campaign and that catalog — the parts
        //    of `metadataColumns` that need it; its map key and column cap were already
        //    settled in step 1, from the request.
        let ingested = registry::ingested::live_view(
            &*self.engine,
            campaign.dataset_group_arn.as_deref(),
            campaign.fitted_at,
        );
        let catalog = ingested.items(&campaign.items);
        let recorded = ingested.interactions_of(&input.user_id);
        let metadata_columns = registry::metadata_columns_in_catalog(
            requested_metadata,
            campaign.metadata_enabled,
            &registry::catalog_columns(&catalog),
        )
        .map_err(deny)?;

        let request_filter = guard
            .bind_filter(
                registry::present(input.filter_arn.as_deref()),
                Entity::Items,
                input.filter_values.as_ref(),
            )
            .map_err(deny)?;

        // 4. Score the WHOLE catalog for this request, settle each entry's share of the
        //    certainty over it, apply the request's filter and order what is left by
        //    score descending — the four steps in THAT order, applied through the one
        //    shared pipeline ([`scoring::ranked_by`]) the sibling operations report
        //    their own catalogs under, so the order is stated once for all of them and
        //    no operation can drift from it.
        //
        //    The denominator is the catalog — never the entries this `inputList`
        //    happened to name, nor the ones the request's `filterArn` kept: the reported
        //    score of an item is the model's certainty for the (campaign, userId,
        //    context, item) tuple. Re-ranking the same user over a longer list, or with
        //    a filter, therefore reports an item with the same score, and a ranking of
        //    part of the catalog sums to at most 1.0.
        //
        //    What this operation fills differently is the ONE per-operation part of that
        //    order, the tie-break key: it re-ranks a list the CALLER sent, so an equal
        //    pair keeps the position the caller sent it in, where an operation choosing
        //    its own list settles a tie by the entry's own id. An entry the request named
        //    nowhere takes the position after the last one it did, and is left out of the
        //    reported ranking in step 5.
        let seed = scoring::Seed::with_context(
            &campaign.arn,
            &input.user_id,
            input.context.as_ref(),
        );
        let listed = listed_positions(&input.input_list);
        let unlisted_position = input.input_list.len();
        let ranked = scoring::ranked_by(
            catalog.iter(),
            &seed,
            request_filter.as_ref(),
            &recorded,
            |item| {
                listed
                    .get(item.item_id.as_str())
                    .copied()
                    .unwrap_or(unlisted_position)
            },
        );

        // 5. Report, out of that ranking, the entries `inputList` names — this handler
        //    answers with the REQUEST'S OWN list re-ordered, so the ranking over the
        //    whole catalog is what it selects from rather than what it reports.
        //
        //    Each DISTINCT id is reported once. An id the caller listed again names the
        //    same entry, and an entry's reported score is its share of the one unit of
        //    certainty spread over the scored catalog, so reporting it a second time
        //    would hand out that share twice and the ranking would sum past 1.0. The
        //    FIRST occurrence is the one kept, so repetition changes neither which ids
        //    come back nor the order a tie between them resolves in.
        let mut reported: BTreeSet<&str> = BTreeSet::new();
        let mut personalized_ranking: Vec<PredictedItem> = Vec::new();
        for &(item, score) in &ranked {
            if !listed.contains_key(item.item_id.as_str()) {
                continue;
            }
            if !reported.insert(item.item_id.as_str()) {
                continue;
            }
            personalized_ranking.push(PredictedItem {
                item_id: Some(item.item_id.clone()),
                score: Some(score),
                promotion_name: None,
                metadata: registry::metadata_for(&item.attributes, metadata_columns.as_deref()),
                reason: None,
            });
        }

        // 6. "If an item was not included in the training dataset, the item is appended
        //    to the end of the reranked list" — with no relevance score for the user, so
        //    the list stays ordered by score descending, and in the order the caller
        //    listed those ids in. An entry with no catalog record carries no stored
        //    attributes, so the request's filter decides on an empty attribute set: an
        //    INCLUDE filter cannot keep it, an EXCLUDE filter cannot remove it.
        //
        //    Whether `metadata` is reported is a fact about the REQUEST, so an appended
        //    entry is reported through the same helper, with the empty attribute set it
        //    stores: a request that asked for metadata gets the member on every entry,
        //    holding only the requested columns the entry actually carries — none, here.
        let no_attributes: BTreeMap<String, String> = BTreeMap::new();
        for item_id in &input.input_list {
            //    An entry that names no item appends none: it is read as the entry not
            //    being supplied ([`listed_positions`]), so it is neither a catalog record
            //    nor an item the training dataset left out.
            let Some(item_id) = registry::present(Some(item_id.as_str())) else {
                continue;
            };
            if catalog.iter().any(|item| item.item_id == item_id) {
                continue;
            }
            if !reported.insert(item_id) {
                continue;
            }
            if request_filter
                .as_ref()
                .is_some_and(|filter| !filter.retains(recorded.facts(item_id, &no_attributes)))
            {
                continue;
            }
            personalized_ranking.push(PredictedItem {
                item_id: Some(item_id.to_string()),
                score: Some(0.0),
                promotion_name: None,
                metadata: registry::metadata_for(&no_attributes, metadata_columns.as_deref()),
                reason: None,
            });
        }

        let recommendation_id = guard.issue_recommendation_id(&campaign.arn, now);
        Ok(GetPersonalizedRankingOutput {
            personalized_ranking: Some(personalized_ranking),
            recommendation_id: Some(recommendation_id),
        })
    }
}

/// The DISTINCT ids `inputList` names, each with the position of its FIRST occurrence —
/// this operation's tie-break key, and the set of ids it reports.
///
/// The first occurrence is the one that counts: an id the caller listed again names the
/// same catalog entry, so the repeat may move neither which ids come back nor the order
/// a tie between them resolves in.
///
/// An entry NAMES an id: an entry is an `ItemID`, bounded only by a `@length` that starts
/// at 0, so the boundary admits the empty string for one, and an empty string identifies
/// no item — the one reading of a `UserID`/`ItemID` this crate has
/// ([`registry::present`]). Such an entry is therefore read as the entry not being
/// supplied: it names nothing to report, and the ranking is the one the same list without
/// it gets.
fn listed_positions(input_list: &[String]) -> BTreeMap<&str, usize> {
    let mut listed: BTreeMap<&str, usize> = BTreeMap::new();
    for (position, item_id) in input_list.iter().enumerate() {
        if let Some(item_id) = registry::present(Some(item_id.as_str())) {
            listed.entry(item_id).or_insert(position);
        }
    }
    listed
}

/// Every resource ARN this request NAMES.
///
/// The authorization decision is taken over all of them and nothing else: the campaign the
/// action is performed on (the resource type `personalize:GetPersonalizedRanking` is
/// published for) and the request's own filter. Read from the REQUEST, so the decision is
/// independent of whether either exists — which is what keeps a refused call from telling an
/// unauthorized caller whether one does.
fn referenced_resources(input: &GetPersonalizedRankingInput) -> Vec<&str> {
    let mut referenced: Vec<&str> = Vec::new();
    referenced.extend(registry::present(Some(input.campaign_arn.as_str())));
    referenced.extend(registry::present(input.filter_arn.as_deref()));
    referenced
}

/// The modelled `InvalidInputException` (HTTP 400): "Provide a valid value for the
/// field or parameter."
fn invalid(message: impl Into<String>) -> GetPersonalizedRankingError {
    GetPersonalizedRankingErrorModeled::InvalidInputException(InvalidInputException {
        message: Some(message.into()),
    })
    .into()
}

/// Lift a registry refusal into this operation's error channel.
///
/// Every refusal the registry produces is the invalid-input channel (see
/// [`Denied`]), so this is where that channel's HTTP 400 is chosen for this operation.
fn deny(denied: Denied) -> GetPersonalizedRankingError {
    invalid(denied.message())
}
