Issue-Source: trace-sweep-divergence
Issue-Trace: location-geofence-put-get-list-and-not-found
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: location-geofence-put-get-list-and-not-found
Issue-Shape-Id: com.amazonaws.location#LocationService
Issue-Key: location|PutGeofence|4xx->4xx|$.fieldList[*]
Issue-Commit: bbae9c76ca51adc37e878fdc58411b8d5ef93114

# PutGeofence ring-orientation ValidationException names the indexed ring in fieldList[].name instead of the member

> _Note: this issue was identified against the code base at commit `bbae9c76ca51adc37e878fdc58411b8d5ef93114`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutGeofence

## Description
One cause: the PutGeofence polygon-orientation refusal populates ValidationException.FieldList, but it fills the entry's Name with the indexed ring path taken from the message rather than with the input member whose value was rejected.

Cause 1 of 1: ValidationException fieldList[0].name carries the ring index.
AWS: Smithy: PutGeofence PUT /geofencing/v0/collections/{CollectionName}/geofences/{GeofenceId}, input member Geometry -> GeofenceGeometry, whose Polygon member is a LinearRings list (@length min 1 max 250) documented as "The vertices for the exterior ring must be listed in counter-clockwise sequence. Vertices for all interior rings must be listed in clockwise sequence." Error ValidationException (httpError 400) has three required members: Message (jsonName "message"), Reason (jsonName "reason", enum ValidationExceptionReason) and FieldList (jsonName "fieldList", list of ValidationExceptionField with required Name (jsonName "name") and Message (jsonName "message"), documented as "The field name where the invalid entry was detected"). A polygon whose exterior ring is wound the wrong way answers 400 with one fieldList entry whose name is the INPUT MEMBER PATH, without a list index — fixed text "Geometry.Polygon" — while that entry's message is the per-ring sentence "'Geometry.Polygon[<i>]' must be oriented counter-clockwise." (fixed text "' ... ' must be oriented counter-clockwise." with the bracketed ring ordinal taken from the request's polygon). So the index appears in the message only; name identifies the member, as the model documents it.
Emulator: 400 with the correct status, reason and message, but fieldList[0].name is the indexed path "Geometry.Polygon[<i>]" — the emulator reuses the message's subject string as the field name instead of the member name "Geometry.Polygon".
Why: a client that groups or indexes validation failures by fieldList[].name — highlighting the offending input member in a form, or matching the name against the request's member path — finds no member called "Geometry.Polygon[0]" and cannot attribute the failure; per-ring entries also fail to collapse onto the one member AWS reports, so retry/repair logic keyed on the field name misses it.
Fix: the location crate's ValidationException construction for the PutGeofence geometry validator (the geofence geometry/orientation validation path) — set ValidationExceptionField.Name to the input member path with list indices stripped, and keep the index only inside the entry's Message.
Traces: location-geofence-put-get-list-and-not-found#task#5.

Related but not covering this: open issue 20261029T234705Z-location-refusal-bodies-are-emulator-authored-and-omit covers refusals where reason and fieldList are ABSENT entirely (GetMapGlyphs) and where the message text is emulator-authored; here both members are present and the message matches — only the field name's shape is wrong, so that issue does not close this.

## Evidence
location-geofence-put-get-list-and-not-found#task#5 (PutGeofence, 400 both sides, reason and message matching): $.fieldList[0] expected {"name": "Geometry.Polygon", "message": "'Geometry.Polygon[0]' must be oriented counter-clockwise."}, got {"name": "Geometry.Polygon[0]", "message": "'Geometry.Polygon[0]' must be oriented counter-clockwise."}. The entry's message is identical on both sides; only name differs, carrying the ring index that belongs to the message alone. Reported by the replay as $.fieldList[0] missing/extra for the same cluster (service=location|PutGeofence|status=400->400|keys=$.fieldList[0]).

## Steps to Reproduce
1. Replay fixture location-geofence-put-get-list-and-not-found against the emulator.
2. At task#5, PutGeofence into an existing geofence collection with a Geometry.Polygon whose exterior linear ring is wound clockwise (not counter-clockwise).
3. Observe the 400 ValidationException body: reason is FieldValidationFailed and message matches, but fieldList[0].name is "Geometry.Polygon[0]" where AWS reports the member "Geometry.Polygon" (the index belongs only in fieldList[0].message).
