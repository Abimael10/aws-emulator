Issue-Source: trace-sweep-divergence
Issue-Trace: guardduty-ip-and-threat-intel-set-lifecycle
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: guardduty-ip-and-threat-intel-set-lifecycle guardduty-threat-and-trusted-entity-set-lifecycle
Issue-Shape-Id: com.amazonaws.guardduty#GuardDutyAPIService
Issue-Key: guardduty|GetThreatIntelSet|2xx->2xx|$.location
Issue-Key: guardduty|GetThreatEntitySet|2xx->2xx|$.location
Issue-Key: guardduty|GetTrustedEntitySet|2xx->2xx|$.location
Issue-Commit: 6293265de61e14a506a69e34708f6899cfc52a4d

# Uploaded-list read-backs rewrite the stored s3:// Location into an emulator-host URL

> _Note: this issue was identified against the code base at commit `6293265de61e14a506a69e34708f6899cfc52a4d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetThreatIntelSet

## Description
This issue carries 3 net-new root causes in the guardduty service, found by replaying the simparity-wire-aws-v1 wire corpus against the booted emulator. All three are the SAME fault family — an uploaded-list read-back serves a `location` member whose scheme and authority are not the ones the create stored, because the value is projected through a host-rewriting helper — but each is a DIFFERENT operation with its own fixture, its own step and its own output-construction site, and ALL THREE must be closed before this issue is submitted.

Related already-open issues that do NOT cover these steps and must be reconciled rather than duplicated: 20261118T195055Z-guardduty-getipset-getthreatintelset-rewrite-the-stored-location-into-an.md (behavioral corpus; same helper, asserts byte-exact round-trip on GetIPSet/GetThreatIntelSet), 20261119T085728Z-guardduty-getipset-and-getinvestigation-serve-read-back-values-aws.md (wire; owns the GetIPSet `$.location` cluster of this same replay, which is why GetIPSet is not re-filed here) and the resolved 20261110T102233Z-guardduty-read-backs-serve-content-the-emulator-cannot.md (why the rewrite exists). None of them carries the replay keys of the three clusters below, and none names GetThreatEntitySet or GetTrustedEntitySet at all.

=== Cause 1 of 3: GetThreatIntelSet rewrites the stored ThreatIntelSet Location (fixture guardduty-ip-and-threat-intel-set-lifecycle, step task#7) ===

AWS: Smithy (services/service-com-amazonaws-guardduty-guarddutyapiservice/smithy-model/com-amazonaws-guardduty-guarddutyapiservice.json) models GetThreatIntelSet -> GET /detector/{DetectorId}/threatintelset/{ThreatIntelSetId}, success code 200, output member Location (jsonName `location`, required, target `Location`, @length 1..300, NO @pattern), documented as the URI of the file that contains the ThreatIntelSet; modeled errors are BadRequestException 400 and InternalServerErrorException 500 only. CreateThreatIntelSet carries the identical member on the same shape. The model states no normalization, canonicalization or re-addressing, and offers no channel through which the service could signal that it substituted a different URI, so the member round-trips the stored string — scheme and authority included. The recording confirms it: the step's expected `$.location` is the `s3://<bucket>/<key>` URI the create submitted, served back unchanged with HTTP 200.

Emulator: HTTP 200 with every other member agreeing, but `$.location` is a rewritten URL: a Location stored as `s3://<bucket>/<key>` is served as `https://<emulator-host>:<port>/<bucket>/<key>` — the `s3://` scheme replaced by the request endpoint's scheme, the bucket demoted from authority to first path segment, and an emulator host:port inserted. Replay verdict: mismatch, `$.location: value_mismatch` (expected `s3://sim2real-probe-guardduty-tis/malicious-ips.txt`, got `https://127.0.0.1:32851/sim2real-probe-guardduty-tis/malicious-ips.txt`). The host, port, bucket and key in that quote are this run's per-run values and are not the requirement; the requirement is served == stored.

Why: `location` is the handle a client uses to identify and re-submit a threat list. A caller that reads a ThreatIntelSet back and writes it again (UpdateThreatIntelSet, a CloudFormation/Terraform refresh, any describe-then-apply reconciler) submits the rewritten `https://...` string in place of its own `s3://` URI, so the resource reads as perpetually drifted and the original URI is lost after one such write. Worse, the emulator's own rewritten output is no longer an S3 URI, so the service's create/update location validation refuses it: a read-modify-write loop AWS accepts is rejected here. It also leaks a local host:port into resource configuration clients persist.

