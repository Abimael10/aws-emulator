Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: xray.telemetry-only-host-absent-from-service-graph
Issue-Shape-Id: com.amazonaws.xray#AWSXRay
Issue-Key: xray.telemetry-only-host-absent-from-service-graph|run#16|Timeout|time.sleep(2)
Issue-Commit: b7722f564811d4a38b310da059d7f257f38b41af

# xray GetServiceGraph: a service whose segment PutTraceSegments just accepted never becomes readable as a graph node within the replay window, so the telemetry-vs-trace pipeline contrast is unobservable

> _Note: this issue was identified against the code base at commit `b7722f564811d4a38b310da059d7f257f38b41af`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetServiceGraph

## Description
ONE net-new root cause in the xray service (the only failing xray check in this run that the replay report marks neither ALREADY FILED nor OWNED; xray is bootstrapped and answering, 152/160 xray checks pass, so this is a fidelity defect and not a bootstrap gap).

RELATED, NOT COVERING: open issue 20261025T043615Z-xray-3-net-new-behavioral-root-causes-getservicegraph.md lists this same check id in its Issue-Trace-Ids, but its replay key for it is `xray.telemetry-only-host-absent-from-service-graph|run#16|Timeout|time.sleep(20)` — a key the current check body no longer produces (the check now waits in `_poll_until(...)`/`time.sleep(interval_s)` plus a 2 s grace loop), which is why the replay report did NOT mark this failure OWNED by it. Also related and not covering: 20261022T142349Z-xray-x-ray-group-and-trace-segment-destination-responses.md (the trace-segment-destination singleton, used here only by the pinning helper) and 20261123T064210Z-xray-batchgettraces-re-serializes-the-stored-segment-document--r2.md (BatchGetTraces serialization, a different read).

Cause 1 of 1: the service-graph read does not make a just-ingested service observable, so a GetServiceGraph reader cannot see the node for a segment PutTraceSegments accepted (and therefore cannot tell it apart from a telemetry-only host, which must never be a node).

AWS: Smithy (services/service-com-amazonaws-xray-awsxray/smithy-model/com-amazonaws-xray-awsxray.json) models GetServiceGraph as POST /ServiceGraph, success 200, required StartTime/EndTime, output `Services` -> ServiceList of Service{Name, Names, Type, State, Edges, SummaryStatistics, StartTime, EndTime} plus ContainsOldGroupVersions and NextToken; its only modeled errors are InvalidRequestException (400) and ThrottledException (429). PutTelemetryRecords is documented as the X-Ray daemon's telemetry upload and its result shape models NO members; no modeled operation reads telemetry back. Recorded behavior: a root segment submitted through PutTraceSegments makes its document `name` reachable in a GetServiceGraph window containing that segment — as a node's `Name` or as an entry of some node's `Names` — on a human timescale (tens of seconds), while a host that only called PutTelemetryRecords (EC2InstanceId/Hostname/ResourceARN identify the reporter) never appears as a node at all, because the telemetry and trace pipelines are separate.

Emulator: the check's two ingest legs succeed and are asserted BEFORE any waiting — PutTraceSegments answers with `UnprocessedTraceSegments == []` and PutTelemetryRecords answers HTTP 200 — and no 4xx/5xx and no assertion ever fires. What never completes is the read side: the check then pages GetServiceGraph (up to 30 pages, NextToken followed) over [now-600s, now+300s] collecting every `Services[*].Name` and every `Services[*].Names[*]`, waits for the submitted service name to be present (its own budget 900 s), and then re-pages the same window for a further grace period asserting the telemetry-only hostname stays absent. The replay is cut off at the harness ceiling with verdict `mismatch`, Error: `Check timed out after 60.0s`, so neither the presence of the traced node nor the required absence of the telemetry-only host is ever observed. Hypothesis (neither the model nor the recording states it): the window-scoped graph derivation and its pagination are implemented (services/service-com-amazonaws-xray-awsxray/src/ops/get_service_graph.rs derives from stored traces through crate::state::service_graph in src/state/graphs.rs), so what is diverging is either the selection that decides a stored segment takes part in that window-scoped derivation, or the per-call cost of re-deriving the whole map from the stored documents on every page of every poll, rather than the node shape itself.

