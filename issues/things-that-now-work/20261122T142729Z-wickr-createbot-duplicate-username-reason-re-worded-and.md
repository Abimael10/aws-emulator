Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wickr.create-bot-duplicate-username-rejected wickr.delete-network-cascades-to-bots-and-security-groups
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr.create-bot-duplicate-username-rejected|run#16|AssertionError|assert reasons == [{"field": "username", "reason": "username is unavailable"}], (
Issue-Key: wickr.delete-network-cascades-to-bots-and-security-groups|run#20|AssertionError|assert (bot_code, bot_msg) == ("404", "Bot not found"), (
Issue-Commit: 4d8b014dbcb3833e6a7aad4896bf06c6aff71e41

# wickr: CreateBot duplicate-username reason re-worded, and a deleted network's GetBot is refused as the network rather than the bot

> _Note: this issue was identified against the code base at commit `4d8b014dbcb3833e6a7aad4896bf06c6aff71e41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBot

## Description
Two net-new root causes in the wickr admin API, both in how an operation IDENTIFIES a refusal. Cause 2 is a different operation (GetBot) from the issue's `operation` field; both are bundled here because they belong to the same service and the same fix area (the fixed refusal texts/codes shared by the bot operations).

Cause 1 of 2: CreateBot's duplicate-username refusal splices the request and state into the reason sentence.
  AWS: Smithy (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): CreateBot POST /networks/{networkId}/bots models ValidationError (client, httpError 422) whose members are `reasons` (ErrorDetailList of `field` + `reason`) and `message`; CreateBotRequest.username is documented "This must be unique within the network and follow the network's naming conventions." The model does not state the reason text, so recorded behavior only: a second CreateBot reusing a username the network already holds is refused ValidationError with `reasons` = exactly one ErrorDetail, `field` = the refused member name `username` and `reason` = the fixed sentence `username is unavailable`. That sentence is ENTIRELY fixed text — it has no variable parts: the service deliberately names neither the submitted username nor the network id, so it discloses nothing about what the network already holds.
  Emulator: the error code (ValidationError) and `reasons[0].field` (`username`) are right, but `reasons[0].reason` is the emulator-authored template `username <submitted username> already exists in network <networkId>` — the submitted `username` and the `networkId` are request/state values filled into that template, not part of any requirement (the replayed instance read `username sim2realbehavwickr6244756a2f82bot already exists in network 10000000`; that name and that id are a single run's values).
  Why: the refusal reason is a requirement of the error class. A client that matches or branches on AWS's reason text does not recognise the refusal and treats a taken-username collision as an unknown validation failure; the emulator additionally discloses which username is taken and in which network, information AWS withholds at this boundary.
  Fix: CreateBot's uniqueness refusal, in the operation handler — state the fixed reason sentence (as a shared constant beside the other fixed refusal texts already held in the state module) instead of formatting the request's username and the networkId into it.
  Note: open issue 20261122T054439Z-wickr-createbot-username-naming-rule-unenforced-and-both.md describes this same reason-text divergence as its "Cause 3 of 3" from the wire corpus; it is still open and the behavioral check below was not marked as owned by it, so it is reported here with its behavioral replay key. If that issue's Cause 3 is fixed, this cause is confirmed by re-replaying the check id listed under Traces.
  Traces: check wickr.create-bot-duplicate-username-rejected. Refusal site: src/ops/create_bot.rs lines 226-240; fixed-text refusal constants: src/state.rs lines 175-193.

Cause 2 of 2: after DeleteNetwork, GetBot on that network's bot is refused as a missing NETWORK, with a labelled ResourceNotFoundError, instead of as a missing BOT with the unlabelled 404 AWS sends.
  AWS: Smithy: GetBot GET /networks/{networkId}/bots/{botId} models ResourceNotFoundError (client, httpError 404, member `message` documented "A message identifying which resource was not found"), plus BadRequestError 400, UnauthorizedError 401, ForbiddenError 403, RateLimitError 429, ValidationError 422, InternalServerError 500. The model does not state which resource a bot read inside a deleted network reports, so recorded behavior only: DeleteNetwork is asynchronous; once the deletion has taken effect, GetNetwork for the deleted network is refused error code `ForbiddenError` with message `Network not found` (the emulator matches this), and a GetBot for a bot THAT network held is refused with HTTP status 404 and message `Bot not found` — an error body the SDK cannot resolve to a modelled shape (no `x-amzn-errortype`/`__type` discriminator), so botocore reports Error.Code as the bare status string `404`. The message `Bot not found` is entirely fixed text: it names neither the botId nor the networkId. In other words the cascade is reported at the resource the request actually asked for (the bot), not at its parent network, and it is reported unlabelled.
  Emulator: refuses the same call with Error.Code `ResourceNotFoundError` and message `Network not found`. The handler pre-checks the network and returns the network-level refusal before it ever looks for the bot, and it emits the refusal as the modelled, discriminator-labelled ResourceNotFoundError, so both the code a client branches on and the sentence it reads are wrong.
  Why: a client that cleans up or reconciles after DeleteNetwork branches on the per-resource refusal. Matching Error.Code `404` or the sentence `Bot not found`, it sees neither; it concludes the bot read failed for an unrelated reason (or that the network, not the bot, is the missing resource) and its cascade-verification path never runs. Because this is the first refusal the cascade check reaches, the rest of the recorded cascade behaviour (the deleted network's security group being refused HTTP 401 with `invalid security group request`) was never exercised and is NOT claimed here.
  Fix: GetBot's refusal path — the handler's resource-resolution order (report the member the request named, the bot, rather than short-circuiting on the parent network) and the error channel it uses for a gone bot (the unlabelled 404 form AWS sends, not the discriminated ResourceNotFoundError). The fixed sentence belongs with the other shared gone-texts so no other bot operation can disagree about it.
  Traces: check wickr.delete-network-cascades-to-bots-and-security-groups. Handler: src/ops/get_bot.rs lines 181-190 (network pre-check at 185-187 returns NETWORK_GONE_MESSAGE; the bot lookup and BOT_GONE_MESSAGE at 188-190 are unreachable for a deleted network) and the `gone` helper at lines 210-218; constants: src/state.rs lines 185-193.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout (wickr 4/8 passed; 2 of the 4 failures are the ALREADY FILED GetBotsCount scaffold stub and are not part of this issue).

Cause 1 — [mismatch] wickr.create-bot-duplicate-username-rejected:
  assertion: reasons == [{"field": "username", "reason": "username is unavailable"}]
  AssertionError: unexpected validation reasons [{'field': 'username', 'reason': 'username sim2realbehavwickr6244756a2f82bot already exists in network 10000000'}]
  Expected reasons[0].reason: 'username is unavailable' (fixed text, no variable parts)
  Actual   reasons[0].reason: 'username <username> already exists in network <networkId>' (the recorded run filled in 'sim2realbehavwickr6244756a2f82bot' and '10000000')
  Error.Code ValidationError and reasons[0].field 'username' matched; the check's earlier assertion on the code passed.

Cause 2 — [mismatch] wickr.delete-network-cascades-to-bots-and-security-groups:
  preceding assertion that PASSED: GetNetwork on the deleted network ends up ('ForbiddenError', 'Network not found').
  failing assertion: (bot_code, bot_msg) == ("404", "Bot not found") for GetBot(networkId=<deleted network>, botId=<bot that network held>)
  AssertionError: expected the deleted network's bot to be unreachable (404/'Bot not found'), got 'ResourceNotFoundError': 'Network not found'
  Expected: HTTP 404 with an error body carrying no shape discriminator (botocore Error.Code = '404'), Error.Message = 'Bot not found'
  Actual:   Error.Code = 'ResourceNotFoundError', Error.Message = 'Network not found'
  Not reached, therefore not claimed: the same check's next assertion, GetSecurityGroup on the deleted network == ('401', 'invalid security group request'), and the sibling-network positive controls.

Smithy evidence (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): CreateBot errors include ValidationError (httpError 422, members `reasons`: ErrorDetailList, `message`); GetBot GET /networks/{networkId}/bots/{botId} success 200, errors include ResourceNotFoundError (httpError 404, member `message`: "A message identifying which resource was not found"), UnauthorizedError (401), ForbiddenError (403); DeleteNetwork DELETE /networks/{networkId}, success 200, idempotent.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a botocore client at it: wickr = client('wickr', region='us-east-1').
2. Cause 1: create_network(networkName=<name>, accessLevel='STANDARD'); create_security_group(networkId=<id>, name=<name>, securityGroupSettings={}); create_bot(networkId, username=<name ending in 'bot'>, groupId, challenge=<password satisfying the group policy>). Call create_bot again with the SAME username. Expect ValidationError with reasons == [{'field': 'username', 'reason': 'username is unavailable'}]; observe reasons[0].reason = 'username <username> already exists in network <networkId>'.
3. Cause 2: create a network, take securityGroups[0].id from list_security_groups, create_bot in it, then delete_network(networkId). Poll get_network(networkId) until it is refused — it ends up ForbiddenError/'Network not found' (correct). Then call get_bot(networkId=<deleted network>, botId=<that bot>). Expect Error.Code '404' (an unlabelled HTTP 404) with Message 'Bot not found'; observe Error.Code 'ResourceNotFoundError' with Message 'Network not found'.
4. Or re-replay the behavioral checks: wickr.create-bot-duplicate-username-rejected and wickr.delete-network-cascades-to-bots-and-security-groups.
