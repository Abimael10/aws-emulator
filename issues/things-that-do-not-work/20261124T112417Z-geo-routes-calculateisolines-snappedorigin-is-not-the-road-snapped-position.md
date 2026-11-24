Issue-Source: trace-sweep-divergence
Issue-Trace: geo-routes-calculate-isolines-distance-threshold
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-routes-calculate-isolines-distance-threshold
Issue-Shape-Id: com.amazonaws.georoutes#RoutesService
Issue-Key: geo-routes|CalculateIsolines|2xx->2xx|$.SnappedOrigin[*]
Issue-Commit: df3e5af3e6ae07ab9a03dea7418cd4cc91eea7e3

# CalculateIsolines SnappedOrigin is not the road-snapped position

> _Note: this issue was identified against the code base at commit `df3e5af3e6ae07ab9a03dea7418cd4cc91eea7e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CalculateIsolines

## Description
geo-routes answers CalculateIsolines with HTTP 200 and a structurally valid body (Isolines, IsolineGeometryFormat, the times and PricingBucket all replay clean), but the SnappedOrigin it reports is not the point the road network produced for the submitted Origin. This bundle carries ONE cause.

=== Cause 1 of 1: SnappedOrigin is a synthesized approximation of the request Origin, not the road-snapped position ===
AWS: Smithy com.amazonaws.georoutes#CalculateIsolines (POST /v2/isolines, success code 200; modeled errors ValidationException 400, AccessDeniedException 403, ThrottlingException 429, InternalServerException 500 — none in play here, both sides answer 200, so no status, error code or error message diverges). CalculateIsolinesResponse.SnappedOrigin targets `Position` (list of Double, @length min 2 max 2, @sensitive) and is documented as the actual point on the road network used for the calculation, which may differ from the requested Origin when that Origin was not directly on a road. SnappedOrigin is therefore a value DERIVED from the road graph and carried at the full precision of that computed position — the recording carries 7 decimal places ([-122.3421045, 47.6097019]). The sibling SnappedDestination has the same target and the same semantics with respect to Destination.
Emulator: SnappedOrigin comes back as a locally synthesized approximation of the request coordinate rather than a road-graph resolution. In the replayed step the longitude is emitted at 4-decimal precision (-122.3421 against the recorded -122.3421045, i.e. the request coordinate quantized, losing the snap's trailing digits) while the latitude is a value (47.609664) that is neither the recorded snapped latitude (47.6097019) nor a correct rounding of it — the two axes are not even produced by the same rule. Hypothesis: the handler derives the position arithmetically from the request Origin (quantizing one axis and applying a fixed synthetic offset to the other) instead of resolving it against a road graph; the model states only the member's semantics and the recording only shows the resulting values, so the exact emulator formula is recorded behavior only.
Why: a client that displays where the isoline computation actually started, or that feeds SnappedOrigin back into a follow-up CalculateIsolines/CalculateRoutes call as a road-anchored waypoint, is handed a point that is not on the road network and is displaced from the true snap by up to roughly 10 m per axis. The standard client check "was my origin on a road?" — comparing Origin with SnappedOrigin — gives a meaningless answer, and any offset-derived value (access point, walk-to-road distance, map-pin correction) is wrong. Because the response is a 200 carrying a plausible-looking Position of the right length, the failure is completely silent.
Fix: the CalculateIsolines handler's response synthesis in the geo-routes service crate (services/service-com-amazonaws-georoutes-routesservice) — resolve the request Origin (and Destination, when supplied) against the road graph and emit the resulting position at the precision of that computed position into SnappedOrigin / SnappedDestination, instead of quantizing and offsetting the request coordinate.
Related issues, none of which covers this failure: the OPEN issue 20261116T014451Z-geo-routes-synthesized-positions-are-the-request-coordinates.md describes the same defect class for CalculateRoutes and SnapToRoads only and does NOT own this cluster (replay reports owned_by: [] for it). Three earlier issues on this exact member — 20261123T182046Z-geo-routes-calculateisolines-snappedorigin-is-the-request-origin-rounded-instead.md, 20261114T030203Z-geo-routes-calculateisolines-snappedorigin-is-the-request-origin-rounded-not.md and 20261103T145348Z-geo-routes-calculateisolines-response-synthesis-diverges-departuretime-omitted-origin-not.md — are all RESOLVED/fixed, so none of them covers this failure; the emitted values have changed since those fixes (the latitude is no longer a plain 4-dp rounding) but SnappedOrigin still diverges, so the member is re-filed here.
Traces: geo-routes-calculate-isolines-distance-threshold#task#1 (the cluster's only member; affected_count 1).

## Evidence
Replay of fixture geo-routes-calculate-isolines-distance-threshold, step task#1 — expected_status 200, actual_status 200, verdict mismatch. The divergence is entirely in body values:

  $.SnappedOrigin[0]: value_mismatch (expected -122.3421045, got -122.3421)
  $.SnappedOrigin[1]: value_mismatch (expected 47.6097019, got 47.609664)

divergence_key: service=geo-routes|CalculateIsolines|status=200->200|keys=$.SnappedOrigin[0],$.SnappedOrigin[1]; affected: [geo-routes-calculate-isolines-distance-threshold#task#1]; affected_count 1; stub_path empty; owned_by empty.

The specific coordinates above are that recording's values and are examples, not literal requirements. The requirement is the rule: SnappedOrigin must be the position produced by snapping the request Origin onto the road network, emitted at the precision of that computed position — not an arithmetic transform of the request coordinate. Note that the emulator's longitude is the coordinate at 4 dp while its latitude (47.609664) is not a rounding of the recorded snap at all, so the two axes do not come from one consistent rule.

No status, error code or error message divergence is involved: the step returns HTTP 200 on both sides and no modeled error (ValidationException / AccessDeniedException / ThrottlingException / InternalServerException) is exercised. $.Isolines, $.IsolineGeometryFormat, the departure/arrival times and the x-amz-geo-pricing-bucket header all replay clean.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus()).
3. Cause 1: replay fixture geo-routes-calculate-isolines-distance-threshold, step task#1 — POST /v2/isolines (CalculateIsolines) with the recorded request (an Origin that is not on the carriageway, Thresholds carrying a distance threshold).
4. The response is HTTP 200 and $.Isolines matches the recording. Compare $.SnappedOrigin against (a) the Origin you submitted and (b) the recorded response's SnappedOrigin.
5. Observe that $.SnappedOrigin is derived from the submitted Origin rather than from the road network: the longitude is the coordinate at 4 decimal places (got -122.3421 vs expected -122.3421045) and the latitude is an offset value that is not the recorded snap or any rounding of it (got 47.609664 vs expected 47.6097019).
6. The fix is confirmed when SnappedOrigin (and SnappedDestination, when a Destination is supplied) is a position derived from the road graph at that position's own precision, and this step replays clean.
