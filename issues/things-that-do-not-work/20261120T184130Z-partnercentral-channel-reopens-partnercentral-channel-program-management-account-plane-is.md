Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: partnercentral-channel.delete-program-management-account-nonexistent-error
Issue-Shape-Id: com.amazonaws.partnercentralchannel#PartnerCentralChannel
Issue-Key: partnercentral-channel|ListProgramManagementAccounts|AccessDenied
Issue-Commit: a1afeb9b14b5a3f3f07e5dc2397b21c63bef9856

# REOPENS: partnercentral-channel program-management-account plane is still refused 403 AccessDenied "Sign the request with the emulator credentials" instead of the modeled AccessDeniedException / ValidationException / ResourceNotFoundException (1 cause)

> _Note: this issue was identified against the code base at commit `a1afeb9b14b5a3f3f07e5dc2397b21c63bef9856`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListProgramManagementAccounts

## Description
REOPENS 20261109T185542Z-partnercentral-channel-listprogrammanagementaccounts-deleteprogrammanagementaccount-entitlement-refusal-is-served.md: its fix left the call refused.

That issue is marked fixed and its Resolution states the program-management-account plane "now answers the way this issue describes", declining only the SigV4A signing gate as a vendor-level gap. Replaying its own recorded check id against the current checkout still gets the identical pre-dispatch refusal on the identical call, so the plane is still unreachable and the check still fails on its very first request. This is not a new defect: it is the same cause, still open, re-filed with the replay key it fails on now.

Also related but NOT covering: the open issue 20261029T174824Z-partnercentral-channel-is-unusable-every-operation-is-refused.md describes the same family of refusals and even lists this check id, but its recorded replay keys for this check bind to a later assertion (`...delete-program-management-account-nonexistent-error|run#4|AssertionError|assert code0 == "ValidationException"...`). In this run the check fails EARLIER, at the ListProgramManagementAccounts entitlement probe (check line 33), so the suite did not mark this failure OWNED. The other 19 partnercentral-channel failures in this run WERE marked OWNED by that issue and were skipped.

Cause 1 of 1 — the program-management-account plane answers every call with the pre-dispatch signing refusal, so neither the entitlement denial, nor input-shape validation, nor an existence check ever runs.

