Issue-Source: trace-sweep-divergence
Issue-Trace: geo-places-reverse-geocode-political-view-changes-country-code
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: geo-places-reverse-geocode-political-view-changes-country-code
Issue-Shape-Id: com.amazonaws.geoplaces#PlacesService
Issue-Key: geo-places|ReverseGeocode|2xx->2xx|$.ResultItems[*].PlaceId
Issue-Commit: cda032ca99efd252a540b47900271345a944d5a8

# ReverseGeocode returns the default-view PlaceId when PoliticalView is requested

> _Note: this issue was identified against the code base at commit `cda032ca99efd252a540b47900271345a944d5a8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ReverseGeocode

## Description
Cause 1 of 1: a ReverseGeocode result served under a `PoliticalView` carries the place's default-view `PlaceId` instead of the handle that identifies the place AS ATTRIBUTED UNDER THAT VIEW.

AWS: Smithy (services/service-com-amazonaws-geoplaces-placesservice/smithy-model/com-amazonaws-geoplaces-placesservice.json) — `ReverseGeocode` POST /v2/reverse-geocode, 200; input member `ReverseGeocodeRequest$PoliticalView` (target `CountryCode`, @length 2..3, @pattern ^([A-Z]{2}|[A-Z]{3})$, @sensitive) documented as "The alpha-2 or alpha-3 character code for the political view of a country. The political view applies to the results of the request to represent unresolved territorial claims through the point of view of the specified country"; output member `ReverseGeocodeResultItem$PlaceId` is documented only as "The PlaceId of the place you wish to receive the information for". The model therefore says nothing about the token's content, so this is recorded behavior only: in the recording, reverse-geocoding one and the same `QueryPosition` returns one `PlaceId` for the default view and a DIFFERENT, equally stable `PlaceId` once `PoliticalView` is supplied — the view-specific result is a distinct place record with its own handle, which GetPlace resolves back to that view's attribution. The handle is stable, not per-run: every other PlaceId in this corpus (the default-view steps) matched the recording byte-for-byte, so PlaceId is part of the recorded contract and only the view-specific one diverges. The response status is 200 in both the recording and the emulator; no error is involved.

Emulator: the political view is modeled as an alternative ADDRESS only, never as a distinct handle. `PlaceRecord` carries `political_attributions: BTreeMap<String, AddressRecord>` and a single per-row `place_id`; `address_in`/`address_modeled_in` select the view's `AddressRecord`, so Address/Country code DO change under the view (that part of the step passes), but the handler projects `place_id: place.place_id.clone()` unconditionally. The emulator consequently answers a `PoliticalView` request with the default-view handle, i.e. the same `PlaceId` it returns when no view is requested. Replay reports `$.ResultItems[0].PlaceId: value_mismatch` — expected the recording's view-specific handle `AQAAADgAaka_5tvGNEUE87w-SQyqJwsExU8izfcRz4f1-vNHdwoqACA5oTDsEA4mw55RSWwL1b4uAWrvgSPF4FcSFMAvDiCVirKjrJnNJgq3hbLIDEmRF17D1DPh6g`, got the catalog row's default handle `AQAAADgApjjt2WZrCL3y4TIpZfYohfzY_CnvR89lM4pJoXwZmFlTtTdjHN-fQ2xZcNKgU6zd_-jLCssEJSvYtSiIjj-Tgop4nlezkfb8Zt8Lf5EW7b6IVOG2E_UjKA` (the literal values are evidence of the recorded run, not the requirement; the requirement is that a view-attributed result carry a handle distinct from — and not equal to — the same place's default-view handle, stable across runs and resolvable through GetPlace to that view's attribution).

Why: a client relying on AWS's behavior treats `PlaceId` as the identity of the result it got. Against AWS, persisting the handle from a `PoliticalView=<cc>` reverse-geocode and later calling GetPlace with it re-fetches the place under that view's attribution, and de-duplicating results by `PlaceId` keeps the two attributions of a disputed place apart. Against the emulator the handle collapses onto the default-view record, so the round-trip silently loses the political view (GetPlace answers with the default country/region), and any cache or de-duplication keyed on `PlaceId` conflates results obtained under different views as one place. Code that asserts the handle differs per view — the exact behavior this fixture exercises — fails only against the emulator.

Fix: the handle belongs in the place catalog's state, not in the serializer. `PlaceRecord` needs a per-political-view handle alongside the per-view `AddressRecord` it already carries (so a view-attributed row has its own stable `PlaceId`, derived the same way as every other handle so it stays in one token class), the record's lookup index must resolve those view handles back to the view's attribution, and every operation that projects a record under a political view — ReverseGeocode's handler here, and the same projection in the other Places operations that accept `PoliticalView` — must hand out the view's handle rather than the row's default one.

Traces: geo-places-reverse-geocode-political-view-changes-country-code#task#2 (the only failing step; the default-view step of the same fixture passes, which is what isolates the view-specific handle as the gap). Relevant source: services/service-com-amazonaws-geoplaces-placesservice/src/ops/reverse_geocode.rs:278 (`place_id: place.place_id.clone()`), services/service-com-amazonaws-geoplaces-placesservice/src/state.rs:1419 (single per-row `place_id`), :1442 (`political_attributions` holds only an `AddressRecord`), :1559 (`with_political_attribution`), :1700 (`address_in`), :2171 (`synthesize_place_id`). No existing open issue covers geo-places.

## Evidence
Replay divergence (verdict: mismatch, service geo-places, operation ReverseGeocode, fixture geo-places-reverse-geocode-political-view-changes-country-code, step task#2):
  expected_status 200, actual_status 200 — the status is correct.
  detail: $.ResultItems[0].PlaceId: value_mismatch
    expected 'AQAAADgAaka_5tvGNEUE87w-SQyqJwsExU8izfcRz4f1-vNHdwoqACA5oTDsEA4mw55RSWwL1b4uAWrvgSPF4FcSFMAvDiCVirKjrJnNJgq3hbLIDEmRF17D1DPh6g'
    got      'AQAAADgApjjt2WZrCL3y4TIpZfYohfzY_CnvR89lM4pJoXwZmFlTtTdjHN-fQ2xZcNKgU6zd_-jLCssEJSvYtSiIjj-Tgop4nlezkfb8Zt8Lf5EW7b6IVOG2E_UjKA'
  divergence_key: service=geo-places|ReverseGeocode|status=200->200|keys=$.ResultItems[0].PlaceId

The 'got' token is verbatim the default handle seeded on the catalog row for the queried locality (state.rs:4354, the row that also declares .with_political_attribution("RUS", ...)), which shows the emulator served the record's default-view handle rather than a view-specific one. The other ResultItems members in the same step — including the view-dependent country code — compared equal, and the fixture's default-view step passed with its PlaceId matching, so PlaceId is deterministic in this corpus and only the political-view handle diverges.

Expected behavior: the PlaceId of a result returned under PoliticalView is a distinct, run-stable opaque handle for the place under that view's attribution, not equal to the same place's default-view PlaceId, and resolvable through GetPlace to that view's attribution. Actual behavior: it equals the default-view PlaceId.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus); fixture geo-places-reverse-geocode-political-view-changes-country-code, step task#2 reports the mismatch.
2. Manually: POST /v2/reverse-geocode to the geo-places endpoint with a QueryPosition over a locality whose territorial claim is unresolved (the seeded Ukrainian locality at roughly [33.85939, 45.7142], which carries a political attribution for view 'RUS'), with NO PoliticalView. Note ResultItems[0].PlaceId.
3. Repeat the identical request with PoliticalView set to that view's alpha-3 code (e.g. "RUS"). The Address/country code change as expected, but ResultItems[0].PlaceId is byte-identical to the value from step 2, whereas the recording returns a different handle for the view-attributed result.
4. Optionally GET /v2/place/{PlaceId} with the handle from step 3: the emulator returns the default-view attribution, because the handle is the default-view record's.
