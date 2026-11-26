Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: wickr.delete-network-cascades-to-bots-and-security-groups
Issue-Shape-Id: com.amazonaws.wickr#WickrAdminApi
Issue-Key: wickr.delete-network-cascades-to-bots-and-security-groups|run#22|AssertionError|assert (group_code, group_msg) == ("401", "invalid security group request"), (
Issue-Commit: a8808f5e759731ce56a683280b0bc81ea90f67f7

# wickr: a deleted network's GetSecurityGroup is refused as a missing network (labelled ResourceNotFoundError) instead of the unlabelled 401 'invalid security group request'

> _Note: this issue was identified against the code base at commit `a8808f5e759731ce56a683280b0bc81ea90f67f7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetSecurityGroup

## Description
One net-new root cause in the wickr admin API: how GetSecurityGroup IDENTIFIES the refusal of a security group whose parent network has been deleted.

Cause 1 of 1: after DeleteNetwork has taken effect, GetSecurityGroup on a group that network held is refused as a missing NETWORK, with a discriminator-labelled ResourceNotFoundError whose message splices in the request's networkId, instead of the unlabelled HTTP 401 with the fixed sentence AWS sends.
  AWS: Smithy (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): GetSecurityGroup GET /networks/{networkId}/security-groups/{groupId}, success 200, models BadRequestError (client, httpError 400), UnauthorizedError (client, httpError 401 — documented "The request was not authenticated or the authentication credentials were invalid"), ForbiddenError (403), ValidationError (422), RateLimitError (429), ResourceNotFoundError (client, httpError 404, member `message` documented "A message identifying which resource was not found") and InternalServerError (500). The model does not state which resource a security-group read inside a deleted network reports, so recorded behavior only: DeleteNetwork is asynchronous; once the deletion has taken effect GetNetwork for that network is refused error code `ForbiddenError` with message `Network not found` and GetBot for a bot it held is refused HTTP 404 with message `Bot not found` (the emulator now matches both — the assertions preceding this one pass). A GetSecurityGroup for a group THAT network held is refused with HTTP status 401 and message `invalid security group request`, in an error body the SDK cannot resolve to a modelled shape (no `x-amzn-errortype` / `__type` discriminator), so botocore reports Error.Code as the bare status string `401`. The sentence `invalid security group request` is ENTIRELY fixed text: it has no variable parts — it names neither the groupId, nor the networkId, nor the deleted network's state, so the boundary discloses nothing about what the account used to hold. In other words the cascade is reported at the resource the request actually asked for (the security group), is reported as an authorization-class 401 rather than a not-found, and is reported unlabelled.
  Emulator: the same call is refused with Error.Code `ResourceNotFoundError` (a labelled HTTP 404) and message `network <networkId> does not exist` — the networkId is a request value filled into an emulator-authored template, not part of any requirement (the replayed instance read `network 10000000 does not exist`; that id is a single run's value). The handler pre-checks the parent network and returns a network-level refusal before it ever looks for the group, and it emits that refusal through the modelled, discriminated error channel.
  Why: the status, the error code and the message are requirements of the error class. A client that reconciles or cleans up after DeleteNetwork branches on the per-resource refusal: matching HTTP 401 / Error.Code `401` or the sentence `invalid security group request` it sees none of them, so it classifies a cascaded group deletion as a plain not-found (or as an unrelated failure of the wrong resource — the network) and its cascade-verification and retry-vs-give-up paths take the wrong branch. The emulator additionally discloses the networkId at a boundary where AWS withholds all request detail.
  Fix: GetSecurityGroup's refusal path — the handler's resource-resolution order (report the resource the request named, the security group, rather than short-circuiting on the parent network) and the error channel it uses for a group whose network is gone (the unlabelled 401 form AWS sends, not the discriminated ResourceNotFoundError). The fixed sentence belongs with the other shared gone-texts in the state module so no other security-group operation can disagree about it. This is NOT a reopen of 20261122T142729Z-wickr-createbot-duplicate-username-reason-re-worded-and.md: that (now fixed) issue fixed the GetBot leg of the same cascade and explicitly recorded the GetSecurityGroup assertion as not reached and not claimed; it is reached for the first time now.
  Traces: check wickr.delete-network-cascades-to-bots-and-security-groups. Handler: services/service-com-amazonaws-wickr-wickradminapi/src/ops/get_security_group.rs lines 171-176 (the network pre-check that returns the network-level not_found) and the `not_found` helper at lines 197-202; shared fixed refusal texts: src/state.rs.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against this checkout: wickr 5/8 passed; the other 2 failures are the ALREADY FILED GetBotsCount scaffold stub and are not part of this issue.

[mismatch] wickr.delete-network-cascades-to-bots-and-security-groups
  assertions that PASSED first: GetNetwork on the deleted network ends up ('ForbiddenError', 'Network not found'); GetBot on that network's bot is ('404', 'Bot not found').
  failing assertion (check line 107): (group_code, group_msg) == ("401", "invalid security group request") for GetSecurityGroup(networkId=<deleted network>, groupId=<that network's default group>)
  AssertionError: expected the deleted network's security group to be refused (401/'invalid security group request'), got 'ResourceNotFoundError': 'network 10000000 does not exist'
  Expected: HTTP 401 with an error body carrying no shape discriminator (botocore Error.Code = '401'), Error.Message = 'invalid security group request' (entirely fixed text, no variable parts).
  Actual:   Error.Code = 'ResourceNotFoundError' (labelled HTTP 404), Error.Message = 'network <networkId> does not exist' (the recorded run filled in '10000000', which is a per-run value and not part of the requirement).
  Not reached, therefore not claimed: the same check's sibling-network positive controls (GetNetwork and GetBot on the untouched control network).

Smithy evidence (services/service-com-amazonaws-wickr-wickradminapi/smithy-model/com-amazonaws-wickr-wickradminapi.json): GetSecurityGroup GET /networks/{networkId}/security-groups/{groupId}, success 200, errors BadRequestError (400), ForbiddenError (403), InternalServerError (500), RateLimitError (429), ResourceNotFoundError (404, member `message`), UnauthorizedError (401), ValidationError (422); input networkId (NetworkId, len 8..8, pattern ^[0-9]{8}$, httpLabel, required), groupId (GenericString, httpLabel, required). DeleteNetwork DELETE /networks/{networkId}, success 200, idempotent.

Emulator source (services/service-com-amazonaws-wickr-wickradminapi/src/ops/get_security_group.rs): handle() checks `store.networks.contains_key(&input.network_id)` first and returns `not_found(format!("network {} does not exist", input.network_id))`, where `not_found` constructs `ResourceNotFoundError`; the group lookup below it is unreachable for a deleted network.

## Steps to Reproduce
1. Boot the emulator from this checkout and point a botocore client at it: wickr = client('wickr', region='us-east-1').
2. network = create_network(networkName=<name>, accessLevel='STANDARD'); group_id = list_security_groups(networkId=network['networkId'])['securityGroups'][0]['id']; create_bot(networkId, username=<name ending in 'bot'>, groupId=group_id, challenge=<password satisfying the group policy>).
3. delete_network(networkId=<that network>). Poll get_network(networkId) until it is refused — it ends up ForbiddenError/'Network not found' (correct). Confirm get_bot(networkId, botId) is refused '404'/'Bot not found' (correct).
4. Call get_security_group(networkId=<deleted network>, groupId=<group_id from step 2>). Expect HTTP 401 with an unlabelled error body (botocore Error.Code '401') and Message 'invalid security group request'; observe Error.Code 'ResourceNotFoundError' with Message 'network <networkId> does not exist'.
5. Or re-replay the behavioral check: wickr.delete-network-cascades-to-bots-and-security-groups.
