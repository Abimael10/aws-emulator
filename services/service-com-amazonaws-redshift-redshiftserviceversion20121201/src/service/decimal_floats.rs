// SPDX-License-Identifier: CC0-1.0

//! The floating-point members of this service's responses, rendered the way AWS renders
//! one on the XML wire: as a decimal carrying a fractional part.
//!
//! # The gap this closes
//!
//! A Smithy `double` leaves an AWS XML response as a decimal — a whole value reads `0.0`,
//! `3743.6`, never `0` — while Rust's own rendering drops the fractional part of a whole
//! value (`0.0f64.to_string() == "0"`), and the framework's XML writer hands the value
//! straight to `to_string()`. So every whole-valued floating-point member this service
//! answered with left it in a form no AWS service answers with: a reserved node offering
//! charging nothing per running hour reported `<UsagePrice>0</UsagePrice>` where AWS
//! reports `0.0`, and a client reading the text of such a member — a price, a recurring
//! charge, a backup size, a transfer rate — saw an integer where the API documents a
//! decimal.
//!
//! The rule is the PROTOCOL's, not this service's, and the writer it belongs in cannot be
//! edited here (`vendor/` is read-only), so it is applied at the one seam a service owns:
//! each operation below is served behind
//! [`service_common::query_decimal_floats::DecimalFloatsOperation`], which runs the
//! framework's own request walk and changes nothing but how a floating-point leaf of the
//! response is written. The rule itself lives in that shared module — this one is the
//! wiring, and names no rendering of its own.
//!
//! # Which operations are served through it
//!
//! Every operation whose MODELLED OUTPUT can reach a floating-point member. This model
//! declares one floating-point shape and its optional form
//! (`smithy_query(query="shape", shape="Double")` — a `double`), and nine structures carry
//! a member of it: `ReservedNodeOffering` and `ReservedNode` (`FixedPrice`, `UsagePrice`),
//! `RecurringCharge` (`RecurringChargeAmount`), `Snapshot`
//! (`TotalBackupSizeInMegaBytes`, `ActualIncrementalBackupSizeInMegaBytes`,
//! `BackupProgressInMegaBytes`, `CurrentBackupRateInMegaBytesPerSecond`),
//! `CustomerStorageMessage` (`TotalBackupSizeInMegaBytes`,
//! `TotalProvisionedStorageInMegaBytes`), `ResizeProgressMessage`
//! (`AvgResizeRateInMegaBytesPerSecond`, `DataTransferProgressPercent`), `RestoreStatus`
//! (`CurrentRestoreRateInMegaBytesPerSecond`), `DataTransferProgress`
//! (`CurrentRateInMegaBytesPerSecond`) and `NodeConfigurationOption`
//! (`EstimatedDiskUtilizationPercent`).
//!
//! Four groups of operations reach one of those structures, which is why the table below
//! is grouped the way it is:
//!
//! * every operation reporting a `Cluster`, which carries a `RestoreStatus` and a
//!   `DataTransferProgress`;
//! * every operation reporting a `Snapshot`;
//! * every operation reporting a `ReservedNode` or a `ReservedNodeOffering`, the recurring
//!   charges of either among them;
//! * the three reads whose own output shape carries a double:
//!   `CustomerStorageMessage` (`DescribeStorage`), `ResizeProgressMessage`
//!   (`DescribeResize`, `CancelResize`) and `NodeConfigurationOption`
//!   (`DescribeNodeConfigurationOptions`).
//!
//! An operation whose output declares no floating-point member is not listed, because for
//! it the wrapper is a no-op: the rule reaches a value only where the model puts one.

use std::sync::Arc;

use service_common::query_decimal_floats::DecimalFloatsOperation;
use smithy_server_runtime::{
    ErasedOperation, Operation, OperationRoutingTable, PreDispatchProtocol,
};

/// The envelope a pre-dispatch failure of a wrapped operation renders in: this service
/// speaks `awsQuery` (not the EC2 dialect), which is the same value the generated
/// per-operation shim passes.
const QUERY_ENVELOPE: PreDispatchProtocol<'static> = PreDispatchProtocol::Query { ec2: false };

