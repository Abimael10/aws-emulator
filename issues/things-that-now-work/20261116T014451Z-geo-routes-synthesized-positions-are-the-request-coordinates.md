Issue-Source: trace-sweep-divergence
Issue-Trace: geo-routes-calculate-routes-truck-travel-mode-options-wiring-success
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-routes-calculate-routes-truck-travel-mode-options-wiring-success geo-routes-calculate-routes-zero-max-alternatives-returns-single-route geo-routes-snap-to-roads-basic-success geo-routes-snap-to-roads-invalid-coordinate-rejected geo-routes-snap-to-roads-snap-radius-out-of-range-rejected
Issue-Shape-Id: com.amazonaws.georoutes#RoutesService
Issue-Key: geo-routes|CalculateRoutes|2xx->2xx|$.Routes[*].Legs[*].VehicleLegDetails.Arrival.Place.OriginalPosition[*]
Issue-Key: geo-routes|CalculateRoutes|2xx->2xx|$.Routes[*].Legs[*].VehicleLegDetails.Arrival.Place.Position[*]
Issue-Key: geo-routes|CalculateRoutes|2xx->2xx|$.Routes[*].Legs[*].VehicleLegDetails.Departure.Place.OriginalPosition[*]
Issue-Key: geo-routes|CalculateRoutes|2xx->2xx|$.Routes[*].Legs[*].VehicleLegDetails.Departure.Place.Position[*]
Issue-Key: geo-routes|SnapToRoads|2xx->2xx|$.SnappedGeometry.Polyline
Issue-Key: geo-routes|SnapToRoads|2xx->2xx|$.SnappedTracePoints[*].Confidence
Issue-Key: geo-routes|SnapToRoads|2xx->2xx|$.SnappedTracePoints[*].SnappedPosition[*]
Issue-Commit: 6370de200c8fc422001a26814fa6edc1d0ccfb06

# geo-routes synthesized positions are the request coordinates rounded, not road-snapped points, and SnapToRoads geometry and confidence are invented

> _Note: this issue was identified against the code base at commit `6370de200c8fc422001a26814fa6edc1d0ccfb06`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CalculateRoutes

## Description
This bundle carries 3 net-new causes in the geo-routes position/geometry synthesis. All three answer 200 with a well-formed body, so the divergence is in the values, not the shape. A related open issue, 20261114T030203Z-geo-routes-calculateisolines-snappedorigin-is-the-request-origin-rounded-not.md, records the SAME class of fault on CalculateIsolines.SnappedOrigin; it does not cover any of the operations below and none of these clusters is owned by it.

=== Cause 1 of 3: CalculateRoutes leg Place.Position / Place.OriginalPosition ===
AWS: Smithy: CalculateRoutes -> CalculateRoutesResponse.Routes (RouteList, required); a vehicle leg's RouteVehicleLegDetails.Arrival/Departure.Place is RouteVehiclePlace. RouteVehiclePlace.OriginalPosition (target Position23, @length 2..3) is documented "Position provided in the request"; RouteVehiclePlace.Position (required, Position23) is documented "Position in World Geodetic System (WGS 84) format: [longitude, latitude]" and is the point on the road network the request position was matched to. Both are lists of Double, so full double precision is carried and nothing in the model quantizes them. The recording shows the contract directly: OriginalPosition reproduces the submitted coordinate digit-for-digit (departure longitude -122.3410001, arrival -122.3088001 / 47.4501999) while Position is a DISTINCT nearby snapped coordinate (-122.3409556 / 47.6089359 at the departure, -122.3029601 / 47.45124 at the arrival).
Emulator: both members come back as the request coordinate rounded to 4 decimal places (-122.341, -122.3088, 47.4502), and Position is byte-identical to OriginalPosition. Two things are therefore wrong at once: the echo of the request position loses precision, and no snapping is performed at all — Position is just the rounded input.
Why: a client that joins a returned leg back to its request by matching OriginalPosition against the coordinate it sent never gets an exact match, so the leg cannot be attributed to its waypoint. A client that derives anything from the offset between Position and OriginalPosition — side-of-street selection, access-point or "walk to the road" distance, map pin correction — always computes zero offset, and a client that round-trips the coordinate back into a subsequent CalculateRoutes call silently moves the point by up to ~10 m per axis.
Fix: the CalculateRoutes handler's place/position serialization in the geo-routes crate. Echo OriginalPosition at the precision it was received (no rounding/quantization step on the way out) and populate Position from the road-snap step rather than copying the input.
Traces: geo-routes-calculate-routes-truck-travel-mode-options-wiring-success#task#1 (representative) and geo-routes-calculate-routes-zero-max-alternatives-returns-single-route#task#1, which diverge on the same seven position keys.

