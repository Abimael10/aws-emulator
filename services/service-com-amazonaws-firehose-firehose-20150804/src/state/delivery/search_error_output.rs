// SPDX-License-Identifier: CC0-1.0

//! The error-output document of one document an Amazon OpenSearch Service destination did not
//! take: what a consumer of the destination's Amazon S3 backup reads.
//!
//! "Firehose writes any documents that could not be indexed to the configured Amazon S3
//! destination, with `AmazonOpenSearchService-failed/` appended to the key prefix"
//! (`S3BackupMode`) — and what it writes there, per document, is the envelope below: how the
//! indexing failed, when it was attempted, which index and document id it was attempted
//! under, and the record's own bytes.
//!
//! The envelope is written in the DESTINATION KIND's own vocabulary, and that decides both
//! halves of it alike: an Amazon OpenSearch Service destination
//! (`AmazonopensearchserviceDestinationConfiguration`) reports its codes under `OS.`
//! ([`super::search_error_code`]) and names the index it used `osIndexName`, while the same
//! offering under its legacy Elasticsearch name (`ElasticsearchDestinationConfiguration`)
//! reports `ES.` and names it `esIndexName`.
//!
//! None of this is modelled: Smithy carries the destination configuration that SELECTS the
//! backup and not the envelope written under it, so what is below is the destination's
//! documented behaviour rather than a shape of the model.

use super::*;

/// The error output ONE Amazon OpenSearch Service destination kind is written in.
///
/// Two values, one per kind ([`OPENSEARCH_SERVICE_OUTPUT`], [`ELASTICSEARCH_OUTPUT`]): the
/// two kinds are the same offering under its current and its old name and deliver through one
/// path, so what differs between them is only the vocabulary their error output is written
/// in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchErrorOutput {
    /// The codes this kind reports a failure under.
    codes: search_error_code::SearchErrorCodes,
    /// What this kind's members naming the delivery's index and document id begin with.
    member_prefix: &'static str,
}

/// The error output of an `Amazonopensearchservice` destination.
pub(super) const OPENSEARCH_SERVICE_OUTPUT: SearchErrorOutput = SearchErrorOutput {
    codes: search_error_code::OPENSEARCH_SERVICE_CODES,
    member_prefix: "os",
};

/// The error output of an `Elasticsearch` destination — the same offering under its legacy
/// name.
pub(super) const ELASTICSEARCH_OUTPUT: SearchErrorOutput = SearchErrorOutput {
    codes: search_error_code::ELASTICSEARCH_CODES,
    member_prefix: "es",
};

impl SearchErrorOutput {
    /// The `errorCode` for a document the CLUSTER refused, from the engine's own error type.
    #[must_use]
    pub fn rejected(self, engine_error_type: &str) -> String {
        self.codes.rejected(engine_error_type)
    }

    /// The `errorCode` for a buffer whose destination domain answered NOTHING.
    #[must_use]
    pub fn unavailable(self) -> String {
        self.codes.unavailable()
    }

    /// The member naming the index the delivery wrote to.
    #[must_use]
    pub fn index_member(self) -> String {
        format!("{}IndexName", self.member_prefix)
    }

    /// The member naming the document id the delivery wrote under.
    #[must_use]
    pub fn document_id_member(self) -> String {
        format!("{}DocumentId", self.member_prefix)
    }
}

/// The error-output document of one document the domain did not index.
///
/// The common members are the ones every Firehose error output carries (`errorCode`,
/// `errorMessage`, `attemptsMade`, `arrivalTimestamp`, `attemptEndingTimestamp`, `rawData`),
/// and the two the error output of an Amazon OpenSearch Service destination adds are the
/// index and the document id the delivery used, each named in this destination kind's own
/// vocabulary. `errorCode` is Firehose's OWN code for the class the refusal falls under, and
/// the cluster's own error type and wording are what `errorMessage` carries.
pub(super) fn envelope(
    record: Option<&StoredRecord>,
    failure: &SearchDocumentFailure,
    output: SearchErrorOutput,
    now: SystemTime,
) -> ::serde_json::Value {
    let mut document = ::serde_json::json!({
        "attemptsMade": attempts_made(record),
        "arrivalTimestamp": record.map_or_else(|| epoch_millis(now), |record| epoch_millis(record.received_at)),
        "errorCode": failure.error_code,
        "errorMessage": failure.error_message,
        "attemptEndingTimestamp": epoch_millis(now),
        "rawData": record.map(|record| ::aws_smithy_types::base64::encode(&record.data)).unwrap_or_default(),
    });
    document[output.document_id_member()] = failure.document_id.clone().into();
    document[output.index_member()] = failure.index_name.clone().into();
    document
}