/// The operations of this service whose response can carry a floating-point member, each
/// served behind the renderer above, keyed by the routing key the protocol resolves.
///
/// A PARTIAL table: a request it does not hold is served by the generated routing table
/// unchanged (see [`crate::service::query_error_envelope`]). Each row is filed under the
/// operation's own [`Operation::NAME`], the model-derived key the generated table uses, so
/// a wrapped operation cannot land under a key the protocol never resolves.
pub fn table<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) -> OperationRoutingTable {
    use crate::ops;

    let mut table = OperationRoutingTable::new();

    // A reported `Cluster` carries a `RestoreStatus` and a `DataTransferProgress`.
    serve(&mut table, ops::create_cluster::CreateCluster::new(Arc::clone(engine)));
    serve(&mut table, ops::delete_cluster::DeleteCluster::new(Arc::clone(engine)));
    serve(&mut table, ops::describe_clusters::DescribeClusters::new(Arc::clone(engine)));
    serve(&mut table, ops::modify_cluster::ModifyCluster::new(Arc::clone(engine)));
    serve(
        &mut table,
        ops::modify_cluster_db_revision::ModifyClusterDbRevision::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::modify_cluster_iam_roles::ModifyClusterIamRoles::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::modify_cluster_maintenance::ModifyClusterMaintenance::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::modify_snapshot_copy_retention_period::ModifySnapshotCopyRetentionPeriod::new(
            Arc::clone(engine),
        ),
    );
    serve(
        &mut table,
        ops::enable_snapshot_copy::EnableSnapshotCopy::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::disable_snapshot_copy::DisableSnapshotCopy::new(Arc::clone(engine)),
    );
    serve(&mut table, ops::reboot_cluster::RebootCluster::new(Arc::clone(engine)));
    serve(&mut table, ops::resize_cluster::ResizeCluster::new(Arc::clone(engine)));
    serve(&mut table, ops::pause_cluster::PauseCluster::new(Arc::clone(engine)));
    serve(&mut table, ops::resume_cluster::ResumeCluster::new(Arc::clone(engine)));
    serve(
        &mut table,
        ops::restore_from_cluster_snapshot::RestoreFromClusterSnapshot::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::rotate_encryption_key::RotateEncryptionKey::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::failover_primary_compute::FailoverPrimaryCompute::new(Arc::clone(engine)),
    );

    // A reported `Snapshot` carries four backup-size and backup-rate doubles.
    serve(
        &mut table,
        ops::create_cluster_snapshot::CreateClusterSnapshot::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::copy_cluster_snapshot::CopyClusterSnapshot::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::delete_cluster_snapshot::DeleteClusterSnapshot::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::describe_cluster_snapshots::DescribeClusterSnapshots::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::modify_cluster_snapshot::ModifyClusterSnapshot::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::authorize_snapshot_access::AuthorizeSnapshotAccess::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::revoke_snapshot_access::RevokeSnapshotAccess::new(Arc::clone(engine)),
    );

    // A reported `ReservedNode` or `ReservedNodeOffering` carries its prices and the
    // amount of each of its recurring charges.
    serve(
        &mut table,
        ops::purchase_reserved_node_offering::PurchaseReservedNodeOffering::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::describe_reserved_nodes::DescribeReservedNodes::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::describe_reserved_node_offerings::DescribeReservedNodeOfferings::new(Arc::clone(
            engine,
        )),
    );
    serve(
        &mut table,
        ops::accept_reserved_node_exchange::AcceptReservedNodeExchange::new(Arc::clone(engine)),
    );
    serve(
        &mut table,
        ops::get_reserved_node_exchange_offerings::GetReservedNodeExchangeOfferings::new(
            Arc::clone(engine),
        ),
    );
    serve(
        &mut table,
        ops::get_reserved_node_exchange_configuration_options::GetReservedNodeExchangeConfigurationOptions::new(
            Arc::clone(engine),
        ),
    );

    // The reads whose OWN output shape carries a double.
    serve(&mut table, ops::describe_storage::DescribeStorage::new(Arc::clone(engine)));
    serve(&mut table, ops::describe_resize::DescribeResize::new(Arc::clone(engine)));
    serve(&mut table, ops::cancel_resize::CancelResize::new(Arc::clone(engine)));
    serve(
        &mut table,
        ops::describe_node_configuration_options::DescribeNodeConfigurationOptions::new(
            Arc::clone(engine),
        ),
    );

    table
}

/// File one already-constructed operation under its modelled name, served behind the
/// decimal renderer.
fn serve<O: Operation + ErasedOperation + 'static>(
    table: &mut OperationRoutingTable,
    operation: O,
) {
    table.add_operation(
        O::NAME,
        Box::new(DecimalFloatsOperation::new(operation, QUERY_ENVELOPE)),
    );
}

#[cfg(test)]
mod tests {
    use super::table;
    use std::sync::Arc;

    /// Every operation this module serves is filed under the key the protocol resolves —
    /// its modelled operation name — and nothing else is, so a request for an operation
    /// with no floating-point member in its output still reaches the generated table.
    #[test]
    fn each_wrapped_operation_is_filed_under_its_modelled_name() {
        let table = table(&Arc::new(()));
        // One per group of the table: a cluster report, a snapshot report, a reserved-node
        // report, an offering catalog read and each read whose own output shape carries a
        // double.
        for name in [
            "CreateCluster",
            "DescribeClusters",
            "RestoreFromClusterSnapshot",
            "DescribeClusterSnapshots",
            "AuthorizeSnapshotAccess",
            "DescribeReservedNodes",
            "DescribeReservedNodeOfferings",
            "GetReservedNodeExchangeConfigurationOptions",
            "DescribeStorage",
            "DescribeResize",
            "CancelResize",
            "DescribeNodeConfigurationOptions",
        ] {
            assert!(
                table.get(name).is_some(),
                "case {name:?}: expected the operation to be served behind the decimal \
                 renderer"
            );
        }
        // Operations whose output declares no floating-point member, so the generated
        // table serves them unwrapped.
        for name in [
            "DescribeEventCategories",
            "CreateClusterParameterGroup",
            "DescribeClusterVersions",
            "Nonesuch",
        ] {
            assert!(
                table.get(name).is_none(),
                "case {name:?}: expected no row, so the generated table serves it"
            );
        }
    }
}
