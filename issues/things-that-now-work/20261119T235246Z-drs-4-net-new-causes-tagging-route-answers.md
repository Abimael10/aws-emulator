Issue-Source: trace-sweep-divergence
Issue-Trace: drs-tag-resource-lifecycle
Issue-Step: task#9
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: drs-tag-resource-lifecycle drs-launch-configuration-template-lifecycle
Issue-Shape-Id: com.amazonaws.drs#ElasticDisasterRecoveryService
Issue-Key: drs|TagResource|4xx->4xx|$.message
Issue-Key: drs|TagResource|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: drs|TagResource|4xx->4xx|status
Issue-Key: drs|TagResource|2xx->2xx|status
Issue-Key: drs|UntagResource|2xx->2xx|status
Issue-Key: drs|DescribeLaunchConfigurationTemplates|2xx->2xx|$.items[*].copyPrivateIp
Issue-Key: drs|DescribeLaunchConfigurationTemplates|2xx->2xx|$.items[*].launchDisposition
Issue-Key: drs|DescribeLaunchConfigurationTemplates|2xx->2xx|$.items[*].recoveryMode
Issue-Commit: c3a7d511a5670169ecde182d84092cb0c8e6535e

# drs: 4 net-new causes — tagging route answers 200 instead of 204, refuses an unsupported resource ARN as 404 NotFoundException instead of 400, and a launch-configuration-template read-back is stale

