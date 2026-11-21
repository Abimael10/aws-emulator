Issue-Source: trace-sweep-divergence
Issue-Trace: location-place-index-search-and-get-place
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: location-place-index-search-and-get-place
Issue-Shape-Id: com.amazonaws.location#LocationService
Issue-Key: location|SearchPlaceIndexForText|2xx->2xx|$.Results[*]
Issue-Key: location|SearchPlaceIndexForText|2xx->2xx|$.Summary.MaxResults
Issue-Key: location|GetPlace|2xx->2xx|$.Place.Categories
Issue-Key: location|GetPlace|4xx->4xx|$.fieldList[*]
Issue-Commit: 8482ca1a85e7d388da6ae1cfee3bd2042dc26182

# Location place-index Places plane diverges: Summary.MaxResults, Place.Categories and the GetPlace fieldList name

> _Note: this issue was identified against the code base at commit `8482ca1a85e7d388da6ae1cfee3bd2042dc26182`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SearchPlaceIndexForText

## Description
Three net-new causes on the Amazon Location Places (place index) read plane, all in the location crate: a text search's Summary drops the effective result limit, the projected Place carries no Amazon Location Categories (on both the search result and GetPlace), and the GetPlace invalid-identifier ValidationException names the wrong field in fieldList. All three are reached through the same fixture, location-place-index-search-and-get-place.

Cause 1 of 3: SearchPlaceIndexForText's Summary omits MaxResults when the request names none, and the reported result record does not describe the place AWS geocodes the query to (no Categories).
AWS: Smithy: SearchPlaceIndexForText POST /places/v0/indexes/{IndexName}/search/text, output Summary -> SearchPlaceIndexForTextSummary (required) with member MaxResults -> PlaceIndexSearchResultLimit (@range min 1 max 50), documented "Contains the optional result count limit specified in the request"; SearchPlaceIndexForTextRequest$MaxResults documents the default as 50. The recorded response carries Summary.MaxResults = 50 for a request that named no MaxResults (had the request carried it, the emulator would have echoed it), so AWS reports the EFFECTIVE limit in the summary — the request's value when it names one, otherwise the documented default — rather than omitting the member. Smithy: Results -> SearchForTextResultList of SearchForTextResult{Place (required), Distance, Relevance, PlaceId}; Place$Categories -> PlaceCategoryList (@length min 1 max 10) is "The Amazon Location categories that describe this Place", drawn from the published Amazon Location category list. In the recording the query resolves to a municipality-granularity record whose Place carries Categories ["MunicipalityType"], a Label/Geometry/PostalCode describing the city itself, SubRegion "King", and no Relevance member.
Emulator: 200 with Summary.MaxResults absent entirely, and Results[0].Place carrying NO Categories member at all, a street-address record (AddressNumber/Street/Neighborhood of an address inside the city) in place of the municipality the query names, plus a Relevance value. The absent Categories is structural, not data: crate::state::places::projected_place emits Categories only when the matched record's categories list is non-empty, and the stand-in dataset (crate::state::places::dataset) both leaves address-granularity records with an empty categories list and draws its vocabulary from its own lowercase CATEGORIES set ("airport", "coffee_shop", "hotel", "market", "monument", "museum", "park", "tourist_attraction") rather than from the Amazon Location category names AWS reports (e.g. the municipality category in the recording). So no geocode of a city can ever answer with the category AWS attaches to it.
Why: a client that reads Summary.MaxResults to learn the page size it was actually served (paging UIs, "showing N of up to M" text, assertions that a default limit applied) finds the member missing and must guess; a client that branches on Place.Categories — filtering points of interest by category, choosing an icon, or round-tripping the categories into a FilterCategories follow-up search — sees every place as uncategorised, so category-driven code paths are dead against the emulator. A client geocoding a city name also receives a record of a different granularity, so coordinates and postal code it stores for "the city" are a specific street address.
Fix: the SearchPlaceIndexForText handler's summary construction (set Summary.MaxResults to the effective limit, i.e. the clamped request value or the DEFAULT_SEARCH_RESULT_LIMIT it already computes for the result take), and the Places dataset/projection in the location crate's state (give records the Amazon Location category values AWS publishes, including municipality-granularity records, so the projection emits Categories).
Traces: location-place-index-search-and-get-place#task#2.

Cause 2 of 3: GetPlace omits Place.Categories for a place AWS classifies.
AWS: Smithy: GetPlace GET /places/v0/indexes/{IndexName}/places/{PlaceId}, output Place -> Place (required); Place$Categories -> PlaceCategoryList (@length 1..10), "The Amazon Location categories that describe this Place". The recorded answer for the resolved identifier carries Categories with the place's Amazon Location category (a municipality record: ["MunicipalityType"]); every other member of the recorded Place matches the emulator's, so this is the one omitted member.
Emulator: 200 with the Place body otherwise matching but no Categories member — the same root as cause 1, reached through the shared projection: crate::ops::get_place calls crate::state::places::projected_place, which emits Categories only for a record whose dataset categories list is non-empty.
Why: GetPlace is the documented way to re-read a place by the PlaceId a search handed out, and AWS reports the same Place both times. A client that resolves an identifier and then classifies the place by its Categories (icon, filter, analytics bucket) gets nothing, and a client comparing a search hit with its GetPlace re-read cannot detect the omission as a data difference.
Fix: the Places projection/dataset in the location crate's state (one statement shared by GetPlace and the search operations), so a record carries and projects its Amazon Location categories.
Traces: location-place-index-search-and-get-place#task#3.

