Issue-Source: trace-sweep-divergence
Issue-Trace: wellarchitected-lens-review-answer-lifecycle
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wellarchitected-lens-review-answer-lifecycle wellarchitected-pagination-and-limit-validation wellarchitected-catalog-list-lenses
Issue-Shape-Id: com.amazonaws.wellarchitected#WellArchitectedApiServiceLambda
Issue-Key: wellarchitected|GetAnswer|2xx->2xx|$.Answer.Choices[*].Description
Issue-Key: wellarchitected|GetAnswer|2xx->2xx|$.Answer.HelpfulResourceUrl
Issue-Key: wellarchitected|UpdateAnswer|2xx->2xx|$.Answer.Choices[*].Description
Issue-Key: wellarchitected|UpdateAnswer|2xx->2xx|$.Answer.HelpfulResourceUrl
Issue-Key: wellarchitected|UpdateAnswer|2xx->2xx|$.Answer.ImprovementPlanUrl
Issue-Key: wellarchitected|GetAnswer|2xx->2xx|$.Answer.ImprovementPlanUrl
Issue-Key: wellarchitected|ListAnswers|2xx->2xx|$.AnswerSummaries[*].Choices[*].Description
Issue-Key: wellarchitected|ListAnswers|2xx->2xx|$.NextToken
Issue-Key: wellarchitected|ListLenses|2xx->2xx|$.LensSummaries[*]
Issue-Commit: 91eb64ca5a78a7d78f4748085e65681abc417e15

# AWS-official lens content diverges from AWS: framework answer choice prose, question document URLs and ListLenses catalog timestamps

> _Note: this issue was identified against the code base at commit `91eb64ca5a78a7d78f4748085e65681abc417e15`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Resolution and remaining residue (read this first)

The catalog now holds this data as per-row DATA instead of deriving it:

* Cause 1 / Cause 3 (choice prose): the framework lens's `cloud-financial-management` choice rows carry the lens's published prose. Replay confirms `$.Answer.Choices[0]`, `[1]`, `[2]`, `[3]` and `[5]` now match the recording exactly. RESIDUE: `Choices[4]`, `[6]`, `[7]`, `[8]` and the terminal `[9]` still differ in wording — the recording's exact published strings for those five rows are quoted neither in this report nor by the replay (which withholds recorded values), so they carry the published prose only as closely as this catalog knows it. They were deliberately not reverse-engineered one string at a time.
* Cause 2 (question documents): `Answer$HelpfulResourceUrl` / `$ImprovementPlanUrl` are no longer assembled from `LensVersion` + `LensAlias` + `QuestionId`; they address the document the lens publishes for the question, named by that question's published label (`OPS_1` … `SUS_6`), so the pair no longer moves when the version string moves. RESIDUE: the exact published URL string the recording carries is quoted nowhere available, so these two members still diverge on `task#5`/`task#6`/`task#7`.
* Cause 4 (`ListLenses` instants): each row states its own published creation and last-revision instants. Replay confirms `$.LensSummaries[0]` (the framework lens) and `$.LensSummaries[1]` (`connectedmobility`) now match the recorded elements whole-element, which also disproves this report's `LensAlias` hypothesis — the member is present in the recording. RESIDUE: `$.LensSummaries[2]`..`[15]` still differ: the recorded catalog holds 16 published lenses (it includes `containerbuild` and `mergersandacquisitions`, which this catalog carries no content for) and the published instants of those rows are not disclosed.
* `Issue-Key wellarchitected|ListAnswers|2xx->2xx|$.NextToken` is DECLINED: an opaque server-generated continuation token, satisfying the modelled `NextToken` pattern and round-tripping, is not a value another implementation can reproduce byte for byte.

## Operation
GetAnswer

## Description
TWO causes, both in the AWS-official lens catalog this emulator ships (services/service-com-amazonaws-wellarchitected-wellarchitectedapiservicelambda/src/state/official_lenses.rs plus the framework question table it draws on, FRAMEWORK_LENS_QUESTIONS in .../src/state.rs). Every reading of official lens content goes through that catalog, so both causes are catalog-data defects rather than handler bugs — but they are distinct rows/fields and must be closed separately.

Cause 1 of 4 .. Cause 2 of 4 are the ANSWER content; Cause 3 and Cause 4 are the LENS SUMMARY content. Four causes in total, numbered below.

