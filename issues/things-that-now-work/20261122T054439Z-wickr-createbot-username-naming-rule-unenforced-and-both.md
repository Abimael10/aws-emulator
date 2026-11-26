Issue-Source: trace-sweep-divergence
Issue-Trace: wickr-bot-lifecycle-and-validation
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wickr-bot-lifecycle-and-validation wickr-bot-lifecycle-and-validation-errors wickr-create-bot-in-security-group
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr|CreateBot|4xx->2xx|$.message
Issue-Key: wickr|CreateBot|4xx->2xx|$.reasons
Issue-Key: wickr|CreateBot|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: wickr|CreateBot|4xx->2xx|status
Issue-Key: wickr|CreateBot|2xx->2xx|$.message
Issue-Key: wickr|CreateBot|4xx->4xx|$.reasons[*].reason
Issue-Commit: aa08ae4fa3a7591aa8a19f0853720b4a5376e00a

# wickr CreateBot: username naming rule unenforced and both result messages re-worded

> _Note: this issue was identified against the code base at commit `aa08ae4fa3a7591aa8a19f0853720b4a5376e00a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBot

## Description
Three net-new causes in the wickr `CreateBot` handler: it creates a bot whose username breaks the network's bot-naming convention instead of refusing it, and the two sentences it reports (the success message and the duplicate-username refusal reason) are emulator-authored rather than the ones the service states.

Cause 1 of 3: a `username` that does not end with `bot` is created instead of refused.
  AWS: Smithy: CreateBot.username (GenericString, pattern ^[\S\s]*$, required) is documented as "The username for the bot. This must be unique within the network and follow the network's naming conventions." The model expresses no part of that convention (the pattern matches every string), so the service owns it; CreateBot models ValidationError (httpError 422, members `reasons`: ErrorDetailList, `message`). Recorded behavior: a create whose username does not end with the literal `bot` is refused HTTP 422 ValidationError, carrying the `x-amzn-errortype` header, with `reasons` = one ErrorDetail whose `field` is the refused member name (`username`) and whose `reason` is the fixed sentence `Bot Username must end with bot`, and with `message` UNSET (no envelope sentence beside the per-field detail). The sentence has no variable parts — it names neither the submitted username nor the network.
  Emulator: answers HTTP 200 and creates the bot. The response body is the success envelope (`message` = `Bot created successfully`, `botId`, `username`, ...), no `reasons` list and no `x-amzn-errortype` header. The handler validates the group's password policy and username uniqueness only; there is no bot-username naming check at all.
  Why: a client that relies on AWS refusing an ill-named bot gets a bot account created with a username real Wickr would never mint. The caller's error path never runs, its 422 `reasons` parsing never executes, and the network is left holding an account whose name no subsequent live run could reproduce — so every later read, listing and bot count diverges from AWS.
  Fix: CreateBot's input validation, in the handler, before anything is stored (the same place the password policy and uniqueness checks already sit). The naming rule and its fixed reason sentence belong beside the other shared bot constants so any other operation that accepts a bot username cannot disagree about them.
  Traces: wickr-bot-lifecycle-and-validation#task#3 (representative), also wickr-bot-lifecycle-and-validation-errors#task#3. Handler: services/service-com-amazonaws-wickr-wickradminapi/src/ops/create_bot.rs lines 182-279 (checks run at 188, 196-221, 226-240; no username-shape check); shared bot constants: src/state/bots.rs lines 11-36.

Cause 2 of 3: the success body's `message` is an emulator sentence.
  AWS: Smithy: CreateBotResponse.message (GenericString) is "A message indicating the result of the bot creation operation." Recorded behavior: on a successful create the service reports the fixed text `success`. It is entirely fixed text — it names neither the bot, its id, nor the network, so there are no variable parts.
  Emulator: reports `message` = `Bot created successfully` (the crate's own `CREATED_MESSAGE` constant).
  Why: the create's result indicator is part of the response contract; a client that asserts on or logs/branches on this field sees a value AWS never sends, and any fixture or assertion written against AWS fails against the emulator.
  Fix: CreateBot's success response construction — the handler's result-message constant.
  Traces: wickr-bot-lifecycle-and-validation#task#4 (cluster representative), also wickr-bot-lifecycle-and-validation-errors#task#4 and wickr-create-bot-in-security-group#task#3. Constant: src/ops/create_bot.rs line 287 (`CREATED_MESSAGE`), used at line 272.

Cause 3 of 3: the duplicate-username refusal reason splices the request and state into the sentence.
  AWS: Smithy: ValidationError (httpError 422) with `reasons` (ErrorDetailList: `field` + `reason`); CreateBot.username must be "unique within the network". Recorded behavior: creating a second account with a username the network already holds is refused HTTP 422 ValidationError with `reasons[0].field` = `username` and `reasons[0].reason` = the fixed sentence `username is unavailable`. The whole sentence is fixed text: the service deliberately reports nothing about what the network already holds, so neither the submitted username nor the network id appears in it.
  Emulator: the status (422) and `reasons[0].field` are right, but `reasons[0].reason` is `username <submitted username> already exists in network <networkId>` — an emulator-authored sentence that splices in the request's `username` and the `networkId` label (recorded instance: `username sim2realprobe65593438bot already exists in network 10000000`; those two values are request/state values filling the emulator's template, not part of any requirement).
  Why: the refusal reason is a requirement of the error class. A client matching AWS's reason text does not recognise the refusal, and the emulator additionally discloses which username is taken — information AWS withholds here.
  Fix: CreateBot's uniqueness refusal, in the handler — state the fixed reason sentence (as a shared constant beside the other fixed refusal texts in the state module, which already holds `NETWORK_GONE_MESSAGE`/`SECURITY_GROUP_GONE_MESSAGE`/`BOT_GONE_MESSAGE`) instead of formatting the values in.
  Traces: wickr-bot-lifecycle-and-validation#task#9 (cluster representative), also wickr-bot-lifecycle-and-validation-errors#task#7 and wickr-create-bot-in-security-group#task#6. Refusal: src/ops/create_bot.rs lines 226-240; fixed-text constants: src/state.rs lines 187-193.

Not covered by any open issue: `20261121T040104Z-wickr-createbot-stub.md` ("Implement wickr CreateBot: unimplemented scaffold stub") and `20261121T071809Z-wickr-createbot-does-not-enforce-the-security-group-a92f22.md` both describe a CreateBot that answers 501; in this checkout CreateBot is implemented and answers 200/422, and all three causes above are divergences of that implemented handler.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200):

Cause 1 — fixture wickr-bot-lifecycle-and-validation, step task#3, CreateBot, verdict mismatch:
  status: expected 422, got 200
  $.reasons: missing (expected [{'field': 'username', 'reason': 'Bot Username must end with bot'}], got None)
  $.message: unexpected_present (expected None, got 'Bot created successfully')
  $headers.x-amzn-errortype: missing (expected '<shaped-not-asserted>', got None)
  affected: wickr-bot-lifecycle-and-validation#task#3, wickr-bot-lifecycle-and-validation-errors#task#3

Cause 2 — fixture wickr-bot-lifecycle-and-validation, step task#4, CreateBot, verdict mismatch:
  $.message: value_mismatch (expected 'success', got 'Bot created successfully')
  affected: wickr-bot-lifecycle-and-validation#task#4, wickr-bot-lifecycle-and-validation-errors#task#4, wickr-create-bot-in-security-group#task#3

Cause 3 — fixture wickr-bot-lifecycle-and-validation, step task#9, CreateBot, verdict mismatch (status 422 on both sides):
  $.reasons[0].reason: value_mismatch (expected 'username is unavailable', got 'username sim2realprobe65593438bot already exists in network 10000000')
  affected: wickr-bot-lifecycle-and-validation#task#9, wickr-bot-lifecycle-and-validation-errors#task#7, wickr-create-bot-in-security-group#task#6

Smithy evidence (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): CreateBot POST /networks/{networkId}/bots, success 200, errors include ValidationError (client, httpError 422) and ResourceNotFoundError (404); ValidationError members `reasons` (ErrorDetailList) and `message`; CreateBotResponse.message documented as "A message indicating the result of the bot creation operation."; CreateBotRequest.username documented as "This must be unique within the network and follow the network's naming conventions."

## Steps to Reproduce
1. Boot the emulator from this checkout and create a wickr network plus a security group (so a valid groupId exists).
2. Cause 1: POST /networks/{networkId}/bots with a `username` that does NOT end with `bot` (and a password that satisfies the group's policy). Expect HTTP 422 ValidationError with x-amzn-errortype set and body {"reasons":[{"field":"username","reason":"Bot Username must end with bot"}]} and no `message`; observe HTTP 200 and a created bot with message 'Bot created successfully'.
3. Cause 2: POST the same URI with a username that DOES end with `bot`. Expect 200 with `message` = 'success'; observe `message` = 'Bot created successfully'.
4. Cause 3: POST again with that same username. Expect 422 with reasons[0] = {field: 'username', reason: 'username is unavailable'}; observe reasons[0].reason = 'username <name> already exists in network <networkId>'.
5. Or replay the fixtures: wickr-bot-lifecycle-and-validation steps task#3, task#4, task#9.
