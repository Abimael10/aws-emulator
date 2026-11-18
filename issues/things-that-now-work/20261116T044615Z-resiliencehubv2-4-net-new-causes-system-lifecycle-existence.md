Issue-Source: trace-sweep-divergence
Issue-Trace: resiliencehubv2-system-association-and-delete-conflict
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: resiliencehubv2-system-association-and-delete-conflict resiliencehubv2-system-lifecycle resiliencehubv2-system-validation-and-conflict-errors
Issue-Shape-Id: com.amazonaws.resiliencehubv2#NGRHServiceCore
Issue-Key: resiliencehubv2|UpdateService|4xx->2xx|$.message
Issue-Key: resiliencehubv2|UpdateService|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: resiliencehubv2|UpdateService|4xx->2xx|status
Issue-Key: resiliencehubv2|GetSystem|4xx->2xx|$.message
Issue-Key: resiliencehubv2|GetSystem|4xx->2xx|$.resourceId
Issue-Key: resiliencehubv2|GetSystem|4xx->2xx|$.resourceType
Issue-Key: resiliencehubv2|GetSystem|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: resiliencehubv2|GetSystem|4xx->2xx|status
Issue-Key: resiliencehubv2|GetSystem|2xx->2xx|$.system.description
Issue-Key: resiliencehubv2|CreateSystem|2xx->2xx|$.system.systemId
Issue-Commit: 54eea575b4ba415294c52286e8229e8e478313b2

# resiliencehubv2: 4 net-new causes — System lifecycle existence checks, mutations and minted id shape diverge from AWS