Why: the service map is X-Ray's primary aggregate read. A client that submits segments and then reads GetServiceGraph to discover topology, latency and fault statistics gets a map that does not contain the service it just reported, so dependency discovery, console-style maps and anything alarming off SummaryStatistics are blind; and because the traced node never shows up, a client cannot distinguish "no traffic" from "the graph does not report this traffic", nor verify the real invariant that a telemetry-only reporter is NOT topology.

Fix: the GetServiceGraph handler and the window-scoped graph derivation and pagination it reads (services/service-com-amazonaws-xray-awsxray/src/ops/get_service_graph.rs, services/service-com-amazonaws-xray-awsxray/src/state/graphs.rs and the trace/segment store the derivation walks) — make a segment accepted by PutTraceSegments observable as a node of a window containing it, on every page of a repeated read, at a cost that does not grow with the number of reads; the telemetry store (PutTelemetryRecords) must keep contributing no node.

Traces: xray.telemetry-only-host-absent-from-service-graph.

## Evidence
Failing check: xray.telemetry-only-host-absent-from-service-graph — suite verdict `mismatch`, Error: `Check timed out after 60.0s` (164/176 suite checks pass; xray 152/160).

What the check establishes before it waits (so these are NOT what diverged): put_trace_segments(TraceSegmentDocuments=[one root segment {id: 16 hex, name: 'sim2real-behav-xray-traced-<hex>', trace_id: '1-<8 hex>-<24 hex>', start_time: now-1.5, end_time: now, http:{request:{method:'GET',url:...},response:{status:200}}}]) returns UnprocessedTraceSegments == [] (asserted), and put_telemetry_records(EC2InstanceId='i-<17 hex>', Hostname='sim2real-behav-xray-telonly-<hex>', ResourceARN='*', TelemetryRecords=[{Timestamp, SegmentsReceivedCount:5, SegmentsSentCount:5, SegmentsSpilloverCount:0, SegmentsRejectedCount:0}]) returns HTTPStatusCode 200 (asserted). The account trace-segment destination is pinned to 'XRay' first, and the emulator's default record is XRay/ACTIVE, so the pin is satisfied without a switch.

Expected (recorded AWS): a fully paginated get_service_graph(StartTime=now-600s, EndTime=now+300s) walk contains 'sim2real-behav-xray-traced-<hex>' as a node Name or as an entry of a node's Names, and never contains 'sim2real-behav-xray-telonly-<hex>'. The hostname and service name above are per-run values filling that template; the requirement is the traced name's presence and the telemetry-only name's absence.

Observed: the check is cut off inside that read loop at the 60 s replay ceiling with no error response and no failed assertion — i.e. GetServiceGraph answered 200 but the walk never reported the submitted service, so the absence half of the contrast was never reached either.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite (it builds and boots the emulator); confirm bootstrap_signal is false and xray answers (152/160 xray checks pass).
2. xray.get_trace_segment_destination() — confirm Destination 'XRay', Status 'ACTIVE' (no switch needed).
3. xray.put_trace_segments(TraceSegmentDocuments=[json.dumps({'id': <16 hex>, 'name': 'svc-probe', 'trace_id': '1-<8 hex>-<24 hex>', 'start_time': now-1.5, 'end_time': now, 'http': {'request': {'method': 'GET', 'url': 'https://example.invalid/traced'}, 'response': {'status': 200}}})]) — observe UnprocessedTraceSegments == [].
4. xray.put_telemetry_records(EC2InstanceId='i-<17 hex>', Hostname='telonly-probe', ResourceARN='*', TelemetryRecords=[{'Timestamp': now, 'SegmentsReceivedCount': 5, 'SegmentsSentCount': 5, 'SegmentsSpilloverCount': 0, 'SegmentsRejectedCount': 0}]) — observe HTTP 200.
5. Page xray.get_service_graph(StartTime=now-600s, EndTime=now+300s), following NextToken, and collect every Services[*].Name and Services[*].Names[*]; repeat the walk a few times. Observe 'svc-probe' is not reported (expected: a node for it, Root true, State 'active'), and time each call to see whether the walk is also slow enough to consume a bounded poll budget.
6. Confirm 'telonly-probe' is absent from the same walk (this must stay true after the fix).
7. Re-replay xray.telemetry-only-host-absent-from-service-graph to confirm the fix.
