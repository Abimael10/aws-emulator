Issue-Source: trace-sweep-divergence
Issue-Trace: wellarchitected-catalog-list-lenses
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: wellarchitected-catalog-list-lenses wellarchitected-lens-review-answer-lifecycle wellarchitected-pagination-and-limit-validation
Issue-Shape-Id: com.amazonaws.wellarchitected#WellArchitectedApiServiceLambda
Issue-Key: wellarchitected|ListLenses|2xx->2xx|$.LensSummaries[*]
Issue-Key: wellarchitected|GetAnswer|2xx->2xx|$.Answer.Choices[*].Description
Issue-Key: wellarchitected|GetAnswer|2xx->2xx|$.Answer.HelpfulResourceUrl
Issue-Key: wellarchitected|UpdateAnswer|2xx->2xx|$.Answer.Choices[*].Description
Issue-Key: wellarchitected|UpdateAnswer|2xx->2xx|$.Answer.HelpfulResourceUrl
Issue-Key: wellarchitected|UpdateAnswer|2xx->2xx|$.Answer.ImprovementPlanUrl
Issue-Key: wellarchitected|GetAnswer|2xx->2xx|$.Answer.ImprovementPlanUrl
Issue-Key: wellarchitected|ListAnswers|2xx->2xx|$.AnswerSummaries[*].Choices[*].Description
Issue-Key: wellarchitected|ListAnswers|2xx->2xx|$.NextToken
Issue-Commit: c11f37d9666aeec66c301c48be9695cec998b743

# AWS-official lens content served by Well-Architected diverges from AWS's published catalog, answer projections and cursor

> _Note: this issue was identified against the code base at commit `c11f37d9666aeec66c301c48be9695cec998b743`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListLenses

## Description
This bundle carries FIVE causes, all in the wellarchitected emulator. Every one of them is a 200 response whose BODY content diverges from the recorded AWS response: the AWS-official lens library this emulator ships (its rows and their metadata), the published choice prose and per-question document URLs the framework lens's answers project, and the pagination cursor ListAnswers hands back. A fixer must close all five before submitting.

Cause 1 of 5: ListLenses serves a different AWS-official lens library than AWS publishes.
AWS: Smithy: ListLenses GET /lenses -> 200, ListLensesOutput$LensSummaries is a list of LensSummary{LensArn, LensAlias, LensName (1..128), LensType (enum, AWS_OFFICIAL), Description (LensDescription 1..1024), CreatedAt, UpdatedAt, LensVersion (1..32), Owner (AwsAccountId), LensStatus}. The model bounds the members but states no catalog content, so the library itself is recorded behavior only: the recording enumerates AWS's official lens library in a fixed order where element index 2 is {LensName 'Container Build Lens', LensArn 'arn:aws:wellarchitected::aws:lens/containerbuild', LensType AWS_OFFICIAL, Owner 'AWS', LensStatus 'PUBLISHED', LensVersion '2024-03-26'}, index 3 is the Data Analytics Lens, and each specialised lens carries the FULL published description paragraph AWS's lens catalog publishes for it (for the Data Analytics Lens: 'The Data Analytics Lens contains insights that AWS has gathered from real-world case studies, ...') plus that lens's own CreatedAt/UpdatedAt publication instants; the recorded official-lens summary carries no LensAlias member.
Emulator: elements 0 and 1 match and every element from index 2 through index 15 diverges. The emulator's index 2 is the Data Analytics Lens and index 3 the DevOps Lens, i.e. the recorded Container Build Lens row is absent from the shipped catalog and every later row is shifted; each specialised row reports the catalog SHORT FORM description 'Best practices for <subject> workload' instead of the published paragraph, reports LensVersion '2025-01-15' (the emulator's own AUTHORED_CONTENT_REVISION) instead of the published version string, reports CreatedAt and UpdatedAt as the start of the day that version names instead of the lens's publication instants, and adds a LensAlias member the recorded summary does not carry.
Why: a client that enumerates the lens library to discover which lenses it may apply gets the wrong set and the wrong order — a lens AWS publishes (containerbuild) is undiscoverable — and a client that caches a lens's Description/LensVersion/CreatedAt/UpdatedAt to detect a lens revision compares against values AWS never published, so its upgrade detection and its rendering of the catalog are both wrong.
Fix: the state backing ListLenses — the AWS-official lens catalog rows and the metadata form they are spelled from, and the LensSummary projection those rows feed.
Traces: wellarchitected-catalog-list-lenses#task#1 (ListLenses, 200 vs 200, $.LensSummaries[2] through $.LensSummaries[15]).

Cause 2 of 5: GetAnswer projects framework-lens choice prose and question document URLs that are not the lens's published content.
AWS: Smithy: GetAnswer GET /workloads/{WorkloadId}/lensReviews/{LensAlias}/answers/{QuestionId} -> 200, GetAnswerOutput$Answer is Answer{Choices: list of Choice{ChoiceId (1..64), Title (1..512), Description (ChoiceDescription 1..1024)}, HelpfulResourceUrl (1..2048), ImprovementPlanUrl (1..2048)}. The model carries no choice text, so the content is recorded behavior only: for the framework lens's COST 1 question (QuestionId cloud-financial-management) the recording's published choice prose is, at Choices[4], 'Implement tools and dashboards to monitor cost proactively for the workload. ...'; at Choices[6] the FULL published sentence pair ending '... Quantifying business value can help you gain more buy-in from stakeholders on future cost optimization investments, and provides a framework to measure the outcomes for your organization's cost optimization activities.'; at Choices[7] 'Set up cloud budgets and configure mechanisms to detect anomalies in usage. Configure related tools for cost and usage alerts against pre-defined targets and receive notifications when any usage exceeds those targets. Have regular meetings to analyze the cost-effectiveness of your workloads and promote cost awareness.'; and distinct published text at Choices[8] and at Choices[9] (the terminal 'None of these' choice). The answer's HelpfulResourceUrl in the recording is the document AWS publishes for that question.
Emulator: the same five choice Descriptions differ. The shipped framework-lens content says 'Implement tooling and dashboards ...' where the lens publishes 'Implement tools and dashboards ...'; the quantify-business-value choice is cut short after '... explain the return on investment to stakeholders.'; the report-and-notify choice reads 'Configure AWS Budgets to provide notifications on cost and usage against targets. Have regular meetings to analyze cost efficiency of this workload and promote cost aware culture.'; the cost-aware-culture choice and the projected 'None of these' choice likewise carry prose of the emulator's own. HelpfulResourceUrl is reported as value_mismatch against the recording: the emulator SYNTHESISES it as '<lens content root>/wat.question.<PILLAR_ABBREVIATION>_<position>.en.html' (for this question, COST_1) from its own published-label rule rather than serving the recorded document URL.
Why: a choice Description is fixed published text with no part taken from the request or from state, so a client that renders the best practice, or that asserts on the lens's published wording, shows text AWS does not publish — including a truncated best practice — and a client that follows HelpfulResourceUrl is sent to an address other than the question's recorded document.
Fix: the framework lens's COST 1 question rows in the lens content state (the choice Descriptions), the projection of the terminal 'None of these' choice, and the per-question document URL derivation the answer projection uses for HelpfulResourceUrl/ImprovementPlanUrl.
Traces: wellarchitected-lens-review-answer-lifecycle#task#5 (GetAnswer, 200 vs 200, $.Answer.Choices[4|6|7|8|9].Description, $.Answer.HelpfulResourceUrl).

Cause 3 of 5: UpdateAnswer returns the same diverging choice prose plus a diverging ImprovementPlanUrl.
AWS: Smithy: UpdateAnswer PATCH /workloads/{WorkloadId}/lensReviews/{LensAlias}/answers/{QuestionId} -> 200, UpdateAnswerOutput$Answer is the same Answer shape, so the response to an update must restate the lens's published choice catalogue and the question's published document URLs exactly as a read of it does. Published content as in Cause 2 (recorded behavior only).
Emulator: UpdateAnswer's projection of the answer repeats the Cause 2 divergences ($.Answer.Choices[4|6|7|8|9].Description, $.Answer.HelpfulResourceUrl) and additionally diverges on $.Answer.ImprovementPlanUrl, which the emulator builds as the HelpfulResourceUrl document plus the fragment '#improvement-plan'.
Why: the choice catalogue a client gets back from a write is the one it uses to drive the next write (ChoiceId -> Title/Description) and to render the improvement plan link, so the write path hands back content and links AWS does not publish even when the write itself succeeded.
Fix: shared with Cause 2 — the lens content rows and the document-URL derivation; UpdateAnswer's answer projection must read the corrected content.
Traces: wellarchitected-lens-review-answer-lifecycle#task#6 (UpdateAnswer, 200 vs 200).

