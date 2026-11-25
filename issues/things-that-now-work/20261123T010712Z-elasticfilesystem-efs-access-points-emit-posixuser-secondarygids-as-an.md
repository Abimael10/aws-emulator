Issue-Source: trace-sweep-divergence
Issue-Trace: efs-access-point-lifecycle
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: efs-access-point-lifecycle
Issue-Shape-Id: com.amazonaws.efs#MagnolioAPIService_v20150201
Issue-Key: elasticfilesystem|CreateAccessPoint|2xx->2xx|$.PosixUser.SecondaryGids
Issue-Key: elasticfilesystem|DescribeAccessPoints|2xx->2xx|$.AccessPoints[Name="sim2real-probe-efs-ap-token-c5da644e"].PosixUser.SecondaryGids
Issue-Commit: fda9c10fdfacd5c8a2bec05c9c210d2748ab313c

# EFS access points emit PosixUser.SecondaryGids as an empty array instead of omitting the unset member

> _Note: this issue was identified against the code base at commit `fda9c10fdfacd5c8a2bec05c9c210d2748ab313c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccessPoint

## Description
Two net-new causes, one shared fault: every EFS response that carries an AccessPointDescription serializes the optional PosixUser.SecondaryGids member as an empty JSON array when the CreateAccessPoint request supplied no secondary GIDs, where AWS omits the member entirely. The emulator never distinguishes "no secondary GIDs were requested" from "an empty secondary-GID list", so the defect reproduces on both the create path and the read path.

Cause 1 of 2: CreateAccessPoint (fixture efs-access-point-lifecycle, step task#3).
AWS: Smithy: AccessPointDescription$PosixUser targets com.amazonaws.efs#PosixUser, whose members are Uid (@required), Gid (@required) and SecondaryGids (OPTIONAL, target com.amazonaws.efs#SecondaryGids, @length min 0 max 16). SecondaryGids carries no @default and is not @required, so when a CreateAccessPoint request's PosixUser omits SecondaryGids, the access point has no secondary GIDs and AWS's awsJson/restJson serializer leaves the member out of the response body altogether — the key is absent, not present-and-empty. The recorded response for this step is HTTP 200 with PosixUser containing only Uid and Gid. Status 200 and the body shape are what the recording and the model agree on; there is no modeled error involved.
Emulator: HTTP 200, but the body contains $.PosixUser.SecondaryGids as []. Replay reports "$.PosixUser.SecondaryGids: type_mismatch (expected None, got [])".
Why: a client that branches on whether the member is present (SDKs that model it as Option<Vec<_>> / a nullable list, and any round-trip that feeds the described access point back into a create/compare) sees a secondary-GID list that was never configured. Code that treats presence as "secondary GIDs were explicitly set" takes the wrong branch, and config-drift/idempotency comparisons between the requested PosixUser and the described PosixUser report a spurious difference.
Fix: the CreateAccessPoint response serializer for AccessPointDescription/PosixUser in the EFS service crate (services/service-com-amazonaws-efs-magnolioapiservice-v20150201), together with the state that records the access point's POSIX identity: an absent SecondaryGids in the request must be stored as absent (not as an empty collection) and must serialize as an omitted member.
Traces: efs-access-point-lifecycle#task#3.

Cause 2 of 2: DescribeAccessPoints (fixture efs-access-point-lifecycle, step task#4; same cluster also covers task#5).
AWS: Smithy: DescribeAccessPointsResponse$AccessPoints is a list of the same AccessPointDescription shape, so the same rule applies — PosixUser.SecondaryGids is omitted for an access point created without secondary GIDs. The recorded response is HTTP 200 with the described access point's PosixUser carrying only Uid and Gid.
Emulator: HTTP 200, but each described access point carries PosixUser.SecondaryGids as []. Replay reports "$.AccessPoints[Name=...].PosixUser.SecondaryGids: type_mismatch (expected None, got [])" (the Name value in that path is the fixture's generated access-point name and is not part of the requirement).
Why: the read path is what clients poll while an access point moves to `available`, so every describe of an access point without secondary GIDs reports a POSIX identity that differs from the one requested; waiters and drift checks that diff described-vs-desired state never converge.
Fix: the DescribeAccessPoints response serializer / the shared AccessPointDescription projection in the same crate, reading the corrected state from Cause 1 rather than defaulting the list.
Traces: efs-access-point-lifecycle#task#4, efs-access-point-lifecycle#task#5.

No open issue covers this. The open elasticfilesystem issues concern mount-target subnet state, the native NFSv4 data plane and CloudFormation-driven EFS resources; none of them touches AccessPointDescription serialization, and the replay report marks both clusters as unowned (owned_by empty, stub_path empty).

## Evidence
Replay summary: trace_set_id=simparity-wire-aws-v1, emulator_up=true, steps_total=99, steps_passed=91, steps_mismatched=8, divergence_count=2, score_pct=91.9.

Cluster 1 — service=elasticfilesystem, operation=CreateAccessPoint, verdict=mismatch, fixture_id=efs-access-point-lifecycle, step_key=task#3, expected_status=200, actual_status=200.
  expected: response body PosixUser = { "Uid": <uid from request>, "Gid": <gid from request> } — SecondaryGids key ABSENT.
  actual:   response body PosixUser additionally contains "SecondaryGids": [].
  detail:   $.PosixUser.SecondaryGids: type_mismatch (expected None, got [])
  divergence_key: service=elasticfilesystem|CreateAccessPoint|status=200->200|keys=$.PosixUser.SecondaryGids

Cluster 2 — service=elasticfilesystem, operation=DescribeAccessPoints, verdict=mismatch, fixture_id=efs-access-point-lifecycle, step_key=task#4 (affected: task#4, task#5), expected_status=200, actual_status=200.
  expected: each AccessPoints[] entry's PosixUser = { "Uid": ..., "Gid": ... } — SecondaryGids key ABSENT.
  actual:   each entry's PosixUser additionally contains "SecondaryGids": [].
  detail:   $.AccessPoints[Name="sim2real-probe-efs-ap-token-c5da644e"].PosixUser.SecondaryGids: type_mismatch (expected None, got [])
  divergence_key: service=elasticfilesystem|DescribeAccessPoints|status=200->200|keys=$.AccessPoints[...].PosixUser.SecondaryGids

The access-point name/id and client token in the paths above are per-run generated values and are not part of the requirement; the requirement is that the optional member is omitted.

Model evidence (smithy_query on services/service-com-amazonaws-efs-magnolioapiservice-v20150201/smithy-model/com-amazonaws-efs-magnolioapiservice-v20150201.json):
  com.amazonaws.efs#PosixUser members = Uid (required, long, range 0..4294967295), Gid (required, long, range 0..4294967295), SecondaryGids (NOT required, list, @length min 0 max 16, no @default).
  com.amazonaws.efs#AccessPointDescription members include PosixUser (not required), and is the element type of DescribeAccessPoints' AccessPoints list.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus trace set simparity-wire-aws-v1.
2. Replay fixture efs-access-point-lifecycle. Step task#3 calls CreateAccessPoint against a file system with a PosixUser that specifies only Uid and Gid (no SecondaryGids member).
3. Inspect the HTTP 200 CreateAccessPoint response body: the emulator returns "PosixUser": {"Uid": ..., "Gid": ..., "SecondaryGids": []}; AWS returns PosixUser with Uid and Gid only, with no SecondaryGids key.
4. Continue to steps task#4 and task#5, which call DescribeAccessPoints for the same file system / access point. Inspect AccessPoints[].PosixUser in the HTTP 200 body: the emulator again emits "SecondaryGids": []; AWS omits the key.
5. Both steps are reported as mismatches with detail "...PosixUser.SecondaryGids: type_mismatch (expected None, got [])". Fix is confirmed when both the create and the describe responses omit the member for an access point created without secondary GIDs (and still emit the list when the request supplies one).
