Issue-Severity: MEDIUM
Issue-Corpus: measured-while-fixing
Issue-Corpus-Kind: behavioral

# cloudformation: a stack brings up the resources that do not depend on one another ONE coming-up window apart, so a create costs one window per resource instead of one per dependency level

> _Note: this report was written from measurements taken while fixing
> `20261117T213136Z-dynamodb-lambda-logs-the-two-stream-onyx-stream.md`, which it is not the
> cause of. It is filed so the measurement is not lost._

## Operation
CreateStack

## Description
Cause 1 of 1: a `CreateStack` whose template declares resources with no dependency between them
brings them up one after another, each after the previous one's coming-up window has elapsed,
instead of starting them together.

AWS: "a resource that depends on another is scheduled only once that one has finished" is the
whole of the ordering a deployment follows, so resources with no such relation between them are
brought up at the same time: a stack operation costs about one coming-up window per LEVEL of its
dependency relation, however many resources that level declares.

Emulator (measured on the assembled router): a stack declaring N independent
`AWS::DynamoDB::Table` resources and nothing else settles `CREATE_COMPLETE` in 3.03 s for
N = 1, 6.04 s for N = 2 and 9.06 s for N = 3, and the `CreationDateTime` DynamoDB records for
the three tables are 3.005 s apart — one table's whole creation window between one create and
the next. The same three tables created back to back through DynamoDB's own API all report
`ACTIVE` at 3.02 s, so the serialisation is the deployment's and not the owning service's; a
stack of three independent `AWS::Kinesis::Stream` resources settles in 575 ms against 558 ms for
one, so it is also specific to the types whose readiness read waits.

Root cause: the deployment walk (`services/service-com-amazonaws-cloudformation-cloudformation/
src/state/provisioning.rs`, `reconcile`) asks the owning service whether each just-created
resource is ready BEFORE it reaches the next declaration, and for a DynamoDB table that read is
`DescribeTable`, which answers a poll landing inside a new table's creation window only once the
creation has fallen due (`services/service-com-amazonaws-dynamodb-dynamodb-20120810/src/ops/
describe_table.rs`). So every later declaration of the same level waits one whole window behind
each earlier one.

Why: a client deploying a template of several independent resources waits a multiple of what the
deployment owes, and `CreateStack` itself does not answer until the whole walk is through, so the
cost is paid inside the call.

Fix: the walk must not wait out one resource's coming-up window before it creates the next. The
create's own answer already carries the owning service's report of the resource's state — a
table's `TableDescription.TableStatus` is `CREATING` — so the readiness wait can be opened on
that answer and released by the next progress pass instead. A companion fix is needed in the
stack's failed-create bookkeeping before that lands: with the resources on the deferred-resume
path, a resource an earlier pass created loses its `CREATE_COMPLETE` record when a later pass
fails, which `services/service-com-amazonaws-cloudformation-cloudformation/tests/all/
application_auto_scaling_resources.rs::a_failed_target_leaves_the_resources_the_stack_did_create_reported`
exposes (`StoredStack::keep_failed_resources` installs only the resources the LAST walk recorded
a transition for).

## Evidence
Expected (AWS): a stack of N independent resources costs about ONE coming-up window, so its
create does not grow with N.

Actual (emulator): 1 / 2 / 3 independent `AWS::DynamoDB::Table` resources settle in
3.03 s / 6.04 s / 9.06 s, with the N `CreateTable` calls issued 3.005 s apart.

## Steps to Reproduce
1. Boot the emulator. 2. `CreateStack` with a template declaring three `AWS::DynamoDB::Table`
resources that reference nothing of each other. 3. Poll `DescribeStacks` to a terminal status and
note how long it takes. 4. `DescribeTable` each table and compare the `CreationDateTime` values:
they are one table creation window apart rather than inside one. 5. Repeat with one table, and
with three `AWS::Kinesis::Stream` resources, for the contrast.
