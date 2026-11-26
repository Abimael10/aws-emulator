Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: guardduty.create-ipset-malformed-s3-location-rejected guardduty.ipset-name-unicode-round-trip guardduty.ipset-unicode-name-round-trip guardduty.threat-intel-set-location-unicode-round-trip
Issue-Shape-Id: com.amazonaws.guardduty#GuardDutyAPIService
Issue-Key: guardduty.create-ipset-malformed-s3-location-rejected|run#16|AssertionError|assert got_location == good_location, (
Issue-Key: guardduty.ipset-name-unicode-round-trip|run#21|AssertionError|assert got["Location"] == location, (got["Location"], location)
Issue-Key: guardduty.ipset-unicode-name-round-trip|run#12|AssertionError|assert got["Location"] == location, (got.get("Location"), location)
Issue-Key: guardduty.threat-intel-set-location-unicode-round-trip|run#21|AssertionError|assert got["Location"] == location, (got["Location"], location)
Issue-Commit: ce750c7fb3d5c045e40deaa1b044dd5f90f97c10

# GetIPSet/GetThreatIntelSet rewrite the stored Location into an emulator-host URL instead of round-tripping the value the create stored

> _Note: this issue was identified against the code base at commit `ce750c7fb3d5c045e40deaa1b044dd5f90f97c10`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetIPSet

## Description
This issue carries 1 net-new root cause in the guardduty service, grounded in 4 failing behavioral checks that all fail on the same assertion.

Cause 1 of 1: every uploaded-list read-back projects `location` through a host-rewriting helper, so the value a caller gets back is not the value CreateIPSet / CreateThreatIntelSet accepted and stored. Failing checks: guardduty.create-ipset-malformed-s3-location-rejected, guardduty.ipset-name-unicode-round-trip, guardduty.ipset-unicode-name-round-trip, guardduty.threat-intel-set-location-unicode-round-trip.

AWS: Smithy (services/service-com-amazonaws-guardduty-guarddutyapiservice/smithy-model/com-amazonaws-guardduty-guarddutyapiservice.json) models `Location` as a plain string member with a length constraint only — CreateIPSet.Location (jsonName `location`, required, target `Location`, @length 1..300, no @pattern) and GetIPSet.Location (jsonName `location`, required, same target and constraint); CreateThreatIntelSet.Location / GetThreatIntelSet.Location are the identical member on the same `Location` shape. The model documents it as "The URI of the file that contains the IPSet" and states no normalization, canonicalization or re-addressing of any kind; there is no modeled rule that lets the service substitute a different URI. GuardDuty therefore stores the URI the create submitted and reports that same string back: the member round-trips byte-exactly, both the scheme and the authority included. Both reads answer HTTP 200 (success code 200); their modeled errors are BadRequestException 400 and InternalServerErrorException 500 only, so there is no error channel through which a service could signal "I changed your location". Recorded behavior confirms the round-trip: a CreateIPSet with Location `s3://<bucket>/<key>` is followed by GetIPSet returning exactly `s3://<bucket>/<key>`, and likewise for CreateThreatIntelSet/GetThreatIntelSet.

Emulator: the create stores the submitted value unchanged (services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/create_ip_set.rs stores `input.location.clone()`), but the read-back substitutes a different URI. GetIPSet serializes `crate::state::announced_list_location(ctx, &ip_set.location)`, and that helper parses the stored S3 URI into (bucket, key) and re-emits `format!("{}/{bucket}/{key}", announced_base(ctx))`, where `announced_base` is the scheme+authority of the endpoint the request arrived on. A list stored as `s3://sim2real-gd-ip-daae92f50cf3/ip-lists/list.txt` reads back as `https://127.0.0.1:46277/sim2real-gd-ip-daae92f50cf3/ip-lists/list.txt`: the `s3://` scheme is replaced by `https://`, the bucket is demoted from authority to first path segment, and an emulator host:port is inserted. GetThreatIntelSet goes through the same helper and diverges identically (`s3://sim2real-gd-ti-93d98ac991b9/threat-intel/list.txt` read back as `https://127.0.0.1:37855/sim2real-gd-ti-93d98ac991b9/threat-intel/list.txt`). The host, port and bucket/key names above are this environment's per-run values and are not the requirement; the requirement is that the served string equals the stored string, whatever it is. Note that the rewrite also makes the member escape its own modeled domain: the helper only falls back to the stored value when the rewritten URL would exceed @length 300, so the served value is otherwise never the stored one.

