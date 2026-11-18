Issue-Source: trace-sweep-divergence
Issue-Trace: iot-managed-integrations-provisioning-profile-lifecycle
Issue-Step: teardown#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-managed-integrations-provisioning-profile-lifecycle
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations|GetProvisioningProfile|4xx->2xx|$.Message
Issue-Key: iot-managed-integrations|GetProvisioningProfile|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: iot-managed-integrations|GetProvisioningProfile|4xx->2xx|status
Issue-Commit: 22ca9934d3e7d714a2a3064609b658283f5e8796

# GetProvisioningProfile still serves a deleted profile with 200 instead of ResourceNotFoundException

> _Note: this issue was identified against the code base at commit `22ca9934d3e7d714a2a3064609b658283f5e8796`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetProvisioningProfile

## Description
Cause 1 of 1: a provisioning profile that has already been deleted is still readable. After DeleteProvisioningProfile succeeds for an identifier, the next GetProvisioningProfile on that same identifier must report the resource as gone; the emulator instead answers the read as if the profile still existed.

AWS: Smithy model services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations: GetProvisioningProfile is GET /provisioning-profiles/{Identifier} (success code 200) and models ResourceNotFoundException (errorKind client, httpError 404, doc "The specified resource does not exist.", members Message/ResourceId/ResourceType). DeleteProvisioningProfile is DELETE /provisioning-profiles/{Identifier}, @idempotent, output Unit. So a read of an identifier that no longer names a profile is a 404 ResourceNotFoundException, carrying the awsJson/restJson error discriminator header x-amzn-errortype: ResourceNotFoundException and a Message body member. The recorded response gives the message for this error class as the fixed text "Provisioning Profile resource not found" — that whole string is fixed text for the operation's not-found class and has no variable parts interpolated from the request (the requested Identifier is not echoed into it); the header value is the fixed error shape name "ResourceNotFoundException".

Emulator: the step replays GET on the just-deleted profile identifier and the emulator answers HTTP 200 with a success body: $.Message is absent (expected 'Provisioning Profile resource not found', got None) and the header $headers.x-amzn-errortype is absent (expected 'ResourceNotFoundException', got None). No error is raised at all, so the deleted profile continues to read back as live. Hypothesis: DeleteProvisioningProfile returns success without removing the profile from the service's state map (or removes it under a different key than GetProvisioningProfile looks up), leaving the read path with a live entry — neither the model nor the recording states which of the two sides drops the removal.

Why: a client that deletes a provisioning profile and then polls the read to confirm removal never observes removal — it sees a live profile forever and either loops, or concludes the delete failed and retries it. Conversely a client using the read as an existence check will treat a deleted profile as available and keep handing its claim certificate / id to downstream provisioning calls. Teardown and re-create flows that rely on the 404 to know an identifier is free also break, and SDK error handling for ResourceNotFoundException is never exercised because no x-amzn-errortype is ever emitted.

Fix: the provisioning-profile state and the GetProvisioningProfile handler for iot-managed-integrations — DeleteProvisioningProfile must remove the profile from the shared state under the same identifier key the read resolves, and GetProvisioningProfile must return the modeled ResourceNotFoundException (HTTP 404, x-amzn-errortype: ResourceNotFoundException, Message "Provisioning Profile resource not found") when the identifier resolves to nothing, rather than falling through to the success serializer.

Traces: iot-managed-integrations-provisioning-profile-lifecycle#teardown#7.

Note: no currently open issue covers this. The closest open items are the delete-semantics issues 20261109T005742Z-iot-managed-integrations-deletecloudconnector-refuses-an-absent-identifier-with-404-instead.md and 20261114T165642Z-iot-managed-integrations-deletenotificationconfiguration-refuses-an-unconfigured-eventtype-with-404-instead.md, which are the opposite fault (a delete over-raising 404) on different operations, and 20261029T224743Z-iot-managed-integrations-deleteotataskconfiguration-404-reuses-the-get-not-found-message.md, which is about not-found message wording on a delete. None of them makes a deleted provisioning profile stop reading back as 200.

## Evidence
Fixture iot-managed-integrations-provisioning-profile-lifecycle, step teardown#7, operation GetProvisioningProfile.

Expected (recorded AWS): status 404; $headers.x-amzn-errortype = 'ResourceNotFoundException'; $.Message = 'Provisioning Profile resource not found'.

Actual (emulator): status 200; $.Message missing (got None); $headers.x-amzn-errortype missing (got None) — i.e. a success response for an identifier whose profile was deleted earlier in the same fixture.

Replay divergence key: service=iot-managed-integrations|GetProvisioningProfile|status=404->200|keys=$.Message,$headers.x-amzn-errortype,status (verdict mismatch, affected_count 1).

Model grounding: GetProvisioningProfile errors include ResourceNotFoundException (client, httpError 404); DeleteProvisioningProfile is idempotent with Unit output.

## Steps to Reproduce
1. Replay fixture iot-managed-integrations-provisioning-profile-lifecycle against the emulator (replay_wire_corpus).
2. The fixture creates a provisioning profile, exercises it, then in teardown issues DELETE /provisioning-profiles/{Identifier} for it.
3. At step teardown#7 it issues GET /provisioning-profiles/{Identifier} for that same, now-deleted identifier.
4. Observe the emulator answers 200 with a success body and no x-amzn-errortype header, instead of 404 ResourceNotFoundException with Message 'Provisioning Profile resource not found'.
