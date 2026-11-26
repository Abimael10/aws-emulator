Issue-Source: trace-sweep-divergence
Issue-Trace: sesv2-send-bulk-email-empty-destination-entries
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: sesv2-send-bulk-email-empty-destination-entries
Issue-Shape-Id: com.amazonaws.sesv2#SimpleEmailService_v2
Issue-Key: sesv2|SendBulkEmail|2xx->4xx|$.BulkEmailEntryResults
Issue-Key: sesv2|SendBulkEmail|2xx->4xx|status
Issue-Commit: b9452e21b4cf2c09e703f5517f289577e00d07ce

# SendBulkEmail refuses the whole call when every entry's Destination is empty instead of returning the per-entry INVALID_PARAMETER result

> _Note: this issue was identified against the code base at commit `b9452e21b4cf2c09e703f5517f289577e00d07ce`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SendBulkEmail

## Description
Two net-new causes on service token sesv2, one shared root fault in SendBulkEmail's gate ORDER: when every BulkEmailEntries entry addresses no recipient at all, AWS settles the call on the entries alone and answers HTTP 200 whose BulkEmailEntryResults carries that entry's INVALID_PARAMETER result; the emulator instead keeps running the REQUEST-level gates that follow (the sending-identity check, and the account resources the request names) and refuses the whole call with a 4xx. The per-entry wording the emulator already holds is correct — only the stage at which the call is settled is wrong, so both causes are fixed in the same ordering decision in the SendBulkEmail handler.

Cause 1 of 2: a bulk send whose every entry has an empty Destination is refused 400 instead of being answered 200 with the entry's result.
AWS: Smithy (services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json) — SendBulkEmail is POST /v2/email/outbound-bulk-emails, success code 200; output member BulkEmailEntryResults (BulkEmailEntryResultList) is REQUIRED, and BulkEmailEntryResult carries Status (enum BulkEmailStatus, which includes INVALID_PARAMETER), Error ("A description of an error that prevented a message being sent") and MessageId. That required per-entry result list IS the operation's partial-success contract: a defect in one entry's own Destination is reported as that entry's result, not as a failure of the call. Recorded behavior for this step: HTTP 200 with BulkEmailEntryResults = [{"Status": "INVALID_PARAMETER", "Error": "Destination cannot be empty", "MessageId": null}]. The message is entirely fixed text — "Destination cannot be empty" names the member and echoes nothing from the request — and it is reported even though the request-level gates that follow (sender verification, the resources the request names) would not have passed, because AWS never reaches them: an entry with no recipient has no message to compose, so no identity and no template is weighed.
Emulator: answers HTTP 400 and no BulkEmailEntryResults member at all (replay: "status: expected 200, got 400" / "$.BulkEmailEntryResults: missing"). Reading the handler (src/ops/send_bulk_email.rs), the per-entry defect IS computed first (entry_defect -> crate::state::send_validation::EMPTY_DESTINATION, which is exactly "Destination cannot be empty"), but the handler then still runs the locked request-level section — the account sending switch, the configuration set the request names, and store.unverified_send_identities(...) for the FromEmailAddress — and returns from there. Hypothesis: the 400 is the MessageRejected refusal "Email address is not verified. The following identities failed the check in region US-WEST-2: <identity list>", since with every entry invalid the recipients set is empty and only the sending identity is weighed; the replay compared only status and $.BulkEmailEntryResults for this step, so the emulator's error code and message are not themselves asserted here.
Why: a client doing a bulk send gets a hard 4xx with nothing in BulkEmailEntryResults where AWS hands back a 200 and one result per entry. SDK code written against the contract reads BulkEmailEntryResults to learn which destinations were taken and which were rejected and why; on the emulator it instead sees an exception, so a batch whose only defect is one caller-side destination is indistinguishable from an account/identity failure, and the diagnosis the caller is given ("your sender is not verified") points at state that is not the cause.
Fix: the gate ORDER in the SendBulkEmail handler (src/ops/send_bulk_email.rs): once every entry has a per-entry defect there is no message to compose, so the handler must return the 200 partial-success response built from entry_defects instead of entering the locked request-level section (sending switch, configuration-set resolution, unverified_send_identities). The per-entry wording itself is already right and must not change.
Traces: sesv2-send-bulk-email-empty-destination-entries#task#1 (cluster also covers #task#2).

Cause 2 of 2: the same call is refused 404 instead of being answered 200 with the entry's result.
AWS: same model facts as Cause 1; NotFoundException is modeled on SendBulkEmail with httpError 404 ("The resource you attempted to access doesn't exist"), but recorded behavior for this step is again HTTP 200 with BulkEmailEntryResults = [{"Status": "INVALID_PARAMETER", "Error": "Destination cannot be empty", "MessageId": null}] — the absent account resource the request would otherwise be refused for is never resolved, because the call is already settled by its entries.
Emulator: answers HTTP 404 with no BulkEmailEntryResults member (replay: "status: expected 200, got 404" / "$.BulkEmailEntryResults: missing"). Hypothesis: this is the handler's NotFoundException for a resource the request names that the account does not hold — the configuration-set lookup in the locked section (store.configuration_sets.get(set_name), refused with crate::state::configuration_set_not_found) or the template resolution — i.e. the same too-late resource resolution as Cause 1, surfacing as 404 rather than 400 because a different gate is reached first.
Why: same client impact as Cause 1, with the extra confusion that a 404 tells the caller a named resource does not exist when AWS reports the call as a 200 whose single entry was rejected for its own empty destination. A caller that creates the missing resource still gets no per-entry results, and one that trusts the 404 removes a resource reference that was never the problem.
Fix: same single ordering change in src/ops/send_bulk_email.rs — no account resource (configuration set, template) may be resolved for a call in which no entry has a message to compose; the response is the 200 built from the per-entry defects.
Traces: sesv2-send-bulk-email-empty-destination-entries#task#3.

