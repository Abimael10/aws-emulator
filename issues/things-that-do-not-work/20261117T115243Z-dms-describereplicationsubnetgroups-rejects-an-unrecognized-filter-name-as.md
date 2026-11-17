Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dms.subnet-group-describe-rejects-unrecognized-filter-name
Issue-Shape-Id: com.amazonaws.databasemigrationservice#AmazonDMSv20160101
Issue-Key: dms.subnet-group-describe-rejects-unrecognized-filter-name|run#17|AssertionError|assert code == "InvalidParameterValueException", (
Issue-Commit: 344b8f3034cadbab62678adde891e6a7e93d6f2a

# DMS DescribeReplicationSubnetGroups rejects an unrecognized Filter.Name as ValidationException with its own wording, where AWS answers 400 InvalidParameterValueException "Unrecognized filter name: <name>"

> _Note: this issue was identified against the code base at commit `344b8f3034cadbab62678adde891e6a7e93d6f2a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeReplicationSubnetGroups

## Description
One net-new root cause.

Cause 1 of 1: the unrecognized-filter-name rejection of DescribeReplicationSubnetGroups carries the wrong error code and the wrong message. Failing check: dms.subnet-group-describe-rejects-unrecognized-filter-name.

AWS: Smithy models Filter.Name as a required String and DescribeReplicationSubnetGroupsMessage.Filters' member doc states "Filters applied to replication subnet groups. Valid filter names: replication-subnet-group-id", so a name outside that set is a rejection and not a no-op. The model does NOT declare the error for this operation (DescribeReplicationSubnetGroups models only ResourceNotFoundFault, and the shape InvalidParameterValueException does not exist in com.amazonaws.databasemigrationservice), so this is recorded behavior only: the recording shows the call refused with HTTP 400, Error.Code `InvalidParameterValueException`, and Error.Message `Unrecognized filter name: <filter name>`. Message template: the fixed text is exactly `Unrecognized filter name: ` and the single variable part is the offending Filter.Name taken verbatim from the request (in the recording the request's name was an arbitrary unsupported value, which fills the template and is not part of it). The rejection is per-request validation only: after it, a subsequent DescribeReplicationSubnetGroups using the documented `replication-subnet-group-id` filter still returns the matching group, which the check confirms as a positive control.

Emulator: the call is refused, but with error code `ValidationException` and the emulator's own invented message, of the form `<name> is not a supported filter name; the valid filter names are replication-subnet-group-id`. The replay reports: "an unrecognized filter name was rejected as 'ValidationException', expected 'InvalidParameterValueException'". So both the code and the message diverge; the message assertion is not even reached because the code check fails first. The refusal comes from the handler's own filter-name guard, which routes through the crate's generic scaffold validation error channel rather than emitting the DMS parameter-value fault.

Why: a client cannot classify the failure. botocore surfaces the code verbatim, so `except dms.exceptions...`/`Error.Code == "InvalidParameterValueException"` branches never fire and the error falls through to a generic handler or is retried as an unknown fault; error-code-driven routing, alarms and tests that distinguish a caller's bad filter name from a server-side validation fault all mis-bucket it. Code that logs or surfaces the message to a user shows wording real AWS never emits, and any parsing of `Unrecognized filter name: <name>` to recover which filter was rejected gets nothing.

Fix: the DescribeReplicationSubnetGroups handler's filter-name validation. It must emit the DMS parameter-value fault — HTTP 400, code `InvalidParameterValueException`, message `Unrecognized filter name: ` followed by the request's Filter.Name — instead of the generic scaffold validation error. Since the operation's modeled error set does not contain this fault, the error channel for this operation has to be able to carry it (as the handler's non-modeled refusal path), not be squeezed into ValidationException. The same unrecognized-name rule applies to every DMS Describe* filter guard, so prefer one shared DMS helper over an operation-local string.

Traces: dms.subnet-group-describe-rejects-unrecognized-filter-name. Handler and the guard that produces the wrong code are at services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/describe_replication_subnet_groups.rs lines 165-166 (the call) and 201-218 (`DOCUMENTED_FILTERS` / `check_filter_names`, which returns `crate::scaffolded::Scaffolded::validation(...)`).

## Evidence
Failing check: dms.subnet-group-describe-rejects-unrecognized-filter-name [mismatch]

Request (after the check creates a VPC, two subnets in two available AZs, the fixed-name dms-vpc-role, and a replication subnet group):
  dms.describe_replication_subnet_groups(Filters=[{"Name": "not-a-real-filter", "Values": [<subnet group id>]}])

Expected (recorded AWS):
  HTTP 400
  Error.Code    = "InvalidParameterValueException"
  Error.Message = "Unrecognized filter name: not-a-real-filter"
  (fixed text "Unrecognized filter name: "; variable part = the Filter.Name from the request)

Actual (emulator):
  Error.Code    = "ValidationException"
  Error.Message = "not-a-real-filter is not a supported filter name; the valid filter names are replication-subnet-group-id"

Replay assertion:
  File "check_dms_subnet_group_describe_rejects_unrecognized_filter_name.py", line 121, in run
    assert code == "InvalidParameterValueException", (
  AssertionError: an unrecognized filter name was rejected as 'ValidationException', expected 'InvalidParameterValueException'

The check's second assertion (message == "Unrecognized filter name: <name>") is unreached but equally unsatisfied by the emulator's wording.

The positive control in the same check — describe_replication_subnet_groups with Filters=[{"Name": "replication-subnet-group-id", ...}] returning exactly one group with the matching ReplicationSubnetGroupIdentifier — is the behavior that must keep working after the fix.

Source of the divergence: services/service-com-amazonaws-databasemigrationservice-amazondmsv20160101/src/ops/describe_replication_subnet_groups.rs:207-218, where check_filter_names returns Scaffolded::validation(format!("{} is not a supported filter name; the valid filter names are {}", ...)).

## Steps to Reproduce
1. Create a VPC and two subnets in two distinct available, opt-in-not-required availability zones.
2. Ensure the fixed-name IAM service role dms-vpc-role exists (trust policy for dms.amazonaws.com, AmazonDMSVPCManagementRole attached).
3. Call dms.CreateReplicationSubnetGroup with a fresh ReplicationSubnetGroupIdentifier, a description, and the two SubnetIds.
4. Call dms.DescribeReplicationSubnetGroups with Filters=[{"Name": "not-a-real-filter", "Values": [<that identifier>]}] — any Filter.Name other than "replication-subnet-group-id".
5. Observe the refusal: the emulator returns Error.Code "ValidationException" with message "not-a-real-filter is not a supported filter name; the valid filter names are replication-subnet-group-id". AWS returns HTTP 400 with Error.Code "InvalidParameterValueException" and Error.Message "Unrecognized filter name: not-a-real-filter".
6. Call dms.DescribeReplicationSubnetGroups with Filters=[{"Name": "replication-subnet-group-id", "Values": [<that identifier>]}] and confirm it still returns exactly that one group (this part already passes and must not regress).
