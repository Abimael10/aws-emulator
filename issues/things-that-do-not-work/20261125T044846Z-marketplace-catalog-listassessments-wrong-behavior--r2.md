Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 42a0d765b3bd054d51e2034b3a861b2852aa6236

# A minted delivery-option id can duplicate an id a change already submitted, so the AMISecurityFilters DeliveryOptionId filter reports another entity's assessment

_Filed: 2026-11-25T04:48:46Z_
_Operation: ListAssessments | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `42a0d765b3bd054d51e2034b3a861b2852aa6236`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`identify_delivery_options` assigns an id to every submitted delivery option that carries none, and that id is what each `AssessmentRecord` is then reported and FILTERED by (`FrameworkSummary$DeliveryOptionId`, `AMISecurityFilters$DeliveryOptionId`). The minter, `ComAmazonawsMarketplacecatalogAwsmpseymourState::mint_delivery_option_id`, is the ONLY id minter of this crate that does not check the id it hands out against the ids already in play: it increments a counter and returns `do-{seq:012x}` unconditionally, while `mint_change_set_id`, `mint_entity_id` and `mint_assessment_id` each loop until the candidate is unused (and `mint_entity_id` additionally skips an id a running change set reserved, "so two change sets in flight never name the same new entity").

The same handler path deliberately KEEPS an id a change submitted itself ("An option submitted WITH an id keeps it: the change is updating the option that id names"), and a change's details are opaque JSON the model constrains in no way, so a caller may legally submit an option whose `Id` lies ahead of the counter inside the emulator's own `do-` namespace. When the counter later reaches that value, two delivery options of two DIFFERENT entities carry one id, and two assessments are stored under it.

From then on `ListAssessments` with `FrameworkFilters.AMISecurityFilters.DeliveryOptionId` reports BOTH assessments — `AssessmentRecord::matches_framework_filters` selects on `delivery_option_id` equality alone — so a caller asking for the assessments of one delivery option is handed the assessment of a delivery option of an entity it did not name, and `DescribeAssessment` of each reports the same `FrameworkSummary$DeliveryOptionId` for two unrelated options. `DescribeEntity` likewise reports an entity carrying a delivery-option id that names another entity's option.

The fix is the loop the three sibling minters already have: skip a candidate id the store already knows (the assessment records hold every delivery-option id this catalog has reported). Nothing else about the minted form changes, and no existing assertion about the first minted id moves while there is no collision.

## Evidence

MODEL — the id is documented UNIQUE on both the reported member and the filter that selects by it (smithy_query query="shape" shape="AMISecuritySummary,AMISecurityFilters"):
```
"AMISecuritySummary": ... "DeliveryOptionId": {"target":"ResourceId", "doc":"<p>The unique ID of the delivery option that was evaluated.</p>"}
"AMISecurityFilters": ... "DeliveryOptionId": {"target":"ResourceId", "doc":"<p>The unique ID of the delivery option whose AMI Security assessments you want to\n            list.</p>"}
```

SIDE 1 — the minter with NO uniqueness check. services/service-com-amazonaws-marketplacecatalog-awsmpseymour/src/state.rs:1975-1983:
```
    /// Mint a delivery option id for an option a change submitted without one,
    /// matching `ResourceId`'s `^[\w\-]+$`.
    pub fn mint_delivery_option_id(&mut self) -> String {
        self.delivery_option_seq = self.delivery_option_seq.wrapping_add(1);
        format!(
            "{DELIVERY_OPTION_ID_PREFIX}{:012x}",
            self.delivery_option_seq & 0xffff_ffff_ffff
        )
    }
```

SIDE 2 — its three siblings in the same `impl`, each of which loops until the id is unused. src/state.rs:1935-1944:
```
    /// Mint an unused change set id matching `ResourceId`'s `^[\w\-]+$`.
    pub fn mint_change_set_id(&mut self) -> String {
        loop {
            self.change_set_seq = self.change_set_seq.wrapping_add(1);
            let id = format!("cs-{:012x}", self.change_set_seq & 0xffff_ffff_ffff);
            if !self.change_sets.contains_key(&id) {
                return id;
```
src/state.rs:1946-1958:
```
    /// Mint an unused entity id matching `ResourceId`'s `^[\w\-]+$`.
    ///
    /// The loop also skips an id a running change set has already reserved, so
    /// two change sets in flight never name the same new entity.
    pub fn mint_entity_id(&mut self) -> String {
        loop {
            ...
            if !self.entities.contains_key(&id) && self.locking_change_set(&id).is_none() {
```
src/state.rs:1960-1973: `/// Mint an unused assessment id, in the `assessment-…` form` ... `if !self.assessments.contains_key(&id) { return id; }`.

