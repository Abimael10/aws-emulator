Issue-Source: trace-sweep-divergence
Issue-Trace: direct-connect-gateway-lifecycle
Issue-Step: task#7
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: direct-connect-gateway-lifecycle direct-connect-validation-and-errors
Issue-Shape-Id: com.amazonaws.directconnect#OvertureService
Issue-Key: directconnect|UpdateDirectConnectGateway|2xx->2xx|$.directConnectGateway.directConnectGatewayArn
Issue-Key: directconnect|UpdateDirectConnectGateway|2xx->2xx|$.directConnectGateway.directConnectGatewayState
Issue-Key: directconnect|UpdateDirectConnectGateway|2xx->2xx|$.directConnectGateway.directConnectGatewayType
Issue-Key: directconnect|UpdateDirectConnectGateway|2xx->2xx|$.directConnectGateway.tags
Issue-Key: directconnect|DeleteDirectConnectGateway|2xx->2xx|$.directConnectGateway.directConnectGatewayArn
Issue-Key: directconnect|DeleteDirectConnectGateway|2xx->2xx|$.directConnectGateway.directConnectGatewayType
Issue-Key: directconnect|DeleteDirectConnectGateway|2xx->2xx|$.directConnectGateway.tags
Issue-Key: directconnect|DescribeTags|2xx->2xx|$.resourceTags[*].tags
Issue-Key: directconnect|DeleteDirectConnectGateway|4xx->4xx|$.message
Issue-Commit: 75e93429d3ea083e11be0578db4814848b1cd3ff

# directconnect: Direct Connect gateway mutation responses, tag reads and the missing-gateway refusal diverge

> _Note: this issue was identified against the code base at commit `75e93429d3ea083e11be0578db4814848b1cd3ff`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateDirectConnectGateway

## Description
The Direct Connect gateway surface diverges on four separate paths: the two MUTATION responses (update, delete) render an incomplete gateway record, a tag read of a tagged gateway reports no tags at all, and the refusal for a gateway id nobody holds is worded by the emulator rather than as AWS words it. This bundle has 4 causes; ALL FOUR must be fixed before it is closed.

Cause 1 of 4: UpdateDirectConnectGateway's response gateway record is incomplete and still 'pending' (fixture direct-connect-gateway-lifecycle, step task#7).
AWS: Smithy: UpdateDirectConnectGateway (input directConnectGatewayId + newDirectConnectGatewayName, both @required) -> UpdateDirectConnectGatewayResponse.directConnectGateway : DirectConnectGateway; DirectConnectGatewayState is an enum of [pending, available, deleting, deleted]. The on-disk model's DirectConnectGateway structure declares neither directConnectGatewayArn nor directConnectGatewayType, so for those two members this is recorded behavior only: the recorded 200 UpdateDirectConnectGateway body carries a directConnectGatewayArn in the Direct Connect gateway ARN form 'arn:aws:directconnect::<ownerAccount>:dx-gateway/<directConnectGatewayId>' (the account id and gateway id are values from the emulator's own state, never literal), a directConnectGatewayType of 'Default', a tags list (empty when the gateway carries no tags) and a directConnectGatewayState of 'available' for a gateway that has finished being created.
Emulator: answers 200 but $.directConnectGateway has directConnectGatewayArn missing, directConnectGatewayType missing, tags missing (absent rather than an empty list) and directConnectGatewayState 'pending'.
Why: a client that renames a gateway and then uses the returned record — to tag it, to reference it by ARN, or to branch on whether it is a Default gateway — reads null for all three, and a client that treats the mutation response as the authoritative post-rename state sees a gateway that is perpetually still being created, so a wait-for-available loop driven off the update response never terminates.
Fix: UpdateDirectConnectGateway's handler and the single stored-gateway projection it serialises through (services/service-com-amazonaws-directconnect-overtureservice/src/state.rs gateway_output) — the projection must emit the ARN assembled from the owner account and the gateway id, the gateway type, and an empty tags list for an untagged gateway; and the gateway state record must have reached 'available' by the time a rename is answered.
Traces: direct-connect-gateway-lifecycle#task#7.

