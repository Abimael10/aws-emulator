Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: partnercentral-account.list-connection-invitations-invalid-status-filter-rejected
Issue-Shape-Id: com.amazonaws.partnercentralaccount#PartnerCentralAccount
Issue-Key: partnercentral-account.list-connection-invitations-invalid-status-filter-rejected|run#5|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Commit: d85a460d74d2b644ac8854d21d0c700c1abb7f50

# ListConnectionInvitations enum ValidationException message includes the rejected value, unlike AWS

> _Note: this issue was identified against the code base at commit `d85a460d74d2b644ac8854d21d0c700c1abb7f50`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListConnectionInvitations

## Description
Cause 1 of 1: the constraint-violation message the emulator emits for an out-of-enum `Status` filter on ListConnectionInvitations carries an extra `Value '<input>'` clause that AWS does not emit for this member.

AWS: Smithy: ListConnectionInvitations.Status targets the enum shape `InvitationStatus` (values PENDING, ACCEPTED, REJECTED, CANCELED, EXPIRED); ListConnectionInvitations declares `ValidationException` (client, httpError 400). Recorded behavior: a Status outside that enum is refused with HTTP 400, error code `ValidationException`, and message "1 validation error detected: Value at 'status' failed to satisfy constraint: Member must satisfy enum value set: [EXPIRED, ACCEPTED, PENDING, CANCELED, REJECTED]". Fixed text of the template: "1 validation error detected: Value at '", "' failed to satisfy constraint: Member must satisfy enum value set: [", "]". Variable parts taken from the request/model: the member path (here the lowercased member name `status`) and the enum value set rendered in the order AWS lists it for this member. AWS does NOT interpolate the offending input value for this member — the clause is "Value at 'status'", not "Value '<input>' at 'status'".

Emulator: returns ValidationException/400 with the extra value clause interpolated: "1 validation error detected: Value 'NOT_A_REAL_STATUS' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [EXPIRED, ACCEPTED, PENDING, CANCELED, REJECTED]". Status code, error code and the enum value set all match; only the message's fixed text diverges, by an inserted request-echo.

Why: a client (or a test, log scraper, or support tooling) that matches on AWS's exact validation message for this member never matches the emulator's, so enum-rejection handling that is correct against AWS reports an unrecognized validation failure against the emulator. It also leaks the caller's raw filter value into a message AWS keeps value-free for this member, so redaction behavior differs.

Fix: the ValidationException message formatter used by the enum/constraint validation path for ListConnectionInvitations' input members (the operation's input validation in the partnercentral-account crate) must render the member-path form without the offending-value clause for this member, rather than the generic "Value '<v>' at '<path>'" form.

Traces: partnercentral-account.list-connection-invitations-invalid-status-filter-rejected (the check's positive control, ListConnectionInvitations with Status='PENDING', is not implicated — the run stopped at the message assertion).

## Evidence
Check: partnercentral-account.list-connection-invitations-invalid-status-filter-rejected (verdict: mismatch)

Call: ListConnectionInvitations(Catalog='AWS', Status='NOT_A_REAL_STATUS') against us-east-1.

Expected (AWS):
  HTTP 400, Error.Code = 'ValidationException'
  Error.Message = "1 validation error detected: Value at 'status' failed to satisfy constraint: Member must satisfy enum value set: [EXPIRED, ACCEPTED, PENDING, CANCELED, REJECTED]"

Actual (emulator):
  HTTP 400, Error.Code = 'ValidationException'  (correct)
  Error.Message = "1 validation error detected: Value 'NOT_A_REAL_STATUS' at 'status' failed to satisfy constraint: Member must satisfy enum value set: [EXPIRED, ACCEPTED, PENDING, CANCELED, REJECTED]"

AssertionError raised at check line 46:
  assert message == expected, f"expected message {expected!r}, got {message!r}"

Delta: the emulator inserts a "Value '<request value>' " clause before "at '<member path>'". The quoted token NOT_A_REAL_STATUS is the request's own value filling that clause; it is not part of the required template. Everything else (status, code, member path, enum value set and its ordering) agrees.

## Steps to Reproduce
1. Boot the emulator and create a partnercentral-account client pinned to region us-east-1.
2. Call ListConnectionInvitations with Catalog='AWS' and Status set to any value outside the InvitationStatus enum (e.g. 'NOT_A_REAL_STATUS').
3. Catch the ClientError and inspect response['Error']['Code'] and response['Error']['Message'].
4. Observe Code == 'ValidationException' (correct) but Message contains the extra "Value '<supplied value>' " clause, where AWS emits "Value at 'status' failed to satisfy constraint: ...".
5. Re-replay check partnercentral-account.list-connection-invitations-invalid-status-filter-rejected to confirm the fix; its second leg (Status='PENDING') must still return a well-formed ConnectionInvitationSummaries list.