=== Cause 2 of 3: SnapToRoads SnappedTracePoints[].SnappedPosition and .Confidence ===
AWS: Smithy: SnapToRoads -> SnapToRoadsResponse.SnappedTracePoints (required). RoadSnapSnappedTracePoint.SnappedPosition (Position, required) is "Snapped position of the TracePoint provided within the request, at the same index"; .Confidence (SensitiveDouble, required, @range min 0 max 1) is "Confidence value for the correctness of this point match". The recording shows snapped positions that are road-network points carried at 5-decimal precision and offset from the submitted trace point, and per-point confidences that vary with match quality: 1 for a point lying on the road and small values (0.05, 0.04, 0.12) for points the matcher is unsure of. Hypothesis: AWS derives the confidence from the distance between the trace point and the road segment it was matched to; the model states only the 0..1 range and the "correctness of this match" meaning, so the exact function is recorded behavior only.
Emulator: SnappedPosition is the submitted trace point rounded to 4 decimals and never actually matched to a road — it returns -122.3365 / 47.6101 where the record has -122.33662 / 47.61009, and -122.3363 / 47.611 where the record has -122.33718 / 47.61071, so the value is not even a correct rounding of the recorded snap. Confidence is a full-width pseudo-random double with no relation to the point (0.528049932485678, 0.2453644186911803, 1.0, 0.24533805074412507); it reports 1.0 for the point AWS scores 0.04, so even the RANKING of the points by confidence is inverted.
Why: Confidence is the only signal a caller has for discarding bad matches, so the usual client rule ("drop trace points below a threshold") keeps the bad points and drops the good ones, and does so differently per request. The un-snapped positions mean a caller that renders or re-measures the snapped track gets the raw GPS noise back instead of the road geometry, which is the entire purpose of the call.
Fix: the SnapToRoads handler's snapping and confidence computation — match each trace point to a road segment and derive both SnappedPosition and Confidence from that match instead of rounding the input and drawing a random score.
Traces: geo-routes-snap-to-roads-basic-success#task#1 (13 diverging keys) and the same fault in geo-routes-snap-to-roads-invalid-coordinate-rejected#task#3 / geo-routes-snap-to-roads-snap-radius-out-of-range-rejected#task#2.

=== Cause 3 of 3: SnapToRoads SnappedGeometry.Polyline ===
AWS: Smithy: SnapToRoadsResponse.SnappedGeometry (RoadSnapSnappedGeometry) with Polyline (@length min 1, sensitive) documented "An ordered list of positions used to plot a route on a map in a lossy compression format", and SnappedGeometryFormat required. RoadSnapSnappedGeometry's own doc calls it "Interpolated geometry for the snapped route that is overlay-able onto a map" — so the encoded line is INTERPOLATED along the road between the snapped points, not merely the snapped points joined. The recorded polyline accordingly decodes to a long run of vertices following the road.
Emulator: emits a far shorter polyline encoding only the (already wrong, rounded) trace points with no interpolation — for the basic fixture it returns a ~35-character string where the record carries a ~75-character one, and the first vertex differs because the snapped positions themselves are wrong.
Why: a caller that draws SnappedGeometry gets a straight-line zig-zag between GPS samples instead of a road-following line, and a caller that measures the geometry's length to compute a travelled distance under-measures the snapped route by the road curvature it dropped. Because the polyline is derived from Cause 2's positions, fixing Cause 2 alone still leaves the geometry un-interpolated.
Fix: the SnapToRoads geometry builder/serializer — build SnappedGeometry by interpolating along the matched road segments between consecutive snapped points, then encode.
Traces: geo-routes-snap-to-roads-basic-success#task#1 and geo-routes-snap-to-roads-invalid-coordinate-rejected#task#3 / geo-routes-snap-to-roads-snap-radius-out-of-range-rejected#task#2 (the $.SnappedGeometry.Polyline key in both clusters).

