Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.thing-versioning-and-recreate-identity
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|UpdateThing|ResourceNotFoundException
Issue-Commit: 4a31818efe332ca88a0519d4a493d7749df24842

# iot UpdateThing on a deleted thing returns the account-qualified not-found sentence ('Thing <account>:<name> not found.') where AWS words it 'Thing <name> not found.'

> _Note: this issue was identified against the code base at commit `4a31818efe332ca88a0519d4a493d7749df24842`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateThing

## Description
Cause 1 of 1: UpdateThing against a thingName the registry no longer holds is refused with the registry's ACCOUNT-QUALIFIED not-found sentence, but AWS IoT words UpdateThing's miss WITHOUT the account qualifier. Failing check: iot.thing-versioning-and-recreate-identity.

AWS: recorded behavior only for the message text. Smithy (services/service-com-amazonaws-iot-awsiotservice/smithy-model/com-amazonaws-iot-awsiotservice.json): UpdateThing is PATCH /things/{thingName} with error ResourceNotFoundException (errorKind client, httpError 404); DescribeThing is GET /things/{thingName} with the same modeled error. The model states no message text, so the wording requirement comes from the recording, which shows the two single-thing registry operations wording the SAME miss DIFFERENTLY and the check asserting both in one run:
  * DescribeThing on a deleted thing -> 404 ResourceNotFoundException, message `Thing <accountId>:<thingName> not found.` — fixed text `Thing `, the `:` separator and ` not found.`; variable parts are the account the call was made for and the thingName from the request.
  * UpdateThing on the same deleted thing -> 404 ResourceNotFoundException, message `Thing <thingName> not found.` — fixed text `Thing ` and ` not found.`; the ONLY variable part is the thingName from the request. There is no account id and no `:` separator.
HTTP status (404) and error code (ResourceNotFoundException) are identical for both.

Emulator: UpdateThing answers 404 ResourceNotFoundException — status and code correct — but with DescribeThing's account-qualified sentence, i.e. `Thing <emulator accountId>:<thingName> not found.`. In this replay the assertion read: expected message 'Thing <thingName> not found.', got 'Thing 000000000000:<thingName> not found.' (the `000000000000` is this environment's emulator account id and the thing name is a per-run generated name — neither value is part of the requirement; only the presence of the `<accountId>:` segment is). The DescribeThing assertion immediately before it passes in the same check, so the divergence is specifically that UpdateThing reuses the account-qualified registry sentence instead of UpdateThing's unqualified one.

Why: a client that distinguishes IoT refusals by message text — a provisioning or device-management tool that surfaces or matches the service's refusal, or a test asserting the refusal AWS actually sends — sees an extra `<accountId>:` segment real AWS never emits for this operation, and any parse that extracts the thing name out of the sentence takes the account id as part of the name. It also leaks the serving account id into a refusal where AWS does not.

Fix: the refusal wording UpdateThing's handler maps its registry miss through — the missing-thing sentence on the UpdateThing path must be the unqualified `Thing <thingName> not found.` form, while DescribeThing and the other registry readers whose recordings show the qualified form keep the account-qualified one; the two must stop sharing one sentence. This belongs in the UpdateThing handler's error mapping plus the IoT state's not-found wording.

Traces: iot.thing-versioning-and-recreate-identity — the UpdateThing assertion at check_iot_thing_versioning_and_recreate_identity.py line 78 (`_update_gone = ("ResourceNotFoundException", f"Thing {name} not found.")`), reached after the DescribeThing assertion at line 75 (`f"Thing {ctx.account}:{name} not found."`) passes. Likely code sites: services/service-com-amazonaws-iot-awsiotservice/src/ops/update_thing.rs (maps the registry refusal to ResourceNotFoundException via the refusal's message), services/service-com-amazonaws-iot-awsiotservice/src/state/thing_update.rs (`ThingUpdateRefusal::MissingThing`), services/service-com-amazonaws-iot-awsiotservice/src/state/not_found.rs (the registry not-found helper that prepends the emulator account id and `:` for every registry family).

Related open issue, which does NOT cover this check: 20261115T093926Z-iot-updatething-s-resourcenotfoundexception-message-is-account-qualified.md reports the same root cause but carries only the replay key of iot.create-thing-conflicting-attributes-fails; this check's key (iot.thing-versioning-and-recreate-identity) is not on it, so it is filed here so the fixer re-replays it too. A fix to that issue should close this one.

## Evidence
Check iot.thing-versioning-and-recreate-identity, verdict mismatch.

Replay output:
  File "check_iot_thing_versioning_and_recreate_identity.py", line 78, in run -> expect_error(...)
  File "check_iot_thing_versioning_and_recreate_identity.py", line 20, in expect_error
  assert err.get("Message") == message
  AssertionError: expected message 'Thing sim2real-behav-thing-fresh-a4dea46311ca not found.', got 'Thing 000000000000:sim2real-behav-thing-fresh-a4dea46311ca not found.'

Expected (AWS, from the recording):
  UpdateThing(thingName=<deleted thing>) -> HTTP 404, code ResourceNotFoundException, message `Thing <thingName> not found.`
  (fixed text `Thing ` + ` not found.`; sole variable part = the thingName from the request)

Actual (emulator):
  UpdateThing(thingName=<deleted thing>) -> HTTP 404, code ResourceNotFoundException, message `Thing <accountId>:<thingName> not found.`
  (the `000000000000:` prefix is the emulator's own account id and the thing name is a per-run generated value — neither is part of the requirement, only the extra `<accountId>:` segment is the defect)

Control in the SAME check that passes, proving the two wordings genuinely differ on AWS:
  DescribeThing(thingName=<deleted thing>) -> ResourceNotFoundException, `Thing <accountId>:<thingName> not found.` — asserted at line 75 and matched.

Status and error code agree; only the message's variable composition diverges.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. iot CreateThing(thingName=T, attributePayload={attributes:{env:"v1"}}).
3. iot UpdateThing(thingName=T, expectedVersion=1, attributePayload={attributes:{env:"v2"}}); DescribeThing shows version 2.
4. iot DeleteThing(thingName=T).
5. iot DescribeThing(thingName=T) -> 404 ResourceNotFoundException, message `Thing <accountId>:T not found.` (this is AWS's wording for DescribeThing; the emulator matches here).
6. iot UpdateThing(thingName=T, attributePayload={attributes:{env:"x"}}) -> emulator answers 404 ResourceNotFoundException with message `Thing <accountId>:T not found.`; AWS answers 404 ResourceNotFoundException with message `Thing T not found.` (no account id, no `:`).
7. Equivalently, replay the behavioral check iot.thing-versioning-and-recreate-identity; it fails only on that UpdateThing message assertion.
