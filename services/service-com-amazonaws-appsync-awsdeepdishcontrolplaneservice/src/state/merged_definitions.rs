// SPDX-License-Identifier: CC0-1.0

//! What RESOLVES a field of a Merged API: the definitions a merge carried into it beside the
//! source API's types.
//!
//! A merge is "the merge of the source API's schema into the Merged API" (`StartSchemaMerge`),
//! and what a Merged API then SERVES at its own `GRAPHQL` endpoint is that schema — so the
//! merge has to carry what answers those fields as well as what declares them: the resolvers
//! attached to the source API's fields, the functions a pipeline of one invokes and the data
//! sources a step of one calls. A merge that carried the declarations alone would leave a
//! Merged API whose schema type-checks and whose every field resolves to nothing.
//!
//! The definitions are a SNAPSHOT, taken at the instant the merge came due, held beside the
//! types that merge carried ([`super::SourceApiAssociationRecord::merged_definitions`]) and
//! replaced whole by the next merge. That is what makes a change in a source API reach the
//! Merged API through a MERGE rather than the moment it is written, and what withdraws a
//! source API's definitions with its association.
//!
//! [`Definitions`] is the one view a resolution reads them through, so a field of a standard
//! API and a merged field of a Merged API are resolved from the same three maps and nothing
//! has to know which it was handed.

use std::collections::BTreeMap;

use super::{
    DataSourceRecord, FunctionRecord, GraphqlApiRecord, ResolverKey, ResolverRecord,
    SourceApiAssociationRecord,
};

/// The definitions ONE performed merge carried into a Merged API: the source API's resolvers,
/// the functions they invoke and the data sources their steps call, as they stood when the
/// merge came due.
///
/// Empty while no merge of the association has been performed, and for one whose contribution
/// could not be composed into the Merged API's schema — a merge that carried no schema
/// carries nothing that resolves one either.
///
/// Bounded by what one source API may hold: the per-API quotas on resolvers, functions and
/// data sources bound each map, and a source API takes part in at most one association.
#[derive(Debug, Clone, Default)]
pub struct MergedDefinitions {
    /// The resolvers attached to the source API's fields, keyed as that API keys them.
    pub resolvers: BTreeMap<ResolverKey, ResolverRecord>,
    /// The functions of the source API, keyed by `functionId`, which a merged pipeline
    /// resolver names its steps by.
    pub functions: BTreeMap<String, FunctionRecord>,
    /// The data sources of the source API, keyed by name, which a merged step calls.
    pub data_sources: BTreeMap<String, DataSourceRecord>,
}

impl MergedDefinitions {
    /// The definitions `api` holds right now, as a merge of it carries them.
    pub fn of(api: &GraphqlApiRecord) -> Self {
        Self {
            resolvers: api.resolvers.clone(),
            functions: api.functions.clone(),
            data_sources: api.data_sources.clone(),
        }
    }

    /// This snapshot as the view a resolution reads definitions through.
    pub fn view(&self) -> Definitions<'_> {
        Definitions {
            resolvers: &self.resolvers,
            functions: &self.functions,
            data_sources: &self.data_sources,
        }
    }
}

/// The three maps a resolution reads: whichever API — or whichever merge — holds them.
#[derive(Debug, Clone, Copy)]
pub struct Definitions<'a> {
    pub resolvers: &'a BTreeMap<ResolverKey, ResolverRecord>,
    pub functions: &'a BTreeMap<String, FunctionRecord>,
    pub data_sources: &'a BTreeMap<String, DataSourceRecord>,
}

impl Definitions<'_> {
    /// The resolver attached to `field_name` of `type_name`, or `None` when these
    /// definitions hold none for that field.
    pub fn resolver(&self, type_name: &str, field_name: &str) -> Option<&ResolverRecord> {
        self.resolvers
            .get(&(type_name.to_string(), field_name.to_string()))
    }
}

impl GraphqlApiRecord {
    /// The definitions this API holds in its OWN right, as the view a resolution reads them
    /// through.
    pub fn definitions(&self) -> Definitions<'_> {
        Definitions {
            resolvers: &self.resolvers,
            functions: &self.functions,
            data_sources: &self.data_sources,
        }
    }
}

/// Which association's merge carried the definition of one field into a Merged API, and what
/// it carried: the source API that contributed the field, and the definitions to resolve it
/// from.
#[derive(Debug, Clone, Copy)]
pub struct MergedField<'a> {
    /// The source API the field came from, as the association holds it.
    pub source_api_id: &'a str,
    /// That API's own ARN, which is the resource an execution of this field is authorized
    /// against.
    pub source_api_arn: &'a str,
    /// The definitions that merge carried, which the field is resolved from.
    pub definitions: Definitions<'a>,
}

impl super::ComAmazonawsAppsyncAwsdeepdishcontrolplaneserviceState {
    /// The merge that carried the definition of `field_name` of `type_name` into the Merged
    /// API `merged_api_id`, or `None` when no association of it carries one.
    ///
    /// Read in the order the Merged API's schema is COMPOSED in
    /// ([`SourceApiAssociationRecord::merge_order`]), so the association that is answered is
    /// the one whose merge installed that field — the first contribution holding it — and not
    /// whichever association the store happens to be walked in the order of. An association
    /// whose deletion the clock already owes still answers for its fields until that deletion
    /// comes due, which is when its whole contribution is withdrawn.
    pub fn merged_field(
        &self,
        merged_api_id: &str,
        type_name: &str,
        field_name: &str,
    ) -> Option<MergedField<'_>> {
        let mut contributions: Vec<&SourceApiAssociationRecord> = self
            .source_api_associations
            .values()
            .filter(|association| association.merged_api_id == merged_api_id)
            .collect();
        contributions.sort_by_key(|association| (association.merge_order, association.seq));
        contributions
            .into_iter()
            .find(|association| {
                association
                    .merged_definitions
                    .view()
                    .resolver(type_name, field_name)
                    .is_some()
            })
            .map(|association| MergedField {
                source_api_id: &association.source_api_id,
                source_api_arn: &association.source_api_arn,
                definitions: association.merged_definitions.view(),
            })
    }
}
