Issue-Source: trace-sweep-divergence
Issue-Trace: glue-data-catalog-export-configuration-validation-errors
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: glue-data-catalog-export-configuration-validation-errors glue-start-data-quality-ruleset-evaluation-run-validation glue-table-optimizer-column-statistics-source-control-not-found-errors glue-table-optimizer-runs-listing-errors glue-data-quality-result-lookup-errors glue-get-data-quality-rule-recommendation-run-not-found glue-column-statistics-task-settings-not-found-and-range glue-update-job-from-source-control-errors
Issue-Shape-Id: com.amazonaws.glue#AWSGlue
Issue-Key: glue|PutDataCatalogExportConfiguration|4xx->4xx|$.Message
Issue-Key: glue|PutDataCatalogExportConfiguration|4xx->4xx|$.__type
Issue-Key: glue|StartDataQualityRulesetEvaluationRun|4xx->4xx|$.Message
Issue-Key: glue|StartDataQualityRulesetEvaluationRun|4xx->4xx|$.__type
Issue-Key: glue|UpdateTableOptimizer|4xx->4xx|$.Message
Issue-Key: glue|UpdateTableOptimizer|4xx->4xx|$.__type
Issue-Key: glue|ListTableOptimizerRuns|4xx->4xx|$.Message
Issue-Key: glue|ListTableOptimizerRuns|4xx->4xx|$.__type
Issue-Key: glue|GetDataQualityResult|4xx->4xx|$.__type
Issue-Key: glue|GetDataQualityRuleRecommendationRun|4xx->4xx|$.__type
Issue-Key: glue|UpdateColumnStatisticsTaskSettings|4xx->4xx|$.message
Issue-Key: glue|StartDataQualityRulesetEvaluationRun|4xx->4xx|$.message
Issue-Key: glue|ListTableOptimizerRuns|4xx->4xx|$.message
Issue-Key: glue|UpdateJobFromSourceControl|4xx->4xx|$.message
Issue-Related: 20261117T133709Z-glue-entity-not-found-refusals-carry-emulator-authored.md
Issue-Commit: b8e5c8616008a33e0a34d66e8eacd10130426f37

# Glue input-validation refusals use the wrong exception type and malformed constraint messages

> _Note: this issue was identified against the code base at commit `b8e5c8616008a33e0a34d66e8eacd10130426f37`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutDataCatalogExportConfiguration

## Description
Ten Glue request-validation refusals diverge from AWS. Four emit the wrong error code (__type ValidationException with no Message, where AWS raises InvalidInputException carrying a message), two skip a modeled member constraint entirely and fall through to a not-found refusal, and four render the generic 'N validation error detected' sentence with the wrong value or the wrong value set. The HTTP status, the error code and the message are all requirements.

Cause 1 of 10: PutDataCatalogExportConfiguration with ExportSetting absent.
AWS: Smithy: PutDataCatalogExportConfiguration input ExportSetting @required (enum); modeled errors include InvalidInputException (httpError 400) — ValidationException is NOT in this operation's error list. Recorded response: 400 __type 'InvalidInputException', Message '{"Message":"ExportSetting is required."}' (AWS wraps this one in a JSON envelope; fixed text, no request-derived part).
Emulator: 400 __type 'ValidationException' and NO Message member at all.
Why: an SDK deserializes an unmodeled error code into a generic service error, so `except Glue.Client.exceptions.InvalidInputException` never matches and the client sees no reason string.
Fix: the PutDataCatalogExportConfiguration handler's input validation (raise the modeled InvalidInputException with the message).
Traces: glue-data-catalog-export-configuration-validation-errors#task#1 (and #task#2 in the same cluster).

Cause 2 of 10: StartDataQualityRulesetEvaluationRun with an empty RulesetNames list.
AWS: Smithy: RulesetNames @required with @length min 1 max 10; modeled errors include InvalidInputException (400), not ValidationException. Recorded: 400 InvalidInputException, Message 'At least one ruleset name must be provided.' (fixed text).
Emulator: 400 ValidationException with no Message.
Fix: the StartDataQualityRulesetEvaluationRun handler's validation.
Traces: glue-start-data-quality-ruleset-evaluation-run-validation#task#1.

Cause 3 of 10: UpdateTableOptimizer with CatalogId omitted.
AWS: Smithy: UpdateTableOptimizer CatalogId @required; InvalidInputException (400) is modeled. Recorded: 400 InvalidInputException, Message 'CatalogId is required for UpdateTableOptimizer' — fixed text ending in the operation name.
Emulator: 400 ValidationException with no Message.
Fix: the UpdateTableOptimizer handler's validation.
Traces: glue-table-optimizer-column-statistics-source-control-not-found-errors#task#2.

