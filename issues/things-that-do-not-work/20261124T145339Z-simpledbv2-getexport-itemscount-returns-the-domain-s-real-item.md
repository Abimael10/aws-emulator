Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: simpledbv2.export-lifecycle-lands-verified-data-in-s3
Issue-Shape-Id: com.amazonaws.simpledbv2#SimpleDBv2
Issue-Key: simpledbv2.export-lifecycle-lands-verified-data-in-s3|run#16|AssertionError|assert terminal["itemsCount"] == 100, (
Issue-Commit: f32430db6efba8f0e69b19469a03fa7e522d5fbe

# GetExport.itemsCount returns the domain's real item count instead of AWS's fixed 100 placeholder

> _Note: this issue was identified against the code base at commit `f32430db6efba8f0e69b19469a03fa7e522d5fbe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExport

## Description
One root cause.

Cause 1 of 1: GetExport reports a computed item count where AWS reports a constant.
Failing check id: simpledbv2.export-lifecycle-lands-verified-data-in-s3

AWS: GetExport (POST /v2/GetExport, output shape GetExportResponse) returns the optional member `itemsCount` (target ItemsCount, long, @range min 0). The Smithy model documents it as "Total number of exported items.", but the recorded AWS behavior does NOT honour that documentation: on a SUCCEEDED export, real SimpleDBv2 answers `itemsCount` as a fixed placeholder value of 100, independent of how many items the exported domain actually contained. The authoritative per-export count is published only in the export's own manifest object in S3 (`.../AWSSimpleDB/<exportId>/<domainName>/manifest-summary.json`, field `itemsCount`), which does reflect the real number of items. Recorded behavior only for the constant — the model states the prose meaning, the recording states the constant, and where they conflict the recorded wire value is what clients observe. The call is a normal 200 in both AWS and the emulator; only the value of this one member diverges.

Emulator: for an export of a domain holding 2 items (two PutAttributes of `widget-1` and `widget-2`), after the export reached SUCCEEDED, GetExport returned `itemsCount` = 2 — i.e. the emulator computes the real number of exported items and serves that, rather than serving the fixed 100 that AWS serves. The replay failed at check_simpledbv2_export_lifecycle_lands_verified_data_in_s3.py line 136: "AssertionError: expected GetExport.itemsCount to be the fixed placeholder 100 regardless of the domain's real 2 items, got 2".

Why: a client calibrated against AWS reads GetExport.itemsCount as a meaningless constant and goes to the S3 manifest-summary.json for the true count. Against the emulator the same field carries a real, varying count, so (a) any client or test that pins/filters on the AWS constant (e.g. asserting itemsCount == 100, or detecting "placeholder" responses) breaks on the emulator, and (b) the emulator teaches the opposite lesson about which source of truth is trustworthy — code written against the emulator will trust GetExport.itemsCount and then read a bogus 100 in production, silently mis-reporting export sizes. The divergence is bidirectional and only visible on domains whose item count is not 100.

Fix: the GetExport handler / export-state projection for simpledbv2 — wherever the export record's item tally is projected onto the GetExportResponse `itemsCount` member. GetExport must serialize the fixed 100 for a terminal export regardless of the stored tally; the real tally must continue to be written only into the manifest-summary.json object that the export emits to S3 (the manifest's own `itemsCount` is separately asserted by this check to equal the real number of items, so that path must keep the true value). The same placeholder applies to any ListExports/export-summary projection of the field if one exists.

Traces: simpledbv2.export-lifecycle-lands-verified-data-in-s3 (StartDomainExport on a 2-item domain, poll GetExport to SUCCEEDED, inspect itemsCount). Note the check aborts at this assertion, so the later assertions in the same check (exportManifest not resolving to a real S3 object, the /AWSSimpleDB/ key layout, manifest-file.json data files and exported item content, ListExports membership) were never evaluated on this run and may hide further divergences once itemsCount is corrected — re-replay the whole check after the fix.

## Evidence
Check: simpledbv2.export-lifecycle-lands-verified-data-in-s3 (mismatch)

Setup: create_domain(<domain>); put_attributes(widget-1 color=red); put_attributes(widget-2 color=blue); start_domain_export(domainName=<domain>, s3Bucket=<bucket>, s3KeyPrefix="exports/"); poll get_export(exportArn=<arn>) until exportStatus leaves PENDING/IN_PROGRESS (reached SUCCEEDED).

Expected (recorded AWS): GetExport response member itemsCount == 100 — a fixed placeholder, unrelated to the 2 items the domain actually held. Smithy: GetExportResponse.itemsCount, target ItemsCount (long, @range min 0), documented "Total number of exported items."; the recording shows the constant instead.

Actual (emulator): GetExport response member itemsCount == 2 (the domain's real item count).

Failure text, verbatim:
  File "check_simpledbv2_export_lifecycle_lands_verified_data_in_s3.py", line 136, in run
    assert terminal["itemsCount"] == 100
  AssertionError: expected GetExport.itemsCount to be the fixed placeholder 100 regardless of the domain's real 2 items, got 2

Fixed vs variable: the value 100 is AWS's fixed, request-independent text for this member. The values that are per-run and NOT requirements: the domain name, bucket name, export id/exportArn, requestedAt timestamps, and the number 2 (it is just this recording's real item count — any domain with an item count other than 100 exposes the same defect).

HTTP status and error code: none diverge — both AWS and the emulator answer GetExport with HTTP 200 and no error. The divergence is confined to the itemsCount member's value.

## Steps to Reproduce
1. Boot the emulator and point botocore at it with clients for `sdb`, `simpledbv2` and `s3`.
2. sdb.create_domain(DomainName=D) for a fresh domain name D.
3. sdb.put_attributes(DomainName=D, ItemName="widget-1", Attributes=[{"Name":"color","Value":"red"}]) and sdb.put_attributes(DomainName=D, ItemName="widget-2", Attributes=[{"Name":"color","Value":"blue"}]) — any item count other than 100 works.
4. s3.create_bucket(Bucket=B) (with CreateBucketConfiguration.LocationConstraint outside us-east-1).
5. resp = simpledbv2.start_domain_export(domainName=D, s3Bucket=B, s3KeyPrefix="exports/"); arn = resp["exportArn"].
6. Poll g = simpledbv2.get_export(exportArn=arn) until g["exportStatus"] not in ("PENDING","IN_PROGRESS"); it reaches "SUCCEEDED".
7. Inspect g["itemsCount"]. AWS returns 100; the emulator returns 2 (the real number of items put in step 3).
8. Equivalent single command: replay the behavioral check simpledbv2.export-lifecycle-lands-verified-data-in-s3.
