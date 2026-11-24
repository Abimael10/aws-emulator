Issue-Source: trace-sweep-divergence
Issue-Trace: network-security-manager-scope-and-template-crud
Issue-Step: task#5
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: network-security-manager-scope-and-template-crud
Issue-Shape-Id: com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService
Issue-Key: network-security-manager|GetTemplate|4xx->4xx|$.message
Issue-Commit: 9b3ec711581582504a687984a45cdda758f31515

# network-security-manager GetTemplate not-found refusal states the generic resource message instead of AWS's "Template not found: <templateId>"

> _Note: this issue was identified against the code base at commit `9b3ec711581582504a687984a45cdda758f31515`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetTemplate

## Description
One net-new divergence cluster in the Network Security Manager (com.amazonaws.networksecuritymanager#PiccoloCustomerAPIService, restJson1) TEMPLATE read surface, observed replaying the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (emulator_up true, /__admin/health 200, 8 fixtures, 55 steps, 31 passed, 24 mismatched, score_pct 56.4, 4 divergence clusters — all service=network-security-manager). This cluster reports stub_path "" and owned_by [], i.e. no filed issue carries its replay key; the other three clusters are reported owned by '20261121T005237Z-network-security-manager-policy-validation-and-scope-read.md' and are excluded.

Related open issue that does NOT own this cause: '20261118T134137Z-network-security-manager-template-read-path-and-response.md' faulted this same step when the whole miss path was wrong (its Cause 3 = GetTemplate answering HTTP 200 where AWS answers 404, replay key GetTemplate|4xx->2xx|...). At this commit the status half is fixed — the read now answers 404 with the modeled ResourceNotFoundException — and the replay reports a NEW key (GetTemplate|status=404->404|keys=$.message) with owned_by [], because only the error MESSAGE TEXT is still divergent. That remaining half is what is filed here.

=== Cause 1 of 1: the 404 ResourceNotFoundException for an unresolvable templateIdentifier carries the service's shared generic sentence instead of AWS's template-specific message ===
AWS: Smithy model com.amazonaws.networksecuritymanager#GetTemplate — GET /templates/{templateIdentifier}, success @http code 200; input templateIdentifier is @httpLabel + @required (TemplateIdentifier, @length 1..1010). Modeled errors: AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400. ResourceNotFoundException is @error client / @httpError 404, documented "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again.", with @required message plus optional resourceId and resourceType. The model does not dictate the message text, so the message is recorded behavior: for a templateIdentifier naming a template that no longer exists (in the recorded CRUD fixture, the template the preceding removal step deleted) the recorded AWS response is HTTP 404 with Error.Message "Template not found: <templateId>". Its FIXED text is "Template not found: "; its single VARIABLE part is the template id taken from the request's templateIdentifier (the recording's own id value is one run's fill of that template, not part of it). The status (404), the error shape and the x-amzn-errortype header all match already.
Emulator: answers HTTP 404 with ResourceNotFoundException but states message "The requested resource was not found." The replay reports `$.message: value_mismatch (expected 'Template not found: <templateId>', got 'The requested resource was not found.')`. The text is the service-wide constant crate::state::RESOURCE_NOT_FOUND_MESSAGE, taken verbatim by the template read's miss branch; the same module's doc comment already records that a read whose wording is known to differ must state its own sentence (the rule read does exactly that with "Rule not found: <ruleId>"), and the template read is such a read.
Why: the error message is part of the contract. A client that matches, logs, alerts on or displays the not-found message for a template read sees a sentence AWS never sends, and it loses the identifier AWS puts in the text — so a caller cannot tell from the message WHICH template was missing when several reads are in flight, and a test asserting AWS's exact message fails against the emulator even though the status and error code are right.
Fix: GetTemplate's error path — the ResourceNotFoundException the template-lookup miss branch constructs must state its own message, "Template not found: " followed by the template id derived from the request's templateIdentifier, rather than taking the shared generic constant. resourceId/resourceType are already populated and are not at issue.
Traces: network-security-manager-scope-and-template-crud#task#5.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout: emulator_up true ("/__admin/health answered 200 and the :41393 data listener accepts"), 8 fixtures replayed, 55 steps (31 passed, 24 mismatched, 0 blocked, 0 errored), score_pct 56.4, 4 divergence clusters — every one service=network-security-manager. Three carry a non-empty owned_by and are excluded; the one below reports stub_path "" and owned_by [].

Cause 1 — fixture network-security-manager-scope-and-template-crud, step task#5, operation GetTemplate, verdict mismatch, expected_status 404 / actual_status 404, affected_count 1, divergence_key "service=network-security-manager|GetTemplate|status=404->404|keys=$.message":
  $.message: value_mismatch (expected 'Template not found: 4acc6233fa33ba37fe70852ca9309f7b1', got 'The requested resource was not found.')
  (the recorded id 4acc6233fa33ba37fe70852ca9309f7b1 is that run's fill of the variable part; the requirement is the fixed text "Template not found: " plus the template id from the request's templateIdentifier)
  model: GetTemplate GET /templates/{templateIdentifier} @http code 200; templateIdentifier @httpLabel @required, TemplateIdentifier @length 1..1010; errors AccessDeniedException 403, InternalServerException 500, ResourceNotFoundException 404, ThrottlingException 429, ValidationException 400; ResourceNotFoundException @error client @httpError 404 { message required, resourceId, resourceType }, doc "The specified resource was not found. Verify that the resource identifier is correct and that the resource exists, then try your request again."
  code: services/service-com-amazonaws-networksecuritymanager-piccolocustomerapiservice/src/ops/get_template.rs lines 182-191 — the `store.template_by_identifier(&input.template_identifier)` miss branch builds `ResourceNotFoundException { message: crate::state::RESOURCE_NOT_FOUND_MESSAGE.to_string(), resource_id: Some(input.template_identifier.clone()), resource_type: Some(crate::state::RESOURCE_TYPE_TEMPLATE.to_string()) }`; services/.../src/state.rs line 75 — `pub const RESOURCE_NOT_FOUND_MESSAGE: &str = "The requested resource was not found."`, whose own doc comment at lines 67-74 notes the rule read words its miss as "Rule not found: <ruleId>" and that a read known to differ states its own sentence.
  status, error shape and x-amzn-errortype all matched in this step — only $.message diverged.

## Steps to Reproduce
Replay the simparity-wire-aws-v1 wire corpus against the emulator built from this checkout (replay_wire_corpus), or drive the step directly against the NSM endpoint:

Cause 1: create a template (POST /templates with a valid templateName, associatedRuleList and firewallType="WAF"), delete it (the fixture's template-removal step, DeleteTemplate), then GET /templates/<that template's ARN> (botocore: nsm.get_template(templateIdentifier=arn)). Expect HTTP 404 with an x-amzn-errortype header naming ResourceNotFoundException and message "Template not found: <the template id from the identifier>"; observe HTTP 404 whose message is the generic "The requested resource was not found." Replay network-security-manager-scope-and-template-crud#task#5.
