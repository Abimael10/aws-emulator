Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: pca-connector-scep.create-challenge-concurrent-same-client-token-dedup pca-connector-scep.create-challenge-different-client-token-creates-distinct pca-connector-scep.create-challenge-idempotent-client-token pca-connector-scep.create-challenge-returns-password-and-metadata pca-connector-scep.delete-challenge-removes-password pca-connector-scep.delete-connector-cascades-challenges pca-connector-scep.list-challenge-metadata-pagination pca-connector-scep.scep-get-cacert-native-protocol-roundtrip pca-connector-scep.tag-challenge-resource
Issue-Shape-Id: com.amazonaws.pcaconnectorscep#PcaConnectorScep
Issue-Key: pca-connector-scep|CreateResourceShare|MalformedArnException
Issue-Commit: 2b82b69d1d1b9a3f3311d2778846fa1839308fd7

# RAM CreateResourceShare refuses an AWS Private CA certificate-authority ARN with MalformedArnException "You cannot share the selected resource type.", so no SCEP connector can ever be set up

> _Note: this issue was identified against the code base at commit `2b82b69d1d1b9a3f3311d2778846fa1839308fd7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateResourceShare

## Description
REOPENS 20261022T005013Z-ram-resource-share-state-diverges-resourceshareconfiguration-and-tag.md: its fix left the call refused.

This issue carries ONE root cause (Cause 1 of 1). It is reported under the pca-connector-scep heading because that is where the suite lists the nine failing checks, but the defect is in the RAM slice the checks call during setup.

Cause 1 of 1: RAM's shareable-resource-type catalog omits AWS Private CA, so CreateResourceShare refuses a certificate-authority ARN.

AWS: Smithy (com.amazonaws.ram#CreateResourceShare) — input `resourceArns: ResourceArnList`, `principals: PrincipalArnOrIdList`, `sources: SourceArnOrAccountList`, `permissionArns: PermissionArnList`; http {method: POST, uri: /createresourceshare, code: 200}; output `resourceShare: ResourceShare`. The only ARN-shaped refusal it models is `MalformedArnException` (httpError 400, awsQueryError InvalidArn.Malformed), documented as "The operation failed because the specified Amazon Resource Name (ARN) has a format that isn't valid." — i.e. a FORMAT refusal, not a statement about which resource types RAM can share. The model enumerates no shareable-resource-type list (there is no such shape; `CreatePermission$resourceType` points at ListResourceTypes instead), so the catalog itself is documentation-only: AWS Resource Access Manager publishes AWS Private CA as a shareable resource type, display name `acm-pca:CertificateAuthority`, whose resources are named by `arn:<partition>:acm-pca:<region>:<account>:certificate-authority/<id>`, and it supports customer managed permissions. AWS Private CA's own "Connector for SCEP — setting up" procedure requires exactly this call: share the CA through RAM with the service principal `pca-connector-scep.amazonaws.com`, naming the AWS-managed permission `arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority` and the sharing account as the source. On AWS that request is answered 200 with the new `resourceShare` (status ACTIVE/PENDING) and the share's RESOURCE/PRINCIPAL/SOURCE associations.

Emulator: the same request is refused 400 `MalformedArnException` with message `You cannot share the selected resource type.` — the fixed text of the emulator's own non-shareable-type refusal, which interpolates nothing from the request. Two divergences in one: (a) a certificate-authority ARN is classified as non-shareable at all, and (b) the refusal of a well-formed ARN is rendered as the FORMAT error `MalformedArnException` rather than as a type/permission refusal. The emulator's shareable-type catalog has no `acm-pca:` entry of any kind, so every AWS Private CA ARN fails the check regardless of partition, region, account or CA id.

Why: a client following AWS's documented Connector for SCEP setup cannot complete step 3, so it can never bring a connector to ACTIVE and every downstream pca-connector-scep operation (CreateChallenge, GetChallengePassword, GetChallengeMetadata, ListChallengeMetadata, DeleteChallenge, DeleteConnector, TagResource, and the native SCEP GetCACert endpoint) is unreachable. More generally, any client that shares an AWS Private CA — or any other resource type absent from the emulator's catalog — is told its ARN is malformed, which sends it down an input-validation path instead of the (nonexistent) authorization or quota path, and no retry or ARN correction can succeed.

Fix: the RAM shareable-resource-type catalog and the CreateResourceShare ARN classifier — add the AWS Private CA entry (`acm-pca:CertificateAuthority`, customer managed permissions supported) to the catalog the classifier consults, so a `certificate-authority/<id>` ARN passes and the share is created and answered 200; and keep `MalformedArnException` for values that are genuinely not ARNs. The same catalog backs ListResourceTypes, so the type must also become visible there. The related open issue 20261115T141853Z-ram-listpermissions-serves-an-incomplete-aws-managed-permission.md covers the AWS-MANAGED PERMISSION catalog (ListPermissions), including the AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority permission; it does NOT cover the shareable-resource-type catalog or this CreateResourceShare refusal, so it does not subsume this cause.

Traces: all nine failing pca-connector-scep checks listed in trace_ids fail on this one call during their shared setup helper (`_share_ca_with_scep` / `_share_ca_with_scep_connector`), before any pca-connector-scep assertion is reached. Code citations: services/service-com-amazonaws-ram-amazonresourcesharing/src/state.rs lines 416-473 (`RESOURCE_TYPES`, no acm-pca entry), lines 2151-2152 (`NON_SHAREABLE_RESOURCE_TYPE_MESSAGE`), lines 2165-2200 (`is_shareable_resource_arn` / `shareable_resource_arn_rejection`); services/service-com-amazonaws-ram-amazonresourcesharing/src/ops/create_resource_share.rs lines 250-258 (the rejection rendered as `MalformedArnException`).

## Evidence
Replay of simparity-behavioral-aws-v1: 12/21 passed (57.1%), 0 mismatched, 9 errored, 0 skipped, 0 unloadable; bootstrap_signal false (the emulator is up and answering). All 9 failures are under `### pca-connector-scep` and all 9 carry the identical traceback tail:

  File "client.py", line 1094, in _make_api_call
    raise error_class(parsed_response, operation_name)
  botocore.errorfactory.MalformedArnException: An error occurred (MalformedArnException) when calling the CreateResourceShare operation: You cannot share the selected resource type.

