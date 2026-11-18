Issue-Source: trace-sweep-divergence
Issue-Trace: ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes
Issue-Shape-Id: com.amazonaws.directoryservice#DirectoryService_20150416
Issue-Key: ds|CreateMicrosoftAD|4xx->2xx|$.Message
Issue-Key: ds|CreateMicrosoftAD|4xx->2xx|$.RequestId
Issue-Related: 20261115T135855Z-ds-createmicrosoftad-two-net-new-causes-edition-enum.md
Issue-Commit: 348a1dac96f13fd043510a8154a8a5e530b83548

# ds CreateMicrosoftAD accepts a VpcSettings subnet pick EC2 resolves nothing for, instead of refusing it with ClientException

> _Note: this issue was identified against the code base at commit `348a1dac96f13fd043510a8154a8a5e530b83548`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrosoftAD

## Description
One cause.

Cause 1 of 1: CreateMicrosoftAD answers HTTP 200 with a minted DirectoryId — and persists the directory — for a VpcSettings subnet pick AWS refuses with 400 ClientException, because the placement check refuses only what Amazon EC2 affirmatively reported and treats a subnet EC2 reported nothing for (or reported without a VPC/zone) as an undecided topology that gates nothing.

AWS: Smithy (services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json): CreateMicrosoftAD.VpcSettings is @required and targets DirectoryVpcSettings (VpcId @required, SubnetIds @required); the operation's modeled errors are ClientException (client, httpError 400, doc "A client exception has occurred."), DirectoryLimitExceededException, InvalidParameterException, ServiceException and UnsupportedOperationException; ClientException's members are Message and RequestId. SubnetIds' own documentation states the placement rule: "The identifiers of the subnets for the directory servers. The two subnets must be in different Availability Zones. Directory Service creates a directory server and a DNS server in each of these subnets." The model states no message text, so the refusal itself is recorded behavior only: AWS RESOLVES the named subnets against the service that owns subnets and, whenever the resolved topology cannot carry the directory's two servers in the named VPC, answers HTTP 400 with __type ClientException, a Message and a RequestId, and creates NO directory. The recorded step asserts status, $.__type, $.Message and $.RequestId (the latter two by shape, '<shaped-not-asserted>'), so the requirement this step carries is the status, the error code ClientException, and a Message of that refusal class. The fixed text of the sibling refusals of this class in the same recording is the invalid-subnets opener plus the rule the pick broke (e.g. "Invalid subnet ID(s). The subnets must belong to the VPC specified." / "... They must correspond to two subnets in different Availability Zones."), with " : RequestId: <request id>" appended; the request id is the only variable part and is per-run, not a requirement. A subnet identifier the account does not hold names no network in the requested VPC, so it belongs to this same refusal class.

Emulator: answers HTTP 200 with a minted DirectoryId and persists the directory. Replay of this step reports: status expected 400, got 200; $.__type missing (expected 'ClientException', got None); $.Message missing; $.RequestId missing. The cause is in the placement decision: services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/ops/create_microsoft_ad.rs calls crate::state::resolve_subnet_networks(...) and then crate::state::placement_defect(...), and src/state/subnet_zones.rs decides the placement with exactly two halves — foreign_vpc_defect, which refuses only when a resolved subnet reports a vpc_id DIFFERENT from the requested one (`.and_then(|subnet| subnet.vpc_id.as_deref()).is_some_and(|held| held != vpc_id)`), and single_zone_defect, which returns None (via `?`) the moment any named subnet has no reported availability_zone. Both halves therefore say "servable" for a subnet EC2 reported nothing for, and the module's own doc commits to that reading ("a subnet EC2 reported nothing for ... leaves that half undecided"). No guard anywhere in the crate refuses an UNRESOLVED subnet id, so the create falls through to minting and inserting. Hypothesis: this step's pick names well-formed subnet ids the account's EC2 state does not hold (its sibling step task#10, which names a subnet of another VPC, IS refused), which is precisely the case both halves leave undecided.

Why: a caller receives a Managed Microsoft AD that AWS would never have created, bound to a network topology that does not exist — so every downstream assumption about the directory's VPC (routing, the controllers security group, DNS reachability, DescribeDirectories' VpcSettings) is unsound, and the caller is never told its VpcSettings were wrong. A typo'd, stale or cross-account subnet id silently yields a persisted directory instead of an immediate 400 ClientException, and input-validation tests written against the emulator pass here and fail against AWS. The emulator's state is also left holding a directory the recording's subsequent steps do not expect.

