Issue-Source: trace-sweep-divergence
Issue-Trace: eks-register-cluster-request-shape-validation
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: eks-register-cluster-request-shape-validation
Issue-Shape-Id: com.amazonaws.eks#AWSWesleyFrontend
Issue-Key: eks|RegisterCluster|4xx->4xx|$.message
Issue-Key: eks|RegisterCluster|4xx->4xx|$.clusterName
Issue-Commit: 81e21ca007588719de802b4dd67d83498ccb7823

# eks RegisterCluster request-shape refusals use the wrong required-field sentence and omit clusterName from InvalidParameterException (2 causes)

> _Note: this issue was identified against the code base at commit `81e21ca007588719de802b4dd67d83498ccb7823`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterCluster

## Description
RegisterCluster's request-shape refusals carry the right HTTP status and error type but the wrong body. Two independent causes.

Cause 1 of 2 - the missing-required-member message is worded by the emulator's framework, not by EKS.
AWS: Smithy RegisterCluster requires the connectorConfig member (target ConnectorConfigRequest, required) and models InvalidParameterException (httpError 400). The recorded refusal message is exactly 'Field connectorConfig is required' - fixed text 'Field ' + the missing member name + ' is required', with no trailing period.
Emulator: answers the same 400 with 'The parameter connectorConfig is required.' - the generic framework sentence ('The parameter <name> is required.'), not EKS's wording.
Why: a client or test that matches on the refusal sentence (the only machine-readable hint of WHICH member was missing, since the error carries no field member) does not match, and operators reading logs see a message AWS never produces.
Fix: the RegisterCluster handler's required-member validation - emit EKS's sentence for this error class instead of the shared framework message.
Traces: eks-register-cluster-request-shape-validation#task#1 and #task#2 (same cluster, 2 steps), status 400->400.

Cause 2 of 2 - the InvalidParameterException body omits the clusterName member.
AWS: Smithy InvalidParameterException carries clusterName ('The Amazon EKS cluster associated with the exception'), nodegroupName, fargateProfileName, addonName, subscriptionId and message. For a RegisterCluster call whose name violates ClusterName's pattern ^[0-9A-Za-z][A-Za-z0-9\-_]*$ (e.g. a leading '-'), the recorded 400 echoes the rejected name in clusterName alongside the message.
Emulator: returns the error with message only - clusterName is absent (missing, got None).
Why: a client that correlates a refusal with the cluster it submitted (batch registration, or any handler that keys retries off error.clusterName) loses the identifier and cannot attribute the failure.
Fix: the RegisterCluster validation path's error construction - populate InvalidParameterException.clusterName from the request's name for name-shape refusals.
Traces: eks-register-cluster-request-shape-validation#task#3 and #task#4 (same cluster, 2 steps), status 400->400.

## Evidence
Cause 1 (task#1, status 400->400): $.message: value_mismatch (expected 'Field connectorConfig is required', got 'The parameter connectorConfig is required.').
Cause 2 (task#3, status 400->400): $.clusterName: missing (expected the rejected cluster name submitted in the request, e.g. '-sim2real-probe-eks-regval-2b824b02', got None). The name itself is a per-run probe value; the requirement is that clusterName echoes the request's name member.

## Steps to Reproduce
1. Boot the emulator and replay fixture eks-register-cluster-request-shape-validation.
2. Cause 1: POST RegisterCluster with a valid name and no connectorConfig (task#1/#task#2): status 400 and x-amzn-errortype InvalidParameterException are right, but the message is 'The parameter connectorConfig is required.' instead of 'Field connectorConfig is required'.
3. Cause 2: POST RegisterCluster with a name that violates the ClusterName pattern, e.g. a leading hyphen (task#3/#task#4): the 400 InvalidParameterException body has only message - no clusterName echoing the submitted name.
