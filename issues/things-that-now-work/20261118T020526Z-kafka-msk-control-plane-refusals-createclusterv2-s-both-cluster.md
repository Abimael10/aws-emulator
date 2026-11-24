Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kafka.cluster-create-v2-both-cluster-types-rejected kafka.configuration-arn-rejected-for-cluster-operations kafka.tag-resource-malformed-arn-rejected
Issue-Shape-Id: com.amazonaws.kafka#Kafka
Issue-Key: kafka.cluster-create-v2-both-cluster-types-rejected|run#7|AssertionError|assert message == _ONE_CLUSTER_TYPE_MESSAGE, message
Issue-Key: kafka.configuration-arn-rejected-for-cluster-operations|expect_error#7|AssertionError|assert got_code == code, f"expected {code}, got {got_code}: {got_message!r}"
Issue-Key: kafka.tag-resource-malformed-arn-rejected|run#5|AssertionError|assert tag_err == expected, tag_err
Issue-Commit: 775d8b7a5dc0233a720d935fa0de7ffef061d5f2

# MSK control-plane refusals: CreateClusterV2's both-cluster-types message regressed, GetBootstrapBrokers answers 400 for a non-cluster MSK ARN, and the tagging bad-ARN message echoes the input (3 causes)

> _Note: this issue was identified against the code base at commit `775d8b7a5dc0233a720d935fa0de7ffef061d5f2`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateClusterV2

## Description
THREE distinct net-new root causes on the kafka (MSK) control-plane refusal paths. Each is a separate rule in a separate handler and must be closed separately; all three must be closed before this issue is submitted. None of the three failing checks is marked ALREADY FILED or OWNED in the replay report.

Cause 1 of 3: CreateClusterV2's mutual-exclusion refusal now carries the wrong message text (a regression of a previously fixed cause).
AWS: CreateClusterV2 (Smithy: POST /api/v2/clusters, success 200; input CreateClusterV2Request members ClusterName jsonName clusterName @required @length min 1 max 64, Provisioned -> ProvisionedRequest, Serverless -> ServerlessRequest, both optional structure members rather than a union, so the mutual-exclusion rule is enforced by the service; errors BadRequestException httpError 400, ConflictException 409, ForbiddenException 403, UnauthorizedException 401, TooManyRequestsException 429, InternalServerErrorException 500, ServiceUnavailableException 503). The model does not state the message, so for the wording this is recorded behavior only: a CreateClusterV2 whose body supplies a COMPLETE, otherwise-valid Provisioned body AND a COMPLETE Serverless body is refused HTTP 400 / BadRequestException with the message "You can specify only one cluster type, either provisioned or serverless." That sentence is entirely FIXED text — no member name, cluster name or type token is interpolated — and no cluster is created (ListClustersV2 filtered on the name stays empty).
Emulator: the status and code are right (BadRequestException), but the message is now the single fixed sentence "Invalid request body".
Why: a client that classifies MSK's 400s by message (distinguishing a mutually-exclusive cluster-type selection from a malformed body, a quota refusal or a bad broker spec) takes the wrong branch, and a suite asserting MSK's sentence fails even though status and code match. This cause was already filed and closed once as Cause 5 of the FIXED issue 20261104T171616Z-kafka-createclusterv2-validates-no-provisioned-cluster-input-four-requests.md (which demanded exactly this sentence for this same check id), so the behavior has regressed. Note the open WIRE issue 20261117T050101Z-kafka-msk-createclusterv2-cluster-type-validation-refusal-carries-emulator.md asks for the opposite text ("Invalid request body") from its own fixture step; the two recordings are of DIFFERENT request classes and both must hold — a body MSK cannot deserialize at all answers "Invalid request body", while a body that deserializes and names two valid cluster types answers the cluster-type sentence. Fixing one by overwriting the other will keep flipping this check.
Fix: the cluster-type selection validation in the CreateClusterV2 handler (services/service-com-amazonaws-kafka-kafka/src/ops/create_cluster_v2.rs) must distinguish an undeserializable body from a well-formed body naming both cluster types, and emit the cluster-type sentence for the latter while leaving the former on "Invalid request body"; no cluster record may be written on either path.
Traces: kafka.cluster-create-v2-both-cluster-types-rejected.

