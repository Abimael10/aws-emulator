Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: location.get-map-tile-enforces-per-style-zoom-bound
Issue-Shape-Id: com.amazonaws.location#LocationService
Issue-Key: location|GetMapTile|ValidationException
Issue-Commit: d9ff7938ad585818e5d4c00867351109df7b038a

# Location GetMapTile out-of-range refusal sentences are emulator-authored: the zoom refusal names a bare number instead of "maximum zoom of style <Style> (<max>)", and the X/Y refusal omits AWS's range clause

> _Note: this issue was identified against the code base at commit `d9ff7938ad585818e5d4c00867351109df7b038a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMapTile

## Description
One net-new root cause on the Amazon Location Service Maps v1 data plane (service token `location`): GetMapTile refuses an out-of-pyramid tile address with 400 ValidationException, but the message text is this emulator's own sentence rather than the sentence AWS words for that refusal class, in both of the sentences the operation can emit for a tile address. No open Location issue covers it — `20261029T234705Z-location-refusal-bodies-are-emulator-authored-and-omit.md` covers the missing ValidationException `Reason`/`FieldList` envelope members, and `20261115T022721Z-location-map-data-plane-responses-are-emulator-authored.md` covers the 200 bodies (style descriptor contents, tile Content-Type); neither states GetMapTile's address-refusal message text.

Cause 1 of 1: the GetMapTile address-out-of-range ValidationException messages do not use AWS's wording (failing check location.get-map-tile-enforces-per-style-zoom-bound).
AWS: Smithy (services/service-com-amazonaws-location-locationservice/smithy-model/com-amazonaws-location-locationservice.json) models GetMapTile as GET /maps/v0/maps/{MapName}/tiles/{Z}/{X}/{Y} with Z/X/Y as required @httpLabel @sensitive strings whose only modeled constraint is pattern `\d+`; the model declares no numeric upper bound, and ValidationException (HTTP 400) is the modeled refusal. The bound and the sentence are therefore recorded behavior only: AWS refuses a Z deeper than the MAP'S OWN STYLE's maximum zoom with 400 ValidationException whose message is `'Z' must not be greater than maximum zoom of style <StyleName> (<maxZoom>).` — fixed text `'Z' must not be greater than maximum zoom of style ` + `<StyleName>` (the style stored on the addressed map resource, e.g. VectorEsriStreets) + ` (` + `<maxZoom>` (that style's published maxzoom, the same number its style descriptor declares) + `).`, terminated by a period; and it refuses an X (or Y) outside the grid of the requested zoom with message `'X' must be an integer in the range 0..2^Zoom-1 (0..<axis-1>).` — fixed text `'X' must be an integer in the range 0..2^Zoom-1 (0..` + `<axis-1>` (2^Z - 1 for the requested Z, e.g. 7 at Z=3) + `).`. The member name in single quotes is taken from the request's failing label, the style name from the map's stored configuration, and the numbers from that style's bound and the requested zoom; the recording's own style name and numbers are template fill, not requirements.
Emulator: answers the correct 400 ValidationException but with its own sentences. For Z=30 against a map created with Configuration.Style=VectorEsriStreets it returns message `'Z' must not be greater than 22` — no `maximum zoom of style <Style>` phrase, no parenthesised bound, no terminating period, so nothing in the message identifies WHICH style's bound was applied. The sibling axis refusal is built by the same helper and has the same defect: services/service-com-amazonaws-location-locationservice/src/state/map_tiles.rs `zoom_out_of_range` formats `'{Z}' must not be greater than {max_zoom}` and `axis_out_of_range` formats `'{member}' must not be greater than {axis-1}` instead of AWS's `must be an integer in the range 0..2^Zoom-1 (0..N).` clause. The check asserts the zoom sentence first, so it stops there; the axis sentence is the same authored-wording fault in the same helper and is listed here as part of this cause rather than as a separate work item.
Why: a client parsing the refusal to discover the style's usable zoom range (the check does exactly this, and map clients do it to clamp their zoom slider or pick a fallback style) finds no style name and no declared maximum in the emulator's sentence, so it cannot tell a per-style zoom ceiling from a generic bound and cannot learn the ceiling at all; test suites and tooling that assert AWS's message text fail against the emulator even though status 400 and code ValidationException are right. The same applies to the X/Y sentence, which in AWS states the computed valid range and in the emulator states only an upper bound.
Fix: the GetMapTile refusal-message helpers for tile addresses — `zoom_out_of_range` and `axis_out_of_range` in the Location map-tile state module — which must emit AWS's sentences, with the style name resolved from the addressed map resource's stored MapConfiguration.Style and the bounds from that style's catalog entry and the requested zoom; the handler's ordering (label parse, map lookup, zoom bound, axis bound) is already right. File/line citations: services/service-com-amazonaws-location-locationservice/src/state/map_tiles.rs:104-120 (both sentence builders), services/service-com-amazonaws-location-locationservice/src/ops/get_map_tile.rs:229-249 (the two call sites).
Traces: location.get-map-tile-enforces-per-style-zoom-bound.

Not asserted by this run, and NOT filed as defects: the check's later steps (tile bytes served at exactly the declared maximum zoom, and the one-past-maximum refusal) were never reached because the first assertion failed, so whether the emulator's catalog value for VectorEsriStreets' maximum zoom (22) equals AWS's published maxzoom for that style is not evidenced here and is not claimed.

## Evidence
Behavioral suite simparity-behavioral-aws-v1, service `location`, check `get-map-tile-enforces-per-style-zoom-bound` — verdict: mismatch.

Failure as reported:
  File "check_location_get_map_tile_enforces_per_style_zoom_bound.py", line 47, in run
    assert match, f"could not find a declared maximum zoom in {err['Message']!r}"
  AssertionError: could not find a declared maximum zoom in "'Z' must not be greater than 22"

Sequence the check runs: CreateMap(MapName=<generated>, Configuration={"Style": "VectorEsriStreets"}), then GetMapTile(MapName=<that map>, Z="30", X="0", Y="0").
  Expected: 400 ValidationException, message matching `maximum zoom of style \S+ \((\d+)\)` — i.e. `'Z' must not be greater than maximum zoom of style VectorEsriStreets (<maxZoom>).`
  Actual:   400 ValidationException, message `'Z' must not be greater than 22` (error code and HTTP status are correct; only the message diverges).

The check's further expectations, unreached because the first assertion aborted it:
  GetMapTile at Z=<maxZoom> must return tile bytes;
  GetMapTile at Z=<maxZoom+1> must answer message exactly `'Z' must not be greater than maximum zoom of style VectorEsriStreets (<maxZoom>).`;
  GetMapTile(Z="3", X="8") must answer message exactly `'X' must be an integer in the range 0..2^Zoom-1 (0..7).` — the emulator's `axis_out_of_range` instead produces `'X' must not be greater than 7`.

Smithy consulted: GetMapTile — uri /maps/v0/maps/{MapName}/tiles/{Z}/{X}/{Y}, method GET, code 200; input Z/X/Y @httpLabel @required @sensitive target SensitiveString pattern `\d+` (no numeric bound modeled), MapName @required pattern `^[-._\w]+$` len 1..100; errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400).

Emulator source: services/service-com-amazonaws-location-locationservice/src/state/map_tiles.rs:104 `format!("'{ZOOM_MEMBER}' must not be greater than {max_zoom}")`; :115 `format!("'{member}' must not be greater than {}", axis_tiles.saturating_sub(1))`.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the behavioral suite check `location.get-map-tile-enforces-per-style-zoom-bound`.
2. Or by hand against the emulator's location endpoint:
   a. `aws location create-map --map-name <name> --configuration Style=VectorEsriStreets`
   b. `aws location get-map-tile --map-name <name> --z 30 --x 0 --y 0`
   Observe HTTP 400 ValidationException with message `'Z' must not be greater than 22`, instead of AWS's `'Z' must not be greater than maximum zoom of style VectorEsriStreets (<maxZoom>).` (style name from the map's stored configuration, bound from that style's published maxzoom, terminating period).
   c. `aws location get-map-tile --map-name <name> --z 3 --x 8 --y 0`
   Observe HTTP 400 ValidationException with message `'X' must not be greater than 7`, instead of AWS's `'X' must be an integer in the range 0..2^Zoom-1 (0..7).`
3. After a fix, re-replay `location.get-map-tile-enforces-per-style-zoom-bound`; it also asserts that a tile at exactly the declared maximum zoom returns bytes and that one past it is refused with the full AWS sentence.
