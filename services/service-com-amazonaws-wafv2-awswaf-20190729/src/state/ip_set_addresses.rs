// SPDX-License-Identifier: CC0-1.0

//! The notation and the ORDER an IP set holds its `Addresses` in.
//!
//! `IPSet$Addresses` "contains an array of strings that specifies zero or more IP
//! addresses or blocks of IP addresses that you want WAF to inspect for in incoming
//! requests", and what WAF holds is the SET those strings name: a block named twice
//! is one entry, and the order the array happens to arrive in is not part of the
//! set. The order a READ reports is therefore WAF's own — derived from the entries
//! themselves, not from the request — and recorded WAF responses show it: an
//! address list comes back in neither the submitted nor the sorted order, and the
//! same membership comes back the same way every time.
//!
//! Both halves of that are what makes a read usable: a client that compares
//! `GetIPSet` against its intended configuration sees the same list for the same
//! membership whichever order it wrote it in, so an idempotent apply does not
//! rewrite the set, and the stored list is a fixed point — resubmitting what a read
//! reported stores exactly that.

#[allow(unused_imports)]
use super::*;

/// The longest prefix an IPv4 CIDR block can carry: the width of an IPv4 address in
/// bits. `IPSet$Addresses` supports "all IPv4 and IPv6 CIDR ranges except for `/0`",
/// so a block's prefix length runs from `/1` to the width of its family.
const IPV4_MAX_PREFIX_LEN: u32 = 32;

/// The longest prefix an IPv6 CIDR block can carry. See [`IPV4_MAX_PREFIX_LEN`].
const IPV6_MAX_PREFIX_LEN: u32 = 128;

/// Whether one `Addresses` entry is a CIDR block of the family the set declares.
///
/// `IPSet$Addresses` documents the rule: "All addresses must be specified using
/// Classless Inter-Domain Routing (CIDR) notation. WAF supports all IPv4 and IPv6
/// CIDR ranges except for `/0`", and `IPAddressVersion` fixes which family the
/// entries belong to.
///
/// What the doc does NOT say is anything about the host bits below the prefix
/// length, so `10.0.0.1/8` is deliberately accepted and stored verbatim: the
/// documented rule is the notation, the family and the `/0` exclusion, and this
/// check goes no further than the rule it can cite.
pub fn is_cidr_block(address: &str, ip_address_version: &str) -> bool {
    let Some((literal, prefix)) = address.split_once('/') else {
        return false;
    };
    if prefix.is_empty() || !prefix.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    let Ok(prefix_len) = prefix.parse::<u32>() else {
        return false;
    };
    let (parsed, max_prefix) = match ip_address_version {
        "IPV4" => (
            literal.parse::<::std::net::Ipv4Addr>().is_ok(),
            IPV4_MAX_PREFIX_LEN,
        ),
        "IPV6" => (
            literal.parse::<::std::net::Ipv6Addr>().is_ok(),
            IPV6_MAX_PREFIX_LEN,
        ),
        // A family this service does not know names no notation to judge the entry
        // against, so no entry is a block of it.
        _ => return false,
    };
    // `/0` is the one range WAF documents as unsupported, for either family.
    parsed && (1..=max_prefix).contains(&prefix_len)
}

/// The first entry of `addresses` WAF will not accept for a set declaring
/// `ip_address_version`, or `None` when every one of them is a CIDR block of that
/// family.
///
/// One reading for every operation that saves an IP set's membership
/// ([`is_cidr_block`]), so a create and an update cannot disagree about which
/// entries a set may hold.
pub fn refused_ip_set_address<'addresses>(
    addresses: &'addresses [String],
    ip_address_version: &str,
) -> Option<&'addresses str> {
    addresses
        .iter()
        .map(String::as_str)
        .find(|address| !is_cidr_block(address, ip_address_version))
}

/// [`invalid_parameter_exception`] for an `Addresses` entry WAF will not accept:
/// `Field` [`FIELD_IP_ADDRESS`] (the settings the address was written in),
/// `Parameter` the entry as the request wrote it and `Reason`
/// [`FORMAT_NOT_VALID_REASON`].
///
/// ONE sentence for the whole class — text that is no CIDR block at all, a prefix
/// length outside the family's range, and a well-formed block of the OTHER family
/// alike — because that is what WAF answers for all of them: the `Parameter` member
/// carries the rejected entry, so the reason does not have to distinguish them, and
/// the refusal discloses nothing per entry beyond the value the request sent.
pub fn invalid_ip_address_exception(address: &str) -> WAFInvalidParameterException {
    invalid_parameter_exception(FIELD_IP_ADDRESS, address, FORMAT_NOT_VALID_REASON)
}

/// The smallest table the set's entries are laid out in, in buckets: the floor a
/// hash table keeps even for a membership far below it.
const ADDRESS_TABLE_MIN_BUCKETS: usize = 16;

/// How full the table is allowed to get before it doubles, as a fraction: three
/// quarters.
const ADDRESS_TABLE_LOAD_NUMERATOR: usize = 3;
/// See [`ADDRESS_TABLE_LOAD_NUMERATOR`].
const ADDRESS_TABLE_LOAD_DENOMINATOR: usize = 4;

/// The multiplier the string hash below accumulates with: the odd prime a
/// polynomial string hash is classically specified at.
const ADDRESS_HASH_MULTIPLIER: i32 = 31;

