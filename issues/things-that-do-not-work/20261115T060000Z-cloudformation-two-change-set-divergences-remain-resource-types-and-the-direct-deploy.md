Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudformation.change-set-resource-types-excluding-a-template-type-rejected cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set
Issue-Shape-Id: com.amazonaws.cloudformation#CloudFormation
Issue-Key: cloudformation.change-set-resource-types-excluding-a-template-type-rejected|_error#16|AssertionError|raise AssertionError(f"expected a refusal, the call succeeded: {result!r}")
Issue-Key: cloudformation.cdk-deploy-method-direct-creates-and-updates-without-a-change-set|_err#18|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Commit: 765add48a71cdb59193040bf215c1324536972c5

# cloudformation: two change-set divergences remain — the ResourceTypes bound on CreateChangeSet, and a change set a direct deploy's check still finds

> _Carried over from
> `issues/things-that-now-work/20261102T101202Z-cloudformation-8-net-new-root-causes-in-the.md`
> (causes 4 and 8 of eight), whose other six causes are resolved. The two keys above are this
> issue's blocking evidence. What follows is what the earlier fix established, so the next
> attempt does not repeat it._

## Operation
CreateChangeSet

## Description

Cause 1 of 2: the `ResourceTypes` bound on `CreateChangeSet` is enforced, and the check still
diverges on a LATER call of the same fixture.

What the emulator does now: `CreateChangeSet` weighs the stated `ResourceTypes` against the
template the plan would deploy and refuses a body declaring an uncovered type with
`ValidationError` "Resource type <type> is not allowed by parameter ResourceTypes [<list>]",
before the change set (and, for a `CREATE` change set, before the stack) is written — the same
rule, wording and code `CreateStack` and `UpdateStack` already answer, whose own recorded
checks pass with it.

What the replay shows, in the order the variants were tried:

* with that rule, the fixture gets past its own `_error#16` expectation and then raises an
  UNCAUGHT `ClientError` at a LATER `CreateChangeSet` whose stated list is exactly
  `[AWS::CloudFormation::WaitConditionHandle]` and whose template/plan carries an
  `AWS::SSM::Parameter`. So the fixture has at least two change sets (the response of the
  refused one names `…-cs1-…`), the first of which AWS refuses and a later of which AWS
  ACCEPTS with the same one-entry list over a template that declares an uncovered type.
* narrowing the rule to the resources the PLAN acts on (add/modify/remove/import), then to
  add/modify/import, then to additions only, leaves that later call refused — so the later
  call's plan really does add an `AWS::SSM::Parameter` in this emulator.
* restricting the rule to a change set that CREATES its stack lets the `_error#16` call
  through, so that call is an UPDATE change set over an existing stack.
* answering the modelled `InsufficientCapabilitiesException` instead gets the fixture past
  `_error#16` and then fails a `run#17` tuple comparison, which prints
  `('InsufficientCapabilitiesException', 'Resource type AWS::SSM::Parameter is not allowed by
  parameter ResourceTypes [AWS::CloudFormation::WaitConditionHandle]')`.

No rule over the submitted template or over the computed plan refuses the first call and admits
the second, so what differs is something else about the later call (its type, its template
source, or the state the fixture had reached by then). The next attempt needs the fixture's own
calls: the recorded `(Code, Message)` pair of the refusal and the arguments of each
`CreateChangeSet` it makes.

Cause 2 of 2: a fixture that deploys with `cdk deploy --method=direct` still finds a change set
under the deploy's own name.

AWS: recorded behavior only — the fixture's `DescribeChangeSet` for that name is refused with
`ChangeSetNotFound` (Smithy: `ChangeSetNotFoundException`, httpError 404, awsQueryError
`ChangeSetNotFound`).
Emulator: `DescribeChangeSet` answers 200 with a change set whose `ChangeSetName` is the
fixture's own label.

What was established: the direct path synthesises NOTHING. `CreateChangeSet` (and the nested
expansion it drives) is the only writer of a change-set record in the crate — `grep` for
`change_sets.push`/`extend` finds it in `ops/create_change_set.rs` alone — and
`change_set_bounds_and_property_values::a_direct_update_reports_the_cleanup_phase_too` pins,
end to end over the wire, that a `CreateStack` + `UpdateStack` deploy leaves `DescribeChangeSet`
answering `ChangeSetNotFound` and `ListChangeSets` empty for both the stack's own name and
`cdk-deploy-change-set`. The reclamation paths are in place too: a change set of a stack whose
deletion completed is dropped (`state.rs`'s `reclaim_orphaned_change_sets`), and
`DeleteChangeSet` discards the plan of an executed change set so every read answers the modelled
not-found (`ops/delete_change_set.rs`). So the record the fixture finds was created by a
`CreateChangeSet` the fixture itself makes, and the divergence is in whatever the recorded run
then did with it (a delete the emulator refused or no-opped, or a branch the recorded run took
because ITS change set was empty where this emulator's was not).

## Evidence
Cause 1: 'ClientError: An error occurred (ValidationError) when calling the CreateChangeSet
operation: Resource type AWS::SSM::Parameter is not allowed by parameter ResourceTypes
[AWS::CloudFormation::WaitConditionHandle]' raised outside the fixture's own refusal
expectation.
Cause 2: 'expected the call to be refused, but it succeeded: {ChangeSetName:
<fixture label>, ChangeSetId: arn:…:changeSet/<fixture label>/<uuid>, StackId: …}'.
Ids, names and uuids above are per-run values filling the template.

## Steps to Reproduce
Replay the checks in trace_ids: 1) `CreateChangeSet` with `ResourceTypes` excluding a type the
template declares, then the fixture's later change set under the same list; 2) `cdk deploy
--method=direct` then `DescribeChangeSet` for the CDK change-set name.
