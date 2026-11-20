Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ssm.ssm-patch-baseline-scan-reports-compliance-for-a-real-instance
Issue-Key: ec2__ssm.ssm-patch-baseline-scan-reports-compliance-for-a-real-instance|run#52|AssertionError|assert by_state.get(patch_state, 0) == summary_count, (
Issue-Commit: a14965d6060559849ea419b74faa25e7836c4558

# ec2__ssm: a Patch Manager scan of a live Amazon Linux 2023 managed node inventories nothing installed — DescribeInstancePatchStates reports InstalledCount 0 and a synthetic 12 MISSING

> _Note: this issue was identified against the code base at commit `a14965d6060559849ea419b74faa25e7836c4558`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeInstancePatches

## Description
One net-new root cause. No open issue carries this check.

Cause 1 of 1: the patch scan does not read the managed node's package database.
AWS: an AWS-RunPatchBaseline scan against a real Amazon Linux 2023 managed node reads the node's own RPM database, so DescribeInstancePatchStates reports a non-zero InstalledCount (a real host has hundreds of installed packages) alongside whatever is Missing, and DescribeInstancePatches returns one PatchComplianceData record per patch whose per-state tally reproduces the named counters exactly (InstalledCount, InstalledOtherCount, InstalledPendingRebootCount, InstalledRejectedCount, MissingCount, FailedCount), with NotApplicableCount bounded below by the NotApplicable records. Smithy (ssm): PatchComplianceData.State targets PatchComplianceDataState, whose values are INSTALLED, INSTALLED_OTHER, INSTALLED_PENDING_REBOOT, INSTALLED_REJECTED, MISSING, NOT_APPLICABLE, FAILED, AVAILABLE_SECURITY_UPDATE; the per-state counters on InstancePatchState are the summary of exactly those records. The requirement is that the two views agree and that they describe the node's real package state — recorded behavior only for the actual counts.
Emulator: the scan produces a fabricated record set that no node was read for — DescribeInstancePatchStates reports {'Installed': 0, 'InstalledOther': 0, 'InstalledPendingReboot': 0, 'InstalledRejected': 0, 'Missing': 12, 'Failed': 0} and DescribeInstancePatches returns 12 records all in state MISSING and nothing installed. A live Amazon Linux 2023 guest cannot have zero installed patches, so the scan never inventoried the node; the summary and the record set are generated side by side rather than derived from the host.
Why: Patch Manager is used to decide whether a fleet is compliant. A scan that always reports nothing installed and a fixed set of missing patches makes every node look identically non-compliant regardless of its real state, so patch compliance reporting and any remediation driven off InstalledCount/MissingCount is meaningless.
Fix: the SSM Patch Manager scan path — the AWS-RunPatchBaseline scan must inventory the managed node's installed packages and derive both the DescribeInstancePatches record set and the DescribeInstancePatchStates counters from that inventory.
Note for the fixer: the assertion that tripped first compares the record-set tally against the summary counters, and the emulator's record State value 'MISSING' is the wire-correct enum value per the Smithy model above — do NOT change the enum casing to make the comparison pass; the defect is that the scan reports a synthetic 12-missing/0-installed result for a real node.
Traces: ec2__ssm.ssm-patch-baseline-scan-reports-compliance-for-a-real-instance.

## Evidence
Check ec2__ssm.ssm-patch-baseline-scan-reports-compliance-for-a-real-instance (t3.micro, AMAZON_LINUX_2023):
  expected: the per-state tally of DescribeInstancePatches reproduces the named counters of DescribeInstancePatchStates, and a scan of a real Amazon Linux host reports InstalledCount > 0.
  actual:   AssertionError: "the summary reports 12 patches in state 'Missing' but DescribeInstancePatches returned 0; full record tally {'MISSING': 12} against summary {'Installed': 0, 'InstalledOther': 0, 'InstalledPendingReboot': 0, 'InstalledRejected': 0, 'Missing': 12, 'Failed': 0}" — i.e. the whole record set is 12 MISSING records and zero installed patches for a live AL2023 node.
(The count 12 is this run's value; the requirement is that the counters and records both derive from the node's real package inventory, with a non-zero installed count.)

## Steps to Reproduce
1. Launch a t3.micro Amazon Linux 2023 instance with an instance profile carrying AmazonSSMManagedInstanceCore and wait for it to become an SSM managed node.
2. CreatePatchBaseline for OperatingSystem AMAZON_LINUX_2023 and RegisterPatchBaselineForPatchGroup; tag the instance into that patch group.
3. SendCommand with AWS-RunPatchBaseline, Operation=Scan, and wait for the invocation to reach a terminal status.
4. Call ssm.DescribeInstancePatchStates(InstanceIds=[<id>]) and ssm.DescribeInstancePatches(InstanceId=<id>) and compare: the record set is 12 MISSING entries and InstalledCount is 0.
