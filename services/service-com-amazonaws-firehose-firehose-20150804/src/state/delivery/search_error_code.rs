// SPDX-License-Identifier: CC0-1.0

//! The `errorCode` a document an Amazon OpenSearch Service destination did not take is
//! reported under.
//!
//! Firehose reports a failed delivery under ITS OWN code, never under the cluster's: the
//! error-output document of a document the domain refused ([`super::search_error_output`])
//! carries an `errorCode` in the destination's own error namespace, followed by the Firehose
//! exception name the cluster's rejection falls under. The cluster's own error type and its
//! wording are what that envelope's `errorMessage` states, so naming the CLASS in the code
//! loses nothing a consumer of the backup had.
//!
//! The namespace is the destination's: `OS.` for an Amazon OpenSearch Service destination
//! (`AmazonopensearchserviceDestinationConfiguration`) and `ES.` for the same offering under
//! its legacy Elasticsearch name (`ElasticsearchDestinationConfiguration`). The form is the
//! one every other destination of this emulator already reports its failures in (`S3.…`,
//! `Splunk.…`, `Lambda.…`), stated here for the one destination whose failures are decided by
//! another service's engine rather than by this one.
//!
//! None of this is modelled: Smithy carries the destination configuration that SELECTS the
//! backup (`AmazonopensearchserviceDestinationConfiguration$S3BackupMode`, with its required
//! `S3Configuration`) and not the envelope written under it, so the codes below are the
//! destination's documented behaviour rather than a shape of the model.

use ::service_com_amazonaws_opensearch_amazonopensearchservice::state::ingest;

/// The error codes ONE Amazon OpenSearch Service destination kind reports under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchErrorCodes {
    /// The namespace every code of this destination kind begins with, trailing separator
    /// and all.
    namespace: &'static str,
    /// The name a document the index's mappings could not take is reported under, which
    /// carries the destination service's own exception family.
    parse_exception: &'static str,
}

/// The codes an `Amazonopensearchservice` destination reports under.
pub(super) const OPENSEARCH_SERVICE_CODES: SearchErrorCodes = SearchErrorCodes {
    namespace: "OS.",
    parse_exception: "AmazonOpenSearchServiceParseException",
};

/// The codes an `Elasticsearch` destination — the same offering under its legacy name —
/// reports under.
pub(super) const ELASTICSEARCH_CODES: SearchErrorCodes = SearchErrorCodes {
    namespace: "ES.",
    parse_exception: "AmazonElasticsearchParseException",
};

/// The exception name a document the cluster would not let the destination's role write is
/// reported under: the delivery is a bulk write, and what was denied is the permission to
/// make it.
const BULK_WRITE_PERMISSION_DENIED: &str = "BulkWritePermissionDenied";

/// The exception name every other refusal the cluster states is reported under — the class a
/// consumer of the backup treats as "the destination said no, for a reason of its own".
const SERVICE_EXCEPTION: &str = "ServiceException";

/// The exception name a buffer the destination never answered at all is reported under: no
/// cluster judged those documents, so the failure is of the destination and not of a
/// document.
const SERVICE_UNAVAILABLE: &str = "ServiceUnavailable";

impl SearchErrorCodes {
    /// The code for a document the CLUSTER refused, from the engine's own error type.
    ///
    /// The mapping is by failure class, not by spelling: an unauthorized write is the
    /// permission-denied class whatever the security plugin called it, a document the index's
    /// mappings could not take — an unmapped field under `strict` mappings, or a value a
    /// mapped type could not parse — is the parse class, and everything else the cluster
    /// states is the destination's own service exception.
    #[must_use]
    pub fn rejected(self, engine_error_type: &str) -> String {
        let exception = match engine_error_type {
            ingest::SECURITY_EXCEPTION => BULK_WRITE_PERMISSION_DENIED,
            ingest::MAPPER_PARSING_EXCEPTION | ingest::STRICT_DYNAMIC_MAPPING_EXCEPTION => {
                self.parse_exception
            }
            _ => SERVICE_EXCEPTION,
        };
        format!("{}{exception}", self.namespace)
    }

    /// The code for a buffer whose destination domain answered NOTHING — it is not
    /// provisioned, or this emulator holds no domain at that address — which is parked once
    /// the destination's retry window is spent.
    #[must_use]
    pub fn unavailable(self) -> String {
        format!("{}{SERVICE_UNAVAILABLE}", self.namespace)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code either destination kind reports begins with that kind's namespace, which is
    /// what a consumer of the failed-document backup discriminates on, and none of them
    /// carries the engine's own error type.
    #[test]
    fn every_code_is_namespaced_for_its_destination() {
        let kinds = [
            ("Amazonopensearchservice", OPENSEARCH_SERVICE_CODES, "OS."),
            ("Elasticsearch", ELASTICSEARCH_CODES, "ES."),
        ];
        // The engine error types a write of one document answers with, plus one this emulator
        // does not place.
        let engine_types = [
            ingest::SECURITY_EXCEPTION,
            ingest::MAPPER_PARSING_EXCEPTION,
            ingest::STRICT_DYNAMIC_MAPPING_EXCEPTION,
            ingest::INDEX_NOT_FOUND_EXCEPTION,
            ingest::ILLEGAL_ARGUMENT_EXCEPTION,
            "a_type_this_emulator_does_not_place",
        ];
        for (kind, codes, namespace) in kinds {
            for engine_type in engine_types {
                let code = codes.rejected(engine_type);
                assert!(
                    code.starts_with(namespace),
                    "case {kind:?}/{engine_type:?}: expected a code in {namespace:?}, got \
                     {code:?}"
                );
                assert!(
                    !code.contains(engine_type),
                    "case {kind:?}/{engine_type:?}: expected the engine's own type kept out of \
                     the code, got {code:?}"
                );
            }
            let unavailable = codes.unavailable();
            assert!(
                unavailable.starts_with(namespace),
                "case {kind:?}: expected the unavailable code in {namespace:?}, got \
                 {unavailable:?}"
            );
        }
    }

    /// The class the code names is the class of the cluster's rejection: a denial and a
    /// mapping conflict are reported under different exception names, and the two mapping
    /// conflicts under the same one.
    #[test]
    fn the_code_names_the_class_of_the_rejection() {
        let codes = OPENSEARCH_SERVICE_CODES;
        let denied = codes.rejected(ingest::SECURITY_EXCEPTION);
        let unparsable = codes.rejected(ingest::MAPPER_PARSING_EXCEPTION);
        let unmapped = codes.rejected(ingest::STRICT_DYNAMIC_MAPPING_EXCEPTION);
        assert_ne!(
            denied, unparsable,
            "case \"denial vs mapping conflict\": expected different exception names, got \
             {denied:?}"
        );
        assert_eq!(
            unparsable, unmapped,
            "case \"both mapping conflicts\": expected one parse exception name, got \
             {unparsable:?} and {unmapped:?}"
        );
        assert_eq!(
            codes.rejected(ingest::INDEX_NOT_FOUND_EXCEPTION),
            format!("OS.{SERVICE_EXCEPTION}"),
            "case \"a refusal of no other class\": expected the destination's service exception"
        );
    }
}
