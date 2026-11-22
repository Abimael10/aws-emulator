Issue-Severity: HIGH
Issue-Commit: 55987962084103a763901970f01bf3b64be75e84

# wickr CreateBot does not enforce the security group's password policy on `challenge`: a 6-character password is not refused 422 ValidationError with reasons[0].field='passwordPolicy'

> _Note: this issue was identified against the code base at commit `55987962084103a763901970f01bf3b64be75e84`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBot

## Description
Cause 1 of 1: CreateBot has no password-policy validation of the `challenge` member against the bot's target security group, so the sub-minimum-length rejection AWS performs before any bot is created never happens. Failing check: wickr.create-bot-weak-challenge-rejected-by-password-policy.

Related already-filed work item: the separate scaffold-stub issue naming `CreateBot` / services/service-com-amazonaws-wickr-wickradminapi/src/ops/create_bot.rs covers only 'the operation is a 501 stub'. It does NOT state the password-policy contract below, which is a distinct requirement this check asserts and which will still fail once CreateBot merely returns a success envelope. Fixing the stub alone does not close this issue.

AWS: Smithy (com-amazonaws-wickr-wickradminapi) models CreateBot with required `challenge` (SensitiveString, pattern ^[\S\s]*$ — i.e. the model imposes NO length or complexity constraint, so the rule is enforced in the handler, not by constraint validation) and `groupId` (required), and declares `ValidationError` (HTTP 422) among its errors. ValidationError carries `message` (GenericString) and `reasons` (ErrorDetailList of ErrorDetail{field, reason}). Per AWS Wickr network security groups (https://docs.aws.amazon.com/wickr/latest/adminguide/network-security-groups.html) a security group carries a password policy — minimum length plus required counts of uppercase, lowercase, numeric and symbol characters — and the default group's minimum is 8 characters. Recorded behavior for a challenge that violates that policy: HTTP 422, Error.Code = "ValidationError", top-level Error.Message = "" (EMPTY — the diagnosis lives in reasons[], not in the top-level message), and exactly ONE entry in `reasons`, with `field` = "passwordPolicy" and `reason` = the policy-summary text:

  "password does not meet minimum requirements: \n      {N} uppercase letter(s), \n      {N} lowercase letter(s), \n      {N} number(s), \n      {N} symbol(s), \n      minimum {N} letters\n      "

Fixed text of that message: the leading sentence "password does not meet minimum requirements: ", the five requirement lines in that order with their literal labels "uppercase letter(s),", "lowercase letter(s),", "number(s),", "symbol(s)," and "minimum ... letters", and the exact newline/indentation layout (each line prefixed by a newline and six spaces, with a trailing newline and six spaces at the end). Variable parts: the five counts, which are read from the password policy of the security group named by `groupId` — they are NOT derived from the submitted password and are NOT constants of the message. (For the default STANDARD-network security group the recorded values are 0/0/0/0 and a minimum of 8; a run against a group with a stricter policy must report that group's numbers.) The rejection is independent of the username-format check: a username that passes format validation and a challenge that violates the policy yields this single passwordPolicy reason only.

Also part of the contract: the refusal must be atomic — after it, ListBots for the network returns an empty `bots` list, no bot having been created — and a subsequent CreateBot with a policy-satisfying challenge (e.g. >= the group minimum) must succeed and echo back `username` equal to the requested username.

Emulator: every CreateBot call is refused with HTTP 501, Error.Code = "NotImplemented", message "operation CreateBot is scaffolded but not implemented in this emulator". The replay asserts on the error code and reports: AssertionError: expected ValidationError, got 'NotImplemented'. services/service-com-amazonaws-wickr-wickradminapi/src/ops/create_bot.rs still carries the `//! SCAFFOLDED, NOT IMPLEMENTED` marker and its `handle` body is `Err(crate::scaffolded::Scaffolded::not_implemented("CreateBot"))`, so no password-policy check, no ValidationError envelope and no reasons[] detail exist anywhere on this path.

Why: a client that provisions Wickr bots cannot distinguish a rejected weak bot password from an unsupported operation. Code that branches on Error.Code == "ValidationError" and reads reasons[0].field to decide which input to re-prompt for (password vs username vs group) sees 501 NotImplemented, which is not a modeled CreateBot error at all, so retry/validation logic treats a user-correctable input error as a service outage. Conversely, once CreateBot is implemented without the policy check, a bot gets created with a password AWS would never accept, and the caller's own policy-compliance assumptions about every bot in the network silently break.

Fix: CreateBot's handler and its validation step — services/service-com-amazonaws-wickr-wickradminapi/src/ops/create_bot.rs. Resolve the security group named by `groupId` from the network's state, compare `challenge` against that group's password policy (minimum length and the four character-class counts) BEFORE any state mutation, and on violation return the modeled ValidationError (HTTP 422) with an empty top-level `message` and exactly one ErrorDetail whose `field` is "passwordPolicy" and whose `reason` is rendered from the group's policy values in the layout above; leave the network's bot set untouched.

Traces: wickr.create-bot-weak-challenge-rejected-by-password-policy (the weak challenge "short1" against the default security group of a freshly created STANDARD network).

## Evidence
Check: wickr.create-bot-weak-challenge-rejected-by-password-policy (verdict: mismatch)

Expected (AWS / recorded):
  HTTP 422
  Error.Code    = "ValidationError"
  Error.Message = ""            (empty; detail is in reasons[])
  reasons       = [ { "field": "passwordPolicy",
                      "reason": "password does not meet minimum requirements: \n      0 uppercase letter(s), \n      0 lowercase letter(s), \n      0 number(s), \n      0 symbol(s), \n      minimum 8 letters\n      " } ]
  ListBots(networkId) -> bots == []   (no bot created)
  then CreateBot with challenge "longenoughpassword1" -> 200, username echoed back
  (the five numbers above are the default security group's password-policy values, not literals of the message)

Actual (emulator):
  HTTP 501
  Error.Code    = "NotImplemented"
  Error.Message = "operation CreateBot is scaffolded but not implemented in this emulator"

Replay output:
  File "check_wickr_create_bot_weak_challenge_rejected_by_password_policy.py", line 74, in run
    assert error["Code"] == "ValidationError", f"expected ValidationError, got {error['Code']!r}"
  AssertionError: expected ValidationError, got 'NotImplemented'

Source corroboration: services/service-com-amazonaws-wickr-wickradminapi/src/ops/create_bot.rs lines 4-6 (`SCAFFOLDED, NOT IMPLEMENTED` marker) and line 196 (`Err(crate::scaffolded::Scaffolded::not_implemented("CreateBot"))`) — no password-policy validation exists on this path.

Model corroboration: CreateBot errors include ValidationError (httpError 422); ValidationError members are `message` and `reasons` (ErrorDetailList -> ErrorDetail{field, reason}); `challenge` is required with pattern ^[\S\s]*$ only, so the policy is a handler-side rule.

## Steps to Reproduce
1. Boot the emulator and create a wickr client for region us-east-1.
2. network = CreateNetwork(networkName=<unique, <=20 chars>, accessLevel="STANDARD"); note networkId.
3. groupId = ListSecurityGroups(networkId=networkId)["securityGroups"][0]["id"]  (the default group, minimum password length 8).
4. Call CreateBot(networkId=networkId, username=<unique>-bot, displayName="Weak", groupId=groupId, challenge="short1")  — 6 characters, below the group minimum.
   Expected: HTTP 422, Error.Code "ValidationError", empty Error.Message, exactly one reasons[] entry with field "passwordPolicy" and the policy-summary reason text rendered from the group's policy values.
   Actual:   HTTP 501, Error.Code "NotImplemented".
5. Call ListBots(networkId=networkId) — expected bots == [] (the refusal created nothing).
6. Call CreateBot again with challenge="longenoughpassword1" — expected 200 with username echoed back, confirming the refusal in step 4 was the policy check and not a blanket failure.
7. Cleanup: DeleteNetwork(networkId=networkId).
