// SPDX-License-Identifier: CC0-1.0

//! @op ListMountTargets
//!
//! Pages through the stored mount targets, newest last, optionally filtered to one file
//! system or to the file system an access point belongs to. A reporting operation: the
//! only state it touches is the transition the clock already owed, and every row is the
//! same projection the create and `GetMountTarget` answer with
//! ([`crate::state::MountTargetView`]).
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ListMountTargets`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-s3files-s3files/smithy-model/com-amazonaws-s3files-s3files.json")
//! ```
//!
//! Returns resource information for all mount targets with optional filtering by file
//! system, access point, and VPC.
//!
//! ## Input
//!
//! * `accessPointId` (AccessPointId, len ..256, pattern
//!   ^(arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}/access-point/fsap-[0-9a-f]{17,40}|fsap-[0-9a-f]{17,4
//!   …) — Optional filter to list only mount targets associated with the specified access
//!   point ID or Amazon Resource Name (ARN).
//! * `fileSystemId` (FileSystemId, len ..128, pattern
//!   ^(arn:aws[-a-z]*:s3files:[0-9a-z-:]+:file-system/fs-[0-9a-f]{17,40}|fs-[0-9a-f]{17,40})$)
//!   — Optional filter to list only mount targets associated with the specified S3 File
//!   System ID or Amazon Resource Name (ARN). If provided, only mount targets for this file
//!   system will be returned in the response.
//! * `maxResults` (Integer, default 100) **required** — The maximum number of mount targets
//!   to return in a single response.
//! * `nextToken` (String) — A pagination token returned from a previous call to continue
//!   listing mount targets.
//!
//! ## Output
//!
//! * `mountTargets` (MountTargets) **required** — An array of mount target descriptions.
//! * `nextToken` (String) — A pagination token to use in a subsequent request if more results
//!   are available.
//!
//! ## Errors
//!
//! * `InternalServerException` (HTTP 500) — An internal server error occurred. Retry your
//!   request.
//! * `ResourceNotFoundException` (HTTP 404) — The specified resource was not found. Verify
//!   that the resource exists and that you have permission to access it.
//! * `ValidationException` (HTTP 400) — The input parameters are not valid. Check the
//!   parameter values and try again.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ListMountTargetsError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-s3files-s3files.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ListMountTargetsError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.s3files#ListMountTargets")]
#[derive(Debug)]
pub enum ListMountTargetsErrorModeled {
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ValidationException(ValidationException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_mount_targets::ListMountTargetsError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ListMountTargetsError = crate::scaffolded::Scaffolded<ListMountTargetsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ListMountTargetsErrorModeled> for ListMountTargetsError {
    fn from(modeled: ListMountTargetsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListMountTargets` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ListMountTargets<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListMountTargets<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListMountTargets<Eng>
{
    type Input = ListMountTargetsInput;
    type Output = ListMountTargetsOutput;
    type Error = ListMountTargetsError;

    const NAME: &'static str = "ListMountTargets";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_MOUNT_TARGETS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::LIST_MOUNT_TARGETS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let _ = ctx;
        let now = ::std::time::SystemTime::now();

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            ListMountTargetsErrorModeled::InternalServerException(crate::state::store_unavailable())
        })?;
        let mut store = state.lock();
        // The only write a reporting operation makes: the transition the clock already
        // owed at this instant, whether or not anybody called.
        store.settle(now);

        // Each filter names a resource of this service, so one this store does not hold is
        // the modelled 404 rather than an empty page — the same reading as
        // `ListAccessPoints`.
        let mut file_system_id = match input.file_system_id.as_deref() {
            Some(value) => {
                let id = crate::state::file_system_id_of(value)
                    .ok_or_else(|| {
                        ListMountTargetsErrorModeled::ValidationException(crate::state::invalid(
                            format!(
                                "fileSystemId must be a file system id (fs-<hex>) or its ARN, \
                                 got {value:?}"
                            ),
                        ))
                    })?
                    .to_string();
                if !store.file_systems.contains_key(&id) {
                    return Err(ListMountTargetsErrorModeled::ResourceNotFoundException(
                        crate::state::not_found(crate::state::FILE_SYSTEM_NOT_FOUND_MESSAGE),
                    )
                    .into());
                }
                Some(id)
            }
            None => None,
        };

        // A mount target belongs to a file system, so "the mount targets associated with
        // the specified access point" are the mount targets of the file system that access
        // point is an access point OF. Named with a `fileSystemId` that is a DIFFERENT file
        // system, the two filters select nothing, which is an empty page rather than an
        // error.
        if let Some(value) = input.access_point_id.as_deref() {
            let access_point_id = crate::state::access_point_id_of(value)
                .ok_or_else(|| {
                    ListMountTargetsErrorModeled::ValidationException(crate::state::invalid(
                        format!(
                            "accessPointId must be an access point id (fsap-<hex>) or its ARN, \
                             got {value:?}"
                        ),
                    ))
                })?
                .to_string();
            let owner = store
                .access_points
                .get(&access_point_id)
                .ok_or_else(|| {
                    ListMountTargetsErrorModeled::ResourceNotFoundException(
                        crate::state::not_found(crate::state::ACCESS_POINT_NOT_FOUND_MESSAGE),
                    )
                })?
                .file_system_id
                .clone();
            if file_system_id.as_deref().is_some_and(|named| named != owner) {
                return Ok(ListMountTargetsOutput {
                    next_token: None,
                    mount_targets: Vec::new(),
                });
            }
            file_system_id = Some(owner);
        }

        let after = crate::state::decode_cursor(input.next_token.as_deref()).map_err(|_| {
            ListMountTargetsErrorModeled::ValidationException(crate::state::invalid(
                "nextToken is not a token this service issued",
            ))
        })?;

        let ordered: Vec<(u64, ListMountTargetsDescription)> = store
            .mount_targets_in_order(file_system_id.as_deref())
            .into_iter()
            .map(|mount_target| {
                (
                    mount_target.sequence,
                    crate::state::MountTargetView::from(mount_target).into(),
                )
            })
            .collect();

        // `ListMountTargetsRequest$maxResults` is `@range(1, 100)`.
        let page = crate::state::page_of(
            ordered,
            after,
            input.max_results,
            crate::state::MAX_RESULTS_LIMIT,
        );
        Ok(ListMountTargetsOutput {
            next_token: page.next_token,
            mount_targets: page.items,
        })
    }
}