Cause 1 of 4: the framework lens's choice descriptions are paraphrases, not the lens's published prose (GetAnswer).
AWS: Smithy: GetAnswer -> GetAnswerOutput$Answer, Answer$Choices of Choice, Choice$Description; the model constrains the member but carries no catalog text, so recorded behavior only. The recording shows AWS serving the Well-Architected Framework lens's published choice prose verbatim for the costOptimization question the fixture answers (cloud-financial-management), e.g. Choices[0].Description = "Create a team (Cloud Business Office, Cloud Center of Excellence, or FinOps team) that is responsible for establishing and maintaining cost awareness across your organization. The owner of cost optimization can be individual or a team (requires people from finance, technology, and business teams) that understands the entire organization and cloud finance." and Choices[2].Description = "Adjust existing organizational budgeting and forecasting processes to be compatible with the highly variable nature of cloud costs and usage. Processes must be dynamic, using trend-based or business driver-based algorithms or a combination of both." The whole sentence is fixed text — a choice description has no variable part taken from the request or from state.
Emulator: serves re-written sentences of its own for the same ChoiceIds — "...The team requires people from finance, technology, and business roles across the organization." for Choices[0] and "Adjust existing organizational budgeting and forecasting processes to be dynamic and to account for the variable, on-demand nature of cloud cost and usage, using either a trend-based algorithm over historical cost and usage or an algorithm driven by business drivers, or a combination of both." for Choices[2]. Choices[3] through Choices[9] of the same question diverge the same way.
Why: a client that renders or diffs the lens's best-practice text (a console, a report generator, a workload-review exporter, or a test that pins the published wording) shows text AWS never published; a client keyed on the prose rather than the ChoiceId cannot match a choice at all.
Fix: the shipped question/choice content of the framework lens — the choice description strings in the framework question table the official-lens catalog projects answers from. Carry the published prose verbatim instead of an authored summary of it.

