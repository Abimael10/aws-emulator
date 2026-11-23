Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: endusermessaging.update-notify-code-configuration-merges-channels-and-empty-template-clears-one endusermessaging.update-notify-code-configuration-partial-update-preserves-other-fields endusermessaging.update-notify-code-configuration-with-no-fields-changes-nothing
Issue-Shape-Id: com.amazonaws.endusermessaging#EndUserMessaging
Issue-Key: endusermessaging|UpdateNotifyCodeConfiguration|NotImplemented
Issue-Commit: 2f00469c6b1c115e8270b5c2778d03323e9ec131

# Implement endusermessaging UpdateNotifyCodeConfiguration: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `2f00469c6b1c115e8270b5c2778d03323e9ec131`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateNotifyCodeConfiguration

## Description
`UpdateNotifyCodeConfiguration` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/update_notify_code_configuration.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

3 recorded unit(s) fail on their `UpdateNotifyCodeConfiguration` call, so each one unblocks only when `UpdateNotifyCodeConfiguration` works: `endusermessaging.update-notify-code-configuration-merges-channels-and-empty-template-clears-one`, `endusermessaging.update-notify-code-configuration-partial-update-preserves-other-fields`, `endusermessaging.update-notify-code-configuration-with-no-fields-changes-nothing`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-endusermessaging-endusermessaging/src/ops/update_notify_code_configuration.rs`. Replaying the corpus, the emulator refused the `UpdateNotifyCodeConfiguration` call: File "context.py", line 123, in wrapper
    return func(*args, **kwargs)
           ^^^^^^^^^^^^^^^^^^^^^
  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
botocore.exceptions.ClientError: An error occurred (NotImplemented) when calling the UpdateNotifyCodeConfiguration operation: operation UpdateNotifyCodeConfiguration is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the endusermessaging emulator
Call UpdateNotifyCodeConfiguration and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
