Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot.delete-ca-certificate-active-state-rejected
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot.delete-ca-certificate-active-state-rejected|run#12|AssertionError|assert err == ("CertificateStateException", f"CA Certificate must be deactivated (not ACTIVE) before deletion. Id: {cert_id}"), err
Issue-Commit: 3b67cbc843a1c36f429f2495723307b26dcafbd5

# Implement iot DeleteCACertificate: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `3b67cbc843a1c36f429f2495723307b26dcafbd5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteCACertificate

## Description
`DeleteCACertificate` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-iot-awsiotservice/src/ops/delete_ca_certificate.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `DeleteCACertificate` call, so each one unblocks only when `DeleteCACertificate` works: `iot.delete-ca-certificate-active-state-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-iot-awsiotservice/src/ops/delete_ca_certificate.rs`. Replaying the corpus, the emulator refused the `DeleteCACertificate` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_iot_delete_ca_certificate_active_state_rejected.py", line 53, in run
    assert err == ("CertificateStateException", f"CA Certificate must be deactivated (not ACTIVE) before deletion. Id: {cert_id}"), err
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: ('NotImplemented', 'operation DeleteCACertificate is scaffolded but not implemented in this emulator')

## Steps to Reproduce
Boot the iot emulator
Call DeleteCACertificate and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
