Issue-Source: trace-sweep-divergence
Issue-Trace: xray-put-segment-is-found-by-trace-summaries-and-batch-get
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: xray-put-segment-is-found-by-trace-summaries-and-batch-get
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray|GetTraceSummaries|2xx->2xx|$
Issue-Key: xray|GetTraceSummaries|2xx->2xx|$.TraceSummaries
Issue-Key: xray|GetTraceSummaries|2xx->2xx|$.TraceSummaries[*]
Issue-Related: 20261116T082341Z-xray-gettracesummaries-nexttoken-is-not-an-opaque-aws.md
Issue-Related: 20261117T090620Z-xray-gettracesummaries-pages-a-sub-hour-window-on.md
Issue-Commit: f6abb117b9da04931ba67db417fb20a8f8b758b1

# xray GetTraceSummaries withholds a just-ingested in-window trace: the first page is the empty absolute-hour slice the window ends in

> _Note: this issue was identified against the code base at commit `f6abb117b9da04931ba67db417fb20a8f8b758b1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTraceSummaries

## Description
ONE net-new WIRE root cause in the xray service (one divergence cluster, 1 replayed step). xray answers this corpus (105/126 steps pass, emulator_up true, bootstrap_signal false), so this is a fidelity defect, not a bootstrap gap. The replay report marks this cluster with an EMPTY owned_by and an EMPTY stub_path: no open issue carries its replay key.

RELATED, NOT COVERING (named for the fixer):
- 20261117T090620Z-xray-gettracesummaries-pages-a-sub-hour-window-on.md DOES carry this same fixture step and already owns the two ENVELOPE members reported in the same detail block: $.NextToken being emitted at all on a complete answer, and $.ApproximateTime snapped to the hour-grid boundary. Those two are deliberately NOT re-filed here. That issue explicitly records that in its run "the matching summary WAS returned" and that it is "only about the two response-envelope members"; this issue is about the summary NOT being returned at all, which it does not describe and which a fix limited to the envelope would leave standing.
- 20261109T114723Z-xray-segment-derived-trace-reads-lose-data-gettracesummaries.md carries this step for the CONTENT of a returned TraceSummary (omitted list members, Revision, MatchedEventTime, ServiceIds). It presupposes a summary is returned; here the list is empty, so there is no summary for it to describe.
- 20261115T140926Z-xray-gettracesummaries-hides-in-window-traces-pages-are.md and 20261116T160129Z-xray-gettracesummaries-never-surfaces-a-just-ingested-trace.md are BEHAVIORAL issues describing the same hour-grid mechanism from the suite side; neither carries a wire fixture step, so neither carries this cluster's replay key. A fixer closing them should close this cause in the same change, but this wire step must be re-replayed to confirm it.
- 20261116T082341Z-xray-gettracesummaries-nexttoken-is-not-an-opaque-aws.md is about the FORM of a token, not about which summaries a page carries.

Cause 1 of 1: a GetTraceSummaries query over a minutes-wide window that holds exactly one just-accepted trace answers 200 with an EMPTY TraceSummaries list, because only the one-hour slice of the ABSOLUTE epoch-hour grid that the window's EndTime falls in is served and the trace lies in the window's earlier slice. The trace never appears, however long the caller polls.

AWS: Smithy (services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json) models GetTraceSummaries as POST /TraceSummaries, success 200, StartTime/EndTime required, @paginated {inputToken: NextToken, outputToken: NextToken, items: TraceSummaries}, errors InvalidRequestException (client, 400) and ThrottledException (client, 429). GetTraceSummariesResult$TraceSummaries is documented "Trace IDs and annotations for traces that were found in the specified time frame"; $TracesProcessedCount is "The total number of traces processed, including traces that did not match the specified filter expression"; $NextToken is "If the requested time frame contained more than one page of results, you can use this token to retrieve the next page. The first page contains the most recent results, closest to the end of the time frame." So membership of the answer is keyed on the requested TIME FRAME, and whether a further page exists is keyed on how many RESULTS that frame holds — never on how many absolute hour boundaries the frame happens to cross; and the first page is the frame's MOST RECENT results, which is exactly where a trace just submitted sits. For the sub-hour window this is recorded behavior only: the recording submits one segment document via PutTraceSegments and then queries a window of a few minutes around it, and AWS answers 200 with that one trace present in TraceSummaries on the first and only call (TracesProcessedCount 1, no NextToken).

Emulator: the same request answers 200 with TraceSummaries == [] while TracesProcessedCount == 1 — the trace is counted as processed by the query but is not in the answer — plus a NextToken naming the earlier grid slice. The fixture's poll re-issued the identical query until it was capped at 180 s and the trace never appeared, so for a client that does not paginate a window it had no reason to believe was paginated, the trace is permanently invisible. The cut is the absolute hour grid: services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs documents that "each page is one hour-long slice ... the first page is the slice the window ends in" and that "the slices are cut on the hour GRID rather than from the requested bounds", with TRACE_PAGE_SECONDS = 60*60 and WindowPage.earlier set whenever the requested window reaches into an earlier slice "whether or not the slice just served held anything"; the GetTraceSummaries handler selects candidates only inside that one slice. Whether the caller sees its trace therefore depends on where the clock sits relative to the grid, not on the window it asked about: the identical logical query minutes earlier or later returns the trace.

Why: querying a short, recent window is the primary way a client finds a trace it has just submitted — every post-submit poll, every "last 5/10 minutes" view, and every SDK paginator loop that terminates on an absent NextToken. The client is handed a 200 with no results for a window that demonstrably holds its trace and cannot distinguish "not ingested yet" from "in an earlier slice of the window I asked about"; worse, TracesProcessedCount tells it one trace WAS processed, so the response is internally contradictory. Any read-after-write workflow built on GetTraceSummaries (find the trace, then BatchGetTraces it) stalls, and the behavior is not reproducible because it is a function of the wall clock rather than of the request.

Fix: the page derivation and candidate selection of this read — the window/page boundary derivation in services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs (TRACE_PAGE_SECONDS, the hour-grid boundary walk, WindowPage.start/earlier) together with the candidate walk in the GetTraceSummaries handler, services/service-com-amazonaws-xray-awsxray/src/ops/get_trace_summaries.rs, which admits only traces whose instant falls inside the single served slice. Derive the pages from the REQUESTED window and from the number of summaries selected (walking back from EndTime in page-long steps), so a window shorter than one page is always a single page carrying every matching trace in [StartTime, EndTime), while a genuinely multi-page result set keeps being paged with a resumable token. Nothing about the TraceSummary's CONTENT (open issue 20261109T114723Z) needs to change for this cause.

Traces: xray-put-segment-is-found-by-trace-summaries-and-batch-get#task#2.

## Evidence
Replay of trace set simparity-wire-aws-v1: 21 fixtures replayed, 126 steps, 105 passed, 21 mismatched, 0 errored, 0 blocked, score 83.3%, emulator_up true ("/__admin/health answered 200 and the :33327 data listener accepts"), bootstrap_signal false. 9 divergence clusters, all service=xray; 8 carry a non-empty owned_by and are skipped. This is the only cluster with owned_by == [] and stub_path == "".

Cluster divergence_key: service=xray|GetTraceSummaries|status=200->200|keys=$,$.ApproximateTime,$.NextToken,$.TraceSummaries,$.TraceSummaries[0]
verdict: mismatch; expected_status 200, actual_status 200; affected_count 1 (xray-put-segment-is-found-by-trace-summaries-and-batch-get#task#2).

The member this issue reports, verbatim from the cluster detail:
  $: poll capped at 180s
  $.TraceSummaries[0]: missing (expected the summary of the trace submitted in the preceding PutTraceSegments step — Id '1-8ab6221d-e1920944eabe3835e18843b8', EntryPoint/ServiceIds naming the submitted service, Duration 2.0, ResponseTime 2.0, Http {HttpMethod GET, HttpStatus 200, HttpURL as submitted}, Annotations {order_id -> 'o-1001'}; got None)
  actual body: {'TraceSummaries': [], 'ApproximateTime': 1794916800, 'TracesProcessedCount': 1, 'NextToken': '1794913200'}

Expected: a 200 whose TraceSummaries holds the one trace the window covers, with TracesProcessedCount 1 and no further page. Actual: a 200 whose TraceSummaries is EMPTY while TracesProcessedCount is 1, repeated until the step's poll was capped at 180 s — the trace is never served by this query.

That the cut is the absolute hour grid is visible in the same body: the served page's reported instant 1794916800 is exactly divisible by 3600 (the top of an hour), the deferred token 1794913200 is the preceding hour boundary (also divisible by 3600), and the submitted segment's timing seen in the sibling BatchGetTraces step (task#3) is start_time 1794916490 / end_time 1794916492 — i.e. inside [1794913200, 1794916800), the slice the emulator deferred rather than the slice it served. These epoch values are per-run clock values and are NOT requirements; the requirement is that a trace inside the requested [StartTime, EndTime) is in the answer to that window.

Already-filed members in the same detail block, NOT part of this issue (owned by 20261117T090620Z-xray-gettracesummaries-pages-a-sub-hour-window-on.md, which carries this exact fixture step): $.NextToken (type_mismatch, expected None, got '1794913200') and $.ApproximateTime (value_mismatch, expected 1794915900, got 1794916800).

Source in the checkout (citations, not requirements): services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs lines 3-30 (module doc: "each page is one hour-long slice", "the first page is the slice the window ends in", "the slices are cut on the hour GRID rather than from the requested bounds", a token due "whether or not the slice just served held anything"), line 38 (TRACE_PAGE_SECONDS = 60*60), lines 52-68 (WindowPage.boundary/start/end/earlier); services/service-com-amazonaws-xray-awsxray/src/ops/get_trace_summaries.rs (the candidate walk admitting only instants inside the served page, and the next_token composition). Smithy: GetTraceSummaries POST /TraceSummaries, @paginated{inputToken=NextToken,outputToken=NextToken,items=TraceSummaries}; GetTraceSummariesResult$TraceSummaries "traces that were found in the specified time frame", $TracesProcessedCount "the total number of traces processed", $NextToken "...more than one page of results...the first page contains the most recent results, closest to the end of the time frame"; errors InvalidRequestException (client, 400), ThrottledException (429).

## Steps to Reproduce
1. git_clone, then replay_wire_corpus (it builds and boots the emulator from the checkout); confirm bootstrap_signal is false and emulator_up is true.
2. Observe the divergence cluster service=xray|GetTraceSummaries|status=200->200 whose keys include $, $.TraceSummaries and $.TraceSummaries[0], representative xray-put-segment-is-found-by-trace-summaries-and-batch-get#task#2, with owned_by empty: the step's poll is capped at 180 s with TraceSummaries never non-empty.
3. Reproduce directly against the running emulator, with a window shorter than an hour that crosses the top of an hour (run a few minutes after HH:00:00 UTC, or choose StartTime/EndTime by hand around any hour boundary):
   a. xray.put_trace_segments with ONE segment document {"name": "svc-probe", "id": <16 hex>, "trace_id": "1-<8 hex>-<24 hex>", "start_time": T, "end_time": T+2, "annotations": {"order_id": "<unique>"}} where T is a couple of minutes BEFORE the most recent hour boundary; assert UnprocessedTraceSegments == [].
   b. POST /TraceSummaries with {"StartTime": <T minus 60s>, "EndTime": <now, i.e. after the hour boundary>} and no NextToken.
   c. Observe HTTP 200 with TraceSummaries == [] while TracesProcessedCount counts the trace, and a NextToken present. Expected: the one matching trace present on this first (and only) page.
   d. Re-issue the identical request repeatedly, as a polling client would: the trace never appears. Then feed the NextToken back and observe the trace on the deferred page — confirming the answer to a minutes-wide window was split on the absolute hour boundary.
   e. Repeat (b) with a window that does NOT cross an hour boundary and observe the trace returned on the first page, confirming the grid is what decides it.
4. After the fix, re-run 3(b): the trace must be on the first page of a sub-hour window, with TracesProcessedCount consistent with what is returned. Confirm a genuinely multi-page result set still pages (a token that resumes correctly, each page reporting an instant inside the requested window).
5. Re-replay xray-put-segment-is-found-by-trace-summaries-and-batch-get#task#2: the poll must satisfy promptly and $.TraceSummaries/$.TraceSummaries[0] must no longer be reported. ($.NextToken and $.ApproximateTime on the same step belong to open issue 20261117T090620Z, and the $.TraceSummaries[0].* member-content keys to 20261109T114723Z; those can still be reported until those issues are fixed.)