Cause 2 of 4: DeleteDirectConnectGateway's response gateway record is incomplete the same way (fixture direct-connect-gateway-lifecycle, step task#9).
AWS: Smithy: DeleteDirectConnectGateway (input directConnectGatewayId @required) -> DeleteDirectConnectGatewayResult.directConnectGateway : DirectConnectGateway, i.e. the same shape the other gateway operations return; 'deleting' is documented as 'the initial state after calling DeleteDirectConnectGateway'. The recorded 200 delete body carries directConnectGatewayArn, directConnectGatewayType 'Default' and a tags list beside the transitional state (the state itself matched on replay).
Emulator: answers 200 with the correct state but with directConnectGatewayArn missing, directConnectGatewayType missing and tags missing.
Why: the delete response is the last record a client ever sees for that gateway, so a client that archives or audits the torn-down resource by ARN, or that asserts on its type/tags at teardown, loses those values entirely — and it loses them even though it never read the create or describe path.
Fix: the same shared projection as cause 1, reached from DeleteDirectConnectGateway's handler; it must emit the same member set on the delete path as on every other gateway path.
Traces: direct-connect-gateway-lifecycle#task#9.

Cause 3 of 4: DescribeTags reports no tags for a Direct Connect gateway that holds one (fixture direct-connect-gateway-lifecycle, step task#4; the cluster also covers task#6).
AWS: Smithy: DescribeTags (input resourceArns : ResourceArnList @required) -> DescribeTagsResponse.resourceTags : ResourceTagList of ResourceTag, whose members are resourceArn and tags : TagList with @length min 1 ('The tags.'). TagResource (input resourceArn + tags, both @required) is the write that puts a tag on a Direct Connect resource. The recording shows that after a gateway is tagged, DescribeTags on that gateway's ARN answers a resourceTags element carrying that tag — the key/value pair the request supplied (the recorded 'sim2real' key and its generated 'sim2real-probe-dx-gw-<suffix>' value are that run's fill-in, not part of the requirement).
Emulator: the resourceTags element resolves (its resourceArn matches) but its tags member is absent entirely, i.e. the gateway record the read projects holds no tags. The read projection itself is faithful (src/ops/describe_tags.rs renders crate::state::tags_of_resource and omits an empty list because TagList is @length 1..); what is missing is the WRITE: TagResource is still a scaffolded stub that answers NotImplemented (src/ops/tag_resource.rs, 'Err(crate::scaffolded::Scaffolded::not_implemented("TagResource"))'), so no tag ever reaches the stored gateway. The separately filed stub issue for that unimplemented operation does not carry this fixture step's replay key, so this divergence is not re-replayed by it.
Why: tagging is how clients attribute cost and drive tag-based access control. A client that tags a gateway and then reads its tags back to confirm sees an untagged resource, so an 'ensure tagged' reconciliation loop never converges and any tag-conditioned policy check evaluates against an empty tag set.
Fix: the tag write path — TagResource's handler must persist the submitted tags onto the stored Direct Connect resource record (the gateway record in crate::state), which is the record DescribeTags already projects; no change is needed in DescribeTags' own serializer.
Traces: direct-connect-gateway-lifecycle#task#4, direct-connect-gateway-lifecycle#task#6.