Neither cause is carried by an open issue's replay key (the replay report lists both clusters with an empty owned_by and no stub_path). Related open issues that do NOT close either cause: 20261124T235503Z-sesv2-send-path-s-unverified-identity-refusal-wrong.md (the identity list and wording of the unverified-identity refusal, and its being skipped on the bulk path — here the defect is that this refusal is reached AT ALL for a call AWS settles on its entries), and 20261125T090542Z-sesv2-2-net-new-causes-sendbulkemail-never-applies.md (a missing DefaultContent.TemplateData request-shape gate that must run before identity verification — a different gate, and one that applies to a request whose entries are well formed).

## Evidence
Replay of wire trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, 384 steps, 330 passed).

Cause 1 — fixture sesv2-send-bulk-email-empty-destination-entries, step task#1 (cluster divergence_key "service=sesv2|SendBulkEmail|status=200->400|keys=$.BulkEmailEntryResults,status", affected: #task#1, #task#2):
  status: expected 200, got 400
  $.BulkEmailEntryResults: missing (expected [{'Error': 'Destination cannot be empty', 'MessageId': None, 'Status': 'INVALID_PARAMETER'}], got None)

Cause 2 — fixture sesv2-send-bulk-email-empty-destination-entries, step task#3 (cluster divergence_key "service=sesv2|SendBulkEmail|status=200->404|keys=$.BulkEmailEntryResults,status"):
  status: expected 200, got 404
  $.BulkEmailEntryResults: missing (expected [{'Error': 'Destination cannot be empty', 'MessageId': None, 'Status': 'INVALID_PARAMETER'}], got None)

Requirement vs per-run values: the requirement is status 200, a BulkEmailEntryResults list with one result per request entry, Status 'INVALID_PARAMETER' and the fixed Error text 'Destination cannot be empty' with MessageId absent for a rejected entry. Nothing in that expectation is per-run.

Model evidence (smithy_query on services/service-com-amazonaws-sesv2-simpleemailservice-v2/smithy-model/com-amazonaws-sesv2-simpleemailservice-v2.json):
  SendBulkEmail — http POST /v2/email/outbound-bulk-emails, code 200; input DefaultContent (required), BulkEmailEntries (BulkEmailEntryList, required); output BulkEmailEntryResults (BulkEmailEntryResultList, REQUIRED); errors AccountSuspendedException/BadRequestException/LimitExceededException/MailFromDomainNotVerifiedException/MessageRejected/SendingPausedException @400, NotFoundException @404, TooManyRequestsException @429.
  BulkEmailEntryResult — members Status (BulkEmailStatus), Error (ErrorMessage), MessageId (OutboundMessageId).
  BulkEmailStatus — values include SUCCESS, MESSAGE_REJECTED, MAIL_FROM_DOMAIN_NOT_VERIFIED, CONFIGURATION_SET_NOT_FOUND, TEMPLATE_NOT_FOUND, INVALID_PARAMETER, TRANSIENT_FAILURE, FAILED.
  BulkEmailEntry — Destination (required); Destination — ToAddresses/CcAddresses/BccAddresses, all optional.

Code evidence (services/service-com-amazonaws-sesv2-simpleemailservice-v2/src/ops/send_bulk_email.rs): entry_defect() returns crate::state::send_validation::EMPTY_DESTINATION ("Destination cannot be empty", src/state/send_validation.rs line 48) for an entry whose three recipient lines are empty, and renders_a_message is computed from the defects — but the handler then enters the locked block that reads the account sending switch, resolves input.configuration_set_name (NotFoundException, 404) and calls store.unverified_send_identities(...) (MessageRejected, 400) before any response is built, so a call whose every entry is already rejected is still refused by those gates.

## Steps to Reproduce
1. git_clone the repository and boot the emulator built from that checkout.
2. Replay the wire fixture corpus (replay_wire_corpus) and look at fixture sesv2-send-bulk-email-empty-destination-entries.
3. Cause 1: run its task#1 (and task#2) — SendBulkEmail with DefaultContent.Template naming a template and BulkEmailEntries holding entries whose Destination addresses nobody (ToAddresses/CcAddresses/BccAddresses all absent or empty). Observe HTTP 400 with no BulkEmailEntryResults, instead of HTTP 200 and BulkEmailEntryResults=[{"Status":"INVALID_PARAMETER","Error":"Destination cannot be empty","MessageId":null}].
4. Cause 2: run its task#3 — the same all-entries-empty bulk send in the variant whose request names an account resource the emulator resolves (configuration set / template). Observe HTTP 404 with no BulkEmailEntryResults, instead of the same HTTP 200 per-entry result.
5. Confirm the positive control still holds after the fix: a bulk send with at least one well-formed Destination must STILL be weighed against the account sending switch, the configuration set it names and the sending identity, and must still be refused whole when those gates fail.
6. Re-replay sesv2-send-bulk-email-empty-destination-entries (task#1, task#2, task#3) and confirm all three steps pass.