/// The form an IP set's `Addresses` are STORED in, for the family the set declares.
///
/// Two steps, in order:
///
/// 1. Each entry is taken to the one notation of its value. An IPv6 address has
///    many — `::` elides a run of zero groups, a group's leading zeros may be
///    dropped, and the hex digits may be in either case — so an `IPV6` entry is
///    stored fully expanded, zero-padded and lowercase, with the prefix length it
///    was given. An IPv4 address has a single notation, so an `IPV4` entry is
///    stored exactly as submitted. An entry this does not recognize as an address
///    of the family (which [`is_cidr_block`] refuses on ingress) is kept verbatim
///    rather than silently changed.
/// 2. The entries are laid out as the SET they are: a value that appears twice is
///    one entry, and the list is ordered by [`address_set_order`] — the set's own
///    order, which does not depend on the order the request listed them in.
pub fn canonical_ip_set_addresses(ip_address_version: &str, addresses: &[String]) -> Vec<String> {
    let mut members: Vec<String> = Vec::with_capacity(addresses.len());
    let mut seen: ::std::collections::HashSet<String> =
        ::std::collections::HashSet::with_capacity(addresses.len());
    for address in addresses {
        let canonical = if ip_address_version == "IPV6" {
            canonical_ipv6_cidr(address).unwrap_or_else(|| address.clone())
        } else {
            address.clone()
        };
        if seen.insert(canonical.clone()) {
            members.push(canonical);
        }
    }
    address_set_order(members)
}

/// One IPv6 CIDR block in the fully expanded, zero-padded, lowercase notation of
/// its value, with its prefix length; `None` when `address` is not an IPv6 CIDR
/// block. See [`canonical_ip_set_addresses`].
fn canonical_ipv6_cidr(address: &str) -> Option<String> {
    let (literal, prefix) = address.split_once('/')?;
    let parsed = literal.parse::<::std::net::Ipv6Addr>().ok()?;
    let prefix_len = prefix.parse::<u32>().ok()?;
    let groups: Vec<String> = parsed
        .segments()
        .iter()
        .map(|group| format!("{group:04x}"))
        .collect();
    Some(format!("{}/{prefix_len}", groups.join(":")))
}

/// `members` in the order the set holds them: the order a read reports.
///
/// The set is laid out in a table, so the order its members are enumerated in is
/// the table's: an entry sits in the slot its own hash ([`address_bucket_hash`])
/// selects, and the slots are enumerated in ascending order. The table has
/// [`ADDRESS_TABLE_MIN_BUCKETS`] buckets and doubles whenever the membership would
/// fill more than
/// [`ADDRESS_TABLE_LOAD_NUMERATOR`]/[`ADDRESS_TABLE_LOAD_DENOMINATOR`] of it, so a
/// set's layout is fixed by its size and its members' values alone.
///
/// Two entries can select one bucket, and then the bucket alone does not say which
/// of them comes first; what decides it is their own values, compared as the
/// strings they are stored as. The position each held in the request decides
/// nothing — which is the rule this exists for: the order is a function of the
/// MEMBERSHIP, not of the request. The same addresses submitted in any order are
/// stored in one order, whichever write path stored them, and reading a set twice
/// reports one order.
pub fn address_set_order(members: Vec<String>) -> Vec<String> {
    let buckets = address_table_buckets(members.len());
    let mut ordered: Vec<(usize, String)> = members
        .into_iter()
        .map(|member| (address_bucket(&member, buckets), member))
        .collect();
    // By bucket, then by the entry's own value: the two keys the layout is a
    // function of, which is the pair order on these entries. Neither depends on
    // the order the request listed them in, and the entries are distinct, so no
    // two compare equal and the sort needs no stability.
    ordered.sort_unstable();
    ordered
        .into_iter()
        .map(|(_bucket, member)| member)
        .collect()
}

/// How many buckets the table holding `members` entries has: a power of two, at
/// least [`ADDRESS_TABLE_MIN_BUCKETS`], doubled until the entries occupy no more
/// than the load factor allows.
fn address_table_buckets(members: usize) -> usize {
    let mut buckets = ADDRESS_TABLE_MIN_BUCKETS;
    while members * ADDRESS_TABLE_LOAD_DENOMINATOR
        > buckets * ADDRESS_TABLE_LOAD_NUMERATOR
    {
        match buckets.checked_mul(2) {
            Some(doubled) => buckets = doubled,
            // Unreachable for any membership a set may hold (`MAX_IP_SET_ADDRESSES`
            // is 10,000): stated here so the growth cannot wrap instead.
            None => break,
        }
    }
    buckets
}

/// The bucket `member` sits in, in a table of `buckets` buckets.
///
/// The table masks the hash down to its size, so the high half of the hash would
/// never be looked at; it is folded into the low half first (hash exclusive-or its
/// own top sixteen bits), which is what keeps two entries differing only in their
/// high bits out of one bucket.
fn address_bucket(member: &str, buckets: usize) -> usize {
    let hash = address_bucket_hash(member);
    let spread = hash ^ ((hash as u32) >> 16) as i32;
    (spread as u32 as usize) & buckets.saturating_sub(1)
}

/// The hash of one entry: the polynomial hash of its characters, accumulated over
/// the 16-bit code units the string is made of and wrapping at 32 bits.
///
/// This is not a hash chosen FOR hashing — `std`'s own hasher would serve that, and
/// a crate's would serve it better. It is the function that decides which bucket an
/// entry sits in, and so decides the ORDER a read reports: another function would
/// answer another order, which is the one thing this must not do.
fn address_bucket_hash(member: &str) -> i32 {
    member.encode_utf16().fold(0_i32, |hash, unit| {
        hash.wrapping_mul(ADDRESS_HASH_MULTIPLIER)
            .wrapping_add(i32::from(unit))
    })
}
