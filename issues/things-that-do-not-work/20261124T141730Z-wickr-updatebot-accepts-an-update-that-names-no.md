Issue-Source: trace-sweep-divergence
Issue-Trace: wickr-bot-lifecycle-and-validation
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wickr-bot-lifecycle-and-validation wickr-bot-lifecycle-and-validation-errors
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr|UpdateBot|4xx->2xx|$.message
Issue-Key: wickr|UpdateBot|4xx->2xx|$.reasons
Issue-Key: wickr|UpdateBot|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: wickr|UpdateBot|4xx->2xx|status
Issue-Commit: 6e6fbc444dcb336fbb953ddb77d632192a6135a5

# wickr UpdateBot accepts an update that names no security group instead of refusing it 422 ValidationError

> _Note: this issue was identified against the code base at commit `6e6fbc444dcb336fbb953ddb77d632192a6135a5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateBot

## Description
One net-new cause in the wickr `UpdateBot` handler: it treats a request that names no security group as a partial update and applies it, where the service refuses the request as a validation failure on `groupId`.

Cause 1 of 1: `UpdateBot` does not require the request to name a security group.
  AWS: Smithy: UpdateBot is PATCH /networks/{networkId}/bots/{botId}, success 200, output UpdateBotResponse.message (GenericString, "A message indicating the result of the bot update operation."). `UpdateBotRequest.groupId` (GenericString, pattern ^[\S\s]*$) is documented as "The ID of the new security group to assign the bot to." and is NOT marked @required in the model, and the operation models ValidationError (errorKind client, httpError 422) with members `reasons` (ErrorDetailList of ErrorDetail{field,reason}) and `message`. The model therefore does not state this rule, so the rule itself is recorded behavior only: the recording shows that an UpdateBot request which does not carry a usable `groupId` is refused HTTP 422 ValidationError, carrying the `x-amzn-errortype` header, with `reasons` = exactly one ErrorDetail whose `field` is the refused member name (`groupId`) and whose `reason` is the fixed sentence `Security group id is required`, and with `message` UNSET — the service states the per-field detail only and no envelope sentence beside it. The reason sentence has NO variable parts: it names neither the network, the bot, nor any id, so the whole of it is fixed text and only the `field` value (`groupId`, the refused member's own name) comes from the request shape. Hypothesis: the service requires every UpdateBot to restate the bot's group assignment, so an update that omits it cannot be applied even though the member is not modelled @required.
  Emulator: answers HTTP 200 with the success envelope (`message` = `success`) and APPLIES the update. The handler treats an absent `groupId` as "leave the bot in the group it is already in" and asks nothing further about it, so no `reasons` list and no `x-amzn-errortype` header are ever produced for this request; only a `groupId` that IS present and resolves to no group of the network is refused (and that one is refused 404 ResourceNotFoundError, not 422).
  Why: a client relying on AWS refusing the request gets the opposite outcome and a silently mutated bot. Its 422 error path and its parsing of `reasons[*].field`/`reason` never run; the bot's displayName/password-presence/suspension are written from a request AWS rejected outright, so every later GetBot, ListBots and bot-count read of that network diverges from AWS for the rest of the session. Because the divergence flips a 4xx refusal into a 2xx mutation, retry and reconciliation logic built on AWS's behavior cannot recover.
  Fix: UpdateBot's input validation, in the operation handler — a presence check on `groupId` run BEFORE anything is read or written, alongside the existing group-membership and password-policy checks, returning the modelled ValidationError through the handler's existing `refused(field, reason)` helper so the 422 shape (`reasons[0].field`/`reasons[0].reason`, `message` unset, errortype header) matches the other validation refusals of this crate. The fixed reason sentence belongs with the crate's other fixed refusal texts in the state module rather than inline, so no other operation that accepts a bot `groupId` can word it differently.
  Traces: wickr-bot-lifecycle-and-validation#task#6 (cluster representative), also wickr-bot-lifecycle-and-validation-errors#task#8. Handler: services/service-com-amazonaws-wickr-wickradminapi/src/ops/update_bot.rs lines 196-208 (the only `groupId` handling: absent => skipped) and lines 257-259 (unconditional success envelope, `UPDATED_MESSAGE` at line 269); refusal helper at lines 281-285; shared fixed-text constants in src/state.rs lines 187-229.

Not covered by any open issue: the open wickr issues `20261122T054439Z-wickr-createbot-username-naming-rule-unenforced-and-both.md`, `20261122T054439Z-wickr-bot-projections-report-a-uname-for-a.md`, `20261121T040104Z-wickr-createbot-stub.md`, `20261121T071809Z-wickr-createbot-does-not-enforce-the-security-group-a92f22.md`, `20261120T064747Z-wickr-createnetwork-creates-on-an-invalid-networkname-and-answers.md` and `20261123T084914Z-wickr-getnetwork-refuses-an-unplaceable-networkid-as-401.md` all concern CreateBot, CreateNetwork, GetNetwork or the bot `uname` projection; none of them describes UpdateBot's missing `groupId` presence check, and none carries this replay key.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health answered 200 and the data listener accepts). Summary: 46 steps, 32 passed, 14 mismatched, score 69.6%.

Cause 1 — fixture wickr-bot-lifecycle-and-validation, step task#6, operation UpdateBot, verdict mismatch:
  status: expected 422, got 200
  $.reasons: missing (expected [{'field': 'groupId', 'reason': 'Security group id is required'}], got None)
  $.message: unexpected_present (expected None, got 'success')
  $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  affected (2): wickr-bot-lifecycle-and-validation#task#6, wickr-bot-lifecycle-and-validation-errors#task#8
  divergence_key: service=wickr|UpdateBot|status=422->200|keys=$.message,$.reasons,$headers.x-amzn-errortype,status

Fixed vs variable parts of the diverging message: the refusal's `reasons[0].reason` is entirely fixed text — `Security group id is required` — and `reasons[0].field` is the refused member's own name, `groupId`; nothing in the expected body is taken from the request or from emulator state, and the recorded response carries NO `message` member at all. The emulator sends no error body and instead sends the success member `message` = `success`.

Smithy evidence (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): UpdateBot PATCH /networks/{networkId}/bots/{botId}, success code 200, idempotent; input members networkId (required, httpLabel), botId (required, httpLabel), displayName, groupId, challenge (sensitive), suspend; errors BadRequestError(400), ForbiddenError(403), InternalServerError(500), RateLimitError(429), ResourceNotFoundError(404), UnauthorizedError(401), ValidationError(422). ValidationError members: reasons (ErrorDetailList) and message. UpdateBotRequest$groupId doc: "The ID of the new security group to assign the bot to."

## Steps to Reproduce
1. Boot the emulator from this checkout; create a wickr network, a security group in it, and a bot in that group (so a resolvable networkId/botId pair exists).
2. Send PATCH /networks/{networkId}/bots/{botId} with a body that does NOT carry `groupId` (e.g. only `displayName`, or only `suspend`).
3. Expect: HTTP 422 ValidationError with the x-amzn-errortype header set and body {"reasons":[{"field":"groupId","reason":"Security group id is required"}]} and no `message` member; the bot is left exactly as it was.
4. Observe: HTTP 200 with body {"message":"success"}, no `reasons`, no x-amzn-errortype header, and the submitted members written to the stored bot (visible on the next GetBot/ListBots).
5. Or replay the fixtures: wickr-bot-lifecycle-and-validation step task#6 and wickr-bot-lifecycle-and-validation-errors step task#8.
