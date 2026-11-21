Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: iot-managed-integrations.account-association-lifecycle-and-refresh-conflict
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations.account-association-lifecycle-and-refresh-conflict|_error#25|AssertionError|raise AssertionError(f"{what}: expected a ClientError, call succeeded with {result!r}")
Issue-Commit: e2784ee3a9bae36394452385b38ba2b94553374b

# iot-managed-integrations: CreateAccountAssociation mints a second association for a connector destination that already has an outstanding one instead of refusing with ConflictException

> _Note: this issue was identified against the code base at commit `e2784ee3a9bae36394452385b38ba2b94553374b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAccountAssociation

## Description
This bundle has 1 cause; it must be fixed before the issue is closed.

Cause 1 of 1: CreateAccountAssociation performs no conflict check against the connector destination it is given, so a SECOND create against a destination that already holds an association in a non-terminal state (ASSOCIATION_IN_PROGRESS) succeeds and mints an additional association.

AWS: Smithy model services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/smithy-model/com-amazonaws-iotmanagedintegrations-iotmanagedintegrations.json — CreateAccountAssociation, POST /account-associations, success code 201; input ConnectorDestinationId is @required, @length(1..64), @pattern ^[A-Za-z0-9-_]+$; modelled errors include ConflictException (httpError 409, documented "There is a conflict with the request") alongside ValidationException (400) and ResourceNotFoundException (404). The model declares the error but states no cross-request rule, so for the rule itself: recorded behavior only — AWS answers a second CreateAccountAssociation naming a ConnectorDestinationId whose first association is still outstanding with HTTP 409, error code ConflictException and Message 'Account Association conflicts with existing record.' That whole sentence is FIXED text for this error class: it names the resource class only and interpolates NO value from the request or from state (no destination id, no association id). The recording also establishes what is NOT a conflict: StartAccountAssociationRefresh on the still-outstanding association succeeds and reissues the OAuth authorization URL, leaving the association's AssociationState at ASSOCIATION_IN_PROGRESS — so the conflict is specific to a duplicate CREATE, and the refusal is effect-free (the original association is unchanged and no second record exists).

Emulator: HTTP 201 with a brand-new association — body carries a new AccountAssociationId, AssociationState 'ASSOCIATION_IN_PROGRESS', an account-association Arn built from that new id, and a fresh OAuthAuthorizationUrl — where AWS returns 409 ConflictException. The handler (src/ops/create_account_association.rs) consults state only for (a) ClientToken idempotency replay, (b) existence of the ConnectorDestinationId, and (c) the MAX_ACCOUNT_ASSOCIATIONS bound; nothing examines whether that destination already has an outstanding association, so the create falls straight through to minting a record. The duplicate call in the recording carries no ClientToken, so the idempotency path is not involved — this is a missing uniqueness rule, not a token problem.

Why: a client wiring an end-user OAuth association treats 409 ConflictException as "the association you want already exists, go finish or delete it" and 201 as "a new authorization flow started". Against the emulator it gets the opposite verdict: it receives a second association id and a second authorization URL for one destination, so the end user can be sent through a flow that completes against an association the client has already discarded, and client-side de-duplication keyed on the conflict never fires. The emulator's state is also left holding two live associations for one destination, a shape AWS never produces, which then changes what ListAccountAssociations reports and which association Delete must clean up.

Fix: CreateAccountAssociation's handler/validation in services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/create_account_association.rs — after the ClientToken replay and the ConnectorDestinationId resolution, and BEFORE any id is allocated or anything is written to crate::state's account_associations, reject the create with ConflictException carrying AWS's fixed text when the signing region's store already holds an account association for that ConnectorDestinationId in a non-terminal state; the refusal must store nothing (the pre-existing association keeps its id and ASSOCIATION_IN_PROGRESS state). The matching fixed-text constant belongs next to this service's other recorded refusal sentences in crate::state.

Related but NOT covering this: open issue 20261029T193935Z-iot-managed-integrations-createaccountassociation-skips-connector-destination-resolution.md is grounded in the same check, but its Cause 1 is the earlier deleted-destination ValidationException — and that cause explicitly records that the duplicate-create ConflictException assertion "was never reached in this run, so [it is] not claimed as [a] divergence here". The deleted-destination check is now present in the handler and the check now advances past it, exposing this cause for the first time.

Traces: iot-managed-integrations.account-association-lifecycle-and-refresh-conflict (assertion labelled 'duplicate-create-while-outstanding').

## Evidence
Suite simparity-behavioral-aws-v1: 14/22 passed; iot-managed-integrations 14/22 (64%), 8 failed. Of the failures, two are marked ALREADY FILED (blocked by unimplemented ListOtaTasks and CreateProvisioningProfile) and four are marked OWNED by open issues; this is the one surviving failure.

[mismatch] iot-managed-integrations.account-association-lifecycle-and-refresh-conflict, at the assertion labelled 'duplicate-create-while-outstanding':

  AssertionError: duplicate-create-while-outstanding: expected a ClientError, call succeeded with
  {'ResponseMetadata': {'HTTPStatusCode': 201, ...},
   'OAuthAuthorizationUrl': 'https://example.com/oauth2/authorize?response_type=code&state=<generated id>',
   'AccountAssociationId': '<generated id>',
   'AssociationState': 'ASSOCIATION_IN_PROGRESS',
   'Arn': 'arn:aws:iotmanagedintegrations:<region>:<account>:account-association/<generated id>'}

  Expected: HTTP 409, Error Code 'ConflictException', Message 'Account Association conflicts with existing record.'
  Actual:   HTTP 201 and a newly created second account association.

  (The generated id, region, account number and URL above are per-run values from one recording and are not part of the requirement; the requirement is the status, the code and the fixed message sentence.)

Positive controls in the SAME check that PASSED, establishing the state the duplicate create was issued against:
  - CreateCloudConnector + CreateConnectorDestination succeeded; GetConnectorDestination on a deleted destination answered 404 ResourceNotFoundException / 'Connector Destination not found'.
  - CreateAccountAssociation on the deleted destination was correctly refused ValidationException / 'Connector Destination in the request does not exist.' (the previously filed Cause 1 of 20261029T193935Z-... no longer reproduces).
  - The first CreateAccountAssociation on the live destination returned AssociationState 'ASSOCIATION_IN_PROGRESS'; GetAccountAssociation confirmed it.
  - StartAccountAssociationRefresh on that outstanding association SUCCEEDED and the follow-up GetAccountAssociation still reported ASSOCIATION_IN_PROGRESS — so a refresh is correctly not treated as a conflict, and the association existed and was outstanding at the moment of the duplicate create.

Code evidence: services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/create_account_association.rs lines 200-252 — the only state consultations before minting are the ClientToken replay (lines 203-213), the ConnectorDestinationId existence check (lines 224-236) and the MAX_ACCOUNT_ASSOCIATIONS bound (lines 241-252). ConflictException is a declared variant of CreateAccountAssociationErrorModeled (line 122) but is never constructed in this file.

## Steps to Reproduce
Boot the emulator and replay iot-managed-integrations.account-association-lifecycle-and-refresh-conflict, or by hand against the iot-managed-integrations endpoint in a supported region:

1. CreateCloudConnector (with a backing Lambda endpoint), then CreateConnectorDestination against that connector id, and note the returned destination id D.
2. POST /account-associations with ConnectorDestinationId=D and NO ClientToken. It returns 201 with AccountAssociationId A and AssociationState 'ASSOCIATION_IN_PROGRESS'. Confirm with GetAccountAssociation(AccountAssociationId=A).
3. Optionally StartAccountAssociationRefresh(AccountAssociationId=A) — this must succeed and leave A in ASSOCIATION_IN_PROGRESS (it does today).
4. POST /account-associations AGAIN with ConnectorDestinationId=D and no ClientToken, while A is still ASSOCIATION_IN_PROGRESS.
   Observed: HTTP 201 and a second, different AccountAssociationId with its own Arn and OAuthAuthorizationUrl.
   Expected: HTTP 409, Error Code 'ConflictException', Message exactly 'Account Association conflicts with existing record.'
5. Assert the refusal is effect-free: GetAccountAssociation(AccountAssociationId=A) still reports AccountAssociationId A and AssociationState 'ASSOCIATION_IN_PROGRESS', and ListAccountAssociations reports exactly one association for destination D.
6. DeleteAccountAssociation(A); a subsequent create on D must then succeed again (the conflict must be scoped to an outstanding association, not permanent for the destination).
