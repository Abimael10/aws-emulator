// SPDX-License-Identifier: CC0-1.0

//! The `Data` member a `GetQueryResults` answer does not carry while the query's
//! repository holds no rows.
//!
//! A started query reports two things: the columns its repository is made of (`Fields`)
//! and the rows it selected (`Data`). The columns are what the query asked for, so they
//! are reported from the moment the query exists — but AWS's answer for a query whose
//! repository holds no rows carries NO `Data` member at all, rather than a member holding
//! an empty list. The model's `@required` on `Data`
//! (`smithy_query(query="shape",shape="GetQueryResultsOutput")`) says that an answer
//! reporting rows carries them there; it does not make an empty list the answer for a
//! repository that holds none.
//!
//! That distinction cannot be made by the handler or by the schema serializer: a
//! `@required` list member is a plain `Vec` field, which the generated serializer writes
//! for every value it can hold, the empty one included. The only layer that can leave it
//! out is the one holding the body the serializer just rendered — which is what
//! [`service_common::success_body_members`] owns the plumbing of, and what this module
//! takes exactly one decision in: on a `GetQueryResults` success body whose `Data` member
//! holds no rows, that member is dropped. Every other member, every other operation's
//! body and every failure keep the values their renderer wrote, `Fields` included: a
//! client reading the repository's schema out of an empty result still finds it.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::serde_json::value::RawValue;
use ::service_common::success_body_members::{with_rewritten_success_body, RawObject};
use ::smithy_server_runtime::{BoxFuture, ErasedOperation, OperationContext, Service};

/// The operation whose answer this rewrite owns. A request's routing key is the
/// operation's own shape name (see `smithy_server_runtime::OperationRoutingTable`), so
/// this is what a request for it resolves to.
const OPERATION: &str = "GetQueryResults";

/// The output member holding the rows a query selected.
const DATA_MEMBER: &str = "Data";

/// Whether `req` is a request for the operation this rewrite owns.
pub fn serves<Eng: Send + Sync + 'static>(
    entry: &crate::ServiceDefinition<Eng>,
    req: &RtRequest,
) -> bool {
    entry
        .protocol()
        .routing_key(req)
        .is_ok_and(|key| key == OPERATION)
}

/// Whether one rendered member holds no element.
fn holds_no_rows(member: &RawValue) -> bool {
    ::serde_json::from_str::<Vec<&RawValue>>(member.get())
        .is_ok_and(|rows| rows.is_empty())
}

/// `body` without its `Data` member, or `None` when there is nothing to drop: the body is
/// not a JSON object, it carries no `Data` member, or the member it carries holds rows.
fn without_an_empty_repository(body: &[u8]) -> Option<Vec<u8>> {
    let mut object: RawObject<'_> = ::serde_json::from_slice(body).ok()?;
    if !object.get(DATA_MEMBER).is_some_and(|rows| holds_no_rows(rows)) {
        return None;
    }
    object.remove(DATA_MEMBER);
    ::serde_json::to_vec(&object).ok()
}

/// The response as Internet Monitor answers it: a `GetQueryResults` success whose
/// repository holds no rows carries no `Data` member; anything else keeps every byte of
/// the body its renderer wrote.
#[must_use]
pub fn without_empty_repository(resp: RtResponse) -> RtResponse {
    with_rewritten_success_body(resp, without_an_empty_repository)
}

/// Serves `GetQueryResults` through the service's envelope — so the framework still owns
/// the whole deserialize → validate → handle → serialize sequence, and a refused call
/// still names its exception in `code` — and then leaves out the `Data` member of a
/// repository that holds no rows.
pub struct QueryRepositoryOperation {
    /// The envelope this wrapper delegates to, held in a field because
    /// [`ErasedOperation::run`] borrows the operation it runs for the whole call.
    envelope: super::error_code::ErrorCodeOperation,
}

impl QueryRepositoryOperation {
    /// The wrapper, built once by the service that hands it back from
    /// `Service::override_operation`.
    pub fn new() -> Self {
        Self {
            envelope: super::error_code::ErrorCodeOperation,
        }
    }
}

impl Default for QueryRepositoryOperation {
    fn default() -> Self {
        Self::new()
    }
}

impl ErasedOperation for QueryRepositoryOperation {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            without_empty_repository(self.envelope.run(svc, req, ctx).await)
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", which is the
        // operation the envelope this wrapper delegates to serves, so its model-declared
        // schemes decide and this wrapper cannot weaken them.
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the `Data` member is dropped exactly when it holds no rows. The rows differ
    /// along what the body carries beside it and along what the member holds.
    #[test]
    fn only_a_repository_that_holds_no_rows_drops_the_member() {
        // (case, the rendered body, whether the member must be dropped)
        for (case, body, dropped) in [
            ("no rows", r#"{"Fields":[{"Name":"availability"}],"Data":[]}"#, true),
            ("no rows, spaced", r#"{"Fields":[],"Data":[ ]}"#, true),
            ("no rows and a token", r#"{"Data":[],"NextToken":"t"}"#, true),
            ("one row", r#"{"Fields":[],"Data":[["98.5"]]}"#, false),
            ("one empty row", r#"{"Data":[[]]}"#, false),
            ("no member at all", r#"{"Fields":[]}"#, false),
            ("not an object", r#"[{"Data":[]}]"#, false),
            ("not JSON", "<Results/>", false),
        ] {
            let rewritten = without_an_empty_repository(body.as_bytes());
            assert_eq!(
                rewritten.is_some(),
                dropped,
                "case {case:?}: expected dropped={dropped}, got {rewritten:?}"
            );
            if let Some(rewritten) = rewritten {
                let rendered = String::from_utf8(rewritten).expect("the body is text");
                assert!(
                    !rendered.contains(DATA_MEMBER),
                    "case {case:?}: the member must be gone, got {rendered}"
                );
            }
        }
    }

    /// Every other member survives the rewrite byte for byte, so a caller still reads the
    /// repository's schema and the page's token out of an empty result.
    #[test]
    fn the_rest_of_the_body_survives_the_rewrite() {
        let body = r#"{"Fields":[{"Name":"rtt_p50","Type":"bigint"}],"Data":[],"NextToken":"t"}"#;
        let rewritten =
            without_an_empty_repository(body.as_bytes()).expect("the member must be dropped");
        let rendered = String::from_utf8(rewritten).expect("the body is text");
        for kept in [r#""Name":"rtt_p50""#, r#""Type":"bigint""#, r#""NextToken":"t""#] {
            assert!(
                rendered.contains(kept),
                "expected {kept:?} kept, got {rendered}"
            );
        }
    }
}
