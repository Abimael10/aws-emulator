// SPDX-License-Identifier: CC0-1.0

//! @op ListBulkOperations
//! IMPLEMENTED: reports one `BulkOperationSummary` per bulk operation of the region the
//! request reached — the operation's own ARN, the status it is in, the table it acts on and
//! the instants it began and (once it has) ended — narrowed to one table when the request
//! names one, and paged by `MaxResults` with a `NextToken` that stands for the entry the
//! page stopped on.
//!
//! A REPORTING operation: every summary is read from the bulk-operation catalogue
//! `StartBulkPutItems` writes ([`crate::state::BulkOperation`]), and the status each one
//! reports is derived from the clock at every read, so this call writes nothing and two
//! callers reading it see the same thing.
//!
//! WHERE THIS OPERATION'S CONTRACT COMES FROM. The service's Smithy model carried no
//! bulk-operation shape when this was written, so there is no modelled doc text to quote:
//! the members are the ones the recorded calls name — the summary list, and the ARN, status
//! and instants of each entry — and the shapes this crate is built from now declare them.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request. Reach the
//! store through [`crate::state::service_state`] and read it through the guard it hands
//! back.

#[allow(unused_imports)]
use super::types::*;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-dynamodb-dynamodb-20120810.json");

/// The operation's MODELED errors.
#[smithy_macros::operation_error("com.amazonaws.dynamodb#ListBulkOperations")]
#[derive(Debug)]
pub enum ListBulkOperationsErrorModeled {
    InternalServerError(InternalServerError),
    InvalidEndpointException(InvalidEndpointException),
    ResourceNotFoundException(ResourceNotFoundException),
    ThrottlingException(ThrottlingException),
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::list_bulk_operations::ListBulkOperationsError`).
pub type ListBulkOperationsError = crate::scaffolded::Scaffolded<ListBulkOperationsErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel.
impl From<ListBulkOperationsErrorModeled> for ListBulkOperationsError {
    fn from(modeled: ListBulkOperationsErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ListBulkOperations` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this crate
/// names no concrete engine type.
pub struct ListBulkOperations<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ListBulkOperations<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ListBulkOperations<Eng>
{
    type Input = ListBulkOperationsInput;
    type Output = ListBulkOperationsOutput;
    type Error = ListBulkOperationsError;

    const NAME: &'static str = "ListBulkOperations";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        super::types::LIST_BULK_OPERATIONS_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema =
        super::types::LIST_BULK_OPERATIONS_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        let region = crate::state::request_region(ctx.parts());
        let now = ::std::time::SystemTime::now();
        let state = crate::state::service_state(&*self.engine).ok_or_else(store_unavailable)?;
        // The member narrows the listing to one table's bulk operations, and a request that
        // names none is answered over every table's. It accepts the two spellings every
        // table-addressing member of this service accepts.
        let table = input
            .table_name
            .as_deref()
            .map(|named| crate::state::resolve_table_name_in(named, &region))
            .transpose()
            .map_err(lift)?;
        // A `NextToken` must be one this operation minted, so a token it did not stands for
        // no page and is refused rather than quietly taken for the first one.
        let cursor = input.next_token.as_deref().map(decode_token).transpose()?;
        // WHO this request speaks for, resolved before this service's store is locked: the
        // listing is authorized against the caller's own identity-based policies, over the
        // table it narrows itself to or the account's whole table namespace when it names
        // none ([`crate::state::authorization`]).
        let caller = crate::state::request_caller(&*self.engine, ctx);
        if let Some(denial) = crate::state::authorization::account_denial(
            &caller,
            ACTION,
            &crate::state::authorization::account_resource(&region, table.as_deref()),
        ) {
            return Err(crate::scaffolded::Scaffolded::access_denied(denial));
        }

        let store = state.lock();
        // A reporting operation, and one that reports nothing a WRITE could settle: a
        // bulk-operation record is fixed once written and the status it reports is derived
        // from the clock on each read. So this reads only.
        let mut summaries: Vec<BulkOperationSummary> = store
            .bulk_operations_in(&region)
            .filter(|operation| {
                table
                    .as_deref()
                    .is_none_or(|named| operation.table_name == named)
            })
            // The cursor: this page starts strictly after the entry the previous one
            // stopped on, which that page's `NextToken` carried.
            .filter(|operation| {
                cursor
                    .as_deref()
                    .is_none_or(|start| operation.arn.as_str() > start)
            })
            .map(|operation| operation.summary(now))
            .collect();
        drop(store);

        // The page size the request asked for, or as many as one page may carry when it
        // asked for none. The model's `@range` on that member admits only a positive count,
        // so a page size this reads is one the validating walk ahead of it has already
        // admitted; a value outside `usize` could therefore only be one of those, and it is
        // read as the empty page rather than as an unbounded one.
        let limit = usize::try_from(input.max_results.unwrap_or(DEFAULT_MAX_RESULTS)).unwrap_or(0);
        let truncated = summaries.len() > limit;
        summaries.truncate(limit);
        // Reported only where entries remain beyond this page, and standing for the entry
        // the page stopped on.
        let next_token = truncated
            .then(|| {
                summaries
                    .last()
                    .and_then(|summary| summary.bulk_operation_arn.as_deref())
                    .map(encode_token)
            })
            .flatten();

        Ok(ListBulkOperationsOutput {
            bulk_operations_summary_list: Some(summaries),
            next_token,
        })
    }
}

/// The page size this operation reports when the request names no `MaxResults`: the ceiling
/// the model declares for that member, so a request that asks for no bound is answered with
/// as many operations as one page may carry.
const DEFAULT_MAX_RESULTS: i32 = 100;

/// The IAM action this operation performs, which a refusal names.
const ACTION: &str = "dynamodb:ListBulkOperations";

/// The cursor a truncated page reports: the entry the page stopped on, in hex, so a token
/// is opaque to a client and carries nothing but that entry — and is a value the next
/// request may send straight back.
fn encode_token(cursor: &str) -> String {
    ::hex::encode(cursor.as_bytes())
}

/// The cursor a `NextToken` stands for, or the refusal for a token this operation did not
/// mint.
fn decode_token(token: &str) -> Result<String, ListBulkOperationsError> {
    let bytes = ::hex::decode(token).map_err(|_| invalid_next_token())?;
    let cursor = String::from_utf8(bytes).map_err(|_| invalid_next_token())?;
    if cursor.is_empty() {
        return Err(invalid_next_token());
    }
    Ok(cursor)
}

/// How a `NextToken` that is not one of this operation's is refused: the wording this
/// service states for that case wherever a reader pages by a token.
fn invalid_next_token() -> ListBulkOperationsError {
    crate::scaffolded::Scaffolded::validation("Invalid NextToken")
}

/// The modelled server error for a request that cannot reach this service's store.
fn store_unavailable() -> ListBulkOperationsError {
    ListBulkOperationsErrorModeled::InternalServerError(InternalServerError {
        message: Some("the DynamoDB table store is unavailable".to_string()),
    })
    .into()
}

/// Lift a store refusal into this operation's error channel.
fn lift(error: crate::state::RequestError) -> ListBulkOperationsError {
    match error {
        crate::state::RequestError::NotFound(message) => {
            ListBulkOperationsErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
                message: Some(message),
            })
            .into()
        }
        crate::state::RequestError::Validation(message) => {
            crate::scaffolded::Scaffolded::validation(message)
        }
    }
}
