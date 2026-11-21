Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eks.identity-provider-config-lifecycle eks.identity-provider-config-details-round-trip
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks.identity-provider-config-lifecycle|run#12|AssertionError|assert disassociate_message == not_found, f"expected message {not_found!r}, got {disassociate_message!r}"
Issue-Key: eks.identity-provider-config-details-round-trip|run#26|AssertionError|assert redisassociate_message == not_found, (
Issue-Commit: 0c46b3cc7da45ad21000fa4a6694a9d98cd58e91

# EKS DisassociateIdentityProviderConfig's ResourceNotFoundException message is the emulator's own wording, not AWS's "The requested resource does not exist."

> _Note: this issue was identified against the code base at commit `0c46b3cc7da45ad21000fa4a6694a9d98cd58e91`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DisassociateIdentityProviderConfig

## Description
One net-new root cause.

Cause 1 of 1: DisassociateIdentityProviderConfig refuses an OIDC identity provider config that does not exist on the cluster (never associated, or already disassociated) with the right error code but the wrong message text.
Failing checks: eks.identity-provider-config-lifecycle, eks.identity-provider-config-details-round-trip.

AWS: Smithy (services/service-com-amazonaws-eks-awswesleyfrontend/smithy-model/com-amazonaws-eks-awswesleyfrontend.json) models DisassociateIdentityProviderConfig POST /clusters/{clusterName}/identity-provider-configs/disassociate with ResourceNotFoundException @httpError 404 among its errors. Recorded behavior: when the named identityProviderConfig ({type, name}) is not associated with the (existing, ACTIVE) cluster, EKS answers HTTP 404 ResourceNotFoundException with the fixed, invariant message "The requested resource does not exist." — the whole message is fixed text for this error class; it interpolates NOTHING from the request (no cluster name, no config name, no type). The sibling DescribeIdentityProviderConfig on the same absent config answers with exactly that same message, and the emulator already gets that one right (both checks pass the describe assertion immediately before the failing one), so the divergence is confined to the Disassociate path's own refusal.

Emulator: the error code is ResourceNotFoundException, but the message is the emulator's own templated sentence naming the config: "No identity provider config found for name: <identityProviderConfigName>." Observed verbatim as 'No identity provider config found for name: oidc-77fc452e19d0.' (eks.identity-provider-config-lifecycle, disassociating a never-associated config) and 'No identity provider config found for name: sim2real-oidc-c44b782c93a1.' (eks.identity-provider-config-details-round-trip, re-disassociating a config whose disassociate update had already completed). The 'oidc-...'/'sim2real-oidc-...' portion is a per-run generated config name — it is the request value filling the emulator's template, not part of any requirement; the defect is the template itself, since AWS's message for this class has no variable part at all.

Why: a client that branches on, logs, or asserts the refusal text of a missing identity provider config sees text real EKS never emits. Error-message matching (common in idempotent teardown, CDK/CloudFormation custom resources and test harnesses that treat "The requested resource does not exist." as "already gone") does not match, so a cleanup that should be a no-op is reported as an unexpected failure, and the mismatch also reveals two different wordings for the same ResourceNotFoundException class across DescribeIdentityProviderConfig and DisassociateIdentityProviderConfig in one service.

Fix: the DisassociateIdentityProviderConfig handler's lookup/refusal path in the EKS service crate — the place that constructs the ResourceNotFoundException for an unresolved identityProviderConfig must emit the same fixed message as the Describe path ("The requested resource does not exist.") with HTTP 404, instead of formatting the requested config name into a message of its own.

Traces: eks.identity-provider-config-lifecycle (assertion `disassociate_message == not_found`, disassociating an absent config on a fresh ACTIVE cluster); eks.identity-provider-config-details-round-trip (assertion `redisassociate_message == not_found`, re-disassociating an already-disassociated config). Neither check is marked ALREADY FILED or OWNED in the suite report, and no open issue names DisassociateIdentityProviderConfig.

## Evidence
eks.identity-provider-config-lifecycle:
  AssertionError: expected message 'The requested resource does not exist.', got 'No identity provider config found for name: oidc-77fc452e19d0.'
  (at check_eks_identity_provider_config_lifecycle.py line 177: assert disassociate_message == not_found; the preceding assertion on DescribeIdentityProviderConfig's message for the SAME absent config passed)

eks.identity-provider-config-details-round-trip:
  AssertionError: expected message 'The requested resource does not exist.', got 'No identity provider config found for name: sim2real-oidc-c44b782c93a1.'
  (at check_eks_identity_provider_config_details_round_trip.py line 100: assert redisassociate_message == not_found; the preceding assertions on describe round-trip fields and on DescribeIdentityProviderConfig's not-found message passed)

Expected: HTTP 404, code ResourceNotFoundException, message exactly "The requested resource does not exist." (entirely fixed text — no request- or state-derived parts).
Actual:   code ResourceNotFoundException with message "No identity provider config found for name: <config name from the request>."

## Steps to Reproduce
1. Create an EKS cluster and wait for status ACTIVE.
2. Call DisassociateIdentityProviderConfig(clusterName=<that cluster>, identityProviderConfig={"type":"oidc","name":<a name never associated with the cluster>}).
3. Observe the ClientError: code is ResourceNotFoundException but Message is "No identity provider config found for name: <name>." instead of "The requested resource does not exist.".
4. Same divergence after a real lifecycle: AssociateIdentityProviderConfig with an oidc config, wait for the update to complete, DisassociateIdentityProviderConfig once (succeeds), wait for that update, then call DisassociateIdentityProviderConfig again with the same identityProviderConfig — the second call's message diverges the same way.
5. For contrast, DescribeIdentityProviderConfig on the same absent config returns the correct "The requested resource does not exist.".
