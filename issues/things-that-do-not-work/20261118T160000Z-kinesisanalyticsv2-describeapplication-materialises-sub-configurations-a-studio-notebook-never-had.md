Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesisanalyticsv2.studio-notebook-defaults-disable-checkpoints-snapshots-and-autoscaling-unlike-a-flink-app
Issue-Shape-Id: com.amazonaws.kinesisanalyticsv2#KinesisAnalytics_20180523
Issue-Key: kinesisanalyticsv2.studio-notebook-defaults-disable-checkpoints-snapshots-and-autoscaling-unlike-a-flink-app|run#16|AssertionError|assert present == [], f"the Studio notebook describes blocks it never had: {present!r}"

# DescribeApplication materialises sub-configuration descriptions the request never supplied

## Operation
DescribeApplication

## Description
An application created with no `ApplicationSystemRollbackConfiguration` and no
`ApplicationEncryptionConfiguration` is nonetheless described with an
`ApplicationSystemRollbackConfigurationDescription` and an
`ApplicationEncryptionConfigurationDescription`.

AWS: for a Studio notebook created with no such configuration, the recorded
`DescribeApplication` answer carries NEITHER block — the description reports the
sub-configurations the application actually has, rather than one entry per
sub-configuration the model declares.

Emulator: `crate::state::complete_with_service_defaults` materialises both blocks for
every application, so both are reported whatever the create supplied.

Why it matters: a client that branches on the PRESENCE of a description block (to decide
whether system rollback was ever configured, or whether the application has an encryption
configuration of its own) takes the configured branch for an application that was never
configured that way.

Which blocks a description materialises when the request supplied none, and for which
runtimes, is the rule to establish — not the VALUES of the three streaming switches
(`CheckpointingEnabled`, `SnapshotsEnabled`, `AutoScalingEnabled`), which are keyed on the
kind of application and already correct (see
`issues/things-that-now-work/20261103T022644Z-kinesisanalyticsv2-studio-flink-runtime-rules-unenforced-createapplication-fails-open.md`,
whose fix closed that half and surfaced this one).

Note that this crate's existing suites assert both blocks today
(`tests/all/snapshot_mode_reporting.rs`, `tests/all/sql_input_and_configuration_defaults.rs`),
so their expectations have to move with the rule.

## Evidence
Replay of simparity-behavioral-aws-v1, unit
`kinesisanalyticsv2.studio-notebook-defaults-disable-checkpoints-snapshots-and-autoscaling-unlike-a-flink-app`:
after the three switches were corrected the unit fails later, with
"the Studio notebook describes blocks it never had:
['ApplicationEncryptionConfigurationDescription', 'ApplicationSystemRollbackConfigurationDescription']".

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. `CreateApplication(ApplicationName=<new>, RuntimeEnvironment=ZEPPELIN-FLINK-3_0,
   ApplicationMode=INTERACTIVE, ServiceExecutionRole=<assumable role>)` with no
   `ApplicationConfiguration` at all.
3. `DescribeApplication` that name and read
   `ApplicationConfigurationDescription` — observe
   `ApplicationSystemRollbackConfigurationDescription` and
   `ApplicationEncryptionConfigurationDescription` present where the recorded answer
   carries neither.
