// SPDX-License-Identifier: CC0-1.0

//! WHICH transformer is in force for a log group, which is what decides how the events
//! ingested into it are transformed ([`super::transformer`]).
//!
//! A transformer reaches a log group two ways: `PutTransformer` creates one "at the log group
//! level" on the group itself, and `PutAccountPolicy` with a `TRANSFORMER_POLICY` configures
//! one for every group its `selectionCriteria` select. Both are the same pipeline of
//! processors, so both are read here into one answer: the group's OWN transformer where it has
//! one, and otherwise the processors of the account-level policy that applies to it
//! ([`super::applicable_account_policy`]). A group with neither has no transformer, and its
//! events are stored exactly as they were ingested.
//!
//! The processors an account policy carries are the SAME `Processor` blocks `PutTransformer`
//! takes — "a transformer policy must include one JSON block with the array of processors and
//! their configurations" — so they are read with this service's own wire codec and its own
//! `Processor` schema rather than by a second hand-written reading of each processor kind. A
//! block that is not a processor of this model is left out: it configures nothing this service
//! can run.

use ::aws_smithy_schema_server::DeserializableStruct;

use crate::ops::types::Processor;

use super::transformer::Pipeline;
use super::{
    ComAmazonawsCloudwatchlogsLogs20140328State as State, LogGroupRecord, POLICY_TYPE_TRANSFORMER,
};

/// THE EMULATOR'S OWN CEILING on how many processor blocks are read out of one account-level
/// transformer policy document, so what reading one costs is bounded by this rather than by
/// however long the stored document is. `PutTransformer` admits `Processors` of
/// `@length(1..20)`, which is the most a transformer this service runs can hold wherever it
/// was configured.
const MAX_POLICY_PROCESSORS: usize = 20;

/// The processors that transform the log events ingested into `group`, in the order they are
/// applied.
///
/// The group's OWN transformer wins over an account-level one: "if a log group has both an
/// account-level transformer and a log-group level transformer, the log-group level
/// transformer is used". A group with neither names none.
pub fn processors(store: &State, group: &LogGroupRecord) -> Vec<Processor> {
    if let Some(transformer) = group.transformer.as_ref() {
        return transformer.processors.clone();
    }
    super::applicable_account_policy(&store.account_policies, POLICY_TYPE_TRANSFORMER, group)
        .map(|policy| policy_processors(&policy.policy_document))
        .unwrap_or_default()
}

/// Those same processors, prepared for a batch ([`Pipeline::of`]). Empty for a group no
/// transformer is in force for, and then no event of it has a transformed version.
pub fn pipeline(store: &State, group: &LogGroupRecord) -> Pipeline {
    Pipeline::of(&processors(store, group))
}

/// The processors an account-level transformer policy document carries.
///
/// The blocks are read through the one reading of that document's shape
/// ([`super::policy_documents::transformer_policy_processor_blocks`]), the same one the put that
/// stored it judged the document by, so what RUNS here is what was accepted there.
fn policy_processors(document: &str) -> Vec<Processor> {
    super::policy_documents::transformer_policy_processor_blocks(document)
        .unwrap_or_default()
        .iter()
        .take(MAX_POLICY_PROCESSORS)
        .filter_map(processor_of)
        .collect()
}

/// One processor block, read into the modelled `Processor` it configures.
///
/// Read with the codec this service speaks (`awsJson1_1`, which names members by their Smithy
/// member names) against `Processor`'s own schema, so a block naming `parseKeyValue` or
/// `renameKeys` configures exactly what the same block in a `PutTransformer` request does, and
/// a block this model has no processor for configures nothing.
fn processor_of(block: &::serde_json::Value) -> Option<Processor> {
    let encoded = ::serde_json::to_vec(block).ok()?;
    let mut deserializer = ::aws_smithy_schema_server::json::JsonShapeDeserializer::new(&encoded);
    Processor::deserialize(&mut deserializer).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_processors_of_a_policy_document_in_either_form() {
        let bare = r#"[{"parseKeyValue":{"fieldDelimiter":"&","keyValueDelimiter":"="}}]"#;
        let wrapped = format!(r#"{{"transformerConfig":{bare}}}"#);
        for document in [bare, wrapped.as_str()] {
            let read = policy_processors(document);
            assert_eq!(
                read.len(),
                1,
                "case {document:?}: expected one processor, got {read:?}"
            );
            let parse = read[0].parse_key_value.as_ref();
            assert_eq!(
                parse.and_then(|parse| parse.field_delimiter.as_deref()),
                Some("&"),
                "case {document:?}: expected the configured field delimiter, got {read:?}"
            );
        }
    }

    #[test]
    fn a_document_that_carries_no_processor_array_names_no_processor() {
        for document in ["", "{}", "\"not a policy\"", "[\"not a processor\"]"] {
            let read = policy_processors(document);
            assert!(
                read.is_empty(),
                "case {document:?}: expected no processor, got {read:?}"
            );
        }
    }
}
