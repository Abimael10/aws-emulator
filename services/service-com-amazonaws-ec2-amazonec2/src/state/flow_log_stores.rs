// SPDX-License-Identifier: CC0-1.0

//! The FLOW LOGS this store HOLDS for the account: how many more it has room for, which
//! monitored resource each one is of, whether it already holds one like the ones a create
//! describes, and the insert and removal that change what it holds.
//!
//! A SUBMODULE of the store rather than part of its module root, so that root stays inside the
//! length this workspace holds a file to. What ONE record of such a flow log is, and how one is
//! delivered, are their own concerns ([`super::flow_log_records`],
//! [`super::flow_log_delivery`]); every reader reaches all three the same way, because the root
//! re-exports its parts.

use super::*;

impl ComAmazonawsEc2Amazonec2State {
    /// Whether another `count` flow logs fit inside the store's ceiling
    /// ([`MAX_STORED_RECORDS_PER_STORE`]).
    ///
    /// A COUNT, not one record, because `CreateFlowLogs` stores one flow log per monitored
    /// resource and either stores the whole batch or none of it. The same contract as
    /// [`Self::has_carrier_gateway_capacity`] otherwise: a create asks first and refuses
    /// with [`flow_logs_limit_exceeded`] when it answers `false`.
    pub fn has_flow_log_capacity_for(&self, count: usize) -> bool {
        self.flow_logs.len().saturating_add(count) <= MAX_STORED_RECORDS_PER_STORE
    }

    /// Whether this account holds the resource `resource_id` of the `FlowLogsResourceType`
    /// `resource_type` — the question `CreateFlowLogs` asks of every entry of its
    /// `ResourceIds` before it stores anything.
    ///
    /// Each type is resolved against the ONE store this service keeps that resource in, so
    /// a flow log can never name a resource no describe of that resource would report. A
    /// READ: an id no store holds is not given a resource here.
    pub fn holds_flow_log_resource(&self, resource_type: &str, resource_id: &str) -> bool {
        match resource_type {
            "VPC" => self.vpcs.contains_key(resource_id),
            "Subnet" => self.subnets.contains_key(resource_id),
            "NetworkInterface" => self.network_interfaces.contains_key(resource_id),
            "TransitGateway" => self.holds_transit_gateway_in_service(resource_id),
            "TransitGatewayAttachment" => {
                self.transit_gateway_attachments.contains_key(resource_id)
            }
            "RegionalNatGateway" => self.nat_gateways.contains_key(resource_id),
            // Unreachable: the model's `FlowLogsResourceType` enum has no other member, and
            // the boundary validator refuses a value outside it before a handler runs. An
            // unknown type resolves NO id, so it can only refuse, never admit.
            _ => false,
        }
    }

    /// Whether this account already holds any of the flow logs `specs` describe — the same
    /// resource monitored the same way and delivered to the same destination
    /// ([`FlowLogIdentity`]) — which is the question `CreateFlowLogs` asks of its WHOLE batch
    /// before it stores anything.
    ///
    /// Each stored flow log's identity is read once and each spec's once, rather than once
    /// per pair: one create may name hundreds of resources
    /// ([`MAX_FLOW_LOG_RESOURCE_IDS`]) and the store holds up to
    /// [`MAX_STORED_RECORDS_PER_STORE`] flow logs.
    ///
    /// A READ: it answers from the flow logs the store holds and writes nothing.
    pub fn holds_any_flow_log_like(&self, specs: &[FlowLogSpec]) -> bool {
        let held: HashSet<FlowLogIdentity> = self
            .flow_logs
            .values()
            .map(|record| FlowLogIdentity::of_reported(&record.view))
            .collect();
        specs
            .iter()
            .any(|spec| held.contains(&FlowLogIdentity::of_spec(spec)))
    }

