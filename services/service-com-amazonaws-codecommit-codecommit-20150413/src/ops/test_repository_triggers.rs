// SPDX-License-Identifier: CC0-1.0

//! @op TestRepositoryTriggers
//!
//! "Tests the functionality of repository triggers by SENDING INFORMATION TO THE
//! TRIGGER TARGET": each trigger the caller submits is tested by delivering one record
//! to the destination it names — a publish to an Amazon SNS topic, an asynchronous
//! invocation of a Lambda function — and the operation reports, per trigger, whether
//! that delivery was accepted (`successfulExecutions`) or why it was not
//! (`failedExecutions`). The record itself, and the one place it is sent, are
//! [`crate::state::trigger_notifications`]; "if real data is available in the
//! repository, the test sends data from the last commit. If no data is available,
//! sample data is generated", which is that module's reading of the stored repository.
//!
//! A test is a TEST: the triggers it tests are the ones the CALLER submitted, and the
//! configuration the repository holds is not touched by it — only named, as the
//! configuration the record belongs to — so a test changes nothing a later
//! `GetRepositoryTriggers` reports.
//!
//! The submitted configuration is judged by
//! [`crate::state::trigger_configuration_problem`], the one place this crate decides
//! which configurations a repository can hold, so this operation and
//! `PutRepositoryTriggers` cannot disagree about it; every problem it names is answered
//! with that member's modelled error, in [`refusal`], and nothing is sent at all.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `TestRepositoryTriggers`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-codecommit-codecommit-20150413/smithy-model/com-amazonaws-codecommit-codecommit-20150413.json")
//! ```
//!
//! Tests the functionality of repository triggers by sending information to the trigger
//! target. If real data is available in the repository, the test sends data from the last
//! commit. If no data is available, sample data is generated.
//!
//! ## Input
//!
//! * `repositoryName` (RepositoryName, len 1..100, pattern ^[\w\.-]+$) **required** — The
//!   name of the repository in which to test the triggers.
//! * `triggers` (RepositoryTriggersList) **required** — The list of triggers to test.
//!
//! ## Output
//!
//! * `failedExecutions` (RepositoryTriggerExecutionFailureList) — The list of triggers that
//!   were not tested. This list provides the names of the triggers that could not be tested,
//!   separated by commas.
//! * `successfulExecutions` (RepositoryTriggerNameList) — The list of triggers that were
//!   successfully tested. This list provides the names of the triggers that were successfully
//!   tested, separated by commas.
//!
//! ## Errors
//!
//! * `EncryptionIntegrityChecksFailedException` (HTTP 500) — An encryption integrity check
//!   failed.
//! * `EncryptionKeyAccessDeniedException` (HTTP 400) — An encryption key could not be
//!   accessed.
//! * `EncryptionKeyDisabledException` (HTTP 400) — The encryption key is disabled.
//! * `EncryptionKeyNotFoundException` (HTTP 400) — No encryption key was found.
//! * `EncryptionKeyUnavailableException` (HTTP 400) — The encryption key is not available.
//! * `InvalidRepositoryNameException` (HTTP 400) — A specified repository name is not valid.
//!   This exception occurs only when a specified repository name is not valid. Other
//!   exceptions occur when a required repository parameter is missing, or when a specified
//!   repository does not exist.
//! * `InvalidRepositoryTriggerBranchNameException` (HTTP 400) — One or more branch names
//!   specified for the trigger is not valid.
//! * `InvalidRepositoryTriggerCustomDataException` (HTTP 400) — The custom data provided for
//!   the trigger is not valid.
//! * `InvalidRepositoryTriggerDestinationArnException` (HTTP 400) — The Amazon Resource Name
//!   (ARN) for the trigger is not valid for the specified destination. The most common reason
//!   for this error is that the ARN does not meet the requirements for the service type.
//! * `InvalidRepositoryTriggerEventsException` (HTTP 400) — One or more events specified for
//!   the trigger is not valid. Check to make sure that all events specified match the
//!   requirements for allowed events.
//! * `InvalidRepositoryTriggerNameException` (HTTP 400) — The name of the trigger is not
//!   valid.
//! * `InvalidRepositoryTriggerRegionException` (HTTP 400) — The Amazon Web Services Region
//!   for the trigger target does not match the Amazon Web Services Region for the repository.
//!   Triggers must be created in the same Amazon Web Services Region as the target for the
//!   trigger.
//! * `MaximumBranchesExceededException` (HTTP 400) — The number of branches for the trigger
//!   was exceeded.
//! * `MaximumRepositoryTriggersExceededException` (HTTP 400) — The number of triggers allowed
//!   for the repository was exceeded.
//! * `RepositoryDoesNotExistException` (HTTP 400) — The specified repository does not exist.
//! * `RepositoryNameRequiredException` (HTTP 400) — A repository name is required, but was
//!   not specified.
//! * `RepositoryTriggerBranchNameListRequiredException` (HTTP 400) — At least one branch name
//!   is required, but was not specified in the trigger configuration.
//! * `RepositoryTriggerDestinationArnRequiredException` (HTTP 400) — A destination ARN for
//!   the target service for the trigger is required, but was not specified.
//! * `RepositoryTriggerEventsListRequiredException` (HTTP 400) — At least one event for the
//!   trigger is required, but was not specified.
//! * `RepositoryTriggerNameRequiredException` (HTTP 400) — A name for the trigger is
//!   required, but was not specified.
//! * `RepositoryTriggersListRequiredException` (HTTP 400) — The list of triggers for the
//!   repository is required, but was not specified.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `TestRepositoryTriggersError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-codecommit-codecommit-20150413.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `TestRepositoryTriggersError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.codecommit#TestRepositoryTriggers")]
#[derive(Debug)]
pub enum TestRepositoryTriggersErrorModeled {
    EncryptionIntegrityChecksFailedException(EncryptionIntegrityChecksFailedException),
    EncryptionKeyAccessDeniedException(EncryptionKeyAccessDeniedException),
    EncryptionKeyDisabledException(EncryptionKeyDisabledException),
    EncryptionKeyNotFoundException(EncryptionKeyNotFoundException),
    EncryptionKeyUnavailableException(EncryptionKeyUnavailableException),
    InvalidRepositoryNameException(InvalidRepositoryNameException),
    InvalidRepositoryTriggerBranchNameException(InvalidRepositoryTriggerBranchNameException),
    InvalidRepositoryTriggerCustomDataException(InvalidRepositoryTriggerCustomDataException),
    InvalidRepositoryTriggerDestinationArnException(InvalidRepositoryTriggerDestinationArnException),
    InvalidRepositoryTriggerEventsException(InvalidRepositoryTriggerEventsException),
    InvalidRepositoryTriggerNameException(InvalidRepositoryTriggerNameException),
    InvalidRepositoryTriggerRegionException(InvalidRepositoryTriggerRegionException),
    MaximumBranchesExceededException(MaximumBranchesExceededException),
    MaximumRepositoryTriggersExceededException(MaximumRepositoryTriggersExceededException),
    RepositoryDoesNotExistException(RepositoryDoesNotExistException),
    RepositoryNameRequiredException(RepositoryNameRequiredException),
    RepositoryTriggerBranchNameListRequiredException(RepositoryTriggerBranchNameListRequiredException),
    RepositoryTriggerDestinationArnRequiredException(RepositoryTriggerDestinationArnRequiredException),
    RepositoryTriggerEventsListRequiredException(RepositoryTriggerEventsListRequiredException),
    RepositoryTriggerNameRequiredException(RepositoryTriggerNameRequiredException),
    RepositoryTriggersListRequiredException(RepositoryTriggersListRequiredException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::test_repository_triggers::TestRepositoryTriggersError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type TestRepositoryTriggersError = crate::scaffolded::Scaffolded<TestRepositoryTriggersErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<TestRepositoryTriggersErrorModeled> for TestRepositoryTriggersError {
    fn from(modeled: TestRepositoryTriggersErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `TestRepositoryTriggers` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct TestRepositoryTriggers<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> TestRepositoryTriggers<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

/// The modelled error for a trigger configuration this repository cannot hold.
///
/// [`crate::state::TriggerProblem`] names the member at fault, and
/// `crate::trigger_refusal!` is the ONE mapping from that to the error the model
/// declares for it — shared with `PutRepositoryTriggers`, which takes the same list
/// and models the same errors for it, so the two cannot classify or word one refusal
/// differently.
fn refusal(problem: crate::state::TriggerProblem) -> TestRepositoryTriggersError {
    crate::trigger_refusal!(TestRepositoryTriggersErrorModeled, problem).into()
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for TestRepositoryTriggers<Eng>
{
    type Input = TestRepositoryTriggersInput;
    type Output = TestRepositoryTriggersOutput;
    type Error = TestRepositoryTriggersError;

    const NAME: &'static str = "TestRepositoryTriggers";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::TEST_REPOSITORY_TRIGGERS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::TEST_REPOSITORY_TRIGGERS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            // An unreachable store is an internal mounting fault, not a modelled
            // CodeCommit failure; every handler in this crate refuses it this way.
            crate::scaffolded::Scaffolded::validation("the repository store is unavailable")
        })?;
        // Whose identity the record names: resolved BEFORE this service's store is
        // locked, because the resolution reads the registries that minted the request's
        // credential.
        let caller_arn = crate::state::caller_identity_arn(&*self.engine, ctx);
        let at = std::time::SystemTime::now();

        // Everything the records are rendered from is read under the guard, and the
        // guard is gone before anything is sent: a delivery reaches another service's
        // handler, which takes that service's own lock.
        let notifications = {
            let store = state.lock();
            let repository = store.repositories.get(&input.repository_name).ok_or_else(|| {
                TestRepositoryTriggersErrorModeled::RepositoryDoesNotExistException(
                    RepositoryDoesNotExistException {
                        message: Some(crate::state::repository_does_not_exist_message(
                            &input.repository_name,
                        )),
                    },
                )
            })?;
            // The configuration is judged BEFORE anything is sent, so a trigger the
            // repository could not hold is refused rather than tested.
            if let Some(problem) =
                crate::state::trigger_configuration_problem(&input.triggers, &repository.region)
            {
                return Err(refusal(problem));
            }
            crate::state::trigger_notifications::test_notifications(
                repository,
                &input.triggers,
                &caller_arn,
                at,
            )
        };

        // One delivery per submitted trigger, in the order the triggers were
        // submitted: a destination that refuses one trigger's record does not stop the
        // next trigger from being tested, which is what makes the two lists a per-trigger
        // report rather than a single verdict. How many deliveries one request can make
        // is what the judging above already bounds — a list longer than a repository may
        // hold (`crate::state::MAX_TRIGGERS_PER_REPOSITORY`) is refused before any of
        // them is made.
        let mut successful_executions = Vec::new();
        let mut failed_executions = Vec::new();
        for notification in notifications {
            match notification.send(ctx.callback()).await {
                Ok(()) => successful_executions.push(notification.trigger_name.clone()),
                Err(failure_message) => {
                    failed_executions.push(RepositoryTriggerExecutionFailure {
                        trigger: Some(notification.trigger_name.clone()),
                        failure_message: Some(failure_message),
                    });
                }
            }
        }

        Ok(TestRepositoryTriggersOutput {
            successful_executions: Some(successful_executions),
            failed_executions: Some(failed_executions),
        })
    }
}