WHERE THE COLLIDING ID IS ACCEPTED AND WHAT IT DECIDES — src/state.rs:2342-2351 (`identify_delivery_options`'s own doc):
```
    /// A delivery option is addressed by an id the CATALOG assigns — the id
    /// `FrameworkSummary$DeliveryOptionId` names and
    /// `AMISecurityFilters$DeliveryOptionId` selects by — so an option
    /// submitted without one is given an id here, which is then both what the
    /// entity's details carry and what the assessment of that option reports.
    /// An option submitted WITH an id keeps it: the change is updating the
    /// option that id names.
```
and src/state.rs:2371-2380, which mints only when the submitted `Id` is absent or empty:
```
            if option
                .get(DELIVERY_OPTION_ID_MEMBER)
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
            {
                let minted = self.mint_delivery_option_id();
```
The id is copied onto the record at src/state.rs:2434-2437 (`record_assessment`): `delivery_option_id: option.get(DELIVERY_OPTION_ID_MEMBER).and_then(Value::as_str).map(str::to_string),`.

THE FILTER THAT THEN CONFLATES THEM — src/state.rs:1801-1805:
```
    pub fn matches_framework_filters(&self, filters: &FrameworkFilters) -> bool {
        let (framework, delivery_option) = framework_filters_scope(filters);
        self.framework == framework
            && delivery_option.is_none_or(|wanted| self.delivery_option_id.as_deref() == Some(wanted))
```
No entity or change-set scoping, so one id selects every record carrying it.

THE CRATE'S OWN PINNED RULE THAT BREAKS — tests/all/assessment_production.rs:767-773:
```
        assert_eq!(
            ids,
            vec![option_id.clone()],
            "case {case:?}: the delivery-option filter selects that option's assessment \
             alone, got {:?}",
            by_option.body
        );
```
That test (`an_assessment_names_the_delivery_option_the_entity_carries`, tests/all/assessment_production.rs:617-774) exercises exactly a caller-supplied id beside an assigned one, but picks `let submitted_id = "do-submitted-by-the-caller";` (line 621) — a value the counter can never reach — so nothing in the suite covers a submitted id inside the minted namespace. tests/all/assessments.rs:439 shows a caller filtering by the first minted spelling `"DeliveryOptionId": "do-000000000001"`, i.e. ids of that exact form are values callers hold and send.

## Steps to Reproduce

1. POST /StartChangeSet {"Catalog":"AWSMarketplace","ChangeSet":[{"ChangeType":"CreateProduct","Entity":{"Type":"AmiProduct@1.0"},"DetailsDocument":{"ProductTitle":"first","DeliveryOptions":[{"Id":"do-000000000002","Details":{"AmiDeliveryOptionDetails":{"AmiSource":{"AmiId":"ami-0abcdef1234567890","AccessRoleArn":"arn:aws:iam::123456789012:role/MarketplaceScan","UserName":"ec2-user"},"SecurityGroups":[{"IpRanges":["10.0.0.0/16"]}],"UsageInstructions":"launch it"}}}]}}]} — the option keeps the submitted id and the delivery-option counter is untouched (state.rs:2371-2380).
2. Wait out CHANGE_SET_PREPARING + CHANGE_SET_APPLYING, then POST /ListAssessments {"Catalog":"AWSMarketplace"}: one assessment, whose /FrameworkSummary/AMISecuritySummary/DeliveryOptionId is "do-000000000002".
3. POST /StartChangeSet again, creating a SECOND AmiProduct whose DetailsDocument carries TWO delivery options, neither with an "Id" (same AmiDeliveryOptionDetails body as above, minus the Id member).
4. Wait out the run. The two options are assigned "do-000000000001" and "do-000000000002" by mint_delivery_option_id (state.rs:1977-1982), the second of which is the id entity one already carries. GET /DescribeEntity for the second entity shows an option whose Id names the first entity's option.
5. POST /ListAssessments {"Catalog":"AWSMarketplace","FrameworkFilters":{"AMISecurityFilters":{"DeliveryOptionId":"do-000000000002"}}}: TWO AssessmentSummary entries come back — one per entity — although the member is documented "The unique ID of the delivery option whose AMI Security assessments you want to list" and the crate's own test asserts the filter "selects that option's assessment alone".
6. POST /ListAssessments with {"AssessmentTargetFilter":{"EntityId":"<first entity id>"},"FrameworkFilters":{"AMISecurityFilters":{"DeliveryOptionId":"do-000000000002"}}} and the same for the second entity: both select a record, so the one id addresses two different delivery options.
7. Compare with the sibling minters: repeating the same scenario for entity ids (submit a change naming an entity id the counter has not reached) cannot collide, because mint_entity_id loops past an id already held or reserved (state.rs:1950-1958).
