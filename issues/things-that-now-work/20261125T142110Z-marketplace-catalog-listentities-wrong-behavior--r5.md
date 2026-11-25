Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 64cf88be6b9961da86b7aabeafaa6119dc7ba9bb

# An entity's reported Name is read from ProductTitle for every type, so an Offer/OfferSet/ResaleAuthorization name diverges from the member the same crate filters and sorts on

_Filed: 2026-11-25T14:21:10Z_
_Operation: ListEntities | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `64cf88be6b9961da86b7aabeafaa6119dc7ba9bb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The crate states a PER-TYPE mapping for the details member an entity's name comes from: a product's name is `ProductTitle`, while an `Offer` / `OfferSet` / `ResaleAuthorization` names it `Name`. But `detail_name`, the one place `apply_changes` sets `EntityRecord::name`, takes `entity_type` only to pick the length/pattern contract and then chooses the member by a FIXED priority list (`ProductTitle` first, `Name` second) for every type. So an `Offer` whose change details carry both members takes its name from `ProductTitle` — a member `OfferSummary` does not even report.

That makes two code paths of the same operation disagree about the same attribute. `EntitySummary$Name` and `OfferSummary$Name` are projected from `EntityRecord::name` (i.e. from `ProductTitle`), while the per-type filter `OfferFilters.Name` and the sort `OfferSort.SortBy=Name` go through `EntityRecord::attribute_values("Name")`, which reads `details["Name"]` and only falls back to `self.name` when the details hold nothing at that member. Result: `ListEntities` filtered by `Name = <the value it reports>` returns an empty page, while filtering by `Name = <the value nothing reports>` returns the entity, and a page ordered by `Name` is ordered by a value the page does not show.

The same divergence applies to `OfferSetFilters.Name` / `ResaleAuthorizationFilters.Name` and their `*Sort` counterparts. The crate already knows the per-type mapping (`PRODUCT_ENTITY_TYPES`, `NAMED_ENTITY_TYPES`, used by `entity_name_contract` right next to this code), so the member `detail_name` reads should follow the entity's type the way the contract already does. No test in `tests/all/` gives one entity both members (`entity_type_projections.rs` seeds products with `ProductTitle` only and offers with `Name` only), so nothing pins the current behaviour.

## Evidence

The crate's own statement of the per-type mapping — services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/state.rs:434-442:
```
/// The details members an entity's `Name` is read from.
///
/// `EntitySummary$Name` is "defined by the seller", and the per-type summaries
/// the model carries name it two ways: the five product summaries
/// (`AmiProductSummary`, `ContainerProductSummary`, `DataProductSummary`,
/// `SaaSProductSummary`, `MachineLearningProductSummary`) call it
/// `ProductTitle`, while `OfferSummary` / `OfferSetSummary` /
/// `ResaleAuthorizationSummary` call it `Name`.
pub const ENTITY_NAME_DETAIL_MEMBERS: &[&str] = &["ProductTitle", "Name"];
```
The code that reads it ignores the type when choosing WHICH member — state.rs:2558-2566:
```
fn detail_name(entity_type: &str, members: &Map<String, Value>) -> Option<String> {
    let contract = entity_name_contract(entity_type);
    ENTITY_NAME_DETAIL_MEMBERS
        .iter()
        .filter_map(|member| members.get(*member))
        .filter_map(|value| value.as_str())
        .find(|text| contract.admits_text(text))
        .map(str::to_string)
}
```
(`entity_type` reaches only `entity_name_contract`; the member is the first of the fixed list that is present, i.e. `ProductTitle` for an Offer too.)

The projection side reads that stored value — state.rs:1435-1440 (`summary_name`) feeding state.rs:1374-1387 (`offer_summary: self.is_type("Offer").then(|| OfferSummary { name: self.summary_name(), ... })`).

The filter/sort side reads the NAMED member instead — state.rs:1521-1540:
```
    pub fn attribute_values(&self, member: &str) -> Vec<String> {
        match member { "EntityId" => ..., "LastModifiedDate" => ..., ... }
        let held = match self.details.get(member) { Some(Value::String(text)) => vec![text.clone()], ... };
        if held.is_empty() && ENTITY_NAME_DETAIL_MEMBERS.contains(&member) {
            return self.name.clone().into_iter().collect();
        }
        held
    }
```
and that is the attribute the per-type Offer filter selects on — services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/ops/list_entities.rs:584: `text!(&mut criteria, "Name", asked.name);` (criteria are matched by `EntityCriterion::matches` -> `record.attribute_values(&self.member)`, state.rs:1622-1623).

The model keeps the two members apart: `OfferSummary$Name` doc is "The name of the offer." (smithy_query shape=OfferSummary) while `AmiProductSummary$ProductTitle` doc is "The title of the AMI product." (smithy_query shape=AmiProductSummary) — `ProductTitle` is no member of `OfferSummary` at all.

Test coverage: tests/all/entity_type_projections.rs:215-267 seeds AmiProduct/SaaSProduct details with `ProductTitle` only and Offer/OfferSet/ResaleAuthorization details with `Name` only, and tests/all/entity_type_projections.rs:419,731 sort offers by `Name`; no case gives one entity both members, so the divergent branch is untested and unpinned.

## Steps to Reproduce

1. POST /StartChangeSet with Catalog=AWSMarketplace, Intent=APPLY and ChangeSet=[{ChangeType:"CreateOffer", Entity:{Type:"Offer@1.0"}, Details:"{\"Name\":\"offer name\",\"ProductTitle\":\"product title\",\"State\":\"Draft\"}"}]
2. Wait for the change set to reach SUCCEEDED (CHANGE_SET_PREPARING + CHANGE_SET_APPLYING)
3. POST /ListEntities with Catalog=AWSMarketplace, EntityType="Offer" — observe EntitySummaryList[0].Name == "product title" and EntitySummaryList[0].OfferSummary.Name == "product title", although `OfferSummary$Name` is the offer's `Name` member ("offer name")
4. POST /ListEntities with Catalog=AWSMarketplace, EntityType="Offer", EntityTypeFilters={"OfferFilters":{"Name":{"ValueList":["product title"]}}} — observe an EMPTY page even though the previous step reported exactly that Name
5. POST /ListEntities with the same filter but ValueList=["offer name"] — observe the entity IS returned, carrying a reported Name of "product title": the value the filter matched is reported by no member of the answer
6. POST /ListEntities with EntityTypeSort={"OfferSort":{"SortBy":"Name","SortOrder":"ASCENDING"}} over two such offers — observe the page ordered by the `Name` details member while the `Name` it reports comes from `ProductTitle`