Fix: CreateMicrosoftAD's VpcSettings placement validation — the placement decision reached from the operation's handler after the subnets are resolved and before anything is minted or inserted. The decision must refuse a placement whose named subnets the owner resolved NOTHING for (or resolved without the VPC/zone the rule needs) as the same resource-semantics class the foreign-VPC and single-zone picks get: modeled ClientException, HTTP 400, with the refusal class's fixed sentence naming the subnet pick as the invalid part of the request and the request id appended once as the only substituted value. Refusing a pick that cannot be grounded is not the same as inventing a network: an unresolvable subnet id is a fact about the request, not about EC2's silence.

Traces: ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes#task#11 (representative, the only member of this cluster).

Not covered by the open ds issues the replay names as owners of the neighbouring clusters: 20261106T210535Z-ds-createmicrosoftad-subnet-pair-refusal-sentence-is-emulator.md owns task#7/#8/#10, where a refusal IS produced with the wrong sentence, and 20261115T135855Z-ds-createmicrosoftad-two-net-new-causes-edition-enum.md owns task#1 and describes the foreign-VPC comparison that has since been added (task#10 now refuses) — neither carries the replay key for this step, which the replay reports as owned by nothing.

## Evidence
Replay of trace set simparity-wire-aws-v1 against this checkout (emulator up: /__admin/health 200, data listener accepts; 101 steps, 89 passed, 12 mismatched, score 88.1%, 5 divergence clusters). Request ids, generated directory ids and host/port values in the replay are per-run and are NOT requirements.

Cluster service=ds|CreateMicrosoftAD|status=400->200|keys=$.Message,$.RequestId,$.__type,status, verdict mismatch, affected_count 1, owned_by [] (no open issue carries this replay key), stub_path empty:
  fixture ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes, step task#11
  expected_status 400, actual_status 200
  status: expected 400, got 200
  $.__type: missing (expected 'ClientException', got None)
  $.Message: missing (expected '<shaped-not-asserted>', got None)
  $.RequestId: missing (expected '<shaped-not-asserted>', got None)
  i.e. the emulator answered a SUCCESSFUL CreateMicrosoftAD (minted DirectoryId, directory persisted) where AWS refused the placement with 400 / ClientException and created nothing.

For contrast, the sibling step task#10 of the SAME fixture (a subnet of another VPC) IS refused 400 by this checkout with Message 'Invalid subnet ID(s). The subnets must belong to the VPC specified. : RequestId: <request id>' — so the ownership half exists and only the unresolved-subnet half is missing.

Requirement decomposition for the expected refusal: HTTP status 400 and error code ClientException are fixed; the Message is the refusal class's fixed sentence (the invalid-subnets opener plus the rule a servable pick satisfies) with ' : RequestId: ' and the call's request id appended — the request id is the only variable part and is taken from the request being answered.

Smithy: CreateMicrosoftAD input VpcSettings @required -> DirectoryVpcSettings (VpcId @required, SubnetIds @required); errors include ClientException (errorKind client, httpError 400) with members Message and RequestId; SubnetIds doc states the two-subnets-in-two-AZs placement rule. The model states no message text, so the sentence is recorded behavior.

Emulator source of the gap: services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/ops/create_microsoft_ad.rs lines 248-291 (subnet_pair_defect, vpc_absence, resolve_subnet_networks, placement_vpc_id, placement_defect) and services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/subnet_zones.rs lines 46-110 — foreign_vpc_defect refuses only a subnet whose REPORTED vpc_id differs, and single_zone_defect returns None as soon as any named subnet has no reported availability_zone; no branch refuses a subnet EC2 resolved nothing for.

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus) and look at the cluster for fixture ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes, step task#11: expected 400 / ClientException, actual 200.
3. Equivalently, by hand: create a VPC with two subnets in two Availability Zones in EC2, then call ds CreateMicrosoftAD with a valid Name/Password/Edition and VpcSettings naming that VpcId together with two well-formed subnet ids the account's EC2 state does NOT hold (e.g. subnet-0123456789abcdef0 / subnet-0123456789abcdef1).
4. Observe HTTP 200 with a minted DirectoryId, and DescribeDirectories reporting a persisted directory for that placement.
5. AWS refuses that same request with HTTP 400, __type ClientException and a Message of the invalid-subnet-placement class followed by ' : RequestId: <request id>', and creates no directory — DescribeDirectories after the refusal shows no directory.
6. Re-replay ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes to confirm task#11 passes (and that task#7/#8/#10, owned by other issues, are unaffected).
