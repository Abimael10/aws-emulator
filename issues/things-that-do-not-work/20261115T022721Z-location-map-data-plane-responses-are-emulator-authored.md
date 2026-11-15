Issue-Source: trace-sweep-divergence
Issue-Trace: location-map-data-plane-tile-sprites-glyphs-style
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: location-map-data-plane-tile-sprites-glyphs-style
Issue-Shape-Id: com.amazonaws.location#LocationService
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.created
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.glyphs
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.layers[*]
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.modified
Issue-Key: location|GetMapStyleDescriptor|2xx->2xx|$.sources.esri
Issue-Key: location|GetMapTile|2xx->2xx|$headers.content-type
Issue-Commit: f014434159c45996b81056da7988431a7dca5c01

# Location map data-plane responses are emulator-authored placeholders: style descriptor omits sources/glyphs/timestamps and GetMapTile mislabels Content-Type

> _Note: this issue was identified against the code base at commit `f014434159c45996b81056da7988431a7dca5c01`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMapStyleDescriptor

## Description
Two net-new causes on the Amazon Location Service Maps v1 data plane (service token `location`). Both are the same underlying fault: the map data-plane responses are emulator-authored placeholders rather than the provider-backed artifacts AWS serves, so a 200 is returned whose payload/labelling a map client cannot use. Neither cause is covered by an open issue; the open Location issues touch the control plane and refusal bodies only (`20261110T103416Z-location-deletemap-never-retires-the-map-describemap-still.md`, `20261111T061520Z-location-resource-tagging-plane-diverges-tagresource-untagresource-answer.md`, `20261029T234705Z-location-refusal-bodies-are-emulator-authored-and-omit.md`), not GetMapStyleDescriptor's document or GetMapTile's Content-Type.

