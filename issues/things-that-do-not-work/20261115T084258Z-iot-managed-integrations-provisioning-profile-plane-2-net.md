Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token iot-managed-integrations.provisioning-profile-lifecycle-status
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token|run#21|AssertionError|raise AssertionError(f"expected ResourceNotFoundException reading deleted profile {profile_id}")
Issue-Key: iot-managed-integrations.provisioning-profile-lifecycle-status|run#16|AssertionError|assert bad_code_msg == "1 validation error detected: Value at 'caCertificate' failed to satisfy constraint: Member must satisfy regular expression pattern: ----
Issue-Commit: f5066c1865ee4d315a54fa16eeb439dcd65bbf5e

# iot-managed-integrations provisioning-profile plane: 2 net-new causes — a deleted profile stays readable for a grace window instead of 404ing immediately, and the CaCertificate pattern ValidationException quotes the model regex instead of AWS's wording

> _Note: this issue was identified against the code base at commit `f5066c1865ee4d315a54fa16eeb439dcd65bbf5e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetProvisioningProfile

## Description
Two net-new root causes in the iot-managed-integrations provisioning-profile control plane, both grounded in failing behavioral checks.

Cause 1 of 2: DeleteProvisioningProfile does not take effect for the caller — a profile it accepted is still served by GetProvisioningProfile with HTTP 200.
Failing check: iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token.
AWS: Smithy (services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/smithy-model/com-amazonaws-iotmanagedintegrations-iotmanagedintegrations.json) — DeleteProvisioningProfile is DELETE /provisioning-profiles/{Identifier}, @idempotent, output Unit, success 200; GetProvisioningProfile is GET /provisioning-profiles/{Identifier}, success 200, and models ResourceNotFoundException (errorKind client, httpError 404, "The specified resource does not exist."). Recorded behavior: once DeleteProvisioningProfile has returned success for an Identifier, the very next GetProvisioningProfile on that same Identifier is refused 404 ResourceNotFoundException with message `Provisioning Profile resource not found` — that whole sentence is the fixed text of this resource's not-found class; it interpolates nothing from the request (the Identifier is not echoed into it). A repeat DeleteProvisioningProfile of the same, already-deleted Identifier is likewise refused 404 with the same fixed message. There is no observable grace window in which a deleted profile still reads back.
Emulator: delete_provisioning_profile.rs marks the record `begin_deletion(now)` and leaves it resolvable for a fixed span (crate::state::PROVISIONING_PROFILE_DELETION_DURATION), and get_provisioning_profile.rs resolves any record whose deletion that clock has not yet retired. So immediately after a successful delete the read answers HTTP 200 with the full profile projection (Status DELETE_IN_PROGRESS) instead of 404; the check's `get_provisioning_profile(Identifier=<deleted id>)` returned a success body and the check raised `expected ResourceNotFoundException reading deleted profile <id>`. The same window makes the follow-on repeat delete answer 200 instead of the modeled 404.
Why: a client that deletes a profile and polls the read to confirm removal never observes removal inside its budget — it either loops until timeout or concludes the delete failed and retries. A client using the read as an existence check hands the id and claim certificate of a deleted profile to downstream provisioning calls. Teardown-then-recreate flows that key off the 404 to know an identifier is free break, and SDK ResourceNotFoundException handling for this resource is never exercised because no error is emitted at all.
Fix: the provisioning-profile state's deletion model plus the GetProvisioningProfile and DeleteProvisioningProfile handlers — removal must be visible to the next request rather than deferred behind a wall-clock span, and a read or delete of an identifier that resolves to nothing must return the modeled ResourceNotFoundException (404, x-amzn-errortype: ResourceNotFoundException, Message `Provisioning Profile resource not found`).
Related open issue, NOT a duplicate owner of this check: 20261115T061325Z-iot-managed-integrations-getprovisioningprofile-still-serves-a-deleted-profile-with-200.md reports the same read-after-delete 200 from the wire corpus (fixture iot-managed-integrations-provisioning-profile-lifecycle#teardown#7); it carries no behavioral replay key, so this check is not covered by it. Whoever fixes either should close both; this entry adds the deferred-deletion mechanism in state and the repeat-delete path.

Cause 2 of 2: the CaCertificate pattern ValidationException message quotes the emulator's own regex text instead of the pattern AWS reports.
Failing check: iot-managed-integrations.provisioning-profile-lifecycle-status.
AWS: Smithy CreateProvisioningProfile.CaCertificate carries @pattern "^-----BEGIN CERTIFICATE-----.*(.|\\n)*-----END CERTIFICATE-----\\n?$" and the operation models ValidationException (client, httpError 400). Recorded behavior: AWS refuses a non-PEM CaCertificate with ValidationException whose message is the standard single-violation constraint template — fixed text `1 validation error detected: Value at '<member>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>` — where the variable parts are the wire member name (`caCertificate`) and the pattern AWS itself reports for that member, which is `-----BEGIN CERTIFICATE-----[\s\S]*-----END CERTIFICATE-----\n?` (anchors stripped, the any-char repetition normalized to a character class). Note the value supplied by the request is NOT echoed into this message.
Emulator: it returns the right code (ValidationException) and the right template, but renders the pattern straight from the model string: `1 validation error detected: Value at 'caCertificate' failed to satisfy constraint: Member must satisfy regular expression pattern: -----BEGIN CERTIFICATE-----.*(.|\n)*-----END CERTIFICATE-----\n?` instead of `... pattern: -----BEGIN CERTIFICATE-----[\s\S]*-----END CERTIFICATE-----\n?`.
Why: clients and test harnesses that match or display the service's constraint message for this member see a different regex than AWS emits, so exact-message assertions and operator-facing diagnostics diverge; a caller trying to pre-validate certificates against the reported pattern is handed an expression AWS never publishes on this path.
Fix: the pattern-violation branch of CreateProvisioningProfile's input validation / ValidationException message rendering for this service — emit the pattern text AWS reports for CaCertificate rather than the raw @pattern literal, keeping the surrounding single-violation template and the lower-camel wire member name.

