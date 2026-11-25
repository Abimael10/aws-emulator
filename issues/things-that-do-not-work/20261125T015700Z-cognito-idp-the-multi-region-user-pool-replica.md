Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cognito-idp.user-pool-replica-lifecycle cognito-idp.user-pool-replica-update-lifecycle
Issue-Shape-Id: com.amazonaws.cognitoidentityprovider#AWSCognitoIdentityProviderService
Issue-Key: cognito-idp.user-pool-replica-lifecycle|run#16|AssertionError|assert status == "INACTIVE", (
Issue-Key: cognito-idp.user-pool-replica-update-lifecycle|_poll_until#19|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s, last value={value!r}")
Issue-Commit: 7a03c6c633814b42753ce8c8f03d4e7bdff82ea0

# cognito-idp: the multi-Region user-pool replica lifecycle is still not observable — a created replica never reads back INACTIVE and a deleted one stays DELETING in ListUserPoolReplicas

> _Note: this issue was identified against the code base at commit `7a03c6c633814b42753ce8c8f03d4e7bdff82ea0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListUserPoolReplicas

## Description
Cause 1 of 1: the replica state machine behind CreateUserPoolReplica / UpdateUserPoolReplica / DeleteUserPoolReplica cannot be driven to a terminal, readable state through ListUserPoolReplicas: after a create the replica's Status is not readable as INACTIVE, and after a delete it is still reported DELETING and never leaves the listing.

AWS: Smithy — CreateUserPoolReplica{UserPoolId, RegionName} -> UserPoolReplica (UserPoolReplicaType{RegionName, Status, Role, UserPoolArn}); Status is ReplicaStatusType with values CREATING | ACTIVE | INACTIVE | DELETING, documented as "CREATING: the replica is being created / INACTIVE: the replica has been created, but is not accepting requests for end-users ... / ACTIVE: available for both end-user and administrator operations / DELETING: the replica is being deleted". UpdateUserPoolReplica takes a required Status of UpdateReplicaStatusType (ACTIVE | INACTIVE) only, so INACTIVE is the state a freshly created replica settles in and ACTIVE is reached only by asking for it. DeleteUserPoolReplica removes the replica, after which ListUserPoolReplicas no longer lists that Region and UpdateUserPoolReplica on it answers ResourceNotFoundException (modelled, httpError 404) with the message "Replica not found in the requested region" — entirely fixed text. Recorded behavior only for the transition durations: on AWS a create takes minutes and a delete can stay DELETING for minutes, which is why the checks poll rather than read once.

Emulator: in user-pool-replica-lifecycle the create is accepted and ListUserPoolReplicas lists the Region once, but the status read where INACTIVE is due yields no status at all (None — the Region is absent from the listing): `AssertionError: expected a freshly created replica to settle INACTIVE, got None`. In user-pool-replica-update-lifecycle the CREATING->INACTIVE->ACTIVE->INACTIVE legs are served, but after DeleteUserPoolReplica the replica is still reported DELETING and is never removed from the listing: `AssertionError: condition not met within 0.0s, last value='DELETING'`. In both runs the lifecycle polls were reached with the check's wall-clock budget already consumed by the legs before them (the poll was granted 0.0 s), so no leg of the lifecycle after the create is observable end to end. Hypothesis: the time is spent in the prerequisite legs both checks serve before the lifecycle (KMS CreateKey(MultiRegion=true), ReplicateKey into the second Region, DescribeKey polled to KeyState Enabled, CreateUserPool with KeyConfiguration, CreateUserPoolReplica) and/or in a CREATING span long enough to absorb the 800 s first poll; every individual response observed in those legs was well formed, so the divergence is that the lifecycle never reaches a readable terminal state within a client-sized poll, not a wrong field value.

Why: a client cannot observe a replica settling INACTIVE after CreateUserPoolReplica, cannot confirm DeleteUserPoolReplica actually removed it, and cannot reach the gone-replica refusal (ResourceNotFoundException "Replica not found in the requested region"). Multi-Region user-pool replication therefore cannot be exercised end to end even though each individual call answers 200 — code that waits on the documented statuses waits forever. This is the same divergence 20261103T200951Z-cognito-idp-2-net-new-causes-getprovisionedlimit-refuses.md (status: fixed) recorded as its Cause 2, carrying both of these check keys; its fix did not change the observed behavior.

Fix: the replica state machine and its listing in the cognito-idp crate — the replica records and their activation/deletion spans in the service state, read by the ListUserPoolReplicas handler and written by the CreateUserPoolReplica / UpdateUserPoolReplica / DeleteUserPoolReplica handlers — so that a created replica becomes readable as INACTIVE, a deleted one leaves the listing, and the latency of the multi-Region-key prerequisite those handlers depend on does not consume the whole budget.

Traces (check ids): cognito-idp.user-pool-replica-lifecycle, cognito-idp.user-pool-replica-update-lifecycle.

## Evidence
cognito-idp.user-pool-replica-lifecycle:
```
File "check_cognito_idp_user_pool_replica_lifecycle.py", line 78, in run
  assert status == "INACTIVE", (
AssertionError: expected a freshly created replica to settle INACTIVE, got None
```
(raised after create_user_pool_replica was accepted and ListUserPoolReplicas had listed the replica Region once; the status poll was granted 0.0 s of the check's remaining budget.)
cognito-idp.user-pool-replica-update-lifecycle:
```
File "check_cognito_idp_user_pool_replica_update_lifecycle.py", line 100, in run
  gone = _poll_until(
File "check_cognito_idp_user_pool_replica_update_lifecycle.py", line 154, in _poll_until
  raise AssertionError(f"condition not met within {timeout_s}s, last value={value!r}")
AssertionError: condition not met within 0.0s, last value='DELETING'
```
Expected (AWS): ListUserPoolReplicas reports Status 'INACTIVE' for a freshly created replica, 'ACTIVE' after UpdateUserPoolReplica(Status='ACTIVE'), 'INACTIVE' after UpdateUserPoolReplica(Status='INACTIVE'), and NO entry for that Region after DeleteUserPoolReplica, with UpdateUserPoolReplica on the removed replica answering HTTP 404 ResourceNotFoundException, message 'Replica not found in the requested region' (fixed text, no variable part).
Actual (emulator): no status at all for the Region on the read after the create, and a replica still reported 'DELETING' that never leaves the listing; the gone-replica ResourceNotFoundException is never reached. (Region names, pool ids and the KMS key arn are per-run values.)

## Steps to Reproduce
1. KMS: CreateKey(MultiRegion=true) with a policy granting cognito-idp.amazonaws.com and identitystore.amazonaws.com the replication actions; ReplicateKey into a second Region; poll DescribeKey in that Region until KeyState='Enabled'.
2. CreateUserPool(UserPoolTier='PLUS', KeyConfiguration={'KeyType':'CUSTOMER_MANAGED_KEY','KmsKeyArn':<key arn>}).
3. CreateUserPoolReplica(UserPoolId=<pool>, RegionName=<second Region>), then poll ListUserPoolReplicas: the Region must stay listed and settle Status='INACTIVE'. Observed: the Region is absent on the next read (no status at all).
4. UpdateUserPoolReplica(Status='ACTIVE'), then ('INACTIVE'), confirming each on an independent ListUserPoolReplicas read.
5. DeleteUserPoolReplica, then poll ListUserPoolReplicas until the Region is gone. Observed: Status stays 'DELETING'.
6. UpdateUserPoolReplica on the removed replica. Expected: 404 ResourceNotFoundException 'Replica not found in the requested region'.
Time each leg: the lifecycle must be observable without the prerequisite legs or an over-long CREATING/DELETING span consuming the whole run budget.
