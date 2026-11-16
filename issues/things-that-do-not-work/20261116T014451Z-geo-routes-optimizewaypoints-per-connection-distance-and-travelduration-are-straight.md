Issue-Source: trace-sweep-divergence
Issue-Trace: geo-routes-optimize-waypoints-basic-success
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-routes-optimize-waypoints-basic-success geo-routes-optimize-waypoints-duplicate-ids-rejected geo-routes-optimize-waypoints-origin-only-rejected
Issue-Shape-Id: com.amazonaws.georoutes#RoutesService
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Connections[*].Distance
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Connections[*].TravelDuration
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Distance
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.Duration
Issue-Key: geo-routes|OptimizeWaypoints|2xx->2xx|$.TimeBreakdown.TravelDuration
Issue-Commit: 6370de200c8fc422001a26814fa6edc1d0ccfb06

# OptimizeWaypoints per-connection Distance and TravelDuration are straight-line estimates, so every aggregate total diverges

> _Note: this issue was identified against the code base at commit `6370de200c8fc422001a26814fa6edc1d0ccfb06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
OptimizeWaypoints

## Description
This bundle carries 2 net-new causes, both in OptimizeWaypoints' metric computation. Both steps answer 200 with a correctly shaped body, so the divergence is in the computed numbers. A related open issue, 20261114T131205Z-geo-routes-calculateroutematrix-per-cell-distance-and-duration-still-diverge.md, records the same CLASS of fault on CalculateRouteMatrix cells; it covers a different operation and neither cluster below is owned by it.

=== Cause 1 of 2: multi-waypoint sequence — per-connection metrics and the totals derived from them ===
AWS: Smithy: OptimizeWaypoints -> OptimizeWaypointsResponse.Connections (WaypointOptimizationConnectionList, required), each WaypointOptimizationConnection carrying Distance (DistanceMeters, long, @range 0..4294967295, doc "Distance of the step") and TravelDuration (DurationSeconds, long, doc "Total duration", unit seconds), plus the required top-level Distance/Duration and TimeBreakdown (WaypointOptimizationTimeBreakdown with TravelDuration/RestDuration/WaitDuration/ServiceDuration, all seconds). These are road-network traversal metrics for the optimized sequence: in the recording the two connections are 1631 m / 312 s and 3144 m / 508 s, and the aggregates are exactly their sums (4775 m, 820 s) with TimeBreakdown.TravelDuration = 820 because no rest/wait/service time is configured. The model states the units, types and ranges; the magnitudes themselves are recorded behavior only.
Emulator: returns 1947 m / 240 s and 3793 m / 234 s, totalling 5740 m / 474 s with TimeBreakdown.TravelDuration 474. The distances are ~20% LONG while the durations are ~42% SHORT, i.e. the implied average speed is ~29 km/h against the recorded ~19 km/h. Hypothesis: the handler computes a great-circle/straight-line distance between consecutive waypoints and divides it by one fixed speed constant, instead of traversing a road network — that is the only way a leg can be simultaneously longer and faster than the recorded one. The aggregates and TimeBreakdown.TravelDuration are consistent sums of the per-connection values, so the per-step estimator is the single root cause and the three totals follow from it.
Why: these numbers are the output of the call — a client sizing a delivery or service window, costing a route, or checking a plan against a driver's hours gets a sequence that is reported as travelling further in less time than it can. Because the estimator also drives the objective being optimized, two waypoint orders can be ranked in the wrong relative order, so the returned OptimizedWaypoints sequence is not necessarily the one AWS would choose.
Fix: the OptimizeWaypoints handler's per-connection distance/duration estimator in the geo-routes crate — derive each connection's Distance and TravelDuration from a road-network traversal (the same source CalculateRoutes legs should use) and keep the top-level Distance/Duration and TimeBreakdown.TravelDuration as sums over the connections.
Traces: geo-routes-optimize-waypoints-basic-success#task#1 (representative) and geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2, which diverge on the same seven keys.

=== Cause 2 of 2: single-connection origin-to-destination case ===
AWS: same members as Cause 1. With no intermediate waypoints there is exactly one connection, and the recording shows the single connection's 1227 m / 241 s reproduced verbatim as the top-level Distance/Duration and as TimeBreakdown.TravelDuration.
Emulator: returns 1651 m / 226 s for the sole connection and propagates those to Distance, Duration and TimeBreakdown.TravelDuration. Same over-long/under-timed signature as Cause 1 (~35% long, ~6% fast), on the degenerate one-leg path where no sequencing decision is made at all.
Why: this is the case a client uses as its baseline — the cost of visiting one stop — so a client that compares an optimized multi-stop plan against the single-stop baseline compares two differently-wrong numbers and computes a bogus saving. It is recorded as a separate cluster because it exercises the estimator outside the sequencing path: a fix validated only on the multi-waypoint fixture does not demonstrate this step passes.
Fix: the same OptimizeWaypoints per-connection estimator; confirm it on the single-connection path as well as the multi-waypoint one.
Traces: geo-routes-optimize-waypoints-origin-only-rejected#task#2.

## Evidence
Cause 1 — fixture geo-routes-optimize-waypoints-basic-success step task#1, expected 200 / actual 200:
  $.Connections[0].Distance:         expected 1631, got 1947
  $.Connections[0].TravelDuration:   expected 312,  got 240
  $.Connections[1].Distance:         expected 3144, got 3793
  $.Connections[1].TravelDuration:   expected 508,  got 234
  $.Distance:                        expected 4775, got 5740
  $.Duration:                        expected 820,  got 474
  $.TimeBreakdown.TravelDuration:    expected 820,  got 474
  (same key set in geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2)
  Expected implied speed 4775 m / 820 s = 5.8 m/s (~21 km/h); emulator 5740 m / 474 s = 12.1 m/s (~44 km/h). Totals equal the sum of the connections in both, so only the per-connection estimator diverges.

Cause 2 — fixture geo-routes-optimize-waypoints-origin-only-rejected step task#2, expected 200 / actual 200:
  $.Connections[0].Distance:         expected 1227, got 1651
  $.Connections[0].TravelDuration:   expected 241,  got 226
  $.Distance:                        expected 1227, got 1651
  $.Duration:                        expected 241,  got 226
  $.TimeBreakdown.TravelDuration:    expected 241,  got 226

The listed magnitudes are the recordings' values and are not themselves the requirement: the requirement is that Distance/TravelDuration are road-network metrics for each connection (consistent with each other at a plausible road speed) and that the top-level Distance/Duration and TimeBreakdown.TravelDuration are the sums over Connections. No status or error-code divergence: both steps returned HTTP 200 as expected.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from it.
2. Call replay_wire_corpus().
3. Cause 1: fixture geo-routes-optimize-waypoints-basic-success, step task#1 — POST /v2/optimize-waypoints (OptimizeWaypoints) with the recorded Origin, Destination and two Waypoints. Read $.Connections[*].Distance / .TravelDuration and check each pair's implied speed, then check $.Distance, $.Duration and $.TimeBreakdown.TravelDuration equal the sums. The emulator's legs are longer and faster than the recorded road-network legs. Repeat with geo-routes-optimize-waypoints-duplicate-ids-rejected#task#2 (the successful step after the duplicate-id rejection).
4. Cause 2: fixture geo-routes-optimize-waypoints-origin-only-rejected, step task#2 — the same POST with an Origin and Destination but no intermediate waypoints, so a single connection comes back. Compare $.Connections[0].Distance/.TravelDuration and the three aggregates against the recording; the emulator over-reports distance and under-reports duration on this one-leg path too.
