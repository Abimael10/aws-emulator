Issue-Source: trace-sweep-divergence
Issue-Trace: lex-models-v2-bot-version-alias-lifecycle
Issue-Step: task#16
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: lex-models-v2-bot-version-alias-lifecycle
Issue-Shape-Id: com.amazonaws.lexmodelsv2#LexModelBuildingServiceV2
Issue-Key: lexv2-models|DescribeBotAlias|2xx->2xx|$.botAliasHistoryEvents
Issue-Commit: 24df4b5bb34709ed7c873e3f6d0f18f4590f38dd

# DescribeBotAlias omits botAliasHistoryEvents, so an alias never reports its version history

> _Note: this issue was identified against the code base at commit `24df4b5bb34709ed7c873e3f6d0f18f4590f38dd`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBotAlias

## Description
One cause.

Cause 1 of 1: DescribeBotAlias returns a 200 response body with no `botAliasHistoryEvents` member for a bot alias that has been pointed at a bot version.

AWS: Smithy model services/service-com-amazonaws-lexmodelsv2-lexmodelbuildingservicev2/smithy-model/com-amazonaws-lexmodelsv2-lexmodelbuildingservicev2.json — DescribeBotAlias (GET /bots/{botId}/botaliases/{botAliasId}, success 200) output shape DescribeBotAliasResponse carries member `botAliasHistoryEvents` targeting BotAliasHistoryEventsList, a list of BotAliasHistoryEvent {botVersion: BotVersion (@length 1..5, @pattern ^(DRAFT|[0-9]+)$), startDate: Timestamp, endDate: Timestamp}. The shape is documented as "Provides a record of an event that affects a bot alias. For example, when the version of a bot that the alias points to changes"; `startDate` is "The date and time that the event started" and `endDate` "The date and time that the event ended". So AWS records one history entry per version-pointing episode of the alias: `botVersion` is the version the alias pointed to for that episode, `startDate` is when the alias began pointing at it (the alias create/update time that set it), and `endDate` is when that episode ended — absent/null for the episode still in force. For an alias created (or updated) to point at exactly one version and not since re-pointed, the list has one element: that version, the time it was set, and no endDate. No modeled error is involved; the operation succeeds with 200 either way.

Emulator: the 200 body omits the member entirely. Replay detail: `$.botAliasHistoryEvents: missing (expected [{'botVersion': '1', 'endDate': None, 'startDate': <alias-update time>}], got None)` — i.e. where AWS returns a one-element history list naming the version the alias points to with its start time and a null endDate, the emulator returns nothing at that key. All other DescribeBotAlias members in the same response matched, so this is specifically the history list, not a broken response.

Why: a client that reads DescribeBotAlias to learn which bot versions an alias has served, and when the current pointer took effect, gets an empty/absent list and concludes the alias has no history. Deployment and rollback tooling built on alias history ("what version was live before this one", "when did this alias cut over") sees no events at all and cannot distinguish a brand-new alias from one that has been re-pointed repeatedly; code that iterates the list without a null guard also breaks against a member the model says is present for a used alias.

Fix: the lexv2-models bot-alias state plus the DescribeBotAlias handler's response assembly — alias state must retain a history entry each time the alias's `botVersion` is set (on CreateBotAlias and on each UpdateBotAlias that changes the version), closing the previous entry's `endDate` with the change time and leaving the newest entry's `endDate` unset, and the handler must serialize that list into `botAliasHistoryEvents`. Values must come from the recorded alias state (the version string the request set and the alias create/update timestamps), never from fixed literals.

Traces: fixture lex-models-v2-bot-version-alias-lifecycle, step task#16 (DescribeBotAlias after a bot version is created and the alias is pointed at version "1").

No currently open lexv2-models issue covers this: the four open lexv2-models-named issues are all CloudFormation-provisioning causes (AWS::Lex::Bot stub provisioning, DescribeStacks Outputs, AWS::Lex::BotAlias create rollback, Bot property validation) and none touches the DescribeBotAlias response shape.

## Evidence
Fixture lex-models-v2-bot-version-alias-lifecycle, step task#16, operation DescribeBotAlias, verdict mismatch.
Expected status 200; actual status 200 — the status matches, the body does not.
Divergence key: service=lexv2-models|DescribeBotAlias|status=200->200|keys=$.botAliasHistoryEvents
Detail: $.botAliasHistoryEvents: missing (expected [{'botVersion': '1', 'endDate': None, 'startDate': 1793167231.377}], got None)
Expected (AWS, per model): `botAliasHistoryEvents` present as a list with one BotAliasHistoryEvent whose `botVersion` is the version the alias points to, `startDate` is the time the alias was pointed at it, and `endDate` is unset because that episode is still in force. The quoted version string "1", the epoch 1793167231.377 and any ids in the recording are per-run values that fill the template; the requirement is the member's presence and the shape/derivation of its entries.
Actual (emulator): key `$.botAliasHistoryEvents` absent from the 200 response body.
Affected cluster members (1): lex-models-v2-bot-version-alias-lifecycle#task#16.

## Steps to Reproduce
1. Replay fixture lex-models-v2-bot-version-alias-lifecycle against the emulator (replay_wire_corpus).
2. The fixture creates a lexv2-models bot, creates bot version "1", and creates/updates a bot alias pointing at that version.
3. At step task#16 it issues DescribeBotAlias (GET /bots/{botId}/botaliases/{botAliasId}) for that alias.
4. Inspect the 200 response body: `botAliasHistoryEvents` is missing. AWS returns it as a one-element list naming the alias's current botVersion with the time it was set and no endDate.
