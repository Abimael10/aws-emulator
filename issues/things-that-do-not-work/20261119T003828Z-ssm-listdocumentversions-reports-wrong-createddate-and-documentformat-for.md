Issue-Source: trace-sweep-divergence
Issue-Trace: ssm-catalog-list-document-versions
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ssm-catalog-list-document-versions
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-InstallWindowsUpdates"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-InstallWindowsUpdates"].DocumentFormat
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-StartSSHSession"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-StartPortForwardingSession"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-ApplyAnsiblePlaybooks"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-RunRemoteScript"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-StopEC2Instance"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-StartEC2Instance"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-RestartEC2Instance"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-CreateImage"].CreatedDate
Issue-Key: ssm|ListDocumentVersions|2xx->2xx|$.DocumentVersions[Name="AWS-UpdateLinuxAmi"].CreatedDate
Issue-Commit: 0a8d788699eb3929d2d95f97973e466fa561ffef

# SSM ListDocumentVersions reports wrong CreatedDate and DocumentFormat for Amazon-owned documents

> _Note: this issue was identified against the code base at commit `0a8d788699eb3929d2d95f97973e466fa561ffef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDocumentVersions

## Description
10 causes, one per Amazon-owned document whose seeded version metadata diverges from what AWS publishes. Each cause is one ListDocumentVersions call in fixture ssm-catalog-list-document-versions.

AWS (common to all ten): Smithy: ListDocumentVersions output DocumentVersions -> DocumentVersionInfo{Name, DocumentVersion, CreatedDate (DateTime, 'The date the document was created'), IsDefaultVersion, DocumentFormat (enum JSON|YAML|TEXT, 'The document format, either JSON or YAML'), Status}. For an Amazon-owned document these are catalog facts published by AWS and identical in every account: a given version of AWS-InstallWindowsUpdates has one publication timestamp and one authored format. They are not per-run or per-environment values - nothing in the request or the caller's state influences them.
Emulator (common to all ten): the seeded catalog carries its own invented publication timestamps, and for AWS-InstallWindowsUpdates the wrong authored format, so every version record reports a CreatedDate (and in one case a DocumentFormat) that AWS never reports for that document.
Why (common to all ten): clients that cache or compare document metadata - 'has this runbook changed since I last read it', console version pickers, tooling that parses a document after branching on DocumentFormat - key off exactly these fields. A wrong DocumentFormat makes a YAML-authored document be parsed as JSON; a wrong CreatedDate makes change detection and version ordering against the real catalog disagree.
Fix (common to all ten): the ssm crate's seeded Amazon-owned document catalog (state) - the per-version CreatedDate and DocumentFormat it stores - read by the ListDocumentVersions handler. The recorded numbers below are evidence of divergence, not the requirement: the requirement is that each seeded Amazon-owned document version carries AWS's published creation timestamp and its authored format.

Cause 1 of 10: AWS-InstallWindowsUpdates - CreatedDate diverges AND DocumentFormat is reported 'JSON' where AWS reports 'YAML' (fixture ssm-catalog-list-document-versions, step task#6).
Cause 2 of 10: AWS-StartSSHSession - CreatedDate diverges (step task#7).
Cause 3 of 10: AWS-StartPortForwardingSession - CreatedDate diverges (step task#8).
Cause 4 of 10: AWS-ApplyAnsiblePlaybooks - CreatedDate diverges (step task#9).
Cause 5 of 10: AWS-RunRemoteScript - CreatedDate diverges (step task#10).
Cause 6 of 10: AWS-StopEC2Instance - CreatedDate diverges (step task#11).
Cause 7 of 10: AWS-StartEC2Instance - CreatedDate diverges (step task#12).
Cause 8 of 10: AWS-RestartEC2Instance - CreatedDate diverges (step task#13).
Cause 9 of 10: AWS-CreateImage - CreatedDate diverges (step task#14).
Cause 10 of 10: AWS-UpdateLinuxAmi - CreatedDate diverges (step task#15).

Three further documents in the same fixture show the identical fault and are filed separately only because one issue bundles at most ten causes; see the companion issue titled 'SSM ListDocumentVersions version metadata is stale for CloudWatch, patch and Inspec runbooks'.
Traces: ssm-catalog-list-document-versions#task#6 through #task#15 (task#16 is the missing-document cause, filed with the catalog-completeness issue).

## Evidence
All ten steps are ListDocumentVersions, expected status 200, actual status 200, differing only in the DocumentVersionInfo fields:
- task#6 AWS-InstallWindowsUpdates: $.DocumentVersions[Name="AWS-InstallWindowsUpdates"].CreatedDate: value_mismatch (expected 1781017685.843, got 1503695769.206); .DocumentFormat: value_mismatch (expected 'YAML', got 'JSON')
- task#7 AWS-StartSSHSession: CreatedDate expected 1562087817.013, got 1555361827.515
- task#8 AWS-StartPortForwardingSession: CreatedDate expected 1563487178.453, got 1566497019.264
- task#9 AWS-ApplyAnsiblePlaybooks: CreatedDate expected 1758834443.39, got 1563822104.358
- task#10 AWS-RunRemoteScript: CreatedDate expected 1508978913.261, got 1503695784.612
- task#11 AWS-StopEC2Instance: CreatedDate expected 1531242313.476, got 1514908809.447
- task#12 AWS-StartEC2Instance: CreatedDate expected 1782421644.712, got 1514908801.332
- task#13 AWS-RestartEC2Instance: CreatedDate expected 1511217229.956, got 1514908817.519
- task#14 AWS-CreateImage: CreatedDate expected 1533661478.787, got 1514908833.712
- task#15 AWS-UpdateLinuxAmi: CreatedDate expected 1775747623.945, got 1514908858.046
The DocumentFormat divergence (task#6) is format metadata, not a timestamp, and cannot vary by environment.

## Steps to Reproduce
1. Clone the repo and boot the emulator built from it.
2. Replay the wire corpus (trace set simparity-wire-aws-v1).
3. Replay fixture ssm-catalog-list-document-versions steps task#6 .. task#15 - each is ListDocumentVersions with Name set to one Amazon-owned document.
4. For each response compare DocumentVersions[*].CreatedDate (and, for AWS-InstallWindowsUpdates, DocumentFormat) against the recorded values.
