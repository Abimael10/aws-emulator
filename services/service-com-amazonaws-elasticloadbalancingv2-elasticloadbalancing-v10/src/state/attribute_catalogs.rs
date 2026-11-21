// SPDX-License-Identifier: CC0-1.0

//! The ATTRIBUTE CATALOGS of this service: every load balancer, target group and listener
//! attribute it models, each with the resources that carry it, the values it accepts and the
//! value it is carried at until a write.
//!
//! One file for the three lists because they are one concern — the model's attribute
//! collections are unconstrained key/value pairs, so these catalogs are the only statement of
//! what keys exist — and because each is a long literal that nothing reads but the readers of
//! its own collection ([`super::target_group_attribute`] and its siblings). The specs and the
//! domains they are written in terms of stay beside those readers in [`super`].

use super::*;

/// Every load balancer attribute this service models, with the families that carry it,
/// the values it accepts and the value it is carried at until a write — each one a
/// sentence of the model's own `LoadBalancerAttribute$Key` documentation
/// (`smithy_query(query="doc",shape="LoadBalancerAttribute",member="Key")`), which is the
/// only catalog of them; the Smithy shape itself is an unconstrained key/value pair, so
/// nothing else in the model lists them.
///
/// The ORDER of this list is the order a load balancer's attribute collection is reported
/// in ([`LoadBalancerRecord::effective_attributes`]), which is the service's own and
/// neither alphabetical nor the documentation's grouping: the log destinations first, then
/// the connection and routing settings, then the connection logs.
pub static LOAD_BALANCER_ATTRIBUTES: &[LoadBalancerAttributeSpec] = &[
    // "access_logs.s3.enabled - Indicates whether access logs are enabled. The value is
    // true or false. The default is false." Documented "supported by both Application
    // Load Balancers and Network Load Balancers", as the two beside it are.
    LoadBalancerAttributeSpec {
        key: "access_logs.s3.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_AND_NETWORK,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "access_logs.s3.bucket - The name of the S3 bucket for the access logs." A name
    // the caller chooses, with no domain and no value stated for it: the collection
    // carries it at the empty string until a caller names a bucket.
    LoadBalancerAttributeSpec {
        key: "access_logs.s3.bucket",
        domain: AttributeDomain::Text,
        families: APPLICATION_AND_NETWORK,
        default: |_| Some(String::new()),
        fixed: |_| None,
    },
    // "access_logs.s3.prefix - The prefix for the location in the S3 bucket for the
    // access logs."
    LoadBalancerAttributeSpec {
        key: "access_logs.s3.prefix",
        domain: AttributeDomain::Text,
        families: APPLICATION_AND_NETWORK,
        default: |_| Some(String::new()),
        fixed: |_| None,
    },
    // "The following attributes are supported by only Application Load Balancers:"
    //
    // "health_check_logs.s3.enabled - Indicates whether health check logs are enabled.
    // The value is true or false. The default is false."
    LoadBalancerAttributeSpec {
        key: "health_check_logs.s3.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "health_check_logs.s3.bucket - The name of the S3 bucket for the health check
    // logs."
    LoadBalancerAttributeSpec {
        key: "health_check_logs.s3.bucket",
        domain: AttributeDomain::Text,
        families: APPLICATION_ONLY,
        default: |_| Some(String::new()),
        fixed: |_| None,
    },
    // "health_check_logs.s3.prefix - The prefix for the location in the S3 bucket for
    // the health check logs."
    LoadBalancerAttributeSpec {
        key: "health_check_logs.s3.prefix",
        domain: AttributeDomain::Text,
        families: APPLICATION_ONLY,
        default: |_| Some(String::new()),
        fixed: |_| None,
    },
    // "idle_timeout.timeout_seconds - The idle timeout value, in seconds. The valid
    // range is 1-4000 seconds. The default is 60 seconds."
    LoadBalancerAttributeSpec {
        key: "idle_timeout.timeout_seconds",
        domain: AttributeDomain::Integer { min: 1, max: 4000 },
        families: APPLICATION_ONLY,
        default: |_| Some("60".to_string()),
        fixed: |_| None,
    },
    // "The following attributes are supported by all load balancers:"
    //
    // "deletion_protection.enabled - Indicates whether deletion protection is enabled.
    // The value is true or false. The default is false."
    LoadBalancerAttributeSpec {
        key: "deletion_protection.enabled",
        domain: AttributeDomain::Boolean,
        families: EVERY_FAMILY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "routing.http2.enabled - ... The default is true."
    LoadBalancerAttributeSpec {
        key: "routing.http2.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("true".to_string()),
        fixed: |_| None,
    },
    // "routing.http.drop_invalid_header_fields.enabled - ... The default is false."
    LoadBalancerAttributeSpec {
        key: "routing.http.drop_invalid_header_fields.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "routing.http.xff_client_port.enabled - ... The default is false."
    LoadBalancerAttributeSpec {
        key: "routing.http.xff_client_port.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "routing.http.xff_header_processing.mode - ... The possible values are append,
    // preserve, and remove. The default is append."
    LoadBalancerAttributeSpec {
        key: "routing.http.xff_header_processing.mode",
        domain: AttributeDomain::OneOf(&["append", "preserve", "remove"]),
        families: APPLICATION_ONLY,
        default: |_| Some("append".to_string()),
        fixed: |_| None,
    },
    // "routing.http.preserve_host_header.enabled - ... The default is false."
    LoadBalancerAttributeSpec {
        key: "routing.http.preserve_host_header.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "routing.http.x_amzn_tls_version_and_cipher_suite.enabled - ... The default is
    // false."
    LoadBalancerAttributeSpec {
        key: "routing.http.x_amzn_tls_version_and_cipher_suite.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "routing.http.desync_mitigation_mode - ... The possible values are monitor,
    // defensive, and strictest. The default is defensive."
    LoadBalancerAttributeSpec {
        key: "routing.http.desync_mitigation_mode",
        domain: AttributeDomain::OneOf(&["monitor", "defensive", "strictest"]),
        families: APPLICATION_ONLY,
        default: |_| Some("defensive".to_string()),
        fixed: |_| None,
    },
    // "load_balancing.cross_zone.enabled - ... The default for Network Load Balancers
    // and Gateway Load Balancers is false. The default for Application Load Balancers is
    // true, and can't be changed." Documented "supported by all load balancers".
    LoadBalancerAttributeSpec {
        key: "load_balancing.cross_zone.enabled",
        domain: AttributeDomain::Boolean,
        families: EVERY_FAMILY,
        default: |load_balancer| {
            Some(
                match load_balancer.family() {
                    LoadBalancerFamily::Application => "true",
                    LoadBalancerFamily::Network | LoadBalancerFamily::Gateway => "false",
                }
                .to_string(),
            )
        },
        fixed: |load_balancer| {
            (load_balancer.family() == LoadBalancerFamily::Application)
                .then(|| "true".to_string())
        },
    },
    // "client_keep_alive.seconds - The client keep alive value, in seconds. The valid
    // range is 60-604800 seconds. The default is 3600 seconds."
    LoadBalancerAttributeSpec {
        key: "client_keep_alive.seconds",
        domain: AttributeDomain::Integer {
            min: 60,
            max: 604_800,
        },
        families: APPLICATION_ONLY,
        default: |_| Some("3600".to_string()),
        fixed: |_| None,
    },
    // "waf.fail_open.enabled - ... The default is false."
    LoadBalancerAttributeSpec {
        key: "waf.fail_open.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "zonal_shift.config.enabled - Indicates whether zonal shift is enabled. The
    // possible values are true and false. The default is false." Documented for both
    // Application and Network Load Balancers.
    LoadBalancerAttributeSpec {
        key: "zonal_shift.config.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_AND_NETWORK,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "connection_logs.s3.enabled - Indicates whether connection logs are enabled. The
    // value is true or false. The default is false."
    LoadBalancerAttributeSpec {
        key: "connection_logs.s3.enabled",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_ONLY,
        default: |_| Some("false".to_string()),
        fixed: |_| None,
    },
    // "connection_logs.s3.bucket - The name of the S3 bucket for the connection logs."
    LoadBalancerAttributeSpec {
        key: "connection_logs.s3.bucket",
        domain: AttributeDomain::Text,
        families: APPLICATION_ONLY,
        default: |_| Some(String::new()),
        fixed: |_| None,
    },
    // "connection_logs.s3.prefix - The prefix for the location in the S3 bucket for the
    // connection logs."
    LoadBalancerAttributeSpec {
        key: "connection_logs.s3.prefix",
        domain: AttributeDomain::Text,
        families: APPLICATION_ONLY,
        default: |_| Some(String::new()),
        fixed: |_| None,
    },
    // "ipv6.deny_all_igw_traffic - Blocks internet gateway (IGW) access to the load
    // balancer. It is set to false for internet-facing load balancers and true for
    // internal load balancers". A write of it is accepted on the families the
    // documentation lists it under, but the value follows the scheme rather than being
    // held per load balancer, and the attribute collection a read and a write answer with
    // does not carry it — so it is reported only once a caller has written one.
    LoadBalancerAttributeSpec {
        key: "ipv6.deny_all_igw_traffic",
        domain: AttributeDomain::Boolean,
        families: APPLICATION_AND_NETWORK,
        default: |_| None,
        fixed: |_| None,
    },
    // "The following attributes are supported by only Network Load Balancers:"
    //
    // "dns_record.client_routing_policy - Indicates how traffic is distributed among the
    // load balancer Availability Zones. The possible values are
    // availability_zone_affinity ..., partial_availability_zone_affinity ... and
    // any_availability_zone ..." — three values and no documented default, so it is
    // reported once a caller has set one.
    LoadBalancerAttributeSpec {
        key: "dns_record.client_routing_policy",
        domain: AttributeDomain::OneOf(&[
            "availability_zone_affinity",
            "partial_availability_zone_affinity",
            "any_availability_zone",
        ]),
        families: NETWORK_ONLY,
        default: |_| None,
        fixed: |_| None,
    },
    // "secondary_ips.auto_assigned.per_subnet - The number of secondary IP addresses to
    // configure for your load balancer nodes. ... The valid range is 0 to 7. The default
    // is 0."
    LoadBalancerAttributeSpec {
        key: "secondary_ips.auto_assigned.per_subnet",
        domain: AttributeDomain::Integer { min: 0, max: 7 },
        families: NETWORK_ONLY,
        default: |_| Some("0".to_string()),
        fixed: |_| None,
    },
];

/// Every target group attribute this service models, with the family it belongs to,
/// the values it accepts and the value it is carried at until a write — each one a
/// sentence of the model's own `TargetGroupAttribute$Key` documentation
/// (`smithy_query(query="doc",shape="TargetGroupAttribute",member="Key")`), which is
/// the only catalog of them; the Smithy shape itself is an unconstrained key/value
/// pair, so nothing else in the model lists them.
///
/// The ORDER of this list is the documentation's own, and the order a target group's
/// attribute collection is reported in
/// ([`TargetGroupRecord::effective_attributes`]).
pub static TARGET_GROUP_ATTRIBUTES: &[AttributeSpec] = &[
    // Supported by all load balancers.
    AttributeSpec {
        key: "deregistration_delay.timeout_seconds",
        domain: AttributeDomain::Integer { min: 0, max: 3600 },
        // "If the target is a Lambda function, this attribute is not supported."
        applies: |group| !group.is_lambda(),
        default: |_| Some("300".to_string()),
    },
    AttributeSpec {
        key: "stickiness.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| !group.is_lambda(),
        default: |_| Some("false".to_string()),
    },
    AttributeSpec {
        key: "stickiness.type",
        domain: AttributeDomain::OneOf(&[
            "lb_cookie",
            "app_cookie",
            "source_ip",
            "source_ip_dest_ip",
            "source_ip_dest_ip_proto",
        ]),
        applies: |group| !group.is_lambda(),
        // "lb_cookie and app_cookie for Application Load Balancers. source_ip for
        // Network Load Balancers. source_ip_dest_ip and source_ip_dest_ip_proto for
        // Gateway Load Balancers."
        default: |group| {
            Some(
                match group.family() {
                    LoadBalancerFamily::Application => "lb_cookie",
                    LoadBalancerFamily::Network => "source_ip",
                    LoadBalancerFamily::Gateway => "source_ip_dest_ip",
                }
                .to_string(),
            )
        },
    },
    // Supported by Application Load Balancers and Network Load Balancers — and so by
    // neither a Gateway Load Balancer's target group nor a Lambda one (see
    // [`TargetGroupRecord::carries_connection_attributes`]).
    AttributeSpec {
        key: "load_balancing.cross_zone.enabled",
        domain: AttributeDomain::OneOf(&["true", "false", "use_load_balancer_configuration"]),
        applies: TargetGroupRecord::carries_connection_attributes,
        default: |_| Some("use_load_balancer_configuration".to_string()),
    },
    AttributeSpec {
        key: "target_group_health.dns_failover.minimum_healthy_targets.count",
        // "The minimum number of targets that must be healthy. If the number of healthy
        // targets is below this value, mark the zone as unhealthy in DNS, so that traffic is
        // routed only to healthy zones. The possible values are off or an integer from 1 to
        // the maximum number of targets" — so the ceiling is the number of targets a group
        // can hold at all ([`targets_per_target_group`]'s largest quota), which is also what
        // bounds any span or count a reader derives from this value
        // ([`AttributeDomain::integer_ceiling`]).
        domain: AttributeDomain::IntegerOrOff {
            min: 1,
            max: TARGETS_PER_TARGET_GROUP as i64,
        },
        applies: TargetGroupRecord::carries_connection_attributes,
        default: |_| Some("1".to_string()),
    },
    AttributeSpec {
        key: "target_group_health.dns_failover.minimum_healthy_targets.percentage",
        domain: AttributeDomain::IntegerOrOff { min: 1, max: 100 },
        applies: TargetGroupRecord::carries_connection_attributes,
        default: |_| Some("off".to_string()),
    },
    AttributeSpec {
        key: "target_group_health.unhealthy_state_routing.minimum_healthy_targets.count",
        // "The minimum number of targets that must be healthy" for this routing too, bounded
        // by the same maximum number of targets as its DNS-failover counterpart above.
        domain: AttributeDomain::Integer {
            min: 1,
            max: TARGETS_PER_TARGET_GROUP as i64,
        },
        applies: TargetGroupRecord::carries_connection_attributes,
        default: |_| Some("1".to_string()),
    },
    AttributeSpec {
        key: "target_group_health.unhealthy_state_routing.minimum_healthy_targets.percentage",
        domain: AttributeDomain::IntegerOrOff { min: 1, max: 100 },
        applies: TargetGroupRecord::carries_connection_attributes,
        default: |_| Some("off".to_string()),
    },
    // Supported only if the load balancer is an Application Load Balancer and the
    // target is an instance or an IP address.
    AttributeSpec {
        key: "load_balancing.algorithm.type",
        domain: AttributeDomain::OneOf(&[
            "round_robin",
            "least_outstanding_requests",
            "weighted_random",
        ]),
        applies: TargetGroupRecord::is_application_with_addressable_targets,
        default: |_| Some("round_robin".to_string()),
    },
    AttributeSpec {
        key: "load_balancing.algorithm.anomaly_mitigation",
        domain: AttributeDomain::OneOf(&["on", "off"]),
        applies: TargetGroupRecord::is_application_with_addressable_targets,
        default: |_| Some("off".to_string()),
    },
    AttributeSpec {
        key: "slow_start.duration_seconds",
        // "The range is 30-900 seconds (15 minutes). The default is 0 seconds
        // (disabled)."
        domain: AttributeDomain::ZeroOrInteger { min: 30, max: 900 },
        applies: TargetGroupRecord::is_application_with_addressable_targets,
        default: |_| Some("0".to_string()),
    },
    AttributeSpec {
        key: "stickiness.app_cookie.cookie_name",
        domain: AttributeDomain::CookieName,
        applies: TargetGroupRecord::is_application_with_addressable_targets,
        // A cookie name the caller chooses, with no value stated for it: the collection
        // carries it at the empty string until a caller names a cookie.
        default: |_| Some(String::new()),
    },
    AttributeSpec {
        key: "stickiness.app_cookie.duration_seconds",
        domain: AttributeDomain::Integer { min: 1, max: 604_800 },
        applies: TargetGroupRecord::is_application_with_addressable_targets,
        default: |_| Some("86400".to_string()),
    },
    AttributeSpec {
        key: "stickiness.lb_cookie.duration_seconds",
        domain: AttributeDomain::Integer { min: 1, max: 604_800 },
        applies: TargetGroupRecord::is_application_with_addressable_targets,
        default: |_| Some("86400".to_string()),
    },
    // Supported only if the load balancer is an Application Load Balancer and the
    // target is a Lambda function.
    AttributeSpec {
        key: "lambda.multi_value_headers.enabled",
        domain: AttributeDomain::Boolean,
        applies: TargetGroupRecord::is_lambda,
        default: |_| Some("false".to_string()),
    },
    // Supported only by Network Load Balancers.
    AttributeSpec {
        key: "deregistration_delay.connection_termination.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| group.family() == LoadBalancerFamily::Network,
        // "For new UDP/TCP_UDP target groups the default is true. Otherwise, the
        // default is false."
        default: |group| {
            Some(
                match group.protocol.as_deref() {
                    Some("UDP") | Some("TCP_UDP") => "true",
                    _ => "false",
                }
                .to_string(),
            )
        },
    },
    AttributeSpec {
        key: "preserve_client_ip.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| group.family() == LoadBalancerFamily::Network,
        // "The default is disabled if the target group type is IP address and the
        // target group protocol is TCP or TLS. Otherwise, the default is enabled."
        default: |group| {
            let ip_tcp = group.target_type == "ip"
                && matches!(group.protocol.as_deref(), Some("TCP") | Some("TLS"));
            Some(if ip_tcp { "false" } else { "true" }.to_string())
        },
    },
    AttributeSpec {
        key: "proxy_protocol_v2.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| group.family() == LoadBalancerFamily::Network,
        default: |_| Some("false".to_string()),
    },
    AttributeSpec {
        key: "target_health_state.unhealthy.connection_termination.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| group.family() == LoadBalancerFamily::Network,
        default: |_| Some("true".to_string()),
    },
    AttributeSpec {
        key: "target_health_state.unhealthy.draining_interval_seconds",
        domain: AttributeDomain::Integer { min: 0, max: 360_000 },
        applies: |group| group.family() == LoadBalancerFamily::Network,
        default: |_| Some("0".to_string()),
    },
    // Supported only by Gateway Load Balancers.
    AttributeSpec {
        key: "target_failover.on_deregistration",
        domain: AttributeDomain::OneOf(&["rebalance", "no_rebalance"]),
        applies: |group| group.family() == LoadBalancerFamily::Gateway,
        default: |_| Some("no_rebalance".to_string()),
    },
    AttributeSpec {
        key: "target_failover.on_unhealthy",
        domain: AttributeDomain::OneOf(&["rebalance", "no_rebalance"]),
        applies: |group| group.family() == LoadBalancerFamily::Gateway,
        default: |_| Some("no_rebalance".to_string()),
    },
    AttributeSpec {
        key: "send_tcp_reset.on_unhealthy.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| group.family() == LoadBalancerFamily::Gateway,
        default: |_| Some("false".to_string()),
    },
    AttributeSpec {
        key: "send_tcp_reset.on_deregistration.enabled",
        domain: AttributeDomain::Boolean,
        applies: |group| group.family() == LoadBalancerFamily::Gateway,
        default: |_| Some("false".to_string()),
    },
];

/// Every listener attribute this service models, with the load balancer families whose
/// listeners carry it, the values it accepts and the value it is carried at until a write
/// — each one a sentence of the model's own `ListenerAttribute$Key` documentation
/// (`smithy_query(query="doc",shape="ListenerAttribute",member="Key")`), which is the
/// only catalog of them; the Smithy shape itself is an unconstrained key/value pair, so
/// nothing else in the model lists them.
///
/// The ORDER of this list is the order a listener's attribute collection is reported in
/// ([`ListenerRecord::effective_attributes`]).
pub static LISTENER_ATTRIBUTES: &[ListenerAttributeSpec] = &[
    // "The following attribute is supported by Network Load Balancers, and Gateway Load
    // Balancers: tcp.idle_timeout.seconds - The tcp idle timeout value, in seconds. The
    // valid range is 60-6000 seconds. The default is 350 seconds."
    ListenerAttributeSpec {
        key: "tcp.idle_timeout.seconds",
        domain: AttributeDomain::Integer { min: 60, max: 6000 },
        families: &[LoadBalancerFamily::Network, LoadBalancerFamily::Gateway],
        default: Some("350"),
    },
    // "The following attribute is only supported by Gateway Load Balancers:
    // send_tcp_reset.on_idle_timeout.enabled – ... The value is true or false. The
    // default is false."
    ListenerAttributeSpec {
        key: "send_tcp_reset.on_idle_timeout.enabled",
        domain: AttributeDomain::Boolean,
        families: &[LoadBalancerFamily::Gateway],
        default: Some("false"),
    },
    // "The following attributes are only supported by Application Load Balancers": the
    // mutual-TLS and TLS request header names, each of which the documentation describes
    // as a header NAME the caller chooses, so the model states no domain and no value for
    // any of them. An Application Load Balancer listener's attribute collection is the
    // response-header set below, so none of these is in it: each is reported once a
    // caller has written it.
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_mtls_clientcert_serial_number.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_mtls_clientcert_issuer.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_mtls_clientcert_subject.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_mtls_clientcert_validity.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_mtls_clientcert_leaf.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_mtls_clientcert.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_tls_version.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    ListenerAttributeSpec {
        key: "routing.http.request.x_amzn_tls_cipher_suite.header_name",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: None,
    },
    // "routing.http.response.server.enabled - Enables you to allow or remove the HTTP
    // response server header." Allowed or removed: the two states the sentence names,
    // and the header is allowed until a caller removes it.
    ListenerAttributeSpec {
        key: "routing.http.response.server.enabled",
        domain: AttributeDomain::Boolean,
        families: &[LoadBalancerFamily::Application],
        default: Some("true"),
    },
    // The response header VALUES, each documented as the value of one HTTP response
    // header — again a caller's string, with no domain and no value stated for it, so the
    // collection carries each at the empty string until a caller sets a header value.
    ListenerAttributeSpec {
        key: "routing.http.response.strict_transport_security.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.access_control_allow_origin.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.access_control_allow_methods.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.access_control_allow_headers.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.access_control_allow_credentials.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.access_control_expose_headers.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    // "Specifies how long the results of a preflight request can be cached, in seconds."
    ListenerAttributeSpec {
        key: "routing.http.response.access_control_max_age.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.content_security_policy.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.x_content_type_options.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
    ListenerAttributeSpec {
        key: "routing.http.response.x_frame_options.header_value",
        domain: AttributeDomain::Text,
        families: &[LoadBalancerFamily::Application],
        default: Some(""),
    },
];