## Evidence
Cause 1 — CalculateRoutes, fixture geo-routes-calculate-routes-truck-travel-mode-options-wiring-success step task#1, expected 200 / actual 200:
  $.Routes[0].Legs[0].VehicleLegDetails.Arrival.Place.OriginalPosition[0]: expected -122.3088001, got -122.3088
  $.Routes[0].Legs[0].VehicleLegDetails.Arrival.Place.OriginalPosition[1]: expected 47.4501999, got 47.4502
  $.Routes[0].Legs[0].VehicleLegDetails.Arrival.Place.Position[0]:         expected -122.3029601, got -122.309
  $.Routes[0].Legs[0].VehicleLegDetails.Arrival.Place.Position[1]:         expected 47.45124, got 47.4502
  $.Routes[0].Legs[0].VehicleLegDetails.Departure.Place.OriginalPosition[0]: expected -122.3410001, got -122.341
  $.Routes[0].Legs[0].VehicleLegDetails.Departure.Place.Position[0]:       expected -122.3409556, got -122.341
  $.Routes[0].Legs[0].VehicleLegDetails.Departure.Place.Position[1]:       expected 47.6089359, got 47.6089
  (identical key set in geo-routes-calculate-routes-zero-max-alternatives-returns-single-route#task#1)
  Note that the emulator's Position equals its OriginalPosition in every case, while AWS's never does.

Cause 2 — SnapToRoads, fixture geo-routes-snap-to-roads-basic-success step task#1, expected 200 / actual 200:
  $.SnappedTracePoints[0].Confidence:        expected 1,    got 0.528049932485678
  $.SnappedTracePoints[1].Confidence:        expected 0.05, got 0.2453644186911803
  $.SnappedTracePoints[2].Confidence:        expected 0.04, got 1.0
  $.SnappedTracePoints[3].Confidence:        expected 0.12, got 0.24533805074412507
  $.SnappedTracePoints[0].SnappedPosition:   expected [-122.33662, 47.61009], got [-122.3365, 47.6101]
  $.SnappedTracePoints[1].SnappedPosition:   expected [-122.33718, 47.61071], got [-122.3363, 47.611]
  $.SnappedTracePoints[2].SnappedPosition:   expected [-122.33678, 47.61293], got [-122.336, 47.6121]
  $.SnappedTracePoints[3].SnappedPosition:   expected [-122.33605, 47.61324], got [-122.3357, 47.613]
  and in geo-routes-snap-to-roads-invalid-coordinate-rejected#task#3:
  $.SnappedTracePoints[0].Confidence:        expected 0.14, got 0.528049932485678
  $.SnappedTracePoints[0].SnappedPosition:   expected [-122.34181, 47.60998], got [-122.342, 47.6101]
  $.SnappedTracePoints[1].SnappedPosition:   expected [-122.34277, 47.61098], got [-122.3428, 47.611]
  The emulator repeats the literal confidence 0.528049932485678 for index 0 across two unrelated fixtures, confirming it is generated, not measured.

Cause 3 — SnapToRoads, same two fixtures, expected 200 / actual 200:
  $.SnappedGeometry.Polyline: expected 'BG0u856C326qpHwHnGgP3NgP3Nof_dkNvM8GnGoL_JkN7L8QvR8Lkc4IoVgKkX8B4D0UkwB8GoQ8G8Q', got 'BGov856Cnv6qpHo4BAAwMA4S4kCAo4BAA4S'
  $.SnappedGeometry.Polyline: expected 'BG4n856Cj7krpHkIwMoGsJkD4DsJ_JkNjS4cjwBnGrJ', got 'BGov856C_mlrpHo4BAA_xB'
  (the specific encoded strings are the recordings' values, not a requirement; the requirement is that the geometry is interpolated along the matched roads and starts at the first snapped position)

No status or error-code divergence is involved in any of the three causes: every step returned HTTP 200 as expected.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from it.
2. Replay the wire corpus (replay_wire_corpus()).
3. Cause 1: fixture geo-routes-calculate-routes-truck-travel-mode-options-wiring-success, step task#1 — POST /v2/routes (CalculateRoutes) with the recorded Origin/Destination at full precision. Compare $.Routes[0].Legs[0].VehicleLegDetails.Departure.Place.OriginalPosition against the Origin you sent (must be identical) and against .Position (must differ). The emulator returns both rounded to 4 dp and equal to each other. Repeat with geo-routes-calculate-routes-zero-max-alternatives-returns-single-route#task#1.
4. Cause 2: fixture geo-routes-snap-to-roads-basic-success, step task#1 — POST /v2/snap-to-roads (SnapToRoads) with the recorded 4 trace points. Inspect $.SnappedTracePoints[*].Confidence (expect graded match scores, one of them 1 and others near 0.05) and $.SnappedTracePoints[*].SnappedPosition (expect road-network points at 5 dp that differ from the submitted points). Re-run the request: the emulator's confidences do not correspond to the points. Repeat with geo-routes-snap-to-roads-invalid-coordinate-rejected#task#3 and geo-routes-snap-to-roads-snap-radius-out-of-range-rejected#task#2.
5. Cause 3: in the same two SnapToRoads responses, decode $.SnappedGeometry.Polyline. Expect an interpolated road-following vertex run beginning at the first snapped position; the emulator's polyline contains only the rounded trace points.
