Issue-Source: trace-sweep-divergence
Issue-Trace: wickr-bot-lifecycle-and-validation
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wickr-bot-lifecycle-and-validation wickr-bot-lifecycle-and-validation-errors wickr-create-bot-in-security-group
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr|GetBot|2xx->2xx|$.uname
Issue-Key: wickr|ListBots|2xx->2xx|$.bots[*]
Issue-Key: wickr|ListBots|2xx->2xx|$.bots[*].uname
Issue-Commit: aa08ae4fa3a7591aa8a19f0853720b4a5376e00a

# wickr bot projections report a uname for a bot that has never signed in

> _Note: this issue was identified against the code base at commit `aa08ae4fa3a7591aa8a19f0853720b4a5376e00a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBot

## Description
One shared fault reached through three operations/fixtures: the single bot projection every wickr read answers from fills in `uname` for a freshly created bot, while the service reports no `uname` until the bot has signed in. Each cause below is a distinct replayed cluster of that fault and must be closed on all three.

Cause 1 of 3: GetBot reports a `uname` for a PENDING bot.
  AWS: Smithy: GetBotResponse.uname (GenericString, pattern ^[\S\s]*$) — "The unique username hash identifier for the bot"; GetBotResponse.status (BotStatus, intEnum) — "The current status of the bot (1 for pending, 2 for active)". The model marks no member required and documents nothing about when `uname` is assigned. Recorded behavior only: reading back a bot immediately after CreateBot, the service answers HTTP 200 with `status` 1 (pending) and `uname` NULL — exactly as it reports `pubkey` and `lastLogin` as null for that bot. Hypothesis: the username hash, like the key pair and the last-login instant, exists only once the bot software has signed in with its challenge; the admin API has no operation that signs a bot in, so a bot created through it stays pending and carries none of the three.
  Emulator: answers 200 with `uname` set to a fabricated 64-hex-character value — the SHA-256 of `<networkId>/<username>` minted at create time and stored on the bot record (recorded instance: '687aad6cb530363f95c5e26e3462a911677437ae1cd3332c15f532cbd7691e1a'; that literal is a per-environment derived value, not part of the requirement). It correctly reports `pubkey` and `lastLogin` as absent, so the projection already encodes "a bot that has not signed in has no such value" — it just does not apply it to `uname`.
  Why: a client treating `uname` as the "has this bot registered yet" signal (or using it as the lookup key for a bot that has signed in) sees a registered-looking bot that has never signed in; code that branches on `uname == null`, or that feeds the value to a uname lookup, takes the wrong path against the emulator and the right one against AWS.
  Fix: the shared bot projection in the service's state module (the one function GetBot/ListBots both answer from) — report `uname` only for a bot that has signed in (status ACTIVE), the same way it already withholds `pubkey` and `lastLogin`. The derived value may stay on the stored record; it must not be projected for a pending bot.
  Traces: wickr-bot-lifecycle-and-validation#task#5 (representative), also wickr-bot-lifecycle-and-validation-errors#task#5 and wickr-create-bot-in-security-group#task#4. Projection: services/service-com-amazonaws-wickr-wickradminapi/src/state/bots.rs lines 100-116 (`uname` at 105, `pubkey`/`last_login` withheld at 106-110); record field set at src/ops/create_bot.rs line 251 via `derive_uname` (src/state.rs lines 842-847); GetBot answers from it at src/ops/get_bot.rs lines 194-206.

Cause 2 of 3: the same fabricated `uname` appears in the ListBots record.
  AWS: Smithy: ListBots GET /networks/{networkId}/bots -> ListBotsResponse.bots (Bots, required) of the `Bot` shape, whose members are the same as GetBotResponse's (botId, displayName, username, uname, pubkey, status, groupId, hasChallenge, suspended, lastLogin). Recorded behavior only: the listed record for a just-created bot carries `uname` null (alongside `pubkey` null and `lastLogin` null), with `status` 1, `groupId` '0', `hasChallenge` true, `suspended` false.
  Emulator: the listed record carries the derived hash in `uname`; everything else in the record matches.
  Why: a caller that lists bots to find the unregistered ones, or that compares a listing against a read, sees every bot as already having a username hash.
  Fix: same shared projection — ListBots builds its records from it, so one change closes both, but the listing must be re-replayed to confirm it.
  Traces: wickr-bot-lifecycle-and-validation-errors#task#6 (cluster representative). Listing: src/ops/list_bots.rs, answering from `bot_view` in src/state/bots.rs line 100.

Cause 3 of 3: ListBots in the security-group fixture, same projection.
  AWS: as cause 2 — the listed record for a bot created into a named security group reports `uname` null while the bot is pending.
  Emulator: `$.bots[0].uname` is the derived hash (recorded instance 'e3be797c2a57f3810ea88e7372e967ce48b5c6cd05d43fa1cecfc83ab5bfea55' — a per-environment value).
  Why: as cause 2; this is the independent listing path exercised after a create into a non-default group, so it is the second recorded cluster that only passes once the projection is fixed.
  Fix: same shared projection.
  Traces: wickr-create-bot-in-security-group#task#5 (cluster representative).

No open issue covers this: the only open wickr projection issue, `20261109T141502Z-wickr-getnetwork-and-listnetworks-network-records-drop-tags.md`, is about NETWORK records (tags/standing/migrationState) and owns those replay keys; it says nothing about bot records.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up), three mismatch clusters, all HTTP 200 on both sides:

Cause 1 — fixture wickr-bot-lifecycle-and-validation, step task#5, GetBot:
  $.uname: type_mismatch (expected None, got '687aad6cb530363f95c5e26e3462a911677437ae1cd3332c15f532cbd7691e1a')
  affected: wickr-bot-lifecycle-and-validation#task#5, wickr-bot-lifecycle-and-validation-errors#task#5, wickr-create-bot-in-security-group#task#4

Cause 2 — fixture wickr-bot-lifecycle-and-validation-errors, step task#6, ListBots:
  $.bots[0]: missing (expected {'botId': '<shaped-not-asserted>', 'displayName': 'Probe Bot', 'username': 'sim2real-probe-bot', 'uname': None, 'pubkey': None, 'status': 1, 'groupId': '0', 'hasChallenge': True, 'suspended': False, 'lastLogin': None}, got {'botId': '2', 'displayName': 'Probe Bot', 'username': 'sim2real-probe-bot', 'uname': 'fc10c62b22c4a7d8eef459c1c5547ee39dadc36e13aa52c8ac2e0cdb8877df93', 'status': 1, 'groupId': '0', 'hasChallenge': True, 'suspended': False})
  (the only value that differs is `uname`: expected null, got the derived hash)

Cause 3 — fixture wickr-create-bot-in-security-group, step task#5, ListBots:
  $.bots[0].uname: type_mismatch (expected None, got 'e3be797c2a57f3810ea88e7372e967ce48b5c6cd05d43fa1cecfc83ab5bfea55')

Code: src/state/bots.rs line 105 sets `uname: Some(record.uname.clone())` in `bot_view`, two lines above the comment at 106-110 that explains why `pubkey` and `last_login` are withheld for a bot that has not signed in; src/ops/create_bot.rs line 251 mints the value with `derive_uname(&network_id, &username)` (src/state.rs lines 842-847).

Smithy: GetBotResponse.uname / Bot.uname documented only as "The unique username hash identifier for the bot"; BotStatus is an intEnum with 1 = pending, 2 = active. Nothing in the model requires the member to be present.

## Steps to Reproduce
1. Boot the emulator from this checkout; create a wickr network and a security group.
2. POST /networks/{networkId}/bots to create a bot (username ending in `bot`, password meeting the group policy).
3. GET /networks/{networkId}/bots/{botId}. Expect 200 with status 1 and `uname` null (as `pubkey` and `lastLogin` are); observe `uname` set to a 64-hex-char SHA-256 of '<networkId>/<username>'.
4. GET /networks/{networkId}/bots. Expect `bots[0].uname` null; observe the same derived hash.
5. Repeat step 4 for a bot created into a non-default security group.
6. Or replay fixtures: wickr-bot-lifecycle-and-validation#task#5, wickr-bot-lifecycle-and-validation-errors#task#6, wickr-create-bot-in-security-group#task#5.