AWS: ListProgramManagementAccounts is a read-only paginated read of the program-management-account plane (Smithy, services/service-com-amazonaws-partnercentralchannel-partnercentralchannel/smithy-model/com-amazonaws-partnercentralchannel-partnercentralchannel.json: PartnerCentralChannel, awsJson1_0, authSchemes sigv4/sigv4a; ListProgramManagementAccounts -> ListProgramManagementAccountsResponse{items, nextToken}, readonly, paginated(inputToken nextToken / outputToken nextToken / pageSize maxResults / items items); input `catalog` @required @length 1..64 @pattern ^[a-zA-Z]*$, `maxResults` @range 1..100 default 20; modeled errors AccessDeniedException 403, ValidationException 400, ResourceNotFoundException 404, ThrottlingException 429, InternalServerException 500). For a correctly signed caller the answer is one of exactly two things: HTTP 200 with `items` always present (the empty list when the catalog and filters select nothing), or — when the caller's account holds no active AWS Partner benefit — the modeled AccessDeniedException (awsJson1_0 `__type` AccessDeniedException, HTTP 403, member `message` @required). The message for that entitlement class is fixed text with no variable part (recorded behavior only; the model documents AccessDeniedException merely as insufficient permissions): `This action requires an active AWS Partner benefit. Register as an AWS Partner if you haven't already, or contact AWS Partner Central Support.`
The same answer order governs DeleteProgramManagementAccount (Smithy: idempotent; `catalog` @required; `identifier` @required -> ProgramManagementAccountIdentifier @length 17..1011 @pattern ^(arn:[a-z-]+:partnercentral:[a-z0-9-]+:[0-9]{12}:catalog/[a-zA-Z]+/program-management-account/)?pma-[a-z0-9]{13}$; `clientToken` @idempotencyToken; same modeled error set plus ConflictException 409). Constraint validation runs FIRST, so an `identifier` that is not a pma- id or ARN at all is ValidationException 400 whose message is the fixed sentence `1 validation error detected.` plus a fieldList entry naming the offending member — `name` = the request member that failed (`identifier` here), `code` = the violation class (`INVALID_STRING_FORMAT` for a @pattern violation), `message` = `Value at '<member>' failed to satisfy constraint: <the rule from the model>`. The member name and the rule text come from the request and the model; the count and the wrapper sentence are fixed. Only for a well-formed identifier does the entitlement/existence decision follow: AccessDeniedException with the fixed partner-benefit sentence for an un-entitled account, otherwise ResourceNotFoundException 404 — and the same code and the same message on an immediate repeat of the identical call.

Emulator: every one of these calls is answered HTTP 403 with awsJson1_0 error code `AccessDenied` and message `Sign the request with the emulator credentials`, before any handler runs. Observed verbatim on the ListProgramManagementAccounts probe as `AssertionError: {'Message': 'Sign the request with the emulator credentials', 'Code': 'AccessDenied'}`. No modeled error is ever produced: not AccessDeniedException, not ValidationException, not ResourceNotFoundException, and no `fieldList`. The same body is what the other 19 checks of this service get on every other operation, and the same botocore session reaches sts:GetCallerIdentity successfully in the same run, so the caller's credentials are healthy and the refusal is this service's own pre-dispatch gate.

Why: a client cannot classify or act on the failure. `AccessDenied` is not a modeled error shape of this service at all, so a typed `except AccessDeniedException` / `except ValidationException` / `except ResourceNotFoundException` handler catches nothing and the fault falls through as unknown. The message tells a partner-onboarding flow to re-sign its request instead of to register as an AWS Partner. The 400-vs-403 boundary between "your request is malformed" and "you may not do this" disappears, so a retry policy that retries authorization faults retries a permanently malformed identifier forever; and because `items` is never served, the plane is unreadable and undeletable.

Fix: the pre-dispatch request-authentication/credential gate the router applies to this service must accept the signature scheme this service's callers use (the model binds aws.auth#sigv4a alongside sigv4, and the prior resolution identified the only registered authenticator as SigV4-HMAC-only) so that a request reaches a handler at all; then the request-validation stage and the entitlement/existence decision of the ListProgramManagementAccounts and DeleteProgramManagementAccount handlers, and the awsJson1_0 error serializer, which must emit the modeled AccessDeniedException / ValidationException (with `fieldList`) / ResourceNotFoundException bodies instead of a signing-layer `AccessDenied`. The prior resolution declined the gate as vendor-level; as long as it is declined the operation stays refused and this check cannot pass, so the fix has to land wherever that gate lives rather than only in the handlers.

Traces: partnercentral-channel.delete-program-management-account-nonexistent-error.

## Evidence
Behavioral suite (simparity-behavioral-aws-v1): partnercentral-channel 0/20 passed (0%) — 13 mismatched, 7 errored, 0 skipped, 0 unloadable. 19 of the 20 failures were marked "OWNED by 20261029T174824Z-partnercentral-channel-is-unusable-every-operation-is-refused.md" and were skipped. This one was NOT marked owned and is not marked ALREADY FILED.

[mismatch] partnercentral-channel.delete-program-management-account-nonexistent-error
  File "check_partnercentral_channel_delete_program_management_account_nonexistent_error.py", line 33, in run
    assert (error["Code"], error.get("Message")) == ("AccessDeniedException", _NO_PARTNER_BENEFIT), error
  AssertionError: {'Message': 'Sign the request with the emulator credentials', 'Code': 'AccessDenied'}

The failing call is the check's first one: ListProgramManagementAccounts(catalog='Sandbox').
Expected: HTTP 200 with `items` present (entitled caller), or HTTP 403 with Error.Code = `AccessDeniedException` and Error.Message = `This action requires an active AWS Partner benefit. Register as an AWS Partner if you haven't already, or contact AWS Partner Central Support.` (un-entitled caller). That sentence is entirely fixed text — it has no variable part.
Observed: HTTP 403, Error.Code = `AccessDenied`, Error.Message = `Sign the request with the emulator credentials`. `AccessDenied` is not a modeled error shape of this service.

