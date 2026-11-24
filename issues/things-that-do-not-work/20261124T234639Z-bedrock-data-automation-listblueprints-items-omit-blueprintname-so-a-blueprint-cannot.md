Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: bedrock-data-automation__cloudformation.deploy-blueprint-and-project-round-trip-then-delete bedrock-data-automation__cloudformation.deploy-blueprint-invalid-type-enum-rejected
Issue-Key: bedrock-data-automation__cloudformation.deploy-blueprint-and-project-round-trip-then-delete|run#25|KeyError|bmatches = [b for b in blueprints if b["blueprintName"] == blueprint_name]
Issue-Key: bedrock-data-automation__cloudformation.deploy-blueprint-invalid-type-enum-rejected|run#8|KeyError|if b["blueprintName"] == good_name]
Issue-Commit: f8e20ae46548b9b285010d18fd783588395da7bd

# ListBlueprints items omit blueprintName, so a blueprint cannot be found by name in a listing

> _Note: this issue was identified against the code base at commit `f8e20ae46548b9b285010d18fd783588395da7bd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListBlueprints

## Description
Cause 1 of 1: ListBlueprints serves BlueprintSummary items that carry no `blueprintName` member, so no caller can resolve a blueprint's ARN from the name it created it under.

Failing checks: bedrock-data-automation__cloudformation.deploy-blueprint-and-project-round-trip-then-delete, bedrock-data-automation__cloudformation.deploy-blueprint-invalid-type-enum-rejected (both are listed in the suite report under the cross-service heading `### bedrock-data-automation__cloudformation`; the diverging call is bedrock-data-automation's ListBlueprints, which is the service this issue is filed under).

AWS: Smithy model services/service-com-amazonaws-bedrockdataautomation-amazonbedrockkeystonebuildtimeservice/smithy-model/com-amazonaws-bedrockdataautomation-amazonbedrockkeystonebuildtimeservice.json — `ListBlueprints` output member `blueprints` (@required) targets the list `Blueprints`, whose member is `BlueprintSummary` ("Summary of a Blueprint"). `BlueprintSummary` declares `blueprintArn` (@required), `creationTime` (@required), `blueprintVersion`, `blueprintStage`, `lastModifiedTime` and `blueprintName` (target BlueprintName, @length 1..128, @pattern ^[a-zA-Z0-9-_]+$, @sensitive). AWS populates `blueprintName` on every summary of an account-owned blueprint — that is what makes the listing an enumeration a client can key by the name it supplied to CreateBlueprint (or to the CloudFormation `AWS::Bedrock::Blueprint` `BlueprintName` property). The two checks assert exactly that: they read every page of ListBlueprints and select the item whose `blueprintName` equals the name they deployed. `blueprintName` being @sensitive governs LOGGING of the value, not whether it is serialized.

Emulator: the summary projection hardcodes the member away. `BlueprintRecord::to_summary` returns `BlueprintSummary { blueprint_arn, blueprint_version, blueprint_stage: None, blueprint_name: None, creation_time, last_modified_time: None }`, and ListBlueprints builds its page straight from it (`blueprints: page.iter().map(|record| record.to_summary()).collect()`). The record itself holds the name — the store keeps a `blueprint_names` name->ARN index and GetBlueprint reports `blueprintName` correctly — so the value exists and is simply dropped on the listing path. The doc comment above `to_summary` states the choice explicitly ("a listing fills only blueprintArn, blueprintVersion and creationTime ... blueprintStage, lastModifiedTime and the @sensitive blueprintName belong to the full Blueprint a read serves"). On the wire the member is therefore absent from each JSON item, and both checks die with `KeyError: 'blueprintName'` while iterating the page. The same projection also drops `blueprintStage` and `lastModifiedTime`, which AWS fills for an account-owned blueprint; the two failing checks only exercise `blueprintName`, so that is the grounded part — the sibling members are the same gap in the same function.

