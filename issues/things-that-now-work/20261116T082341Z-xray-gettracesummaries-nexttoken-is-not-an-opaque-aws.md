Issue-Source: trace-sweep-divergence
Issue-Trace: xray-alarm-firing-trace-window-and-filter-mistakes
Issue-Step: task#4
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: xray-alarm-firing-trace-window-and-filter-mistakes xray-trace-summaries-hourly-pagination xray-trace-window-unit-mistakes
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray|GetTraceSummaries|2xx->2xx|$.NextToken
Issue-Commit: 32d9f0db2b4658a030b866cd7211dedac491b2d0

# xray GetTraceSummaries NextToken is not an opaque AWS pagination token: it serves the raw internal hour-grid cursor

> _Note: this issue was identified against the code base at commit `32d9f0db2b4658a030b866cd7211dedac491b2d0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTraceSummaries

## Description
ONE net-new wire root cause in the xray service (one divergence cluster, 4 replayed steps across 3 fixtures). xray answers this corpus (101/126 steps pass, emulator_up true, bootstrap_signal false), so this is a fidelity defect, not a bootstrap gap. The replay report marks this cluster with an EMPTY owned_by and an EMPTY stub_path: no open issue carries its replay key.

RELATED, NOT COVERING (named for the fixer; neither carries this fixture step):
- 20261115T140926Z-xray-gettracesummaries-hides-in-window-traces-pages-are.md is a BEHAVIORAL issue about WHICH summaries a page carries (the absolute-hour-grid slicing hiding in-window traces). This issue is about the WIRE FORM of the token the response hands back, which diverges even on the steps whose summaries matched; the two are independent and a fix for either leaves the other standing.
- 20261109T114723Z-xray-segment-derived-trace-reads-lose-data-gettracesummaries.md concerns the CONTENT of a TraceSummary, not the pagination token.

Cause 1 of 1: GetTraceSummaries returns a NextToken that is the emulator's internal paging cursor in clear text — the epoch-second hour boundary of the next slice, optionally suffixed with ':' and the trace id to resume at — instead of an opaque pagination token of the shape AWS issues.

AWS: Smithy (services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json) models GetTraceSummaries as POST /TraceSummaries (success 200) with @paginated {inputToken: NextToken, outputToken: NextToken, items: TraceSummaries}; GetTraceSummariesResult$NextToken targets String with no length, pattern or format constraint, documented "If the requested time frame contained more than one page of results, you can use this token to retrieve the next page". The model therefore states no token FORMAT — for the format this is recorded behavior only: every recording that pages this operation carries a NextToken that the fixture asserts by SHAPE rather than by value (the recorded value is redacted as '<shaped-not-asserted>' precisely because it is an opaque, run-specific blob), and the emulator's value fails that shape assertion. AWS issues an opaque cursor; it does not publish a decimal clock value as the token.

Emulator: the token is composed by crate::state::page_token as either the next slice's hour boundary rendered with i64::to_string(), or "<boundary>:<trace-id>" when the current slice still holds summaries — e.g. the bare 10-digit decimal '1794812400'. The response status (200) and every other asserted member of the response matched; the sole divergence is $.NextToken, reported as value_mismatch against the asserted token shape on all four replayed steps.

Why: a client is told by the model and the docs that NextToken is opaque, and the emulator instead publishes a parseable internal cursor. Three concrete consequences: (1) anything that validates or canonicalises the token against the shape AWS emits (SDK/CLI pagination test doubles, recorded-response contract checks, proxies that store or hash a cursor) rejects or mangles the emulator's token, which is exactly what this replay observed; (2) the token leaks the service's internal paging scheme — the hour grid and the next trace id — so a client can read a real wall-clock instant and a trace id out of a value AWS treats as meaningless, and code written against the emulator can come to depend on decoding it and then break on AWS; (3) the token's text is a function of the clock rather than of the result set, so two emulator runs of the same logical query yield tokens whose form AWS would never produce, defeating any recorded-trace comparison of the paged read.

Fix: the token composition and parsing for this read — crate::state::page_token / crate::state::parse_page_token in the trace-paging state module, and the next_token assembly in the GetTraceSummaries handler that calls them. Emit an opaque, self-contained cursor (an encoded form of the same boundary/resume pair) so the value carries no directly readable clock or trace id and matches the shape AWS hands out, and keep the resume path round-tripping the emulator's own tokens: a token this read never issued must still be refused with the existing InvalidRequestException wording, so the encoding and the decoding stay the single paired concern that file already documents. Nothing about which summaries a page carries needs to change for this cause.

Traces: xray-alarm-firing-trace-window-and-filter-mistakes#task#4 (representative), xray-trace-summaries-hourly-pagination#task#1, xray-trace-summaries-hourly-pagination#task#2, xray-trace-window-unit-mistakes#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1: 21 fixtures replayed, 126 steps, 101 passed, 25 mismatched, 0 errored, 0 blocked, score 80.2%, emulator_up true ("/__admin/health answered 200 and the :44041 data listener accepts"), bootstrap_signal false. 13 divergence clusters, all service=xray; 12 carry a non-empty owned_by and are skipped. This is the only cluster with owned_by == [] and stub_path == "".

Cluster divergence_key: service=xray|GetTraceSummaries|status=200->200|keys=$.NextToken
verdict: mismatch; expected_status 200, actual_status 200.
detail: "$.NextToken: value_mismatch (expected '<shaped-not-asserted>', got '1794812400')"
affected_count 4: xray-alarm-firing-trace-window-and-filter-mistakes#task#4, xray-trace-summaries-hourly-pagination#task#1, xray-trace-summaries-hourly-pagination#task#2, xray-trace-window-unit-mistakes#task#5.

Expected: a GetTraceSummaries 200 whose NextToken is an opaque pagination token of the shape AWS emits (the recording asserts the token's shape, not its value, hence '<shaped-not-asserted>').
Actual: the bare decimal '1794812400' — the epoch-second hour boundary of the next slice, i.e. the emulator's internal cursor rendered verbatim. The second token form the same code path produces is "<boundary>:<trace-id>" when a slice carries more summaries than one page.

Source in the checkout (citations, not requirements): services/service-com-amazonaws-xray-awsxray/src/state/trace_pages.rs lines 40-44 (TOKEN_SEPARATOR ':'), 147-155 (page_token: format!("{boundary}:{trace}") or boundary.to_string()), 167-179 (parse_page_token: splits on ':' and parses the boundary as i64, requiring boundary % 3600 == 0); services/service-com-amazonaws-xray-awsxray/src/ops/get_trace_summaries.rs lines 536-541 (next_token composed from page_token) and 556 (returned as the output's next_token), with TRACE_SUMMARIES_PAGE_SIZE = 5 at line 570. Smithy: GetTraceSummaries @paginated {inputToken: NextToken, outputToken: NextToken, items: TraceSummaries}; GetTraceSummariesResult$NextToken: String, unconstrained; errors InvalidRequestException (client, 400) and ThrottledException (429).

## Steps to Reproduce
1. git_clone, then replay_wire_corpus (it builds and boots the emulator from the checkout); confirm bootstrap_signal is false and emulator_up is true.
2. Observe the divergence cluster service=xray|GetTraceSummaries|status=200->200|keys=$.NextToken, representative xray-alarm-firing-trace-window-and-filter-mistakes#task#4, with owned_by empty.
3. Reproduce directly against the running emulator, using a window wide enough to be paged (more than one hour, or more than 5 matching traces in one hour):
   a. POST /TraceSummaries with {"StartTime": <epoch now-3*3600>, "EndTime": <epoch now>} and no NextToken.
   b. Observe HTTP 200 whose NextToken is a bare decimal epoch-second value on the hour grid (e.g. ending in 00 and divisible by 3600), or "<that value>:<16-hex trace id>" when the served slice still holds summaries. Expected: an opaque token that does not expose an epoch instant or a trace id.
   c. Feed that token back as NextToken and confirm the next page is served, so the token is a working cursor whose only defect is its wire form.
4. After the fix, re-run 3(a)-(c): the token must be opaque (no directly readable clock value or trace id), must still resume the same page when fed back, and a token this read never issued must still be refused with the existing InvalidRequestException wording.
5. Re-replay the fixture steps in Traces/trace_ids — xray-alarm-firing-trace-window-and-filter-mistakes#task#4, xray-trace-summaries-hourly-pagination#task#1 and #task#2, xray-trace-window-unit-mistakes#task#5 — all four must pass with $.NextToken no longer reported.
