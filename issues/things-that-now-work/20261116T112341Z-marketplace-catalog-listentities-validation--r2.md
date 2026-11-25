Issue-Severity: MEDIUM
Issue-Gap-Class: validation
Issue-Commit: d68524ddfe39deacc36a1df634022e0b97d0c530

# EntityTypeFilters entity-id value lists accept a repeated value the generic FilterList refuses

_Filed: 2026-11-16T11:23:41Z_
_Operation: ListEntities | Class: validation_

> _Note: this issue was identified against the code base at commit `d68524ddfe39deacc36a1df634022e0b97d0c530`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ListEntities` has two surfaces that filter on an entity id, and only one of them enforces the uniqueness the documentation states for both.

The generic `FilterList` goes through the shared filter rule, which refuses a `ValueList` that names the same `EntityId` twice — `filter_values_unique` returns true for `EntityId` and `filter_predicate` answers the modelled `ValidationException`. The per-type surface (`EntityTypeFilters`) builds its `EntityId` criterion with the `values!` macro, which clones the submitted `ValueList` verbatim; nothing in `type_criteria` or in `EntityCriterion` looks for a repeat, so the same request shape is served 200.

So on one and the same operation, filtering the same attribute, `FilterList: [{"Name": "EntityId", "ValueList": ["prod-abc", "prod-abc"]}]` is a 422 and `EntityTypeFilters: {"AmiProductFilters": {"EntityId": {"ValueList": ["prod-abc", "prod-abc"]}}}` is a 200 — even though the model words the two the same way ("a list of unique `EntityId`s" / "A string array of unique entity id values to be filtered on").

## Evidence

The rule the crate implements for one surface:
- `src/state.rs:201-209`: "`Filter$ValueList` states it per filter: the `EntityId` list is \"a list of unique `EntityId`s\" while the `ChangeSetName` list is \"a list of NON-unique `ChangeSetName`s\", so a value repeated in the first is a request the documentation forbids and one repeated in the second is not." / `pub fn filter_values_unique(name: &str) -> bool { name == ENTITY_ID_FILTER_NAME }`
- `src/state.rs:311-321`: `if filter_values_unique(&name) { if let Some(repeated) = first_repeated(&values) { return Err(validation("FilterList.ValueList", format!("filter {name} matches a list of unique values, and '{repeated}' is named twice"))); } }`
- `src/ops/list_entities.rs:269-276` wires that in: `let predicates = match crate::state::filter_predicates(Self::NAME, crate::state::ENTITY_FILTER_NAMES, input.filter_list.unwrap_or_default(),)`

The same operation's other surface, which skips it:
- `src/ops/list_entities.rs:529`: `values!(&mut criteria, "EntityId", asked.entity_id);` (and the identical line for every other `*Filters` arm: `:536`, `:543`, `:550`, `:557`, `:564`, `:597`, `:611`, `:654`)
- `src/ops/list_entities.rs:448-456` (the macro): `push_criterion($into, EntityCriterion::on($member).values($filter.as_ref().and_then(|held| held.value_list.as_ref())))` — no uniqueness check.
- `src/state.rs:1374-1377`: `pub fn values(mut self, value_list: Option<&Vec<String>>) -> Self { self.value_list = value_list.cloned().unwrap_or_default(); self }`
- `src/state.rs:1417-1423` (`EntityCriterion::matches`): `if !self.value_list.is_empty() && !held.iter().any(|value| self.value_list.iter().any(|wanted| wanted == value)) { return false; }` — repeats simply match twice.

Smithy, both wordings:
- `Filter$ValueList` doc: "`ListEntities` - This is a list of unique `EntityId`s."
- `AmiProductEntityIdFilter$ValueList` doc: "A string array of unique entity id values to be filtered on." (smithy_query shape=AmiProductEntityIdFilter)

Test coverage: `tests/all/filter_value_forms.rs:784` covers the generic list ("The filters the documentation does NOT state unique take a repeated value"); no test in `tests/all/` exercises a repeated value inside an `EntityTypeFilters` member — `tests/all/entity_type_projections.rs:529-679` exercises `EntityTypeFilters` selection only, so nothing pins the current acceptance.

## Steps to Reproduce

1. Start and let succeed a change set creating one AmiProduct entity, and note its EntityId.
2. POST /ListEntities with {"Catalog": "AWSMarketplace", "EntityType": "AmiProduct", "FilterList": [{"Name": "EntityId", "ValueList": ["<id>", "<id>"]}]} — refused 422 ValidationException naming the repeated value.
3. POST /ListEntities with {"Catalog": "AWSMarketplace", "EntityType": "AmiProduct", "EntityTypeFilters": {"AmiProductFilters": {"EntityId": {"ValueList": ["<id>", "<id>"]}}}} — answered 200 with the entity listed.
4. Compare: the same attribute, the same operation, the same documented uniqueness, two different verdicts.