Why: a client that enumerates blueprints to find one by name gets a page of items it cannot identify. There is no ARN-from-name lookup in this API other than the listing, so the ARN of a blueprint created by name becomes unreachable: name-keyed reconciliation loops, "does this blueprint already exist" checks, and absence assertions after a delete all break. A typed SDK hands the caller an unset optional (or, in Python, a missing dict key), turning a fidelity gap into a crash rather than a wrong answer. The CloudFormation `AWS::Bedrock::Blueprint` round trip is affected the same way: the Ref'd ARN cannot be cross-checked against an independent listing.

Fix: the shared summary projection in this service's state — `BlueprintRecord::to_summary` must fill `blueprint_name` from the record's stored name (and, for the same reason, `blueprint_stage` and `last_modified_time` from the record) instead of `None`; ListBlueprints' handler needs no change beyond consuming the corrected projection.

Traces: services/service-com-amazonaws-bedrockdataautomation-amazonbedrockkeystonebuildtimeservice/src/state.rs lines 736-756 (`to_summary`, `blueprint_name: None`); services/service-com-amazonaws-bedrockdataautomation-amazonbedrockkeystonebuildtimeservice/src/ops/list_blueprints.rs line 282 (page built from `to_summary`). No existing open issue covers this: the open bedrock-data-automation issues concern InvokeBlueprintOptimizationAsync S3-URI validation, DeleteDataAutomationLibrary reference checking, and the ListDataAutomationLibraryEntities / InvokeDataAutomation stubs.

## Evidence
Expected (AWS): ListBlueprints returns, for each account-owned blueprint, a BlueprintSummary containing blueprintName set to the name the blueprint was created with, e.g. an item of the form {"blueprintArn": "<arn>", "blueprintName": "<the name supplied at create>", "blueprintVersion": "<n>", "blueprintStage": "LIVE"|"DEVELOPMENT", "creationTime": <date-time>, "lastModifiedTime": <date-time>}. The names/ARNs/timestamps are per-run values; the requirement is that the blueprintName member is PRESENT and equals the requested name.

Actual (emulator): every item in the `blueprints` list is serialized without a `blueprintName` member at all (also without `blueprintStage` and `lastModifiedTime`). HTTP status is 200 and no error is returned — the response is simply missing the member. Both checks fail on the first dereference of it:

  check_bedrock_data_automation__cloudformation_deploy_blueprint_and_project_round_trip_then_delete.py line 257:
    bmatches = [b for b in blueprints if b["blueprintName"] == blueprint_name]
    KeyError: 'blueprintName'

  check_bedrock_data_automation__cloudformation_deploy_blueprint_invalid_type_enum_rejected.py line 177:
    if b["blueprintName"] == good_name]
    KeyError: 'blueprintName'

In both cases the preceding CloudFormation stack reached CREATE_COMPLETE and the blueprint was created, so the listing did return items — they just carry no name. Source of the omission, verbatim from the checkout (state.rs, BlueprintRecord::to_summary):

    BlueprintSummary {
        blueprint_arn: self.blueprint_arn.clone(),
        blueprint_version: self.latest_version().map(|v| v.to_string()),
        blueprint_stage: None,
        blueprint_name: None,
        creation_time: self.creation_time,
        last_modified_time: None,
    }

## Steps to Reproduce
1. CreateBlueprint (or deploy a CloudFormation stack with an AWS::Bedrock::Blueprint resource) with BlueprintName set to a known name N, Type DOCUMENT and any valid Schema; note the returned blueprintArn A.
2. Call ListBlueprints (no filters, paginating to the end).
3. Inspect the items of the `blueprints` list.
   - AWS: exactly one item has blueprintName == N, and its blueprintArn == A.
   - Emulator: items contain blueprintArn/blueprintVersion/creationTime only; `blueprintName` is absent from every item, so selecting by name raises KeyError (Python) / yields an unset optional in a typed SDK.
4. Compare with GetBlueprint(blueprintArn=A), which does report blueprintName == N — showing the name is stored and only the listing projection drops it.

Replay: bedrock-data-automation__cloudformation.deploy-blueprint-and-project-round-trip-then-delete and bedrock-data-automation__cloudformation.deploy-blueprint-invalid-type-enum-rejected.