Request shape that is refused (from the checks' setup helper): CreateResourceShare(name=<generated>, resourceArns=[<acm-pca certificate-authority ARN just created by acm-pca CreateCertificateAuthority>], principals=["pca-connector-scep.amazonaws.com"], sources=[<caller account id>], permissionArns=["arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority"], tags=[{key:"sim2real-check", value:<check id>}]).

Expected (AWS): HTTP 200 with $.resourceShare populated, then GetResourceShares reports the share reaching ACTIVE, and the subsequent pca-connector-scep CreateConnector reaches ACTIVE.
Actual (emulator): HTTP 400, error code MalformedArnException, message "You cannot share the selected resource type." Message anatomy: the whole sentence is fixed emulator text with no variable part — it names nothing from the request (no ARN, member name, id, account or region), which is itself why the refusal is unactionable.

Model evidence: smithy_query(query="operation", operation="CreateResourceShare", model_path="services/service-com-amazonaws-ram-amazonresourcesharing/smithy-model/com-amazonaws-ram-amazonresourcesharing.json") → http {POST /createresourceshare, code 200}, errors include MalformedArnException (client, 400, InvalidArn.Malformed), InvalidParameterException, OperationNotPermittedException, UnknownResourceException. smithy_query(query="doc", shape="MalformedArnException") → "The operation failed because the specified Amazon Resource Name (ARN) has a format that isn't valid."

Source evidence: services/service-com-amazonaws-ram-amazonresourcesharing/src/state.rs `RESOURCE_TYPES` (lines 416-473) lists 57 entries spanning apigateway, appmesh, cloudtrail, codebuild, codeconnections, ec2, ec2-image-builder, glue, imagebuilder, license-manager, network-firewall, networkmanager, outposts, rds, resource-groups, route53resolver, s3-outposts, servicecatalog, ssm-contacts, ssm-incidents and vpc-lattice — and NO acm-pca entry. `is_shareable_resource_arn` (line 2165) matches `<arn service field>:<resource segment>` against that table, so `acm-pca:certificate-authority` matches nothing; `shareable_resource_arn_rejection` (line 2194) therefore returns NON_SHAREABLE_RESOURCE_TYPE_MESSAGE, which create_resource_share.rs (lines 250-258) raises as MalformedArnException.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. acm-pca: CreateCertificateAuthority (ROOT, RSA_2048/SHA256WITHRSA), then issue and import its self-signed certificate so the CA is ACTIVE. Note its ARN, of the form arn:<partition>:acm-pca:<region>:<account>:certificate-authority/<id>.
3. ram: call CreateResourceShare with resourceArns=[<that CA ARN>], principals=["pca-connector-scep.amazonaws.com"], sources=[<your account id>], permissionArns=["arn:aws:ram::aws:permission/AWSRAMBlankEndEntityCertificateAPICSRPassthroughIssuanceCertificateAuthority"].
4. Observe HTTP 400 MalformedArnException "You cannot share the selected resource type." where AWS answers HTTP 200 with the created resourceShare.
5. Optional confirmation that the type is simply absent from the catalog: ram ListResourceTypes and look for an entry whose resourceType is acm-pca:CertificateAuthority — it is not reported.
6. Re-replay the behavioral suite and re-run the nine checks in trace_ids; each one calls step 3 in its setup helper before any pca-connector-scep assertion, so each fails at the same call.
