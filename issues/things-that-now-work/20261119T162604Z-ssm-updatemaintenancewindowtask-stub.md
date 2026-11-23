Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.maintenance-window-task-update-and-schedule
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm|UpdateMaintenanceWindowTask|NotImplemented
Issue-Commit: cd64e37d3e3efbbf828b0eac083377c231c88f4f

# Implement ssm UpdateMaintenanceWindowTask: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `cd64e37d3e3efbbf828b0eac083377c231c88f4f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateMaintenanceWindowTask

## Description
`UpdateMaintenanceWindowTask` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-ssm-amazonssm/src/ops/update_maintenance_window_task.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `UpdateMaintenanceWindowTask` call, so each one unblocks only when `UpdateMaintenanceWindowTask` works: `ssm.maintenance-window-task-update-and-schedule`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-ssm-amazonssm/src/ops/update_maintenance_window_task.rs`. Replaying the corpus, the emulator refused the `UpdateMaintenanceWindowTask` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the UpdateMaintenanceWindowTask operation: operation UpdateMaintenanceWindowTask is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the ssm emulator
Call UpdateMaintenanceWindowTask and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
