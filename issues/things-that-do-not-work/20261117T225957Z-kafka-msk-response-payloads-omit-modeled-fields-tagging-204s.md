Issue-Source: trace-sweep-divergence
Issue-Trace: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka|TagResource|2xx->2xx|$headers.content-length
Issue-Key: kafka|UntagResource|2xx->2xx|$headers.content-length
Issue-Key: kafka|ListClustersV2|2xx->2xx|$.clusterInfoList[*].serverless.vpcConfigs[*].securityGroupIds
Issue-Commit: a82c0430faa27c08585abbfbf36f0bc62a674ea6

# MSK response payloads omit modeled fields: tagging 204s carry no content-length and ListClustersV2 drops serverless securityGroupIds (3 causes)

> _Note: this issue was identified against the code base at commit `a82c0430faa27c08585abbfbf36f0bc62a674ea6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
TagResource

## Description
THREE distinct root causes in the kafka (Amazon MSK) service, all of the same family: the emulator's successful responses omit a field AWS always serves, so the payload is structurally thinner than MSK's. Each cause must be closed separately before this issue is submitted.

Cause 1 of 3: TagResource's 204 response omits the content-length header. (fixture kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found, step task#8.)
AWS: Smithy TagResource (POST /v1/tags/{ResourceArn}, http code 204, output shape Unit, errors BadRequestException 400 / NotFoundException 404 / InternalServerErrorException 500). The model gives the operation an empty output and status 204, and says nothing about headers, so: recorded behavior only - MSK's 204 for a successful tag write is a complete HTTP message that still carries a content-length header (value 0 for the empty body). The header NAME and its presence are the requirement; its value is the length of the body the emulator serves for that response, and the recording asserts shape only, never a literal taken from one run.
Emulator: the same TagResource call returns 204 with the correct empty body but with NO content-length header at all (expected the header to be present, shape asserted only; got None).
Why: a client or proxy that frames the response from its headers - an HTTP/1.1 intermediary, a connection-reusing SDK transport, or a strict HTTP client that distinguishes "declared zero-length body" from "no length declared" - cannot tell the message is complete and must fall back to reading to connection close or to chunked framing. That breaks keep-alive reuse against the emulator where it works against AWS, and any recorded-vs-live header comparison fails on an operation whose body is otherwise identical.
Fix: the TagResource handler's 204 response construction in the kafka service (the empty-success response builder / its header serializer), which must emit content-length for a no-content response as the rest of the surface does.
Traces: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found#task#8.

Cause 2 of 3: UntagResource's 204 response omits the content-length header. (fixture kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found, step task#10.)
AWS: Smithy UntagResource (DELETE /v1/tags/{ResourceArn} with TagKeys bound to httpQuery tagKeys, http code 204, output shape Unit, errors BadRequestException 400 / NotFoundException 404 / InternalServerErrorException 500). As with TagResource the model models no headers, so: recorded behavior only - MSK answers the successful tag removal with 204 and a content-length header (0 for the empty body). This is a SEPARATE handler from Cause 1 (different method, different uri binding, different input binding) and is reported on its own fixture step, so it is its own cause even though the omitted field is the same.
Emulator: UntagResource returns 204 with no content-length header (expected present, shape asserted only; got None); the cluster's tags are removed correctly, only the response framing diverges.
Why: same client consequence as Cause 1 - an intermediary or transport that frames on content-length cannot establish that the 204 is complete, so connection reuse and header-level fidelity checks diverge from AWS on the tag-removal path as well as the tag-write path.
Fix: the UntagResource handler's 204 response construction / the kafka service's empty-success response builder and header serializer.
Traces: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found#task#10 (the same cluster's second untag, step task#12, is the same cause).

Cause 3 of 3: ListClustersV2 drops securityGroupIds from a serverless cluster's vpcConfigs. (fixture kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found, step task#15.)
AWS: Smithy ListClustersV2 (GET /api/v2/clusters, http code 200) returns ClusterInfoList (jsonName clusterInfoList, list of Cluster). Cluster.Serverless (jsonName serverless, shape Serverless) carries VpcConfigs (jsonName vpcConfigs, required, list of VpcConfig), and VpcConfig has SubnetIds (jsonName subnetIds, required, doc "The IDs of the subnets associated with the cluster") and SecurityGroupIds (jsonName securityGroupIds, list of string, doc "The IDs of the security groups associated with the cluster"). MSK's list view of a cluster is the same Cluster shape DescribeClusterV2 returns, so for a serverless cluster created with security groups in serverless.vpcConfigs the listed entry echoes them: the member is present and non-empty. The group ids themselves are VPC/account values taken from the create request, not fixed text.
Emulator: $.clusterInfoList[0].serverless.vpcConfigs[0].securityGroupIds is absent entirely (expected a list, shape asserted only; got None) while the sibling subnetIds is served, so the listed serverless cluster reports only half of the VPC configuration that was submitted.
Why: a client that enumerates clusters and reads each one's network configuration from the list page - to attach an event source mapping, a VPC connection or another in-VPC client without a second DescribeClusterV2 per cluster, or to assert a deployment converged - sees no security groups at all and cannot distinguish "none configured" from "not reported", so dependent provisioning and configuration-drift checks break.
Fix: the ListClustersV2 serializer's serverless branch and/or the cluster state that records a serverless cluster's VpcConfigs - SecurityGroupIds must be persisted at create time and emitted alongside SubnetIds wherever the Cluster shape is serialized.
Traces: kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found#task#15. The OPEN issue 20261110T080620Z-kafka-msk-serverless-cluster-surface-diverges-provisioned-arn-suffix.md covers the same omission on the DESCRIBE path (DescribeClusterV2, its Cause 2) and carries only that fixture step's replay key, not this one; the LIST path is not exercised by it, so closing it does not close this. If the fix is the shared state/serializer, both must be re-replayed.

## Evidence
All three causes replayed from fixture `kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found` (service kafka, verdict mismatch, empty stub_path, no owning issue).

Cause 1 - step task#8, TagResource, status 204 expected / 204 actual:
  $headers.content-length: missing (expected present, shape asserted only; got None)
  Status and body agreed; only the header is absent.

Cause 2 - step task#10, UntagResource, status 204 expected / 204 actual:
  $headers.content-length: missing (expected present, shape asserted only; got None)
  Cluster member of the same cluster: step task#12 (second UntagResource) shows the identical omission.

Cause 3 - step task#15, ListClustersV2, status 200 expected / 200 actual:
  $.clusterInfoList[0].serverless.vpcConfigs[0].securityGroupIds: missing (expected a list, shape asserted only; got None)
  Sibling subnetIds was served; only securityGroupIds is absent.

Smithy cited: TagResource POST /v1/tags/{ResourceArn} code 204, output Unit, errors BadRequestException(400)/NotFoundException(404)/InternalServerErrorException(500); UntagResource DELETE /v1/tags/{ResourceArn} code 204 with TagKeys bound to httpQuery `tagKeys`, output Unit, same error set; ListClustersV2 GET /api/v2/clusters code 200, output ClusterInfoList (jsonName clusterInfoList, list of Cluster) -> Cluster.Serverless (jsonName serverless) -> Serverless.VpcConfigs (jsonName vpcConfigs, required, list of VpcConfig) -> VpcConfig.SubnetIds (jsonName subnetIds, required) + VpcConfig.SecurityGroupIds (jsonName securityGroupIds, list of __string). No header is modeled for the 204 operations, so their content-length requirement is recorded behavior only.

## Steps to Reproduce
Boot the emulator from this checkout, then replay fixture `kafka-serverless-iam-auth-without-security-group-tags-brokers-listed-and-not-found`.

Cause 1 (step task#8):
1. Create a serverless MSK cluster with CreateClusterV2 (serverless.vpcConfigs with subnetIds and securityGroupIds; clientAuthentication.sasl.iam.enabled = true).
2. Call TagResource on that cluster ARN with one or more tags.
3. Assert the 204 response carries a content-length header (value = the served body length, i.e. 0) - today the header is absent.

Cause 2 (step task#10):
1. Using the same cluster, call UntagResource with tagKeys naming a tag set in step 2 above.
2. Assert the 204 response carries a content-length header - today it is absent. Repeat for a second untag (step task#12) to confirm.

Cause 3 (step task#15):
1. With the serverless cluster from Cause 1 in place, call ListClustersV2 (optionally with clusterNameFilter naming it).
2. Locate the cluster's entry in clusterInfoList and read serverless.vpcConfigs[0].
3. Assert securityGroupIds is present and lists the security groups supplied at create time, alongside subnetIds - today the member is omitted while subnetIds is served.
