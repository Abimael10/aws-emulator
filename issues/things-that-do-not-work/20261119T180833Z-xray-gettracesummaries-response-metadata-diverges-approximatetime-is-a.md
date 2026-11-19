Issue-Source: trace-sweep-divergence
Issue-Trace: xray-trace-summaries-hourly-pagination
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: xray-trace-summaries-hourly-pagination xray-trace-window-unit-mistakes
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray|GetTraceSummaries|2xx->2xx|$.ApproximateTime
Issue-Related: 20261116T082341Z-xray-gettracesummaries-nexttoken-is-not-an-opaque-aws.md
Issue-Commit: ac7af8ba0fa4d606f78d87a5edcd23b822b52815

# xray GetTraceSummaries response metadata diverges: ApproximateTime is a window-relative step start instead of the hour-aligned instant, and NextToken is the raw page index

> _Note: this issue was identified against the code base at commit `ac7af8ba0fa4d606f78d87a5edcd23b822b52815`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTraceSummaries

## Description
TWO net-new wire root causes in the xray service, both in the non-result metadata GetTraceSummaries hands back with its 200 (the TraceSummaries list itself matched on these steps). They are bundled because they are produced side by side by the same page-assembly code path in the GetTraceSummaries handler and its paging state module. xray answers this corpus (104/126 steps pass, emulator_up true, bootstrap_signal false), so these are fidelity defects, not a bootstrap gap. Both divergence clusters carry an EMPTY owned_by and an EMPTY stub_path: no open issue carries their replay key.

RELATED, NOT COVERING (named for the fixer; none of these carries either fixture step of this issue):
- 20261116T082341Z-xray-gettracesummaries-nexttoken-is-not-an-opaque-aws.md is an open issue about the SAME property as Cause 2, but it was written against an older token form (the bare epoch hour boundary, e.g. '1794812400', optionally ':<trace-id>'); the emulator has since changed the token to a bare page INDEX ('1'), its divergence key moved, and the replay reports this cluster as unowned. Cause 2 restates the requirement against the token the emulator serves today; a fixer closing that issue should close this cause with it.
- 20261115T140926Z-xray-gettracesummaries-hides-in-window-traces-pages-are.md (behavioral) and the FIXED 20261103T034719Z issue are about WHICH summaries a page carries and about window-relative stepping. Cause 1 is NOT a request to undo window-relative page SELECTION — only the instant the response reports is wrong.
- 20261109T114723Z-xray-segment-derived-trace-reads-lose-data-gettracesummaries.md is about the CONTENT of a TraceSummary, not the page metadata.

Cause 1 of 2: GetTraceSummaries reports an ApproximateTime that is derived from the requested window's per-page step (EndTime minus one page-length, clipped to StartTime), so the instant is an arbitrary non-aligned clock value and it differs from page to page of one query. AWS reports a single hour-aligned instant — the top of the hour the queried window ends in — and reports the SAME instant on every page of that query.

AWS: Smithy (services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json) models GetTraceSummaries as POST /TraceSummaries, success 200, StartTime/EndTime required, @paginated {inputToken: NextToken, outputToken: NextToken, items: TraceSummaries}, errors InvalidRequestException (client, 400) and ThrottledException (429); GetTraceSummariesResult$ApproximateTime targets Timestamp with the prose doc 'The start time of this page of results' and no constraint on its granularity. The granularity is therefore recorded behavior only: every recorded GetTraceSummaries 200 in this corpus asserts an ApproximateTime that is an exact multiple of 3600 epoch-seconds and equals the hour boundary at or before the query's EndTime, and the SAME value is asserted on every page of the paged fixture and on a second, independent fixture recorded in the same session — i.e. the reported instant is a function of the hour the window ends in, not of which page is being served. The general requirement: ApproximateTime is the start of the hour-aligned bucket the requested window ends in (floor(EndTime / 3600) * 3600), identical for all pages of one query, and inside the requested window.

Emulator: ApproximateTime is set to the current page's own step start, so it answers an unaligned instant one page-length before the requested EndTime. On the replayed steps the recordings expect 1795111200 (an exact hour boundary) and the emulator answers 1795107953 — not a multiple of 3600, 3247 seconds earlier than the required instant, and a value that would further change with each page of the same query. Status 200 and every other asserted member matched on these steps.

Why: a client reads ApproximateTime to label the page of results it is holding and to anchor a 'traces from HH:00' view or a resumable sweep. Against AWS that label is a stable bucket boundary shared by every page of one query; against the emulator it is an unaligned instant that moves per page, so a client bucketing or de-duplicating by ApproximateTime produces as many buckets as it fetched pages, and any code or recorded-response comparison that checks the instant is hour-aligned (or that two pages of one query agree on it) fails on the emulator and succeeds on AWS.

