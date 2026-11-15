Issue-Source: trace-sweep-divergence
Issue-Trace: location-map-data-plane-tile-sprites-glyphs-style
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: location-map-data-plane-tile-sprites-glyphs-style
Issue-Shape-Id: com.amazonaws.location#LocationService
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.created
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.layers[*]
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.modified
Issue-Commit: 03beaf84322f581b75af082edd0eeb6cd376c02b

# Location GetMapStyleDescriptor serves an emulator-synthesized style document instead of the map's published style

> _Note: this issue was identified against the code base at commit `03beaf84322f581b75af082edd0eeb6cd376c02b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMapStyleDescriptor

## Description
GetMapStyleDescriptor must return, verbatim, the MapLibre GL style document published for the map resource's configured style. The emulator instead composes a small style document of its own at request time, so both the layer definitions and the document's createdbcmodified stamps are emulator-invented. One divergence cluster, one root cause with two observable symptom groups (layer document, document timestamps).

=== Cause 1 of 1: the style-descriptor document is generated at request time rather than served from the map's stored published style ===
AWS: Smithy (services/service-com-amazonaws-location-locationservice/smithy-model/com-amazonaws-location-locationservice.json) binds GetMapStyleDescriptor to GET /maps/v0/maps/{MapName}/style-descriptor, method GET, code 200 (defaulted), readonly, hostPrefix "maps."; input MapName (@httpLabel, required, @length 1-100, @pattern ^[-._\w]+$) and Key (@httpQuery "key", sensitive); output Blob (@httpPayload) plus Content-Type and Cache-Control headers; errors AccessDeniedException(403), InternalServerException(500), ResourceNotFoundException(404), ThrottlingException(429), ValidationException(400). The model declares the body only as an opaque Blob and says nothing about its internal structure, so for the document contents this is RECORDED BEHAVIOR ONLY. Recorded behavior: 200 with the published style document for the map's configured style (an Esri-sourced style, source "esri"). Each layer entry is the published layer verbatim — it carries the published layer `id` (a slash-namespaced display name, e.g. a Land layer named with the published "<Layer>/<sub-class>" form), the published `source-layer` (capitalised exactly as the published tile schema spells it, e.g. "Land"), the published `type`, the published `paint` values (the published fill colour), the published `minzoom`, and ALSO the members the published style sets that the emulator omits entirely: `filter` (the published symbol/class predicate, e.g. an ["==", "_symbol", <n>] class selector) and `layout` (present, possibly as an empty object). The document's `created` and `modified` members are the published style's own stamps and are the zero-value timestamp "0001-01-01T00:00:00Z" — a FIXED constant of the published asset, identical on every call, in every region and account, NOT a per-run clock value.
Emulator: 200, correct status and headers, but a different document. Its first layer is a synthesized layer: a lowercased single-token `id` ("land" where the published layer is "Land/Not ice"), a lowercased `source-layer` ("land" where the published schema uses "Land"), a different `paint.fill-color` (#e8e3d6 where the published style sets #f7f6d5), and NO `filter` and NO `layout` member at all. It also replaces the published `created`/`modified` sentinel with the emulator's own wall clock at the moment the response is built (the replay observed a current-date ISO-8601 instant with milliseconds in both members), so the two members are not merely a different constant — they are not stable across runs.
Why: the style descriptor is the ONLY thing that tells a MapLibre/MapBox GL renderer what to draw. A lowercased `source-layer` does not match the layer names in the vector tiles GetMapTile serves, so that layer silently renders nothing; a missing `filter` makes one layer paint every feature class the source layer contains instead of the published sub-class, flooding the map; a missing `layout` breaks clients that read layout properties without a presence check; and the wrong fill colour makes a rendered basemap visibly differ from AWS's. The invented `created`/`modified` stamps additionally break any client that caches or ETags the style on those values or asserts the document against the published asset: AWS's style descriptor is a static document whose stamps never move, and the emulator's move on every request.
Fix: the GetMapStyleDescriptor handler for this service and the style asset in its state — the handler must look up and emit the STORED published style document for the map's configured style (keyed by the map's style name), byte-for-byte including every layer's id/source-layer/type/filter/layout/paint/minzoom and the published `created`/`modified` values, instead of building a style document in code at request time. It must not stamp the response with the current time.
Traces: location-map-data-plane-tile-sprites-glyphs-style#task#2 (this issue's representative and only failing step).

ADJACENT OPEN ISSUES THAT DO NOT COVER THIS: 20261115T073241Z-location-getmaptile-out-of-range-refusal-sentences-are.md (GetMapTile refusal wording), 20261029T234705Z-location-refusal-bodies-are-emulator-authored-and-omit.md (ValidationException Reason/FieldList bodies, which owns the GetMapGlyphs cluster in this same fixture) and 20261107T065214Z-maps-geo-getsprites-sprites-json-index-is-a-synthesized-uniform.md (a DIFFERENT service, geomaps/maps.geo, GetSprites) are all distinct faults; the replay reports owned_by=[] for this cluster, so no open issue carries its replay key.

## Evidence
Wire replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up=true, bootstrap_signal=false, 12 fixtures replayed, steps_total=83, steps_passed=52, steps_mismatched=30, steps_blocked=1, 7 divergence clusters, score_pct=62.7. All 7 clusters are service=location. No cluster carries a stub_path. Six clusters report a non-empty owned_by and are excluded. This cluster reports owned_by=[].

Cluster: fixture location-map-data-plane-tile-sprites-glyphs-style, step task#2, operation GetMapStyleDescriptor, verdict mismatch, expected_status 200 / actual_status 200, affected_count 1, divergence_key service=location|GetMapStyleDescriptor|status=200->200|keys=$.created,$.layers[0],$.modified.

Reported field-level divergences (expected vs actual):
  $.layers[0]: value_mismatch
    expected {"filter": ["==", "_symbol", 0], "id": "Land/Not ice", "layout": {}, "minzoom": 0, "paint": {"fill-color": "#f7f6d5"}, "source": "esri", "source-layer": "Land", "type": "fill"}
    actual   {"id": "land", "source": "esri", "source-layer": "land", "type": "fill", "paint": {"fill-color": "#e8e3d6"}, "minzoom": 0}
    -> `filter` absent, `layout` absent, `id` and `source-layer` lowercased/de-namespaced, `paint.fill-color` different. Only `source`, `type` and `minzoom` agree.
  $.created: value_mismatch, expected "0001-01-01T00:00:00Z", actual a current wall-clock instant (the replay observed "2026-11-15T20:25:36.261Z").
  $.modified: value_mismatch, expected "0001-01-01T00:00:00Z", actual the same current wall-clock instant.
The expected value of `created`/`modified` is a fixed zero-value sentinel belonging to the published asset, so this is not a per-run-value difference: the requirement is the constant, and the emulator substitutes a value that changes every run.

Status, Content-Type and Cache-Control match; the divergence is entirely in the style document.

Model evidence (Smithy, services/service-com-amazonaws-location-locationservice/smithy-model/com-amazonaws-location-locationservice.json): GetMapStyleDescriptor -> GET /maps/v0/maps/{MapName}/style-descriptor, code 200 (defaulted), readonly, endpoint hostPrefix "maps."; input {MapName: @httpLabel required @length 1-100 @pattern ^[-._\w]+$, Key: @httpQuery "key" @length max 1000 sensitive}; output {Blob: @httpPayload, ContentType: @httpHeader Content-Type, CacheControl: @httpHeader Cache-Control}; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. The payload is an opaque Blob, so the document's contents are asserted from the recording only.

## Steps to Reproduce
1. git clone the repo and build/boot the emulator from this checkout (/__admin/health answers 200 and the data listener accepts; the location service answers).
2. Replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus). 7 divergence clusters, all service=location; this one is fixture location-map-data-plane-tile-sprites-glyphs-style, step task#2, and is the only cluster with owned_by=[] and no stub_path.
3. Replay that fixture up to step task#2: create the map resource the fixture creates (with its configured Esri style), then issue GET /maps/v0/maps/{MapName}/style-descriptor.
4. The emulator answers 200 with a style document whose first layer is {"id":"land","source":"esri","source-layer":"land","type":"fill","paint":{"fill-color":"#e8e3d6"},"minzoom":0} and whose `created`/`modified` are the current time.
5. Diff it against the recorded response: the published layer is {"id":"Land/Not ice","source":"esri","source-layer":"Land","type":"fill","filter":["==","_symbol",0],"layout":{},"paint":{"fill-color":"#f7f6d5"},"minzoom":0} and `created`/`modified` are both "0001-01-01T00:00:00Z".
6. Call the same endpoint twice and observe the emulator's `created`/`modified` change between calls, while AWS's are a fixed constant.
7. This step must pass (layers[0] equal to the published layer including `filter` and `layout`, and both timestamps equal to the published sentinel) before the issue is closed.
