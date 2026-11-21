Issue-Source: trace-sweep-divergence
Issue-Trace: iot-managed-integrations-account-association-error-paths
Issue-Step: task#3
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: iot-managed-integrations-account-association-error-paths
Issue-Shape-Id: com.amazonaws.iotmanagedintegrations#IotManagedIntegrations
Issue-Key: iot-managed-integrations|DeleteAccountAssociation|2xx->4xx|$
Issue-Key: iot-managed-integrations|DeleteAccountAssociation|2xx->4xx|status
Issue-Commit: d9773982178bffc206ea9531196a8841a020d5e3

# DeleteAccountAssociation answers 404 ResourceNotFoundException for an unknown id instead of succeeding idempotently

> _Note: this issue was identified against the code base at commit `d9773982178bffc206ea9531196a8841a020d5e3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteAccountAssociation

## Description
Cause 1 of 1: DeleteAccountAssociation refuses a delete of an account-association id the store holds nothing under, where AWS completes the delete as a no-op.

AWS: DELETE /account-associations/{AccountAssociationId} is modelled @idempotent with a success status of 200 and an empty (Unit) output body (Smithy: DeleteAccountAssociation, http {method: DELETE, uri: /account-associations/{AccountAssociationId}, code: 200}, output shape Unit, trait @idempotent; input AccountAssociationId @httpLabel @required, @length 1..64, @pattern ^[0-9a-zA-Z]+$). The recorded wire response for a delete naming a syntactically valid but non-existent AccountAssociationId is HTTP 200 with the body {} — i.e. the removal is treated as already satisfied and no error is raised. Recorded behavior only for the specific choice of 200-over-404 on an absent id: the model declares ResourceNotFoundException for this operation but the recording shows AWS does not use it for a plain absent-id delete. Hypothesis: the service resolves the delete against the end state (nothing associated under that id) rather than against the resource's current existence, which is what @idempotent on a DELETE is for.

Emulator: the handler looks the id up per signing region, and when the lookup misses it returns HTTP 404 with the JSON body {"__type": "ResourceNotFoundException", "Message": "Account association not found.", "ResourceId": "<the id from the request URI label>", "ResourceType": "AccountAssociation"} (x-amzn-errortype: ResourceNotFoundException). Replay reports: status expected 200, got 404; body $ expected {}, got that ResourceNotFoundException document. Of that body only the error class (ResourceNotFoundException), the fixed sentence "Account association not found." and the fixed ResourceType "AccountAssociation" are template text; ResourceId is a value echoed from the request URI label (in the recorded run a 46-character all-zero/A id used by the fixture precisely because no such association exists) and is not itself part of any requirement.

Why: a client that deletes defensively — retrying a delete after a timeout, or tearing down a connector destination's associations without first reading them — gets a hard 404 from the emulator where AWS returns success. Retry-on-timeout loops surface a spurious ResourceNotFoundException, and teardown/cleanup code that AWS lets run to completion aborts partway. The second delete of the SAME association is affected identically: against AWS it is a 200, against the emulator it is a 404, so idempotent delete semantics cannot be exercised at all.

Fix: the DeleteAccountAssociation handler, in services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/delete_account_association.rs — the `if !removed { return Err(... ResourceNotFoundException ...) }` arm must not fire for an id that merely resolves to nothing; the handler should return the empty 200 output whether or not a record was present, keeping the existing state cleanup (account_associations / account_association_order / forget_idempotency_tokens_for) on the path where a record WAS found. Input-shape violations (length/pattern on AccountAssociationId) remain ValidationException and are unaffected. Nothing in crate::state needs to change; crate::state::account_association_not_found stays in use by the read paths (GetAccountAssociation).

Note: three other open iot-managed-integrations issues touch nearby 404 wording and response framing (UpdateEventLogConfiguration 404 body, the OTA-task refusal bundle, UpdateManagedThing 204 Content-Length). None of them covers DeleteAccountAssociation's absent-id outcome — this is a status-class divergence (404 where AWS returns 200), not a message-wording one.

Traces: iot-managed-integrations-account-association-error-paths#task#3.

## Evidence
Fixture iot-managed-integrations-account-association-error-paths, step task#3, operation DeleteAccountAssociation.

Expected (recorded AWS): HTTP 200, body {} (empty Unit output).
Actual (emulator): HTTP 404, header x-amzn-errortype: ResourceNotFoundException, body {"__type": "ResourceNotFoundException", "Message": "Account association not found.", "ResourceId": "0000000000000000000000000000000000000000AAAAAA", "ResourceType": "AccountAssociation"}.

Replay divergence detail, verbatim:
  status: expected 200, got 404
  $: value_mismatch (expected {}, got {'__type': 'ResourceNotFoundException', 'Message': 'Account association not found.', 'ResourceId': '0000000000000000000000000000000000000000AAAAAA', 'ResourceType': 'AccountAssociation'})
divergence_key: service=iot-managed-integrations|DeleteAccountAssociation|status=200->404|keys=$,status

Model evidence: Smithy DeleteAccountAssociation carries @idempotent, http code 200, output smithy.api#Unit.
Code evidence: services/service-com-amazonaws-iotmanagedintegrations-iotmanagedintegrations/src/ops/delete_account_association.rs lines 184-213 — existence is probed with account_association_in(&region, &input.account_association_id).is_some() and a miss returns ResourceNotFoundException.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus).
2. Fixture iot-managed-integrations-account-association-error-paths reaches step task#3: DELETE /account-associations/<an AccountAssociationId matching ^[0-9a-zA-Z]+$ that no association exists under in the signing region>, with no prior CreateAccountAssociation for that id.
3. Observe HTTP 404 with a ResourceNotFoundException body; the recorded AWS response for the same request is HTTP 200 with an empty body {}.
4. Equivalent by hand: CreateAccountAssociation against a connector destination, DELETE the returned AccountAssociationId (200), then DELETE the same id again — the emulator answers 404 where AWS answers 200.
