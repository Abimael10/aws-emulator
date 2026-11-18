Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-catalog-list-document-versions
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-catalog-list-document-versions
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|ListDocumentVersions|2xx->4xx|$.DocumentVersions
Issue-Key: ssm|ListDocumentVersions|2xx->4xx|status
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-RunShellScript"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-RunPowerShellScript"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-UpdateSSMAgent"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-RunPatchBaseline"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-GatherSoftwareInventory"].CreatedDate
Issue-Commit: 0d074a71eaca712c4eb2c65274b3985903118640

# SSM document catalog: ListDocumentVersions fails for most AWS-managed documents and dates the rest from emulator start (2 causes)

> _Note: this issue was identified against the code base at commit `0d074a71eaca712c4eb2c65274b3985903118640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDocumentVersions

## Description
Two net-new root causes in the AWS-managed SSM document catalog, both on ssm. A fixer must close BOTH before submitting.

Cause 1 of 2 - ListDocumentVersions answers 400 for AWS-managed documents the emulator's catalog does not carry.
AWS: Smithy - ListDocumentVersions takes Name (required, pattern ^[a-zA-Z0-9_\\-.:/]{3,128}$) and returns ListDocumentVersionsResult.DocumentVersions, a DocumentVersionList with @length min 1; its only not-found error is InvalidDocument (client, HTTP 400). An AWS-managed document such as AWS-ConfigureAWSPackage exists in every account and region, so AWS answers HTTP 200 with one DocumentVersionInfo per version - the recording shows [{Name: 'AWS-ConfigureAWSPackage', DocumentVersion: '1', IsDefaultVersion: true, DocumentFormat: 'JSON', Status: 'Active', CreatedDate: <the document's publication date>}].
Emulator: answers HTTP 400 with no $.DocumentVersions for 15 of the AWS-managed documents the fixture walks - i.e. it treats a document AWS always publishes as non-existent, because its seeded catalog holds only a handful of documents.
Why: any client that resolves a managed document before using it (Run Command, State Manager, patch workflows, the console's document picker) is told the document does not exist and cannot proceed; the failure is a hard 400, not a degraded listing.
Fix: the ssm service's seeded AWS-managed document catalog (state) plus the ListDocumentVersions handler's lookup - an AWS-owned document must resolve to at least one Active version instead of raising the not-found path.
Traces: ssm-catalog-list-document-versions#task#4 (representative; the cluster also covers #task#6, #task#7, #task#8, #task#9, #task#10, #task#11, #task#12, #task#13, #task#14, #task#15, #task#16, #task#18, #task#19, #task#20). The same catalog gap also shows up on ListDocuments, where the AWS-managed DocumentIdentifiers are largely absent.

Cause 2 of 2 - CreatedDate of an AWS-managed document is the emulator's own seed/boot time.
AWS: Smithy - DocumentVersionInfo.CreatedDate targets `DateTime` (@timestamp), documented as "The date the document was created". For an AWS-owned document that date is a fixed property of AWS's published catalog, identical in every account, region and run: the recording has AWS-RunShellScript 1503695717.821, AWS-RunPowerShellScript 1503695741.94, AWS-UpdateSSMAgent 1622567950.979, AWS-RunPatchBaseline 1786631569.844, AWS-GatherSoftwareInventory 1564011832.472.
Emulator: returns one and the same value for all of them - 1794937152.7460456, the moment the emulator seeded its catalog (note also the sub-millisecond precision, which no AWS timestamp carries; same serializer defect as the Parameter Store timestamps filed separately).
Why: a client that caches or diffs managed-document metadata sees every document created "just now" and all on the same instant, so version/age comparisons, cache invalidation and any assertion against the published catalogue date are wrong; the values also change on every emulator restart.
Fix: the seeded AWS-managed document catalog in the ssm service state must carry each document's published CreatedDate as data, and the ListDocumentVersions/ListDocuments projections must serve it instead of the seeding clock.
Traces: ssm-catalog-list-document-versions#task#1 (AWS-RunShellScript), #task#2 (AWS-RunPowerShellScript), #task#3 (AWS-UpdateSSMAgent), #task#5 (AWS-RunPatchBaseline), #task#17 (AWS-GatherSoftwareInventory).

No open issue owns these replay keys (the replay report listed owned_by = [] for all six clusters). 20261111T045110Z-ssm-3-net-new-causes-listdocuments-serves-no.md concerns ListDocuments filter-key wording and does not carry any ListDocumentVersions step.

## Evidence
Replay of trace set simparity-wire-aws-v1 (180 fixtures, 907 steps, score 74.3%):
- ssm-catalog-list-document-versions#task#4 ListDocumentVersions: expected 200 with $.DocumentVersions = [{'CreatedDate': 1706038480.989, 'DocumentFormat': 'JSON', 'DocumentVersion': '1', 'IsDefaultVersion': True, 'Name': 'AWS-ConfigureAWSPackage', 'Status': 'Active'}]; got 400 with $.DocumentVersions missing. One cluster, 15 affected steps (task#4, 6-16, 18-20).
- ssm-catalog-list-document-versions#task#1: $.DocumentVersions[Name="AWS-RunShellScript"].CreatedDate expected 1503695717.821, got 1794937152.7460456.
- #task#2: AWS-RunPowerShellScript expected 1503695741.94, got 1794937152.7460456.
- #task#3: AWS-UpdateSSMAgent expected 1622567950.979, got 1794937152.7460456.
- #task#5: AWS-RunPatchBaseline expected 1786631569.844, got 1794937152.7460456.
- #task#17: AWS-GatherSoftwareInventory expected 1564011832.472, got 1794937152.7460456.
The single repeated actual value is the emulator's catalog-seed instant, shared by every document.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Cause 1: replay ssm-catalog-list-document-versions#task#4 - ListDocumentVersions {Name: 'AWS-ConfigureAWSPackage'} must answer 200 with one Active, default DocumentVersionInfo; it answers 400. Repeat for the other 14 AWS-managed names the fixture walks.
4. Cause 2: replay ssm-catalog-list-document-versions#task#1/#task#2/#task#3/#task#5/#task#17 and compare CreatedDate with the recorded publication dates; restart the emulator and observe the value move with the clock.
