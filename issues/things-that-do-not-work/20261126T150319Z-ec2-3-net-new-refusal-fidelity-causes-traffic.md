Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-delete-traffic-mirror-filter ec2.op-contract-delete-traffic-mirror-target ec2.op-contract-describe-declarative-policies-reports
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-delete-traffic-mirror-filter|run#24|AssertionError|assert code == "TrafficMirrorFilterInUse", f"expected TrafficMirrorFilterInUse, got {code!r}"
Issue-Key: ec2.op-contract-delete-traffic-mirror-target|run#25|AssertionError|assert code == "TrafficMirrorTargetInUse", f"expected TrafficMirrorTargetInUse, got {code!r}"
Issue-Key: ec2.op-contract-describe-declarative-policies-reports|run#14|AssertionError|assert bogus_code_msg == f"The report ID '{bogus_id}' does not exist.", (
Issue-Commit: e1287cf409e72f27d41b5362b1ef9b2f278addc7

# ec2: 3 net-new refusal-fidelity causes — traffic-mirror in-use deletes answer DependencyViolation instead of TrafficMirrorFilterInUse/TrafficMirrorTargetInUse, and the DescribeDeclarativePoliciesReports NotFound message is reworded

> _Note: this issue was identified against the code base at commit `e1287cf409e72f27d41b5362b1ef9b2f278addc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteTrafficMirrorFilter

## Description
Three independent refusal-fidelity defects on EC2 read/delete paths. Each is its own root cause and must be closed separately.

Cause 1 of 3: DeleteTrafficMirrorFilter refuses an in-use filter with the generic DependencyViolation code and message instead of the traffic-mirror-specific TrafficMirrorFilterInUse.
Failing check: ec2.op-contract-delete-traffic-mirror-filter
AWS: Smithy models no errors on DeleteTrafficMirrorFilter (errors: []), and documents the rule in the operation's own prose: "Deletes the specified Traffic Mirror filter. You cannot delete a Traffic Mirror filter that is in use by a Traffic Mirror session." The recorded behavior for that rule is an HTTP 400 EC2 query error with Code TrafficMirrorFilterInUse and Message of the fixed form `Cannot delete <filter-id>. Filter is in use by <session-id>` — the fixed text is "Cannot delete ", ". Filter is in use by "; the two variable parts are the TrafficMirrorFilterId the request named and the TrafficMirrorSessionId of the session still referencing it (taken from emulator state, not from any one recording). Note the refusal NAMES THE DEPENDENT session id, unlike the generic DependencyViolation class.
Emulator: answers Code 'DependencyViolation' with Message "The trafficMirrorFilter '<filter-id>' has dependencies and cannot be deleted." — the wrong error code, the wrong message template, and no dependent session id at all. The handler chooses this deliberately (see Traces for the file/line).
Why: a client that branches on the error code to distinguish "a session still mirrors through this filter" (retry after deleting the session) from the generic VPC-family DependencyViolation takes the wrong branch, and a client that parses the session id out of the message to know WHICH session to tear down finds nothing to parse. Terraform/CDK-style teardown loops that special-case TrafficMirrorFilterInUse never fire.
Fix: the in-use branch of the DeleteTrafficMirrorFilter handler and the EC2 error-constructor it calls — it needs a traffic-mirror-specific TrafficMirrorFilterInUse constructor carrying the dependent session id, not the shared dependency_violation helper.

Cause 2 of 3: DeleteTrafficMirrorTarget refuses an in-use target with DependencyViolation instead of TrafficMirrorTargetInUse.
Failing check: ec2.op-contract-delete-traffic-mirror-target
AWS: Smithy models no errors on DeleteTrafficMirrorTarget (errors: []); the operation documents "You cannot delete a Traffic Mirror target that is in use by a Traffic Mirror session." The recorded behavior is HTTP 400, Code TrafficMirrorTargetInUse, Message of the fixed form `Cannot delete <target-id>. Target is in use by <session-id>` — fixed text "Cannot delete " and ". Target is in use by "; variable parts are the TrafficMirrorTargetId from the request and the TrafficMirrorSessionId from state.
Emulator: answers Code 'DependencyViolation' with Message "The trafficMirrorTarget '<target-id>' has dependencies and cannot be deleted." — wrong code, wrong message, dependent session id omitted.
Why: same client breakage as Cause 1, on the target half of the traffic-mirror family; a caller cannot tell a mirror-session dependency from an ENI/VPC dependency, and cannot learn which session blocks the delete.
Fix: the in-use branch of the DeleteTrafficMirrorTarget handler and its EC2 error constructor.

Cause 3 of 3: DescribeDeclarativePoliciesReports' not-found message renames the resource and drops the terminating period.
Failing check: ec2.op-contract-describe-declarative-policies-reports
AWS: Smithy models no errors on DescribeDeclarativePoliciesReports (errors: []); ReportIds is "One or more report IDs". Recorded behavior for a well-formed but nonexistent ReportId is HTTP 400, Code InvalidDeclarativePoliciesReportId.NotFound, Message of the fixed form `The report ID '<report-id>' does not exist.` — fixed text "The report ID '", "' does not exist." including the final period; the only variable part is the report id copied from the request. The error code is already correct in the emulator; only the message diverges.
Emulator: answers the right code with Message "The declarative-policies-report ID '<report-id>' does not exist" — it substitutes the internal resource noun "declarative-policies-report" for AWS's "report" and omits the terminating period.
Why: the error message is part of the contract; log scrapers, assertion suites and support tooling that match AWS's wording on this refusal class do not match, and the message leaks an emulator-internal resource name.
Fix: the EC2 shared error constructor invalid_declarative_policies_report_id_not_found in the service's state/error module (the handler itself calls it correctly and in the right order).

## Evidence
Cause 1 — ec2.op-contract-delete-traffic-mirror-filter:
  expected: Code == "TrafficMirrorFilterInUse", Message == "Cannot delete <filter-id>. Filter is in use by <session-id>"
  actual:   AssertionError: expected TrafficMirrorFilterInUse, got 'DependencyViolation'
  emulator message template (services/service-com-amazonaws-ec2-amazonec2/src/ops/delete_traffic_mirror_filter.rs:176-179): "The trafficMirrorFilter '{filter_id}' has dependencies and cannot be deleted."
  The handler comment at lines 167-171 asserts EC2 answers DependencyViolation here; the recorded contract says otherwise.

Cause 2 — ec2.op-contract-delete-traffic-mirror-target:
  expected: Code == "TrafficMirrorTargetInUse", Message == "Cannot delete <target-id>. Target is in use by <session-id>"
  actual:   AssertionError: expected TrafficMirrorTargetInUse, got 'DependencyViolation'
  emulator message template (services/service-com-amazonaws-ec2-amazonec2/src/ops/delete_traffic_mirror_target.rs:171-174): "The trafficMirrorTarget '{target_id}' has dependencies and cannot be deleted."

Cause 3 — ec2.op-contract-describe-declarative-policies-reports:
  expected: Message == "The report ID '<report-id>' does not exist."
  actual:   AssertionError: expected the exact InvalidDeclarativePoliciesReportId.NotFound message, got "The declarative-policies-report ID 'dpr-00000000000000000' does not exist"
  (the id shown is the check's placeholder; the requirement is the template, not that value)
  refusal raised at services/service-com-amazonaws-ec2-amazonec2/src/ops/describe_declarative_policies_reports.rs:227 via state::invalid_declarative_policies_report_id_not_found

Related but NOT covering these: open issues 20261117T185234Z-ec2-10-more-net-new-causes-getimagefindings-is.md and 20261117T185234Z-ec2-7-net-new-over-permissive-causes-describeaddressesattribute.md touch other traffic-mirror refusals (ModifyTrafficMirrorFilterRule, rule-number/port-range validation); neither carries DeleteTrafficMirrorFilter, DeleteTrafficMirrorTarget or DescribeDeclarativePoliciesReports.

## Steps to Reproduce
Cause 1: CreateVpc + CreateSubnet; create two network interfaces and attach each to a running Nitro instance (RunInstances with NetworkInterfaces[0].NetworkInterfaceId); CreateTrafficMirrorTarget on the target ENI; CreateTrafficMirrorFilter; CreateTrafficMirrorSession(NetworkInterfaceId=source ENI, TrafficMirrorTargetId, TrafficMirrorFilterId, SessionNumber=1). Then call DeleteTrafficMirrorFilter(TrafficMirrorFilterId). Observe Code 'DependencyViolation' where AWS answers 'TrafficMirrorFilterInUse' with "Cannot delete <filter-id>. Filter is in use by <session-id>".

Cause 2: same estate; call DeleteTrafficMirrorTarget(TrafficMirrorTargetId) while the session exists. Observe Code 'DependencyViolation' where AWS answers 'TrafficMirrorTargetInUse' with "Cannot delete <target-id>. Target is in use by <session-id>".

Cause 3: call DescribeDeclarativePoliciesReports(ReportIds=["dpr-00000000000000000"]) on an account with no reports. The code InvalidDeclarativePoliciesReportId.NotFound is correct; compare the Message against "The report ID 'dpr-00000000000000000' does not exist." (note the resource noun and the trailing period).

Re-replay: ec2.op-contract-delete-traffic-mirror-filter, ec2.op-contract-delete-traffic-mirror-target, ec2.op-contract-describe-declarative-policies-reports
