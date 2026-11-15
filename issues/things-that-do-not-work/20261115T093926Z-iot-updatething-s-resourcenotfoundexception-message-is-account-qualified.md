Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.create-thing-conflicting-attributes-fails
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.create-thing-conflicting-attributes-fails|run#14|AssertionError|assert mutate_dead == ("ResourceNotFoundException", f"Thing {name} not found."), mutate_dead
Issue-Commit: 93b39d9773f21562abd34e3a61b3ac354e4ba79c

# iot UpdateThing's ResourceNotFoundException message is account-qualified ('Thing <account>:<name> not found.') where AWS words it 'Thing <name> not found.'

> _Note: this issue was identified against the code base at commit `93b39d9773f21562abd34e3a61b3ac354e4ba79c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateThing

## Description
Cause 1 of 1: UpdateThing on a thingName the registry does not hold is refused with the registry's ACCOUNT-QUALIFIED not-found sentence, but AWS IoT words UpdateThing's miss WITHOUT the account qualifier. Failing check: iot.create-thing-conflicting-attributes-fails.

AWS: recorded behavior only — the Smithy model (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json) models UpdateThing with PATCH /things/{thingName} and the error ResourceNotFoundException (errorKind client, httpError 404), documented only as "The specified resource does not exist."; it states no message text. The recording shows the two single-thing operations of the thing registry word the same miss DIFFERENTLY, and the check asserts both in the same run:
  * DescribeThing on a thing that is gone -> ResourceNotFoundException, message `Thing <accountId>:<thingName> not found.` (fixed text: the family noun `Thing `, the separator `:`, and ` not found.`; variable parts: the account the call was made for and the thingName from the request).
  * UpdateThing on the same gone thing -> ResourceNotFoundException, message `Thing <thingName> not found.` (fixed text: `Thing ` and ` not found.`; the ONLY variable part is the thingName from the request — there is NO account id and no `:` separator).
The HTTP status (404) and the error code (ResourceNotFoundException) are the same in both.

Emulator: UpdateThing answers 404 ResourceNotFoundException — status and code correct — but with the account-qualified DescribeThing wording. The replayed assertion shows the emulator returning `('ResourceNotFoundException', 'Thing <accountId>:<thingName> not found.')` where the expected value is `('ResourceNotFoundException', 'Thing <thingName> not found.')`; the account id the emulator interpolates is its own emulator account id. The DescribeThing assertion immediately before it passes, so the defect is specifically that UpdateThing reuses the registry's account-qualified sentence instead of UpdateThing's unqualified one.

Why: a client that distinguishes IoT refusals by message text — a provisioning or device-management tool that surfaces or matches the service's refusal, or a test that asserts the refusal AWS actually sends — reads an extra `<accountId>:` segment that real AWS never emits for this operation. Worse, the extra segment leaks the serving account id into a refusal where AWS does not, so a message captured against the emulator does not round-trip against AWS and any parse that splits the name out of the sentence takes the account id as part of the thing name.

Fix: the refusal wording UpdateThing's handler maps its registry refusal through — the missing-thing sentence used for the UpdateThing path (the `ThingUpdateRefusal::MissingThing` wording), not DescribeThing's. UpdateThing needs the unqualified `Thing <thingName> not found.` form while DescribeThing (and the other registry readers whose recordings show the qualified form) keep the account-qualified one, so the two must stop sharing a single sentence. This belongs in the IoT state's not-found wording plus the UpdateThing handler's error mapping; file and line citations are under Traces.

Traces: iot.create-thing-conflicting-attributes-fails (the UpdateThing assertion in check_iot_create_thing_conflicting_attributes_fails.py line 50, after the DescribeThing assertion at line 48 passes). Code sites: services/service-com-amazonaws-iot-awsiotservice/src/ops/update_thing.rs lines 186-218 (maps `ThingUpdateRefusal` to `ResourceNotFoundException` using `refusal.message()`), services/service-com-amazonaws-iot-awsiotservice/src/state/thing_update.rs lines 114-133 (`ThingUpdateRefusal::MissingThing` -> `missing_thing_message`), services/service-com-amazonaws-iot-awsiotservice/src/state/not_found.rs lines 32-48 (`registry_not_found` prepends `::engine::emulator_account_id()` and `:` for every registry family, which is what makes UpdateThing account-qualified). No open issue covers this: the nearest, 20261114T082236Z-iot-refusal-message-fidelity-7-causes-addthingtothinggroup-createstream.md, lists AddThingToThingGroup, CreateStream, CreateCertificateProvider, DescribeProvisioningTemplateVersion and GetTopicRule, not UpdateThing.

## Evidence
Check iot.create-thing-conflicting-attributes-fails, verdict mismatch.

Expected (AWS, from the recording):
  UpdateThing(thingName=<deleted thing>) -> HTTP 404, code ResourceNotFoundException, message `Thing <thingName> not found.`
  (fixed text `Thing ` + ` not found.`; sole variable part = thingName from the request)

Actual (emulator):
  UpdateThing(thingName=<deleted thing>) -> HTTP 404, code ResourceNotFoundException, message `Thing <accountId>:<thingName> not found.`
  Replay assertion: AssertionError: ('ResourceNotFoundException', 'Thing 000000000000:sim2real-behav-thing-conflict-532692367161 not found.')
  against `assert mutate_dead == ("ResourceNotFoundException", f"Thing {name} not found.")`
  (the `000000000000:` prefix is the emulator's own account id and the thing name is a per-run generated name — neither value is part of the requirement, only the presence/absence of the `<accountId>:` segment is)

Control in the same check that PASSES, showing the two wordings are genuinely different on AWS:
  DescribeThing(thingName=<deleted thing>) -> ResourceNotFoundException, `Thing <accountId>:<thingName> not found.` — asserted and matched.

Status and error code agree; only the message's variable composition diverges.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. iot CreateThing(thingName=T, attributePayload={attributes:{model:"x1"}}).
3. iot DeleteThing(thingName=T).
4. iot DescribeThing(thingName=T) -> observe 404 ResourceNotFoundException with message `Thing <accountId>:T not found.` (this is the AWS wording for DescribeThing).
5. iot UpdateThing(thingName=T, attributePayload={attributes:{model:"y"}}) -> emulator answers 404 ResourceNotFoundException with message `Thing <accountId>:T not found.`; AWS answers 404 ResourceNotFoundException with message `Thing T not found.` (no account id, no `:`).
6. Equivalently, replay the behavioral check iot.create-thing-conflicting-attributes-fails; it fails on the UpdateThing assertion only.