Traces: iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token (cause 1), iot-managed-integrations.provisioning-profile-lifecycle-status (cause 2; it also asserts cause 1's read-after-delete 404 later in the same check, after the message assertion it currently fails on).

## Evidence
Behavioral suite: 17/22 passed for iot-managed-integrations.

Cause 1 — check iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token [mismatch]:
  AssertionError: expected ResourceNotFoundException reading deleted profile d88aa67c2ef14655831b7d794d405f0b
  Sequence: register_custom_endpoint -> 5 concurrent CreateProvisioningProfile sharing Name+ClientToken (one winning Id, idempotent retry returns the same Id — all passed) -> delete_provisioning_profile(Identifier=<id>) returns success -> get_provisioning_profile(Identifier=<id>) returns HTTP 200 with a profile body.
  Expected (AWS): 404 ResourceNotFoundException, Error.Message == 'Provisioning Profile resource not found'; and the subsequent repeat delete_provisioning_profile likewise 404 with that same message.
  Actual (emulator): 200 success; no error raised.
  Code: services/.../src/ops/delete_provisioning_profile.rs:208-223 marks `profile.begin_deletion(now)` and keeps the record until PROVISIONING_PROFILE_DELETION_DURATION elapses (comment at lines 203-207: "until then every reader still resolves it and reports DELETE_IN_PROGRESS"); services/.../src/ops/get_provisioning_profile.rs:168-174 resolves via `provisioning_profile_in(&region, &identifier, SystemTime::now())`, which still returns the not-yet-retired record.

Cause 2 — check iot-managed-integrations.provisioning-profile-lifecycle-status [mismatch]:
  Request: CreateProvisioningProfile(ProvisioningType=FLEET_PROVISIONING, Name=<unique>, CaCertificate="not-a-pem-certificate").
  Expected (AWS, recorded): Code ValidationException; Message == "1 validation error detected: Value at 'caCertificate' failed to satisfy constraint: Member must satisfy regular expression pattern: -----BEGIN CERTIFICATE-----[\s\S]*-----END CERTIFICATE-----\n?"
  Actual (emulator): Code ValidationException (correct); Message == "1 validation error detected: Value at 'caCertificate' failed to satisfy constraint: Member must satisfy regular expression pattern: -----BEGIN CERTIFICATE-----.*(.|\n)*-----END CERTIFICATE-----\n?"
  Fixed text of the class: "1 validation error detected: Value at '<member>' failed to satisfy constraint: Member must satisfy regular expression pattern: <pattern>". Variable parts: member name (caCertificate) and the reported pattern. Only the <pattern> part diverges.
  Model grounding: smithy_query(operation="CreateProvisioningProfile") -> CaCertificate pattern "^-----BEGIN CERTIFICATE-----.*(.|\\n)*-----END CERTIFICATE-----\\n?$"; errors include ValidationException (client, 400).

## Steps to Reproduce
Cause 1:
1. Boot the emulator and point a botocore client for service 'iot-managed-integrations' at it, region eu-west-1.
2. register_custom_endpoint() (tolerate ConflictException).
3. create_provisioning_profile(ProvisioningType='FLEET_PROVISIONING', Name=<name <=36 chars>, ClientToken=<token>) and keep the returned Id.
4. delete_provisioning_profile(Identifier=<Id>) — succeeds.
5. get_provisioning_profile(Identifier=<Id>) — observe HTTP 200 with a profile body (Status DELETE_IN_PROGRESS) instead of 404 ResourceNotFoundException / Message 'Provisioning Profile resource not found'.
6. delete_provisioning_profile(Identifier=<Id>) again — observe success instead of the same 404.
Replay: behavioral check iot-managed-integrations.provisioning-profile-concurrent-create-idempotent-client-token.

Cause 2:
1. Same client and register_custom_endpoint() prerequisite.
2. create_provisioning_profile(ProvisioningType='FLEET_PROVISIONING', Name=<unique name>, CaCertificate='not-a-pem-certificate').
3. Read Error.Code (ValidationException, correct) and Error.Message; observe the reported regex is '-----BEGIN CERTIFICATE-----.*(.|\n)*-----END CERTIFICATE-----\n?' where AWS reports '-----BEGIN CERTIFICATE-----[\s\S]*-----END CERTIFICATE-----\n?'.
Replay: behavioral check iot-managed-integrations.provisioning-profile-lifecycle-status.