Cause 1 of 2: GetMapStyleDescriptor serves a stub style document with no sources, no glyphs URL and no created/modified timestamps (fixture location-map-data-plane-tile-sprites-glyphs-style, step task#2).
AWS: GetMapStyleDescriptor GET /maps/v0/maps/{MapName}/style-descriptor returns 200 with the style document in the output's `Blob` member. Smithy: GetMapStyleDescriptorResponse.Blob @httpPayload, doc "Contains the body of the style descriptor"; operation doc "Style descriptors follow the Mapbox Style Specification"; errors ResourceNotFoundException(404)/ValidationException(400). The document's contents are provider style data, so recorded behavior only: for an Esri-backed map style the document carries (a) a `sources` object keyed by the data provider (`esri`) whose entry is `type: "vector"` with a `tiles` array holding the tile URL template for THIS map resource (the service's own maps endpoint + `/maps/v0/maps/<MapName>/tiles/{z}/{x}/{y}`), `minzoom`/`maxzoom` bounds and the provider `attribution` string; (b) a `glyphs` URL template for this map resource (`.../maps/v0/maps/<MapName>/glyphs/{fontstack}/{range}.pbf`); (c) the provider's layer catalog, whose first entry is a provider layer bound to the `esri` source (recorded first layer: a `fill` layer over source-layer `Land` with a `_symbol` filter), not a synthetic background layer; and (d) `created` and `modified` timestamps for the style. The host, region and map name in those URL templates are environment values, not requirements — the requirement is that the entries exist and that their templates address the emulator's own GetMapTile/GetMapGlyphs routes for the requested map.
Emulator: answers 200 but with a minimal authored document: `$.sources.esri` absent, `$.glyphs` absent, `$.created` absent, `$.modified` absent, and `$.layers[0]` is `{"id": "ground", "type": "background", "paint": {"background-color": "#e8e3d6"}}` instead of a provider layer bound to a declared source.
Why: a Mapbox-GL / MapLibre client loads the style descriptor and takes its tile and glyph endpoints from `sources` and `glyphs`. With neither present the client has no tile source and no font source, so it renders a flat background and never fetches a single tile — the map is silently blank even though every call returned 200. Missing `created`/`modified` also breaks clients that cache or version a style.
Fix: the Location Maps data-plane GetMapStyleDescriptor handler and the style-document builder behind it (and the map-resource state that must record the style's created/modified instants and the configured style name/data provider), which must emit a provider style keyed to the stored MapConfiguration.Style with source and glyph templates derived from the serving endpoint and the requested MapName.
Traces: location-map-data-plane-tile-sprites-glyphs-style#task#2.

Cause 2 of 2: GetMapTile labels the tile payload with a non-AWS Content-Type (fixture location-map-data-plane-tile-sprites-glyphs-style, step task#3).
AWS: GetMapTile GET /maps/v0/maps/{MapName}/tiles/{Z}/{X}/{Y} returns 200 with the tile bytes in `Blob` @httpPayload and the media type echoed in the `ContentType` member bound to the `Content-Type` header (Smithy: GetMapTileResponse.ContentType @httpHeader("Content-Type")). The model does not fix a value, so recorded behavior only: the recorded 200 carries `content-type: application/octet-stream` for the vector tile.
Emulator: returns 200 with `content-type: application/vnd.mapbox-vector-tile`.
Why: the Content-Type is part of the operation's wire contract and is surfaced to callers as the output's ContentType member. A client, proxy or cache that branches on the declared media type (or that asserts the value the SDK hands back) sees a type AWS never sends for this operation, so tile handling and any content-type-keyed caching diverge from production.
Fix: the Location Maps data-plane GetMapTile response serializer, which must set the Content-Type header (and the corresponding ContentType output member) to the media type AWS serves for v1 map tiles rather than a locally chosen vector-tile type.
Traces: location-map-data-plane-tile-sprites-glyphs-style#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200).

Cause 1 — GetMapStyleDescriptor, fixture location-map-data-plane-tile-sprites-glyphs-style step task#2, expected status 200 / actual 200, body diverges:
  $.sources.esri: missing (expected {'attribution': "<a style='color:#0073bb;' href='https://www.esri.com/en-us/legal/terms/data-attributions'>Esri</a>, TomTom, Garmin, FAO, NOAA, USGS, \u00a9 OpenStreetMap", 'maxzoom': 15, 'minzoom': 0, 'tiles': ['<maps endpoint>/maps/v0/maps/<MapName>/tiles/{z}/{x}/{y}'], 'type': 'vector'}, got None)
  $.glyphs: missing (expected '<maps endpoint>/maps/v0/maps/<MapName>/glyphs/{fontstack}/{range}.pbf', got None)
  $.layers[0]: value_mismatch (expected a provider layer {'filter': ['==', '_symbol', 0], 'id': 'Land/Not ice', 'layout': {}, 'minzoom': 0, 'paint': {'fill-color': '#f7f6d5'}, 'source': 'esri', 'source-layer': 'Land', 'type': 'fill'}, got {'id': 'ground', 'type': 'background', 'paint': {'background-color': '#e8e3d6'}})
  $.created: missing (expected a style creation timestamp, got None)
  $.modified: missing (expected a style modification timestamp, got None)
The recorded host (maps.geo.us-west-2.amazonaws.com), region and generated map name are per-environment values; the requirement is the presence of the sources/glyphs/created/modified entries and a source-bound first layer.

Cause 2 — GetMapTile, fixture location-map-data-plane-tile-sprites-glyphs-style step task#3, expected status 200 / actual 200, headers diverge:
  $headers.content-type: value_mismatch (expected 'application/octet-stream', got 'application/vnd.mapbox-vector-tile')

Smithy model consulted: services/service-com-amazonaws-location-locationservice/smithy-model/com-amazonaws-location-locationservice.json — GetMapStyleDescriptor (uri /maps/v0/maps/{MapName}/style-descriptor, output Blob @httpPayload + ContentType/CacheControl @httpHeader) and GetMapTile (uri /maps/v0/maps/{MapName}/tiles/{Z}/{X}/{Y}, output Blob @httpPayload + ContentType @httpHeader("Content-Type")).

## Steps to Reproduce
1. Boot the emulator from this checkout and replay trace set simparity-wire-aws-v1.
2. Cause 1: replay fixture `location-map-data-plane-tile-sprites-glyphs-style`, step `task#2` — create a map resource (CreateMap with a vector Esri style), then GET /maps/v0/maps/<MapName>/style-descriptor. Observe 200 whose JSON body has no `sources` object, no `glyphs` template, no `created`/`modified`, and whose `layers[0]` is a synthetic `background` layer rather than a layer bound to a declared vector source.
3. Cause 2: same fixture, step `task#3` — GET /maps/v0/maps/<MapName>/tiles/0/0/0. Observe the 200 response's `content-type` header is `application/vnd.mapbox-vector-tile` instead of `application/octet-stream`.
4. Both steps are in one fixture, so re-replaying `location-map-data-plane-tile-sprites-glyphs-style` exercises both causes.
