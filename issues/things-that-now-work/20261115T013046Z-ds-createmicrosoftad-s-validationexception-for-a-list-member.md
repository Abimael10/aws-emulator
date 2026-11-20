Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ds.create-microsoft-ad-simultaneous-modeled-pattern-violations-ordered-in-one-validation-exception
Issue-Shape-Id: com.amazonaws.directoryservice#DirectoryService_20150416
Issue-Key: ds.create-microsoft-ad-simultaneous-modeled-pattern-violations-ordered-in-one-validation-exception|run#15|AssertionError|assert msg2 == (
Issue-Commit: 3cd41981ee722cd642660e763820306da1bf304f

# ds: CreateMicrosoftAD's ValidationException for a list-member @pattern violation omits the "Member must not be null" clause from the member-constraint set (vpcSettings.subnetIds)

> _Note: this issue was identified against the code base at commit `3cd41981ee722cd642660e763820306da1bf304f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateMicrosoftAD

## Description
ONE net-new root cause in the Directory Service (ds) control plane, grounded in the single failing check under `### ds` that is neither ALREADY FILED nor OWNED by an open issue.

Cause 1 of 1: when a request violates the modeled @pattern of a LIST MEMBER, the emulator's ValidationException message renders the member-constraint set with only the pattern clause and drops the dense-list non-null clause, so the bracketed constraint list is one clause short of AWS's sentence.

AWS: the message template for this error class is `<n> validation errors detected: <violation>[; <violation>...]`, and for a member of a dense (non-@sparse) list each violation reads `Value '[<joined values>]' at '<memberPath>' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: <pattern>, Member must not be null]`. Fixed text is everything outside the variable parts; the variable parts are `<n>` (number of detected violations), `<joined values>` (the request's list entries joined with ", "), `<memberPath>` (the lowercase input-member path, here `vpcSettings.subnetIds`) and `<pattern>` (the Smithy @pattern string VERBATIM, anchors included). The bracketed clause list is a property of the TARGET SHAPE, not of the request: because the list is dense, AWS's framework validator always reports BOTH member constraints it carries — the pattern and `Member must not be null` — in that order. Smithy (services/service-com-amazonaws-directoryservice-directoryservice-20150416/smithy-model/com-amazonaws-directoryservice-directoryservice-20150416.json): `CreateMicrosoftAD.VpcSettings` targets `DirectoryVpcSettings`; `DirectoryVpcSettings$SubnetIds` (required) targets `SubnetIds`, a list with NO @sparse trait whose member targets `SubnetId` with @pattern `^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$`; `DirectoryVpcSettings$VpcId` (required) targets `VpcId` with @pattern `^(vpc-[0-9a-f]{8}|vpc-[0-9a-f]{17})$`. Error: ValidationException, HTTP 400.

Emulator: HTTP 400 ValidationException with the correct error code, the correct violation COUNT (`2 validation errors detected: `), the correct ordering (vpcId's violation before subnetIds'), the correct value quoting and the correct anchored patterns — but the subnetIds violation ends `... Member must satisfy constraint: [Member must satisfy regular expression pattern: ^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$]` where AWS ends `... [Member must satisfy regular expression pattern: ^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$, Member must not be null]`. The scalar (non-list) legs of the same check pass — the password+name pair aggregates and orders exactly as AWS does — so the divergence is isolated to how the shared validator renders the constraint set of a LIST member, not to aggregation, ordering, counting or pattern rendering.

Why: an HTTP status, an error code and an error message are all requirements of the error class. A client, SDK conformance test or CLI user that asserts on AWS's refusal sentence for a malformed subnet id never matches, and anything that parses the bracketed clause list to learn which constraints a list member carries concludes the list is @sparse (nullable members allowed) when AWS rejects a null entry. Because the clause is omitted structurally, EVERY ds operation that validates a dense list of pattern-constrained strings (VpcSettings.SubnetIds on CreateDirectory/CreateMicrosoftAD/CreateHybridAD, AddIpRoutes' routes, DirectoryIds lists, TagKeys) diverges the same way, not just this one call.

Fix: the shared input-validation layer of this crate — the centralized `deserialize -> validate -> handle` validate step that formats a constraint violation into the ValidationException message (the renderer feeding `crate::scaffolded::Scaffolded::validation`), specifically the branch that builds a collection member's `Member must satisfy constraint: [...]` clause list. It must append `Member must not be null` after the pattern clause whenever the target list is not @sparse. No change to the operation handler is needed; the handler is never reached.

Related-but-not-covering open issue: 20261109T135909Z-ds-3-net-new-causes-every-pattern-validationexception.md reports the same formatter's ANCHOR stripping on scalar members (its Cause 1); the pattern in this check's observed message is now anchored, and that issue carries none of this check's replay keys nor any list-member path, so it does not cover this cause.

