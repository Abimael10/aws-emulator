Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pinpoint-sms-voice-v2.deleting-configuration-set-eventually-clears-protect-configuration-association
Issue-Shape-Id: com.amazonaws.pinpointsmsvoicev2#PinpointSMSVoiceV2
Issue-Key: pinpoint-sms-voice-v2|DeleteProtectConfiguration|NotImplemented
Issue-Commit: 0ce9de8aa88e633fae36e1cb77f97e8287e67370

# Implement pinpoint-sms-voice-v2 DeleteProtectConfiguration: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `0ce9de8aa88e633fae36e1cb77f97e8287e67370`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteProtectConfiguration

## Description
`DeleteProtectConfiguration` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-pinpointsmsvoicev2-pinpointsmsvoicev2/src/ops/delete_protect_configuration.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteProtectConfiguration` call, so each one unblocks only when `DeleteProtectConfiguration` works: `pinpoint-sms-voice-v2.deleting-configuration-set-eventually-clears-protect-configuration-association`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-pinpointsmsvoicev2-pinpointsmsvoicev2/src/ops/delete_protect_configuration.rs`. Replaying the corpus, the emulator refused the `DeleteProtectConfiguration` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_pinpoint_sms_voice_v2_deleting_configuration_set_eventually_clears_protect_configuration_association.py", line 69, in run
    raise AssertionError(
AssertionError: deleting the protect configuration failed with 'NotImplemented': 'operation DeleteProtectConfiguration is scaffolded but not implemented in this emulator', not the exact association conflict this check tolerates: 'Conflict Occurred - Reason="PROTECT_CONFIGURATION_ASSOCIATED_WITH_CONFIGURATION_SET" ResourceType="protect-configuration" ResourceId="protect-configuration-00000000000000000000000000000001"'

## Steps to Reproduce
Boot the pinpoint-sms-voice-v2 emulator
Call DeleteProtectConfiguration and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
