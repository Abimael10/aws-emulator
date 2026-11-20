Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: simpledbv2.export-lifecycle-lands-verified-data-in-s3
Issue-Shape-Id: com.amazonaws.simpledbv2#SimpleDBv2
Issue-Key: simpledbv2.export-lifecycle-lands-verified-data-in-s3|run#16|AssertionError|assert terminal["itemsCount"] == 100, (
Issue-Commit: afcdf7be57b3fef95e777182b2b5896511e2ffa2

# simpledbv2 GetExport.itemsCount reports the domain's real item count instead of AWS's fixed 100 placeholder

> _Note: this issue was identified against the code base at commit `afcdf7be57b3fef95e777182b2b5896511e2ffa2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExport

## Description
One net-new root cause in the SimpleDBv2 domain-export surface.

Cause 1 of 1: GetExport's `itemsCount` is derived from the exported domain's actual item count, but real Amazon SimpleDBv2 always answers a fixed placeholder for this member.

AWS: Smithy (services/service-com-amazonaws-simpledbv2-simpledbv2/smithy-model/com-amazonaws-simpledbv2-simpledbv2.json) models GetExportResponse.itemsCount as target ItemsCount, type long, @range min 0, documented "Total number of exported items." The model states no value rule beyond that, so the requirement here is recorded behavior only: on a SUCCEEDED export of a domain, real GetExport returns itemsCount = 100 irrespective of how many items the domain actually held (a long-standing AWS reporting defect — the real per-export count is published only in the export's own manifest-summary.json object in S3, which does carry the true count). Status 200, no error involved; this is purely the value of a response member on the success path.

Emulator: for an export of a domain holding 2 items, GetExport on the terminal (SUCCEEDED) export answered itemsCount = 2 — the domain's real item count — where AWS answers 100. The check's assertion message records it exactly: "expected GetExport.itemsCount to be the fixed placeholder 100 regardless of the domain's real 2 items, got 2". The emulator is computing a value AWS does not compute, so the two agree only for a domain that happens to hold exactly 100 items.

Why: a client that reads GetExport.itemsCount gets a number AWS would never have returned. Code written against AWS treats this member as the opaque constant it is and takes the real count from the export's manifest-summary.json; code written against the emulator reads it as authoritative and then disagrees with the manifest for the same export. Anything that cross-checks GetExport.itemsCount against the manifest's itemsCount, or that asserts the AWS placeholder, flips verdict between the emulator and AWS. Because the divergence scales with domain size, it is invisible for small fixtures and wrong for every real domain.

Fix: the GetExport response projection in the simpledbv2 service crate (the GetExport handler and whatever export-record field it reads for itemsCount). The export record's real item total should keep feeding the manifest-summary.json object written to S3, which is where AWS publishes the true count; the GetExport serializer must stop projecting it and emit AWS's fixed placeholder for this member instead. No change to exportStatus, exportManifest or the S3 layout is implied by this cause.

Traces: simpledbv2.export-lifecycle-lands-verified-data-in-s3 (the check fails at this assertion, before it reaches the later exportManifest/manifest-summary/data-file assertions, so only this cause is grounded by the replay).

## Evidence
Check: simpledbv2.export-lifecycle-lands-verified-data-in-s3 — verdict mismatch.

Flow replayed: sdb.CreateDomain(<domain>) -> 2x sdb.PutAttributes (widget-1 color=red, widget-2 color=blue) -> s3.CreateBucket + PutBucketTagging -> simpledbv2.StartDomainExport(domainName=<domain>, s3Bucket=<bucket>, s3KeyPrefix="exports/") -> poll simpledbv2.GetExport(exportArn=<arn>) until terminal.

Expected (recorded AWS): GetExport on the SUCCEEDED export returns itemsCount = 100, the fixed placeholder, regardless of the domain's real item count. The true count (2) appears only in the export's manifest-summary.json in S3.

Actual (emulator): GetExport returned itemsCount = 2.

Failure, verbatim from the replay:
  check_simpledbv2_export_lifecycle_lands_verified_data_in_s3.py, line 136, in run
    assert terminal["itemsCount"] == 100
  AssertionError: expected GetExport.itemsCount to be the fixed placeholder 100 regardless of the domain's real 2 items, got 2

The export itself reached exportStatus SUCCEEDED and the exportArn was well formed (arn:aws:sdb:<region>:<account>:domain/<domain>/export/...), so the divergence is confined to this one response member. "2" and "100": the 100 is AWS's fixed text-equivalent constant for this member and is the requirement; the 2 is this recording's domain size, an input value, not part of any template.

## Steps to Reproduce
1. Create a SimpleDB domain via the sdb (2009-04-15) endpoint: CreateDomain(DomainName=<domain>).
2. PutAttributes twice to give the domain exactly 2 items (e.g. widget-1 color=red, widget-2 color=blue) — any item count other than 100 exposes the cause.
3. Create an S3 bucket in the same region.
4. simpledbv2 StartDomainExport(domainName=<domain>, s3Bucket=<bucket>, s3KeyPrefix="exports/"); keep the returned exportArn.
5. Poll simpledbv2 GetExport(exportArn=<arn>) until exportStatus leaves PENDING/IN_PROGRESS; confirm it is SUCCEEDED.
6. Read itemsCount from that terminal GetExport response.
   AWS: 100. Emulator: 2 (the domain's real item count).
7. For contrast, GetObject the export's manifest-summary.json under the prefix and read its itemsCount — that one is expected to be the real 2 on both AWS and the emulator.
