Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle
Issue-Key: imagebuilder|ListDistributionConfigurations|NotImplemented
Issue-Commit: 4bb76b5ad778511f4e613dac77ae9e1eefd9f644

# Implement imagebuilder ListDistributionConfigurations: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `4bb76b5ad778511f4e613dac77ae9e1eefd9f644`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListDistributionConfigurations

## Description
`ListDistributionConfigurations` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/list_distribution_configurations.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `ListDistributionConfigurations` call, so each one unblocks only when `ListDistributionConfigurations` works: `ec2__iam__imagebuilder.ibp-e2e-ec2-image-builder-pipeline-lifecycle`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-imagebuilder-imagebuilder/src/ops/list_distribution_configurations.rs`. Replaying the corpus, the emulator refused the `ListDistributionConfigurations` call: File "contextlib.py", line 158, in __exit__
    self.gen.throw(value)
  File "check_ec2__iam__imagebuilder_ibp_e2e_ec2_image_builder_pipeline_lifecycle.py", line 331, in phase
    raise _PhaseError(
_simparity_dyn_check_ec2__iam__imagebuilder_ibp_e2e_ec2_image_builder_pipeline_lifecycle_0a9b4ac6._PhaseError: [phase 2/6 pre_invoke] ClientError: An error occurred (NotImplemented) when calling the ListDistributionConfigurations operation: operation ListDistributionConfigurations is scaffolded but not implemented in this emulator

## Steps to Reproduce
Boot the imagebuilder emulator
Call ListDistributionConfigurations and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal
