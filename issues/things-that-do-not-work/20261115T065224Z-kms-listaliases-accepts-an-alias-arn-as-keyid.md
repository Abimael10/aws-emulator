Issue-Source: trace-sweep-divergence
Issue-Trace: kms-rbx-traj-vault-key-lookup-forms
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kms-rbx-traj-vault-key-lookup-forms
Issue-Shape-Id: com.amazonaws.kms#TrentService
Issue-Key: kms|ListAliases|4xx->2xx|$.__type
Issue-Key: kms|ListAliases|4xx->2xx|$.message
Issue-Key: kms|ListAliases|4xx->2xx|status
Issue-Commit: a1fc1312a83efd9dc63ae7bee3778196f32f40f6

# KMS ListAliases accepts an alias ARN as KeyId and answers 200 instead of refusing InvalidArnException

> _Note: this issue was identified against the code base at commit `a1fc1312a83efd9dc63ae7bee3778196f32f40f6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListAliases

## Description
One net-new wire divergence on kms:ListAliases' input validation: an ALIAS identifier supplied in the KeyId parameter is no longer refused at all. The emulator now serves the request as a normal 200 listing, where AWS rejects the request outright. 1 cause.

Related open issue that does NOT cover this cluster: 20261103T160539Z-kms-aws-managed-key-lookup-forms-diverge-alias.md. It describes the same request shape but a DIFFERENT emulator failure (it recorded a 400 NotFoundException with the wrong error class and message, replay key kms|ListAliases|4xx->4xx|$.__type / $.message). The emulator's behavior has since moved: the refusal is gone entirely and the operation now succeeds, so this replay's cluster key is kms|ListAliases|status=400->200|keys=$.__type,$.message,status and the replay report attributes it to no open issue (owned_by empty). Closing that issue's error-class/message cause would not make this step pass, because no error is produced at all. Also not covered by 20261110T201952Z-kms-getkeypolicy-given-an-alias-arn-is-refused.md (a different operation, GetKeyPolicy) or by 20261027T205645Z-backup-a-backup-vault-s-default-encryptionkeyarn-names-a.md / 20261027T205606Z-backup-kms-s3-a-backup-vault-s-default-encryptionkeyarn-names-a.md (a 400 where AWS answers 200 on the backup path - the opposite direction).

Cause 1 of 1: ListAliases given an alias ARN in KeyId returns 200 instead of refusing the identifier form (fixture kms-rbx-traj-vault-key-lookup-forms, step task#3).

AWS: Smithy kms#ListAliases input KeyId (KeyIdType, @length 1-2048) is documented as optional and, when given, restricted to one identifier form: "Specify the key ID or key ARN of the KMS key", with the documented examples being a bare key id (1234abcd-12ab-34cd-56ef-1234567890ab) and a key ARN (arn:aws:kms:<region>:<account>:key/<uuid>). An alias name or alias ARN is NOT an accepted form for this parameter. ListAliases' modelled client errors are InvalidArnException (@httpError 400, awsQueryError InvalidArn), InvalidMarkerException (400) and NotFoundException (@httpError 404). An alias identifier is therefore a bad ARN *form* rather than a missing resource or a successful lookup: the recorded response is HTTP 400 with __type InvalidArnException and message "Key Aliases are not supported for this operation." That message is entirely FIXED text for this error class - it interpolates nothing from the request or from state, so it must be reproduced verbatim. The 400 status (not the shape's own modelled 404, and not a 2xx) and the error code InvalidArnException are likewise requirements.

Emulator: HTTP 200 with a normal ListAliasesResponse body and no error members at all - replay reports "status: expected 400, got 200", "$.__type: missing (expected 'InvalidArnException', got None)", "$.message: missing (expected 'Key Aliases are not supported for this operation.', got None)". The alias-ARN KeyId is silently accepted and the operation runs to success. Hypothesis: KeyId is resolved through the generic key-identifier resolver (which also understands alias names/ARNs) and no check rejects the alias forms for this specific operation, so what used to surface as a wrong-class NotFoundException now resolves and lists.

Why: a client that probes a KMS key identifier through several forms - console-style reference chains, inventory/compliance tools handed an opaque identifier, code that passes a vault's or bucket's configured key reference straight through - relies on this refusal to learn that it holds an ALIAS and must resolve it to a key ARN first. Against the emulator it gets a 200 instead, so the wrong-form request looks correct: the caller never resolves the alias, keeps using the alias identifier as if it were a key identifier, and any alias list, cache key or audit record it builds is attributed to an identifier AWS would never have accepted. A success where AWS returns 4xx is also invisible to SDK error handling, so no retry or fallback branch runs at all; against real AWS the same code path fails at the first call.

Fix: ListAliases' input validation in the kms crate's operation handler - classify an alias name or alias ARN supplied in KeyId as InvalidArnException at HTTP 400 with AWS's fixed message, before the key-identifier resolution and before any alias/key state lookup, rather than letting the shared identifier resolver accept alias forms for this operation.

Traces: kms-rbx-traj-vault-key-lookup-forms#task#3.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, /__admin/health 200; 69 fixtures replayed, 520 steps, 494 passed, 26 mismatched, 0 errored, 0 blocked, score 95.0%, 13 divergence clusters). Twelve clusters are already owned by open issues (20261029T235222Z-kms-crypto-operation-responses-omit-the-keyorigin-projection.md, 20261110T201952Z-kms-getkeypolicy-given-an-alias-arn-is-refused.md, 20261103T160539Z-kms-aws-managed-key-lookup-forms-diverge-alias.md) and are not part of this issue. The single unowned cluster, verbatim:

service=kms | operation=ListAliases | verdict=mismatch | divergence_key=service=kms|ListAliases|status=400->200|keys=$.__type,$.message,status | fixture_id=kms-rbx-traj-vault-key-lookup-forms | step_key=task#3 | expected_status=400 | actual_status=200 | affected_count=1 | owned_by=[] | stub_path=""
detail:
  status: expected 400, got 200
  $.__type: missing (expected 'InvalidArnException', got None)
  $.message: missing (expected 'Key Aliases are not supported for this operation.', got None)

Expected (recorded AWS wire): HTTP 400, __type "InvalidArnException", message "Key Aliases are not supported for this operation." - the whole message is fixed text for this error class, with no request- or state-derived part.
Actual (emulator): HTTP 200, a ListAliasesResponse with no __type and no message.

Model checked (services/service-com-amazonaws-kms-trentservice/smithy-model/com-amazonaws-kms-trentservice.json): kms#ListAliases input {KeyId KeyIdType @length 1-2048, optional, doc "Specify the key ID or key ARN of the KMS key" with key-id and key-ARN examples only; Limit LimitType @range 1-1000; Marker MarkerType @length 1-1024 @pattern ^[\u0020-\u00FF]*$}; output {Aliases, NextMarker, Truncated default false}; errors InvalidArnException @httpError 400 (awsQueryError InvalidArn), InvalidMarkerException 400, NotFoundException @httpError 404, KMSInternalException 500, DependencyTimeoutException 503. The model admits no alias identifier form for KeyId, and InvalidArnException at 400 is exactly the modelled slot for a malformed/unsupported identifier form.

## Steps to Reproduce
1. Clone the repo (git_clone) and boot the emulator built from this checkout.
2. Replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1).
3. Observe the cluster for fixture kms-rbx-traj-vault-key-lookup-forms, step task#3 (operation ListAliases).
4. Directly: call kms:ListAliases with KeyId set to an ALIAS ARN rather than a key id/key ARN - e.g. the alias ARN of an AWS managed key, arn:aws:kms:<region>:<account>:alias/aws/backup (an alias NAME, alias/<name>, is the same wrong form).
5. The emulator answers HTTP 200 with a ListAliasesResponse body. AWS answers HTTP 400 with __type InvalidArnException and the fixed message "Key Aliases are not supported for this operation.".
6. Re-replay kms-rbx-traj-vault-key-lookup-forms#task#3 to confirm a fix: the step must come back 400 with that error code and that exact message text.
