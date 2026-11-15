// SPDX-License-Identifier: CC0-1.0

//! The connectivity information a Greengrass core device publishes for the client devices
//! that connect to its MQTT broker: the store `UpdateConnectivityInfo` writes and
//! `GetConnectivityInfo` reads back.
//!
//! One record per core device thing name, holding the endpoint list as it was written and
//! the version token minted for that write. Both operations of the pair read and write
//! this ONE record, so a read reports exactly what the last write stored, and the version
//! a write answers is the version the record then holds.

use super::*;
use crate::ops::types::ConnectivityInfo;

/// The most core device thing names this store holds connectivity information for.
///
/// The same ceiling as the core-device fleet ([`MAX_CORE_DEVICES`]): AWS records
/// connectivity information for a thing name whether or not a core device has reported
/// itself under it, so the two stores are keyed by the same names and are bounded alike.
pub const MAX_CONNECTIVITY_INFO_THINGS: usize = MAX_CORE_DEVICES;

/// The most endpoints one core device publishes at once.
///
/// A core device publishes the addresses client devices can reach its broker on — one per
/// network interface it is reachable over, plus any the operator adds — so the list is
/// short. The ceiling keeps one caller from growing the record without bound.
pub const MAX_CONNECTIVITY_INFO_ENTRIES: usize = 100;

/// The connectivity information one core device has published.
#[derive(Debug, Clone)]
pub struct ConnectivityInfoRecord {
    /// The version token the write that stored this list was answered with, and the one a
    /// client compares to detect a concurrent overwrite. Minted fresh per write
    /// ([`super::new_identifier`]).
    pub version: String,
    /// The endpoints, in the order they were written: the list is the core device's
    /// current connectivity information, so a write REPLACES it rather than adding to it.
    pub entries: Vec<ConnectivityInfo>,
}

/// Why a connectivity-information write was not stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectivityInfoRefusal {
    /// The list, or one of its entries, is not readable as connectivity information.
    Unparsable,
    /// The store already holds records for [`MAX_CONNECTIVITY_INFO_THINGS`] thing names.
    StoreFull,
}

/// The port value that names no port: the `@default` `ConnectivityInfo$portNumber` carries,
/// so an entry that names no port and one that names this carry the same value on the wire.
pub const UNNAMED_PORT: i32 = 0;

/// Whether `entries` is readable as a core device's connectivity information.
///
/// Connectivity information is a list of ENDPOINTS client devices connect to, so the list
/// has to name at least one, and each entry has to name one: an id to know it by, an
/// address to reach it at, and a port to reach it on.
///
/// An EMPTY list names none. `connectivityInfo` is `@required`
/// (`smithy_query(query="operation",operation="UpdateConnectivityInfo")`), and a request
/// that carries it empty — or carries under that member nothing this surface can read as
/// an endpoint list — asks to publish connectivity information while naming none of it,
/// which is the condition AWS reports as the member being unreadable.
///
/// Each of the three per-entry members is a member of `ConnectivityInfo`
/// (`smithy_query(query="shape",shape="ConnectivityInfo")`) and each is needed to read the
/// entry as an endpoint, however well formed its JSON is:
///
/// * `id` — "An ID for the connectivity information", which is what the discovery response
///   a client device reads names each endpoint by;
/// * `hostAddress` — "The IP address or DNS address where client devices can connect to an
///   MQTT broker on the Greengrass core device"; and
/// * `portNumber` — "The port where the MQTT broker operates on the core device". The
///   member carries a `@default` of `0`, so an absent port and a port of `0` are the same
///   value on the wire, and `0` names no port an MQTT broker listens on — so an entry
///   carrying it names none.
///
/// Blank counts as absent for the two text members: a string of spaces is no id and no
/// address either. `metadata` is the one member an endpoint does not need — "Additional
/// metadata to provide to client devices" — and is stored as it arrived, as is every
/// value: the port is bounded by the model (`PortNumberInt`, `range 0..65535`) and refused
/// before any handler runs, and the id, the address and the metadata are opaque text this
/// service does not interpret further.
#[must_use]
pub fn readable_connectivity_info(entries: &[ConnectivityInfo]) -> bool {
    !entries.is_empty()
        && entries.len() <= MAX_CONNECTIVITY_INFO_ENTRIES
        && entries.iter().all(names_an_endpoint)
}

/// Whether one entry names an endpoint, by the rule [`readable_connectivity_info`] states.
fn names_an_endpoint(entry: &ConnectivityInfo) -> bool {
    let named = |value: &Option<String>| {
        value
            .as_deref()
            .is_some_and(|text| !text.trim().is_empty())
    };
    named(&entry.id) && named(&entry.host_address) && entry.port_number != UNNAMED_PORT
}

impl ComAmazonawsGreengrassv2Greengrassv2State {
    /// The connectivity information `thing_name` has published, or `None` when that core
    /// device has published none.
    #[must_use]
    pub fn connectivity_info(&self, thing_name: &str) -> Option<&ConnectivityInfoRecord> {
        self.connectivity_info.get(thing_name)
    }

    /// Store `entries` as `thing_name`'s connectivity information under a freshly minted
    /// version, answering that version.
    ///
    /// The write REPLACES the record: the list is the core device's current connectivity
    /// information, so what the previous write stored is gone and the new version token is
    /// the only one that names the stored list. A write for a thing name the store does not
    /// already hold is refused once the store is at [`MAX_CONNECTIVITY_INFO_THINGS`], so it
    /// is bounded by a count a caller cannot raise.
    pub fn publish_connectivity_info(
        &mut self,
        thing_name: &str,
        entries: Vec<ConnectivityInfo>,
    ) -> Result<String, ConnectivityInfoRefusal> {
        if !readable_connectivity_info(&entries) {
            return Err(ConnectivityInfoRefusal::Unparsable);
        }
        if !self.connectivity_info.contains_key(thing_name)
            && self.connectivity_info.len() >= MAX_CONNECTIVITY_INFO_THINGS
        {
            return Err(ConnectivityInfoRefusal::StoreFull);
        }
        let version = new_identifier();
        self.connectivity_info.insert(
            thing_name.to_string(),
            ConnectivityInfoRecord {
                version: version.clone(),
                entries,
            },
        );
        Ok(version)
    }
}
