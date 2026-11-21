// SPDX-License-Identifier: CC0-1.0

//! The NETWORK REACHABILITY ANALYSIS shapes of this service: an analysis of a path or of an
//! access scope, the components of a path it walks, and the explanations and hints it reports.
//!
//! Split out of `super` (the shapes more than one operation reaches) so that one file holds
//! one concern; every name is re-exported from there, so nothing about how they are reached
//! changes.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#NetworkInsightsAccessScopeAnalysis")]
#[derive(Debug, Clone, Default)]
pub struct NetworkInsightsAccessScopeAnalysis {
    pub network_insights_access_scope_analysis_id: Option<String>,
    pub network_insights_access_scope_analysis_arn: Option<String>,
    pub network_insights_access_scope_id: Option<String>,
    pub status: Option<String>,
    pub status_message: Option<String>,
    pub warning_message: Option<String>,
    pub start_date: Option<::aws_smithy_types::DateTime>,
    pub end_date: Option<::aws_smithy_types::DateTime>,
    pub findings_found: Option<String>,
    pub analyzed_eni_count: Option<i32>,
    pub tags: Option<Vec<Tag>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#NetworkInsightsAnalysis")]
#[derive(Debug, Clone, Default)]
pub struct NetworkInsightsAnalysis {
    pub network_insights_analysis_id: Option<String>,
    pub network_insights_analysis_arn: Option<String>,
    pub network_insights_path_id: Option<String>,
    pub additional_accounts: Option<Vec<String>>,
    pub filter_in_arns: Option<Vec<String>>,
    pub filter_out_arns: Option<Vec<String>>,
    pub start_date: Option<::aws_smithy_types::DateTime>,
    pub status: Option<String>,
    pub status_message: Option<String>,
    pub warning_message: Option<String>,
    pub network_path_found: Option<bool>,
    pub forward_path_components: Option<Vec<PathComponent>>,
    pub return_path_components: Option<Vec<PathComponent>>,
    pub explanations: Option<Vec<Explanation>>,
    pub alternate_path_hints: Option<Vec<AlternatePathHint>>,
    pub suggested_accounts: Option<Vec<String>>,
    pub tags: Option<Vec<Tag>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#PathComponent")]
#[derive(Debug, Clone, Default)]
pub struct PathComponent {
    pub sequence_number: Option<i32>,
    pub acl_rule: Option<AnalysisAclRule>,
    pub attached_to: Option<AnalysisComponent>,
    pub component: Option<AnalysisComponent>,
    pub destination_vpc: Option<AnalysisComponent>,
    pub outbound_header: Option<AnalysisPacketHeader>,
    pub inbound_header: Option<AnalysisPacketHeader>,
    pub route_table_route: Option<AnalysisRouteTableRoute>,
    pub security_group_rule: Option<AnalysisSecurityGroupRule>,
    pub source_vpc: Option<AnalysisComponent>,
    pub subnet: Option<AnalysisComponent>,
    pub vpc: Option<AnalysisComponent>,
    pub additional_details: Option<Vec<AdditionalDetail>>,
    pub transit_gateway: Option<AnalysisComponent>,
    pub transit_gateway_route_table_route: Option<TransitGatewayRouteTableRoute>,
    pub transit_gateway_policy_table_entry: Option<AnalysisTransitGatewayPolicyTableEntry>,
    pub explanations: Option<Vec<Explanation>>,
    pub elastic_load_balancer_listener: Option<AnalysisComponent>,
    pub firewall_stateless_rule: Option<FirewallStatelessRule>,
    pub firewall_stateful_rule: Option<FirewallStatefulRule>,
    pub service_name: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisAclRule")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisAclRule {
    pub cidr: Option<String>,
    pub egress: Option<bool>,
    pub port_range: Option<PortRange>,
    pub protocol: Option<String>,
    pub rule_action: Option<String>,
    pub rule_number: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisComponent")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisComponent {
    pub id: Option<String>,
    pub arn: Option<String>,
    pub name: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisPacketHeader")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisPacketHeader {
    pub destination_addresses: Option<Vec<String>>,
    pub destination_port_ranges: Option<Vec<PortRange>>,
    pub protocol: Option<String>,
    pub source_addresses: Option<Vec<String>>,
    pub source_port_ranges: Option<Vec<PortRange>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisRouteTableRoute")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisRouteTableRoute {
    pub destination_cidr: Option<String>,
    pub destination_prefix_list_id: Option<String>,
    pub egress_only_internet_gateway_id: Option<String>,
    pub gateway_id: Option<String>,
    pub instance_id: Option<String>,
    pub nat_gateway_id: Option<String>,
    pub network_interface_id: Option<String>,
    pub origin: Option<String>,
    pub transit_gateway_id: Option<String>,
    pub vpc_peering_connection_id: Option<String>,
    pub state: Option<String>,
    pub carrier_gateway_id: Option<String>,
    pub core_network_arn: Option<String>,
    pub local_gateway_id: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisSecurityGroupRule")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisSecurityGroupRule {
    pub cidr: Option<String>,
    pub direction: Option<String>,
    pub security_group_id: Option<String>,
    pub port_range: Option<PortRange>,
    pub prefix_list_id: Option<String>,
    pub protocol: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AdditionalDetail")]
#[derive(Debug, Clone, Default)]
pub struct AdditionalDetail {
    pub additional_detail_type: Option<String>,
    pub component: Option<AnalysisComponent>,
    pub vpc_endpoint_service: Option<AnalysisComponent>,
    pub rule_options: Option<Vec<RuleOption>>,
    pub rule_group_type_pairs: Option<Vec<RuleGroupTypePair>>,
    pub rule_group_rule_options_pairs: Option<Vec<RuleGroupRuleOptionsPair>>,
    pub service_name: Option<String>,
    pub load_balancers: Option<Vec<AnalysisComponent>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#RuleOption")]
#[derive(Debug, Clone, Default)]
pub struct RuleOption {
    pub keyword: Option<String>,
    pub settings: Option<Vec<String>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#RuleGroupTypePair")]
#[derive(Debug, Clone, Default)]
pub struct RuleGroupTypePair {
    pub rule_group_arn: Option<String>,
    pub rule_group_type: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#RuleGroupRuleOptionsPair")]
#[derive(Debug, Clone, Default)]
pub struct RuleGroupRuleOptionsPair {
    pub rule_group_arn: Option<String>,
    pub rule_options: Option<Vec<RuleOption>>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#TransitGatewayRouteTableRoute")]
#[derive(Debug, Clone, Default)]
pub struct TransitGatewayRouteTableRoute {
    pub destination_cidr: Option<String>,
    pub state: Option<String>,
    pub route_origin: Option<String>,
    pub prefix_list_id: Option<String>,
    pub attachment_id: Option<String>,
    pub resource_id: Option<String>,
    pub resource_type: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisTransitGatewayPolicyTableEntry")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisTransitGatewayPolicyTableEntry {
    pub rule_number: Option<i32>,
    pub protocol: Option<String>,
    pub source_cidr: Option<String>,
    pub destination_cidr: Option<String>,
    pub source_port_range: Option<PortRange>,
    pub destination_port_range: Option<PortRange>,
    pub target_route_table_id: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#Explanation")]
#[derive(Debug, Clone, Default)]
pub struct Explanation {
    pub acl: Option<AnalysisComponent>,
    pub acl_rule: Option<AnalysisAclRule>,
    pub address: Option<String>,
    pub addresses: Option<Vec<String>>,
    pub attached_to: Option<AnalysisComponent>,
    pub availability_zones: Option<Vec<String>>,
    pub availability_zone_ids: Option<Vec<String>>,
    pub cidrs: Option<Vec<String>>,
    pub component: Option<AnalysisComponent>,
    pub customer_gateway: Option<AnalysisComponent>,
    pub destination: Option<AnalysisComponent>,
    pub destination_vpc: Option<AnalysisComponent>,
    pub direction: Option<String>,
    pub explanation_code: Option<String>,
    pub ingress_route_table: Option<AnalysisComponent>,
    pub internet_gateway: Option<AnalysisComponent>,
    pub load_balancer_arn: Option<String>,
    pub classic_load_balancer_listener: Option<AnalysisLoadBalancerListener>,
    pub load_balancer_listener_port: Option<i32>,
    pub load_balancer_target: Option<AnalysisLoadBalancerTarget>,
    pub load_balancer_target_group: Option<AnalysisComponent>,
    pub load_balancer_target_groups: Option<Vec<AnalysisComponent>>,
    pub load_balancer_target_port: Option<i32>,
    pub elastic_load_balancer_listener: Option<AnalysisComponent>,
    pub missing_component: Option<String>,
    pub nat_gateway: Option<AnalysisComponent>,
    pub network_interface: Option<AnalysisComponent>,
    pub packet_field: Option<String>,
    pub vpc_peering_connection: Option<AnalysisComponent>,
    pub port: Option<i32>,
    pub port_ranges: Option<Vec<PortRange>>,
    pub prefix_list: Option<AnalysisComponent>,
    pub protocols: Option<Vec<String>>,
    pub route_table_route: Option<AnalysisRouteTableRoute>,
    pub route_table: Option<AnalysisComponent>,
    pub security_group: Option<AnalysisComponent>,
    pub security_group_rule: Option<AnalysisSecurityGroupRule>,
    pub security_groups: Option<Vec<AnalysisComponent>>,
    pub source_vpc: Option<AnalysisComponent>,
    pub state: Option<String>,
    pub subnet: Option<AnalysisComponent>,
    pub subnet_route_table: Option<AnalysisComponent>,
    pub vpc: Option<AnalysisComponent>,
    pub vpc_endpoint: Option<AnalysisComponent>,
    pub vpn_connection: Option<AnalysisComponent>,
    pub vpn_gateway: Option<AnalysisComponent>,
    pub transit_gateway: Option<AnalysisComponent>,
    pub transit_gateway_route_table: Option<AnalysisComponent>,
    pub transit_gateway_route_table_route: Option<TransitGatewayRouteTableRoute>,
    pub transit_gateway_policy_table: Option<AnalysisComponent>,
    pub transit_gateway_policy_table_entry: Option<AnalysisTransitGatewayPolicyTableEntry>,
    pub transit_gateway_attachment: Option<AnalysisComponent>,
    pub component_account: Option<String>,
    pub component_region: Option<String>,
    pub firewall_stateless_rule: Option<FirewallStatelessRule>,
    pub firewall_stateful_rule: Option<FirewallStatefulRule>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisLoadBalancerListener")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisLoadBalancerListener {
    pub load_balancer_port: Option<i32>,
    pub instance_port: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AnalysisLoadBalancerTarget")]
#[derive(Debug, Clone, Default)]
pub struct AnalysisLoadBalancerTarget {
    pub address: Option<String>,
    pub availability_zone: Option<String>,
    pub availability_zone_id: Option<String>,
    pub instance: Option<AnalysisComponent>,
    pub port: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#FirewallStatelessRule")]
#[derive(Debug, Clone, Default)]
pub struct FirewallStatelessRule {
    pub rule_group_arn: Option<String>,
    pub sources: Option<Vec<String>>,
    pub destinations: Option<Vec<String>>,
    pub source_ports: Option<Vec<PortRange>>,
    pub destination_ports: Option<Vec<PortRange>>,
    pub protocols: Option<Vec<i32>>,
    pub rule_action: Option<String>,
    pub priority: Option<i32>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#FirewallStatefulRule")]
#[derive(Debug, Clone, Default)]
pub struct FirewallStatefulRule {
    pub rule_group_arn: Option<String>,
    pub sources: Option<Vec<String>>,
    pub destinations: Option<Vec<String>>,
    pub source_ports: Option<Vec<PortRange>>,
    pub destination_ports: Option<Vec<PortRange>>,
    pub protocol: Option<String>,
    pub rule_action: Option<String>,
    pub direction: Option<String>,
}

#[smithy_macros::smithy(shape = "com.amazonaws.ec2#AlternatePathHint")]
#[derive(Debug, Clone, Default)]
pub struct AlternatePathHint {
    pub component_id: Option<String>,
    pub component_arn: Option<String>,
}
