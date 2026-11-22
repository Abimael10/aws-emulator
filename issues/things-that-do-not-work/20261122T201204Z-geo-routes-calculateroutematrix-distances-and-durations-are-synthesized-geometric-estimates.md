Issue-Source: trace-sweep-divergence
Issue-Trace: geo-routes-calculate-route-matrix-basic-success
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-routes-calculate-route-matrix-basic-success
Issue-Shape-Id: com.amazonaws.georoutes#RoutesService
Issue-Key: geo-routes|CalculateRouteMatrix|2xx->2xx|$.RouteMatrix[*][*].Distance
Issue-Key: geo-routes|CalculateRouteMatrix|2xx->2xx|$.RouteMatrix[*][*].Duration
Issue-Commit: f14686596c152030b22d16342426285c82834d41

# CalculateRouteMatrix distances and durations are synthesized geometric estimates, not road-network values

> _Note: this issue was identified against the code base at commit `f14686596c152030b22d16342426285c82834d41`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CalculateRouteMatrix

## Description
One net-new cause in the geo-routes CalculateRouteMatrix response body. The step answers HTTP 200 with a correctly shaped matrix (right number of rows and columns, all required members present), so the divergence is entirely in the VALUES of every RouteMatrixEntry.

Related open issues record the same CLASS of fault (synthesized geometry instead of road-network computation) on other geo-routes operations: 20261116T014451Z-geo-routes-synthesized-positions-are-the-request-coordinates.md (CalculateRoutes / SnapToRoads positions and geometry), 20261118T133716Z-geo-routes-optimizewaypoints-keeps-the-request-waypoint-order-and-synthesizes.md (OptimizeWaypoints) and 20261114T030203Z-geo-routes-calculateisolines-snappedorigin-is-the-request-origin-rounded-not.md (CalculateIsolines.SnappedOrigin). None of them covers CalculateRouteMatrix and this cluster is not owned by any of them, so it is filed here.

