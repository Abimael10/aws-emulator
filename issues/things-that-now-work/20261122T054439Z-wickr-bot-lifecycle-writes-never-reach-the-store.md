Issue-Source: trace-sweep-divergence
Issue-Trace: wickr-bot-lifecycle-and-validation
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wickr-bot-lifecycle-and-validation wickr-bot-lifecycle-and-validation-errors
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr|GetBot|4xx->2xx|$.message
Issue-Key: wickr|GetBot|4xx->2xx|status
Issue-Key: wickr|GetBot|2xx->2xx|$.displayName
Issue-Key: wickr|GetBot|4xx->2xx|$
Issue-Related: 20261122T054439Z-wickr-bot-projections-report-a-uname-for-a.md
Issue-Commit: aa08ae4fa3a7591aa8a19f0853720b4a5376e00a

# wickr bot lifecycle writes never reach the store: deleted and renamed bots read back unchanged

> _Note: this issue was identified against the code base at commit `aa08ae4fa3a7591aa8a19f0853720b4a5376e00a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteBot

## Description
Three net-new causes sharing one fault: the two wickr operations that MUTATE an existing bot — `DeleteBot` and `UpdateBot` — are unmodified scaffold handlers that answer 501 and write nothing, so the bot store never reflects a delete or a rename and the subsequent read-backs are the divergences the corpus records. The divergences below are on the READS, which is where a client observes the lost write; each is an independent replayed cluster.

Cause 1 of 3: a deleted bot is still readable (GetBot after DeleteBot).
  AWS: Smithy: DeleteBot DELETE /networks/{networkId}/bots/{botId}, idempotent, success 200 with `message`; documented as "Deletes a bot from a specified Wickr network. This operation permanently removes the bot account...". GetBot models ResourceNotFoundError (errorKind client, httpError 404), documented for "a network, user, bot, security group, or other resource that doesn't exist or has been deleted". Recorded behavior: after the delete, GetBot on that botId answers HTTP 404 with body `{"message": "Bot not found"}` — the message is entirely fixed text and names no id (the crate already holds exactly this sentence as `BOT_GONE_MESSAGE`).
  Emulator: GetBot answers HTTP 200 and serves the bot record unchanged (no `message` member at all), because `DeleteBot` is a scaffold stub: its handler body is `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteBot"))`, so it answers 501 and removes nothing from the store.
  Why: a client cannot tell a deleted bot from a live one. Teardown and recreate flows break (the username stays taken, so the next create is refused 422 'username is unavailable'), bot counts and listings keep the removed account forever, and any retry-until-404 loop never terminates.
  Fix: implement the DeleteBot handler so it removes the bot from the network's bot map in the service state (and answers the modelled 200 `message`), after which GetBot's existing 404 path reports the already-correct fixed sentence. GetBot itself is correct — it only needs the state change.
  Traces: wickr-bot-lifecycle-and-validation#task#11 (representative). Stub: services/service-com-amazonaws-wickr-wickradminapi/src/ops/delete_bot.rs lines 167-181 (`not_implemented("DeleteBot")` at 180); store: src/state/bots.rs lines 44-93 (there is an `insert_bot` but no removal helper); GetBot's correct 404 path: src/ops/get_bot.rs lines 188-190 with `BOT_GONE_MESSAGE` = "Bot not found" at src/state.rs line 193.

