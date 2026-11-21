Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wickr.create-bot-weak-challenge-rejected-by-password-policy
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr.create-bot-weak-challenge-rejected-by-password-policy|run#9|AssertionError|assert error["Code"] == "ValidationError", f"expected ValidationError, got {error['Code']!r}"
Issue-Commit: 49e456ae4cd7e040443ab8363e847bf32acfddc0

# wickr CreateBot does not enforce the security group's password policy on `challenge`: a 6-character password is answered 501 NotImplemented instead of 422 ValidationError with reasons[0].field='passwordPolicy'

> _Note: this issue was identified against the code base at commit `49e456ae4cd7e040443ab8363e847bf32acfddc0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBot

## Description
Cause 1 of 1: CreateBot performs no password-policy validation of the `challenge` member against the target security group, so the pre-mutation rejection AWS performs for a sub-minimum-length bot password never happens; the call is instead refused as an unimplemented scaffold.

Related already-filed work items (named for the fixer, neither of which closes this one): (a) `20261121T040104Z-wickr-createbot-stub.md` ("Implement wickr CreateBot: unimplemented scaffold stub") covers only 'the operation is a 501 stub' and will be closed by any handler that returns a success envelope — it does not state the password-policy contract below, which still fails afterwards; (b) `20261121T040104Z-wickr-createbot-does-not-enforce-the-security-group.md` describes this same root cause but carries no behavioral replay key, so the suite does not report this check as owned and it is re-filed here with its check id bound. Treat (b) as the same cause, not a second one.

AWS: Smithy (com.amazonaws.wickr#WickrAdminApi) models CreateBot as POST /networks/{networkId}/bots with required `networkId`, `username`, `groupId` and `challenge` (SensitiveString, pattern ^[\S\s]*$ only — the model imposes NO length or complexity constraint, so the rule is enforced in the handler, not by constraint validation), and declares `ValidationError` (httpError 422) among its errors. ValidationError's members are `message` (GenericString) and `reasons` (ErrorDetailList of ErrorDetail{field, reason}). Per AWS Wickr network security groups (https://docs.aws.amazon.com/wickr/latest/adminguide/network-security-groups.html) a security group carries a password policy — a minimum length plus required counts of uppercase, lowercase, numeric and symbol characters — and a STANDARD network's default group requires a minimum of 8 characters. Recorded behavior for a challenge violating that policy: HTTP 422, Error.Code = "ValidationError", top-level Error.Message = "" (EMPTY — the diagnosis lives in reasons[], not the top-level message), and exactly ONE entry in `reasons` with `field` = "passwordPolicy" and `reason` = the policy-summary text:

  "password does not meet minimum requirements: \n      {uppercase} uppercase letter(s), \n      {lowercase} lowercase letter(s), \n      {numbers} number(s), \n      {symbols} symbol(s), \n      minimum {minLength} letters\n      "

Fixed text of that message: the leading sentence "password does not meet minimum requirements: ", the five requirement lines in that order with their literal labels "uppercase letter(s),", "lowercase letter(s),", "number(s),", "symbol(s)," and "minimum ... letters", and the exact newline/indentation layout (each requirement line prefixed by a newline and six spaces, with a trailing newline and six spaces at the end). Variable parts: the five numbers, read from the password policy of the security group named by `groupId` in the request — they are NOT derived from the submitted password and are NOT constants of the message (for the default STANDARD group the recorded values happen to be 0/0/0/0 with minimum 8; a group with a stricter policy must report that group's numbers). The refusal is independent of the username-format check: a well-formed username plus a policy-violating challenge yields this single passwordPolicy reason only. The refusal is also atomic — afterwards ListBots for the network returns an empty `bots` list — and a subsequent CreateBot with a policy-satisfying challenge succeeds and echoes `username` back unchanged.

Emulator: every CreateBot call is refused with HTTP 501, Error.Code = "NotImplemented", message "operation CreateBot is scaffolded but not implemented in this emulator". The replay asserts on the error code and reports: AssertionError: expected ValidationError, got 'NotImplemented'. No password-policy check, no 422 ValidationError envelope and no reasons[] detail exist anywhere on this path.

Why: a client provisioning Wickr bots cannot distinguish a rejected weak bot password from an unsupported operation. Code that branches on Error.Code == "ValidationError" and reads reasons[0].field to decide which input to re-prompt for (password vs username vs group) instead sees 501 NotImplemented — not a modeled CreateBot error at all — so a user-correctable input error is handled as a service outage and retried rather than surfaced. Conversely, once CreateBot is implemented without the policy check, bots are created with passwords AWS would never accept, silently breaking the caller's assumption that every bot in the network satisfies its group's policy.

Fix: CreateBot's handler and its validation step (services/service-com-amazonaws-wickr-wickradminapi/src/ops/create_bot.rs). Resolve the security group named by `groupId` from the network's state, compare `challenge` against that group's password policy (minimum length and the four character-class counts) BEFORE any state mutation, and on violation return the modeled ValidationError (HTTP 422) with an empty top-level `message` and exactly one ErrorDetail whose `field` is "passwordPolicy" and whose `reason` is rendered from that group's policy values in the layout above, leaving the network's bot set untouched.

Traces: wickr.create-bot-weak-challenge-rejected-by-password-policy (a 6-character challenge against the default security group of a freshly created STANDARD network).

## Evidence
Check: wickr.create-bot-weak-challenge-rejected-by-password-policy (verdict: mismatch)

Expected (AWS / recorded):
  HTTP 422
  Error.Code    = "ValidationError"
  Error.Message = ""            (empty; the detail is in reasons[])
  reasons       = [ { "field": "passwordPolicy",
                      "reason": "password does not meet minimum requirements: \n      0 uppercase letter(s), \n      0 lowercase letter(s), \n      0 number(s), \n      0 symbol(s), \n      minimum 8 letters\n      " } ]
  ListBots(networkId) -> bots == []    (nothing was created)
  then CreateBot with a policy-satisfying challenge -> 200 with `username` echoed back
  (the five numbers above are the referenced security group's password-policy values, filled into the template; they are not literals of the message)

Actual (emulator):
  HTTP 501
  Error.Code    = "NotImplemented"
  Error.Message = "operation CreateBot is scaffolded but not implemented in this emulator"

Replay output:
  File "check_wickr_create_bot_weak_challenge_rejected_by_password_policy.py", line 74, in run
    assert error["Code"] == "ValidationError", f"expected ValidationError, got {error['Code']!r}"
  AssertionError: expected ValidationError, got 'NotImplemented'

Model corroboration (smithy_query on services/service-com-amazonaws-wickr-wickradminapi/smithy-model): CreateBot errors include ValidationError with httpError 422; ValidationError members are `message` (GenericString) and `reasons` -> ErrorDetailList -> ErrorDetail{field, reason}; `challenge` is required with pattern ^[\S\s]*$ and no length/complexity constraint, so the policy is a handler-side rule.

## Steps to Reproduce
1. Boot the emulator and create a wickr client for region us-east-1.
2. network = CreateNetwork(networkName=<unique, <=20 chars>, accessLevel="STANDARD"); note networkId.
3. groupId = ListSecurityGroups(networkId=networkId)["securityGroups"][0]["id"]  (the default group; minimum password length 8).
4. Call CreateBot(networkId=networkId, username=<unique>-bot, displayName="Weak", groupId=groupId, challenge=<a 6-character password, below the group minimum>).
   Expected: HTTP 422, Error.Code "ValidationError", empty Error.Message, exactly one reasons[] entry with field "passwordPolicy" and the policy-summary reason text rendered from that group's policy values.
   Actual:   HTTP 501, Error.Code "NotImplemented".
5. Call ListBots(networkId=networkId) — expected bots == [], proving the refusal created nothing.
6. Call CreateBot again with a challenge at or above the group minimum — expected 200 with `username` echoed back, confirming step 4's refusal was the policy check and not a blanket failure.
7. Cleanup: DeleteNetwork(networkId=networkId).