Cause 4 of 10: ListTableOptimizerRuns with DatabaseName omitted.
AWS: Smithy: ListTableOptimizerRuns CatalogId/DatabaseName/TableName/Type all @required; InvalidInputException (400) modeled. Recorded: 400 InvalidInputException, Message 'DatabaseName is required for ListTableOptimizerRuns' — same '<Member> is required for <Operation>' template as cause 3.
Emulator: 400 ValidationException with no Message.
Fix: the ListTableOptimizerRuns handler's validation.
Traces: glue-table-optimizer-runs-listing-errors#task#2.

Cause 5 of 10: GetDataQualityResult with a ResultId that violates the member pattern (embedded newlines).
AWS: Smithy: ResultId targets HashString with @pattern ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]* (newline not allowed). Recorded: 400 __type 'InvalidInputException', Message "1 validation error detected: Value '<value>' at 'resultId' failed to satisfy constraint: Member must satisfy regular expression pattern: <the member's pattern>" — fixed text of the constraint sentence, variable parts the submitted value, the lowerCamel member name and the pattern.
Emulator: validates nothing and goes straight to lookup: 400 EntityNotFoundException, 'Data quality result with id <value> does not exist'.
Why: constraint violations must be rejected before state is consulted; a client distinguishing 'malformed id' from 'no such result' gets the wrong branch.
Fix: the GetDataQualityResult input validation, ordered before the state lookup.
Traces: glue-data-quality-result-lookup-errors#task#1.

Cause 6 of 10: GetDataQualityRuleRecommendationRun with a RunId that is not a Glue run id.
AWS: recorded behavior only (no modeled pattern narrower than HashString): 400 __type 'InvalidInputException', Message "Invalid RunId: must start with 'dqrun-'" — fixed text; AWS checks the id grammar before any lookup.
Emulator: 400 EntityNotFoundException, 'Data quality rule recommendation run with id <value> does not exist'.
Fix: the GetDataQualityRuleRecommendationRun handler's validation, before the state lookup.
Traces: glue-get-data-quality-rule-recommendation-run-not-found#task#1.

