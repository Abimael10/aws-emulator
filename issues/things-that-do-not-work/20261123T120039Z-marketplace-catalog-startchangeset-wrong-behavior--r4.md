Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 1c1ad928656ab490cd8a01812984ee392dea6071

# The self-contradiction rule keys a claim by details MEMBER, so the two members an entity's Name is read from escape it

_Filed: 2026-11-23T12:00:39Z_
_Operation: StartChangeSet | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `1c1ad928656ab490cd8a01812984ee392dea6071`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`crate::state::contradicting_details` is the rule that fails a change set whose changes are each applicable but which claim a different value for the same attribute of one entity. It keys every claim by `(entity_id, member)` — the details member name — while the rest of the crate declares that `ProductTitle` and `Name` are the TWO members the ONE `Name` attribute of an entity is read from, on every entity type.

Consequence: a change set whose change A gives `{"ProductTitle": "left"}` and whose change B gives `{"Name": "right"}` against the same entity is judged NOT to contradict itself and is applied, even though both changes write the record's single `name` field through `detail_name` — exactly the double claim on one attribute the rule exists to refuse (the all-`ProductTitle` and all-`Name` spellings of the same change set DO fail). The record is left with `name = "right"` (the later change) while its stored details still carry `ProductTitle = "left"`, and the two `ListEntities` paths then disagree about the same attribute: `EntityRecord::to_summary` reports `AmiProductSummary$ProductTitle` out of `self.name` ("right"), while `EntityRecord::attribute_values("ProductTitle")` selects out of `self.details` ("left"). So `ListEntities` filtered on the `ProductTitle` value it just reported answers an empty page, and filtering on the value it does not report returns the entity.

The fix is to key the claim by the ATTRIBUTE the member feeds (name, visibility) rather than by the member's spelling, which is what the function's own doc and `details_defects` already say those members are.

## Evidence

The rule keys by member, not by attribute — `services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/state.rs:2963` (doc) and `:2983`/`:2993` (code):

    /// The `FailureDescription` of a change set that contradicts itself: two of its
    /// changes claim a different value for the SAME attribute of the same entity,
    ...
    fn contradicting_details(changes: &[ChangeRecord]) -> Option<String> {
        let read_members = || {
            ENTITY_NAME_DETAIL_MEMBERS
                .iter()
                .chain(::std::iter::once(&ENTITY_VISIBILITY_DETAIL_MEMBER))
        };
        let mut claimed: HashMap<(String, &str), Value> = HashMap::new();
        ...
                let key = (entity_id.clone(), *member);

The other side — the same crate states those two members ARE one attribute. `state.rs:386`:

    /// The details members an entity's `Name` is read from.
    ///
    /// `EntitySummary$Name` is "defined by the seller", and the per-type summaries
    /// the model carries name it two ways: the five product summaries ... call it
    /// `ProductTitle`, while `OfferSummary` / `OfferSetSummary` /
    /// `ResaleAuthorizationSummary` call it `Name`.
    pub const ENTITY_NAME_DETAIL_MEMBERS: &[&str] = &["ProductTitle", "Name"];

and `state.rs:2919` judges BOTH members, for ANY entity type, against the ONE name contract:

    let name = entity_name_contract(entity_type);
    for member in ENTITY_NAME_DETAIL_MEMBERS {
        if let Some(value) = members.get(*member) {
            if !name.admits(value) {

and `state.rs:2433` lets EITHER member move the record's single `name` field, first-present-wins:

    fn detail_name(entity_type: &str, members: &Map<String, Value>) -> Option<String> {
        let contract = entity_name_contract(entity_type);
        ENTITY_NAME_DETAIL_MEMBERS
            .iter()
            .filter_map(|member| members.get(*member))
            .filter_map(|value| value.as_str())
            .find(|text| contract.admits_text(text))

The report/filter split that makes the result observable — `state.rs:1218` (summary reads the field):

    fn product_summary(&self, entity_type: &str) -> Option<(Option<String>, Option<String>)> {
        (self.entity_type == entity_type && PRODUCT_ENTITY_TYPES.contains(&entity_type))
            .then(|| (self.summary_name(), self.summary_visibility()))

versus `state.rs:1411` (filter/sort read the details member, falling back to the field only when the details carry nothing):

    pub fn attribute_values(&self, member: &str) -> Vec<String> {
        ...
        let held = match self.details.get(member) { ... };
        if held.is_empty() && ENTITY_NAME_DETAIL_MEMBERS.contains(&member) {
            return self.name.clone().into_iter().collect();
        }

Test coverage: `tests/all/change_set_failures.rs:601` states the rule as per-ATTRIBUTE — "two changes claiming a different value for the same ATTRIBUTE of one entity, which are the members the catalog reads" — and its rows at `:615`-`:635` cover `ProductTitle` vs `ProductTitle` (must fail) and `Name` vs `Name` on an Offer (must fail), but never the cross-member pair, so the gap is unpinned. The verdict is reached through `judge_change_set` at `state.rs:2204` (`verdict.failure_description = contradicting_details(&record.changes);`), first driven by `store.settle(now)` in `src/ops/start_change_set.rs:404`.

## Steps to Reproduce

1. POST /StartChangeSet with Catalog=AWSMarketplace and ChangeSet=[{ChangeType:"CreateProduct", Entity:{Type:"AmiProduct@1.0"}, DetailsDocument:{"ProductTitle":"left"}}]; wait for the change set to reach SUCCEEDED and read the created entity id E out of ListChangeSets' EntityIdList.
2. POST /StartChangeSet with TWO changes against E: {ChangeType:"UpdateInformation", Entity:{Type:"AmiProduct@1.0", Identifier:E}, DetailsDocument:{"ProductTitle":"left"}} and {ChangeType:"UpdateDeliveryOptions", Entity:{Type:"AmiProduct@1.0", Identifier:E}, DetailsDocument:{"Name":"right"}}.
3. Wait past CHANGE_SET_PREPARING+CHANGE_SET_APPLYING and GET /DescribeChangeSet for it: Status=SUCCEEDED with no FailureDescription. Repeat step 2 with BOTH changes spelling the name as "ProductTitle" ("left"/"right") — that change set reaches FAILED with a FailureDescription, showing the same double claim on one attribute is refused only when the two changes happen to use the same member spelling.
4. GET /DescribeEntity?catalog=AWSMarketplace&entityId=E — DetailsDocument carries ProductTitle="left" and Name="right".
5. POST /ListEntities with Catalog=AWSMarketplace, EntityType="AmiProduct" — EntitySummary.Name and AmiProductSummary.ProductTitle both report "right", disagreeing with the stored ProductTitle detail.
6. POST /ListEntities with EntityTypeFilters={"AmiProductFilters":{"ProductTitle":{"ValueList":["right"]}}} — empty page, although "right" is the ProductTitle the previous step reported; repeat with ValueList=["left"] — the entity is returned.
