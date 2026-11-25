Issue-Source: trace-sweep-divergence
Issue-Trace: neptunedata-query-languages-live-cluster-round-trip
Issue-Step: task#21
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: neptunedata-query-languages-live-cluster-round-trip
Issue-Shape-Id: com.amazonaws.neptunedata#AmazonNeptuneDataplane
Issue-Key: neptunedata|ExecuteGremlinExplainQuery|2xx->2xx|$
Issue-Key: neptunedata|ExecuteGremlinExplainQuery|2xx->2xx|$headers.content-type
Issue-Commit: 21766dad08fc860bc8c3823b74ce465dff8b4307

# neptunedata ExecuteGremlinExplainQuery explain header block diverges: no blank line before Engine Version, explain time in millisecond-Z form, and the text payload's media type

> _Note: this issue was identified against the code base at commit `21766dad08fc860bc8c3823b74ce465dff8b4307`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteGremlinExplainQuery

## Description
Three net-new causes in the neptunedata Gremlin explain report, all against a running, answering emulator (the step was served HTTP 200 — this is not an unreachability or a 501 scaffold path), and all in the SAME replay cluster: fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#21`, divergence key `service=neptunedata|ExecuteGremlinExplainQuery|status=200->200|keys=$,$headers.content-type`.

These are RESIDUAL causes left after the earlier, now-RESOLVED issue `20261113T062625Z-neptunedata-executegremlinexplainquery-explain-report-diverges-from-neptune-s.md` (that one asked for the centred banner, the Explain Time / Engine Version header lines, the Converted Traversal section with its not-converted trailer, Neptune's step/pattern syntax, the count pushdown and the Predicates trailer — the replay now shows ALL of those present and matching). The payload diverges from Neptune's for the first time at the Explain Time line and no longer anywhere before it. A resolved issue covers nothing, and the replay reports `owned_by` empty for this cluster, so these are filed net-new. The two other divergences in this fixture are owned elsewhere (`20261120T091858Z-neptunedata-getenginestatus-...` for task#18 and `20261124T202724Z-neptunedata-executegremlinquery-omits-the-properties-map-from-g.md` for task#19) and neither touches the explain report.

Cause 1 of 3 — the blank line separating the Explain Time header from the Engine Version header is not emitted. Fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#21`.
AWS: Smithy (com.amazonaws.neptunedata#ExecuteGremlinExplainQuery) models POST /gremlin/explain -> 200 with input {gremlinQuery (String, jsonName `gremlin`, required)} and output {output targeting ReportAsText, @httpPayload, @mediaType "text/plain", blob} — the model fixes only that the payload is a text blob, so the report's internal layout is recorded behavior only. The recording shows Neptune separating the two per-explain header lines with an EMPTY line: the `Original Traversal` section is followed by `Explain Time: <instant>`, then an empty line, then `Engine Version: <engine version>`, then an empty line, then the `Converted Traversal` heading. The fixed text is `Explain Time: `, `Engine Version: ` and the blank lines between and after them; the instant and the version are values taken from the call and from the serving cluster's engine status.
Emulator: writes the two header lines adjacently — `Explain Time: <instant>` immediately followed on the next line by `Engine Version: <version>` with no empty line between them (the empty line AFTER Engine Version is emitted correctly, since the actual payload continues `...R1\n\nConverted Traversal`). This is the first byte position at which the 200 payload diverges from the recorded one.
Why: the report is the whole product of this operation and its consumers (the Neptune workbench `%%gremlin explain` magic, query-tuning scripts, CI assertions on a plan) split it into blocks on blank lines and section rules. A client that reads the header block as a blank-line-delimited paragraph gets `Explain Time` and `Engine Version` fused into one block against the emulator, so a parser that keys the header block by line count, or that reads the engine version as the paragraph following the explain time, mis-reads or drops the version the plan was produced by.
Fix: the explain-report renderer — `crate::state::gremlin_explain_report` in the neptunedata state module (the header-line block it writes between the `Original Traversal` section and the `Converted Traversal` heading), which must terminate the `Explain Time:` line with the blank separator Neptune writes. The handler `src/ops/execute_gremlin_explain_query.rs` (lines 264-273) already hands the renderer the explain instant and the engine version and needs no change for this.
Traces: `neptunedata-query-languages-live-cluster-round-trip#task#21`.

Cause 2 of 3 — the explain instant is formatted as a millisecond UTC instant with a `Z` designator instead of the zone-less nanosecond-precision local datetime Neptune writes. Fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#21`.
AWS: recorded behavior only (as above, the model fixes only that the payload is a `text/plain` blob). The recording shows the explain time written as an ISO-8601 date-time with NO zone designator and NANOSECOND fractional precision — `<date>T<time>.<9 fractional digits>`, i.e. the form `java.time.LocalDateTime.toString()` produces, which is what the engine renders this header with. The fixed text is the `Explain Time: ` label and that layout (date `T` time, dot, nine fractional digits, no trailing `Z` and no offset); the instant itself is a per-run value taken from the moment the explain ran and is NOT part of the requirement.
Emulator: writes the instant as `<date>T<time>.<3 fractional digits>Z` — millisecond precision with a `Z` zone designator. Only the FORM is reported here: the two recordings' instants differ because they were produced in different runs, and that difference is not the defect.
Why: a client that parses this header to correlate an explain with a query run (the workbench does, and tuning scripts that diff successive plans do) parses it with the engine's own zone-less local-date-time format. Against the emulator the trailing `Z` makes that parse fail outright, and a lenient parser silently reads a different precision than the engine ever emits, so an explain cannot be timestamp-matched to the run that produced it.
Fix: the same renderer, `crate::state::gremlin_explain_report` — the formatting of `ExplainContext.explained_at` (set from the call's own instant at `src/ops/execute_gremlin_explain_query.rs:269`) must render a zone-less nanosecond-precision local date-time rather than an RFC-3339 millisecond UTC instant. This is a format change in the renderer only; the instant's SOURCE is already correct.
Traces: `neptunedata-query-languages-live-cluster-round-trip#task#21`.

Cause 3 of 3 — the response's `content-type` for this text payload diverges. Fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#21`.
AWS: Smithy (com.amazonaws.neptunedata#ExecuteGremlinExplainQuery) binds the single output member `output` as `@httpPayload` targeting `ReportAsText` with `@mediaType "text/plain"`, so this 200 response must announce the text media type Neptune announces for the explain report, not the service's default JSON one. The recorded response announces a text content type for the report payload.
Emulator: the replay cluster's divergence key for this step names `$headers.content-type` alongside `$` (`keys=$,$headers.content-type`), i.e. the response header diverges from the recorded one as well as the body. The replay `detail` for this cluster was consumed by the whole-payload `$` diff and so does not quote the two header values; the header is reported as diverging by the cluster key only. Hypothesis: the explain response is serialized through the service's default response path, which announces the service-wide JSON media type (or omits the charset parameter the engine sends) rather than honouring the operation's `@mediaType text/plain` payload binding — the same media-type selection that was reported for the other neptunedata data-plane responses.
Why: a client that dispatches on content-type (an SDK selecting a blob vs a document deserializer, a notebook deciding whether to render the report as text) is told this 200 body is JSON, so it either fails to deserialize the explain report or renders it as an opaque value instead of the text report it is.
Fix: the response serializer for this operation — the media type announced for the `@httpPayload`/`@mediaType text/plain` member of `ExecuteGremlinExplainQueryOutput`, in the neptunedata response-serialization path (the handler at `src/ops/execute_gremlin_explain_query.rs:287-289` returns the report bytes and does not set the header itself).
Traces: `neptunedata-query-languages-live-cluster-round-trip#task#21`.

## Evidence
Replay of trace set `simparity-wire-aws-v1`: fixture `neptunedata-query-languages-live-cluster-round-trip`, step `task#21`, service neptunedata, operation ExecuteGremlinExplainQuery, verdict mismatch, expected_status 200, actual_status 200. Divergence key: `service=neptunedata|ExecuteGremlinExplainQuery|status=200->200|keys=$,$headers.content-type`. `stub_path` empty, `owned_by` empty, affected_count 1.

Replay detail: `$: value_mismatch`. The payload matches the recorded report byte for byte through the banner, the `Query String` section and the `Original Traversal` section, and first diverges at the header block:

Recorded (AWS), with the per-run values shown as placeholders:
  ...
  Original Traversal
  ==================
  [GraphStep(vertex,[]), HasStep([~label.eq(<label>)]), CountGlobalStep]
  <blank>
  Explain Time: <date>T<time>.<9 fractional digits>        <- no zone designator
  <blank>                                                   <- BLANK LINE
  Engine Version: <engine version>
  <blank>
  Converted Traversal
  ...
(the recording's own instance read `Explain Time: 2026-11-05T22:21:39.301188654` and `Engine Version: 1.4.8.0.R1`; the instant and the version are that run's values, the labels, the layout and the blank lines are the requirement)

Actual (emulator), same placeholders:
  ...
  Original Traversal
  ==================
  [GraphStep(vertex,[]), HasStep([~label.eq(<label>)]), CountGlobalStep]
  <blank>
  Explain Time: <date>T<time>.<3 fractional digits>Z        <- millisecond precision + Z
  Engine Version: <engine version>                          <- NO blank line before this
  <blank>
  Converted Traversal
  ...
(the emulator's instance read `Explain Time: 2026-11-25T13:07:18.777Z`; only the FORM of that value is the finding — the instant itself is per-run and is not claimed as a defect)

The replay's `actual` string is truncated by the harness inside the `Converted Traversal` section, so nothing is asserted here about the report beyond the header block: everything up to `Explain Time` is confirmed matching, the header block is confirmed diverging, and the sections after it are not reported on.

Header: the cluster's diverging-key list is `$,$headers.content-type`, so the response's content-type header diverges from the recorded one in addition to the body. The model binds this output as `@httpPayload` + `@mediaType "text/plain"`.

Status 200 on both sides; no error code or error message is involved in any of the three causes.

Code: `crate::state::gremlin_explain_report` renders the report; the handler `services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/ops/execute_gremlin_explain_query.rs` lines 256-273 gather the cluster's engine version and the predicate count and pass the explain instant as `ExplainContext.explained_at` (line 269, `SystemTime::now()`), and lines 287-289 return the rendered report as the `output` blob. The plan printer feeding the report lives in `services/service-com-amazonaws-neptunedata-amazonneptunedataplane/src/state/gremlin_plan.rs` (report banner constant `REPORT_RULE` at line 106).

## Steps to Reproduce
1. Boot the neptunedata emulator from this checkout and address the data plane at the cluster endpoint the Neptune control plane publishes.
2. POST /gremlin/explain (neptunedata ExecuteGremlinExplainQuery) with `gremlin` set to a traversal ending in a terminal count, e.g. `g.V().hasLabel('<some-label>').count()`.
3. Read the 200 `text/plain` payload and look at the two header lines between the `Original Traversal` section and the `Converted Traversal` heading:
   - there is NO empty line between `Explain Time: ...` and `Engine Version: ...` (AWS writes one);
   - the `Explain Time:` value is written with three fractional digits and a trailing `Z` (AWS writes nine fractional digits and no zone designator).
4. Inspect the response's `content-type` header for this payload and compare it against the recorded one; the operation's output is modelled `@httpPayload` with `@mediaType text/plain`.
5. Equivalently: replay fixture `neptunedata-query-languages-live-cluster-round-trip` (replay_wire_corpus) and observe step `task#21` mismatch with key `$,$headers.content-type`.
6. Re-replay the fixture and confirm `task#21` passes.