    /// Insert one flow log, and answer the `FlowLogId` it was given.
    ///
    /// Everything the service itself owns is derived here rather than taken from the caller
    /// — the id, the creation time, the flow log status, the delivery status and the record
    /// format a request that named none gets ([`DEFAULT_FLOW_LOG_FORMAT`]) — so no writer
    /// can store a flow log a describe would have to invent a member for. Like
    /// [`Self::insert_carrier_gateway`], the caller-chosen bytes the record retains
    /// ([`flow_log_payload_bytes`]) are charged against the store's byte budget here, so a
    /// writer cannot store them uncharged.
    pub fn insert_flow_log(&mut self, spec: FlowLogSpec) -> String {
        let payload_bytes = flow_log_payload_bytes(&spec);
        let flow_log_id = self.next_id(FLOW_LOG_ID_PREFIX);
        let view = FlowLog {
            creation_time: Some(system_time_to_date_time(spec.created_at)),
            // "The status of the logs delivery (SUCCESS | FAILED)"
            // (`FlowLog$DeliverLogsStatus`) — reported for every flow log this store holds,
            // on the same basis as the `ACTIVE` flow log status below: the create resolved
            // the destination and the delivery role that destination requires before it got
            // here, so this flow log's delivery is the one the member calls
            // `DELIVER_LOGS_STATUS_SUCCESS`. "Information about the error that occurred"
            // (`$DeliverLogsErrorMessage`) describes a delivery that failed, so a successful
            // one carries no message.
            deliver_logs_error_message: None,
            deliver_logs_status: Some(DELIVER_LOGS_STATUS_SUCCESS.to_string()),
            deliver_logs_permission_arn: spec.deliver_logs_permission_arn,
            deliver_cross_account_role: spec.deliver_cross_account_role,
            flow_log_id: Some(flow_log_id.clone()),
            flow_log_status: Some(FLOW_LOG_STATUS_ACTIVE.to_string()),
            log_group_name: spec.log_group_name,
            resource_id: Some(spec.resource_id),
            traffic_type: spec.traffic_type,
            log_destination_type: Some(spec.log_destination_type),
            log_destination: spec.log_destination,
            // "If you omit this parameter, the flow log is created using the default
            // format" (`CreateFlowLogsRequest$LogFormat`) — applied HERE, so no writer can
            // store a flow log whose record layout a client cannot read.
            log_format: Some(
                spec.log_format
                    .unwrap_or_else(|| DEFAULT_FLOW_LOG_FORMAT.to_string()),
            ),
            tags: Some(spec.tags),
            max_aggregation_interval: Some(spec.max_aggregation_interval),
            // "The destination options" (`FlowLog$DestinationOptions`) — reported exactly when
            // the create ASKED for them, with each member it left out at the default that
            // member's own documentation gives it (the create fills those in). The options are
            // properties of the OBJECTS an Amazon S3 delivery writes
            // (`DestinationOptionsResponse`: a file format, Hive-compatible prefixes, per-hour
            // partitioning), so a create that named none — which is every create for any
            // destination type but Amazon S3, those being refused the member outright
            // ([`flow_log_member_unsupported_for_destination`]) — leaves the member unset
            // rather than being reported a delivery layout it never configured.
            destination_options: spec.destination_options,
            tag_field_specifications: spec.tag_field_specifications,
        };
        self.add_stored_bytes(payload_bytes);
        let client_token = spec.client_token;
        if let Some(token) = client_token.clone() {
            self.flow_log_client_tokens
                .entry(token)
                .or_default()
                .push(flow_log_id.clone());
        }
        self.flow_logs.insert(
            flow_log_id.clone(),
            FlowLogRecord {
                view,
                payload_bytes,
                client_token,
            },
        );
        flow_log_id
    }

    /// Remove the flow log `flow_log_id` and answer it as it was last reported, or `None`
    /// when the store holds no such flow log. The bytes the record held go back to the
    /// store's budget — exactly the number the insert charged — and the `ClientToken` that
    /// minted it stops naming it, so a replayed create cannot answer with a flow log that is
    /// gone.
    ///
    /// The token entry is reached through the record's own
    /// [`FlowLogRecord::client_token`], so removing a batch of flow logs costs one lookup
    /// each rather than a walk of every stored token.
    pub fn remove_flow_log(&mut self, flow_log_id: &str) -> Option<FlowLog> {
        let record = self.flow_logs.remove(flow_log_id)?;
        if let Some(token) = record.client_token.as_deref() {
            if let Some(minted) = self.flow_log_client_tokens.get_mut(token) {
                minted.retain(|minted_id| minted_id != flow_log_id);
                if minted.is_empty() {
                    self.flow_log_client_tokens.remove(token);
                }
            }
        }
        self.release_stored_bytes(record.payload_bytes);
        Some(record.view)
    }
}
