Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation__ec2.cfn-ec2-security-group-ingress-update-in-place-replace-retained-then-delete cloudformation__ec2.deploy-ec2-security-group-ingress-invalid-port-range-rejected-and-rolled-back cloudformation__ec2.deploy-ec2-security-group-egress-invalid-port-range-rejected-and-rolled-back
Issue-Key: cloudformation__ec2.cfn-ec2-security-group-ingress-update-in-place-replace-retained-then-delete|_expect#16|AssertionError|raise AssertionError(
Issue-Key: cloudformation__ec2.deploy-ec2-security-group-ingress-invalid-port-range-rejected-and-rolled-back|run#8|AssertionError|assert got_error == _REJECTED, (
Issue-Key: cloudformation__ec2.deploy-ec2-security-group-egress-invalid-port-range-rejected-and-rolled-back|run#8|AssertionError|assert got_error == _REJECTED, (
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# cloudformation__ec2: 3 net-new causes — an in-place AWS::EC2::SecurityGroupIngress update answers NotImplemented, and the inverted-port-range refusal surfaced in the stack event carries the wrong message and the wrong SDK tail

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateStack

## Description
Three net-new root causes in the CloudFormation-provisioned EC2 security-group surface.

=== Cause 1 of 3: updating the properties of an existing AWS::EC2::SecurityGroupIngress is not implemented, so the stack rolls back ===
AWS: recorded behavior only. An UpdateStack that changes the properties of an AWS::EC2::SecurityGroupIngress resource is applied — the ingress rule is updated (replaced where the property requires replacement, with the retained-replacement semantics the template declares) and the stack reaches UPDATE_COMPLETE.
Emulator: the resource's UPDATE fails and the stack ends UPDATE_ROLLBACK_COMPLETE with StackStatusReason `The following resource(s) failed to update: [Ingress].` and the resource event `Ingress  UPDATE_FAILED  NotImplemented: this emulator does not change the properties of an existing AWS::EC2::SecurityGroupIngress in place; the resource was left as it is.` The `NotImplemented` wording is emulator-authored text that no AWS stack event ever carries.
Why: a client cannot evolve a security group's ingress through its stack at all — the ordinary act of widening or narrowing a rule in a template rolls the whole update back, and the failure reason tells the client it is an emulator limitation rather than anything a template change can fix.
Fix: the AWS::EC2::SecurityGroupIngress resource provider in the cloudformation service must implement the update path (in-place where the property allows it, replacement otherwise) rather than returning a NotImplemented resource failure.
Traces: cloudformation__ec2.cfn-ec2-security-group-ingress-update-in-place-replace-retained-then-delete.

=== Cause 2 of 3: the inverted-port-range refusal message is emulator-authored prose rather than AWS's ===
AWS: a security-group rule whose FromPort is above its ToPort is refused HTTP 400, and the message AWS words for that class is the fixed text `Invalid TCP/UDP port range(` + the from-port + `:` + the to-port + `)`. The only variable parts are the two port numbers taken from the request; the rest is fixed text. The same message is what CloudFormation shows as the resource's CREATE_FAILED reason for both AWS::EC2::SecurityGroupIngress and AWS::EC2::SecurityGroupEgress.
Emulator: the refusal message is `ipPermissions.<n> ends at <to>, before it starts at <from>` with error code `InvalidPermission.Malformed` — a different sentence, a different shape (it names a request-index rather than the range) and a code AWS does not use for this class.
Why: a client that reads a stack's failure reason (or the EC2 refusal directly) to tell an inverted port range from any other malformed permission matches on text AWS never emits, so templates and tooling that key off AWS's wording do not recognise the failure.
Fix: the port-range validation of the EC2 security-group authorize handlers must render AWS's `Invalid TCP/UDP port range(<from>:<to>)` message for an inverted range.
Traces: cloudformation__ec2.deploy-ec2-security-group-ingress-invalid-port-range-rejected-and-rolled-back, cloudformation__ec2.deploy-ec2-security-group-egress-invalid-port-range-rejected-and-rolled-back.

=== Cause 3 of 3: the service tail CloudFormation appends to a failed resource's reason is the wrong format ===
AWS: recorded behavior only. When CloudFormation reports a resource failure caused by a downstream service refusal, the reason ends with the comma-separated tail `(Service: Ec2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)` — fixed text with the service name, the HTTP status, the request id and the attempt count as the only variable parts, and no error code or proxy field.
Emulator: appends the semicolon-separated older form `(Service: Ec2; Status Code: 400; Error Code: <code>; Request ID: <id>; Proxy: null)` — different separators, an extra `Error Code` field, a `Proxy: null` field and no `SDK Attempt Count` clause.
Why: a client that parses a stack event's reason to recover the downstream status code, request id or attempt count (the normal way CloudFormation failures are triaged and correlated to a service call) finds fields in the wrong places, and any fixture of AWS stack-event reasons mismatches on every service-caused failure, not only this one.
Fix: the resource-failure reason renderer in the cloudformation service must emit AWS's comma-separated `(Service: ..., Status Code: ..., Request ID: ...) (SDK Attempt Count: ...)` tail.
Traces: cloudformation__ec2.deploy-ec2-security-group-ingress-invalid-port-range-rejected-and-rolled-back, cloudformation__ec2.deploy-ec2-security-group-egress-invalid-port-range-rejected-and-rolled-back.

## Evidence
Cause 1 — cloudformation__ec2.cfn-ec2-security-group-ingress-update-in-place-replace-retained-then-delete: `AssertionError: stack ended 'UPDATE_ROLLBACK_COMPLETE' ('The following resource(s) failed to update: [Ingress]. '), expected 'UPDATE_COMPLETE'; failed events: [('Ingress', 'UPDATE_FAILED', 'NotImplemented: this emulator does not change the properties of an existing AWS::EC2::SecurityGroupIngress in place; the resource was left as it is.')]`

Causes 2 and 3 — cloudformation__ec2.deploy-ec2-security-group-{ingress,egress}-invalid-port-range-rejected-and-rolled-back, both: `AssertionError: CREATE_FAILED reason: expected ('Invalid TCP/UDP port range(100:50) (Service: Ec2, Status Code: 400, Request ID: <id>) (SDK Attempt Count: <n>)', 'InvalidRequest'), got ('ipPermissions.1 ends at 50, before it starts at 100 (Service: Ec2; Status Code: 400; Error Code: InvalidPermission.Malformed; Request ID: <id>; Proxy: null)', 'InvalidRequest')`. The ports 100 and 50 come from the template's rule; the request id and attempt count are per-run values. The CloudFormation-level code `InvalidRequest` already matches.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Cause 1: CreateStack with a VPC, a security group and an AWS::EC2::SecurityGroupIngress; wait for CREATE_COMPLETE, then UpdateStack changing a property of the ingress resource and poll DescribeStacks — the stack reaches UPDATE_ROLLBACK_COMPLETE and DescribeStackEvents shows the Ingress UPDATE_FAILED with the NotImplemented reason.
3. Causes 2 and 3: CreateStack with an AWS::EC2::SecurityGroupIngress (then an AWS::EC2::SecurityGroupEgress) whose FromPort=100 and ToPort=50; read the CREATE_FAILED resource event's ResourceStatusReason — both the sentence and the trailing service tail differ from AWS's.
