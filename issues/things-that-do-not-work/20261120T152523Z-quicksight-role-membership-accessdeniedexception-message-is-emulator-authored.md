Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: quicksight.role-membership-group-lifecycle
Issue-Shape-Id: com.amazonaws.quicksight#QuickSight_20180401
Issue-Key: quicksight.role-membership-group-lifecycle|run#11|AssertionError|assert list_err["Message"] == _IDENTITY_STORE_DENIAL, (
Issue-Commit: 3a9f51772abd0c25cb9b53fc574d47447671aafb

# QuickSight role-membership AccessDeniedException message is emulator-authored instead of AWS's identity-store denial text

> _Note: this issue was identified against the code base at commit `3a9f51772abd0c25cb9b53fc574d47447671aafb`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListRoleMemberships

## Description
One net-new root cause in the QuickSight emulator. The emulator raises the right error shape (AccessDeniedException) on the role-membership operations of an account whose identity store it does not manage, but it writes its OWN denial sentence and appends request-derived context AWS never includes, so the text a client reads is wording AWS does not emit. For this error class the message is part of the contract. This bundle has 1 cause; it must be closed before submitting.

Related already-open issue 20261111T073736Z-quicksight-resourcenotfoundexception-messages-for-namespaces-and-themes-are.md describes the same fault CLASS (emulator-authored message text) but on ResourceNotFoundException for namespaces and themes, and does NOT cover the role-membership AccessDeniedException text; neither does 20261112T051305Z-quicksight-error-payloads-carry-a-synthetic-requestid-member.md (that one is about an extra RequestId member, not the message). So this is filed separately with its own behavioral check id.

Cause 1 of 1: ListRoleMemberships (and CreateRoleMembership) denial message on an account whose QuickSight identities are not QuickSight-managed.
AWS: ListRoleMemberships (Smithy: GET /accounts/{AwsAccountId}/namespaces/{Namespace}/roles/{Role}/members; modeled errors include AccessDeniedException, errorKind client, httpError 401) and CreateRoleMembership (POST /accounts/{AwsAccountId}/namespaces/{Namespace}/roles/{Role}/members/{MemberName}; same modeled AccessDeniedException, client, 401). On an account whose identities are not managed by QuickSight itself, both answer Code AccessDeniedException with the identity-store denial text: the message is ENTIRELY fixed text, "Operation is disabled for identities managed by QuickSight. " — one sentence, ending with a period followed by a single trailing space, with NO variable parts at all: no account id, no namespace, no role, nothing taken from the request. The model fixes the shape and the status but not the text, so the template itself is recorded behavior only.
Emulator: Code AccessDeniedException (correct shape) with the message "Operation is not supported for IAM_ONLY identity store.AWS account id: <AwsAccountId>, Namespace: <Namespace>, Role: <Role>. " — an emulator-authored sentence built from the template "Operation is not supported for {identity_store} identity store." concatenated (with no separating space) to "AWS account id: {AwsAccountId}, Namespace: {Namespace}, Role: {Role}. ", whose variable parts are the account's stored identity-store kind and the request's AwsAccountId, Namespace and Role path labels. None of that appears in AWS's message, and AWS's own fixed sentence appears nowhere in the emulator's.
Why: a client that surfaces, logs or pattern-matches QuickSight's role-membership denial — for example to distinguish "this account's identities are managed elsewhere, so role membership is unavailable" from a plain IAM permissions denial, which is exactly the distinction the recorded message carries — sees a sentence that never appears on AWS and finds none of the fragments it expects. It also leaks the account's internal identity-store kind and echoes request labels into an error AWS keeps context-free, so message-sensitive assertions and log scrapers diverge.
Fix: the shared denial-message builder, crate::state::role_membership_not_authorized_message, and its two callers in the role-membership handlers (the AccessDeniedException construction in ops/list_role_memberships.rs and in ops/create_role_membership.rs) — emit AWS's fixed sentence with no request-derived or state-derived context instead of an emulator-authored one. The behavioral check fails on ListRoleMemberships before it reaches its CreateRoleMembership assertion, which asserts the IDENTICAL message, so the fix must cover both call paths, not only the one the check reaches first.
Traces: quicksight.role-membership-group-lifecycle.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against the emulator. The error Code matched (AccessDeniedException); only the message text diverges.

quicksight.role-membership-group-lifecycle — subscribe the account, CreateGroup as a positive control, then ListRoleMemberships(AwsAccountId, Namespace="default", Role="READER").
  Expected (AWS): Error.Code = AccessDeniedException (modeled httpError 401), Error.Message = "Operation is disabled for identities managed by QuickSight. " — the whole string is fixed text, including the trailing period-and-space; there are NO variable parts.
  Actual (emulator): Error.Code = AccessDeniedException, Error.Message = 'Operation is not supported for IAM_ONLY identity store.AWS account id: <AwsAccountId>, Namespace: <Namespace>, Role: <Role>. ' — observed with the emulator's 12-digit account id, the namespace "default" and the role "READER" filling the template, and with the account's identity-store kind (IAM_ONLY) filling the first clause. Those values only fill the emulator's template; they are not part of any requirement.
  Check assertion: `assert list_err["Message"] == "Operation is disabled for identities managed by QuickSight. "` -> AssertionError ("expected the denial to name the identity-store restriction, not a bare permissions denial, got message: ..."). The check then asserts the same message for CreateRoleMembership(..., MemberName=<group>), which the run never reaches.

Code citations: services/service-com-amazonaws-quicksight-quicksight-20180401/src/ops/list_role_memberships.rs lines 186-199 build the refusal via crate::state::access_denied(crate::state::role_membership_not_authorized_message(&store.identity_store(..), &input.aws_account_id, &input.namespace, &input.role), ..) — the builder takes the account id, namespace and role precisely so it can interpolate them, which is the defect.

Smithy citations (services/service-com-amazonaws-quicksight-quicksight-20180401/smithy-model/com-amazonaws-quicksight-quicksight-20180401.json): ListRoleMemberships errors -> AccessDeniedException (client, httpError 401); CreateRoleMembership errors -> AccessDeniedException (client, httpError 401). Neither fixes the message text, so the expected sentence above is recorded behavior only.

## Steps to Reproduce
1. Subscribe the account (CreateAccountSubscription, edition ENTERPRISE, authentication method IAM_ONLY) if DescribeAccountSubscription does not already report ACCOUNT_CREATED/ENABLED.
2. CreateGroup(AwsAccountId, Namespace="default", GroupName=<fresh name>) as a positive control — group management must keep working.
3. Call ListRoleMemberships(AwsAccountId, Namespace="default", Role="READER") and read Error.Message: the emulator answers "Operation is not supported for IAM_ONLY identity store.AWS account id: <account>, Namespace: default, Role: READER. " where AWS answers exactly "Operation is disabled for identities managed by QuickSight. " with no request context.
4. Call CreateRoleMembership(AwsAccountId, Namespace="default", Role="READER", MemberName=<the group from step 2>) and read Error.Message: the same context-free AWS sentence is expected there, so verify this path too.
5. Confirm the denials had no effect: DescribeGroup on the group still succeeds, and the Code on both calls is AccessDeniedException (that part already matches).
