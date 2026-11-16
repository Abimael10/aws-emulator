// SPDX-License-Identifier: CC0-1.0

//! Whether the customer managed key the ACCOUNT's query results are encrypted under can still
//! serve them — the question every Insights query of this account is answered with.
//!
//! `AssociateKmsKey` puts a key in that path: "Specify the resourceIdentifier parameter with a
//! query-result resource, to use that key to encrypt the stored results of all future
//! StartQuery operations in the account", and `StartQuery` says the same from its own side ("if
//! you have associated a KMS key with the query results in this account, then StartQuery uses
//! that key to encrypt the results when it stores them"). So while such an association stands,
//! no query of the account can be run or read without the key.
//!
//! Using a key is a KMS operation, so what decides whether CloudWatch Logs MAY use it is what
//! KMS answers about the key: whether KMS reports a key it can encrypt with at all
//! ([`super::kms_key_reference`]) and whether the key's own key policy still lets CloudWatch
//! Logs use it for the query-result ARN the key was associated against
//! ([`super::kms_key_grant`]) — the same two questions the association itself was judged by,
//! asked again on every query because either answer can change after the association, and the
//! same pair the read path of a log group's own events is decided by
//! ([`super::event_key_access`]). A key either answer withholds leaves the account's query
//! results unreadable, which is a resource CloudWatch Logs can no longer reach: the call is
//! refused with the modelled `ResourceNotFoundException` rather than answered with the rows,
//! and nothing of the store changes, so disassociating the key — or making it usable again —
//! makes queries run.
//!
//! KMS is asked with NO guard of this service's store held (the two in-process stores are never
//! locked at once). An answer that is not KMS reporting on the key establishes nothing and
//! therefore refuses nothing, exactly as where an association is judged.

/// The sentence a query of an account whose query-result key no longer grants CloudWatch Logs
/// access is refused with. Fixed text for the class: it names neither the key nor the query.
pub const QUERY_RESULTS_KEY_NO_ACCESS_MESSAGE: &str =
    "The KMS Key associated with the Account does not have access on query results ARN";

/// That sentence inside the envelope of the `ResourceNotFoundException` it is refused under, so
/// the status and the code stated in the text are the ones the response answers with.
#[must_use]
pub fn query_results_key_no_access_refusal() -> String {
    super::with_sdk_error_envelope(
        QUERY_RESULTS_KEY_NO_ACCESS_MESSAGE,
        super::ENVELOPE_RESOURCE_NOT_FOUND,
    )
}

/// `Some(message)` when the account's query results cannot be served because the key they are
/// encrypted under is one CloudWatch Logs can no longer use, and `None` when they can — which
/// is every account that has associated no query-result key at all.
///
/// `associated_key` is the key the account's query results are associated with
/// ([`super::ComAmazonawsCloudwatchlogsLogs20140328State::query_results_kms_key_id`]), read
/// under the store's guard and passed here so KMS is asked with that guard released. Both
/// questions are asked of KMS, and either one answering withholds the results; a question KMS
/// does not answer establishes nothing and withholds nothing.
pub async fn refusal_for(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    associated_key: Option<&str>,
) -> Option<String> {
    let key = associated_key?;
    let resource_arn = super::query_results_arn(region);
    let unusable = super::kms_key_reference::refusal_for(ctx, region, key)
        .await
        .is_some()
        || super::kms_key_grant::refusal_for(ctx, region, key, &resource_arn)
            .await
            .is_some();
    unusable.then(query_results_key_no_access_refusal)
}
