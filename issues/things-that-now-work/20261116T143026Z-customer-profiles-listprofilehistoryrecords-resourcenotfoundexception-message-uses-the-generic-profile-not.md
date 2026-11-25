Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: customer-profiles.profile-creation-generates-history-record customer-profiles.merge-profiles-deep-history-and-deletion
Issue-Shape-Id: com.amazonaws.customerprofiles#CustomerProfiles_20200815
Issue-Key: customer-profiles|ListProfileHistoryRecords|ResourceNotFoundException
Issue-Key: customer-profiles.merge-profiles-deep-history-and-deletion|run#19|AssertionError|assert history_message == f"Profile {merged_profile} not found in domain {domain}", (
Issue-Commit: cb1fe1cd01dea9b9b76399bb5958c78b62c26090

# ListProfileHistoryRecords ResourceNotFoundException message uses the generic profile-not-found wording instead of its own domain-qualified sentence

> _Note: this issue was identified against the code base at commit `cb1fe1cd01dea9b9b76399bb5958c78b62c26090`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListProfileHistoryRecords

## Description
Cause 1 of 1: ListProfileHistoryRecords rejects a ProfileId the domain does not hold with the right error code and status, but with the WRONG message text — it reuses the service's generic "profile addressed by id" sentence rather than the domain-qualified sentence AWS uses for this operation. Failing checks: customer-profiles.profile-creation-generates-history-record, customer-profiles.merge-profiles-deep-history-and-deletion.

AWS: Smithy (services/service-com-amazonaws-customerprofiles-customerprofiles-20200815/smithy-model/com-amazonaws-customerprofiles-customerprofiles-20200815.json) models ListProfileHistoryRecords POST /domains/{DomainName}/profiles/history-records with ResourceNotFoundException (HTTP 404, "The requested resource does not exist, or access was denied."); the model does not state the message text, so this is recorded behavior only. The recording shows ListProfileHistoryRecords answering 404 ResourceNotFoundException with the message template:
    Profile <ProfileId> not found in domain <DomainName>
Fixed text: "Profile ", " not found in domain ". Variable parts: <ProfileId> is the request's ProfileId member, <DomainName> is the request's DomainName member. (Both checks' assertions are built from the request values they sent, not from any recorded literal.) Note this wording is specific to ListProfileHistoryRecords: the same two recordings confirm UpdateProfile on an unheld id keeps the OTHER template, "Profile with profileId <ProfileId> is not found in the domain" — that assertion passes in the merge check, so the two operations genuinely word it differently and must not share one sentence.

Emulator: returns ResourceNotFoundException (404) with "Profile with profileId <ProfileId> is not found in the domain" — the generic sentence, missing the domain name entirely and phrasing the id as "with profileId ... is not found in the domain". Replay output: 'Profile with profileId 581dbc71fdb443909a0e5877fee37c80 is not found in the domain' for a never-created id, and 'Profile with profileId d1299f4bbc92420cbde0701f34aae0c6 is not found in the domain' for a merged-away id. The handler deliberately shares the generic sentence (its own comment says it is "Worded with the sentence this service reports an absent profile with wherever one is addressed by id"), which is the defect: this operation is the exception.

Why: a client that reads or matches the error message — log scrapers, operator-facing surfacing, tests and retry logic that distinguish "this profile is unknown in THIS domain" from the generic id-not-found sentence — gets different text than AWS produces, and loses the domain name that AWS includes. Any assertion or parser written against real AWS output fails against the emulator even though code and status match.

Fix: the ListProfileHistoryRecords handler's not-found path — it must build its own message rather than delegating to the shared profile-not-found sentence builder; the correct wording for the generic id case must stay unchanged for the operations that do use it (UpdateProfile and friends).

Traces: customer-profiles.profile-creation-generates-history-record (ListProfileHistoryRecords on a freshly generated uuid in a new domain), customer-profiles.merge-profiles-deep-history-and-deletion (ListProfileHistoryRecords on a profile merged away by MergeProfiles). Code citations: services/service-com-amazonaws-customerprofiles-customerprofiles-20200815/src/ops/list_profile_history_records.rs lines 200-202 (the not-found branch) and line 248-253 (not_found helper), which pass crate::state::profile_not_found(...) as the message.

## Evidence
Check customer-profiles.profile-creation-generates-history-record:
  assert message == f"Profile {fake_profile_id} not found in domain {domain}"
  AssertionError: unexpected message listing history for a never-created profile id: 'Profile with profileId 581dbc71fdb443909a0e5877fee37c80 is not found in the domain'

Check customer-profiles.merge-profiles-deep-history-and-deletion:
  assert history_message == f"Profile {merged_profile} not found in domain {domain}"
  AssertionError: unexpected ListProfileHistoryRecords message for merged-away d1299f4bbc92420cbde0701f34aae0c6: 'Profile with profileId d1299f4bbc92420cbde0701f34aae0c6 is not found in the domain'

Expected (AWS): HTTP 404, Code=ResourceNotFoundException, Message="Profile <ProfileId> not found in domain <DomainName>".
Actual (emulator): HTTP 404, Code=ResourceNotFoundException, Message="Profile with profileId <ProfileId> is not found in the domain".
Status and error code match; only the message diverges. In the same merge recording the UpdateProfile assertion on the SAME merged-away id expects and gets "Profile with profileId <ProfileId> is not found in the domain", so the divergence is confined to ListProfileHistoryRecords' own wording.

## Steps to Reproduce
1. CreateDomain(DomainName=<new domain>, DefaultExpirationDays=30).
2. Call ListProfileHistoryRecords(DomainName=<that domain>, ProfileId=<a 32-hex id never created in the domain>).
3. Observe the ClientError: Code is ResourceNotFoundException and status 404 (correct), but Message is "Profile with profileId <id> is not found in the domain" instead of "Profile <id> not found in domain <that domain>".
4. Equivalently: CreateProfile twice, MergeProfiles the second into the first, then ListProfileHistoryRecords(DomainName=<domain>, ProfileId=<merged-away id>) — same wrong message. In the same flow UpdateProfile on the merged-away id returns the generic sentence, which IS correct, showing the two messages must differ.