> _Note: this issue was identified against the code base at commit `c3a7d511a5670169ecde182d84092cb0c8e6535e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
Four net-new wire divergences in the drs (Elastic Disaster Recovery) emulator: three in the shared resource-tagging route (/tags/{resourceArn}) and one in the launch-configuration-template read-back. All four are unowned by any open issue (the replay reports no owning issue and no blocking stub for them).

Cause 1 of 4: TagResource answers HTTP 400 BadRequestException "Invalid input resource arn" on AWS for a resourceArn that is not a taggable DRS resource; the emulator answers 404 NotFoundException "Resource was not found". (fixture drs-tag-resource-lifecycle, step task#9)
AWS: Smithy (com.amazonaws.drs) models TagResource POST /tags/{resourceArn} with input resourceArn: ARN @httpLabel @required @length(20,2048) @pattern ^arn:.{16,2044}$ and tags: TagsMap @required, output Unit, and errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400). The model declares NO BadRequestException shape at all and says nothing about which resource kinds the generic tag route supports, so this refusal is recorded behavior only: the recorded response is HTTP 400 with message exactly "Invalid input resource arn" (the recording asserts the status and the message literally, and asserts only the SHAPE of x-amzn-errortype; the already-open behavioral issue 20261119T225730Z-drs-tagging-route-accepts-a-launch-configuration-template.md records the same refusal's error code as BadRequestException). That message is entirely FIXED text — no part of it is interpolated from the request or from state: it carries no ARN, no id and no resource kind. The refusal is an INPUT fault about the ARN's resource kind, not an existence check, so it is independent of whether anything with that ARN exists, and 404/not-found stays reserved for a well-formed ARN of a SUPPORTED kind that state does not hold.
Emulator: the step gets HTTP 404 with $.message "Resource was not found" and x-amzn-errortype "NotFoundException" (expected status 400, expected message "Invalid input resource arn"). The tagging route's one refusal class is a 404, built in state/tags.rs, and it is raised for every ARN that resolves to no resource — an unsupported resource kind and an absent supported resource are collapsed into the same answer.
Why: a client cannot tell "you addressed something the DRS tag API does not tag" from "that DRS resource is gone". Code that branches on BadRequestException (to fall back to create-time tagging, or to fail fast on a mis-built ARN) never fires, retry/cleanup logic that treats 404 as a benign already-deleted condition silently swallows a permanently malformed request, and a 4xx class change moves the error out of the client's catch for the input fault.
Fix: the tagging route's ARN resolution and its refusal classes — the route must classify the resourceArn's resource kind BEFORE any state lookup and refuse an unsupported kind with HTTP 400, error code BadRequestException and the fixed message "Invalid input resource arn"; only a supported kind absent from state may answer 404. Related open issues that do NOT cover this step: 20261119T114813Z-drs-tag-apis-listtagsforresource-drops-tagresource-added-keys.md reports the same 404-instead-of-400 fault on ListTagsForResource (and the replay marks the ListTagsForResource step drs-tag-resource-lifecycle#task#10 as owned by it, so it is not bundled here), and 20261119T225730Z-drs-tagging-route-accepts-a-launch-configuration-template.md reports TagResource ACCEPTING a launch-configuration-template ARN with 200; neither covers TagResource answering 404 for this ARN.
Traces: drs-tag-resource-lifecycle#task#9.

Cause 2 of 4: TagResource answers HTTP 200 on success where AWS answers 204 No Content. (fixture drs-tag-resource-lifecycle, step task#5)
AWS: Smithy models TagResource output as Unit and its http trait carries NO explicit code (smithy_query reports http {method POST, uri /tags/{resourceArn}, code 200, codeDefaulted: true} — the 200 is the protocol default, not a modelled requirement), so the success status is recorded behavior only: the recording shows a successful tag write answered with HTTP 204 and an empty body.
Emulator: the step gets HTTP 200 (expected 204); the response body matches otherwise. The operation hard-codes the default success status (const SUCCESS_STATUS: u16 = 200u16).
Why: a client that distinguishes 204 from 200 — a strict assertion on the status, an HTTP layer or proxy that expects no body for a no-content reply, or a test harness recorded against AWS — sees a different response class for the same successful mutation.
Fix: the operation's success-status declaration / serializer for TagResource, which must answer 204 with an empty body on success.
Traces: drs-tag-resource-lifecycle#task#5.

Cause 3 of 4: UntagResource answers HTTP 200 on success where AWS answers 204 No Content. (fixture drs-tag-resource-lifecycle, step task#7)
AWS: Smithy models UntagResource DELETE /tags/{resourceArn} (resourceArn @httpLabel @required, tagKeys @httpQuery @required) with output Unit and, again, no explicit http code (code 200, codeDefaulted: true), so the status is recorded behavior only: the recording shows a successful untag answered with HTTP 204 and an empty body.
Emulator: the step gets HTTP 200 (expected 204). Same cause as above in the sibling operation (const SUCCESS_STATUS: u16 = 200u16).
Why: same as cause 2 — the successful mutation is reported in a different response class than AWS uses, so status-sensitive clients and recordings diverge.
Fix: the operation's success-status declaration / serializer for UntagResource, which must answer 204 with an empty body on success.
Traces: drs-tag-resource-lifecycle#task#7.

Cause 4 of 4: DescribeLaunchConfigurationTemplates reports a template's CREATE-time settings after the lifecycle mutated them, so the read-back is stale. (fixture drs-launch-configuration-template-lifecycle, step task#5)
AWS: Smithy models DescribeLaunchConfigurationTemplates POST /DescribeLaunchConfigurationTemplates -> {items: LaunchConfigurationTemplates, nextToken}, whose item LaunchConfigurationTemplate carries launchDisposition (LaunchDisposition: STARTED|STOPPED), copyPrivateIp (Boolean) and recoveryMode (RecoveryMode: FAST|OPTIMAL). The model states no defaults for those members, so what a read must report is recorded behavior only: a read of a template reports the settings as they stand after the most recent accepted mutation of that template, and the recorded step asserts launchDisposition STARTED, copyPrivateIp false and recoveryMode OPTIMAL for items[0].
Emulator: items[0].launchDisposition is "STOPPED", items[0].copyPrivateIp is true and items[0].recoveryMode is "FAST" — the three settings the fixture changes earlier in the lifecycle are all reported at their pre-change values, while every other member of the item matches. Describe itself is faithful (it renders the stored record verbatim), and so is Create (it stores exactly what the request supplied), so the stale values mean the template record was never updated. Hypothesis: the lifecycle's settings change goes through UpdateLaunchConfigurationTemplate, which in this checkout is still a 501 scaffold stub, so the stored record keeps its create-time values and this read diverges as the downstream symptom.
Why: a client that updates a launch configuration template and then reads it back is told its change did not take, and any recovery launched from the emulator's template would use the old disposition/recovery mode — the account-level launch settings a caller believes are in force are not the ones reported.
Fix: the template-mutating handler and the stored record it writes (crate::state::launch_configuration_templates::LaunchConfigurationTemplateRecord): the account's launch-configuration-template settings must be persisted by the mutation and reported by the read. Already open and NOT covering this read-back step: 20261119T225730Z-drs-updatelaunchconfigurationtemplate-stub.md (UpdateLaunchConfigurationTemplate is an unimplemented scaffold stub) — the replay does not mark this Describe cluster as blocked or owned, so closing the stub must be verified by re-replaying THIS step.
Traces: drs-launch-configuration-template-lifecycle#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, 68 steps, 40 passed, score 58.8%).

Cause 1 — fixture drs-tag-resource-lifecycle step task#9, TagResource: expected HTTP 400 with $.message "Invalid input resource arn" and a shaped x-amzn-errortype; actual HTTP 404 with $.message "Resource was not found" and x-amzn-errortype "NotFoundException". Divergence key service=drs|TagResource|status=400->404|keys=$.message,$headers.x-amzn-errortype,status. Code: services/service-com-amazonaws-drs-elasticdisasterrecoveryservice/src/ops/tag_resource.rs lines 190-192 (resolve-or-404) and src/state/tags.rs lines 151-169 (NOT_FOUND_MESSAGE = "Resource was not found", 404 NotFoundException is the route's only refusal) plus lines 58-73 (TaggedResource::in_arn returns None for any unsupported resource kind, which becomes that 404).

Cause 2 — fixture drs-tag-resource-lifecycle step task#5, TagResource: expected status 204, actual 200 (body otherwise matching). Divergence key service=drs|TagResource|status=204->200|keys=status. Code: src/ops/tag_resource.rs line 168 (const SUCCESS_STATUS: u16 = 200u16). Model: smithy_query operation=TagResource -> http {method: POST, uri: /tags/{resourceArn}, code: 200, codeDefaulted: true}, output_shape Unit.

Cause 3 — fixture drs-tag-resource-lifecycle step task#7, UntagResource: expected status 204, actual 200. Divergence key service=drs|UntagResource|status=204->200|keys=status. Code: src/ops/untag_resource.rs line 166 (const SUCCESS_STATUS: u16 = 200u16). Model: smithy_query operation=UntagResource -> http {method: DELETE, uri: /tags/{resourceArn}, code: 200, codeDefaulted: true}, output_shape Unit.

Cause 4 — fixture drs-launch-configuration-template-lifecycle step task#5, DescribeLaunchConfigurationTemplates: HTTP 200 as expected, but $.items[0].launchDisposition expected 'STARTED', got 'STOPPED'; $.items[0].copyPrivateIp expected false, got true; $.items[0].recoveryMode expected 'OPTIMAL', got 'FAST'. Divergence key service=drs|DescribeLaunchConfigurationTemplates|status=200->200|keys=$.items[0].copyPrivateIp,$.items[0].launchDisposition,$.items[0].recoveryMode. Code: src/ops/describe_launch_configuration_templates.rs lines 181-231 (renders the stored record verbatim), src/state/launch_configuration_templates.rs lines 57-99 (record + to_wire, every setting held exactly as the create supplied it), src/ops/create_launch_configuration_template.rs lines 213-245 (stores the create request's values), src/ops/update_launch_configuration_template.rs line 178 (Scaffolded::not_implemented("UpdateLaunchConfigurationTemplate") — the mutation never lands). Model: smithy_query shape=LaunchConfigurationTemplate (launchDisposition -> LaunchDisposition [STOPPED, STARTED], recoveryMode -> RecoveryMode [FAST, OPTIMAL], copyPrivateIp -> Boolean; no defaults declared).

## Steps to Reproduce
1. git clone the repo and build/boot the drs emulator from this checkout (services/service-com-amazonaws-drs-elasticdisasterrecoveryservice).
2. Replay trace set simparity-wire-aws-v1.
3. Cause 1: fixture drs-tag-resource-lifecycle, step task#9 — POST /tags/{resourceArn} with a resourceArn that is not a taggable DRS resource and a tags map; expect HTTP 400, error code BadRequestException, message exactly "Invalid input resource arn"; observe HTTP 404, x-amzn-errortype NotFoundException, message "Resource was not found".
4. Cause 2: fixture drs-tag-resource-lifecycle, step task#5 — POST /tags/{resourceArn} for an existing DRS resource with a valid tags map; expect HTTP 204 with an empty body; observe HTTP 200.
5. Cause 3: fixture drs-tag-resource-lifecycle, step task#7 — DELETE /tags/{resourceArn}?tagKeys=... for an existing DRS resource; expect HTTP 204 with an empty body; observe HTTP 200.
6. Cause 4: fixture drs-launch-configuration-template-lifecycle — create a launch configuration template, change its launchDisposition/copyPrivateIp/recoveryMode through the lifecycle's update step, then at step task#5 POST /DescribeLaunchConfigurationTemplates for that id; expect items[0] to report the post-update settings (launchDisposition STARTED, copyPrivateIp false, recoveryMode OPTIMAL); observe the create-time settings (STOPPED / true / FAST).
7. Re-replay all four steps (drs-tag-resource-lifecycle#task#9, #task#5, #task#7 and drs-launch-configuration-template-lifecycle#task#5) to confirm a fix: each is a separate cause and must pass on its own.