Cause 7 of 10: UpdateColumnStatisticsTaskSettings with SampleSize above the modeled maximum — the value is rendered with the wrong numeric form.
AWS: Smithy: SampleSize targets SampleSizePercentage, type double, @range min 0 max 100. Recorded: 400, $.message "1 validation error detected: Value '150.0' at 'sampleSize' failed to satisfy constraint: Member must have value less than or equal to 100" — because the member is a double, the offending value is rendered in its double form ('150.0'), not as the integer literal the client sent.
Emulator: same sentence but with Value '150' — it echoes the request literal instead of the member's modeled type form.
Fix: the shared constraint-message serializer (render the rejected value using the target shape's type).
Traces: glue-column-statistics-task-settings-not-found-and-range#task#2.

Cause 8 of 10: StartDataQualityRulesetEvaluationRun with 11 ruleset names — the rejected list value is omitted from the message.
AWS: Smithy: RulesetNames @length max 10. Recorded: 400, $.message "1 validation error detected: Value '[r1, r2, ...]' at 'rulesetNames' failed to satisfy constraint: Member must have length less than or equal to 10" — the list value is rendered inside the message as '[' + the elements joined by ', ' + ']'.
Emulator: "1 validation error detected: Value at 'rulesetNames' failed to satisfy constraint: Member must have length less than or equal to 10" — the 'Value ...' clause is dropped for collection members.
Fix: the shared constraint-message serializer (render list/collection values, not only scalars).
Traces: glue-start-data-quality-ruleset-evaluation-run-validation#task#2.

Cause 9 of 10: ListTableOptimizerRuns with an unknown Type — the enum value set is listed in the wrong order.
AWS: Smithy: Type targets TableOptimizerType. Recorded message lists the admissible set as [compaction, orphan_file_deletion, retention]; the model's declaration order is compaction, retention, orphan_file_deletion, so AWS does NOT emit declaration order for this enum.
Emulator: "... Member must satisfy enum value set: [compaction, retention, orphan_file_deletion]" — model declaration order.
Why: the message text is part of the contract; a client or test asserting AWS's sentence fails on the reordering.
Fix: the enum constraint-message serializer (emit each enum's AWS-ordered value set, per enum, not the model's member order).
Traces: glue-table-optimizer-runs-listing-errors#task#1.

Cause 10 of 10: UpdateJobFromSourceControl with an unknown Provider — same ordering defect, different order.
AWS: Smithy: Provider targets SourceControlProvider, declaration order GITHUB, GITLAB, BITBUCKET, AWS_CODE_COMMIT. Recorded message lists [GITHUB, AWS_CODE_COMMIT, BITBUCKET, GITLAB] — neither declaration nor alphabetical order, so the order must be carried per enum (recorded behavior only).
Emulator: "... enum value set: [GITHUB, GITLAB, BITBUCKET, AWS_CODE_COMMIT]".
Fix: the same enum constraint-message serializer as cause 9.
Traces: glue-update-job-from-source-control-errors#task#2.

## Evidence
All replayed at status 400.
1. glue-data-catalog-export-configuration-validation-errors#task#1 PutDataCatalogExportConfiguration — $.__type expected 'InvalidInputException', got 'ValidationException'; $.Message expected '{"Message":"ExportSetting is required."}', got absent. (cluster also covers #task#2)
2. glue-start-data-quality-ruleset-evaluation-run-validation#task#1 — __type expected 'InvalidInputException', got 'ValidationException'; Message expected 'At least one ruleset name must be provided.', got absent.
3. glue-table-optimizer-column-statistics-source-control-not-found-errors#task#2 UpdateTableOptimizer — __type expected 'InvalidInputException', got 'ValidationException'; Message expected 'CatalogId is required for UpdateTableOptimizer', got absent.
4. glue-table-optimizer-runs-listing-errors#task#2 ListTableOptimizerRuns — __type expected 'InvalidInputException', got 'ValidationException'; Message expected 'DatabaseName is required for ListTableOptimizerRuns', got absent.
5. glue-data-quality-result-lookup-errors#task#1 GetDataQualityResult — __type expected 'InvalidInputException', got 'EntityNotFoundException'; Message expected "1 validation error detected: Value 'bad\nresult\nid' at 'resultId' failed to satisfy constraint: Member must satisfy regular expression pattern: [\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*", got 'Data quality result with id bad\nresult\nid does not exist'.
6. glue-get-data-quality-rule-recommendation-run-not-found#task#1 — __type expected 'InvalidInputException', got 'EntityNotFoundException'; Message expected "Invalid RunId: must start with 'dqrun-'", got 'Data quality rule recommendation run with id <id> does not exist'.
7. glue-column-statistics-task-settings-not-found-and-range#task#2 UpdateColumnStatisticsTaskSettings — $.message expected "... Value '150.0' at 'sampleSize' ...", got "... Value '150' at 'sampleSize' ...".
8. glue-start-data-quality-ruleset-evaluation-run-validation#task#2 — $.message expected "... Value '[r1, r2, r3, r4, r5, r6, r7, r8, r9, r10, r11]' at 'rulesetNames' ...", got "... Value at 'rulesetNames' ...".
9. glue-table-optimizer-runs-listing-errors#task#1 — $.message expected "... enum value set: [compaction, orphan_file_deletion, retention]", got "[compaction, retention, orphan_file_deletion]".
10. glue-update-job-from-source-control-errors#task#2 — $.message expected "... enum value set: [GITHUB, AWS_CODE_COMMIT, BITBUCKET, GITLAB]", got "[GITHUB, GITLAB, BITBUCKET, AWS_CODE_COMMIT]".

## Steps to Reproduce
1. Replay the wire corpus (replay_wire_corpus).
2. Causes 1-4: call PutDataCatalogExportConfiguration with no ExportSetting; StartDataQualityRulesetEvaluationRun with RulesetNames=[]; UpdateTableOptimizer with no CatalogId; ListTableOptimizerRuns with no DatabaseName. Each answers ValidationException with no Message instead of InvalidInputException + message.
3. Causes 5-6: call GetDataQualityResult with a ResultId containing newlines, and GetDataQualityRuleRecommendationRun with a RunId not prefixed 'dqrun-'. Each answers EntityNotFoundException instead of the pattern/grammar InvalidInputException.
4. Cause 7: UpdateColumnStatisticsTaskSettings with SampleSize=150.
5. Cause 8: StartDataQualityRulesetEvaluationRun with 11 ruleset names.
6. Causes 9-10: ListTableOptimizerRuns with Type='nonsense-optimizer-type'; UpdateJobFromSourceControl with Provider='NOT_A_REAL_PROVIDER'. Compare the enum value set ordering in $.message.