The same check then grades DeleteProgramManagementAccount and expects, in order:
  identifier='not-a-valid-pma-identifier' -> ValidationException 400, message `1 validation error detected.` + fieldList[{name: identifier, code: INVALID_STRING_FORMAT, message: "Value at 'identifier' failed to satisfy constraint: <the ProgramManagementAccountIdentifier pattern rule>"}];
  identifier='pma-1111111111111' (well formed, nonexistent) -> AccessDeniedException with the fixed partner-benefit text for an un-entitled account, else ResourceNotFoundException;
  an immediate second identical call -> the same code and the same message.
None of these is reached, because the service refuses at the signing gate.

Smithy (services/service-com-amazonaws-partnercentralchannel-partnercentralchannel/smithy-model/com-amazonaws-partnercentralchannel-partnercentralchannel.json):
  ListProgramManagementAccounts — readonly, paginated(items/nextToken/maxResults); input catalog @required @length 1..64 @pattern ^[a-zA-Z]*$, maxResults @range 1..100 default 20, optional displayNames/programs/accountIds/statuses/sort/nextToken; output items + nextToken; errors AccessDeniedException(403, client), ValidationException(400, client), ResourceNotFoundException(404, client), ThrottlingException(429), InternalServerException(500). http POST /ListProgramManagementAccounts, bindings inert (awsJson1_0, dispatch by X-Amz-Target).
  DeleteProgramManagementAccount — idempotent; input catalog @required, identifier @required -> ProgramManagementAccountIdentifier @length 17..1011 @pattern ^(arn:[a-z-]+:partnercentral:[a-z0-9-]+:[0-9]{12}:catalog/[a-zA-Z]+/program-management-account/)?pma-[a-z0-9]{13}$, clientToken @idempotencyToken @length 1..64 @pattern ^[!-~]*$; empty output; errors AccessDeniedException(403), ConflictException(409), ValidationException(400), ResourceNotFoundException(404), ThrottlingException(429), InternalServerException(500).

Regression evidence: 20261109T185542Z-...-entitlement-refusal-is-served.md is status=fixed, carries Issue-Trace-Ids `partnercentral-channel.delete-program-management-account-nonexistent-error` and Issue-Key `partnercentral-channel|ListProgramManagementAccounts|AccessDenied`, and its Resolution claims the plane "now answers the way this issue describes" while declining the signing gate as vendor-level. Replaying that exact check now still yields the identical `AccessDenied` / `Sign the request with the emulator credentials` on the identical ListProgramManagementAccounts call, i.e. the fix left the call refused and nothing it implemented is observable from the wire.

Bootstrap gate: partnercentral-channel is stood up — crate service-com-amazonaws-partnercentralchannel-partnercentralchannel is in get_bootstrap_status().bootstrapped and get_bootstrap_status().pending is empty — and the service answers HTTP; it answers wrongly.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from it.
2. run_behavioral_suite() and look at the partnercentral-channel group: 0/20 pass; partnercentral-channel.delete-program-management-account-nonexistent-error fails at its first call (check line 33), and is the only failure in the group not marked OWNED.
3. With the emulator's standard credentials, call ListProgramManagementAccounts(catalog='Sandbox'). Expected either HTTP 200 with `items` present, or HTTP 403 AccessDeniedException with the fixed message "This action requires an active AWS Partner benefit. Register as an AWS Partner if you haven't already, or contact AWS Partner Central Support.". Observed HTTP 403 AccessDenied "Sign the request with the emulator credentials".
4. Call DeleteProgramManagementAccount(catalog='Sandbox', identifier='not-a-valid-pma-identifier'). Expected ValidationException 400, message "1 validation error detected.", with a fieldList entry naming `identifier` (code INVALID_STRING_FORMAT, message "Value at 'identifier' failed to satisfy constraint: <the pattern rule>"). Observed the same AccessDenied signing refusal.
5. Call DeleteProgramManagementAccount(catalog='Sandbox', identifier='pma-1111111111111') twice. Expected AccessDeniedException with the partner-benefit sentence (un-entitled account) or ResourceNotFoundException 404 (entitled account), identical on both calls. Observed the same AccessDenied signing refusal both times.
6. Confirm the regression against the fixed issue: its recorded Issue-Key is `partnercentral-channel|ListProgramManagementAccounts|AccessDenied`, which still reproduces unchanged.
7. Re-replay partnercentral-channel.delete-program-management-account-nonexistent-error to confirm the fix.