Cause 2 of 3: GetBootstrapBrokers refuses a well-formed MSK ARN of the wrong resource type as an invalid parameter instead of as an unresolvable cluster.
AWS: GetBootstrapBrokers (Smithy: GET /v1/clusters/{ClusterArn}/bootstrap-brokers, success 200, ClusterArn is an @httpLabel @required plain string with no pattern constraint; modeled errors BadRequestException 400, ConflictException 409, ForbiddenException 403, UnauthorizedException 401, InternalServerErrorException 500 — NotFoundException is NOT modeled on this operation). Recorded behavior only for the class: handed an MSK CONFIGURATION ARN as its ClusterArn, MSK answers HTTP 404 with error code NotFoundException and the message "The requested resource doesn\u2019t exist." (entirely fixed text, U+2019 apostrophe; neither the ARN nor the region is echoed) — i.e. an MSK ARN that is not a cluster ARN is treated as a cluster reference that does not resolve, not as a syntactically invalid parameter. The sibling DeleteCluster on the same ARN DOES answer BadRequestException "One or more of the parameters are not valid." (that assertion passes today), so the two classes are per-operation and must not be unified.
Emulator: HTTP 400 BadRequestException with the message "One or more of the parameters are not valid." — the handler's `crate::state::is_cluster_arn` shape gate (services/service-com-amazonaws-kafka-kafka/src/ops/get_bootstrap_brokers.rs, the `malformed_arn(CLUSTER_ARN_MEMBER)` branch) runs before the cluster lookup and rejects any MSK ARN whose resource type is not `cluster`, so the handler's own correct 404 path (`RESOURCE_NOT_FOUND_MESSAGE` via the unmodeled channel) is never reached.
Why: a client branches on 400-vs-404: a 404 means the cluster is absent (fix the identifier / wait for eventual consistency), a 400 means the request itself is malformed. Pointed at a configuration ARN by mistake, a caller is told its request is invalid rather than that no such cluster exists, and tooling that retries not-found but surfaces bad-request behaves differently against the emulator than against MSK.
Fix: in get_bootstrap_brokers.rs, restrict the pre-lookup shape gate to strings that are not usable MSK ARNs at all, and let a well-formed MSK ARN of another resource type fall through to the cluster lookup so it is answered 404 NotFoundException with the fixed sentence. The same gate is shared by DescribeCluster and DescribeClusterV2, which the check asserts behave identically (those two calls were never reached in this run because it stopped at GetBootstrapBrokers), so align all three readers while leaving DeleteCluster's 400 as it is.
Traces: kafka.configuration-arn-rejected-for-cluster-operations.

Cause 3 of 3: the tagging operations' not-an-ARN refusal interpolates the submitted value into a message AWS keeps fixed.
AWS: TagResource (Smithy: POST /v1/tags/{ResourceArn}, success 204, errors BadRequestException 400 and NotFoundException 404), UntagResource (DELETE /v1/tags/{ResourceArn}) and ListTagsForResource (GET /v1/tags/{ResourceArn}, 200) all take ResourceArn as an @httpLabel @required string. Recorded behavior for the wording: a ResourceArn that is not an ARN at all is refused HTTP 400 / BadRequestException with the message "Invalid input resource arn: bad syntax" — the WHOLE string is fixed text; the offending value is NOT echoed into it — and all three operations answer identically.
Emulator: HTTP 400 / BadRequestException (class correct) with the message "not-an-arn is not a valid Amazon MSK resource ARN.", i.e. the emulator's own sentence with the request's ResourceArn value interpolated as its leading token (services/service-com-amazonaws-kafka-kafka/src/ops/tag_resource.rs, the `TagTargetRejected::NotAnArn` branch, plus the same branch in untag_resource.rs and list_tags_for_resource.rs).
Why: the message is part of the contract — log scrapers, error tables and exact-match assertions keyed on MSK's sentence stop matching, and the emulator reflects caller input back into a constant message, so an identical defect produces a different string per request and cannot be matched at all. Related open issue 20261029T202809Z-kafka-msk-not-found-already-exists-and-tagging-refusals.md (Causes 5/6) describes the same handlers for a DIFFERENT input class (a well-formed-but-unusable ARN, whose recorded message has no ": bad syntax" suffix) and carries only wire fixture keys, so it does not cover this check; both input classes must end up on their recorded text.
Fix: replace the formatted sentence in the shared not-an-ARN refusal used by tag_resource.rs / untag_resource.rs / list_tags_for_resource.rs with the service's fixed message constant, and keep the value out of `message`.
Traces: kafka.tag-resource-malformed-arn-rejected.

## Evidence
All three are kafka behavioral checks, verdict mismatch, from suite simparity-behavioral-aws-v1 (overall 171/238); none marked ALREADY FILED or OWNED.

Cause 1 — kafka.cluster-create-v2-both-cluster-types-rejected
  Request: CreateClusterV2(ClusterName=<unique>, Provisioned={BrokerNodeGroupInfo{InstanceType kafka.m5.large, 2 distinct-AZ ClientSubnets, SecurityGroups, EbsStorageInfo VolumeSize 10}, KafkaVersion 3.9.x, NumberOfBrokerNodes 2}, Serverless={VpcConfigs[{SubnetIds, SecurityGroupIds}], ClientAuthentication{Sasl{Iam{Enabled true}}}}) — both bodies complete and individually valid.
  Expected: Error.Code 'BadRequestException' (matches) and Error.Message exactly 'You can specify only one cluster type, either provisioned or serverless.'; then ListClustersV2(ClusterNameFilter=<name>) == [].
  Actual: AssertionError at `assert message == _ONE_CLUSTER_TYPE_MESSAGE, message` with message == 'Invalid request body'.
  Message anatomy: the expected sentence is 100% fixed text; the per-run cluster name is NOT part of it.

