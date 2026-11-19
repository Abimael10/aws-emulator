Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-catalog-list-document-versions
Issue-Step: task#18
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-catalog-list-document-versions
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-ConfigureCloudWatch"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-PatchInstanceWithRollback"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-RunInspecChecks"].CreatedDate
Issue-Commit: 0a8d788699eb3929d2d95f97973e466fa561ffef

# SSM ListDocumentVersions version metadata is stale for CloudWatch, patch and Inspec runbooks

> _Note: this issue was identified against the code base at commit `0a8d788699eb3929d2d95f97973e466fa561ffef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDocumentVersions

## Description
3 causes. Same fault as the companion issue 'SSM ListDocumentVersions reports wrong CreatedDate and DocumentFormat for Amazon-owned documents'; split out only because a bundle carries at most ten causes. A fixer must close these three as well as that issue's ten.

AWS (common to all three): Smithy: ListDocumentVersions output DocumentVersions -> DocumentVersionInfo{Name, DocumentVersion, CreatedDate (DateTime, 'The date the document was created'), IsDefaultVersion, DocumentFormat, Status}. For an Amazon-owned document the per-version creation timestamp is catalog data AWS publishes identically to every account; it is not derived from the request, the caller, or the run.
Emulator (common to all three): the seeded catalog stores its own invented creation timestamps for these documents, so each version record reports a CreatedDate AWS never reports for that document.
Why (common to all three): clients compare this timestamp to detect that an Amazon-owned runbook has been republished and to order versions; a seeded value unrelated to AWS's catalog makes that comparison wrong in both directions (stale documents look fresh, republished ones look unchanged).
Fix (common to all three): the ssm crate's seeded Amazon-owned document catalog (state) - the per-version CreatedDate it stores - read by the ListDocumentVersions handler. The recorded epochs are evidence, not the requirement.

Cause 1 of 3: AWS-ConfigureCloudWatch - CreatedDate diverges (fixture ssm-catalog-list-document-versions, step task#18).
Cause 2 of 3: AWS-PatchInstanceWithRollback - CreatedDate diverges (step task#19).
Cause 3 of 3: AWS-RunInspecChecks - CreatedDate diverges (step task#20).
Traces: ssm-catalog-list-document-versions#task#18, #task#19, #task#20.

## Evidence
All three steps are ListDocumentVersions, expected status 200, actual status 200:
- task#18: $.DocumentVersions[Name="AWS-ConfigureCloudWatch"].CreatedDate: value_mismatch (expected 1525720957.739, got 1503695816.733)
- task#19: $.DocumentVersions[Name="AWS-PatchInstanceWithRollback"].CreatedDate: value_mismatch (expected 1761232679.723, got 1514908866.117)
- task#20: $.DocumentVersions[Name="AWS-RunInspecChecks"].CreatedDate: value_mismatch (expected 1758834488.387, got 1551308281.476)

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Replay fixture ssm-catalog-list-document-versions steps task#18, task#19 and task#20 - ListDocumentVersions for AWS-ConfigureCloudWatch, AWS-PatchInstanceWithRollback and AWS-RunInspecChecks.
4. Compare DocumentVersions[*].CreatedDate in each response against the recording.
