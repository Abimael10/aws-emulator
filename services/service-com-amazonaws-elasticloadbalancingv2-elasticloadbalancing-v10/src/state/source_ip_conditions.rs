// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS BLOCKS a `source-ip` rule condition matches on: what makes one of its values
//! a block at all, and the block a value names.
//!
//! `SourceIpConditionConfig$Values` is "the source IP addresses, in CIDR format. You can use
//! both IPv4 and IPv6 addresses. Wildcards are not supported", and "if you specify multiple
//! addresses, the condition is satisfied if the source IP address of the request matches one
//! of the CIDR blocks". So a value is a block of addresses a connection can have come from:
//! an IPv4 or IPv6 address with a prefix length its family admits, and nothing else — no
//! wildcard, no bare address, no prefix past the width of the family.
//!
//! One more value parses as a block and is none: the IPv4 all-ones address, which RFC 919 §7
//! reserves as the LIMITED BROADCAST address. No host answers to it and no connection comes
//! from it, so a condition written on it could match no request; Elastic Load Balancing
//! refuses it rather than storing a condition that can never be met.
//!
//! ONE reading serves both sides of such a value's life, so the two cannot drift: the
//! operations that SET a rule's conditions refuse a value that is no block
//! ([`check_source_ip_conditions`], reached from [`super::check_rule_conditions`]), and the
//! data plane evaluates a stored condition against the address the connection came from
//! through the same parse (`crate::service::data_plane`).

use std::net::{IpAddr, Ipv4Addr};

use ipnet::IpNet;

use crate::ops::types::RuleCondition;

/// The `RuleCondition$Field` value whose configuration shape carries these blocks.
const SOURCE_IP_FIELD: &str = "source-ip";

/// How wide an IPv4 address is, which is the prefix length of a block holding exactly one of
/// them.
const IPV4_ADDRESS_BITS: u8 = 32;

/// The address block `value` names, or `None` when it is no address block.
///
/// The WHOLE value is the block: nothing is trimmed, because a condition value is read as the
/// caller wrote it, and a value carrying anything around the block is not in the CIDR format
/// the member documents.
#[must_use]
pub fn source_ip_block(value: &str) -> Option<IpNet> {
    let block: IpNet = value.parse().ok()?;
    (!is_limited_broadcast(block)).then_some(block)
}

/// Whether `block` is the IPv4 limited broadcast address ALONE — the one block that parses
/// and names no address a request can come from.
fn is_limited_broadcast(block: IpNet) -> bool {
    block.addr() == IpAddr::V4(Ipv4Addr::BROADCAST) && block.prefix_len() == IPV4_ADDRESS_BITS
}

/// The refusal a `source-ip` condition value that is no address block earns, in Elastic Load
/// Balancing's own wording: the value the request carried is the only part that varies, and it
/// is reported exactly as it arrived, so the caller can see which of its values was read.
#[must_use]
pub fn invalid_cidr_block_message(value: &str) -> String {
    format!("The specified value '{value}' is not a valid CIDR block")
}

/// Refuse the first `source-ip` condition value of `conditions` that is no address block.
///
/// Every value of every such condition is judged: the values of one condition are
/// alternatives, so one that can match nothing is a value the caller meant something by.
pub fn check_source_ip_conditions(conditions: &[RuleCondition]) -> Result<(), String> {
    for condition in conditions {
        if condition.field.as_deref() != Some(SOURCE_IP_FIELD) {
            continue;
        }
        let values = condition
            .source_ip_config
            .as_ref()
            .and_then(|config| config.values.as_deref())
            .unwrap_or_default();
        for value in values {
            if source_ip_block(value).is_none() {
                return Err(invalid_cidr_block_message(value));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    //! The reading of ONE value. What a rule's conditions are refused for, and what a stored
    //! condition then matches, are pinned end to end in `tests/all/alb_data_plane.rs`.

    use super::*;

    #[test]
    fn a_value_is_a_block_exactly_when_it_names_one() {
        // The rows span what the rule generalises over: each family, each published prefix
        // bound and one past it, a bare address, a wildcard, text that is no address, a value
        // with something around the block, and the all-ones address.
        let cases: &[(&str, bool)] = &[
            ("10.0.0.0/8", true),
            ("0.0.0.0/0", true),
            ("203.0.113.4/32", true),
            ("::/0", true),
            ("2001:db8::/32", true),
            ("::1/128", true),
            ("10.0.0.0/33", false),
            ("::/129", false),
            ("10.0.0.1", false),
            ("10.0.*.*/16", false),
            ("not an address/8", false),
            (" 10.0.0.0/8", false),
            ("255.255.255.255/32", false),
        ];
        for (value, expected) in cases {
            let read = source_ip_block(value).is_some();
            assert_eq!(
                read, *expected,
                "case {value:?}: expected it to read as a block={expected}, got {read}"
            );
        }
    }

    #[test]
    fn the_all_ones_address_is_refused_only_as_the_whole_block() {
        // A block that HOLDS the all-ones address alongside others is a block: only the
        // address on its own is none.
        let held = source_ip_block("255.255.255.254/31");
        assert!(
            held.is_some(),
            "case a block holding the all-ones address: expected a block, got {held:?}"
        );
    }
}