Cause 2 — kafka.configuration-arn-rejected-for-cluster-operations
  Setup: CreateConfiguration(Name=<unique>, ServerProperties=b'auto.create.topics.enable=true', KafkaVersions=[3.9.x]) -> Arn (a configuration ARN). DeleteCluster(ClusterArn=<that configuration ARN>) correctly answered ('BadRequestException', 'One or more of the parameters are not valid.').
  Then GetBootstrapBrokers(ClusterArn=<that configuration ARN>):
    Expected: ('NotFoundException', 'The requested resource doesn\u2019t exist.')
    Actual:   ('BadRequestException', 'One or more of the parameters are not valid.')
  Failure line: `assert got_code == code` -> "expected NotFoundException, got BadRequestException: 'One or more of the parameters are not valid.'". The check's remaining assertions (DescribeClusterV2, DescribeCluster on the same ARN, and the DescribeConfiguration positive control) were never reached.
  Message anatomy: 'The requested resource doesn\u2019t exist.' is entirely fixed text with a U+2019 apostrophe; the ARN and region must not appear in it.

Cause 3 — kafka.tag-resource-malformed-arn-rejected
  Positive control ListClustersV2() succeeded, so the client/credentials are healthy.
  TagResource(ResourceArn='not-an-arn', Tags={'env':'sim2real'}):
    Expected: ('BadRequestException', 'Invalid input resource arn: bad syntax')
    Actual:   ('BadRequestException', 'not-an-arn is not a valid Amazon MSK resource ARN.')
  The check then asserts the identical pair for UntagResource(ResourceArn='not-an-arn', TagKeys=['env']) and ListTagsForResource(ResourceArn='not-an-arn'); neither was reached.
  Message anatomy: the expected message is entirely fixed text. In the emulator's string, 'not-an-arn' is the REQUEST's ResourceArn value spliced in — the one part that varies per request and that AWS never includes; the rest is emulator prose.
  Handler evidence: services/service-com-amazonaws-kafka-kafka/src/ops/tag_resource.rs lines 187-198 format `"{} is not a valid Amazon MSK resource ARN.", input.resource_arn` on the `TagTargetRejected::NotAnArn` branch.

Smithy cited (services/service-com-amazonaws-kafka-kafka/smithy-model/com-amazonaws-kafka-kafka.json): CreateClusterV2 POST /api/v2/clusters 200, BadRequestException httpError 400, Provisioned/Serverless both optional members; GetBootstrapBrokers GET /v1/clusters/{ClusterArn}/bootstrap-brokers 200 with NO NotFoundException modeled (hence 'recorded behavior only' for the 404); TagResource POST /v1/tags/{ResourceArn} 204 and ListTagsForResource GET /v1/tags/{ResourceArn} 200, both modeling BadRequestException 400 and NotFoundException 404.

## Steps to Reproduce
1. Boot the emulator and point an MSK (kafka) client at its endpoint; discover two subnets in distinct AZs and a security group.
2. Cause 1: call CreateClusterV2 with a valid ClusterName and BOTH a complete Provisioned body (kafka.m5.large, 2 brokers, the two subnets, 10 GiB EBS, KafkaVersion 3.9.x) and a complete Serverless body (VpcConfigs with the same subnets/SG, Sasl.Iam enabled). Assert HTTP 400, Error.Code 'BadRequestException' and Error.Message exactly 'You can specify only one cluster type, either provisioned or serverless.', then assert ListClustersV2(ClusterNameFilter=<name>) is empty. Separately assert that a body MSK cannot deserialize still answers 'Invalid request body', so both recorded classes hold.
3. Cause 2: CreateConfiguration(Name=<unique>, ServerProperties=b'auto.create.topics.enable=true', KafkaVersions=['3.9.x']) and take its Arn. Call GetBootstrapBrokers(ClusterArn=<that configuration ARN>) and assert HTTP 404, Error.Code 'NotFoundException', Error.Message exactly 'The requested resource doesn\u2019t exist.'. Assert the same for DescribeClusterV2 and DescribeCluster on that ARN, that DeleteCluster on it still answers ('BadRequestException','One or more of the parameters are not valid.'), and that DescribeConfiguration(Arn=<that ARN>) still succeeds and echoes Arn and Name.
4. Cause 3: call TagResource(ResourceArn='not-an-arn', Tags={'env':'sim2real'}), UntagResource(ResourceArn='not-an-arn', TagKeys=['env']) and ListTagsForResource(ResourceArn='not-an-arn'). Assert each answers HTTP 400 BadRequestException with Error.Message exactly 'Invalid input resource arn: bad syntax', with the submitted value absent from the message.
5. Re-replay kafka.cluster-create-v2-both-cluster-types-rejected, kafka.configuration-arn-rejected-for-cluster-operations and kafka.tag-resource-malformed-arn-rejected and confirm all three pass.
