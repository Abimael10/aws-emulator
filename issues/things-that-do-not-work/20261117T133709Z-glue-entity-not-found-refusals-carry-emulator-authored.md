Issue-Source: trace-sweep-divergence
Issue-Trace: glue-blueprint-run-and-session-endpoint-not-found
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-blueprint-run-and-session-endpoint-not-found glue-get-blueprint-runs-not-found glue-data-quality-result-lookup-errors glue-data-quality-ruleset-evaluation-run-not-found glue-get-data-quality-rule-recommendation-run-not-found glue-ml-transform-update-and-task-run-not-found glue-crawler-update-description-and-unknown-crawler glue-start-crawler-not-found glue-stop-column-statistics-task-run-not-running-and-empty-table-name
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|GetBlueprintRun|4xx->4xx|$.Message
Issue-Key: glue|GetBlueprintRuns|4xx->4xx|$.Message
Issue-Key: glue|GetDataQualityResult|4xx->4xx|$.Message
Issue-Key: glue|GetDataQualityRulesetEvaluationRun|4xx->4xx|$.Message
Issue-Key: glue|GetDataQualityRuleRecommendationRun|4xx->4xx|$.Message
Issue-Key: glue|GetMLTaskRun|4xx->4xx|$.Message
Issue-Key: glue|UpdateCrawler|4xx->4xx|$.Message
Issue-Key: glue|StartCrawler|4xx->4xx|$.Message
Issue-Key: glue|StopColumnStatisticsTaskRun|4xx->4xx|$.Message
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue entity-not-found refusals carry emulator-authored message text

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBlueprintRun

## Description
Nine Glue operations return the modeled error shape and HTTP status for an absent (or not-running) entity, but the Message member is emulator-authored prose rather than AWS's wording for that error class. The error message is a requirement: its fixed text must be reproduced exactly and only its variable parts (names, ids, account id) may come from the request or from emulator state. Every cause below is a Message-only divergence — status (400) and __type are already correct.

Cause 1 of 9: GetBlueprintRun, run id unknown for an existing blueprint.
AWS: Smithy: GetBlueprintRun input BlueprintName+RunId both @required; error EntityNotFoundException (httpError 400). Recorded response: 400 EntityNotFoundException with Message 'Blueprint run with id:<RunId> and blueprint name:<BlueprintName> not found'. Fixed text: 'Blueprint run with id:', ' and blueprint name:', ' not found'; variable parts: the RunId and BlueprintName from the request.
Emulator: 400 EntityNotFoundException, Message 'Blueprint with given name not found.' — it reports the blueprint as missing although the blueprint exists and only the run id is unknown.
Why: a client cannot tell an unknown run from an unknown blueprint, and log/alarm matchers keyed on AWS's sentence never fire.
Fix: the GetBlueprintRun handler's not-found path (it must distinguish blueprint-missing from run-missing and format the run-missing sentence).
Traces: glue-blueprint-run-and-session-endpoint-not-found#task#1.

Cause 2 of 9: GetBlueprintRuns for a blueprint that does not exist.
AWS: Smithy: GetBlueprintRuns error EntityNotFoundException (400). Recorded response Message is the fixed sentence 'Entity not found' — no request-derived part at all.
Emulator: Message 'Blueprint with given name not found.'
Why: the refusal text differs from AWS's for the same condition; clients that assert on it break.
Fix: the GetBlueprintRuns handler's not-found path.
Traces: glue-get-blueprint-runs-not-found#task#1.

Cause 3 of 9: GetDataQualityResult for an unknown ResultId.
AWS: Smithy: GetDataQualityResult input ResultId @required (HashString, length 1-255); error EntityNotFoundException (400). Recorded Message: 'Cannot find Data Quality Result in account <accountId> with id <ResultId>'. Fixed text: 'Cannot find Data Quality Result in account ', ' with id '; variable parts: the caller's account id and the ResultId from the request.
Emulator: Message 'Data quality result with id <ResultId> does not exist'.
Fix: the GetDataQualityResult handler's not-found path.
Traces: glue-data-quality-result-lookup-errors#task#2.

Cause 4 of 9: GetDataQualityRulesetEvaluationRun for an unknown RunId.
AWS: error EntityNotFoundException (400). Recorded Message: 'Cannot find Data Quality Run in account <accountId> with id <RunId>' — the same 'Cannot find Data Quality Run in account ... with id ...' template as cause 5.
Emulator: Message 'Data quality ruleset evaluation run with id <RunId> does not exist'.
Fix: the GetDataQualityRulesetEvaluationRun handler's not-found path.
Traces: glue-data-quality-ruleset-evaluation-run-not-found#task#1.

Cause 5 of 9: GetDataQualityRuleRecommendationRun for an unknown, well-formed RunId.
AWS: error EntityNotFoundException (400). Recorded Message: 'Cannot find Data Quality Run in account <accountId> with id <RunId>'. Note AWS uses the SAME 'Data Quality Run' noun for recommendation runs and evaluation runs.
Emulator: Message 'Data quality rule recommendation run with id <RunId> does not exist'.
Fix: the GetDataQualityRuleRecommendationRun handler's not-found path.
Traces: glue-get-data-quality-rule-recommendation-run-not-found#task#2.

