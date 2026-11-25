Issue-Source: trace-sweep-divergence
Issue-Trace: geo-routes-optimize-waypoints-basic-success
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-routes-optimize-waypoints-basic-success geo-routes-optimize-waypoints-duplicate-ids-rejected geo-routes-optimize-waypoints-origin-only-rejected
Issue-Shape-Id: com.amazonaws.georoutes#RoutesService
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Connections[*].Distance
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Connections[*].From
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Connections[*].To
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Connections[*].TravelDuration
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Distance
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Duration
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.TimeBreakdown.TravelDuration
Issue-Commit: 789c7a890d26c428a053c99e86a00990d57bc346

# OptimizeWaypoints returns the requested waypoint order rather than an optimized sequence, with synthesized distances and durations

> _Note: this issue was identified against the code base at commit `789c7a890d26c428a053c99e86a00990d57bc346`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
OptimizeWaypoints

## Description
geo-routes answers OptimizeWaypoints with HTTP 200 and a structurally complete body, but it does not optimize: the Connections it returns traverse the waypoints in the order they were submitted, and the distances and durations attached to them (and to the response totals and TimeBreakdown) are locally synthesized estimates rather than road-network values. This bundle carries TWO causes.

=== Cause 1 of 2: the returned sequence is the request order, not the optimized order, and Connections/total distance and duration are synthesized ===
AWS: Smithy com.amazonaws.georoutes#OptimizeWaypoints (POST /v2/optimize-waypoints, success 200; modeled errors ValidationException 400, AccessDeniedException 403, ThrottlingException 429, InternalServerException 500 - none in play, both sides answer 200, so no status, error code or error message diverges). The operation's contract is to reorder the submitted Waypoints between the @required Origin and the optional Destination so as to minimize the objective given by OptimizeSequencingFor, and to report that sequence in @required OptimizedWaypoints (WaypointOptimizationOptimizedWaypoint: Id, Position, DepartureTime, ArrivalTime) and @required Connections. Each WaypointOptimizationConnection is documented "information such as distance and duration from one waypoint to the next waypoint in the sequence", with @required From and To (WaypointId, @length 1..100, @pattern ^[^,;!]+$) naming the consecutive waypoints and @required Distance (DistanceMeters, @range 0..4294967295) and TravelDuration (DurationSeconds, same range) measured along the road network. The response's @required Distance and Duration are the totals over that sequence, and @required TimeBreakdown.TravelDuration is "the traveling phase of the cycle" - the sum of the connections' travel time. Because the sequence is chosen to minimize the objective, the reported total must be no worse than the request-order total.
Emulator: the Connections chain visits the waypoints in the order they appear in the request instead of the optimized order - Connections[0].To is the first submitted waypoint where the recording hands back the other one, and Connections[1].From/.To are correspondingly swapped (expected To 'pioneer-square' then From 'pioneer-square'/To 'space-needle'; got To 'space-needle' then From 'space-needle'/To 'pioneer-square'). The per-connection and total metrics are also synthesized rather than measured: every Distance and TravelDuration differs from the recording and the totals come out strictly WORSE than AWS's optimized result (total Distance expected 4775, got 5472; total Duration expected 820, got 1082; TimeBreakdown.TravelDuration follows Duration at 1082 against 820), which is itself proof that no minimization ran - an optimizer cannot return a sequence costlier than the one AWS found over the same waypoints. RestDuration, WaitDuration and ServiceDuration replay clean. Hypothesis: the handler emits the request order and derives each hop's distance/duration from the straight-line separation of the two positions scaled by a nominal speed; the model states only the member semantics and the recording only shows resulting values, so the exact formula is recorded behavior only.
Why: OptimizeWaypoints exists solely to answer "in what order should I visit these stops?". A client - a dispatcher building a driver manifest, a delivery app sequencing drops - is handed back its own input order presented as the optimized answer, with totals that understate nothing and overstate the real cost, so routes are longer than necessary and every ETA derived from DepartureTime/ArrivalTime or TimeBreakdown is wrong. Because the response is a 200 with a fully populated, correctly shaped body, there is no signal that no optimization happened; the client cannot distinguish "your order was already optimal" from "I did not optimize".
Fix: the OptimizeWaypoints handler in the geo-routes service crate - actually sequence the waypoints for the requested OptimizeSequencingFor objective (honouring Origin/Destination options, Clustering, Traffic and Driver inputs) and derive each Connection's Distance/TravelDuration, the response totals and TimeBreakdown.TravelDuration from road-network traversal of that sequence rather than from straight-line geometry over the request order.
Traces: geo-routes-optimize-waypoints-basic-success#task#1 (representative), geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2 (affected_count 2).

