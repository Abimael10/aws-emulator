// SPDX-License-Identifier: CC0-1.0

//! @op StartImportLabelsTaskRun
//!
//! Imports the labels a caller uploaded into one machine learning transform: "users upload
//! their answers/labels with a call to StartImportLabelsTaskRun… By default,
//! StartMLLabelingSetGenerationTaskRun continually learns from and combines all labels that
//! you upload unless you set Replace to true." The call READS the label file at the
//! `InputS3Path` out of the emulator's own Amazon S3, folds its labels into the set the
//! transform holds ([`crate::state::MlLabelSet`]), records the `IMPORT_LABELS` task run
//! that did it and answers the `TaskRunId` that addresses it — "you can check on the status
//! of your task run by calling the GetMLTaskRun operation".
//!
//! The labels it stored are the labels `StartExportLabelsTaskRun` writes back out, so a
//! label file round-trips through the transform.
//!
//! This is the ONLY file to touch for this operation.
//!
//! ## Shape of an operation
//!
//! Each operation is a struct that owns the shared engine and implements
//! [`smithy_server_runtime::Operation`]. The framework constructs one per service via
//! `StartImportLabelsTaskRun::new(engine)` and drives it through the centralized request walk:
//! deserialize → validate → `handle` → serialize. You write ONLY the `handle` body.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request — so
//! engine access is guaranteed by the time any request runs. Reach the store through
//! [`crate::state::service_state`] and read and write it through the guard it hands back.
//!
//! `ctx` ([`smithy_server_runtime::OperationContext`]) carries request metadata only —
//! headers, URI, method, and anything middleware placed in `ctx.parts().extensions`.
//! The engine is NOT reached through `ctx`.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `StartImportLabelsTaskRun`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-glue-awsglue/smithy-model/com-amazonaws-glue-awsglue.json")
//! ```
//!
//! Enables you to provide additional labels (examples of truth) to be used to teach the
//! machine learning transform and improve its quality. This API operation is generally used
//! as part of the active learning workflow that starts with the
//! StartMLLabelingSetGenerationTaskRun call and that ultimately results in improving the
//! quality of your machine learning transform. After the
//! StartMLLabelingSetGenerationTaskRun finishes, Glue machine learning will have generated
//! a series of questions for humans to answer. (Answering these questions is often called
//! 'labeling' in the machine learning workflows). In the case of the FindMatches transform,
//! these questions are of the form, “What is the correct way to group these rows together
//! into groups composed entirely of matching records?” After the labeling process is
//! finished, users upload their answers/labels with a call to StartImportLabelsTaskRun.
//! After StartImportLabelsTaskRun finishes, all future runs of the machine learning
//! transform use the new and improved labels and perform a higher-quality transformation.
//! By default, StartMLLabelingSetGenerationTaskRun continually learns from and combines all
//! labels that you upload unless you set Replace to true. If you set Replace to true,
//! StartImportLabelsTaskRun deletes and forgets all previously uploaded labels and learns
//! only from the exact set that you upload. Replacing labels can be helpful if you realize
//! that you previously uploaded incorrect labels, and you believe that they are having a
//! negative effect on your transform quality. You can check on the status of your task run
//! by calling the GetMLTaskRun operation.
//!
//! ## Input
//!
//! * `InputS3Path` (UriString) **required** — The Amazon Simple Storage Service (Amazon S3)
//!   path from where you import the labels.
//! * `ReplaceAllLabels` (ReplaceBoolean, default false) **required** — Indicates whether to
//!   overwrite your existing labels.
//! * `TransformId` (HashString, len 1..255, pattern
//!   ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) **required** — The unique
//!   identifier of the machine learning transform.
//!
//! ## Output
//!
//! * `TaskRunId` (HashString, len 1..255, pattern
//!   ^[\u0020-\uD7FF\uE000-\uFFFD\uD800\uDC00-\uDBFF\uDFFF\t]*$) — The unique identifier for
//!   the task run.
//!
//! ## Errors
//!
//! * `EntityNotFoundException` (HTTP 400) — A specified entity does not exist
//! * `InternalServiceException` (HTTP 500) — An internal service error occurred.
//! * `InvalidInputException` (HTTP 400) — The input provided was not valid.
//! * `OperationTimeoutException` (HTTP 400) — The operation timed out.
//! * `ResourceNumberLimitExceededException` (HTTP 400) — A resource numerical limit was
//!   exceeded.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `StartImportLabelsTaskRunError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-glue-awsglue.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `StartImportLabelsTaskRunError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.glue#StartImportLabelsTaskRun")]
#[derive(Debug)]
pub enum StartImportLabelsTaskRunErrorModeled {
    EntityNotFoundException(EntityNotFoundException),
    InternalServiceException(InternalServiceException),
    InvalidInputException(InvalidInputException),
    OperationTimeoutException(OperationTimeoutException),
    ResourceNumberLimitExceededException(ResourceNumberLimitExceededException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::start_import_labels_task_run::StartImportLabelsTaskRunError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type StartImportLabelsTaskRunError = crate::scaffolded::Scaffolded<StartImportLabelsTaskRunErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<StartImportLabelsTaskRunErrorModeled> for StartImportLabelsTaskRunError {
    fn from(modeled: StartImportLabelsTaskRunErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `StartImportLabelsTaskRun` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct StartImportLabelsTaskRun<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> StartImportLabelsTaskRun<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for StartImportLabelsTaskRun<Eng>
{
    type Input = StartImportLabelsTaskRunInput;
    type Output = StartImportLabelsTaskRunOutput;
    type Error = StartImportLabelsTaskRunError;

    const NAME: &'static str = "StartImportLabelsTaskRun";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::START_IMPORT_LABELS_TASK_RUN_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::START_IMPORT_LABELS_TASK_RUN_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let Some(state) = crate::state::service_state(&*self.engine) else {
            // This operation models a server fault, so an unreachable store is reported as
            // that rather than as a client error the caller could act on.
            return Err(internal("the Glue machine learning transform store is unavailable"));
        };

        // `TransformId` and `InputS3Path` are both `@required`, and the id's
        // `length 1..255` is refused at the boundary — so the only question left about the
        // request itself is whether the path names an object in the object store at all.
        // A path that names none is the request being wrong, not the labels being
        // unreadable, so it is refused here and starts no run — through the same predicate
        // the read itself resolves the object with, so the two cannot drift apart.
        if crate::state::label_file_object(&input.input_s3_path).is_none() {
            return Err(invalid_path(&input.input_s3_path));
        }

        // Whether this account holds the transform the labels would be imported into is
        // decided BEFORE the file is read: the labels are stored ON that transform, so an
        // id this account holds none under imports nothing and reads nothing.
        {
            let store = state.lock();
            if !store.ml_transforms.contains_key(&input.transform_id) {
                return Err(not_found(&input.transform_id));
            }
        }

        // The label file is read with NO store guard held: nothing is awaited while the
        // store is locked. The read goes over the in-process callback into this same
        // emulator, so the object a caller's own `PutObject` stored is the one imported.
        let region = crate::state::request_region(ctx.parts());
        let uploaded =
            crate::state::read_label_file(ctx, &region, &input.input_s3_path).await;

        // The instants the run reports are stamped AFTER the reading, so the span the clock
        // owes the run starts when the run was recorded: a caller that reads the run
        // straight after this call still observes it in flight, however long the read took.
        let now = ::std::time::SystemTime::now();
        let mut store = state.lock();
        // The store is re-read under the guard the run is recorded under: a delete that
        // landed while the file was being read decides this start. The only work done while
        // that guard is held is folding the uploaded labels into the set the transform
        // holds, which is bounded BEFORE it allocates and at its very widest is
        // `crate::state::MAX_LABEL_CELLS` pointer moves — the file's own bytes were read
        // above, with no guard held.
        let Some(transform) = store.ml_transforms.get_mut(&input.transform_id) else {
            return Err(not_found(&input.transform_id));
        };

        // The id is minted only once the request is going to be served, so the refusals
        // above hand out none.
        let run_id = crate::state::new_ml_task_run_id();
        // What the run IS, in the modelled shape every reader of it reports: an importing
        // labels task, carrying the properties that kind of task has — "the Amazon Simple
        // Storage Service (Amazon S3) path from where you will import the labels" and
        // whether the upload overwrites the labels already held.
        let properties = TaskRunProperties {
            task_type: Some(crate::state::ML_TASK_TYPE_IMPORT_LABELS.to_string()),
            import_labels_task_run_properties: Some(ImportLabelsTaskRunProperties {
                input_s3_path: Some(input.input_s3_path.clone()),
                replace: input.replace_all_labels,
            }),
            // The properties of the other task kinds belong to the runs those starts
            // record, not to this one.
            export_labels_task_run_properties: None,
            labeling_set_generation_task_run_properties: None,
            find_matches_task_run_properties: None,
        };
        // The task was really carried out here, so the run is owed the end that work
        // reached rather than the one a task nobody performed is terminated in.
        let owed_end = match uploaded {
            Ok(uploaded) => {
                let labels = uploaded.len();
                let held = transform.import_labels(uploaded, input.replace_all_labels);
                ::tracing::info!(
                    operation = "StartImportLabelsTaskRun",
                    transform = %input.transform_id,
                    run = %run_id,
                    labels,
                    held,
                    "imported labels into a Glue machine learning transform"
                );
                crate::state::MlTaskRunEnd::Succeeded
            }
            Err(reason) => {
                // Nothing was imported, so the labels the transform holds are left exactly
                // as they were — including when the request asked to replace them.
                ::tracing::debug!(
                    operation = "StartImportLabelsTaskRun",
                    transform = %input.transform_id,
                    run = %run_id,
                    reason = %reason,
                    "a Glue machine learning transform's labels could not be imported"
                );
                crate::state::MlTaskRunEnd::Failed(reason)
            }
        };
        transform.record_task_run(
            crate::state::MlTaskRunEntry::start(
                &input.transform_id,
                run_id.clone(),
                properties,
                now,
            )
            .ending(owed_end),
        );

        Ok(StartImportLabelsTaskRunOutput {
            // "The unique identifier for the task run."
            task_run_id: Some(run_id),
        })
    }
}

/// This operation's modelled miss: "A specified entity does not exist" — worded in the one
/// sentence every operation of this service states a transform miss in.
fn not_found(transform_id: &str) -> StartImportLabelsTaskRunError {
    StartImportLabelsTaskRunErrorModeled::EntityNotFoundException(EntityNotFoundException {
        message: Some(crate::state::messages::ml_transform_not_found(transform_id)),
        from_federation_source: None,
    })
    .into()
}

/// This operation's modelled refusal of a path the labels cannot live at: "The input
/// provided was not valid."
fn invalid_path(path: &str) -> StartImportLabelsTaskRunError {
    StartImportLabelsTaskRunErrorModeled::InvalidInputException(InvalidInputException {
        message: Some(crate::state::messages::invalid_label_file_path(
            "InputS3Path",
            path,
        )),
        from_federation_source: None,
    })
    .into()
}

/// This operation's modelled server fault: "An internal service error occurred."
fn internal(message: &str) -> StartImportLabelsTaskRunError {
    StartImportLabelsTaskRunErrorModeled::InternalServiceException(InternalServiceException {
        message: Some(message.to_string()),
    })
    .into()
}
