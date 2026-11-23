Issue-Severity: MEDIUM
Issue-Gap-Class: parameter-ignored
Issue-Commit: 91604195cb4050ca128792b34b06a1a1d7d167f9

# IncludeEc2InstanceShutdown skips the declared components' EC2 instances for an application whose discovery has not completed

_Filed: 2026-11-23T16:59:34Z_
_Operation: StopApplication | Class: parameter-ignored_

> _Note: this issue was identified against the code base at commit `91604195cb4050ca128792b34b06a1a1d7d167f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StopApplication$IncludeEc2InstanceShutdown` asks the handler for "the Amazon EC2 instances the registration associated with the application". The handler reads them off `StoredApplication::ec2_instance_ids()`, whose own documentation says those are the `Instances` member AND the `Ec2InstanceId` of each DECLARED component (citing `registered_components`, the declaration-time set). Its body, however, chains `self.components()` — which `components()`'s own doc states is EMPTY until the application's discovery has completed. So for an application whose discovery has not completed, the flag shuts down only the single `Instances` entry and silently leaves every `ComponentsInfo` instance running, and no `OperationEvent` milestone is recorded for those instances either (`record_instance_milestones` is handed only what `ec2_instance_ids()` answered).

That state is not a corner the crate forbids — it is one `StopApplication` deliberately serves: `application_control_available` exempts `Stop` from the in-flight gate while the application is `REGISTERING` ("a stop does not wait for the registration that is still running ... so a caller that registers an application and stops it straight away is served"), and the handler's module doc repeats that exemption. The same silent gap is permanent for an application whose registration FAILED, because `discovery_completed` never becomes true for one. The result is timing-dependent: the identical call made after `TRANSITION_SPAN` (once discovery settled to SUCCESS) stops both instances, while the one made inside that window stops only one.

The fix is to read the instances the registration DECLARED (`described_components`), which is what `ec2_instance_ids`'s documentation already says it answers.

## Evidence

The helper's doc names the DECLARED components — services/service-com-amazonaws-ssmsap-ssmsap/src/state.rs:1058-1066:
```
/// The Amazon EC2 instances this application runs on, each once, in the order the
/// registration named them.
///
/// A registration associates an application with an EC2 instance in EITHER of two
/// places, exactly as it describes a component ([`registered_components`]): the
/// `Instances` the SAP system itself runs on, and the `Ec2InstanceId` each declared
/// component runs on. Both are instances of this application, so both are what a
/// call that acts on "the associated Amazon EC2 instance" acts on; an id named twice
/// is one instance, so it is answered once.
```
Its body reads the DISCOVERED set instead — state.rs:1067-1072:
```
pub fn ec2_instance_ids(&self) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    let declared = self
        .instances
        .iter()
        .chain(self.components().iter().map(|c| &c.ec2_instance_id));
```
and that set is empty before discovery completes — state.rs:1235-1248:
```
/// A component of a registered application exists only once the service's discovery of
/// that application has produced it, so an application whose discovery has not
/// completed has NONE, whatever its registration described
/// ([`Self::described_components`]) ...
pub fn components(&self) -> &[StoredComponent] {
    if self.discovery_completed {
        &self.described_components
    } else {
        &[]
```
The stop handler calls exactly that helper — src/ops/stop_application.rs:338-347:
```
// `IncludeEc2InstanceShutdown` shuts the application's Amazon EC2 instances down "in
// addition to the application", so the instances its registration associated with it
// are read off the record here ...
let instances_to_shut_down = if input.include_ec2_instance_shutdown == Some(true) {
    application.ec2_instance_ids()
```
and the pre-discovery state is deliberately served — src/state.rs:3791-3807:
```
/// STOPPING is the exception, and the registration is what it is an exception to: a stop
/// does not wait for the registration that is still running ...
    if control == ApplicationControl::Stop
        && application.status == APPLICATION_STATUS_REGISTERING
    {
        return Ok(());
```
Model member text for the flag: "If included and if set to `True`, the StopApplication operation will shut down the associated Amazon EC2 instance in addition to the application."

Test coverage: tests/all/ec2_instance_shutdown.rs states the same rule ("The instances associated with an application are the ones its registration named: the `Instances` the SAP system runs on and the `Ec2InstanceId` of each declared component", lines 11-13) but every row stops an application through `register_activated`, which sleeps `settle_wait()` and asserts the registration is ACTIVATED before stopping (lines 240-256) — so no case exercises a stop before discovery completed, and the gap is unpinned.

## Steps to Reproduce

1. Mount ssm-sap beside EC2 and launch two running instances, A (the SAP system) and B (a component host).
2. POST /register-application with ApplicationId=abap-wd, ApplicationType=SAP_ABAP, Instances=[A], Sid=PRD, and ComponentsInfo=[{ComponentType:WD, Sid:PRD, Ec2InstanceId:B}]. The response reports Status=REGISTERING.
3. Immediately (inside TRANSITION_SPAN, before the registration settles) POST /stop-application with ApplicationId=abap-wd and IncludeEc2InstanceShutdown=true.
4. Call EC2 DescribeInstances for A and B: A is stopping/stopped, B is still running.
5. POST /list-operation-events for the returned OperationId: there is a milestone for the application, and one for instance A only — none for B.
6. Repeat the same registration and wait out TRANSITION_SPAN before stopping: both A and B are now shut down, showing the result depends only on whether discovery had completed.
