Issue-Source: trace-sweep-divergence
Issue-Trace: iot-ca-certificate-crud
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-ca-certificate-crud iot-iot-ca-certificate-lifecycle iot-ca-certificate-lifecycle iot-ca-certificate-lifecycle-with-device-cert iot-ca-certificate-not-found-errors
Issue-Shape-Id: com.amazonaws.iot#AWSIotService
Issue-Key: iot|DescribeCACertificate|2xx->2xx|$.certificateDescription.customerVersion
Issue-Key: iot|DescribeCACertificate|2xx->2xx|$.certificateDescription.status
Issue-Key: iot|DescribeCACertificate|4xx->4xx|$.message
Issue-Key: iot|DescribeCACertificate|4xx->2xx|$.message
Issue-Key: iot|DescribeCACertificate|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot|DescribeCACertificate|4xx->2xx|status
Issue-Commit: c894371411e6f4d2d3006659666973f5ae958f34

# iot CA-certificate plane diverges: UpdateCACertificate never applies, DeleteCACertificate leaves the CA readable, not-found wording is emulator-authored

> _Note: this issue was identified against the code base at commit `c894371411e6f4d2d3006659666973f5ae958f34`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeCACertificate

## Description
The CA-certificate (`/cacertificate/{certificateId}`) plane of the iot emulator diverges from AWS in three independent ways: a write to a CA certificate is not reflected by the subsequent read, a delete does not remove the CA from the registry, and the not-found refusal text is emulator-authored. All three are reached through DescribeCACertificate, which is the only read of that state. 3 causes.

