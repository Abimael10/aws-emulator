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
Issue-Commit: 36a2cbc5d8303556afeb5458aed728a902f118f6

# OptimizeWaypoints keeps the request waypoint order and synthesizes straight-line metrics instead of an optimized sequence with road-network distances

> _Note: this issue was identified against the code base at commit `36a2cbc5d8303556afeb5458aed728a902f118f6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
OptimizeWaypoints

## Description
This bundle carries 2 net-new causes in the geo-routes OptimizeWaypoints result computation. Both steps answer HTTP 200 with a well-formed, fully-populated body, so the divergence is in the VALUES, not the shape or the status. Related open issues record a DIFFERENT class of fault and do not cover either cause below, and neither cluster is owned by them: 20261109T081037Z-geo-routes-optimizewaypoints-validationexception-message-quotes-the-member-names-diverging.md and 20261110T072828Z-geo-routes-optimizewaypoints-validation-message-quotes-field-names.md are about OptimizeWaypoints ValidationException wording, and 20261114T131205Z-geo-routes-calculateroutematrix-per-cell-distance-and-duration-still-diverge.md is the same class of metric fault on a different operation (CalculateRouteMatrix).

=== Cause 1 of 2: the returned Connections sequence is the request order, not an optimized order, and its per-leg and total metrics are wrong ===
AWS: Smithy: OptimizeWaypoints (POST /v2/optimize-waypoints) takes Origin (Position, required), Waypoints (WaypointOptimizationWaypointList) and OptimizeSequencingFor (enum WaypointOptimizationSequencingObjective); it returns OptimizeWaypointsResponse.Connections (WaypointOptimizationConnectionList, required) and OptimizedWaypoints (WaypointOptimizationOptimizedWaypointList, required). WaypointOptimizationConnection documents itself as "information such as distance and duration from one waypoint to the next waypoint in the sequence", with From (WaypointId, required, @length 1..100, @pattern ^[^,;!]+$) "the ID of the starting waypoint in this connection" and To (WaypointId, required) "the ID of the ending waypoint in this connection". The existence of OptimizeSequencingFor and of a distinct OptimizedWaypoints list is the contract: the service REORDERS the submitted waypoints and the Connections list is emitted in that solved order, which need not be the order they were submitted in. Connection.Distance (DistanceMeters, long, @range 0..4294967295) is "Distance of the step" and Connection.TravelDuration (DurationSeconds, long) is the travel time of that step in seconds; the response-level Distance and Duration (both required) and TimeBreakdown.TravelDuration ("Traveling phase of the cycle", seconds) are the totals over the solved sequence. The model does not state how the per-step distance is measured, so the measurement itself is recorded behavior only: the recording shows road-network values for an Origin plus two waypoints, visiting 'pioneer-square' first and 'space-needle' second (Connections[0].To = pioneer-square at 1631 m / 312 s, Connections[1] = pioneer-square -> space-needle at 3144 m / 508 s), totalling Distance 4775, Duration 820 and TimeBreakdown.TravelDuration 820 — i.e. the totals are the sum of the connection legs over the OPTIMIZED order, and that order is cheaper than the submitted one.
Emulator: it never solves a sequence. Connections comes back in the order the waypoints were submitted — Connections[0].To = 'space-needle' and Connections[1].From = 'space-needle' / .To = 'pioneer-square', exactly the inverse of the recorded solution — and the per-leg metrics are synthesized rather than measured on a road network (1756 m / 345 s and 3687 m / 731 s against the recorded 1631/312 and 3144/508). The totals are internally consistent sums of those synthetic legs (1756+3687 = 5443 against the recorded 4775; 345+731 = 1076 against 820, repeated in TimeBreakdown.TravelDuration), so the response is self-coherent and simply describes a different, un-optimized trip.
Why: the single thing a client calls this API for is the ORDER to drive the waypoints in. A caller that reads Connections (or OptimizedWaypoints) as its dispatch sequence drives the stops in the order it already had, so the call adds cost instead of removing it; a caller that compares the returned total Distance/Duration against its own naive route to decide whether to accept the optimization sees the optimized total as WORSE than the real optimum and can reject a genuine saving. Because the per-leg values are synthesized, any ETA or cost per stop derived from Connection.TravelDuration is wrong independently of the ordering (see Cause 2), and a caller keyed on Connection.From/To will attribute a leg's distance to the wrong pair of stops.
Fix: the OptimizeWaypoints handler in the geo-routes crate — the waypoint-sequencing step (honour OptimizeSequencingFor and emit Connections/OptimizedWaypoints in the solved order rather than input order) and the leg-metric computation feeding Connection.Distance/TravelDuration and the response totals Distance/Duration/TimeBreakdown.TravelDuration, which must be road-network measurements summed over the solved sequence.
Traces: geo-routes-optimize-waypoints-basic-success#task#1 (representative, 10 diverging keys) and the same cluster member geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2.

=== Cause 2 of 2: a single-connection sequence still returns wrong Distance/TravelDuration, so the metric computation is wrong independently of the ordering ===
AWS: Smithy: the same OptimizeWaypointsResponse members — Connections[].Distance (DistanceMeters, "Distance of the step"), Connections[].TravelDuration (DurationSeconds), and the required totals Distance, Duration and TimeBreakdown.TravelDuration. With a single waypoint there is only one possible sequence, so the one connection's Distance/TravelDuration ARE the totals and no sequencing choice is involved. The recording shows the road-network leg for the submitted origin/waypoint pair: Connections[0].Distance 1227 m, Connections[0].TravelDuration 241 s, with response Distance 1227, Duration 241 and TimeBreakdown.TravelDuration 241 — the totals equalling the single leg is the requirement; the specific metres and seconds are that recording's values for that coordinate pair, not a constant.
Emulator: it returns 1318 m and 263 s for the same pair, and propagates them to Distance, Duration and TimeBreakdown.TravelDuration (1318 / 263 / 263). The totals-equal-the-single-leg invariant holds, so the defect is purely the measurement: the emulator over-states distance and duration for the leg. Hypothesis: the value is a geometric (straight-line/great-circle) estimate between the two coordinates with a fixed speed assumption rather than a road-network traversal — the model states nothing about the measurement, and this cluster shows the error survives when no reordering is possible, so it is a separate root cause from Cause 1.
Why: a caller that uses Duration as the ETA for the leg, or Distance for mileage reimbursement, fuel or billing, is wrong on every single-stop optimization — the simplest and most common call shape — even after the sequencing of Cause 1 is fixed. Because the error is in the same direction as Cause 1's legs, a fixer who only reorders the waypoints will still fail this fixture and must close both.
Fix: the leg-metric computation used by the OptimizeWaypoints handler (the distance/duration source shared by every connection), not the sequencer; the totals and TimeBreakdown then follow from it.
Traces: geo-routes-optimize-waypoints-origin-only-rejected#task#2 (5 diverging keys: $.Connections[0].Distance, $.Connections[0].TravelDuration, $.Distance, $.Duration, $.TimeBreakdown.TravelDuration).

## Evidence
Cause 1 — OptimizeWaypoints, fixture geo-routes-optimize-waypoints-basic-success step task#1, expected 200 / actual 200 (cluster also covers geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2):
  $.Connections[0].To:               expected 'pioneer-square', got 'space-needle'
  $.Connections[1].From:             expected 'pioneer-square', got 'space-needle'
  $.Connections[1].To:               expected 'space-needle',   got 'pioneer-square'
  $.Connections[0].Distance:         expected 1631, got 1756
  $.Connections[0].TravelDuration:   expected 312,  got 345
  $.Connections[1].Distance:         expected 3144, got 3687
  $.Connections[1].TravelDuration:   expected 508,  got 731
  $.Distance:                        expected 4775, got 5443
  $.Duration:                        expected 820,  got 1076
  $.TimeBreakdown.TravelDuration:    expected 820,  got 1076
  The From/To pairs show the emulator visiting the waypoints in submission order ('space-needle' then 'pioneer-square') while AWS solves to the reverse order. Both sides' totals are exact sums of their own legs (1631+3144 = 4775; 1756+3687 = 5443), so the response is self-consistent and simply describes an un-optimized trip; the concrete ids and metres/seconds are this recording's values, the requirement is that the sequence is solved and the metrics are road-network measurements over it.

Cause 2 — OptimizeWaypoints, fixture geo-routes-optimize-waypoints-origin-only-rejected step task#2, expected 200 / actual 200:
  $.Connections[0].Distance:         expected 1227, got 1318
  $.Connections[0].TravelDuration:   expected 241,  got 263
  $.Distance:                        expected 1227, got 1318
  $.Duration:                        expected 241,  got 263
  $.TimeBreakdown.TravelDuration:    expected 241,  got 263
  Only one connection exists here, so no sequencing decision can account for the gap: the leg measurement itself diverges, and the emulator correctly propagates its own wrong leg into the three totals.

No status or error-code divergence is involved in either cause: every step returned HTTP 200 as expected, and no modeled error (ValidationException 400, AccessDeniedException 403, ThrottlingException 429, InternalServerException 500) is in play.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus()).
3. Cause 1: fixture geo-routes-optimize-waypoints-basic-success, step task#1 — POST /v2/optimize-waypoints (OptimizeWaypoints) with the recorded Origin and the two recorded Waypoints ('space-needle', 'pioneer-square') in the recorded submission order. Read $.Connections: the first connection must end at the waypoint the solver picks first ('pioneer-square' in the recording), and $.Connections[*].From/To must describe the solved order, not the submitted one. The emulator returns the submitted order. Then check that $.Distance / $.Duration / $.TimeBreakdown.TravelDuration equal the sum of the connection legs for that solved order (4775 / 820 / 820 recorded). The same divergence reproduces in geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2.
4. Cause 2: fixture geo-routes-optimize-waypoints-origin-only-rejected, step task#2 — POST /v2/optimize-waypoints with the recorded Origin and a single waypoint. There is only one possible sequence, so compare $.Connections[0].Distance / .TravelDuration (1227 / 241 recorded) and the totals $.Distance / $.Duration / $.TimeBreakdown.TravelDuration against the recording. The emulator returns 1318 / 263 and propagates them, showing the metric source is wrong even with no reordering to do.
5. Re-replay both fixtures after a fix: Cause 1 requires the solved ordering AND road-network metrics; Cause 2 must pass on the metrics alone. Fixing only the sequencer leaves Cause 2 failing.
