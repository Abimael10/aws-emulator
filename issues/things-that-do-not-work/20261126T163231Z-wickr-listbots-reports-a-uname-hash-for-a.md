Issue-Source: trace-sweep-divergence
Issue-Trace: wickr-bot-lifecycle-and-validation-errors
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wickr-bot-lifecycle-and-validation-errors
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr|ListBots|2xx->2xx|$.bots[groupId="0"].uname
Issue-Commit: 1a1b4308e2988b500cff1b534f223c232c5ace70

# wickr ListBots reports a uname hash for a bot that is still pending

> _Note: this issue was identified against the code base at commit `1a1b4308e2988b500cff1b534f223c232c5ace70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListBots

## Description
One cause, reached through the ListBots listing path in the bot-lifecycle error fixture: the bot record ListBots projects carries a `uname` for a bot that has never signed in, where the service reports that member as null until the bot becomes active.

Cause 1 of 1: ListBots projects a fabricated `uname` onto a PENDING bot record.
  AWS: Smithy (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): ListBots GET /networks/{networkId}/bots, success 200, output ListBotsResponse.bots (Bots, @required) of shape Bot; Bot.uname (GenericString, pattern ^[\S\s]*$) documented only as "The unique username hash identifier for the bot"; Bot.pubkey "The public key of the bot used for encryption"; Bot.lastLogin "The timestamp of the bot's last login"; Bot.status targets BotStatus, an intEnum with PENDING=1 ("created but is not yet active, pending initial setup or activation") and ACTIVE=2. No member of Bot is marked required and the model states nothing about when `uname` is assigned, so: recorded behavior only — the listing of a network whose only bot was just created by CreateBot answers HTTP 200 and the listed record reports `uname` null, together with `pubkey` null and `lastLogin` null, while `status` is 1 (pending), `groupId` '0', `hasChallenge` true, `suspended` false. Hypothesis: the username hash, like the key pair and the last-login instant, comes into being only when the bot software signs in with its challenge; the admin API exposes no sign-in, so a bot created through it stays pending and reports none of the three.
  Emulator: answers 200, and the listed record carries `uname` set to a 64-hex-character SHA-256 derived from '<networkId>/<username>' at create time (the replayed run shows 'fc10c62b22c4a7d8eef459c1c5547ee39dadc36e13aa52c8ac2e0cdb8877df93' — a per-environment derived value, not part of the requirement). Every other member of the record matches the recording, including the correctly-absent `pubkey` and `lastLogin`: the projection already encodes "a bot that has not signed in has no such value", it just does not apply that to `uname`.
  Why: a client that uses `uname` as the "has this bot registered yet" signal, or that lists bots to pick out the ones still to be provisioned, sees every freshly created bot as already carrying a username hash. Code branching on `uname == null`, or feeding the listed value into a uname-keyed lookup, takes the wrong branch against the emulator and the right one against AWS; a listing compared against a GetBot read agrees for the wrong reason.
  Fix: the shared bot projection `crate::state::bot_view` that ListBots maps every paged record through (services/service-com-amazonaws-wickr-wickradminapi/src/ops/list_bots.rs line 270, `bots: page.iter().map(crate::state::bot_view).collect()`), in the service's state module — project `uname` only for a bot whose status is ACTIVE, the same way the projection already withholds `pubkey` and `lastLogin`. The derived hash may remain on the stored record; it must not appear in a pending bot's projection. No status, error code or error message is at stake here: both sides answer 200 and the only diverging member is `uname`.
  Traces: wickr-bot-lifecycle-and-validation-errors#task#6 (cluster representative, affected_count 1).

Related open issue, which does NOT cover this cluster: `20261122T054439Z-wickr-bot-projections-report-a-uname-for-a.md` describes the same underlying projection fault and owns the GetBot cluster (wickr-bot-lifecycle-and-validation#task#5) and the security-group ListBots cluster (wickr-create-bot-in-security-group#task#5). The replay reports this cluster with an EMPTY `owned_by`: its divergence key is `wickr|ListBots|status=200->200|keys=$.bots[groupId="0"].uname`, which that issue's recorded replay keys do not join, so nothing re-replays this step and it would be closed unrun. It is filed here so the group-filtered listing path has its own replay key. A fixer that changes the shared projection should re-replay both.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up=true, /__admin/health 200): 46 steps, 37 passed, 9 mismatched, score 80.4%, 4 divergence clusters — all wickr.

This cluster (verdict=mismatch, owned_by empty, stub_path empty):
  fixture_id: wickr-bot-lifecycle-and-validation-errors
  step_key:   task#6
  operation:  ListBots   service: wickr
  expected_status 200, actual_status 200
  divergence_key: service=wickr|ListBots|status=200->200|keys=$.bots[groupId="0"].uname
  detail: $.bots[groupId="0"].uname: type_mismatch (expected None, got 'fc10c62b22c4a7d8eef459c1c5547ee39dadc36e13aa52c8ac2e0cdb8877df93')
  affected (1): wickr-bot-lifecycle-and-validation-errors#task#6

The recorded response's listed bot record reports `uname` null alongside `pubkey` null and `lastLogin` null with `status` 1; the emulator's listed record is identical except that `uname` holds the derived 64-hex-char hash. The hash value itself is environment-derived (SHA-256 of '<networkId>/<username>') and is not a requirement — the requirement is that the member is absent/null while the bot is pending.

Model evidence (smithy_query on services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json):
  ListBots -> http GET /networks/{networkId}/bots, code 200; output {bots: Bots (required), nextToken}
  Bot.members = botId, displayName, username, uname, pubkey, status, groupId, hasChallenge, suspended, lastLogin — none required
  BotStatus = intEnum {PENDING: 1, ACTIVE: 2}

Code evidence: services/service-com-amazonaws-wickr-wickradminapi/src/ops/list_bots.rs line 270 builds the listing with `crate::state::bot_view`, the single projection shared with the other bot reads; the state module's bot store (src/state/bots.rs) keeps the derived `uname` on the record.

## Steps to Reproduce
1. Boot the emulator from this checkout (its data listener; /__admin/health answers 200).
2. Create a wickr network and a security group in it.
3. POST /networks/{networkId}/bots to create a bot (username ending in `bot`, password satisfying the group policy). The bot comes back pending (status 1).
4. GET /networks/{networkId}/bots (the default-group listing, groupId '0'). Expect 200 with the listed record's `uname` null — as its `pubkey` and `lastLogin` are null — because the bot has never signed in. Observe `uname` set to a 64-hex-character SHA-256 derived from '<networkId>/<username>'.
5. Or replay the fixture step directly: wickr-bot-lifecycle-and-validation-errors, step task#6 (ListBots).