Cause 4 of 5: the re-read of the updated answer shows the divergence persists after the write.
AWS: as Cause 2 — GetAnswer's Answer restates the lens's published choice prose and the question's published HelpfulResourceUrl/ImprovementPlanUrl, unchanged by the update that preceded it (recorded behavior only).
Emulator: the post-update GetAnswer reports the same five choice Description mismatches plus $.Answer.HelpfulResourceUrl and $.Answer.ImprovementPlanUrl, confirming the content is wrong in the stored lens catalogue rather than in one handler's response assembly.
Why: the defect is not recoverable by re-reading, so every reading of this question in a review shows non-published prose and non-published links.
Fix: shared with Cause 2 — it is the lens content state, which is why the read, the write and the list below all show it.
Traces: wellarchitected-lens-review-answer-lifecycle#task#7 (GetAnswer, 200 vs 200).

Cause 5 of 5: ListAnswers repeats the choice-prose divergence in AnswerSummaries AND returns a NextToken that is a hex-encoded internal cursor document.
AWS: Smithy: ListAnswers GET /workloads/{WorkloadId}/lensReviews/{LensAlias}/answers -> 200, paginated {inputToken NextToken, outputToken NextToken, pageSize MaxResults}; AnswerSummaries is a list of AnswerSummary{..., Choices: the same Choice list}, and NextToken is 'The token to use to retrieve the next set of results' with pattern ^[A-Za-z0-9+\/=_-]+$ — an OPAQUE continuation token. The recording's token is shape-asserted rather than value-asserted (recorded behavior only: a token of AWS's own opaque form), and the recorded AnswerSummaries[0].Choices descriptions are the published prose of Cause 2.
Emulator: $.AnswerSummaries[0].Choices[4|6|7|8|9].Description carry the same non-published prose as Cause 2, and $.NextToken is reported as value_mismatch against the recorded token shape: the emulator emits '7b2271223a22636c6f75642d66696e616e6369616c2d6d616e6167656d656e74227d', the hex encoding of the JSON document {"q":"cloud-financial-management"} — i.e. the shared question cursor serialises its internal resume key as readable JSON and hex-encodes it, so the token is not of the opaque form the recording shows. Hypothesis: the corpus's shape assertion on NextToken encodes AWS's token form (a padded base64-class opaque blob), which a hex-encoded JSON document does not satisfy.
Why: the choice prose is wrong for every answer the list reports, not just for the one question read directly; and the cursor publishes the emulator's internal pagination key to any client that inspects or logs it, so a client written against AWS's token form (length/alphabet assertions, or storing a token it believes opaque and fixed-shape) behaves differently here than against AWS.
Fix: shared lens content state for the Choices half; for the token, the shared question-cursor token encoding in the service state that ListAnswers' page helper issues and parses (it must mint an opaque token that does not restate the internal key in cleartext, and must keep accepting only tokens it issued).
Traces: wellarchitected-pagination-and-limit-validation#task#7 (ListAnswers, 200 vs 200, $.NextToken and $.AnswerSummaries[0].Choices[*].Description).

Not part of this bundle: the sixth divergence in this corpus (wellarchitected-pagination-and-limit-validation#task#6, ListAnswers' ValidationException message for an undefined PillarId) is already owned by the open issue 'ListAnswers refuses an undefined PillarId with a constant message that diverges from AWS's ValidationException diagnostic' (20261120T200450Z-wellarchitected-listanswers-refuses-an-undefined-pillarid-with-a-constant.md), which carries its replay key; it is skipped here rather than refiled.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 94 steps, 88 passed, 6 mismatched, score 93.6%, emulator_up=true, bootstrap_signal=false. All 6 divergences are service=wellarchitected; 1 is owned by an existing open issue, the 5 below are net-new. No cluster carried a stub_path.

1) wellarchitected-catalog-list-lenses#task#1 — ListLenses, expected 200 / actual 200.
   divergence_key: service=wellarchitected|ListLenses|status=200->200|keys=$.LensSummaries[2..15]
   $.LensSummaries[2]: missing — expected {'LensName': 'Container Build Lens', 'LensArn': 'arn:aws:wellarchitected::aws:lens/containerbuild', 'Description': 'The Container Build Lens will focus specifically on the container design and build process...', 'LensVersion': '2024-03-26', 'Owner': 'AWS', 'LensStatus': 'PUBLISHED', 'CreatedAt': 1711463669, 'UpdatedAt': 1711463669, 'LensType': 'AWS_OFFICIAL'}; got {'LensArn': 'arn:aws:wellarchitected::aws:lens/dataanalytics', 'LensAlias': 'dataanalytics', 'LensName': 'Data Analytics Lens', 'LensType': 'AWS_OFFICIAL', 'Description': 'Best practices for Data Analytics workload', 'CreatedAt': 1736899200, 'UpdatedAt': 1736899200, 'LensVersion': '2025-01-15', 'Owner': 'AWS', 'LensStatus': 'PUBLISHED'}
   $.LensSummaries[3]: missing — expected the Data Analytics Lens with its published description paragraph and LensVersion '2024-03-26'; got the DevOps Lens with 'Best practices for DevOps workload' / '2025-01-15'. Indices 4..15 diverge the same way.

