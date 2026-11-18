Issue-Source: trace-sweep-divergence
Issue-Trace: network-security-manager-scope-and-template-crud
Issue-Step: task#2
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: network-security-manager-scope-and-template-crud
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager|CreateTemplate|2xx->2xx|$.updatedAt
Issue-Key: network-security-manager|GetTemplate|2xx->2xx|$.updatedAt
Issue-Key: network-security-manager|GetTemplate|4xx->2xx|$.message
Issue-Key: network-security-manager|GetTemplate|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: network-security-manager|GetTemplate|4xx->2xx|status
Issue-Commit: b2652fd521aaccb01db45f6fe06ce7b73f89751d

# network-security-manager template read path and response serialization diverge: 3 causes — updatedAt serialized as an epoch number on CreateTemplate and GetTemplate, and GetTemplate's miss path answers 200 instead of 404

> _Note: this issue was identified against the code base at commit `b2652fd521aaccb01db45f6fe06ce7b73f89751d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTemplate

## Description
Three net-new divergence clusters in the Network Security Manager (PiccoloCustomerAPIService, restJson1) TEMPLATE entity surface, observed replaying the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (emulator_up true, /__admin/health 200, 55 steps, 9 passed, score 16.4%). All three clusters report stub_path "" and owned_by [], i.e. no filed issue carries their replay key. They share one fault: the template read/response path does not answer what AWS answers — the stored record is rendered with the wrong wire type for its timestamp, and an identifier that must no longer resolve still resolves.

Related open issues, none of which owns any cause here: '20261118T031240Z-network-security-manager-rule-read-path-and-response.md' faults the SAME two clauses on the RULE entity (its Cause 1 = GetRule miss path, its Cause 3 = DateTimestamp epoch serialization on CreateRule/GetRule) and carries only those rule replay keys, so the template steps below are not exercised by it — the replay reports no owner for them; '20261109T164319Z-network-security-manager-create-entity-operations-diverge-3.md' owns CreatePolicy WAF validation, the CreateRule pattern-anchor message and CreateScope's 501; '20261118T113746Z-network-security-manager-createtemplate-stub.md' claims CreateTemplate is an unimplemented scaffold, which is stale — CreateTemplate answers 201 with a full body here (src/ops/create_template.rs carries no SCAFFOLDED marker), and the divergence below is in what it serializes, not that it is missing.

=== Cause 1 of 3: CreateTemplate serializes `updatedAt` as a JSON epoch-seconds number instead of the modeled date-time string ===
AWS: Smithy model com.amazonaws.networksecuritymanager#CreateTemplate — POST /templates, success @http code 201; CreateTemplateOutput.updatedAt targets shape DateTimestamp, a timestamp carrying @timestampFormat("date-time") (documented "The time when the resource was last updated."). Under restJson1 a @timestampFormat("date-time") member is serialized as an RFC-3339/ISO-8601 STRING (e.g. "2026-11-18T03:12:40.860Z"), never as the protocol-default epoch-seconds number. The recorded AWS response carries updatedAt in that string form; the fixture asserts the member's SHAPE, because the instant itself is a per-run value and is not a requirement.
Emulator: emits a JSON number of epoch seconds with a fractional part for $.updatedAt in the 201 CreateTemplate body, so the member fails its date-time shape assertion. Status (201) and every other member of the body match.
Why: this is a wire-type divergence, not a formatting nicety. An SDK deserializing the modeled date-time member against a JSON number either raises a parse error or yields a wrong value, so every client that reads a template's last-modified time breaks on a response AWS would have sent as a string. The defect is in the shared rendering of DateTimestamp, so it affects every entity response in this service that exposes updatedAt.
Fix: the response serializer for DateTimestamp members (the shared timestamp-rendering path CreateTemplate's output serializer goes through) must honor @timestampFormat("date-time") and emit a string; the handler is also handing it a DateTime built from a SystemTime with sub-second precision where a date-time value is required.
Traces: network-security-manager-scope-and-template-crud#task#2.

=== Cause 2 of 3: GetTemplate serializes `updatedAt` as a JSON epoch-seconds number instead of the modeled date-time string ===
AWS: Smithy model GetTemplate — GET /templates/{templateIdentifier}, success @http code 200; GetTemplateOutput.updatedAt targets the same DateTimestamp shape with @timestampFormat("date-time"), so the 200 read body carries updatedAt as an RFC-3339/ISO-8601 string exactly as the create response does. A read must not disagree with the create that returned it, and neither may use the protocol-default epoch encoding.
Emulator: the 200 GetTemplate body carries $.updatedAt as a JSON epoch-seconds number with a fractional part — the same value the create returned, in the same wrong wire type. Status and all other members match.
Why: the same client-side deserialization break as Cause 1, now on the read path every poll/describe loop uses. A caller comparing a template's updatedAt across reads, or feeding it to a modeled date-time field, gets a number where the model promises a string.
Fix: the same shared DateTimestamp serializer, reached here through GetTemplate's output serializer; it is a separate cluster because it is a separate operation's response path and must be re-replayed on its own.
Traces: network-security-manager-scope-and-template-crud#task#3.

=== Cause 3 of 3: GetTemplate still resolves a template identifier that must no longer resolve — HTTP 200 with the template body instead of the modeled 404 ResourceNotFoundException, and the service's miss message is the wrong text ===
AWS: Smithy model GetTemplate — GET /templates/{templateIdentifier}, code 200; input templateIdentifier is @httpLabel + @required (TemplateIdentifier, @length 1..1010), documented "The identifier of the template. This is the template's Amazon Resource Name (ARN)." Modeled errors: AccessDeniedException (403), InternalServerException (500), ResourceNotFoundException (404), ThrottlingException (429), ValidationException (400). ResourceNotFoundException is documented "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again." and carries @required `message` plus optional `resourceId`/`resourceType`. When the identifier names a template that no longer exists (in the recorded CRUD fixture, the template the preceding removal step deleted), the recorded AWS response is HTTP 404 with an x-amzn-errortype response header naming the error shape and Error.Message "Template not found: <templateId>" — the fixed text is "Template not found: " and the one variable part is the template id taken from the request's templateIdentifier (the recording's own id value is just one run's fill of that template). A template, once removed, must never again be returned by this read.
Emulator: answers HTTP 200 and serializes the full GetTemplateOutput for that identifier. The replay reports `status: expected 404, got 200`, `$.message: missing (expected 'Template not found: <templateId>', got None)` and `$headers.x-amzn-errortype: missing (expected shaped, got None)`. Two defects compound here: (a) the template is still present in the template store at that point in the lifecycle, so GetTemplate's miss path never runs; (b) when that path does run it states the shared constant "The requested resource was not found." (src/state.rs), not AWS's "Template not found: <templateId>" — the rule read already carries its own wording, and the template read needs its own too. Hypothesis for (a): the fixture's removal step is the unimplemented DeleteTemplate scaffold (it answers 501 and is counted blocked, its own stub issue owning that operation), so nothing ever drops the entity from state; the requirement filed here is GetTemplate's own contract — an unresolvable template identifier must produce the modeled 404 with AWS's message wording and the x-amzn-errortype header, not a 200 body.
Why: a client cannot distinguish a live template from a deleted one. Existence probes and idempotent teardown loops ("delete, then confirm GetTemplate raises ResourceNotFoundException") never terminate the way they do on AWS, `except nsm.exceptions.ResourceNotFoundException` is dead code, and a caller re-reads and re-associates a template AWS would consider gone. A client matching or displaying the error text sees a different sentence than AWS sends.
Fix: GetTemplate's handler and the template entity state it reads — the lookup that resolves templateIdentifier against the template store must not return removed entities — plus the ResourceNotFoundException message text its error path constructs.
Traces: network-security-manager-scope-and-template-crud#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true ("/__admin/health answered 200 and the :41247 data listener accepts"), 8 fixtures replayed, 55 steps (9 passed, 38 mismatched, 8 blocked), score_pct 16.4, 11 divergence clusters — every one service=network-security-manager. The 8 clusters reported with a non-empty owned_by are excluded; the 3 below report stub_path "" and owned_by [].

Cause 1 — fixture network-security-manager-scope-and-template-crud, step task#2, operation CreateTemplate, verdict mismatch, expected_status 201 / actual_status 201, affected_count 1:
  $.updatedAt: value_mismatch (expected '<shaped-not-asserted>' date-time string, got 1795009177.884567)
  model: CreateTemplate POST /templates @http code 201; CreateTemplateOutput.updatedAt -> DateTimestamp = timestamp @timestampFormat "date-time"; protocol restJson1.
  code: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/create_template.rs builds the output from the record it stores; the timestamp value comes from Template::updated_at_timestamp() (same pattern as src/state.rs lines 164-167, `to_date_time(self.updated_at)`).

Cause 2 — fixture network-security-manager-scope-and-template-crud, step task#3, operation GetTemplate, verdict mismatch, expected_status 200 / actual_status 200, affected_count 1:
  $.updatedAt: value_mismatch (expected '<shaped-not-asserted>' date-time string, got 1795009177.884567)
  model: GetTemplate GET /templates/{templateIdentifier} @http code 200; GetTemplateOutput.updatedAt -> DateTimestamp @timestampFormat "date-time".
  code: services/.../src/ops/get_template.rs line 205 — `updated_at: Some(template.updated_at_timestamp())`.

Cause 3 — fixture network-security-manager-scope-and-template-crud, step task#5, operation GetTemplate, verdict mismatch, expected_status 404 / actual_status 200, affected_count 1:
  status: expected 404, got 200
  $.message: missing (expected 'Template not found: <templateId>' — the recording shows 'Template not found: 2403e4e80f361d066b1e46812d15505c1', the id being the request-derived part — got None)
  $headers.x-amzn-errortype: missing (expected shaped, not asserted; got None)
  model: GetTemplate errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ResourceNotFoundException { message required, resourceId, resourceType }, @error client @httpError 404.
  code: services/.../src/ops/get_template.rs lines 181-191 — `store.template_by_identifier(..)` still resolves the identifier, and the miss branch it would otherwise take builds its message from `crate::state::RESOURCE_NOT_FOUND_MESSAGE` = "The requested resource was not found." (src/state.rs line 60) instead of "Template not found: <templateId>". The fixture's removal step is still a scaffold: services/.../src/ops/delete_template.rs line 184 returns `Scaffolded::not_implemented("DeleteTemplate")`.

## Steps to Reproduce
Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (replay_wire_corpus), or drive the steps directly against the NSM endpoint:

Cause 1: POST /templates with a valid templateName, associatedRuleList and firewallType=WAF (botocore: nsm.create_template(templateName=..., associatedRuleList=[...], firewallType="WAF")). Expect HTTP 201 whose $.updatedAt is an ISO-8601/RFC-3339 date-time STRING (DateTimestamp @timestampFormat date-time); observe a JSON number of epoch seconds with a fractional part. Replay network-security-manager-scope-and-template-crud#task#2.

Cause 2: GET /templates/<the returned templateArn> (botocore: nsm.get_template(templateIdentifier=arn)). Expect HTTP 200 whose $.updatedAt is a date-time string; observe the same epoch-seconds number. Replay network-security-manager-scope-and-template-crud#task#3.

Cause 3: create a template, run the fixture's template-removal step for it, then GET /templates/<that template's ARN>. Expect HTTP 404 with an x-amzn-errortype header naming the error shape and message "Template not found: <the template id from the identifier>"; observe HTTP 200 with the full template body, no error members and no errortype header (and, when the miss path is reached at all, the generic "The requested resource was not found." text). Replay network-security-manager-scope-and-template-crud#task#5.
