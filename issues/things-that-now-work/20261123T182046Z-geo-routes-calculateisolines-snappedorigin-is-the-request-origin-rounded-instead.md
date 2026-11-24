Issue-Source: trace-sweep-divergence
Issue-Trace: geo-routes-calculate-isolines-distance-threshold
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-routes-calculate-isolines-distance-threshold
Issue-Shape-Id: com.amazonaws.georoutes#RoutesService
Issue-Key: geo-routes|CalculateIsolines|2xx->2xx|$.SnappedOrigin[*]
Issue-Commit: 6e06957da8d63bf47f50e8aab29b9e886ef2aec8

# CalculateIsolines SnappedOrigin is the request Origin rounded instead of the road-snapped position

> _Note: this issue was identified against the code base at commit `6e06957da8d63bf47f50e8aab29b9e886ef2aec8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CalculateIsolines

## Description
geo-routes answers CalculateIsolines with HTTP 200 and a structurally valid body (the Isolines, geometry format, times and pricing bucket all replay clean), but the SnappedOrigin it reports is not a point taken from the road network: it is the request's own Origin coordinate re-emitted at reduced precision. This bundle carries ONE cause.

=== Cause 1 of 1: SnappedOrigin echoes the request Origin quantized to 4 decimal places, with no snapping applied ===
AWS: Smithy com.amazonaws.georoutes#CalculateIsolines -> CalculateIsolinesResponse.SnappedOrigin, target `Position` (list of Double, @length min 2 max 2, @sensitive), documented "The actual point on the road network used for calculations, which may differ from the requested origin if Origin was not directly on a road." So SnappedOrigin is a value DERIVED from the road graph and carried at the full precision of that computed position (the recording carries 7 decimal places), and it is expected to be laterally displaced from the submitted Origin whenever that Origin is not on the carriageway. The same applies to the sibling SnappedDestination (same Position target, same semantics for Destination). The operation is POST /v2/isolines with success code 200; its modeled errors are ValidationException (400), AccessDeniedException (403), ThrottlingException (429) and InternalServerException (500) — none is in play here, both sides answer 200, so no status, error code or error message diverges.
Emulator: SnappedOrigin[0] and SnappedOrigin[1] come back as the request's Origin rounded/truncated to 4 decimal places. No road-graph resolution happens at all: the returned point is wherever the caller put it, merely quantized. In the replayed step the emulator returns [-122.342, 47.6097] — exactly the submitted Origin at 4 dp — where the recording carries the full-precision snapped point [-122.3421045, 47.6097019]. The fact that the actual value is precisely the input at 4 dp (rather than a slightly different snap) is what identifies the behavior as "no snap, plus rounding".
Why: a client that shows the user where the isoline computation actually started, or that feeds SnappedOrigin back into a follow-up CalculateIsolines/CalculateRoutes call as a road-anchored waypoint, is handed an off-road point that is additionally quantized by up to roughly 10 m per axis. The usual client check "was my origin on a road?" — comparing Origin with SnappedOrigin — always reports "already on a road", and any offset-derived value (access point, walk-to-road distance, map pin correction) is always zero. Because the response is a 200 carrying a plausible-looking Position, the failure is completely silent.
Fix: the CalculateIsolines handler's response synthesis in the geo-routes crate — resolve the request Origin (and Destination) against the road graph and emit the resulting position at full precision into SnappedOrigin / SnappedDestination, instead of copying the request coordinate through a rounding step.
Related issues, none of which covers this failure: 20261114T030203Z-geo-routes-calculateisolines-snappedorigin-is-the-request-origin-rounded-not.md reported exactly this cause but is RESOLVED/fixed, so it no longer covers it — the divergence is live again at the current commit and is re-filed here; 20261103T145348Z-geo-routes-calculateisolines-response-synthesis-diverges-departuretime-omitted-origin-not.md is also resolved (its DepartureTime and pricing-bucket halves do replay clean now, the snapping half does not); and the OPEN issue 20261116T014451Z-geo-routes-synthesized-positions-are-the-request-coordinates.md describes the same rounding-instead-of-snapping defect class for CalculateRoutes and SnapToRoads only and does NOT own this CalculateIsolines cluster (replay reports owned_by: [] for it).
Traces: geo-routes-calculate-isolines-distance-threshold#task#1 (the cluster's only member; affected_count 1).

## Evidence
Replay of fixture geo-routes-calculate-isolines-distance-threshold, step task#1 — expected_status 200, actual_status 200, verdict mismatch. The divergence is entirely in body values:

  $.SnappedOrigin[0]: value_mismatch (expected -122.3421045, got -122.342)
  $.SnappedOrigin[1]: value_mismatch (expected 47.6097019, got 47.6097)

divergence_key: service=geo-routes|CalculateIsolines|status=200->200|keys=$.SnappedOrigin[0],$.SnappedOrigin[1]; affected: [geo-routes-calculate-isolines-distance-threshold#task#1]; affected_count 1; stub_path empty; owned_by empty.

The specific numbers above are that recording's coordinates and are examples, not literal requirements. The requirement is the rule: SnappedOrigin must be the position produced by snapping the request Origin onto the road network, emitted at the precision of that computed position, not the request coordinate rounded to 4 decimals. The emulator's two values are bit-for-bit the submitted Origin at 4 dp.

No status, error code or error message divergence is involved: the step returns HTTP 200 on both sides and no modeled error (ValidationException / AccessDeniedException / ThrottlingException / InternalServerException) is exercised. $.Isolines and the remaining response members replay clean.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus()).
3. Cause 1: replay fixture geo-routes-calculate-isolines-distance-threshold, step task#1 — POST /v2/isolines (CalculateIsolines) with the recorded request (an Origin that is not on the carriageway, Thresholds carrying a distance threshold).
4. The response is HTTP 200 and $.Isolines matches the recording. Compare $.SnappedOrigin with (a) the Origin you submitted and (b) the recorded response's SnappedOrigin.
5. Observe that $.SnappedOrigin is the submitted Origin rounded to 4 decimal places (got -122.342, 47.6097) while the recording carries a distinct full-precision road-snapped point (expected -122.3421045, 47.6097019).
6. The fix is confirmed when SnappedOrigin (and SnappedDestination, when a Destination is supplied) is a position derived from the road graph at full precision rather than the quantized request coordinate, and this step replays clean.