Cause 1 of 3: UpdateCACertificate does not apply newStatus and does not bump customerVersion (fixture iot-ca-certificate-crud, step task#5).
AWS: Smithy: UpdateCACertificate PUT /cacertificate/{certificateId} with `newStatus` (httpQuery `newStatus`, enum CACertificateStatus = ACTIVE|INACTIVE), empty output; DescribeCACertificate GET /cacertificate/{certificateId} returns CACertificateDescription with `status` (CACertificateStatus) and `customerVersion` (integer, @range min 1, "The customer version of the CA certificate"). After a successful UpdateCACertificate that sets newStatus=INACTIVE, AWS's next DescribeCACertificate reports certificateDescription.status = INACTIVE and a customerVersion incremented by the customer-initiated write (recorded response: customerVersion 2 for a CA registered then updated once).
Emulator: DescribeCACertificate answers 200 but still reports certificateDescription.status 'ACTIVE' and certificateDescription.customerVersion 1 — the status transition requested by UpdateCACertificate was accepted (200) and then discarded, and customerVersion is pinned at its registration value.
Why: a client that deactivates a CA to stop auto-registration of new device certificates, then reads the CA back to confirm, is told the CA is still ACTIVE; any workflow that gates on INACTIVE (or that uses customerVersion as an optimistic-concurrency/change marker to detect that its own write landed) either loops forever or proceeds believing the CA is still live.
Fix: UpdateCACertificate's handler/state — it must persist newStatus (and newAutoRegistrationStatus) onto the stored CA record and increment the record's customerVersion on each customer-initiated write, and DescribeCACertificate must project the stored values rather than the registration-time ones.
Traces: iot-ca-certificate-crud#task#5, iot-iot-ca-certificate-lifecycle#task#7.

Cause 2 of 3: the ResourceNotFoundException message for an unknown CA certificate id is emulator-authored (fixture iot-ca-certificate-crud, step task#6).
AWS: Smithy: DescribeCACertificate errors include ResourceNotFoundException (client, httpError 404). Recorded behavior: for a well-formed but unregistered 64-hex certificateId AWS answers 404 with x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/' and the message 'Requested CA Certificate does not exist.' — that text is entirely FIXED, with no variable part: AWS does not echo the requested certificateId into it.
Emulator: the status (404) and error type are right, but the message is 'CertificateId <certificateId> does not exist' — different fixed wording, and it interpolates the requested id (a request value) into a message AWS keeps constant.
Why: clients and test suites that match on AWS's refusal text (log scrapers, error-classification shims, assertion libraries that compare the whole message) do not recognise the refusal; the interpolated id also makes the emulator's message non-deterministic across runs where AWS's is stable.
Fix: the DescribeCACertificate error path (its refusal construction) must emit AWS's fixed text 'Requested CA Certificate does not exist.' for the CA-not-found class.
Traces: iot-ca-certificate-crud#task#6, iot-ca-certificate-lifecycle#setup#1, iot-ca-certificate-lifecycle-with-device-cert#task#1, iot-ca-certificate-not-found-errors#task#1.

Cause 3 of 3: a deleted CA certificate stays readable — DescribeCACertificate still answers 200 after DeleteCACertificate (fixture iot-ca-certificate-crud, step teardown#8).
AWS: Smithy: DeleteCACertificate DELETE /cacertificate/{certificateId} (errors include CertificateStateException 406 and ResourceNotFoundException 404) removes the CA from the account's registry, so the following DescribeCACertificate for that id is a not-found: 404 with x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/' and the fixed message 'Requested CA Certificate does not exist.' (same refusal class as Cause 2; the id is not part of the text).
Emulator: after DeleteCACertificate reports success, DescribeCACertificate answers 200 and returns the full certificateDescription — no 404, no message, no x-amzn-errortype header. The delete is acknowledged without removing the record.
Why: deletion is unobservable, so a client cannot tell a live CA from a deleted one: re-registering the same CA PEM, cleanup/idempotency checks and "is this resource gone?" polling all see a CA that AWS has already removed, and a stale CA that still reports ACTIVE keeps auto-registering device certificates in the emulator's view of the world.
Fix: DeleteCACertificate's handler/state must actually remove the CA record (after its state/attachment preconditions), so every subsequent read of that id takes the not-found path.
Traces: iot-ca-certificate-crud#teardown#8, iot-ca-certificate-lifecycle#teardown#6, iot-iot-ca-certificate-lifecycle#teardown#12.

Related but NOT covering these: open issue 20261108T051259Z-iot-certificate-plane-4-causes-deleted-certificates-stay.md and 20261117T200355Z-iot-5-net-new-causes-updatecertificate-s-two.md concern the DEVICE certificate plane (DeleteCertificate/UpdateCertificate, /certificates/{certificateId}); the replay reports these CA-certificate clusters as unowned, so the CA plane (/cacertificate/{certificateId}) is still unfixed.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up; /__admin/health 200):

Cause 1 — iot-ca-certificate-crud#task#5 (DescribeCACertificate, expected 200, got 200), cluster also covers iot-iot-ca-certificate-lifecycle#task#7:
  $.certificateDescription.customerVersion: value_mismatch (expected 2, got 1)
  $.certificateDescription.status: value_mismatch (expected 'INACTIVE', got 'ACTIVE')

Cause 2 — iot-ca-certificate-crud#task#6 (DescribeCACertificate, expected 404, got 404), cluster also covers iot-ca-certificate-lifecycle#setup#1, iot-ca-certificate-lifecycle-with-device-cert#task#1, iot-ca-certificate-not-found-errors#task#1:
  $.message: value_mismatch (expected 'Requested CA Certificate does not exist.', got 'CertificateId <64-hex id from the request> does not exist')
  Fixed text: the whole expected sentence. Variable text: none in AWS's message; the emulator injects the request's certificateId.

Cause 3 — iot-ca-certificate-crud#teardown#8 (DescribeCACertificate), cluster also covers iot-ca-certificate-lifecycle#teardown#6, iot-iot-ca-certificate-lifecycle#teardown#12:
  status: expected 404, got 200
  $.message: missing (expected 'Requested CA Certificate does not exist.', got None)
  $headers.x-amzn-errortype: missing (expected 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', got None)

Model citations: DescribeCACertificate GET /cacertificate/{certificateId} -> CACertificateDescription{status: CACertificateStatus(ACTIVE|INACTIVE), customerVersion: integer @range(min 1)}, errors ResourceNotFoundException(404); UpdateCACertificate PUT /cacertificate/{certificateId} ?newStatus; DeleteCACertificate DELETE /cacertificate/{certificateId}, errors CertificateStateException(406), ResourceNotFoundException(404).

## Steps to Reproduce
Cause 1:
1. RegisterCACertificate (or RegisterCertificateWithoutCA path used by the fixture) to obtain a CA certificateId.
2. PUT /cacertificate/{certificateId}?newStatus=INACTIVE (UpdateCACertificate) — expect 200.
3. GET /cacertificate/{certificateId} (DescribeCACertificate). AWS: certificateDescription.status INACTIVE, customerVersion 2. Emulator: status ACTIVE, customerVersion 1.

Cause 2:
1. GET /cacertificate/<a well-formed 64-hex id that was never registered> (DescribeCACertificate).
2. AWS: 404, x-amzn-errortype 'ResourceNotFoundException:http://iot.amazonaws.com/doc/2015-10-02/', message 'Requested CA Certificate does not exist.'. Emulator: 404 with message 'CertificateId <id> does not exist'.

Cause 3:
1. Register a CA certificate, then DELETE /cacertificate/{certificateId} (DeleteCACertificate) — succeeds.
2. GET /cacertificate/{certificateId} again. AWS: 404 ResourceNotFoundException 'Requested CA Certificate does not exist.'. Emulator: 200 with the full certificateDescription.

Replay: replay_wire_corpus() over trace set simparity-wire-aws-v1, fixtures iot-ca-certificate-crud, iot-ca-certificate-lifecycle, iot-ca-certificate-lifecycle-with-device-cert, iot-ca-certificate-not-found-errors, iot-iot-ca-certificate-lifecycle.