Cause 2 of 4: the answer's question-document URLs are synthesized, not the lens's published document (GetAnswer/UpdateAnswer).
AWS: Smithy: Answer$HelpfulResourceUrl and Answer$ImprovementPlanUrl (both @length(1,2048)); no value is modelled, so recorded behavior only. The recording carries the lens's own published document URL for the question, and the improvement-plan URL that AWS publishes for the same question — neither matches a URL assembled from the lens version and the question id.
Emulator: derives both from the catalog row — official_lenses::lens_question_document_url builds "<content root>/<lens.version>/<lens.alias>/<QuestionId>.html" and improvement_plan_url_of appends the "#improvement-plan" fragment to it — so the pair is a function of the lens version string rather than the document AWS publishes, and both members diverge on GetAnswer (task#5, task#7) and on UpdateAnswer's returned Answer (task#6).
Why: the two URLs are the only pointers an answer carries into the lens's guidance; a client that follows them reaches a document that does not exist, and the URL silently changes every time the lens version string moves.
Fix: the official-lens catalog's question rows — hold the published helpful-resource and improvement-plan URLs as catalog data per question, instead of formatting them from the version and question id.

Cause 3 of 4: the same two answer-content defects are served on the LIST path as well (ListAnswers).
AWS: Smithy: ListAnswers -> ListAnswersOutput$AnswerSummaries of AnswerSummary, AnswerSummary$Choices of Choice. Recorded behavior only, and the recorded summaries carry exactly the published choice prose quoted under Cause 1.
Emulator: AnswerSummaries[0].Choices[0] and Choices[2] through Choices[9] carry the same paraphrased descriptions, because the list projection reads the same catalog rows.
Why: a client that pages a lens review's answers (the normal way a review is read in bulk) sees the unpublished text everywhere, so fixing only GetAnswer leaves the list path wrong.
Fix: same catalog rows as Cause 1/Cause 2 — verify the list projection after the rows are corrected, because this is a separate replayed step and is asserted separately.

Cause 4 of 4: ListLenses reports every AWS-official lens's CreatedAt/UpdatedAt as midnight of its version day.
AWS: Smithy: ListLenses -> ListLensesOutput$LensSummaries of LensSummary, LensSummary$CreatedAt/$UpdatedAt (timestamps), LensSummary$LensAlias. The model states no values, so recorded behavior only: each row carries the real instant AWS first published the lens and the real instant it was last revised, which are ordinary times of day and are NOT derivable from the LensVersion string. In the recording the framework lens reports CreatedAt 1568405610 and UpdatedAt 1790853813 while carrying LensVersion 2025-02-25, and the Connected Mobility lens reports CreatedAt = UpdatedAt = 1711050610 while carrying LensVersion 2024-03-26 — i.e. UpdatedAt is independent of the version day in both directions.
Emulator: OfficialLens::created_at() returns midnight_utc(first_published or version) and summary() projects that pair, so every row's instants are 00:00:00Z of the day a version string names: 1568332800/1740441600 for the framework lens and 1711411200 for connectedmobility (a day LATER than the recorded instant). All 16 recorded LensSummaries mismatch — $.LensSummaries[0] through $.LensSummaries[15] — so no row of the published catalog matches.
Hypothesis (verify against the fixture before acting on it, the recording is the only evidence either way): the recorded summaries for the specialised lenses (connectedmobility, containerbuild, ...) carry no LensAlias member at all, while OfficialLens::summary() always sets lens_alias; only the framework row carries LensAlias in the recording. Since the comparison is whole-element, the member set has to be reconciled as well as the instants.
Why: CreatedAt vs UpdatedAt is the only signal in a LensSummary that tells a client a lens has ever been revised, and deriving both from the version string makes the signal circular — a lens revised without a version bump never appears revised, and every lens appears to have been created and revised at midnight. A client that caches on UpdatedAt never refreshes.
Fix: the catalog rows and the LensSummary projection in state/official_lenses.rs — carry each lens's published creation and last-revision instants as per-row data rather than computing them from the version day.

## Evidence
Replay of trace_set simparity-wire-aws-v1 (score 92.6%, 7/94 steps mismatched):

1. wellarchitected-lens-review-answer-lifecycle#task#5 GetAnswer, 200 vs 200, keys $.Answer.Choices[0].Description, [2], [3], [4], [5], [6], [7], [8], [9], $.Answer.HelpfulResourceUrl.
   expected $.Answer.Choices[0].Description = 'Create a team (Cloud Business Office, Cloud Center of Excellence, or FinOps team) that is responsible for establishing and maintaining cost awareness across your organization. The owner of cost optimization can be individual or a team (requires people from finance, technology, and business teams) that understands the entire organization and cloud finance.'
   actual   = 'Create a team (Cloud Business Office, Cloud Center of Excellence, or FinOps team) that is responsible for establishing and maintaining cost awareness across your organization. The team requires people from finance, technology, and business roles across the organization.'
   expected $.Answer.Choices[2].Description = 'Adjust existing organizational budgeting and forecasting processes to be compatible with the highly variable nature of cloud costs and usage. Processes must be dynamic, using trend-based or business driver-based algorithms or a combination of both.'
   actual   = 'Adjust existing organizational budgeting and forecasting processes to be dynamic and to account for the variable, on-demand nature of cloud cost and usage, using either a trend-based algorithm over historical cost and usage or an algorithm driven by business drivers, or a combination of both.'
   expected $.Answer.Choices[3].Description = 'Implement cost awareness, create transparency, and accountability of costs into new or existing processes that impact usage, and leverage existing processes for cost awareness. Implement cost awareness into employee training.'
   actual   = 'Implement cost awareness, create transparency, and establish accountability for costs in new and existing processes that influence usage, and leverage existing processes for cost awareness. Implement cost awareness into employee training.'
   $.Answer.HelpfulResourceUrl also value_mismatch.

2. wellarchitected-lens-review-answer-lifecycle#task#6 UpdateAnswer, 200 vs 200: same Choices[0..9].Description divergences plus $.Answer.HelpfulResourceUrl AND $.Answer.ImprovementPlanUrl.

3. wellarchitected-lens-review-answer-lifecycle#task#7 GetAnswer, 200 vs 200: same set as (2).

4. wellarchitected-pagination-and-limit-validation#task#7 ListAnswers, 200 vs 200: $.AnswerSummaries[0].Choices[0].Description and [2]..[9] diverge with the identical expected/actual pairs.

5. wellarchitected-catalog-list-lenses#task#1 ListLenses, 200 vs 200: $.LensSummaries[0] .. $.LensSummaries[15] all reported missing (whole-element mismatch).
   expected LensSummaries[0] = {..., 'LensVersion': '2025-02-25', 'CreatedAt': 1568405610, 'UpdatedAt': 1790853813, ...}
   actual   LensSummaries[0] = {..., 'LensVersion': '2025-02-25', 'CreatedAt': 1568332800 (2019-09-13T00:00:00Z), 'UpdatedAt': 1740441600 (2025-02-25T00:00:00Z), ...}
   expected LensSummaries[1] (connectedmobility) = {'LensName': 'Connected Mobility Lens', 'LensArn': 'arn:aws:wellarchitected::aws:lens/connectedmobility', 'LensVersion': '2024-03-26', 'CreatedAt': 1711050610, 'UpdatedAt': 1711050610, ...} with NO LensAlias member
   actual   LensSummaries[1] = {..., 'LensAlias': 'connectedmobility', 'CreatedAt': 1711411200 (2024-03-26T00:00:00Z), 'UpdatedAt': 1711411200, ...}

Source of the emulator behavior: services/service-com-amazonaws-wellarchitected-wellarchitectedapiservicelambda/src/state/official_lenses.rs — OfficialLens::created_at() = midnight_utc(first_published or version) (line ~297), OfficialLens::summary() (line ~308, always sets lens_alias), lens_question_document_url() (line ~127) and improvement_plan_url_of() (line ~136).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Cause 1/2: replay fixture wellarchitected-lens-review-answer-lifecycle step task#5 (GetAnswer on the workload's wellarchitected lens review, costOptimization question cloud-financial-management) and compare $.Answer.Choices[*].Description, $.Answer.HelpfulResourceUrl and $.Answer.ImprovementPlanUrl against the recorded response; repeat for task#6 (UpdateAnswer) and task#7 (GetAnswer).
3. Cause 3: replay fixture wellarchitected-pagination-and-limit-validation step task#7 (ListAnswers, PillarId=costOptimization with MaxResults) and compare $.AnswerSummaries[0].Choices[*].Description.
4. Cause 4: replay fixture wellarchitected-catalog-list-lenses step task#1 (GET /lenses) and compare every $.LensSummaries[i] element — CreatedAt/UpdatedAt and the member set — against the recorded summaries.