2) wellarchitected-lens-review-answer-lifecycle#task#5 — GetAnswer, 200 / 200.
   $.Answer.Choices[4].Description: expected 'Implement tools and dashboards to monitor cost proactively for the workload. ...'; got 'Implement tooling and dashboards to monitor cost proactively for the workload. ...'
   $.Answer.Choices[6].Description: expected '... quantifying business value allows you to explain the return on investment to stakeholders. Quantifying business value can help you gain more buy-in from stakeholders on future cost optimization investments, and provides a framework to measure the outcomes for your organization's cost optimization activities.'; got the same text truncated after '... explain the return on investment to stakeholders.'
   $.Answer.Choices[7].Description: expected 'Set up cloud budgets and configure mechanisms to detect anomalies in usage. ...'; got different text.
   also $.Answer.Choices[8].Description, $.Answer.Choices[9].Description, $.Answer.HelpfulResourceUrl.

3) wellarchitected-lens-review-answer-lifecycle#task#6 — UpdateAnswer, 200 / 200. Same five Choices[*].Description mismatches plus $.Answer.HelpfulResourceUrl and $.Answer.ImprovementPlanUrl.

4) wellarchitected-lens-review-answer-lifecycle#task#7 — GetAnswer, 200 / 200. Same keys as (3), so the write did not repair the content.

5) wellarchitected-pagination-and-limit-validation#task#7 — ListAnswers, 200 / 200.
   $.NextToken: expected '<shaped-not-asserted>', got '7b2271223a22636c6f75642d66696e616e6369616c2d6d616e6167656d656e74227d' (hex of {"q":"cloud-financial-management"}).
   $.AnswerSummaries[0].Choices[4|6|7|8|9].Description: the same prose mismatches as (2).

Corroboration from the checkout: services/service-com-amazonaws-wellarchitected-wellarchitectedapiservicelambda/src/state/framework_lens.rs:635 holds 'Implement tooling and dashboards ...', :639 the truncated quantify-business-value sentence, :641 'Configure AWS Budgets to provide notifications on cost and usage against targets. ...', :643 the cost-aware-culture text — the COST 1 (cloud-financial-management) choice rows. services/.../src/state/official_lenses.rs:235 defines AUTHORED_CONTENT_REVISION = "2025-01-15" and :242-250 the 'Best practices for <subject> workload' short-form description the specialised catalog rows are spelled from; :190-200 derive HelpfulResourceUrl as '{LENS_CONTENT_ROOT}/wat.question.{LABEL}.en.html' and ImprovementPlanUrl as that URL plus '#improvement-plan'. The ListAnswers cursor comes from crate::state::question_page (services/.../src/ops/list_answers.rs:267, token returned at :305).

## Steps to Reproduce
1. git_clone() the project and let the emulator be built from the checkout.
2. replay_wire_corpus() — trace set simparity-wire-aws-v1.
3. Cause 1: replay fixture wellarchitected-catalog-list-lenses, step task#1 (GET /lenses). Compare LensSummaries element by element against the recording: elements 2..15 diverge in membership/order, Description form, LensVersion, CreatedAt/UpdatedAt and the presence of LensAlias.
4. Causes 2-4: replay fixture wellarchitected-lens-review-answer-lifecycle, steps task#5 (GetAnswer), task#6 (UpdateAnswer) and task#7 (GetAnswer) for the framework lens question cloud-financial-management, and compare Answer.Choices[4],[6],[7],[8],[9].Description plus Answer.HelpfulResourceUrl / Answer.ImprovementPlanUrl against the recorded response.
5. Cause 5: replay fixture wellarchitected-pagination-and-limit-validation, step task#7 (GET .../answers with a page size that leaves a continuation). Compare AnswerSummaries[0].Choices[*].Description and NextToken; hex-decode the returned NextToken to see the internal cursor document {"q":"cloud-financial-management"}.
6. All five steps must pass before the bundle is closed; step task#6 of the pagination fixture belongs to a different, already-open issue.
