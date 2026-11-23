Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2.op-contract-provision-byoip-cidr-rejected
Issue-Shape-Id: com.amazonaws.ec2#AmazonEC2
Issue-Key: ec2.op-contract-provision-byoip-cidr-rejected|run#7|AssertionError|assert err == "InvalidParameterValue", f"expected InvalidParameterValue, got {err}"
Issue-Commit: e7612e758c56c4625d86f552a9150b3dc94d2a44

# EC2 ProvisionByoipCidr: a malformed Cidr is refused MissingParameter because the CidrAuthorizationContext check runs before Cidr validation

> _Note: this issue was identified against the code base at commit `e7612e758c56c4625d86f552a9150b3dc94d2a44`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ProvisionByoipCidr

## Description
One net-new root cause.

Cause 1 of 1: ProvisionByoipCidr validates its members in the wrong order, so a request whose `Cidr` is not a CIDR at all and which also omits `CidrAuthorizationContext` is told about the omitted parameter instead of about the unparsable range. Failing check: ec2.op-contract-provision-byoip-cidr-rejected.

AWS: Smithy (services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json) declares ProvisionByoipCidr with `Cidr` @required (clientOptional) and `CidrAuthorizationContext` optional in the model; the operation declares NO modeled errors, so the refusal classes are recorded behavior only. Recorded behavior: ProvisionByoipCidr parses and validates `Cidr` FIRST. A value that is not a valid CIDR is refused with error code `InvalidParameterValue` and message `The CIDR (<cidr>) is not valid.` — fixed text `The CIDR (` + `) is not valid.`, with `<cidr>` the exact `Cidr` string taken verbatim from the request. Only a request whose `Cidr` IS a valid, provisionable range and which carries no authorization document is refused with error code `MissingParameter` and message `You must provide a CidrAuthorizationContext parameter.` (entirely fixed text, no variable part). Both refusals leave the BYOIP inventory unchanged, and DescribeByoipCidrs lists the same CIDRs before and after.

Emulator: for `Cidr="999.999.999.0/24"` with no `CidrAuthorizationContext`, the emulator answers `MissingParameter` / `You must provide a CidrAuthorizationContext parameter.` instead of `InvalidParameterValue` / `The CIDR (999.999.999.0/24) is not valid.`. The handler makes this ordering explicit and deliberate: the `input.cidr_authorization_context.as_ref()` guard returns `state::missing_parameter("You must provide a CidrAuthorizationContext parameter.")` before `state::provisionable_byoip_cidr(...)` is ever called, with a comment stating that "a request that is wrong in two ways is told about the parameter it left out rather than about the range it did send" — which is the opposite of what AWS does. The emulator already produces the right message for the genuine missing-context case and already has the correct `state::invalid_byoip_cidr(requested)` refusal for a malformed range; only the order of the two gates is wrong.

Why: a client that validates user-supplied address ranges by calling ProvisionByoipCidr (or that branches on the error code to decide whether to re-prompt for the range versus to fetch/sign an RPKI authorization document) is steered to the wrong remedy. A caller handed `MissingParameter` for `999.999.999.0/24` will go build an authorization document for a range that can never be provisioned, and retries will keep failing with the same code, so the real defect (the typo'd CIDR) is never surfaced. Error-code-driven retry/validation logic therefore diverges from real AWS on the very first bad input.

Fix: the operation's validation ordering in its handler — move the `Cidr` parse/validation gate ahead of the `CidrAuthorizationContext` presence gate so the range is judged before the authorization document is demanded, keeping both existing messages intact.

Traces: ec2.op-contract-provision-byoip-cidr-rejected — asserts `InvalidParameterValue` + `The CIDR (999.999.999.0/24) is not valid.` for the malformed range, then `MissingParameter` + `You must provide a CidrAuthorizationContext parameter.` for a valid range with no context, then that DescribeByoipCidrs is unchanged. It fails on the first assertion with `AssertionError: expected InvalidParameterValue, got MissingParameter`. Handler and the ordering comment: services/service-com-amazonaws-ec2-amazonec2/src/ops/provision_byoip_cidr.rs lines 176-236.

## Evidence
Failing check ec2.op-contract-provision-byoip-cidr-rejected:

  File "check_ec2_op_contract_provision_byoip_cidr_rejected.py", line 28, in run
    assert err == "InvalidParameterValue", f"expected InvalidParameterValue, got {err}"
  AssertionError: expected InvalidParameterValue, got MissingParameter

Expected (AWS), for ProvisionByoipCidr(Cidr="999.999.999.0/24") with no CidrAuthorizationContext:
  Error.Code    = InvalidParameterValue
  Error.Message = "The CIDR (999.999.999.0/24) is not valid."
                  (fixed text "The CIDR (" ... ") is not valid."; the parenthesised value is the request's Cidr)

Actual (emulator), same request:
  Error.Code    = MissingParameter
  Error.Message = "You must provide a CidrAuthorizationContext parameter."
                  (the message AWS reserves for a VALID range sent without an authorization document)

The second leg of the check — ProvisionByoipCidr(Cidr="198.51.100.0/24") with no CidrAuthorizationContext => MissingParameter / "You must provide a CidrAuthorizationContext parameter." — is never reached, so it is untested; the emulator's code path for it already builds that exact message.

Source of the ordering, services/service-com-amazonaws-ec2-amazonec2/src/ops/provision_byoip_cidr.rs:
  line 183: `let Some(authorization) = input.cidr_authorization_context.as_ref() else {`
  line 184: `    return Err(state::missing_parameter("You must provide a CidrAuthorizationContext parameter."))`
  line 219: `let block = match state::provisionable_byoip_cidr(requested, publicly_advertisable) {`
  line 221-222: `Err(state::ByoipCidrProblem::Malformed) => return Err(state::invalid_byoip_cidr(requested))`
The Malformed arm (the correct InvalidParameterValue refusal) sits 36 lines AFTER the MissingParameter return, so it is unreachable whenever the authorization document is also absent.

The positive control in the same check passes: DescribeByoipCidrs(MaxResults=10) answers HTTP 200 with a ByoipCidrs list, so credentials and the BYOIP read path are healthy and the divergence is confined to ProvisionByoipCidr's refusal classification.

## Steps to Reproduce
1. Point an AWS SDK/CLI client at the emulator's EC2 endpoint.
2. Positive control: `ec2.describe_byoip_cidrs(MaxResults=10)` — expect HTTP 200 with a `ByoipCidrs` list. Record the set of `Cidr` values.
3. Call `ec2.provision_byoip_cidr(Cidr="999.999.999.0/24")` with NO `CidrAuthorizationContext`.
   - AWS: ClientError with Error.Code == "InvalidParameterValue" and Error.Message == "The CIDR (999.999.999.0/24) is not valid."
   - Emulator: ClientError with Error.Code == "MissingParameter" and Error.Message == "You must provide a CidrAuthorizationContext parameter."
4. Call `ec2.provision_byoip_cidr(Cidr="198.51.100.0/24")` with NO `CidrAuthorizationContext`.
   - Both: Error.Code == "MissingParameter", Error.Message == "You must provide a CidrAuthorizationContext parameter."
   Steps 3 and 4 must NOT produce the same error code; that they do is the defect.
5. Call `ec2.describe_byoip_cidrs(MaxResults=10)` again and confirm the `Cidr` set is unchanged from step 2 (neither rejected call may leave a trace in the inventory).
6. Replay the behavioral check directly: ec2.op-contract-provision-byoip-cidr-rejected.