Cause 4 of 4: the DeleteDirectConnectGateway refusal for an id no gateway exists under is worded by the emulator, not as AWS words it (fixture direct-connect-validation-and-errors, step task#2).
AWS: Smithy: DeleteDirectConnectGateway's only client-side failure is DirectConnectClientException ('One or more parameters are not valid', HTTP 400), so the sentence in $.message is the only thing a client can branch on to tell this refusal from any other client fault on this operation. The model does not state the sentence, so this is recorded behavior only: the recorded 400 body words it 'Direct Connect Gateway for owner <ownerAccount> and with id <directConnectGatewayId> does not exist'. Its FIXED text is 'Direct Connect Gateway for owner ', ' and with id ' and ' does not exist' (note AWS's capitalised 'Gateway' and that it names the OWNER account as well as the id); its variable parts are the owner account the lookup was made for and the directConnectGatewayId the request carried — the recorded '000000000000' and '00000000-0000-0000-0000-000000000000' are one run's values and are not part of the requirement.
Emulator: answers 400 with the right error shape but the message 'Direct Connect gateway ID <directConnectGatewayId> does not exist' — AWS's owner clause is dropped, the id is introduced as 'gateway ID' instead of 'Gateway for owner ... and with id', and 'Gateway' is lower-cased. The sentence is built inline in src/ops/delete_direct_connect_gateway.rs at both of its not-found returns.
Why: a client (or a CloudFormation/Terraform provider) that matches this sentence to decide 'already deleted, treat as success' does not match it, so an idempotent delete surfaces as a hard failure; and the message no longer tells the operator which account the gateway was looked for in, which is the distinguishing detail when a gateway exists but is owned elsewhere.
Fix: DeleteDirectConnectGateway's not-found refusal path — state the sentence once beside this service's other refusal wording in crate::state (as this crate already does for its other messages) and fill it from the request's gateway id and the owner account resolved for the lookup.
Traces: direct-connect-validation-and-errors#task#2.

Related open issue that does NOT cover these: 20261109T130740Z-directconnect-direct-connect-gateway-records-omit-arn-type.md covers the same incomplete gateway projection on the CreateDirectConnectGateway and DescribeDirectConnectGateways paths and the lost rename, but it carries no replay key for the update (task#7) or delete (task#9) responses, for the tag read, or for the refusal wording.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (29 steps, 12 passed, 17 mismatched).

Cause 1 - direct-connect-gateway-lifecycle#task#7, UpdateDirectConnectGateway, expected 200 / actual 200:
  $.directConnectGateway.directConnectGatewayArn: missing (expected the dx-gateway ARN, got None)
  $.directConnectGateway.directConnectGatewayState: value_mismatch (expected 'available', got 'pending')
  $.directConnectGateway.directConnectGatewayType: missing (expected 'Default', got None)
  $.directConnectGateway.tags: missing (expected [], got None)

Cause 2 - direct-connect-gateway-lifecycle#task#9, DeleteDirectConnectGateway, expected 200 / actual 200:
  $.directConnectGateway.directConnectGatewayArn: missing (expected the dx-gateway ARN, got None)
  $.directConnectGateway.directConnectGatewayType: missing (expected 'Default', got None)
  $.directConnectGateway.tags: missing (expected [], got None)

Cause 3 - direct-connect-gateway-lifecycle#task#4 (cluster also covers task#6), DescribeTags, expected 200 / actual 200:
  $.resourceTags[0].tags: missing (expected the one tag the gateway was tagged with, got None)

Cause 4 - direct-connect-validation-and-errors#task#2, DeleteDirectConnectGateway, expected 400 / actual 400:
  $.message: value_mismatch
    expected 'Direct Connect Gateway for owner <ownerAccount> and with id <gatewayId> does not exist'
    got      'Direct Connect gateway ID <gatewayId> does not exist'

## Steps to Reproduce
1. Replay fixture direct-connect-gateway-lifecycle against the emulator.
2. Create a Direct Connect gateway, then at step task#7 call UpdateDirectConnectGateway with that directConnectGatewayId and a new newDirectConnectGatewayName; inspect $.directConnectGateway - directConnectGatewayArn, directConnectGatewayType and tags are absent and directConnectGatewayState is 'pending' instead of 'available'.
3. Step task#9: call DeleteDirectConnectGateway for that gateway and inspect $.directConnectGateway - directConnectGatewayArn, directConnectGatewayType and tags are absent.
4. Tag the gateway (TagResource with its 'arn:aws:directconnect::<account>:dx-gateway/<id>' ARN and one tag), then at steps task#4 / task#6 call DescribeTags with that ARN - $.resourceTags[0] resolves but carries no tags member.
5. Replay fixture direct-connect-validation-and-errors, step task#2: call DeleteDirectConnectGateway with a directConnectGatewayId no gateway exists under - the 400 DirectConnectClientException $.message reads 'Direct Connect gateway ID <id> does not exist' instead of AWS's 'Direct Connect Gateway for owner <account> and with id <id> does not exist'.