Why: `location` is the handle a client uses to identify and re-submit a trusted-IP or threat list. A caller that reads an IPSet and writes it back (UpdateIPSet, a CloudFormation/Terraform refresh, any describe-then-apply reconciler) now submits the rewritten `https://<emulator-host>/<bucket>/<key>` string in place of its own `s3://<bucket>/<key>`, so the resource is seen as perpetually drifted and is rewritten on every pass; after one such write the original S3 URI is lost from the store entirely. Equality checks against the configured location fail, idempotent applies become non-idempotent, and any value that cannot be parsed back into an `s3://` URI (the emulator's own output cannot) is then refused by the same service's create/update validation (`crate::state::is_s3_location`), so a plain read-modify-write loop that AWS accepts is rejected. The rewrite also leaks a host:port of the local test environment into resource configuration that clients persist.

Note on a prior resolved issue: 20261110T102233Z-guardduty-read-backs-serve-content-the-emulator-cannot.md (status fixed) asked, from a wire recording whose expected `location` was shaped-not-asserted, for the read-back to announce a host the emulator serves; the helper above is that fix, and it is what these behavioral checks now contradict against real AWS. The two must be reconciled in favour of the modeled contract: the API member must round-trip verbatim. Making the advertised object fetchable is a concern for the emulator's object-storage addressing (resolving the stored `s3://<bucket>/<key>` internally to its own S3 front door), not for the value this operation serializes. Also distinct from the open issue 20261031T120647Z-guardduty-refuses-every-uploaded-list-location-that-is.md, which is about which Locations the create ACCEPTS — the validation side is untouched here and must not be re-broken: in guardduty.create-ipset-malformed-s3-location-rejected the BadRequestException for `this is not a valid s3 uri or arn` was correct, and only the positive control's read-back diverged.

Fix: the `location` projection the read-back handlers serialize, in the guardduty crate's uploaded-list state — the read must emit the stored string as stored, for every uploaded-list family that shares the helper (IPSet, ThreatIntelSet, and the entity-set/threat-entity-set siblings reading through it).

Traces: guardduty.ipset-unicode-name-round-trip and guardduty.ipset-name-unicode-round-trip (GetIPSet Location assertion), guardduty.threat-intel-set-location-unicode-round-trip (GetThreatIntelSet Location assertion), guardduty.create-ipset-malformed-s3-location-rejected (the same GetIPSet divergence surfacing in that check's positive control). File citations: services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/get_ip_set.rs line 185; services/service-com-amazonaws-guardduty-guarddutyapiservice/src/state/announced_list_location.rs lines 63-72 and 80-98; services/service-com-amazonaws-guardduty-guarddutyapiservice/src/ops/create_ip_set.rs lines 280-298 (stores the value unchanged).

## Evidence
Behavioral suite simparity-behavioral-aws-v1 replayed against the booted emulator: 64/80 passed, 16 mismatched, 0 errored, 0 skipped, 0 unloadable, bootstrap_signal=false; all failures under the `### guardduty` heading. Of those 16, 11 are marked OWNED by open guardduty issues and are skipped here; 1 (guardduty.e2e-malware-scan-to-finding-flow) timed out on its own 180s in-check settle under a 60s check budget and is not filed. The 4 below are the surviving cluster, all failing the same `Location` equality assertion.

guardduty.ipset-name-unicode-round-trip, check_guardduty_ipset_name_unicode_round_trip.py:46 — `assert got["Location"] == location`:
  AssertionError: ('https://127.0.0.1:46277/sim2real-gd-ip-daae92f50cf3/ip-lists/list.txt', 's3://sim2real-gd-ip-daae92f50cf3/ip-lists/list.txt')
  (first element = what GetIPSet served, second = what CreateIPSet was given and accepted)

guardduty.ipset-unicode-name-round-trip, check_guardduty_ipset_unicode_name_round_trip.py:42 — `assert got["Location"] == location`:
  AssertionError: ('https://127.0.0.1:45785/sim2real-gd-ipset-bucket-d69919e72f13/list.txt', 's3://sim2real-gd-ipset-bucket-d69919e72f13/list.txt')

guardduty.threat-intel-set-location-unicode-round-trip, check_guardduty_threat_intel_set_location_unicode_round_trip.py:46 — `assert got["Location"] == location`:
  AssertionError: ('https://127.0.0.1:37855/sim2real-gd-ti-93d98ac991b9/threat-intel/list.txt', 's3://sim2real-gd-ti-93d98ac991b9/threat-intel/list.txt')

guardduty.create-ipset-malformed-s3-location-rejected, check_guardduty_create_ipset_malformed_s3_location_rejected.py:56:
  AssertionError: expected Location 's3://sim2real-gd-ipset-bucket-df8d0c6492eb/list.txt' on the accepted IPSet, got 'https://127.0.0.1:42925/sim2real-gd-ipset-bucket-df8d0c6492eb/list.txt'
  The earlier half of this check passed: the malformed Location `this is not a valid s3 uri or arn` was correctly refused with BadRequestException and the exact message "The request is rejected because the parameter ipSetLocation has an invalid value.", and the rejected attempt provisioned nothing. Only the positive control's read-back diverged. In every quote above the host, port, bucket name and key are this run's values, not requirements; the requirement is served == stored.

MODEL EVIDENCE (smithy_query, model_path=services/service-com-amazonaws-guardduty-guarddutyapiservice/smithy-model/com-amazonaws-guardduty-guarddutyapiservice.json):
  CreateIPSet: POST /detector/{DetectorId}/ipset, code 200; Location (jsonName `location`, required, target Location, @length 1..300, no pattern); errors AccessDeniedException 403, BadRequestException 400, InternalServerErrorException 500.
  GetIPSet: GET /detector/{DetectorId}/ipset/{IpSetId}, code 200; output name/format/location/status required (location -> Location, @length 1..300), tags and expectedBucketOwner optional; errors BadRequestException 400, InternalServerErrorException 500.
  CreateThreatIntelSet / GetThreatIntelSet: POST and GET /detector/{DetectorId}/threatintelset[/{ThreatIntelSetId}], code 200; the same required `location` member on the same `Location` shape.
  No member of any of the four carries a pattern, a default, or any trait that would license rewriting the value.

SOURCE EVIDENCE:
  src/ops/get_ip_set.rs:185 — `location: crate::state::announced_list_location(ctx, &ip_set.location)`.
  src/state/announced_list_location.rs:63-72 — parses the stored value into (bucket, key) and returns `format!("{}/{bucket}/{key}", announced_base(ctx))`, falling back to the stored value only when the rewrite exceeds LOCATION_MAX_LENGTH (300).
  src/state/announced_list_location.rs:80-98 — `announced_base` builds scheme+authority from the endpoint the request was reached on (reached_emulator_endpoint_of_parts / served_endpoint_authority), which is the source of the `https://127.0.0.1:<port>` prefix seen above.
  src/ops/create_ip_set.rs:280-298 — the create stores `input.location.clone()` unchanged, so the stored value is correct and the divergence is entirely in the read projection.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator, then replay the simparity-behavioral-aws-v1 suite. Confirm bootstrap_signal=false and that the guardduty service is answering before reading any divergence.
2. Replay guardduty.ipset-unicode-name-round-trip and observe the failure at the `got["Location"] == location` assertion.
3. Harness-free repro (IPSet): CreateDetector(Enable=true); CreateIPSet(DetectorId, Name='sim2real-gd-ipset-a', Format='TXT', Location='s3://<some-bucket>/list.txt', Activate=false); then GetIPSet(DetectorId, IpSetId). Assert the returned `Location` is byte-for-byte the string passed to CreateIPSet (scheme `s3://`, bucket as authority) — the emulator instead returns `https://<the endpoint host:port the call was made to>/<bucket>/list.txt`.
4. Harness-free repro (ThreatIntelSet): CreateThreatIntelSet(DetectorId, Name='sim2real-ti-a', Format='TXT', Location='s3://<some-bucket>/threat-intel/list.txt', Activate=false); then GetThreatIntelSet(DetectorId, ThreatIntelSetId) and make the same byte-exact assertion. Also check the entity-set read-backs that share the same projection helper.
5. Read-modify-write check that shows the downstream consequence: take the `Location` GetIPSet returned and pass it unchanged to UpdateIPSet for the same IPSet. On AWS this is a no-op; here the value is no longer an S3 URI and the service's own location validation refuses it with BadRequestException "The request is rejected because the parameter ipSetLocation has an invalid value."
6. Guard the validation side while fixing: re-run guardduty.create-ipset-malformed-s3-location-rejected and confirm a malformed Location is still refused with BadRequestException and the message "The request is rejected because the parameter ipSetLocation has an invalid value.", provisioning nothing — only the read-back must change.
7. Re-replay all four checks listed in trace_ids and confirm they pass before submitting.
