Issue-Source: trace-sweep-divergence
Issue-Trace: simpledbv2-export-domain-to-s3-succeeds
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: simpledbv2-export-domain-to-s3-succeeds
Issue-Shape-Id: com.amazonaws.simpledbv2#SimpleDBv2
Issue-Key: sdb|GetExport|2xx->2xx|$.exportManifest
Issue-Commit: d480a9c819aca039333e6ccd75fa51254e2ac722

# GetExport reports an exportManifest key that omits the AmazonSimpleDB/domain/export-ARN path segments

> _Note: this issue was identified against the code base at commit `d480a9c819aca039333e6ccd75fa51254e2ac722`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetExport

## Description
Replaying the recorded domain-export success fixture, the sdb step that reads a SUCCEEDED export back with GetExport now answers 200 (the end-to-end refusal filed as 20261110T055258Z-sdb-wire-replay-domain-export-lifecycle-fixture-refused-end.md no longer reproduces on these steps), but the body's `exportManifest` names a different S3 key than the one AWS reports. This issue carries ONE cause.

Cause 1 of 1: the manifest-summary key a finished export reports is built from the key prefix and the bare export id only (fixture simpledbv2-export-domain-to-s3-succeeds, step task#5).
  AWS: Smithy: GetExport POST /v2/GetExport, output member `exportManifest` -> ExportManifestSummary (@length min 1), documented as "The name of the manifest summary file for the export."; the model states no layout for the key, so the layout itself is recorded behavior only. The recording shows that for an export whose `exportStatus` is SUCCEEDED the key is composed of FOUR parts after the request's `s3KeyPrefix`: the service-fixed segment `/AmazonSimpleDB/`, then the exported `domainName`, then the export's full `exportArn` (itself `arn:aws:sdb:<region>:<account>:domain/<domainName>/export/<exportId>`), then the fixed file name `manifest-summary.json`. In the recorded step the request's prefix is reproduced verbatim and not normalised, so the prefix and the fixed segment's leading separator appear adjacently in the key. The variable parts are the request's `s3KeyPrefix`, the request's `domainName`, and the ARN/export id this service minted; the fixed parts are `AmazonSimpleDB` and `manifest-summary.json`.
  Emulator: it answers 200 and reports `<s3KeyPrefix><exportId>/manifest-summary.json` — the prefix concatenated directly with the bare export id and the file name. The `/AmazonSimpleDB/` segment, the exported domain name and the export ARN are all absent, and the prefix is concatenated without a separator. Replay: `$.exportManifest` value_mismatch, expected `exports//AmazonSimpleDB/<domainName>/<exportArn>/manifest-summary.json`, got `exports/<exportId>/manifest-summary.json` (the concrete domain name, ARN and export id in the recording are per-run values, not requirements).
  Why: `exportManifest` is the only pointer a client has to the export's output. A client that takes the reported key and GETs it from the bucket — or that lists/builds the AWS key layout `<prefix>/AmazonSimpleDB/<domain>/<exportArn>/...` to find the data files beside the manifest — looks in a place that does not exist against AWS, and against the emulator the manifest object is written at the same wrong key, so neither the reported key nor the AWS-shaped key agrees across the two. Any tooling that parses the domain or the export ARN out of the manifest key also gets nothing.
  Fix: the export record's manifest-key derivation in this service's state (the value both the GetExport/ListExports serializers report and the export's own run writes the object to), so the key carries the fixed `AmazonSimpleDB` segment, the exported domain name and the export ARN between the caller's prefix and `manifest-summary.json`.
  Traces: simpledbv2-export-domain-to-s3-succeeds#task#5. Code evidence: services/service-com-amazonaws-simpledbv2-simpledbv2/src/state.rs:208-222 (`manifest_key()` = s3_key_prefix + export_id + "/manifest-summary.json"), state.rs:192-193 (the value is captured onto the record when the export settles to SUCCEEDED), src/state/domain_export_run.rs:1-45 (the run writes its objects under that same key space).

No open issue carries this step's replay key: 20261110T055258Z-sdb-...-refused-end.md covers steps setup#1, setup#2, task#4, task#6 and teardown#9 of this fixture (all 200->400 refusals) and does not mention task#5 or `exportManifest`; the two REOPENS issues cover the 2009-04-15 SigV2 CreateDomain refusal.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture `simpledbv2-export-domain-to-s3-succeeds`, step `task#5` (service sdb, operation GetExport), verdict mismatch:

- Expected status 200, actual status 200 — the status agrees; the divergence is the body value.
- `$.exportManifest`: value_mismatch. Expected `exports//AmazonSimpleDB/sim2real-probe-simpledbv2-domain-3f96c313/arn:aws:sdb:us-west-2:000000000000:domain/sim2real-probe-simpledbv2-domain-3f96c313/export/c8f69479-b761-4721-a354-82eb8cd0a6b7/manifest-summary.json`; got `exports/c8f69479-b761-4721-a354-82eb8cd0a6b7/manifest-summary.json`.
- Reading the recorded value as a template: `<s3KeyPrefix>` (from the request) + `/AmazonSimpleDB/` (fixed) + `<domainName>` (from the request) + `/` + `<exportArn>` (minted by the service) + `/manifest-summary.json` (fixed). The domain name `sim2real-probe-simpledbv2-domain-3f96c313`, the account `000000000000`, the region `us-west-2` and the export id `c8f69479-b761-4721-a354-82eb8cd0a6b7` are that run's values and are NOT part of the requirement.
- The emulator's value keeps only `<s3KeyPrefix>` + `<exportId>` + `/manifest-summary.json`, which matches services/service-com-amazonaws-simpledbv2-simpledbv2/src/state.rs:216-222 exactly.
- Corpus summary for this run: 19 steps, 17 passed, 2 mismatched, score 89.5%; the other mismatch (s3 DeleteBucket, teardown#8) is already owned by an open issue.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1.
2. Observe fixture `simpledbv2-export-domain-to-s3-succeeds` diverge at step `task#5` (GetExport): status 200 as recorded, but `$.exportManifest` mismatched.
3. By hand: POST /v2/StartDomainExport with {"domainName":"<an existing domain>","s3Bucket":"<a bucket>","s3KeyPrefix":"exports/"}, wait ~1s for the export to settle, then POST /v2/GetExport with the returned {"exportArn":...}.
4. Observe `exportStatus` SUCCEEDED with `exportManifest` = "exports/<exportId>/manifest-summary.json", instead of a key carrying the fixed `AmazonSimpleDB` segment, the exported domain name and the export ARN between the prefix and `manifest-summary.json`.
5. Re-replay to confirm the fix: simpledbv2-export-domain-to-s3-succeeds#task#5.
