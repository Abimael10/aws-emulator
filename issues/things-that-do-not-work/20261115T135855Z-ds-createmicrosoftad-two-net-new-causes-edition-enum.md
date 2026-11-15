Issue-Source: trace-sweep-divergence
Issue-Trace: ds-madl-traj-create-microsoft-ad-request-mistakes
Issue-Step: task#1
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ds-madl-traj-create-microsoft-ad-request-mistakes ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes
Issue-Shape-Id: com.amazonaws.directoryservice#DirectoryService_20150416
Issue-Key: ds|CreateMicrosoftAD|4xx->4xx|$.message
Issue-Key: ds|CreateMicrosoftAD|4xx->2xx|$.__type
Issue-Key: ds|CreateMicrosoftAD|4xx->2xx|status
Issue-Related: 20261103T100504Z-ds-directory-service-certificate-and-directory-lookups-unvalidated-shortname.md
Issue-Commit: e35babe621893e5087518213f3a8eab658fbcbae

# ds CreateMicrosoftAD: two net-new causes — Edition enum-value-set clause is mis-rendered and a VpcSettings subnet pick AWS refuses is accepted

> _Note: this issue was identified against the code base at commit `e35babe621893e5087518213f3a8eab658fbcbae`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrosoftAD

## Description
TWO net-new root causes in CreateMicrosoftAD's request refusal path: the request that is refused is refused with the wrong sentence, and a request AWS refuses outright is accepted and creates a directory. Both are in the same operation's validation of the same input (Edition, VpcSettings) but they are DISTINCT defects in DISTINCT components and a fixer must close BOTH before submitting.