Fix: the ApproximateTime computation in the GetTraceSummaries handler's response assembly (services/service-com-amazonaws-xray-awsxray/src/ops/get_trace_summaries.rs, the approximate_time field of the returned GetTraceSummariesOutput, around lines 547-563), reading the aligned instant from the paging state module (services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs) so the handler and the paging rule cannot disagree. Keep the window-relative page SELECTION exactly as it is (that is what 20261115T140926Z requires); only the instant reported changes, and it must be the same on every page of a query.

Traces: xray-trace-summaries-hourly-pagination#task#1 (representative, cluster service=xray|GetTraceSummaries|status=200->200|keys=$.ApproximateTime,$.NextToken), xray-trace-summaries-hourly-pagination#task#2, xray-trace-window-unit-mistakes#task#5, and xray-trace-summaries-hourly-pagination#task#4 (cluster service=xray|GetTraceSummaries|status=200->200|keys=$.ApproximateTime).

Cause 2 of 2: the NextToken GetTraceSummaries hands back is the emulator's internal paging cursor in clear text — the decimal page index, optionally suffixed with ':' and the trace id to resume at — instead of an opaque pagination token of the shape AWS issues.

AWS: Smithy models GetTraceSummaries @paginated {inputToken: NextToken, outputToken: NextToken, items: TraceSummaries}; GetTraceSummariesResult$NextToken targets String with no length, pattern or format constraint, documented 'If the requested time frame contained more than one page of results, you can use this token to retrieve the next page. The first page contains the most recent results, closest to the end of the time frame'. The model states no token FORMAT, so for the format this is recorded behavior only: every recording that pages this read carries a NextToken asserted by SHAPE rather than by value (recorded as '<shaped-not-asserted>' precisely because AWS's token is an opaque, run-specific blob), and the emulator's value fails that shape assertion. AWS issues an opaque cursor; it does not publish a small decimal ordinal, nor a readable trace id, as the token.

Emulator: the token is composed by crate::state::page_token as either the page index rendered with to_string() or '<index>:<trace-id>' when the step still holds summaries — the replayed step answers the bare '1'. The response status (200) and the summaries matched; $.NextToken is reported as value_mismatch against the asserted token shape.

Why: the model and the docs tell a client NextToken is opaque, and the emulator publishes a parseable internal cursor instead. Anything that validates, stores or canonicalises a cursor against the shape AWS emits (SDK/CLI pagination doubles, recorded-response contract checks, proxies that hash a cursor) rejects or mangles it — which is exactly what this replay observed — and the value leaks the service's paging scheme (the ordinal and the next trace id), so code written against the emulator can come to depend on decoding the token and then break on AWS. A single-character ordinal is also trivially guessable, so a client can fabricate a 'token' the service never issued and be served a page.

Fix: the token composition and parsing for this read — crate::state::page_token / crate::state::parse_page_token in services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs (around lines 136-164) and the next_token assembly in the GetTraceSummaries handler that calls them (services/service-com-amazonaws-xray-awsxray/src/ops/get_trace_summaries.rs, around lines 528-545). Emit an opaque, self-contained cursor encoding the same (page index, resume trace) pair so the value exposes no readable ordinal or trace id, keep the resume path round-tripping the emulator's own tokens, and keep refusing a token this read never issued with the existing InvalidRequestException wording. Which summaries a page carries does not change for this cause.

Traces: xray-trace-summaries-hourly-pagination#task#1 (representative), xray-trace-summaries-hourly-pagination#task#2, xray-trace-window-unit-mistakes#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: 21 fixtures replayed, 126 steps, 104 passed, 22 mismatched, 0 errored, 0 blocked, score 82.5%, emulator_up true ('/__admin/health answered 200 and the :46711 data listener accepts'), bootstrap_signal false. 11 divergence clusters, all service=xray; 9 carry a non-empty owned_by and are skipped. The two clusters below are the only ones with owned_by == [] and stub_path == "".

Cluster A (Cause 1 + Cause 2), divergence_key: service=xray|GetTraceSummaries|status=200->200|keys=$.ApproximateTime,$.NextToken; verdict mismatch; expected_status 200, actual_status 200; affected_count 3.
detail:
  $.ApproximateTime: value_mismatch (expected 1795111200, got 1795107953)
  $.NextToken: value_mismatch (expected '<shaped-not-asserted>', got '1')
affected: xray-trace-summaries-hourly-pagination#task#1 (representative), xray-trace-summaries-hourly-pagination#task#2, xray-trace-window-unit-mistakes#task#5.

Cluster B (Cause 1), divergence_key: service=xray|GetTraceSummaries|status=200->200|keys=$.ApproximateTime; verdict mismatch; expected_status 200, actual_status 200; affected_count 1.
detail: $.ApproximateTime: value_mismatch (expected 1795111200, got 1795107953)
affected: xray-trace-summaries-hourly-pagination#task#4 (representative).

Cause 1 — Expected: a 200 whose ApproximateTime is the hour-aligned start of the bucket the requested window ends in: 1795111200 in this run is exactly 498642 * 3600, and the SAME value is asserted on three pages of the paged fixture and on the unrelated xray-trace-window-unit-mistakes fixture recorded in the same session (the replay's own clock in this run is ~1795111539, whose hour floor is 1795111200). Actual: 1795107953 — not a multiple of 3600, and exactly 3600 s before an EndTime of ~1795111553, i.e. the current page's window-relative step start. Only the per-run absolute epoch values differ between environments; the requirement is the alignment and the per-query stability, not the literal 1795111200.

Cause 2 — Expected: an opaque pagination token (the fixture asserts the token's SHAPE, hence '<shaped-not-asserted>'). Actual: the bare decimal '1'. The same code path also emits '<index>:<trace-id>' when a step holds more summaries than one page carries.

Source in the checkout (citations, not requirements): services/service-com-amazonaws-xray-awsxray/src/ops/get_trace_summaries.rs lines 547-563 (approximate_time = page.start, documented there as 'where this page's own STEP of the requested window starts'), lines 528-545 (next_token composed from crate::state::page_token), line 576 (TRACE_SUMMARIES_PAGE_SIZE = 5); services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs lines 39 (TRACE_PAGE_SECONDS = 3600), 45 (TOKEN_SEPARATOR ':'), 123-134 (window_page: start = step_back(end, index+1).max(start)), 139-144 (page_token: index.to_string() or '<index>:<trace>'), 156-164 (parse_page_token). Smithy: GetTraceSummariesResult$ApproximateTime: Timestamp, 'The start time of this page of results'; GetTraceSummariesResult$NextToken: String, unconstrained, @paginated outputToken; errors InvalidRequestException (400), ThrottledException (429).

## Steps to Reproduce
1. git_clone, then replay_wire_corpus (it builds and boots the emulator from the checkout); confirm bootstrap_signal is false and emulator_up is true.
2. Observe the two unowned clusters: service=xray|GetTraceSummaries|status=200->200|keys=$.ApproximateTime,$.NextToken (representative xray-trace-summaries-hourly-pagination#task#1) and service=xray|GetTraceSummaries|status=200->200|keys=$.ApproximateTime (representative xray-trace-summaries-hourly-pagination#task#4).
3. Reproduce Cause 1 directly against the running emulator, with a window wider than one hour so the read pages:
   a. POST /TraceSummaries with {"StartTime": <epoch now-3*3600>, "EndTime": <epoch now>} and no NextToken.
   b. Observe HTTP 200 whose ApproximateTime is NOT a multiple of 3600 and equals EndTime-3600. Expected: floor(EndTime/3600)*3600, the top of the hour the window ends in.
   c. Feed the returned NextToken back and observe ApproximateTime CHANGE on the next page. Expected: every page of the same query reports the same hour-aligned instant.
   d. Repeat (a) with a window shorter than one hour: ApproximateTime must again be the hour boundary at or before EndTime, and must still lie inside the requested window.
4. Reproduce Cause 2 from the same response: the NextToken of step 3(b) is the bare decimal page ordinal '1' (or '<ordinal>:<16-hex trace id>' when a step holds more than 5 matching traces), i.e. a directly readable internal cursor rather than an opaque token.
5. After the fix: (i) ApproximateTime is hour-aligned, identical on every page of one query, and inside [StartTime, EndTime); (ii) NextToken is opaque — no readable ordinal or trace id — still resumes the same page when fed back, and a token this read never issued is still refused with the existing InvalidRequestException wording; (iii) which summaries each page carries is unchanged (a window no longer than one page still returns every trace it holds, per 20261115T140926Z).
6. Re-replay the fixture steps in Traces/trace_ids — xray-trace-summaries-hourly-pagination#task#1, #task#2 and #task#4, and xray-trace-window-unit-mistakes#task#5 — all four must pass with neither $.ApproximateTime nor $.NextToken reported.
