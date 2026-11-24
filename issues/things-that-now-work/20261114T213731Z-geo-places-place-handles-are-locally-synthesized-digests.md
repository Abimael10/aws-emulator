Issue-Source: trace-sweep-divergence
Issue-Trace: geo-places-geocode-include-place-types-filter
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-places-geocode-include-place-types-filter geo-places-geocode-query-components-structured-search geo-places-reverse-geocode-political-view-changes-country-code
Issue-Shape-Id: com.amazonaws.geoplaces#PlacesService
Issue-Key: geo-places|Geocode|2xx->2xx|$.ResultItems[*].PlaceId
Issue-Key: geo-places|ReverseGeocode|2xx->2xx|$.ResultItems[*].PlaceId
Issue-Commit: d3b7765ce05e53288156df4eb42460237bf11326

# geo-places place handles are locally synthesized digests instead of the recorded AWS PlaceId

> _Note: this issue was identified against the code base at commit `d3b7765ce05e53288156df4eb42460237bf11326`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Geocode

## Description
ONE root cause, reached through TWO divergence clusters (4 replayed steps across 3 fixtures). Both clusters must be re-replayed and both must pass before this issue is closed — fixing only the Geocode path leaves the ReverseGeocode cluster failing.

Cause 1 of 2 — Geocode.ResultItems[].PlaceId (fixtures geo-places-geocode-include-place-types-filter#task#1 and geo-places-geocode-query-components-structured-search#task#1).
Cause 2 of 2 — ReverseGeocode.ResultItems[].PlaceId (fixture geo-places-reverse-geocode-political-view-changes-country-code, steps task#1 and task#2).

AWS: Smithy models PlaceId as GeocodeResultItem$PlaceId / ReverseGeocodeResultItem$PlaceId -> SensitiveString, @length(0,500), documented only as "The PlaceId of the place result", and GetPlace binds the very same token as the @httpLabel of GET /v2/place/{PlaceId} (GetPlaceRequest$PlaceId, @required, @length(max 500)). The model says nothing about the token's content, so the requirement is recorded behavior only: the handle is a property OF THE PLACE — every recorded response that returns a given place (whatever operation served it, whatever Filter, QueryComponents or PoliticalView the request carried) carries that place's one handle, a ~126-character base64url token opening with the service's fixed version prefix. The recordings in this corpus return the same Title, Address, Position, PlaceType and MapView the emulator already serves for these places, so the catalog clearly carries the recorded place data; the handle is the one property of those records that is not the place's own.

Emulator: for all four diverging steps the emulator answers 200 with every other response member matching, and diverges ONLY on $.ResultItems[0].PlaceId. The handle is not the place's handle at all — it is computed locally in services/service-com-amazonaws-geoplaces-placesservice/src/state.rs by synthesize_place_id(title, position): the fixed 6-byte PLACE_ID_VERSION header (which is why the emulator's token still opens with the same prefix characters as AWS's) followed by 88 bytes of SHA-256 over the seed string "<title>|<lat>|<lon>", rendered base64url. So the prefix and the length class match AWS and everything behind the prefix is a digest of the emulator's own catalog row. PlaceRecord::new assigns it, so EVERY place-returning operation (Geocode, ReverseGeocode, SearchText, SearchNearby, Autocomplete, Suggest, GetPlace) hands out the synthesized handle, not just the two operations the corpus exercises.

Why: a PlaceId is the one durable thing a Places client keeps. Clients persist it (saved places, favourites, de-duplication keys), log it, and hand it back to GetPlace later, and AWS examples/tests carry literal PlaceIds captured from the real service. Against this emulator none of those resolve: GetPlace looks the token up with state.place(place_id) and, on a miss, refuses with 400 ValidationException, Reason CannotParse and a FieldList entry naming "PlaceId" (src/ops/get_place.rs lines 256-269) — i.e. a handle AWS issued for a place the emulator serves is reported as unparseable. Conversely a handle the emulator issues is meaningless against AWS, so a recorded/cross-checked workflow cannot be replayed in either direction, and any assertion or golden file keyed on a place's handle fails even though the place data itself is right. Note the ids are stable across emulator runs, so this is a data-fidelity gap, not a nondeterminism bug: the round trip inside the emulator does work.

Fix: the catalog seed and the handle derivation in the service's state, not the operation handlers — the handlers already project place.place_id unchanged. Carry each catalog record's real handle as seeded place data, the same way its Title, Address and Position are carried, and let synthesize_place_id remain only the fallback for a record that has no recorded handle. Keep the handle one per place record and keep it independent of the request (Filter, QueryComponents, PoliticalView, Language), so one operation's handle still resolves through GetPlace, which is the invariant the state module's own header documents.

Traces: geo-places-geocode-include-place-types-filter#task#1 (representative, Geocode), geo-places-geocode-query-components-structured-search#task#1 (same cluster), geo-places-reverse-geocode-political-view-changes-country-code#task#1 and #task#2 (ReverseGeocode cluster).

## Evidence
Replay of simparity-wire-aws-v1: 5 steps, 1 passed, 4 mismatched (score 20%), emulator_up=true, bootstrap_signal=false, 0 blocked/errored. Both clusters are verdict=mismatch with expected_status 200 and actual_status 200 — only the handle differs.

Cluster 1, divergence_key service=geo-places|Geocode|status=200->200|keys=$.ResultItems[0].PlaceId, fixture geo-places-geocode-include-place-types-filter step task#1 (also geo-places-geocode-query-components-structured-search#task#1):
  $.ResultItems[0].PlaceId: value_mismatch
  expected 'AQAAADgAS4dlANlTcJAwxzkQeY4KmDsNP_-mbI9KgIrdgBJy84gfbsSLbZGuZFNi0t5RW4yDS2sWLBA3xaF0T_lgjPYHVrFMDllob_vUqyz9hUkyF68F5Y5-4DE66Q'
  got      'AQAAADgADv-iRi6gce5gCWxg37T01jzsVtSGP4Xnf8oZpDhxzQHtWd4azGvJOGGmD3MZQmYG5sqgDWcGBXG-XIOSDYH6BJmnbOMYPBg_0a8om03ygon9-XSt7PViFg'

Cluster 2, divergence_key service=geo-places|ReverseGeocode|status=200->200|keys=$.ResultItems[0].PlaceId, fixture geo-places-reverse-geocode-political-view-changes-country-code steps task#1 and task#2:
  $.ResultItems[0].PlaceId: value_mismatch
  expected 'AQAAADgApjjt2WZrCL3y4TIpZfYohfzY_CnvR89lM4pJoXwZmFlTtTdjHN-fQ2xZcNKgU6zd_-jLCssEJSvYtSiIjj-Tgop4nlezkfb8Zt8Lf5EW7b6IVOG2E_UjKA'
  got      'AQAAADgANOurlXZm0cmDbzwiVQ8rrS8KRyz3zd6N57EB0PiJGaGyl4DsdPgBLd6f2iQYt2VAPIsnjQgu859k9JLBkUShy1IG7bt1RK3IyXiTluKR12rr7h7Dhfb8HA'

In both clusters the expected and actual tokens share the fixed leading characters 'AQAAADgA' (the 6-byte PLACE_ID_VERSION header, state.rs line 2117) and the same ~126-character length class, and differ in the whole payload behind the header — the payload the emulator fills with SHA-256 blocks over "<title>|<lat>|<lon>" (state.rs synthesize_place_id, lines 2148-2163; assigned in PlaceRecord::new, line 1487). No other response member of any of the four steps diverged, and no status or error code diverged. Fixture geo-places-get-place-* style steps that do not report a handle are the single passing step.

Corroborating emulator source: src/state.rs module header states the catalog is seeded deterministically and that "a PlaceId handed out by Autocomplete, Geocode, SearchText, SearchNearby or ReverseGeocode resolves through GetPlace to the same Title and Address" — true inside the emulator, but the handle it resolves is the synthesized one. src/ops/get_place.rs lines 256-269 is the refusal an AWS-issued handle hits: 400 ValidationException, Reason CannotParse, field "PlaceId".

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the simparity-wire-aws-v1 wire corpus (replay_wire_corpus).
2. Cluster 1: replay fixture geo-places-geocode-include-place-types-filter, step task#1 — POST /v2/geocode for the recorded query. Compare $.ResultItems[0].PlaceId with the recorded response: status is 200 and every other member matches, the handle does not. Same for geo-places-geocode-query-components-structured-search#task#1 (POST /v2/geocode with QueryComponents).
3. Cluster 2: replay fixture geo-places-reverse-geocode-political-view-changes-country-code, steps task#1 and task#2 — POST /v2/reverse-geocode for the recorded QueryPosition under each PoliticalView. Both steps answer 200 and diverge on $.ResultItems[0].PlaceId only.
4. Round-trip check: take the PlaceId the recording expects for any of those steps and call GET /v2/place/{PlaceId} against the emulator. AWS resolves it to that place; the emulator refuses with 400 ValidationException, Reason CannotParse, FieldList naming "PlaceId".
5. Stability check (shows this is fidelity, not nondeterminism): restart the emulator and replay step 2 — the emulator returns the same synthesized handle again, still not the recorded one.