Cause 3 of 3: the GetPlace invalid-identifier ValidationException names the modeled member in fieldList[].name where AWS names the URI path parameter.
AWS: Smithy: GetPlace input PlaceId -> PlaceId (@httpLabel, required, @sensitive); error ValidationException (httpError 400) whose FieldList (jsonName "fieldList") holds ValidationExceptionField entries with required Name (jsonName "name", "The field name where the invalid entry was detected") and Message. For an identifier this index cannot read, the recorded refusal is 400 with reason FieldValidationFailed and ONE fieldList entry whose name is the path parameter spelling "placeID" and whose message is "'PlaceId' must be a valid ID." — the message's fixed text is "' ... ' must be a valid ID." with the modeled member name 'PlaceId' quoted inside it, while the entry's name is the lowercase-first URI label spelling placeID. Both halves are fixed text for this error class; nothing in them comes from the request (PlaceId is @sensitive, so the rejected value never appears).
Emulator: 400 with the correct status, reason and message, but fieldList[0].name is "PlaceId" — the emulator reuses the modeled member name as the field name. The constant is crate::state::places::PLACE_ID_MEMBER ("PlaceId"), passed to crate::state::field_validation_exception from the GetPlace handler's record_of_place_id refusal.
Why: a client that keys validation handling off fieldList[].name — mapping the failure back to the request field it sent, or matching against AWS's own documented/observed field names in tests and error dashboards — matches nothing for AWS's "placeID" and so cannot attribute or group the refusal.
Fix: the GetPlace handler's ValidationException construction (the field-name constant it passes for the place-identifier refusal) in the location crate — report the field name AWS reports for this error, keeping the member spelling inside the message.
Traces: location-place-index-search-and-get-place#task#5.

Related but not covering these: open issue 20261029T234705Z-location-refusal-bodies-are-emulator-authored-and-omit covers Location refusals where reason and fieldList are ABSENT (GetMapGlyphs) and where message text is emulator-authored; here reason and message match and only the field NAME differs. Open issue 20261118T080214Z-location-putgeofence-ring-orientation-validationexception-names-the-indexed-ring covers the PutGeofence geometry validator stripping a list index from the field name — a different operation, a different validator and the opposite correction. Open issue 20261120T005521Z-location-describeplaceindex-answers-200-for-a-deleted-place covers the place-index control plane, not the Places read plane. None of them close any cause above.

## Evidence
Replay of fixture location-place-index-search-and-get-place against the emulator (trace set simparity-wire-aws-v1), three mismatch clusters:

1) task#2, SearchPlaceIndexForText, 200 both sides, cluster key service=location|SearchPlaceIndexForText|status=200->200|keys=$.Results[0],$.Summary.MaxResults:
   - $.Summary.MaxResults: missing — expected 50, got None (request named no MaxResults; AWS reports the effective/default limit).
   - $.Results[0]: value_mismatch — expected a municipality-granularity Place (Label "Seattle, WA, United States", Geometry.Point the city centre, Municipality "Seattle", SubRegion "King", PostalCode "98104", Interpolated false, TimeZone America/Los_Angeles, Categories ["MunicipalityType"], a PlaceId, and no Relevance); got a street-address Place (AddressNumber/Street/Neighborhood of an address in the city, SubRegion "King County", a different PostalCode, NO Categories member, Relevance 0.5). The structural divergence is the absent Categories member and the granularity of the record; the particular label text, coordinates and generated PlaceId of either side are dataset values, not requirements.

2) task#3, GetPlace, 200 both sides, cluster key service=location|GetPlace|status=200->200|keys=$.Place.Categories:
   - $.Place.Categories: missing — expected the place's Amazon Location category list (recorded: ["MunicipalityType"]), got None. Every other Place member matched, so the omission is the only difference.

3) task#5, GetPlace, 400 both sides with matching reason and message, cluster key service=location|GetPlace|status=400->400|keys=$.fieldList[0]:
   - $.fieldList[0]: expected {"name": "placeID", "message": "'PlaceId' must be a valid ID."}, got {"name": "PlaceId", "message": "'PlaceId' must be a valid ID."} (reported as missing+extra for the same entry). The message is byte-identical on both sides; only the entry's name differs.

Code corroboration in the checkout: crate::state::places::projected_place emits Categories only for a record with a non-empty categories list, and dataset.rs's CATEGORIES set is ["airport","coffee_shop","hotel","market","monument","museum","park","tourist_attraction"] — none of the Amazon Location category names AWS reports; SearchPlaceIndexForText's summary sets max_results: input.max_results (absent when the request omits it) although it already computes the default limit for the result take; GetPlace refuses an unresolvable identifier with field_validation_exception(places::PLACE_ID_MEMBER /* "PlaceId" */, PLACE_ID_NOT_VALID).

## Steps to Reproduce
1. Replay fixture location-place-index-search-and-get-place against the emulator (3 diverging steps: task#2, task#3, task#5).
2. Cause 1 — at task#2, call SearchPlaceIndexForText on an existing place index with free-form Text naming a city and NO MaxResults member. Observe the 200 body: Summary carries no MaxResults (AWS reports the effective limit, the documented default 50 when the request names none), and Results[0].Place carries no Categories member while describing a street address inside the city rather than the city AWS geocodes the query to.
3. Cause 2 — at task#3, GetPlace that index with the PlaceId the search returned. Observe the 200 body: the Place matches AWS member for member except that Categories is absent, where AWS reports the place's Amazon Location category list.
4. Cause 3 — at task#5, GetPlace the same index with a PlaceId that index's provider never minted. Observe the 400 ValidationException: status, reason (FieldValidationFailed) and message ("'PlaceId' must be a valid ID.") match AWS, but fieldList[0].name is "PlaceId" where AWS reports "placeID".
