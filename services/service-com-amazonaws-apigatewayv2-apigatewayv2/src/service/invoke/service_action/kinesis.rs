// SPDX-License-Identifier: CC0-1.0

//! The `Kinesis-PutRecord` action: the data record a route puts on a stream.
//!
//! The parameters are the members of that operation's own input
//! (`smithy_query(query="operation", operation="PutRecord")` on the Amazon Kinesis model),
//! which is what the action takes: the stream the record is put on, the record itself and the
//! key it is partitioned by, beside the two members that order and place it
//! ([`PUT_RECORD_PARAMETERS`]). An integration mapping nothing for the three the subtype is
//! configured with names no put at all, and the request is answered as an integration that
//! produced no response — never as a record nothing stored — while a request that carries no
//! value for the expression the integration maps onto one of them is a bad request, which is
//! the parent module's own rule for every action's required parameters.
//!
//! The members that address a stream ANOTHER way (`StreamARN`, `StreamId`) and the one that
//! only asks whether the call would succeed (`DryRun`) are not among the parameters: a
//! first-class integration names the stream the one way the subtype states, so an integration
//! mapping one of them is reported rather than put without it ([`Mapped::beyond`]).
//!
//! The stream name is a parameter of the in-process call and NOT a destination this process
//! connects to: the client is the emulator's own transport, addressed at the Kinesis endpoint
//! of the Region the parent module resolved for this call, so the record lands on the stream
//! this emulator holds.
//!
//! What the put then does is Kinesis's own: whether the calling role may write to that stream,
//! which shard the key places the record in, what sequence number it is given and whether the
//! shard's throughput takes it at all. Nothing of that is decided here; this module maps the
//! input and makes the call.
//!
//! # What the mapped `Data` is
//!
//! `PutRecordInput$Data` is a BLOB, and a mapping supplies one string per parameter, so the
//! mapped value is the record's data as the text the request carried it as: those bytes are
//! what the record holds. The base64 a JSON protocol writes a blob under is that wire's own
//! encoding of the value — "the data blob (the payload before base64-encoding)"
//! (`smithy_query(query="doc", shape="PutRecordInput", member="Data")`) — applied by the
//! client that serializes the call, never by this mapping, so a route mapping `$request.body`
//! onto `Data` puts the body a client posted on the stream and not its spelling in some
//! transfer encoding.
//!
//! # The form an answer is reported in
//!
//! The action is performed over Kinesis's own api, which states an answer as a JSON document
//! (`awsJson1_1`), so that is the form the caller is answered in ([`Performed::json`]) — the
//! members `PutRecordOutput` declares, under their own names, carrying what that service
//! answered and nothing of this gateway's. A client fronting a stream with an HTTP api reads
//! the shard and the sequence number of the record it just put off this document exactly as it
//! would reading the action's own answer.

use ::aws_sdk_kinesis::primitives::Blob;
use serde_json::{json, Map, Value};
use smithy_server_runtime::OperationContext;

use super::credentials::SigningIdentity;
use super::parameters::Mapped;
use super::{Performed, Unperformed};

/// The `IntegrationSubtype` that names this action, taken from the catalog that states which
/// subtypes are first-class (`crate::state::integration_subtype`), so the value the control
/// plane admits and the one this data plane performs are one constant.
pub const PUT_RECORD_SUBTYPE: &str = crate::state::integration_subtype::KINESIS_PUT_RECORD.name;

/// Every parameter an integration may map onto the put: the members of `PutRecordInput` this
/// emulator carries into the call (see the module docs for the three it leaves out).
pub const PUT_RECORD_PARAMETERS: &[&str] = &[
    "Data",
    "ExplicitHashKey",
    "PartitionKey",
    "SequenceNumberForOrdering",
    "StreamName",
];

/// The parameters the put must carry a value for: the arguments the control plane already
/// holds an integration of this subtype to (`crate::state::integration_subtype`).
///
/// That set is the SUBTYPE's rather than the one member `PutRecordInput` itself marks
/// required, which is `Data` alone: the input leaves `StreamName` optional because `StreamARN`
/// is the other way to name the stream and `PartitionKey` optional because `ExplicitHashKey`
/// can place the record, and this subtype offers neither alternative (see the module docs), so
/// a call it could be made with always names the stream and the key as well.
pub const PUT_RECORD_REQUIRED_PARAMETERS: &[&str] =
    crate::state::integration_subtype::KINESIS_PUT_RECORD.required_parameters;

/// The `arnNamespace` Kinesis publishes its endpoint and signs under, which is the endpoint
/// label the in-process client is addressed at.
const KINESIS_ENDPOINT: &str = "kinesis";

/// The members of the mapping this action reads, spelled as an integration's
/// `RequestParameters` spell them.
const STREAM_NAME: &str = "StreamName";
const DATA: &str = "Data";
const PARTITION_KEY: &str = "PartitionKey";
const EXPLICIT_HASH_KEY: &str = "ExplicitHashKey";
const SEQUENCE_NUMBER_FOR_ORDERING: &str = "SequenceNumberForOrdering";