Traces: ds.create-microsoft-ad-simultaneous-modeled-pattern-violations-ordered-in-one-validation-exception.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout: Total 148/180 passed; ds 131/157 (26 failed). The service answers and the refusal is correctly classified, so this is a fidelity divergence in the message, not a bootstrap gap. Generated names, directory ids, request ids and timestamps in the replay are per-run values and are NOT requirements.

ds.create-microsoft-ad-simultaneous-modeled-pattern-violations-ordered-in-one-validation-exception [mismatch]:
  File "check_ds_create_microsoft_ad_simultaneous_modeled_pattern_violations_ordered_in_one_validation_exception.py", line 92, in run
    assert msg2 == (...)
  AssertionError: both violations should be reported together, vpcId's before subnetIds'

Request: CreateMicrosoftAD(Name='<label>.test', Password='Sim2Real#Pass99', Edition='Standard', VpcSettings={VpcId:'bad-vpc-id', SubnetIds:['bad-subnet-1','bad-subnet-2']}).

Expected (AWS), one message, both violations, vpcId's first:
  "2 validation errors detected: Value 'bad-vpc-id' at 'vpcSettings.vpcId' failed to satisfy constraint: Member must satisfy regular expression pattern: ^(vpc-[0-9a-f]{8}|vpc-[0-9a-f]{17})$; Value '[bad-subnet-1, bad-subnet-2]' at 'vpcSettings.subnetIds' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: ^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$, Member must not be null]"

Observed (emulator), identical except the final clause is missing:
  "2 validation errors detected: Value 'bad-vpc-id' at 'vpcSettings.vpcId' failed to satisfy constraint: Member must satisfy regular expression pattern: ^(vpc-[0-9a-f]{8}|vpc-[0-9a-f]{17})$; Value '[bad-subnet-1, bad-subnet-2]' at 'vpcSettings.subnetIds' failed to satisfy constraint: Member must satisfy constraint: [Member must satisfy regular expression pattern: ^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$]"

Delta: the literal text ", Member must not be null" before the closing "]" of the subnetIds member-constraint list. Everything else in the sentence matched — code ValidationException, status 400, the count "2 validation errors detected", the "; " join, the vpcId-before-subnetIds order, the quoted request values and both anchored patterns.

Scope evidence from the same check: the earlier leg (bad Password + single-label Name, both @sensitive/scalar members) asserted "2 validation errors detected: Value at 'password' failed to satisfy constraint: Member must satisfy regular expression pattern: <password pattern>; Value '<name>' at 'name' failed to satisfy constraint: Member must satisfy regular expression pattern: ^([a-zA-Z0-9]+[\\.-])+([a-zA-Z0-9])+$" and PASSED (the check failed at line 92, the second leg), proving aggregation, fixed ordering, the sensitive-value elision and anchored pattern rendering are all already correct and only the list-member clause set diverges.

Smithy confirmation that the missing clause is required: SubnetIds = {"type": "list", "member": {"target": "SubnetId", "pattern": "^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$"}} with no @sparse trait — a dense list, so its member carries both the pattern constraint and the implicit non-null constraint AWS enumerates.

## Steps to Reproduce
1. git_clone(), then run_behavioral_suite(); ds answers 131/157 checks, so this is a fidelity divergence rather than a bootstrap gap.
2. Call CreateMicrosoftAD with a VALID Name (at least two labels, e.g. '<label>.test'), a compliant Password, Edition='Standard', and VpcSettings={VpcId:'bad-vpc-id', SubnetIds:['bad-subnet-1','bad-subnet-2']} — i.e. both VpcSettings members simultaneously breaking their modeled @pattern.
3. Expect HTTP 400 ValidationException whose message is "2 validation errors detected: " followed by the vpcId violation, then "; ", then the subnetIds violation ending "... Member must satisfy constraint: [Member must satisfy regular expression pattern: ^(subnet-[0-9a-f]{8}|subnet-[0-9a-f]{17})$, Member must not be null]".
4. Observe the same message with the trailing ", Member must not be null" clause absent from the bracketed member-constraint list.
5. Same request with ALL FOUR pattern members broken (single-label Name, one-character-class Password, bad VpcId, bad SubnetIds) must answer "4 validation errors detected: <password>; <vpcId>; <subnetIds>; <name>" with the subnetIds violation carrying both clauses.
6. Control: the all-valid request (good Name, good Password, well-formed but nonexistent vpc-/subnet- ids) must clear validation and be refused ClientException "Invalid VPC ID. : RequestId: <rid>" instead — confirming the refusals above are per-member, not blanket.
7. Re-replay ds.create-microsoft-ad-simultaneous-modeled-pattern-violations-ordered-in-one-validation-exception.
