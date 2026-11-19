Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.cpucredit-modify-instance-credit-specification-invalid-mode-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.cpucredit-modify-instance-credit-specification-invalid-mode-rejected|run#19|AssertionError|assert code == _EXPECTED_CODE, (
Issue-Commit: 20b40f42cd78b95666b130bae2475dd4b1dad646

# REOPENS ModifyInstanceCreditSpecification: an out-of-enum CpuCredits is refused as a raised error instead of a 200 carrying an UnsuccessfulInstanceCreditSpecifications entry

> _Note: this issue was identified against the code base at commit `20b40f42cd78b95666b130bae2475dd4b1dad646`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ModifyInstanceCreditSpecification

## Description
REOPENS 20261104T142958Z-ec2-modifyinstancecreditspecification-stub.md: its fix left the call refused.

AWS: ModifyInstanceCreditSpecification reports a CpuCredits value outside the closed set {standard, unlimited} as a PER-INSTANCE failure under HTTP 200, not as a raised error. The response carries an entry in `UnsuccessfulInstanceCreditSpecifications` naming the instance, whose `Error.Code` is `InvalidCpuCredits.Malformed` — a parameter-specific code, not the generic InvalidParameterValue a malformed enum usually draws. The SDK call RETURNS; nothing is raised, so a caller's try/except never fires and the failure must be read out of the response body. The instance's existing credit specification is left untouched. Smithy: ModifyInstanceCreditSpecification declares NO modelled operation errors, and its output declares both `SuccessfulInstanceCreditSpecifications` and `UnsuccessfulInstanceCreditSpecifications`; UnsuccessfulInstanceCreditSpecificationItem carries `InstanceId` plus `Error` (UnsuccessfulInstanceCreditSpecificationItemError) — the per-entry error channel is modelled, the raised one is not. The code and message text are recorded behaviour only.

Emulator: the call is REFUSED — it raises a ClientError with code `InvalidParameterValue` instead of answering 200 with an unsuccessful entry. Both the channel (raised error vs 200 + body entry) and the code diverge. The positive controls in the same check pass: setting CpuCredits to `unlimited` is accepted, named in SuccessfulInstanceCreditSpecifications, and read back by DescribeInstanceCreditSpecifications.

Why: this changes the control flow of every client written against the real service. Code that calls ModifyInstanceCreditSpecification for a batch and inspects UnsuccessfulInstanceCreditSpecifications sees an exception thrown out of the call instead, so a partial-failure handler is never reached; code that keys retry/branching on the error code sees InvalidParameterValue instead of InvalidCpuCredits.Malformed.

Fix: ModifyInstanceCreditSpecification's validation and response serialization in the ec2 handler for that operation — out-of-enum CpuCredits must be routed to the per-instance unsuccessful list under a 200 with Error.Code `InvalidCpuCredits.Malformed`, not to a raised refusal.

Traces: ec2.cpucredit-modify-instance-credit-specification-invalid-mode-rejected

## Evidence
Failing check ec2.cpucredit-modify-instance-credit-specification-invalid-mode-rejected, from the behavioral replay:

AssertionError: "CpuCredits is a closed enum of 'standard' and 'unlimited', so 'turbo' must be refused with 'InvalidCpuCredits.Malformed'; real AWS refused it with 'InvalidParameterValue' (via exception)"

Expected: HTTP 200, response body `UnsuccessfulInstanceCreditSpecifications: [{InstanceId: <id>, Error: {Code: "InvalidCpuCredits.Malformed", ...}}]`, nothing raised, and the instance still reporting its previous CpuCredits.
Actual: a raised ClientError with Error.Code `InvalidParameterValue` (channel reported by the check as "via exception").

The immediately preceding assertions in the same check passed, so the accept path and DescribeInstanceCreditSpecifications read-back are correct; only the out-of-enum refusal path diverges.

## Steps to Reproduce
Replay the behavioral suite and re-run ec2.cpucredit-modify-instance-credit-specification-invalid-mode-rejected:
1. RunInstances a burstable instance (t3.micro) and wait for `running`.
2. ModifyInstanceCreditSpecification with [{InstanceId: <id>, CpuCredits: "unlimited"}] — expect 200, the id in SuccessfulInstanceCreditSpecifications, empty UnsuccessfulInstanceCreditSpecifications, and DescribeInstanceCreditSpecifications reporting "unlimited" (this already works).
3. ModifyInstanceCreditSpecification with [{InstanceId: <id>, CpuCredits: "turbo"}] — expect the SDK call to RETURN 200 with an UnsuccessfulInstanceCreditSpecifications entry for <id> whose Error.Code is `InvalidCpuCredits.Malformed`; nothing may be raised.
4. DescribeInstanceCreditSpecifications — CpuCredits must still be "unlimited".