Cause 1 of 2: the ValidationException for an Edition value outside the modeled enum renders the enum value set with the wrong members in the wrong order.
AWS: this error class's message template is '<n> validation error(s) detected: Value '<value>' at '<memberPath>' failed to satisfy constraint: Member must satisfy enum value set: [<value set>]', HTTP 400 ValidationException. Fixed text is everything outside the variable parts; the variable parts are <n> (the number of violations detected), <value> (the rejected value, verbatim from the request), <memberPath> (the lowercase input-member path, here 'edition') and <value set> (the enum values of the member's target shape, as the SERVICE enumerates them). Smithy (services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json): CreateMicrosoftAD.Edition targets the DirectoryEdition enum; the checked-in model lists its values as Enterprise, Standard, Hybrid. The model does not state the clause's rendering, so the value set AWS prints is recorded behavior only: for this member AWS prints exactly '[Enterprise, EnterpriseHybrid, Standard]' — three values, ascending lexicographic order, with the hybrid value spelled EnterpriseHybrid (the live service's spelling; the checked-in model's 'Hybrid' is behind it). Hypothesis: the clause is ordered by sorting the value set, not by model declaration order.
Emulator: answers HTTP 400 with the right code and the right fixed text and the right variable <value>/<memberPath>, but prints the value set as '[Standard, Hybrid, Enterprise]' — model declaration-ish order rather than AWS's ordering, and the hybrid value spelled 'Hybrid' rather than 'EnterpriseHybrid'. Full observed message: "1 validation error detected: Value 'standard' at 'edition' failed to satisfy constraint: Member must satisfy enum value set: [Standard, Hybrid, Enterprise]" where AWS words it "... Member must satisfy enum value set: [Enterprise, EnterpriseHybrid, Standard]".
Why: an error message is a requirement of the error class. A client, SDK conformance test or CLI user that asserts on AWS's refusal sentence never matches, and anything that PARSES the bracketed set to discover the admissible values of Edition is told the member accepts 'Hybrid' (a value AWS's own validator does not name) and is never told about 'EnterpriseHybrid'. Because the clause is built by the shared renderer, every ds operation that refuses an enum member diverges the same way, not just this call.
Fix: the crate's shared input-validation layer — the centralized deserialize -> validate -> handle validate step that formats a constraint violation into the ValidationException message (the renderer feeding crate::scaffolded::Scaffolded::validation), specifically the branch that builds the 'Member must satisfy enum value set: [...]' clause: it must emit the service's value set in AWS's order and with AWS's spelling of each value. The CreateMicrosoftAD handler is not reached and needs no change for this cause. (Open issue 20261115T013046Z-ds-createmicrosoftad-s-validationexception-for-a-list-member.md reports a different branch of this same renderer — a dense LIST member's 'Member must satisfy constraint: [...]' clause set — and carries no enum clause and no wire replay key for this step, so it does not cover this cause.)
Traces: ds-madl-traj-create-microsoft-ad-request-mistakes#task#1 (representative).

Cause 2 of 2: a VpcSettings subnet pick AWS refuses with ClientException is ACCEPTED and creates a directory.
AWS: Smithy: CreateMicrosoftAD.VpcSettings is @required and targets DirectoryVpcSettings (VpcId @required @pattern ^(vpc-[0-9a-f]{8}|vpc-[0-9a-f]{17})$, SubnetIds @required); the operation models ClientException (client, httpError 400, 'A client exception has occurred.'), whose members are Message and RequestId. SubnetIds' own documentation states the placement rule: 'The identifiers of the subnets for the directory servers. The two subnets must be in different Availability Zones. Directory Service creates a directory server and a DNS server in each of these subnets.' Recorded behavior: the service RESOLVES the named subnets against the service that owns subnets and refuses — HTTP 400, __type ClientException, no directory created — whenever the resolved topology cannot carry the directory's servers in the named VPC; the recorded step asserts status, __type, Message and RequestId by shape. The sibling refusal of this family is worded 'Invalid subnet ID(s). Both subnets must belong to the specified VPC.' with ' : RequestId: <request id>' appended, the request id being the only variable part.
Emulator: answers HTTP 200 with a minted DirectoryId and persists the directory. src/ops/create_microsoft_ad.rs refuses only (a) a settings block that names no two distinct subnets (InvalidParameterException via state::subnet_pair_defect), (b) a VpcId the EC2 state does not hold (ClientException via state::vpc_absence) and (c) a pair EC2 reports in ONE Availability Zone (ClientException via state::single_zone_defect). It never compares the VPC the resolved subnets belong to (state::placement_vpc_id over state::resolve_subnet_networks, which it already computes) against input.vpc_settings.vpc_id, and state/subnet_zones.rs single_zone_defect returns None for any subnet EC2 reported nothing for — so a subnet owned by another VPC, or one that resolves to nothing at all, passes unvalidated. No ClientException is produced and the expected $.__type / $.Message / $.RequestId members are absent from the 200 body.
Why: the caller gets a Managed Microsoft AD placed in a network topology AWS rejects, so every downstream assumption about its VPC (routing, the controllers security group, DNS reachability) is unsound, and the caller is never told its VpcSettings were wrong. Input-validation tests written against the emulator pass and fail against AWS, and a typo'd or stale subnet id silently yields a directory bound to nothing instead of an immediate 400.
Fix: CreateMicrosoftAD's VpcSettings validation in the operation's handler, after the VPC is resolved and before anything is minted or inserted: compare the VPC the resolved subnets belong to against the requested VpcId and refuse with the modeled ClientException (AWS's fixed sentence for the class plus the request id), and treat a subnet the owner resolved nothing for as a refusal of the same class rather than as an unknown topology that gates nothing.
Traces: ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes#task#10 and #task#11 (the two members of one cluster). Source of the missing guard: services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/ops/create_microsoft_ad.rs lines 239-281 (subnet_pair_defect, vpc_absence, resolve_subnet_networks, placement_vpc_id, single_zone_defect) and services/service-com-amazonaws-directoryservice-directoryservice-20150416/src/state/subnet_zones.rs lines 43-59. Not covered by the open wire issue 20261106T210535Z-ds-createmicrosoftad-subnet-pair-refusal-sentence-is-emulator.md (that one owns task#7/#8, where a refusal IS produced with the wrong sentence); the open behavioral issue 20261102T102235Z-ds-ec2-seven-net-new-root-causes-createmicrosoftad.md describes the same missing cross-service resolution for behavioral check ids but carries NO wire replay key for these steps, so it does not own them.

## Evidence
Replay of trace set simparity-wire-aws-v1 against this checkout (emulator up: /__admin/health 200; 101 steps, 82 passed, score 81.2%). Request ids, generated directory ids and host/port values in the replay are per-run and are NOT requirements.

Cause 1 — cluster service=ds|CreateMicrosoftAD|status=400->400|keys=$.message, verdict mismatch, affected_count 1:
  fixture ds-madl-traj-create-microsoft-ad-request-mistakes, step task#1
  expected_status 400, actual_status 400 (status and error class agree)
  $.message: value_mismatch
    expected: "1 validation error detected: Value 'standard' at 'edition' failed to satisfy constraint: Member must satisfy enum value set: [Enterprise, EnterpriseHybrid, Standard]"
    actual:   "1 validation error detected: Value 'standard' at 'edition' failed to satisfy constraint: Member must satisfy enum value set: [Standard, Hybrid, Enterprise]"
  Fixed text: "<n> validation error detected: Value '", "' at '", "' failed to satisfy constraint: Member must satisfy enum value set: [", "]".
  Variable text: the rejected value ('standard', from the request), the member path ('edition'), and the value set — which for this member is the service's own enumeration, printed by AWS as 'Enterprise, EnterpriseHybrid, Standard'.
  Smithy: CreateMicrosoftAD.Edition -> DirectoryEdition enum, values Enterprise | Standard | Hybrid (services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json). The model states no message text, so the clause's order and the 'EnterpriseHybrid' spelling are recorded behavior.

Cause 2 — cluster service=ds|CreateMicrosoftAD|status=400->200|keys=$.Message,$.RequestId,$.__type,status, verdict mismatch, affected_count 2:
  fixture ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes, steps task#10 and task#11
  status: expected 400, got 200
  $.__type: missing (expected 'ClientException', got None)
  $.Message: missing (expected '<shaped-not-asserted>', got None)
  $.RequestId: missing (expected '<shaped-not-asserted>', got None)
  i.e. the emulator answered a successful CreateMicrosoftAD (minted DirectoryId, directory persisted) where AWS refused the placement with 400 ClientException.
  Smithy: VpcSettings @required (DirectoryVpcSettings: VpcId @required, SubnetIds @required); errors include ClientException (client, httpError 400) with members Message + RequestId; SubnetIds doc states the two-subnets-in-two-AZs placement rule.
  Emulator source of the gap: src/ops/create_microsoft_ad.rs computes placement_vpc_id(&subnet_networks) but uses it only to place the controllers security group — it is never compared with input.vpc_settings.vpc_id, and src/state/subnet_zones.rs:35-58 documents that 'a subnet EC2 reported nothing for ... refuses nothing'. No ownership refusal and no unresolved-subnet refusal exist in the crate (no 'Both subnets must belong to the specified VPC' sentence anywhere in src/state).

## Steps to Reproduce
1. git_clone() the repo and boot the emulator built from that checkout.
2. Replay the wire corpus (replay_wire_corpus) and look at ds-madl-traj-create-microsoft-ad-request-mistakes#task#1 and ds-madl-traj-create-microsoft-ad-subnet-pick-mistakes#task#10 (and #task#11).
3. Cause 1 by hand: call ds CreateMicrosoftAD with a valid Name/Password/VpcSettings and Edition='standard' (a value outside the modeled enum). Observe HTTP 400 ValidationException whose message ends 'Member must satisfy enum value set: [Standard, Hybrid, Enterprise]'; AWS ends it 'Member must satisfy enum value set: [Enterprise, EnterpriseHybrid, Standard]'.
4. Cause 2 by hand: create two VPCs in EC2, each with subnets; call ds CreateMicrosoftAD with a valid Name/Password/Edition and VpcSettings naming VpcId of the first VPC together with SubnetIds that do not belong to it (e.g. one subnet from each VPC, or two well-formed subnet ids EC2 does not hold). Observe HTTP 200 with a minted DirectoryId and a persisted directory.
5. AWS refuses that same request with HTTP 400, __type ClientException and a Message naming the subnet/VPC ownership rule followed by ' : RequestId: <request id>', and creates no directory — confirm with DescribeDirectories that no directory exists after the refusal.
6. Re-replay both fixtures above to confirm both causes are closed.