=== Cause 1 of 1: every RouteMatrix[i][j].Distance and .Duration ===
AWS: Smithy (com.amazonaws.georoutes#RoutesService): CalculateRouteMatrix POST /v2/route-matrix -> CalculateRouteMatrixResponse.RouteMatrix (required, list of RouteMatrixRow of RouteMatrixEntry). RouteMatrixEntry.Distance (DistanceMeters, long, required, @range 0..4294967295, sensitive) is documented "The total distance of travel for the route"; RouteMatrixEntry.Duration (DurationSeconds, long, required, same range, sensitive) is documented "The expected duration of travel for the route. Unit: seconds". RouteMatrix's own doc fixes the indexing: "Each row corresponds to one entry in Origins. Each entry in the row corresponds to the route from that entry in Origins to an entry in Destination positions." So each cell must be the travel distance and travel duration of an actual route over the road network for that origin/destination pair, honouring the request's TravelMode, TravelModeOptions, Avoid, Traffic and OptimizeRoutingFor. The model does not state how the value is derived, so the derivation itself is recorded behavior only: in the recording the four cells carry distances 22249, 54104, 27518 and 59553 metres with durations 976, 2047, 1247 and 2354 seconds, i.e. implied average speeds that VARY per pair (roughly 22, 26, 22 and 25 m/s) because different pairs use different road classes, and the per-pair distances do not scale with straight-line separation.
Emulator: every cell is computed from the request coordinates alone. For the same request it returns 29355/1405, 47735/2327, 26313/1326 and 58336/2628, i.e. its implied speed is nearly constant across all four pairs (about 21, 21, 20 and 22 m/s) and its distances err in BOTH directions relative to the recording — too high by ~32% for the first pair and too low by ~12% for the second. Hypothesis: the handler derives the distance from the great-circle separation of the pair multiplied by a fixed detour factor and the duration by dividing by a fixed assumed speed, which is why no road class, turn restriction or traffic condition can change the ratio, and why the error has no consistent sign. The recorded response gives no Error member on any cell, and the emulator sets none either, so the divergence is confined to Distance/Duration.
Why: the whole point of a route matrix is to feed a cost matrix into a downstream decision — vehicle-to-job assignment, nearest-depot selection, a TSP/VRP solve, an ETA table. Because the emulator's error is not a constant factor but varies per pair, the RANKING of pairs changes: a client picks a different "nearest" origin than AWS would, and any solver fed the matrix returns a different optimal assignment, so an integration test that asserts on the chosen route or depot passes against the emulator and fails against AWS. Clients that surface the figures to users show ETAs minutes wrong (1405 s vs 976 s is a 7-minute error on a 16-minute trip) and distances kilometres wrong, and clients that compare a matrix cell against a CalculateRoutes result for the same pair see the two disagree. Because Distance/Duration are the only payload this operation carries, no caller can use it correctly.
Fix: the CalculateRouteMatrix handler's matrix-entry computation in the geo-routes service crate — populate each RouteMatrixEntry from the same road-network routing used to answer CalculateRoutes for that origin/destination pair (so a matrix cell and the corresponding CalculateRoutes leg agree), honouring TravelMode/TravelModeOptions/Avoid/Traffic/OptimizeRoutingFor, instead of synthesizing from coordinate separation and a fixed speed. Keep the documented row/column indexing (row = Origins index, column = Destinations index).
Traces: geo-routes-calculate-route-matrix-basic-success#task#1 (all 8 Distance/Duration keys of the 2x2 matrix diverge; it is the only member of the cluster).

## Evidence
Fixture geo-routes-calculate-route-matrix-basic-success, step task#1, POST /v2/route-matrix (CalculateRouteMatrix). Expected status 200, actual status 200 — no status, error code or error message divergence is involved; every diverging key is a value in the success body:

  $.RouteMatrix[0][0].Distance: expected 22249, got 29355
  $.RouteMatrix[0][0].Duration: expected 976,   got 1405
  $.RouteMatrix[0][1].Distance: expected 54104, got 47735
  $.RouteMatrix[0][1].Duration: expected 2047,  got 2327
  $.RouteMatrix[1][0].Distance: expected 27518, got 26313
  $.RouteMatrix[1][0].Duration: expected 1247,  got 1326
  $.RouteMatrix[1][1].Distance: expected 59553, got 58336
  $.RouteMatrix[1][1].Duration: expected 2354,  got 2628

Every cell of the matrix diverges in both members; none matches. The specific metre and second figures above are this recording's values for this recording's Origins/Destinations and are NOT the requirement — the requirement is that each cell is the road-network travel distance and travel duration for its (Origins[i], Destinations[j]) pair under the requested travel mode and options. Two properties of the recording that ARE structural, and that the emulator violates for any input:
  (a) the implied average speed differs per cell (22.8, 26.4, 22.1, 25.3 m/s in the recording) because different pairs traverse different road classes; the emulator's implied speed is effectively constant (20.9, 20.5, 19.8, 22.2 m/s);
  (b) the emulator's distance error has no consistent sign (+32% on [0][0], -12% on [0][1]), so it is not a precision or unit problem that a scale factor could correct — the values are not derived from a road network at all.
No RouteMatrixEntry.Error is present in either the recorded or the emulator response, and ErrorCount, PricingBucket and RoutingBoundary all matched.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus: replay_wire_corpus().
3. Fixture geo-routes-calculate-route-matrix-basic-success, step task#1 — POST /v2/route-matrix with the recorded Origins (2) and Destinations (2).
4. Read $.RouteMatrix[i][j].Distance and .Duration for all four cells and divide Distance by Duration per cell: the emulator yields practically the same implied speed for every pair, whereas the recorded response's implied speeds differ per pair. Compare each cell against the recorded expectation: all 8 values diverge (see Evidence).
5. Cross-check against CalculateRoutes for the same origin/destination pair (e.g. fixture geo-routes-calculate-routes-truck-travel-mode-options-wiring-success): the matrix cell and the route leg should report the same distance and duration for the same pair; the emulator's do not come from the same computation.
6. Re-issue the same request with a different TravelMode / TravelModeOptions / Avoid setting: the emulator's Distance and Duration are unchanged, showing the values are synthesized from coordinates only.