> _Note: this issue was identified against the code base at commit `54eea575b4ba415294c52286e8229e8e478313b2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Four distinct root causes in the resiliencehubv2 System lifecycle, found by replaying the wire fixture corpus simparity-wire-aws-v1. A fixer must close ALL FOUR before submitting.

Related open issues, none of which covers these wire steps: 20261111T074347Z-resiliencehubv2-error-bodies-diverge-from-aws-validation-messages.md owns the validation-message wording and clientToken-conflict steps of this corpus; 20261113T163326Z-resiliencehubv2-tagresource-empty-response-omits-the-content-length.md owns the TagResource header step; 20261115T181817Z-resiliencehubv2-2-net-new-causes-updateservice-accepts-associations.md is a BEHAVIORAL issue (Issue-Corpus: simparity-behavioral-aws-v1) that describes the same UpdateService and DeleteSystem faults as causes 1 and 2 here but carries NO wire replay key, so the fixture steps below stay un-gated by it; 20261110T225437Z-resiliencehubv2-4-net-new-causes-availabilityslo-target-allowed.md covers availabilitySlo/clientToken/duplicate-name causes only.

Cause 1 of 4: UpdateService accepts an `associatedSystems` entry whose systemArn names no stored system, instead of refusing the call.
AWS: Smithy: UpdateService (POST /v2/update-service) input member `associatedSystems` targets AssociatedSystemList (@length max 20); each AssociatedSystem carries a @required `systemArn` of type Arn (@length min 31, pattern ^arn:(aws|aws-cn|aws-iso|aws-iso-[a-z]{1}|aws-us-gov):...); the operation models ValidationException (HTTP 400) and ResourceNotFoundException (HTTP 404). A well-formed ARN satisfies the pattern, so the reference has to be RESOLVED against the account's systems rather than only pattern-checked. Recorded response for an ARN that is well formed but names no system: HTTP 400 with an `x-amzn-errortype` header and body `message` = "System not found: <systemArn>" — the fixed text is `System not found: ` and the variable part is the offending systemArn taken verbatim from the request's associatedSystems entry (the recording's own value, a system ARN with extra characters appended to the system id, is only one instance of that template).
Emulator: the call succeeds with HTTP 200, the dangling reference is persisted and echoed back, and no error body is produced at all — the replay reports `status: expected 400, got 200`, `$.message: missing`, `$headers.x-amzn-errortype: missing` for two consecutive steps of the same fixture.
Why: a client that relies on AWS rejecting an unknown system silently records a broken association. Negative-path handling never sees the ValidationException it is written for, GetService/ListServices then report a membership that does not exist, and a typo or stale ARN becomes corrupt state instead of a 400.
Fix: UpdateService's handler/validation — resolve every `associatedSystems[].systemArn` against the system store before merging, and return the modeled ValidationException (400, message `System not found: <systemArn>`) leaving the record untouched when any entry does not resolve. CreateService's equivalent path needs the same gate.
Traces: resiliencehubv2-system-association-and-delete-conflict#task#5 (and #task#6). Component: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/update_service.rs, which merges `input.associated_systems` into the record with no lookup in the store's `systems` map.

Cause 2 of 4: a deleted system is still readable — GetSystem answers 200 with the system body where AWS answers 404, because DeleteSystem is an unimplemented scaffold so the record is never removed.
AWS: Smithy: GetSystem (GET /v2/get-system) models ResourceNotFoundException (HTTP 404), whose shape carries a @required `message` plus `resourceId` ("The identifier of the resource that was not found") and `resourceType` ("The type of the resource that was not found"). Recorded response after the system has been deleted: HTTP 404 with `x-amzn-errortype`, body `message` = "System not found: <systemId>" (fixed text `System not found: ` plus the system's own id from state — NOT the ARN), `resourceId` = that same systemId, `resourceType` = "System".
Emulator: GetSystem answers HTTP 200 with the full system body; the replay reports `status: expected 404, got 200` and `$.message`, `$.resourceId`, `$.resourceType`, `$headers.x-amzn-errortype` all missing, in two different fixtures. GetSystem's own not-found path is correct (services/.../src/ops/get_system.rs returns ResourceNotFoundException when `store.system_view(..)` is None); the system is still in the store because services/.../src/ops/delete_system.rs is the untouched scaffold returning `Scaffolded::not_implemented("DeleteSystem")` (HTTP 501), so the delete step preceding the read never removes anything.
Why: a client cannot ever observe the post-delete contract that tells it a system is gone. Teardown and reconciliation loops read a system they just deleted as live, systems accumulate for the life of the emulator, and the referential guard that protects an in-use system (ConflictException while a service is still associated) is absent as well.
Fix: implement DeleteSystem in its handler file over the system store — refuse with the modeled ConflictException (409) while any service still lists the system in `associatedSystems`, otherwise remove the record (and its name/clientToken index entries), echo `systemArn`, and delete the SCAFFOLDED marker line — so GetSystem's existing 404 path (message `System not found: <systemId>`, resourceId=<systemId>, resourceType=System) becomes reachable.
Traces: resiliencehubv2-system-association-and-delete-conflict#task#12 and resiliencehubv2-system-lifecycle#task#7.

Cause 3 of 4: a system's description update never lands — UpdateSystem is an unimplemented scaffold, so the next read answers the creation-time value.
AWS: Smithy: UpdateSystem (POST /v2/update-system) takes @required `systemArn` (Arn) plus `description` (EntityDescription, @length max 500) and `sharingEnabled` (Boolean), returns the @required `system` (System) and models AccessDeniedException 403, ConflictException 409, InternalServerException 500, ResourceNotFoundException 404, ValidationException 400. Recorded behavior: the value supplied in `description` replaces the stored one, and both UpdateSystem's own response and the following GetSystem report `system.description` as the NEW value (`updatedAt` advances with it).
Emulator: UpdateSystem answers HTTP 501 `NotImplemented` (services/.../src/ops/update_system.rs returns `Scaffolded::not_implemented("UpdateSystem")` and still carries the `SCAFFOLDED, NOT IMPLEMENTED` marker), so nothing is written and the subsequent GetSystem serves the stale description — the replay reports `$.system.description: value_mismatch` with the pre-update text where the trace expects the updated text.
Why: systems are effectively immutable after creation. A client's edit silently has no effect on a later read (the read is a 200, not an error, so nothing surfaces the loss), and any flow that updates metadata and then reconciles it diverges permanently from AWS.
Fix: implement UpdateSystem in its handler file against the system store — resolve `systemArn` (404 ResourceNotFoundException when it names nothing), apply `description`/`sharingEnabled`, bump `updatedAt`, return the stored projection used by CreateSystem/GetSystem, and delete the SCAFFOLDED marker line.
Traces: resiliencehubv2-system-lifecycle#task#4.

Cause 4 of 4: CreateSystem mints a systemId that is a version-8 UUID, where AWS's systemId has the shape of a version-4 UUID.
AWS: Smithy: System$systemId targets SystemId (@length 1..255, pattern ^\S{1,255}$) — the model does not pin the format, so recorded behavior only: every recorded systemId (and the resource segment of the system ARN that carries it) is a lowercase RFC-4122 version-4 UUID — 8-4-4-4-12 hex groups with `4` as the first nibble of the third group and 8/9/a/b as the first nibble of the fourth. The fixture asserts the SHAPE of `$.system.systemId`, not a literal value, so no per-run id is being required here.
Emulator: the minted id is a UUID whose third group starts with `8` (version 8), e.g. the replayed `...-8cdf-...` form, so the shaped assertion on `$.system.systemId` fails on CreateSystem in two fixtures. The generator is deliberate: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/state.rs `mint_uuid` sets `bytes[6] = (bytes[6] & 0x0f) | 0x80` ("Version 8 (custom)"). The same generator feeds policy ids and the resource segment of every ARN this crate mints.
Why: a client or tool that parses or validates the id as a version-4 UUID (SDK-adjacent validation, IaC/CloudFormation id patterns, id-version assertions in tests) rejects or mis-handles ids the emulator mints, even though the string length and charset look right — so ids do not round-trip through v4-aware consumers the way AWS's do.
Fix: the shared id generator in the service's state module — set the version nibble to 4 (`bytes[6] = (bytes[6] & 0x0f) | 0x40`) while keeping the RFC variant nibble, so every minted system/policy/service id has AWS's shape.
Traces: resiliencehubv2-system-lifecycle#task#1 and resiliencehubv2-system-validation-and-conflict-errors#task#2.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 124 steps, 55 passed, score 44.4%).

Cause 1 — fixture resiliencehubv2-system-association-and-delete-conflict, step task#5 (cluster also covers task#6), operation UpdateService:
  Expected (AWS recording): HTTP 400, x-amzn-errortype present, $.message = "System not found: <systemArn from the request>".
  Actual (emulator): HTTP 200; replay detail: "status: expected 400, got 200 / $.message: missing (expected 'System not found: arn:aws:resiliencehub:<region>:<account>:system/<id>zz', got None) / $headers.x-amzn-errortype: missing".

Cause 2 — fixture resiliencehubv2-system-association-and-delete-conflict, step task#12 (cluster also covers resiliencehubv2-system-lifecycle#task#7), operation GetSystem:
  Expected (AWS recording): HTTP 404, x-amzn-errortype present, $.message = "System not found: <systemId>", $.resourceId = <systemId>, $.resourceType = "System".
  Actual (emulator): HTTP 200 with the system body; replay detail: "status: expected 404, got 200 / $.message: missing / $.resourceId: missing / $.resourceType: missing / $headers.x-amzn-errortype: missing".
  Source: services/service-com-amazonaws-resiliencehubv2-ngrhservicecore/src/ops/delete_system.rs returns Scaffolded::not_implemented("DeleteSystem") (501), so the preceding delete removes nothing; get_system.rs:152-161 would have answered the 404 had the record been gone.

Cause 3 — fixture resiliencehubv2-system-lifecycle, step task#4, operation GetSystem:
  Expected (AWS recording): $.system.description = the value the preceding UpdateSystem supplied ("...updated" text).
  Actual (emulator): $.system.description = the creation-time description; replay detail: "$.system.description: value_mismatch (expected 'sim2real probe system, updated', got 'sim2real probe system')".
  Source: services/.../src/ops/update_system.rs:161 returns Scaffolded::not_implemented("UpdateSystem"); marker at line 4.

Cause 4 — fixture resiliencehubv2-system-lifecycle, step task#1 (cluster also covers resiliencehubv2-system-validation-and-conflict-errors#task#2), operation CreateSystem:
  Expected (AWS recording): $.system.systemId shaped as a lowercase version-4 UUID (shape asserted, value not).
  Actual (emulator): a version-8 UUID; replay detail: "$.system.systemId: value_mismatch (expected '<shaped-not-asserted>', got '151a9fa8-790e-8cdf-b21f-446ddc8d91eb')" — third group begins with 8, not 4.
  Source: services/.../src/state.rs mint_uuid, line ~620: bytes[6] = (bytes[6] & 0x0f) | 0x80 with the comment "Version 8 (custom)".

## Steps to Reproduce
Prerequisites: clone the repo, build/boot the emulator, point an AWS SDK client for resiliencehubv2 at it.

Cause 1:
1. CreateSystem(name=<unique>) -> note systemArn.
2. CreateService(name=<unique>, regions=[<region>], permissionModel={invokerRoleName:<an IAM role trusting resiliencehub.amazonaws.com>}) -> note serviceArn.
3. Build a well-formed but unknown system ARN (append characters to the system id of the ARN from step 1).
4. UpdateService(serviceArn=<svc>, associatedSystems=[{systemArn:<unknown>}]).
   Expected: HTTP 400 ValidationException, message "System not found: <unknown>". Observed: HTTP 200 with the dangling association persisted.

Cause 2:
1. CreateSystem(name=<unique>) -> note systemArn and system.systemId.
2. DeleteSystem(systemArn=<that arn>) (today: HTTP 501 NotImplemented).
3. GetSystem(systemArn=<that arn>).
   Expected: HTTP 404 ResourceNotFoundException, message "System not found: <systemId>", resourceId=<systemId>, resourceType="System". Observed: HTTP 200 with the system still present.

Cause 3:
1. CreateSystem(name=<unique>, description="<initial>") -> note systemArn.
2. UpdateSystem(systemArn=<that arn>, description="<initial>, updated").
   Expected: HTTP 200 with system.description = the new text. Observed: HTTP 501 NotImplemented.
3. GetSystem(systemArn=<that arn>).
   Expected: system.description = the new text. Observed: the initial text.

Cause 4:
1. CreateSystem(name=<unique>).
2. Inspect system.systemId (and the resource segment of system.systemArn).
   Expected: a lowercase version-4 UUID (third group starts with '4', fourth with 8/9/a/b). Observed: the third group starts with '8' (version 8), e.g. 151a9fa8-790e-8cdf-b21f-446ddc8d91eb.
