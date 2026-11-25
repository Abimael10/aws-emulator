// SPDX-License-Identifier: CC0-1.0

//! Which instance-collection vocabulary a cluster is described in, and how Amazon EMR
//! refuses an operation of the OTHER one.
//!
//! One concern, split out of [`super`]: "the `RunJobFlow` request can contain
//! `InstanceFleets` parameters or `InstanceGroups` parameters, but not both" (`RunJobFlow`
//! doc, Smithy model), so a cluster carries instance fleets or instance groups and never
//! both — which is what its `InstanceCollectionType` names
//! ([`instance_collection_type`]). The exclusion holds for the whole life of the cluster,
//! not only for its launch: an operation that reads or changes the collection a cluster
//! does NOT carry is refused for the cluster's vocabulary and pointed at the operation that
//! serves the one it does ([`wrong_instance_collection_refusal`]), rather than answered
//! with an empty projection — which would tell a caller probing a cluster's topology that
//! it is the other one.
//!
//! The sentence is recorded behavior; its fixed text and its variable parts are written on
//! [`instance_collection_mismatch_message`].

use super::*;

/// `ListInstanceFleets`, which serves the fleet vocabulary's listing.
///
/// Named here, beside the counterpart below, because a refusal POINTS a caller at the other
/// operation: the pair is the one fact both listings need and neither owns.
pub const LIST_INSTANCE_FLEETS_OPERATION: &str = "ListInstanceFleets";

/// `ListInstanceGroups`, which serves the group vocabulary's listing.
pub const LIST_INSTANCE_GROUPS_OPERATION: &str = "ListInstanceGroups";

/// How Amazon EMR names one instance-collection vocabulary in a sentence: the collection a
/// cluster of that `InstanceCollectionType` carries.
///
/// `INSTANCE_FLEET` clusters carry "instance fleets" and every other cluster carries
/// "instance groups" — the two values `com.amazonaws.emr#InstanceCollectionType` declares,
/// read as the plural noun the refusal speaks of.
pub fn instance_collection_noun(collection_type: &str) -> &'static str {
    if collection_type == instance_collection_type::INSTANCE_FLEET {
        return "fleets";
    }
    "groups"
}

/// How Amazon EMR refuses an operation of one instance-collection vocabulary on a cluster
/// described in the other.
///
/// Recorded behavior for the text. Fixed: the mutual-exclusion statement, the clause that
/// says the operation does not support such clusters, and the clause that names what to
/// call instead. Variable: the collection the cluster REALLY carries, read off its own
/// record ([`instance_collection_noun`]), the operation the request called, and the
/// operation that serves that collection — so the same sentence serves either direction of
/// the exclusion and carries nothing else of the request.
pub fn instance_collection_mismatch_message(
    cluster_collection_type: &str,
    operation: &str,
    counterpart_operation: &str,
) -> String {
    let carried = instance_collection_noun(cluster_collection_type);
    format!(
        "Instance fleets and instance groups are mutually exclusive. The EMR cluster \
         specified in the request uses instance {carried}. The {operation} operation does \
         not support clusters that use instance {carried}. Use the \
         {counterpart_operation} operation instead."
    )
}

/// The refusal an operation serving the `served_collection_type` vocabulary earns on a
/// cluster described in the other one, or `None` when the cluster is described in the
/// vocabulary the operation serves.
///
/// `cluster_collection_type` is the resolved cluster's own
/// `InstancesRecord::instance_collection_type`, settled by the launch that created it;
/// `operation` is the operation the request called and `counterpart_operation` the one that
/// serves the collection the cluster carries. The refusal travels under the client error
/// the calling operation models, and carries no `ErrorCode`: Amazon EMR publishes no
/// discriminator for this class, the way it does for an unresolvable cluster id
/// ([`unknown_cluster`]).
pub fn wrong_instance_collection_refusal(
    cluster_collection_type: &str,
    served_collection_type: &str,
    operation: &str,
    counterpart_operation: &str,
) -> Option<InvalidRequestException> {
    (cluster_collection_type != served_collection_type).then(|| InvalidRequestException {
        error_code: None,
        message: Some(instance_collection_mismatch_message(
            cluster_collection_type,
            operation,
            counterpart_operation,
        )),
    })
}