=== Cause 2 of 2: with a single hop (no reordering possible) the distances and durations are still synthesized estimates ===
AWS: same OptimizeWaypoints contract as Cause 1. Connections[*].Distance and TravelDuration, and the response's @required Distance/Duration and TimeBreakdown.TravelDuration, are road-network measurements for the hop(s) in the sequence. This fixture first exercises a rejected request (origin only) and then a valid one; the rejection step replays clean, so no status, error code or error message diverges in this cluster - the valid step returns 200 on both sides and only body values differ.
Emulator: with only one connection there is no ordering to get wrong, and the ordering fields match - yet every metric is still off by a consistent ~8% high (Connections[0].Distance expected 1227, got 1327; Connections[0].TravelDuration expected 241, got 264; response Distance 1227 -> 1327; Duration 241 -> 264; TimeBreakdown.TravelDuration 241 -> 264). This isolates the metric synthesis from the sequencing defect: even on a single hop the emulator reports a geometric estimate instead of a routed distance and time.
Why: a client that sizes a delivery window, a fuel budget or an SLA from Distance/Duration is working from a number that is not a road distance at all, and the error is systematic rather than noise, so it compounds over a multi-stop day. As with Cause 1 the 200 response gives no indication the figures are estimates.
Fix: same component as Cause 1 (the OptimizeWaypoints handler's distance/duration computation); this cause exists so the fixer re-replays the single-hop/validation fixture too, where the sequencing half of the fault is invisible.
Traces: geo-routes-optimize-waypoints-origin-only-rejected#task#2 (affected_count 1).

Related issues, none of which covers these failures: the OPEN issue 20261122T201204Z-geo-routes-calculateroutematrix-distances-and-durations-are-synthesized-geometric-estimates.md reports the same synthesized-metric class for CalculateRouteMatrix only and owns only that cluster (replay reports owned_by empty for both causes above); 20261124T112417Z-geo-routes-calculateisolines-snappedorigin-is-not-the-road-snapped-position.md concerns CalculateIsolines' SnappedOrigin. Neither covers OptimizeWaypoints, and neither covers the sequencing defect in Cause 1.

## Evidence
Replay of corpus simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, bootstrap_signal false, 19 steps, 10 mismatched). Both causes are verdict "mismatch" with expected_status 200 and actual_status 200 - the divergence is entirely in body values, and no modeled error (ValidationException / AccessDeniedException / ThrottlingException / InternalServerException) is exercised.

Cause 1 - fixture geo-routes-optimize-waypoints-basic-success, step task#1, operation OptimizeWaypoints:
  $.Connections[0].To: expected 'pioneer-square', got 'space-needle'
  $.Connections[1].From: expected 'pioneer-square', got 'space-needle'
  $.Connections[1].To: expected 'space-needle', got 'pioneer-square'
  $.Connections[0].Distance: expected 1631, got 1754
  $.Connections[0].TravelDuration: expected 312, got 344
  $.Connections[1].Distance: expected 3144, got 3718
  $.Connections[1].TravelDuration: expected 508, got 738
  $.Distance: expected 4775, got 5472
  $.Duration: expected 820, got 1082
  $.TimeBreakdown.TravelDuration: expected 820, got 1082
divergence_key: service=geo-routes|OptimizeWaypoints|status=200->200|keys=$.Connections[0].Distance,$.Connections[0].To,...,$.Distance,$.Duration,$.TimeBreakdown.TravelDuration; affected: [geo-routes-optimize-waypoints-basic-success#task#1, geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2]; affected_count 2; stub_path empty; owned_by empty.
The To/From swap shows the chain is Origin -> (first submitted waypoint) -> (second submitted waypoint) while AWS returns Origin -> the other waypoint first. The emulator's total (5472 m / 1082 s) is strictly worse than AWS's (4775 m / 820 s) over the same waypoint set, which no minimization could produce.

Cause 2 - fixture geo-routes-optimize-waypoints-origin-only-rejected, step task#2, operation OptimizeWaypoints:
  $.Connections[0].Distance: expected 1227, got 1327
  $.Connections[0].TravelDuration: expected 241, got 264
  $.Distance: expected 1227, got 1327
  $.Duration: expected 241, got 264
  $.TimeBreakdown.TravelDuration: expected 241, got 264
affected_count 1; stub_path empty; owned_by empty. Connections[0].From/.To, RestDuration, WaitDuration and ServiceDuration replay clean here, isolating the metric synthesis from the sequencing defect.

The specific metres, seconds and waypoint ids above are those recordings' values and are examples, not literal requirements. The requirements are the rules: Connections must chain the waypoints in the optimized sequence (From of connection n+1 equal to To of connection n, starting at Origin and ending at Destination when one is supplied) for the requested OptimizeSequencingFor objective; Distance/TravelDuration per connection, the response totals and TimeBreakdown.TravelDuration must be road-network values for that sequence; and the reported total must never exceed the cost of the request order, since the request order is one of the candidate sequences.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus()).
3. Cause 1: replay fixture geo-routes-optimize-waypoints-basic-success, step task#1 - POST /v2/optimize-waypoints (OptimizeWaypoints) with the recorded Origin plus two Waypoints whose optimal visiting order is the reverse of the submitted order. Observe HTTP 200, then walk $.Connections: the chain visits the waypoints in submitted order (Connections[0].To is the first submitted waypoint, got 'space-needle', where the recording has 'pioneer-square'), and $.Distance/$.Duration/$.TimeBreakdown.TravelDuration are larger than the recorded optimized totals (5472/1082 vs 4775/820).
4. Re-order the two Waypoints in the request and replay: the returned sequence follows the request order again, confirming no optimization is performed. Repeat the step with geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2.
5. Cause 2: replay fixture geo-routes-optimize-waypoints-origin-only-rejected - step task#1 (origin-only request) is rejected and replays clean; step task#2 returns 200 with a single connection whose Distance/TravelDuration, and the response totals and TimeBreakdown.TravelDuration, are ~8% above the recorded road-network values (1327 m / 264 s vs 1227 m / 241 s).
6. The fix is confirmed when the returned OptimizedWaypoints/Connections sequence minimizes the requested objective (and is never costlier than the request order), per-connection and total Distance/Duration plus TimeBreakdown.TravelDuration come from road-network traversal, and both fixture steps replay clean.
