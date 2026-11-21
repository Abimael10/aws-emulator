Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: invoicing.batch-get-invoice-profile-unlinked-account-rejected
Issue-Shape-Id: com.amazonaws.invoicing#Invoicing
Issue-Key: invoicing|BatchGetInvoiceProfile|AccessDeniedException
Issue-Commit: 8470bd15600894c04d6297f04472e87ed5582e16

# BatchGetInvoiceProfile refuses the caller's own account with 403 AccessDeniedException instead of returning its invoice profile

> _Note: this issue was identified against the code base at commit `8470bd15600894c04d6297f04472e87ed5582e16`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
BatchGetInvoiceProfile

## Description
REOPENS 20261108T050356Z-invoicing-batchgetinvoiceprofile-answers-the-requester-s-own-account-instead.md: its fix left the call refused.

Cause 1 of 1: `BatchGetInvoiceProfile` refuses a request that names the caller's OWN account id with 403 `AccessDeniedException`, so the caller cannot read its own invoice profile at all unless some stored invoice-unit configuration happens to name that account as a linked account. Failing check: invoicing.batch-get-invoice-profile-unlinked-account-rejected.

AWS: the model documents `BatchGetInvoiceProfile` as "This gets the invoice profile associated with a set of accounts. The accounts must be linked accounts under the requester management account organization", `AccountIds` (AccountIdList, len 1..1000) required, output `Profiles` (ProfileList), errors `AccessDeniedException` (HTTP 403), `InternalServerException` (500), `ResourceNotFoundException` (404), `ThrottlingException` (429), `ValidationException` (400) (smithy_query operation=BatchGetInvoiceProfile, model services/service-com-amazonaws-invoicing-invoicing/smithy-model/com-amazonaws-invoicing-invoicing.json). The model does not say the caller's own account is excluded from the set it may be answered for; recorded behavior only: the behavioral recording shows a request naming ONLY the account `sts:GetCallerIdentity` reports answered 200 with a `Profiles` entry whose `AccountId` is that same account, and a request that adds a well-formed 12-digit account id outside the caller's organization refused 403 `AccessDeniedException` with message "Account {callerAccountId} is not authorized to access resources for account {outsideAccountId}" — fixed text "Account " / " is not authorized to access resources for account ", with the first variable part the account the caller authenticates as and the second the offending account id taken from the request's `AccountIds`. The caller's own account is readable by the caller; only an account the caller's organization does not contain is refused.

Emulator: the own-account request is refused. Replaying the check, the very first call `batch_get_invoice_profile(AccountIds=[<callerAccountId>])` raises `AccessDeniedException` — in this run "Account 000000000000 is not authorized to access resources for account 000000000000" (both ids are the caller's own; the literal id is a per-environment value, not part of the requirement). The readable-account set the handler consults is derived from stored invoice-unit configurations ONLY, with the caller's identity admitting nothing, so with no invoice unit linking the caller the caller's own id is treated as an outsider and the whole request is refused with 403 and zero profiles. The positive half of the behavior (the caller reading itself) is therefore unreachable, and the negative half can never be observed either, because the refusal fires on the caller's own id before any genuinely outside id in the same `AccountIds` list is examined.

Why: a client relying on AWS's behavior cannot read its own invoice profile. Any code that resolves the current account with STS and then asks for its invoice profile — the ordinary single-account use of this operation — gets a hard 403 where AWS returns a profile, so it is indistinguishable from a real permission failure and the client has no path to the data. It also corrupts the denial signal in the other direction: a request mixing the caller's own account with an account outside the organization is refused naming the CALLER's account rather than the offending one, so a client that parses the message to learn which account it may not read is pointed at itself.

Fix: the operation's authorization decision — the readable-account set in the service state that `BatchGetInvoiceProfile`'s handler consults, and the handler's refusal branch. The caller's own identity must be readable by the caller unconditionally (in addition to the accounts stored configurations link), while every account id the organization does not contain must still refuse the request WHOLE with 403 and the message template above, naming the first such account in request order. The refusal wording and its template are already correct; only the membership rule that decides WHEN it is reached is wrong.

Traces: invoicing.batch-get-invoice-profile-unlinked-account-rejected. Handler and state involved: services/service-com-amazonaws-invoicing-invoicing/src/ops/batch_get_invoice_profile.rs:182-199 (refusal branch over `store.linked_accounts()`); services/service-com-amazonaws-invoicing-invoicing/src/state.rs:587-609 (`linked_accounts`, whose doc states the requester's identity admits no account by itself).

## Evidence
Check `invoicing.batch-get-invoice-profile-unlinked-account-rejected`, verdict error:

  botocore.errorfactory.AccessDeniedException: An error occurred (AccessDeniedException) when calling the BatchGetInvoiceProfile operation: Account 000000000000 is not authorized to access resources for account 000000000000

This is raised by the check's FIRST call, `invoicing.batch_get_invoice_profile(AccountIds=[<callerAccountId>])`, where `<callerAccountId>` comes from `sts.get_caller_identity()["Account"]`.

Expected (AWS, per the recording): HTTP 200 with `Profiles` containing an entry whose `AccountId` equals the caller's account id. Then, for `AccountIds=[<callerAccountId>, "111111111111"]`: HTTP 403, error code `AccessDeniedException`, message "Account {callerAccountId} is not authorized to access resources for account 111111111111" — i.e. the second variable part is the id from the request that the caller may not read.

Actual (emulator): HTTP 403 `AccessDeniedException` on the own-account-only request, message "Account {callerAccountId} is not authorized to access resources for account {callerAccountId}", no `Profiles` at all.

Note the direction of drift: fixed issue 20261108T050356Z-invoicing-batchgetinvoiceprofile-answers-the-requester-s-own-account-instead.md removed the unconditional own-account admission from the readable set; that change is what now refuses the caller's own account. Both halves must hold at once: the caller reads itself, and an account outside the organization is refused by name.

## Steps to Reproduce
1. Boot the emulator with no invoice unit stored that names the caller's account as an `InvoiceReceiver` or in an invoice-unit rule's `LinkedAccounts`.
2. Resolve the caller's account id: `sts get-caller-identity` -> `Account`.
3. Call `BatchGetInvoiceProfile` with `AccountIds=[<that account id>]`.
4. Observe HTTP 403 `AccessDeniedException`, message "Account <id> is not authorized to access resources for account <id>". AWS answers 200 with a `Profiles` entry for that account.
5. Call `BatchGetInvoiceProfile` with `AccountIds=[<that account id>, "111111111111"]`. AWS refuses 403 with message "Account <callerId> is not authorized to access resources for account 111111111111"; the emulator refuses naming the caller's own id instead of the outside one.
