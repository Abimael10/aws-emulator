Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-associate-dhcp-options
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-associate-dhcp-options|run#15|AssertionError|assert code == "InvalidDhcpOptionsId.Malformed", f"unexpected error code: {code}"
Issue-Commit: ba9169dcb3a6b6883b8d60843b0b5f765354b232

# EC2 AssociateDhcpOptions answers InvalidDhcpOptionID.NotFound for an id that fails EC2's id-shape check instead of InvalidDhcpOptionsId.Malformed

> _Note: this issue was identified against the code base at commit `ba9169dcb3a6b6883b8d60843b0b5f765354b232`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateDhcpOptions

## Description
One net-new root cause survived triage under `### ec2`; every other failing check in that section is marked ALREADY FILED or OWNED by an open issue and was skipped.

Cause 1 of 1: AssociateDhcpOptions' id-shape ("malformed") refusal class is unreachable, so a DhcpOptionsId that is not a readable EC2 id is answered as a lookup miss instead.
Failing check ids for this cause: ec2.op-contract-associate-dhcp-options

AWS:      recorded behavior only. The EC2 model is ec2Query and declares no error shapes for this operation (Smithy: AssociateDhcpOptions, input DhcpOptionsId -> DefaultingDhcpOptionsId @required, VpcId -> VpcId @required, DryRun -> Boolean; errors: []), so the status, code and message come from the recording. AWS validates the SHAPE/readability of the supplied options-set id BEFORE any lookup: a value that merely looks like `dopt-<hex>` but is not a readable EC2 id is refused HTTP 400 with Error/Code `InvalidDhcpOptionsId.Malformed` (note the lower-cased `Id`, not `ID`) and Error/Message `Invalid id: "<DhcpOptionsId>"`. In that message the fixed text is `Invalid id: ` and the only variable part is the id taken verbatim from the request, wrapped in double quotes. Only an id that PASSES the shape check and then resolves to nothing is refused as a lookup miss (`InvalidDhcpOptionID.NotFound`). Hypothesis: AWS long-form resource ids encode internal structure, so a suffix of the right length and charset but arbitrary content fails the id-shape check before any lookup is attempted — the recording states only the resulting code and message, not the rule.
Emulator: answers Error/Code `InvalidDhcpOptionID.NotFound` for such an id, so the Malformed class is never produced and the `Invalid id: "..."` message never appears. The ordering is right in principle (the handler refuses a non-well-formed id before touching the store) but the well-formedness predicate accepts any value with the `dopt-` prefix and a suffix of the right length/charset, so the request falls through to the holds-lookup and is refused as not found. The check's earlier steps all passed, which bounds the defect to the refusal classification: CreateDhcpOptions succeeded, AssociateDhcpOptions moved the VPC onto the new set, DescribeVpcs read the new DhcpOptionsId back, and the original set was restored.
Why:      the two classes mean different things to a client. `InvalidDhcpOptionsId.Malformed` says the VALUE passed is not an EC2 id at all (a truncated id, a wrong-resource id, a user-typed string) and is not worth retrying or reconciling. `InvalidDhcpOptionID.NotFound` says the id is well-formed but names nothing, which callers treat as a deleted or other-account resource and commonly handle by recreating the options set or by polling for eventual consistency. Code-keyed error handling on the DHCP-options path therefore takes the wrong branch, and byte-exact message assertions never see AWS's wording.
Fix:      the DHCP-options id-shape predicate that the AssociateDhcpOptions handler's validation calls, so that a `dopt-` value which is not a readable EC2 id is refused as Malformed ahead of the store lookup; the not-found path and its code stay as they are for an id that IS readable but resolves to nothing.
Traces:   ec2.op-contract-associate-dhcp-options. Handler: services/service-com-amazonaws-ec2-amazonec2/src/ops/associate_dhcp_options.rs (shape-check refusal site, then the lookup-miss refusal site).

Already-filed issues named for context, none of which covers this failure (the suite reports the check as neither ALREADY FILED nor OWNED, i.e. no open issue carries its current replay key): open issue 20261119T190833Z-ec2-associatedhcpoptions-classifies-an-id-that-fails-ec2.md describes the same classification defect but does not carry this check's replay key, so it did not own the failure in this replay; open issues 20261108T191001Z-ec2-describesubnets-omits-dhcp-option-members-and-describeroutetables-mislabels.md and 20261115T223640Z-ec2-describesubnets-omits-dhcpoptionssource-on-every-subnet.md concern DescribeSubnets read-shape gaps, a different operation and no refusal class.

## Evidence
Replayed failing check under `### ec2`:

  [mismatch] ec2.op-contract-associate-dhcp-options
    File "check_ec2_op_contract_associate_dhcp_options.py", line 54, in run
      assert code == "InvalidDhcpOptionsId.Malformed", f"unexpected error code: {code}"
    AssertionError: unexpected error code: InvalidDhcpOptionID.NotFound

Expected (AWS, recorded): HTTP 400, Error/Code `InvalidDhcpOptionsId.Malformed`, Error/Message `Invalid id: "<DhcpOptionsId>"` — fixed text `Invalid id: ` plus the quoted id copied from the request.
Actual (emulator): Error/Code `InvalidDhcpOptionID.NotFound`; the check never reaches its message assertion, so the `Invalid id: "..."` message is not produced at all.

The offending id is generated by the check as a random same-length hex suffix on the `dopt-` prefix. Its literal value is a per-run value and is NOT part of the requirement: the requirement is that ANY `dopt-` value which is not a readable EC2 id is refused as Malformed with that same id echoed in the message.

The rejected call correctly takes no partial effect — DescribeVpcs after the refusal still reports the VPC's original DhcpOptionsId — so only the classification and message diverge.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite; the failing check is ec2.op-contract-associate-dhcp-options under `### ec2`.
2. Standalone repro against the emulator's EC2 endpoint:
   a. CreateVpc(CidrBlock='10.0.0.0/16'); wait until available; note Vpcs[0].DhcpOptionsId as `original`.
   b. CreateDhcpOptions(DhcpConfigurations=[{'Key':'domain-name-servers','Values':['AmazonProvidedDNS']}]) -> `dopt-A`.
   c. AssociateDhcpOptions(DhcpOptionsId='dopt-A', VpcId=<vpc>); DescribeVpcs must report `dopt-A` (this already works).
   d. AssociateDhcpOptions(DhcpOptionsId=`original`, VpcId=<vpc>) to restore.
   e. AssociateDhcpOptions(DhcpOptionsId='dopt-' + <random hex string of the same length as dopt-A's suffix>, VpcId=<vpc>).
3. Expected at (e): HTTP 400, Error/Code `InvalidDhcpOptionsId.Malformed`, Error/Message `Invalid id: "dopt-<that suffix>"`.
   Observed at (e): Error/Code `InvalidDhcpOptionID.NotFound`.
4. DescribeVpcs after (e) must still report `original` (the emulator already satisfies this).