Cause 6 of 9: GetMLTaskRun for an unknown TaskRunId under an existing transform.
AWS: Smithy: GetMLTaskRun input TransformId+TaskRunId @required; error EntityNotFoundException (400). Recorded Message: 'No ML Task Run found for <TaskRunId>: in account <accountId> for transform <TransformId>' — fixed text 'No ML Task Run found for ', ': in account ', ' for transform ' (note the colon directly after the task run id); variable parts from the request and the caller's account.
Emulator: Message 'Cannot find MLTaskRun in account <accountId> with handle <TaskRunId>' — a different template that also drops the transform id.
Fix: the GetMLTaskRun handler's not-found path.
Traces: glue-ml-transform-update-and-task-run-not-found#task#8.

Cause 7 of 9: UpdateCrawler naming a crawler that does not exist.
AWS: Smithy: UpdateCrawler input Name @required; error EntityNotFoundException (400). Recorded Message is the fixed sentence 'Crawler with provided name does not exist' — it contains NO crawler name.
Emulator: Message 'Crawler entry with name <Name> does not exist' — wrong wording and it interpolates the requested name where AWS emits none.
Fix: the UpdateCrawler handler's not-found path.
Traces: glue-crawler-update-description-and-unknown-crawler#task#7.

Cause 8 of 9: StartCrawler naming a crawler that does not exist.
AWS: Smithy: StartCrawler input Name @required; errors EntityNotFoundException, CrawlerRunningException (both 400). Recorded Message: 'Crawler with name <Name> does not exist' — fixed text 'Crawler with name ' + ' does not exist', variable part the requested Name. This is a DIFFERENT template from UpdateCrawler's (cause 7), so one shared string cannot serve both.
Emulator: Message 'Crawler entry with name <Name> does not exist' (the extra word 'entry').
Fix: the StartCrawler handler's not-found path.
Traces: glue-start-crawler-not-found#task#1.

Cause 9 of 9: StopColumnStatisticsTaskRun when no task run is running for the database/table.
AWS: Smithy: StopColumnStatisticsTaskRun errors include ColumnStatisticsTaskNotRunningException (400). Recorded Message is the fixed sentence 'No task found in Running state for input database and table name combination' — no request-derived part.
Emulator: Message 'Column Statistics task for input table is not running'.
Fix: the StopColumnStatisticsTaskRun handler's not-running path.
Traces: glue-stop-column-statistics-task-run-not-running-and-empty-table-name#task#3.

Related open issues cover other Glue message families (e.g. 20261108T101230Z-glue-table-version-not-found-refusals-carry-emulator.md, 20261030T000205Z-glue-delete-path-refusals-carry-emulator-authored-messages.md); none of them carries the replay keys for the nine operations above.

## Evidence
Replayed fixtures, expected (recorded AWS) vs actual (emulator), all status 400:
1. glue-blueprint-run-and-session-endpoint-not-found#task#1 GetBlueprintRun — $.Message expected 'Blueprint run with id:sim2real-probe-glue-bprun-89ba782d and blueprint name:sim2real-probe-glue-bp-016060c3 not found', got 'Blueprint with given name not found.'
2. glue-get-blueprint-runs-not-found#task#1 GetBlueprintRuns — expected 'Entity not found', got 'Blueprint with given name not found.'
3. glue-data-quality-result-lookup-errors#task#2 GetDataQualityResult — expected 'Cannot find Data Quality Result in account 000000000000 with id <resultId>', got 'Data quality result with id <resultId> does not exist'
4. glue-data-quality-ruleset-evaluation-run-not-found#task#1 GetDataQualityRulesetEvaluationRun — expected 'Cannot find Data Quality Run in account 000000000000 with id <runId>', got 'Data quality ruleset evaluation run with id <runId> does not exist'
5. glue-get-data-quality-rule-recommendation-run-not-found#task#2 GetDataQualityRuleRecommendationRun — expected 'Cannot find Data Quality Run in account 000000000000 with id <runId>', got 'Data quality rule recommendation run with id <runId> does not exist'
6. glue-ml-transform-update-and-task-run-not-found#task#8 GetMLTaskRun — expected 'No ML Task Run found for <taskRunId>: in account 000000000000 for transform <transformId>', got 'Cannot find MLTaskRun in account 000000000000 with handle <taskRunId>'
7. glue-crawler-update-description-and-unknown-crawler#task#7 UpdateCrawler — expected 'Crawler with provided name does not exist', got 'Crawler entry with name <name> does not exist'
8. glue-start-crawler-not-found#task#1 StartCrawler — expected 'Crawler with name <name> does not exist', got 'Crawler entry with name <name> does not exist'
9. glue-stop-column-statistics-task-run-not-running-and-empty-table-name#task#3 StopColumnStatisticsTaskRun — expected 'No task found in Running state for input database and table name combination', got 'Column Statistics task for input table is not running'
(The recorded ids/names above are one run's values; only the fixed text and the template are the requirement.)

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus).
2. For cause 1: create a blueprint, then call GetBlueprintRun with that blueprint name and an unknown RunId — compare $.Message with the recording.
3. For causes 2-6: call GetBlueprintRuns / GetDataQualityResult / GetDataQualityRulesetEvaluationRun / GetDataQualityRuleRecommendationRun / GetMLTaskRun with an identifier that does not exist.
4. For causes 7-8: call UpdateCrawler and StartCrawler with a crawler name that was never created.
5. For cause 9: call StopColumnStatisticsTaskRun for a database/table with no running column-statistics task.
Each step returns 400 with the modeled __type but an emulator-authored $.Message.
