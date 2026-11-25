Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.billing-group-lifecycle-and-membership
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.billing-group-lifecycle-and-membership|_eventually#14|AssertionError|raise AssertionError(f"condition never became true; last error: {last_exc!r}")
Issue-Commit: 2c036246c52324f8f473f1b5e9b5314460be2843

# Implement iot ListThingsInBillingGroup: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `2c036246c52324f8f473f1b5e9b5314460be2843`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListThingsInBillingGroup

## Description
`ListThingsInBillingGroup` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-iot-awsiotservice/src/ops/list_things_in_billing_group.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `ListThingsInBillingGroup` call, so each one unblocks only when `ListThingsInBillingGroup` works: `iot.billing-group-lifecycle-and-membership`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-iot-awsiotservice/src/ops/list_things_in_billing_group.rs`. Replaying the corpus, the emulator refused the `ListThingsInBillingGroup` call: File "check_iot_billing_group_lifecycle_and_membership.py", line 74, in run
    members = _eventually(
              ^^^^^^^^^^^^
  File "check_iot_billing_group_lifecycle_and_membership.py", line 42, in _eventually
    raise AssertionError(f"condition never became true; last error: {last_exc!r}")
AssertionError: condition never became true; last error: ClientError('An error occurred (NotImplemented) when calling the ListThingsInBillingGroup operation: operation ListThingsInBillingGroup is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the iot emulator
Call ListThingsInBillingGroup and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