Fix: the shared `location` read projection and the handler that calls it — src/state/announced_list_location.rs (`announced_list_location` rewrites via `s3_object_of_location` + `announced_base`) consumed by src/ops/get_threat_intel_set.rs. The stored value must be served as stored; making the advertised object fetchable belongs to the emulator's object-storage addressing, not to this member's serialization. Keep the create/update side untouched: `crate::state::is_s3_location` must still refuse a malformed Location with BadRequestException and the message "The request is rejected because the parameter ipSetLocation has an invalid value." (fixed sentence; the parameter name is the member the request named).

Traces: guardduty-ip-and-threat-intel-set-lifecycle#task#7 (representative) and #task#14 — GetThreatIntelSet, expected 200 / got 200, `$.location` value_mismatch. Cluster size 2.

=== Cause 2 of 3: GetThreatEntitySet rewrites the stored threat-entity-set Location (fixture guardduty-threat-and-trusted-entity-set-lifecycle, step task#6) ===

AWS: Smithy models GetThreatEntitySet -> GET /detector/{DetectorId}/threatentityset/{ThreatEntitySetId}, success code 200, output member Location (jsonName `location`, required, target `Location`, @length 1..300, no @pattern); errors BadRequestException 400 and InternalServerErrorException 500. CreateThreatEntitySet carries the same required Location member. As with the threat-intel family nothing in the model licenses rewriting the value, and the recording shows the `s3://<bucket>/<key>` URI the create submitted served back verbatim under HTTP 200.

Emulator: HTTP 200, `$.location` served as `https://<emulator-host>:<port>/<bucket>/<key>` instead of the stored `s3://<bucket>/<key>`. Replay verdict: mismatch, `$.location: value_mismatch` (expected `s3://sim2real-probe-guardduty-tes/malicious-domains.txt`, got `https://127.0.0.1:35251/sim2real-probe-guardduty-tes/malicious-domains.txt`); host, port, bucket and key are this run's values, not the requirement.

Why: identical consequence for the entity-set family — a read-then-update of a threat entity set resubmits an address of the emulator rather than the caller's own S3 URI, so the configured list identity does not survive a round trip and any equality check or idempotent apply against the configured location fails.

Fix: same shared projection, src/state/announced_list_location.rs, as consumed by src/ops/get_threat_entity_set.rs line 227 (`location: crate::state::announced_list_location(ctx, &set.location)`).

Traces: guardduty-threat-and-trusted-entity-set-lifecycle#task#6 — GetThreatEntitySet, expected 200 / got 200, `$.location` value_mismatch. Cluster size 1.

=== Cause 3 of 3: GetTrustedEntitySet rewrites the stored trusted-entity-set Location (fixture guardduty-threat-and-trusted-entity-set-lifecycle, step task#7) ===

AWS: Smithy models GetTrustedEntitySet -> GET /detector/{DetectorId}/trustedentityset/{TrustedEntitySetId}, success code 200, output member Location (jsonName `location`, required, target `Location`, @length 1..300, no @pattern); errors BadRequestException 400 and InternalServerErrorException 500. CreateTrustedEntitySet carries the same member. No modeled rule permits substituting a different URI; the recording serves back the submitted `s3://<bucket>/<key>` URI under HTTP 200.

Emulator: HTTP 200, `$.location` served as `https://<emulator-host>:<port>/<bucket>/<key>`. Replay verdict: mismatch, `$.location: value_mismatch` (expected `s3://sim2real-probe-guardduty-tres/trusted-domains.txt`, got `https://127.0.0.1:35251/sim2real-probe-guardduty-tres/trusted-domains.txt`); the host, port, bucket and key are per-run values.

Why: a trusted entity set is an allow-list; a client that reconciles it against its intended `s3://` location sees a permanent mismatch and rewrites the resource on every pass, and the rewritten value cannot be fed back through the service's own location validation.

Fix: same shared projection, src/state/announced_list_location.rs, as consumed by src/ops/get_trusted_entity_set.rs line 223. Because all four uploaded-list reads share the one helper, a single change closes all three causes — but each operation must be re-replayed, since each has its own output-construction site that could keep (or regain) a local rewrite.

Traces: guardduty-threat-and-trusted-entity-set-lifecycle#task#7 — GetTrustedEntitySet, expected 200 / got 200, `$.location` value_mismatch. Cluster size 1.

## Evidence
Wire corpus simparity-wire-aws-v1 replayed against the booted emulator: emulator_up=true ("/__admin/health answered 200 and the :38615 data listener accepts"), bootstrap_signal=false, 86 fixtures replayed, 0 unreplayable, 0 replay errors; 559 steps total, 546 passed, 13 mismatched, 0 errored, 0 blocked, 0 not_implemented, score 97.7%. 7 divergence clusters, all guardduty. Four are marked owned_by open issues and are NOT re-filed: GetThreatEntitySet 404->200 and GetTrustedEntitySet 404->200 (owned by 20261117T132545Z-guardduty-deleted-entity-sets-keep-answering-200-instead-of.md), GetIPSet $.location and GetInvestigation $.investigation.summary (owned by 20261119T085728Z-guardduty-getipset-and-getinvestigation-serve-read-back-values-aws.md). The 3 clusters below are the surviving net-new ones: no stub_path, no owned_by.

CAUSE 1 — fixture guardduty-ip-and-threat-intel-set-lifecycle, step task#7, operation GetThreatIntelSet, verdict mismatch, expected_status 200, actual_status 200, affected_count 2 (guardduty-ip-and-threat-intel-set-lifecycle#task#7, guardduty-ip-and-threat-intel-set-lifecycle#task#14):
  $.location: value_mismatch (expected 's3://sim2real-probe-guardduty-tis/malicious-ips.txt', got 'https://127.0.0.1:32851/sim2real-probe-guardduty-tis/malicious-ips.txt')
  divergence_key: service=guardduty|GetThreatIntelSet|status=200->200|keys=$.location

CAUSE 2 — fixture guardduty-threat-and-trusted-entity-set-lifecycle, step task#6, operation GetThreatEntitySet, verdict mismatch, expected_status 200, actual_status 200, affected_count 1:
  $.location: value_mismatch (expected 's3://sim2real-probe-guardduty-tes/malicious-domains.txt', got 'https://127.0.0.1:35251/sim2real-probe-guardduty-tes/malicious-domains.txt')
  divergence_key: service=guardduty|GetThreatEntitySet|status=200->200|keys=$.location

CAUSE 3 — fixture guardduty-threat-and-trusted-entity-set-lifecycle, step task#7, operation GetTrustedEntitySet, verdict mismatch, expected_status 200, actual_status 200, affected_count 1:
  $.location: value_mismatch (expected 's3://sim2real-probe-guardduty-tres/trusted-domains.txt', got 'https://127.0.0.1:35251/sim2real-probe-guardduty-tres/trusted-domains.txt')
  divergence_key: service=guardduty|GetTrustedEntitySet|status=200->200|keys=$.location

In every quote above the emulator host (127.0.0.1), the port, the bucket name and the object key are this environment's per-run values and are NOT requirements. The requirement common to all three is that the served `location` be the string the create/update stored, scheme and authority included.

MODEL EVIDENCE (smithy_query, model_path=services/service-com-amazonaws-guardduty-guarddutyapiservice/smithy-model/com-amazonaws-guardduty-guarddutyapiservice.json):
  GetThreatIntelSet: GET /detector/{DetectorId}/threatintelset/{ThreatIntelSetId}, code 200; output name/format/location/status required, location -> target Location @length 1..300 (jsonName `location`, no pattern), tags + expectedBucketOwner optional; errors BadRequestException 400, InternalServerErrorException 500.
  GetThreatEntitySet: GET /detector/{DetectorId}/threatentityset/{ThreatEntitySetId}, code 200; output location required, target Location @length 1..300, no pattern; errors BadRequestException 400, InternalServerErrorException 500.
  GetTrustedEntitySet: GET /detector/{DetectorId}/trustedentityset/{TrustedEntitySetId}, code 200; output location required, target Location @length 1..300, no pattern; errors BadRequestException 400, InternalServerErrorException 500.
  CreateThreatIntelSet / CreateThreatEntitySet / CreateTrustedEntitySet: POST on the same collection URIs, code 200, each with the identical required `location` member on the same `Location` shape. No member of any of the six carries a pattern, a default, or any trait that would license rewriting the value.

SOURCE EVIDENCE:
  src/state/announced_list_location.rs:59-64 — `announced_list_location(ctx, stored)` parses the stored value with `s3_object_of_location` and, when it parses, returns `format!("{}/{bucket}/{key}", announced_base(ctx))`, serving the stored string only when it parses as no object at all.
  src/state/announced_list_location.rs:68-83 — `announced_base` builds scheme+authority from the endpoint the request was reached on (`reached_emulator_endpoint_of_parts` / `served_endpoint_authority`), which is the source of the `https://127.0.0.1:<port>` prefix.
  src/ops/get_threat_intel_set.rs:197 — `location: crate::state::announced_list_location(ctx, &set.location)`.
  src/ops/get_threat_entity_set.rs:227 — same call for the threat entity set.
  src/ops/get_trusted_entity_set.rs:223 — same call for the trusted entity set.
  The module header of announced_list_location.rs states the rewrite is deliberate ("only the address it is REPORTED at is this emulator's"); this corpus is what contradicts that reading, so the two must be reconciled in favour of the modeled member round-tripping verbatim.

## Steps to Reproduce
1. git clone the repo, build and boot the emulator, then replay the simparity-wire-aws-v1 wire corpus. Confirm emulator_up=true and bootstrap_signal=false before reading any divergence, and confirm guardduty is answering (546 of 559 steps pass).

Cause 1 (GetThreatIntelSet):
2. Replay fixture guardduty-ip-and-threat-intel-set-lifecycle and observe step task#7 (and task#14): HTTP 200 with `$.location` = 'https://127.0.0.1:<port>/<bucket>/malicious-ips.txt' where the recording expects 's3://<bucket>/malicious-ips.txt'.
3. Harness-free repro: CreateDetector(Enable=true); CreateThreatIntelSet(DetectorId, Name='ti-a', Format='TXT', Activate=false, Location='s3://<some-bucket>/threat-intel/list.txt'); then GetThreatIntelSet(DetectorId, ThreatIntelSetId). Assert the returned `location` is byte-for-byte the string the create was given — the emulator returns the request endpoint's scheme+authority with the bucket demoted to a path segment.

Cause 2 (GetThreatEntitySet):
4. Replay fixture guardduty-threat-and-trusted-entity-set-lifecycle, step task#6, and observe the same `$.location` value_mismatch.
5. Harness-free repro: CreateThreatEntitySet(DetectorId, Name='tes-a', Format='TXT', Activate=false, Location='s3://<some-bucket>/malicious-domains.txt'); then GetThreatEntitySet(DetectorId, ThreatEntitySetId) and make the same byte-exact assertion.

Cause 3 (GetTrustedEntitySet):
6. Replay the same fixture, step task#7, for the GetTrustedEntitySet cluster.
7. Harness-free repro: CreateTrustedEntitySet(DetectorId, Name='tres-a', Format='TXT', Activate=false, Location='s3://<some-bucket>/trusted-domains.txt'); then GetTrustedEntitySet(DetectorId, TrustedEntitySetId) and assert served == stored.

Read-modify-write check that shows the consequence (applies to all three): take the `location` the Get returned and pass it unchanged to the matching Update call. On AWS this is a no-op; here the value is no longer an S3 URI and the service's own location validation refuses it with BadRequestException "The request is rejected because the parameter ipSetLocation has an invalid value."

8. Guard the validation side and the conflicting prior fixes while changing the helper: a malformed Location must still be refused with BadRequestException and that exact message, provisioning nothing (open issue 20261031T120647Z and the behavioral checks listed in 20261118T195055Z), and the GetIPSet `location` cluster owned by 20261119T085728Z goes through the SAME helper — co-ordinate with it rather than flipping the helper back and forth.
9. Re-replay ALL THREE bundled causes before submitting: guardduty-ip-and-threat-intel-set-lifecycle#task#7 (plus #task#14), guardduty-threat-and-trusted-entity-set-lifecycle#task#6 and guardduty-threat-and-trusted-entity-set-lifecycle#task#7, and confirm the corpus's mismatched count drops by 4 steps with no previously passing step regressing.
