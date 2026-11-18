Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.ec2-image-datatype-validation-is-asynchronous-and-the-parameter-never-lands ssm.public-parameter-reference-data-readable-not-owned ssm.compliance-summary-server-computed-aggregation
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.ec2-image-datatype-validation-is-asynchronous-and-the-parameter-never-lands|run#32|AssertionError|assert current["Value"] == real_ami, (
Issue-Key: ssm|GetParametersByPath|AccessDeniedException
Issue-Key: ssm.compliance-summary-server-computed-aggregation|_wait_for_agent_online#2|AssertionError|raise AssertionError(
Issue-Commit: 2c0d8b9f0e4568b2774877fc481a224c15ff713e

# ssm: 3 net-new behavioral causes — an aws:ec2:image overwrite that fails validation replaces the stored value, GetParametersByPath refuses an empty sub-path under a published /aws/ namespace, and an SSM-profiled instance never reaches PingStatus=Online

> _Note: this issue was identified against the code base at commit `2c0d8b9f0e4568b2774877fc481a224c15ff713e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutParameter

## Description
Three net-new root causes under this service heading, each grounded in its own failing check. Numbered so each can be closed on its own; all three must be fixed before this issue is submitted.

Cause 1 of 3: PutParameter with DataType=aws:ec2:image and an invalid AMI id does not fail asynchronously — the unvalidated value replaces the parameter's stored, previously validated value.
AWS: Smithy (ssm) PutParameterRequest.DataType documents the rule verbatim: "If you call PutParameter with aws:ec2:image data type, a successful HTTP 200 response does not guarantee that your parameter was successfully created or updated. The aws:ec2:image value is validated asynchronously, and the PutParameter call returns before the validation is complete. If you submit an invalid AMI value, the PutParameter operation will return success, but the asynchronous validation will fail and the parameter will not be created or updated." So an Overwrite=true PutParameter carrying an AMI id that is not available in the account answers 200 with a new Version number, and then the validation fails and the parameter is NOT updated: GetParameter keeps serving the last validated value and its DataType, and GetParameterHistory retains only validated values. PutParameter models no error for an invalid aws:ec2:image value (errors are ParameterAlreadyExists, ParameterPatternMismatchException, UnsupportedParameterType, TooManyUpdates, …) — the refusal path does not exist, by design.
Emulator: after the bogus overwrite, GetParameter on the same name immediately served the bogus AMI id instead of the stored validated one — the write landed and the validated value is gone. (The replay observed the served Value change to the unvalidated id 0 s after the overwrite.)
Why: a client that stores an AMI id in Parameter Store loses the good value on any bad write — a typo or a stale/deregistered AMI id silently destroys the parameter's current value, and every consumer resolving that parameter (CloudFormation `{{resolve:ssm:...}}`, launch templates, user data) then launches against an AMI id that does not exist. AWS guarantees the opposite: a failed validation is a no-op.
Fix: the PutParameter handler's aws:ec2:image path in the SSM parameter store — validate the AMI id against the EC2 image catalog after answering 200, and apply the new version only when validation succeeds (and never mutate the current version of the parameter until it does).
Traces: ssm.ec2-image-datatype-validation-is-asynchronous-and-the-parameter-never-lands.

Cause 2 of 3: GetParametersByPath on a path BENEATH a published /aws/ public namespace is refused with AccessDeniedException instead of returning an empty page.
AWS: a path that lies under a published public namespace but has no children is an ordinary empty result — GetParametersByPath answers 200 with `Parameters: []` and no NextToken. The AccessDeniedException refusal applies only to a namespace under /aws/ that is not a published namespace at all (e.g. /aws/service/<unknown>), and its message is `No access to "/aws/" namespace: <path-without-leading-slash> is not a valid namespace` — fixed text `No access to "/aws/" namespace: ` and ` is not a valid namespace` around one request-derived value, the requested Path with its leading '/' stripped. Smithy (ssm): GetParametersByPath models errors InternalServerError, InvalidFilterKey, InvalidFilterOption, InvalidFilterValue, InvalidKeyId, InvalidNextToken — no not-found and no access error, so an empty match is an empty list; the AccessDeniedException wording for an unknown /aws/ namespace is recorded behavior.
Emulator: GetParametersByPath(Path="/aws/service/ami-amazon-linux-latest/<unknown-subpath>", Recursive=true) — a sub-path of a namespace the emulator does publish — is refused with AccessDeniedException and the unknown-namespace message `No access to "/aws/" namespace: aws/service/ami-amazon-linux-latest/<unknown-subpath> is not a valid namespace`. The emulator is matching the requested path against an exact list of known namespaces instead of testing only whether its namespace prefix under /aws/ is published.
Why: a client enumerating the public parameter tree (the documented way to discover AMI aliases) gets a hard 400 for any prefix it has not already guessed exactly, so discovery-by-path is impossible; and code that treats AccessDeniedException as a credentials/permissions problem is sent to debug IAM for what AWS reports as "no results".
Fix: the GetParametersByPath handler's /aws/ public-namespace gate in the SSM service — refuse only when the namespace segment(s) directly under /aws/ name no published namespace; a deeper path under a published namespace must fall through to the normal path scan and return an empty Parameters list.
Traces: ssm.public-parameter-reference-data-readable-not-owned.

Cause 3 of 3: an EC2 instance launched with an AmazonSSMManagedInstanceCore instance profile never becomes an Online SSM managed node, so a compliance-aggregation scenario never gets past its setup gate.
AWS: such an instance registers with Systems Manager shortly after boot and DescribeInstanceInformation returns an InstanceInformation row for it with PingStatus `Online`; the node is then a valid ResourceId of ResourceType `ManagedInstance` for PutComplianceItems / ListComplianceSummaries. Smithy (ssm): DescribeInstanceInformation output InstanceInformationList -> InstanceInformation{InstanceId, PingStatus: enum Online|ConnectionLost|Inactive}; an unregistered instance is simply absent from the list and no error is modeled, which is why this surfaces as a timeout rather than a refusal.
Emulator: DescribeInstanceInformation never returns a row for the launched instance, so PingStatus never becomes Online and the check aborts at `instance <instance-id> never reached PingStatus=Online within <timeout>s (SSM Agent never registered)`.
Why: a client following AWS's documented path to a managed node never gets one, so every node-scoped SSM surface — here server-side compliance aggregation over a ManagedInstance resource — is unreachable, and the failure is silent (an empty list, not an error).
Fix: the SSM managed-node registration path (registering an instance whose profile grants the SSM core policy, and maintaining its InstanceInformation row and PingStatus).
Traces: ssm.compliance-summary-server-computed-aggregation. Related but not covering: open issue 20261109T142303Z-ssm-5-net-new-causes-managed-nodes-never.md reports the same registration gap for four maintenance-window checks; this failing check is not carried by it. Open issues 20261118T003001Z-ssm-2-net-new-behavioral-causes-putcomplianceitems-is.md and the scaffold issue for the unimplemented ListComplianceSummaries/PutComplianceItems operations cover the compliance operations themselves, not this registration gate.

## Evidence
Cause 1: `AssertionError: an overwrite whose aws:ec2:image value fails validation must leave the stored value at 'ami-<validated-id>'; 0s after the bogus overwrite it was 'ami-<bogus-id>'`. Expected: PutParameter(Overwrite=true, DataType='aws:ec2:image', Value=<unavailable AMI id>) answers 200 with a new Version, the validation then fails, GetParameter keeps serving the previously validated AMI id and its DataType for the whole observation window, and GetParameterHistory holds only validated values. Actual: the served Value becomes the unvalidated id immediately.

Cause 2: `botocore.errorfactory.AccessDeniedException: An error occurred (AccessDeniedException) when calling the GetParametersByPath operation: No access to "/aws/" namespace: aws/service/ami-amazon-linux-latest/<unknown-subpath> is not a valid namespace`. Expected: HTTP 200 with `{"Parameters": []}` and no NextToken, because the namespace /aws/service/ami-amazon-linux-latest IS published and only the sub-path is unknown. The same AccessDeniedException code and message ARE correct one level up, for /aws/service/<unknown-namespace> (that leg of the same check passes).

Cause 3: `AssertionError: instance <instance-id> never reached PingStatus=Online within <timeout>s (SSM Agent never registered)`. Expected: DescribeInstanceInformation lists the instance with PingStatus='Online' shortly after launch.

## Steps to Reproduce
Cause 1: ssm PutParameter(Name=<n>, Value=<AMI id that exists>, Type='String', DataType='aws:ec2:image') and poll GetParameter(Name=<n>) until it is readable; then PutParameter(Name=<n>, Value=<AMI id that does not exist in the account>, Type='String', DataType='aws:ec2:image', Overwrite=True) — it answers 200 — and immediately GetParameter(Name=<n>): the Value has become the bogus id instead of staying at the validated one. Also check GetParameterHistory: no unvalidated value may appear.

Cause 2: ssm GetParametersByPath(Path='/aws/service/ami-amazon-linux-latest/<name-that-does-not-exist>', Recursive=True) — it raises AccessDeniedException; expected 200 with Parameters=[] and no NextToken. Contrast with GetParametersByPath(Path='/aws/service/<unknown-namespace>', Recursive=True), where the AccessDeniedException and its message are correct.

Cause 3: create a role with arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore and an instance profile for it, RunInstances with that profile, then poll ssm DescribeInstanceInformation(Filters=[{Key:'InstanceIds',Values:[<id>]}]) — the list stays empty and PingStatus is never Online.