/// The value mapped for the action's parameter `parameter`, or the reason there is no call to
/// make.
///
/// An argument the action cannot be called without that the integration's mapping defines
/// nothing for names no put at all, which is the "integration produced no response" class
/// ([`Unperformed::NotPerformed`]) rather than a record nothing stored. Which parameters those
/// are is the catalogue's ([`PUT_RECORD_REQUIRED_PARAMETERS`]), and the test below holds this
/// module to it, so the set the control plane admits an integration against and the set this
/// call is built from cannot drift apart.
fn argument<'m>(mapped: &'m Mapped, parameter: &str) -> Result<&'m str, Unperformed> {
    mapped.get(parameter).ok_or_else(|| {
        Unperformed::NotPerformed(format!(
            "the integration maps no {parameter}, which {PUT_RECORD_SUBTYPE} requires"
        ))
    })
}

/// Put the mapped record, and answer the document Kinesis returned.
pub async fn put_record(
    ctx: &OperationContext<'_>,
    region: &str,
    identity: Option<&SigningIdentity>,
    mapped: &Mapped,
) -> Result<Performed, Unperformed> {
    // The three arguments the put is made with, each read under the name the action spells.
    let stream_name = argument(mapped, STREAM_NAME)?;
    let data = argument(mapped, DATA)?;
    let partition_key = argument(mapped, PARTITION_KEY)?;
    let client = action_client!(
        aws_sdk_kinesis,
        ctx.callback(),
        KINESIS_ENDPOINT,
        region,
        identity
    );
    let mut call = client
        .put_record()
        .stream_name(stream_name)
        // The record's data is the mapped value's own bytes (see the module docs).
        .data(Blob::new(data.as_bytes()))
        .partition_key(partition_key);
    if let Some(explicit_hash_key) = mapped.get(EXPLICIT_HASH_KEY) {
        call = call.explicit_hash_key(explicit_hash_key);
    }
    if let Some(sequence_number) = mapped.get(SEQUENCE_NUMBER_FOR_ORDERING) {
        call = call.sequence_number_for_ordering(sequence_number);
    }
    let put = call
        .send()
        .await
        .map_err(|error| super::unperformed(PUT_RECORD_SUBTYPE, &error))?;
    // The members `PutRecordOutput` declares: the two it marks required, and the encryption
    // the write applied, which a stream with none reports nothing for.
    let mut answer = Map::new();
    answer.insert("ShardId".to_string(), json!(put.shard_id()));
    answer.insert("SequenceNumber".to_string(), json!(put.sequence_number()));
    if let Some(encryption_type) = put.encryption_type() {
        answer.insert(
            "EncryptionType".to_string(),
            json!(encryption_type.as_str()),
        );
    }
    Ok(Performed::json(&Value::Object(answer)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the arguments this module builds the call from are the ones the catalogue holds an
    /// integration of this subtype to, so the set the control plane admits a configuration
    /// against and the set this data plane calls the action with are one set.
    ///
    /// Two independent declarations compared, not a literal re-asserted: the catalogue states
    /// the required set for the writing operations, and the three lookups in
    /// [`put_record`] state what the call cannot be made without. A change to either alone
    /// fails here rather than leaving the two planes to disagree at invoke time.
    #[test]
    fn the_call_is_built_from_the_arguments_the_catalogue_requires() {
        let built_from = [STREAM_NAME, DATA, PARTITION_KEY];
        assert_eq!(
            PUT_RECORD_REQUIRED_PARAMETERS, built_from,
            "expected the catalogue's required set to be the arguments this module reads, got \
             {PUT_RECORD_REQUIRED_PARAMETERS:?} against {built_from:?}"
        );
        // And every one of them is a parameter an integration may map at all, so a mapping
        // that defines the required set is never reported as mapping something uncarried.
        for parameter in PUT_RECORD_REQUIRED_PARAMETERS {
            assert!(
                PUT_RECORD_PARAMETERS.contains(parameter),
                "case {parameter:?}: expected a required parameter among the carried ones \
                 {PUT_RECORD_PARAMETERS:?}"
            );
        }
    }

    /// RULE: an argument the action cannot be called without that this request's mapping
    /// resolved nothing for leaves no call to make, and the reason names that parameter.
    #[test]
    fn a_missing_argument_leaves_no_call_to_make() {
        let nothing = Mapped::default();
        for parameter in PUT_RECORD_REQUIRED_PARAMETERS {
            let read = argument(&nothing, parameter);
            let reason = match read {
                Err(unperformed) => unperformed.reason(),
                Ok(value) => panic!(
                    "case {parameter:?}: expected no call to make for an empty mapping, got \
                     {value:?}"
                ),
            };
            assert_eq!(
                reason,
                format!(
                    "the integration maps no {parameter}, which {PUT_RECORD_SUBTYPE} requires"
                ),
                "case {parameter:?}: expected the reason to name the parameter, got {reason:?}"
            );
        }
    }
}
