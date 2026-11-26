Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 66f67299128e76851653b7e4af3d7969af517dc5

# EntityIdList suppresses a reserved-but-uncreated entity id only for Intent=VALIDATE, so a CANCELLED or FAILED change set reports an id no entity carries

_Filed: 2026-11-26T23:02:33Z_
_Operation: ListChangeSets | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `66f67299128e76851653b7e4af3d7969af517dc5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`ChangeSetRecord::entity_ids()` is the one place this crate decides which entity ids a change set's `ChangeSetSummaryListItem$EntityIdList` names, and it already implements the rule "a change set that applied nothing does not name the entity its creating change merely RESERVED". But it keys that rule on the submitted `Intent` (`self.intent == INTENT_VALIDATE`) instead of on whether the run actually applied anything.

The crate itself states the same "applied nothing" fact for two further terminal statuses of an `Intent=APPLY` change set:

* a change set that FAILED — `settle` takes the `FAILED` branch and never calls `apply_changes`, with the comment "A change set that failed applied nothing: the entities its changes described are not written";
* a change set that was CANCELLED — `CancelChangeSet` clears the schedule with the comment "the changes this change set carried are never applied".

In both of those cases a creating change's reserved id is still reported, so `ListChangeSets` hands back an `EntityIdList` naming an entity that does not exist — and never will, because `mint_entity_id` only ever increments `entity_seq`, so the released reservation is never minted again. `DescribeEntity` / `BatchDescribeEntities` answer 404 / `ResourceNotFoundException` for that id. That is exactly the dangling id the `VALIDATE` branch three lines above exists to keep out of the member, for exactly the same reason.

Second symptom of the same gap (same operation, same class): `ChangeSetRecord::matches_filter` resolves the `EntityId` filter through the same `entity_ids()`, so a `ListChangeSets` filtered by `EntityId = <reserved id>` SELECTS the cancelled or failed change set on an id the catalog holds no entity for — while the identical `VALIDATE` change set is correctly not selected.

## Evidence

The rule and the condition it is keyed on — src/state.rs:1210-1227:
```
    /// The entities this change set acts on, in submission order and without
    /// repeats — `ChangeSetSummaryListItem$EntityIdList`.
    pub fn entity_ids(&self) -> Vec<String> {
        let validating = self.intent == INTENT_VALIDATE;
        ...
            // A validation applies nothing, so the entities it is a part of are
            // the ones its changes NAMED: the id a creating change resolved to
            // is reserved for judging the request, and no entity carries it.
            if validating && change.creates {
                continue;
            }
```

The same "applied nothing" fact, stated for a FAILED change set — src/state.rs:2270-2281:
```
                if verdict.failed() {
                    record.status = STATUS_FAILED.to_string();
                    ...
                    // A change set that failed applied nothing: the entities
                    // its changes described are not written, and the ones they
                    // named are left as they were.
                    continue;
                }
```
(`apply_changes` is reached only on the line below, src/state.rs:2283-2285: `if record.intent == INTENT_APPLY { self.apply_changes(&id, due); }`.)

And for a CANCELLED change set — src/ops/cancel_change_set.rs:203-207:
```
        record.status = crate::state::STATUS_CANCELLED.to_string();
        record.end_time = Some(crate::state::iso8601(now));
        // Cancelling is what the clock no longer owes anything after: the
        // changes this change set carried are never applied.
        record.schedule.clear();
```

What the summary then reports — src/state.rs:1238-1247 (reached from src/ops/list_change_sets.rs:301-302 `matched.iter().map(|record| record.to_summary())`):
```
            // `EntityIdList` is documented "a maximum of 20 entities. It must
            // contain at least one entity", so a change set holding no entity
            // id — a `VALIDATE` change set whose changes all create, since a
            // validation applies nothing — reports NO member rather than an
            // empty list that bound excludes.
            entity_id_list: {
                let ids = self.entity_ids();
                (!ids.is_empty()).then_some(ids)
            },
```

The reservation is never reusable, so the reported id is permanently dangling — src/state.rs:2060-2068:
```
    pub fn mint_entity_id(&mut self) -> String {
        loop {
            self.entity_seq = self.entity_seq.wrapping_add(1);
            let id = format!("ent-{:012x}", self.entity_seq & 0xffff_ffff_ffff);
```

The filter side of the same helper — src/state.rs:1265-1268:
```
            "EntityId" => self
                .entity_ids()
                .iter()
                .any(|id| values.iter().any(|value| value == id)),
```

Model text for the member (smithy_query query="doc" shape="ChangeSetSummaryListItem" member="EntityIdList"): "This object is a list of entity IDs (string) that are a part of a change set. The entity ID list is a maximum of 20 entities. It must contain at least one entity."

Test coverage: no test pins the current answer. tests/all/entity_id_list_contract.rs:249-253 states the rule only as "plus one per change that creates an entity, which only an `APPLY` change set does" and its rows (lines 256-344) are all change sets that SUCCEED. tests/all/change_set_failures.rs:517-528 reads a FAILED change set's `EntityIdList` and asserts each id it finds answers 404 ("A change set that failed applied nothing: no entity it named exists") — it iterates the list, so it passes whether or not the reserved id is listed. tests/all/details_length_bound.rs:418, :523 and :1119 read `entity_ids_of` only for steps the test has already asserted SUCCEEDED (lines 411-416) or whose step-0 payload is inside `DETAILS_JSON_LIMIT` (lines 1060-1096). tests/all/cancel_change_set_window.rs never reads `EntityIdList`.

## Steps to Reproduce

1. POST /StartChangeSet with {"Catalog": "AWSMarketplace", "ChangeSet": [{"ChangeType": "CreateProduct", "Entity": {"Type": "AmiProduct@1.0"}, "DetailsDocument": {"ProductTitle": "titled"}}]} — the change names no Identifier, so StartChangeSet reserves a new entity id for it (src/ops/start_change_set.rs:647 `None => (store.mint_entity_id(), true)`). Note the ChangeSetId.
2. PATCH /CancelChangeSet?catalog=AWSMarketplace&changeSetId=<id> within the PREPARING window (CHANGE_SET_PREPARING = 250 ms, src/state.rs:69) → 200; GET /DescribeChangeSet?catalog=AWSMarketplace&changeSetId=<id> reports Status CANCELLED.
3. POST /ListChangeSets with {"Catalog": "AWSMarketplace"} → the summary for that change set carries EntityIdList: ["ent-000000000001"].
4. GET /DescribeEntity?catalog=AWSMarketplace&entityId=ent-000000000001 → 404 ResourceNotFoundException. The list named an entity that does not exist and, because mint_entity_id only increments, never will.
5. POST /ListChangeSets with {"Catalog": "AWSMarketplace", "FilterList": [{"Name": "EntityId", "ValueList": ["ent-000000000001"]}]} → the cancelled change set is still selected on that id.
6. Repeat steps 1 and 3-5 for the FAILED path instead of cancelling: start an Intent=APPLY change set whose creating change carries a details value the catalog cannot read (e.g. "DetailsDocument": {"ProductTitle": ["titled"]}, as tests/all/change_set_failures.rs:505 does), wait out CHANGE_SET_PREPARING + CHANGE_SET_APPLYING → Status FAILED, and ListChangeSets still reports the reserved id.
7. Contrast: send the step-1 body again with "Intent": "VALIDATE" and let it run to SUCCEEDED — ListChangeSets reports NO EntityIdList member at all (src/state.rs:1219-1221, 1244-1247), which is the answer the crate gives for the same fact that nothing was applied.
