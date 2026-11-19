Issue-Severity: MEDIUM
Issue-Commit: 6a4ecefd511132c775de04942ca2f205592ed51b

# EC2 AssociateDhcpOptions classifies an id that fails EC2's id-shape check as InvalidDhcpOptionID.NotFound instead of InvalidDhcpOptionsId.Malformed

> _Note: this issue was identified against the code base at commit `6a4ecefd511132c775de04942ca2f205592ed51b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
AssociateDhcpOptions

## Description
One net-new root cause under `### ec2`; every other failing check in this replay is marked ALREADY FILED or OWNED by an open issue.

Cause 1 of 1: AssociateDhcpOptions' id-shape ("malformed") refusal class is unreachable, so a DhcpOptionsId that is NOT a readable EC2 id is answered as a lookup miss.

AWS:      recorded behavior only. The EC2 model is ec2Query and declares no error shapes for this operation (Smithy: AssociateDhcpOptions, input DhcpOptionsId -> DefaultingDhcpOptionsId @required, VpcId -> VpcId @required, DryRun; errors: []), so the code and message are the recorded AWS behavior. AWS validates the SHAPE/readability of the supplied options-set id BEFORE any lookup: an id that merely looks like `dopt-<hex>` but is not a readable EC2 id is refused with error code `InvalidDhcpOptionsId.Malformed` (note the lower-cased `Id`, not `ID`) and the message `Invalid id: "<DhcpOptionsId>"` — fixed text `Invalid id: ` followed by the id from the request, in double quotes, as the only variable part. Only an id that PASSES that shape check and then resolves to nothing is refused as a lookup miss (`InvalidDhcpOptionID.NotFound`). Hypothesis: AWS's long-form resource ids encode internal structure, so a suffix of the right length and charset but arbitrary content fails the id-shape check before a lookup is attempted — the recording states only the resulting code and message.
Emulator: answers error code `InvalidDhcpOptionID.NotFound` for such an id, i.e. it takes the ordering decision correctly in principle (services/service-com-amazonaws-ec2-amazonec2/src/ops/associate_dhcp_options.rs:170-176 refuses a non-well-formed id with `state::unreadable_dhcp_options_id` before touching the store) but its well-formedness predicate `state::dhcp_options_id_is_well_formed` accepts any value with the `dopt-` prefix and a suffix of the right length/charset. The request therefore falls through to the holds-lookup at associate_dhcp_options.rs:184-188 and is refused with `state::invalid_dhcp_option_id_not_found`. The Malformed class is consequently never produced for an unreadable id.
Why:      the two classes mean different things to a client. `InvalidDhcpOptionsId.Malformed` says the VALUE a caller passed is not an EC2 id at all (a programming/plumbing error: a truncated id, a wrong-resource id, a user-typed string) and is not worth retrying or reconciling; `InvalidDhcpOptionID.NotFound` says the id is well-formed but names nothing, which callers treat as a deleted/other-account resource and commonly handle by recreating the options set or by polling for eventual consistency. Code-keyed error handling and byte-exact message assertions on the DHCP-options path therefore take the wrong branch, and the AWS message `Invalid id: "<id>"` never appears.
Fix:      the DHCP-options id-shape predicate the AssociateDhcpOptions handler's validation calls (`state::dhcp_options_id_is_well_formed`), so that a `dopt-` value that is not a readable EC2 id is refused by `state::unreadable_dhcp_options_id` ahead of the store lookup; the not-found path and its code stay as they are for an id that IS readable but resolves to nothing.
Traces:   ec2.op-contract-associate-dhcp-options. Handler and the two refusal sites: services/service-com-amazonaws-ec2-amazonec2/src/ops/associate_dhcp_options.rs:171 (shape check) and :184 (lookup miss).

Not covered by any open or fixed issue: the fixed issue 20261103T021956Z-ec2-placement-group-dhcp-options-stale-security-group.md concerns DescribeDhcpOptions' NOT-FOUND message template, a different operation and a different refusal class, and the fixed stub issue 20261023T102725Z-ec2-associatedhcpoptions-stub.md is satisfied — this operation's happy path works in this replay (the swap and the restore both succeed and DescribeVpcs reads the new set back); only the malformed-id classification diverges.

## Evidence
Replayed failing check under `### ec2`:

  [mismatch] ec2.op-contract-associate-dhcp-options
    File "check_ec2_op_contract_associate_dhcp_options.py", line 54, in run
      assert code == "InvalidDhcpOptionsId.Malformed", f"unexpected error code: {code}"
    AssertionError: unexpected error code: InvalidDhcpOptionID.NotFound

Expected (AWS, recorded): HTTP 400 with Error/Code `InvalidDhcpOptionsId.Malformed` and Error/Message `Invalid id: "<DhcpOptionsId>"` — fixed text `Invalid id: ` plus the quoted id taken verbatim from the request.
Actual (emulator): Error/Code `InvalidDhcpOptionID.NotFound` (the lookup-miss class), so the check never reaches its message assertion for `Invalid id: "..."`.

The check's earlier steps all passed, which bounds the defect to the refusal classification: CreateDhcpOptions succeeded, AssociateDhcpOptions moved the VPC onto the new set, DescribeVpcs read the new DhcpOptionsId back, and AssociateDhcpOptions restored the original set.

The offending id in the failing run is produced by the check as a random same-length hex suffix on the `dopt-` prefix; its literal value is a per-run value and is not part of the requirement — the requirement is that ANY `dopt-` value that is not a readable EC2 id is refused as Malformed with that id echoed in the message.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite; the check is ec2.op-contract-associate-dhcp-options under `### ec2`.
2. Standalone repro against the emulator's EC2 endpoint:
   a. CreateVpc(CidrBlock='10.0.0.0/16'); wait for it to be available; note Vpcs[0].DhcpOptionsId as `original`.
   b. CreateDhcpOptions(DhcpConfigurations=[{'Key':'domain-name-servers','Values':['AmazonProvidedDNS']}]) -> `dopt-A`.
   c. AssociateDhcpOptions(DhcpOptionsId='dopt-A', VpcId=<vpc>); DescribeVpcs must report `dopt-A` (this part already works).
   d. AssociateDhcpOptions(DhcpOptionsId=`original`, VpcId=<vpc>) to restore.
   e. AssociateDhcpOptions(DhcpOptionsId='dopt-' + <random hex string of the same length as dopt-A's suffix>, VpcId=<vpc>).
3. Expected at step (e): Error Code `InvalidDhcpOptionsId.Malformed`, Message `Invalid id: "dopt-<that suffix>"`.
   Observed: Error Code `InvalidDhcpOptionID.NotFound`.
4. DescribeVpcs after (e) must still report `original` (the emulator does satisfy this — the rejected call takes no partial effect).