Cause 2 of 3: a renamed bot reads back with its old displayName (GetBot after UpdateBot).
  AWS: Smithy: UpdateBot PATCH /networks/{networkId}/bots/{botId}, idempotent, success 200 with `message`; input members displayName, groupId, challenge, suspend; documented as "Updates the properties of an existing bot ... allows you to modify th[em]". GetBotResponse.displayName is "The display name of the bot that is visible to users." Recorded behavior: after an UpdateBot that sets a new `displayName`, GetBot reports THAT value.
  Emulator: GetBot still reports the display name the bot was created with (recorded instance: expected 'sim2real probe bot renamed', got 'sim2real probe bot' — those strings are the fixture's values, the requirement is that the read reflects the last accepted update), because `UpdateBot` is a scaffold stub returning `not_implemented("UpdateBot")` and writing nothing. (This cluster also flags `$.uname`; that part is the separate bot-projection issue filed alongside this one, not this cause.)
  Why: every property change a client makes to a bot is silently lost — display name, security-group reassignment, password rotation and suspend/unsuspend. A caller that updates then reads sees stale state, and a suspend that does not take effect is a security-relevant divergence.
  Fix: implement the UpdateBot handler so it applies the present input members to the stored bot record (displayName, groupId, challenge -> hasChallenge, suspend -> suspended), refusing an unknown network/bot with the modelled ResourceNotFoundError, and answers the modelled 200 `message`.
  Traces: wickr-bot-lifecycle-and-validation#task#8 (cluster representative). Stub: src/ops/update_bot.rs lines 170-187 (`not_implemented("UpdateBot")` at 186); record to mutate: `BotRecord` in src/state/bots.rs lines 26-36.

Cause 3 of 3: the delete read-back never converges even under polling (GetBot poll after DeleteBot, errors fixture).
  AWS: as cause 1 — GetBot on a deleted bot is 404 `{"message": "Bot not found"}`; the recording polls for that terminal state.
  Emulator: the poll exhausts 20 attempts, every one answering HTTP 200 with the surviving bot record; the step's own report reads "poll: status 200 not in [404] after 20 attempt(s)". This is the independent cluster that proves the lost delete is permanent rather than merely eventual.
  Why: a client written against AWS's documented "permanently removes the bot account" waits for a 404 that never comes, so the whole teardown phase hangs and then fails.
  Fix: same as cause 1 (the DeleteBot handler's state removal); this cluster must be re-replayed separately because it exercises the second fixture's teardown path.
  Traces: wickr-bot-lifecycle-and-validation-errors#task#11 (cluster representative).

Relation to existing issues: no open issue carries these replay keys. `20261121T040104Z-wickr-createbot-stub.md` is a CreateBot stub issue (and is stale — CreateBot is implemented in this checkout); if stub issues exist for DeleteBot/UpdateBot they cover only the 501 of those calls, not these recorded read-back divergences, which are the observable client-facing effect and need the store change to close.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; 8 steps answered NotImplemented):

Cause 1 — fixture wickr-bot-lifecycle-and-validation, step task#11, GetBot, verdict mismatch:
  status: expected 404, got 200
  $.message: missing (expected 'Bot not found', got None)

Cause 2 — fixture wickr-bot-lifecycle-and-validation, step task#8, GetBot, verdict mismatch (200 both sides):
  $.displayName: value_mismatch (expected 'sim2real probe bot renamed', got 'sim2real probe bot')
  $.uname: type_mismatch (expected None, got '687aad...e1a')   <- covered by the separate bot-projection issue

Cause 3 — fixture wickr-bot-lifecycle-and-validation-errors, step task#11, GetBot, verdict mismatch:
  status: expected 404, got 200
  $.message: missing (expected 'Bot not found', got None)
  $: poll: status 200 not in [404] after 20 attempt(s) (expected {'message': 'Bot not found'}, got {'botId': '2', 'displayName': 'Probe Bot', 'username': 'sim2real-probe-bot', 'uname': 'fc10c6...df93', 'status': 1, 'groupId': '0', 'hasChallenge': True, 'suspended': False})

Code: src/ops/delete_bot.rs line 180 `Err(crate::scaffolded::Scaffolded::not_implemented("DeleteBot"))`; src/ops/update_bot.rs line 186 `Err(crate::scaffolded::Scaffolded::not_implemented("UpdateBot"))`. src/state/bots.rs exposes `insert_bot`/`bot_of`/`bots_of`/`bot_by_username` but no removal or mutation helper. GetBot's 404 path already exists (src/ops/get_bot.rs lines 188-190) and already uses the correct fixed sentence `BOT_GONE_MESSAGE` = "Bot not found" (src/state.rs line 193), so the divergence is the missing state change, not the error shape.

Smithy: DeleteBot DELETE /networks/{networkId}/bots/{botId} (idempotent, 200, output `message`), UpdateBot PATCH same URI (idempotent, 200, output `message`, input displayName/groupId/challenge/suspend); both and GetBot model ResourceNotFoundError at httpError 404.

## Steps to Reproduce
1. Boot the emulator from this checkout; create a wickr network, a security group, and a bot.
2. Cause 2: PATCH /networks/{networkId}/bots/{botId} with a new `displayName`. Expect 200 with a `message`; observe HTTP 501 NotImplemented. Then GET the bot: expect the new displayName; observe the original one.
3. Cause 1: DELETE /networks/{networkId}/bots/{botId}. Expect 200 with a `message`; observe HTTP 501. Then GET the bot: expect HTTP 404 with body {"message": "Bot not found"}; observe HTTP 200 and the full bot record.
4. Cause 3: poll that GET until 404 — expect it to converge; observe 200 on all 20 attempts.
5. Or replay fixtures: wickr-bot-lifecycle-and-validation steps task#8 and task#11, and wickr-bot-lifecycle-and-validation-errors step task#11.
