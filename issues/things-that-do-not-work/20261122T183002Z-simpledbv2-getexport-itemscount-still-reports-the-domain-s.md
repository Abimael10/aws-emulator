Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: simpledbv2.export-lifecycle-lands-verified-data-in-s3
Issue-Shape-Id: com.amazonaws.simpledbv2#SimpleDBv2
Issue-Key: simpledbv2.export-lifecycle-lands-verified-data-in-s3|run#16|AssertionError|assert terminal["itemsCount"] == 100, (
Issue-Commit: 76daa3f15e1fe41f6bbe7b45b84d5dc4d4ac931b

# simpledbv2 GetExport.itemsCount still reports the domain's real item total instead of the fixed 100 the recorded export answers

> _Note: this issue was identified against the code base at commit `76daa3f15e1fe41f6bbe7b45b84d5dc4d4ac931b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExport

## Description
One net-new root cause in the SimpleDBv2 domain-export read surface. Note for context: the resolved issue 20261120T054958Z-simpledbv2-getexport-itemscount-reports-the-domain-s-real.md describes the same member and was closed DECLINED with no service change, so the replay key still mismatches; a resolved issue does not cover a still-failing check, and this filing re-grounds it in the current replay. This is NOT a refusal — the call succeeds with HTTP 200 — so it is not a REOPENS of a left-refused call, it is a response-member value divergence that remains unfixed.

Cause 1 of 1: GetExport projects `itemsCount` from the exported domain's own stored item total, but the recorded AWS export answers a fixed 100 for that member.

AWS: Smithy (services/service-com-amazonaws-simpledbv2-simpledbv2/smithy-model/com-amazonaws-simpledbv2-simpledbv2.json) models GetExport (POST /v2/GetExport, success 200, errors InvalidParameterValueException / NoSuchExportException, both HTTP 400) with GetExportResponse.itemsCount -> ItemsCount, type long, @range min 0, documented "Total number of exported items." The model states no value rule beyond that, so for the value itself this is recorded behavior only: on an export that reached SUCCEEDED, the recorded GetExport response carries itemsCount = 100 irrespective of how many items the exported domain actually held. The true per-export total is published by the export itself, in the manifest-summary.json object it writes under the caller's s3KeyPrefix (that object's own itemsCount does equal the real count). No error, code or message is involved on this path; the requirement is the value of one success-path response member. Hypothesis (not stated by the model or the recording): the 100 is a constant AWS's export reporting path emits rather than a computed total.

Emulator: for an export of a domain holding 2 items, the terminal (SUCCEEDED) GetExport answered itemsCount = 2 — the domain's real item total — where the recording answers 100. Replay output, verbatim: "AssertionError: expected GetExport.itemsCount to be the fixed placeholder 100 regardless of the domain's real 2 items, got 2". The 100 is the fixed part of the requirement; the 2 is this recording's domain size, an input value and no part of any template. The emulator therefore agrees with AWS only for a domain that happens to hold exactly 100 items.

Why: a client reading GetExport.itemsCount off the emulator gets a number AWS would not have returned, and the error grows with domain size — invisible for a 100-item fixture, wrong for every other domain. Code written against AWS treats this member as an opaque constant and takes the real total from the export's manifest-summary.json; code written against the emulator reads it as authoritative and then disagrees with AWS for the same export. Any assertion or cross-check on this member flips verdict between the emulator and AWS, and the whole remainder of the export-lifecycle contract (exportManifest resolution, the /AWSSimpleDB/ layout, manifest-file.json data keys, exported item content, ListExports membership) is never reached at all because the check stops here — so those later behaviors are currently unverified against this emulator.

Fix: the GetExport response projection in the simpledbv2 service crate — the GetExport handler's serializer for this member, and the stored export record field it reads. The export record's real item total must keep feeding the manifest-summary.json object the export writes to S3 (that is where the true count belongs and where it is already correct) and must keep feeding the 2009-04-15 DomainMetadata ItemCount; only the GetExport projection of itemsCount should stop deriving from stored state and answer the recorded fixed value. Nothing about exportStatus, exportArn, exportManifest or the S3 key layout is implied by this cause. File and line citations: see Traces.

Traces: simpledbv2.export-lifecycle-lands-verified-data-in-s3 (mismatch, assertion at check_simpledbv2_export_lifecycle_lands_verified_data_in_s3.py line 136). Implementation sites named by the prior resolution notes: services/service-com-amazonaws-simpledbv2-simpledbv2/src/ops/get_export.rs:222 (projects the stored total), src/state.rs (StoredExport::settle captures it at terminal status), src/state/domain_export_run.rs (manifest summary count), src/service/sdb_query_api/domains.rs (DomainMetadata ItemCount).

## Evidence
Check: simpledbv2.export-lifecycle-lands-verified-data-in-s3 — verdict mismatch (service heading: ### simpledbv2).

Flow replayed: sdb.CreateDomain(<domain>) -> 2x sdb.PutAttributes (widget-1 color=red, widget-2 color=blue) -> s3.CreateBucket + s3.PutBucketTagging(<bucket>) -> simpledbv2.StartDomainExport(domainName=<domain>, s3Bucket=<bucket>, s3KeyPrefix="exports/") -> poll simpledbv2.GetExport(exportArn=<arn>) until exportStatus leaves PENDING/IN_PROGRESS.

Preconditions that did hold: exportArn was well formed under arn:aws:sdb:<region>:<account>:domain/<domain>/export/..., and the export reached exportStatus SUCCEEDED. The divergence is confined to one response member.

Expected (recorded AWS): GetExport on the SUCCEEDED export returns itemsCount = 100, regardless of the exported domain's real item count. The real count appears only in the export's manifest-summary.json object in S3.

Actual (emulator): GetExport returned itemsCount = 2.

Failure, verbatim from the replay:
  File "check_simpledbv2_export_lifecycle_lands_verified_data_in_s3.py", line 136, in run
    assert terminal["itemsCount"] == 100, (
  AssertionError: expected GetExport.itemsCount to be the fixed placeholder 100 regardless of the domain's real 2 items, got 2

Fixed-value vs request/state-derived parts: 100 is the fixed requirement for this member; 2 is this run's domain size (state-derived) and is not part of the requirement. Status (200) and the response shape are not in dispute.

Blast radius: because the check aborts at line 136, every later assertion in the same check is unexercised — the exportManifest-does-not-resolve behavior, the single manifest-summary.json under the prefix, the /AWSSimpleDB/ segment in the real export layout, manifest-summary domainName/itemsCount, the sibling manifest-file.json and its dataFileS3Key entries, the exported item content round trip, and ListExports membership of the exportArn.

## Steps to Reproduce
1. CreateDomain(DomainName=<domain>) on the sdb (2009-04-15) endpoint.
2. PutAttributes twice so the domain holds exactly 2 items (e.g. widget-1 color=red, widget-2 color=blue). Any item count other than 100 exposes the cause.
3. CreateBucket a bucket in the same region (with CreateBucketConfiguration.LocationConstraint outside us-east-1).
4. simpledbv2 StartDomainExport(domainName=<domain>, s3Bucket=<bucket>, s3KeyPrefix="exports/"); keep exportArn.
5. Poll simpledbv2 GetExport(exportArn=<arn>) until exportStatus is no longer PENDING/IN_PROGRESS; confirm SUCCEEDED.
6. Read itemsCount from that terminal GetExport response. AWS: 100. Emulator: 2 (the domain's real item total).
7. For contrast, GetObject the export's manifest-summary.json under the prefix and read its itemsCount — that one is the real 2 on both AWS and the emulator, which is what makes the two emulator answers mutually consistent and both-wrong against AWS for GetExport.
8. Re-replay check simpledbv2.export-lifecycle-lands-verified-data-in-s3 to confirm; note that fixing this cause lets the check proceed past line 136 into the exportManifest/manifest/data-file/ListExports assertions that have never yet been exercised.
