// SPDX-License-Identifier: CC0-1.0

//! WHERE a channel's outputs are ANNOUNCED for playback: the pair of URLs every output of
//! every channel this service reports carries, built here and nowhere else.
//!
//! `ResponseOutputItem$PlaybackUrl` is "The URL that your player uses for playback" and
//! `ResponseOutputItem$DualStackPlaybackUrl` "The dual-stack (IPv4 and IPv6) URL that your
//! player uses for playback"
//! (`smithy_query(query="shape",shape="com.amazonaws.mediatailor#ResponseOutputItem",model_path="services/service-com-amazonaws-mediatailor-mediatailor/smithy-model/com-amazonaws-mediatailor-mediatailor.json")`),
//! so both are addresses a client DIALS afterwards — a player, a CDN origin
//! configuration, a health probe. Neither is an address of the service's control-plane API
//! endpoint: a channel's manifests are served by the channel-assembly plane, under its own
//! host, and the API host answers the control-plane routes only.
//!
//! An address this emulator hands a caller therefore follows the same reading every other
//! announced data plane of this tree follows
//! ([`::service_common::data_plane_addressing`]), in two cases:
//!
//! * a request that reached the EMULATOR'S OWN endpoint, or any request on an emulator that
//!   IS served on a port, is announced that `host[:port]` — the one address this process
//!   answers on, and the only one a client on this machine resolves — with the whole
//!   identity of the output kept in the PATH ([`playback_path`]);
//! * a request that addressed one of AWS's own hosts on an emulator served on no port at
//!   all comes from a client that resolves them and there is no own address to announce in
//!   its place, so the host AWS itself publishes channel-assembly playback on stands
//!   ([`channel_assembly_host`]).
//!
//! The choice is made ONCE, when the channel is created, and persisted in that channel's
//! stored outputs, so every read of the channel reports the stored pair unchanged.

use super::*;

/// The scheme-and-separator prefix every playback URL carries: playback is fetched over
/// TLS, and every listener of this process presents the core's certificate.
pub const PLAYBACK_SCHEME: &str = "https://";

/// The label that names the channel-assembly plane inside the host AWS publishes playback
/// on — the fixed part of that host, together with [`SERVICE_DNS_LABEL`] and the Region.
const CHANNEL_ASSEMBLY_LABEL: &str = "channel-assembly";

/// This service's own DNS label, which is its modelled `arnNamespace` and the label its
/// regional endpoints sit under.
const SERVICE_DNS_LABEL: &str = "mediatailor";

/// The partition DNS suffix a single-stack AWS endpoint sits under (the `aws` partition's
/// `dnsSuffix`), and the one its dual-stack (IPv4 and IPv6) counterpart sits under (the
/// `dualStackDnsSuffix` the model's `standardRegionalEndpoints.partitionSpecialCases.aws`
/// names: `https://mediatailor.{region}.{dualStackDnsSuffix}`).
const SINGLE_STACK_DNS_SUFFIX: &str = "amazonaws.com";
const DUAL_STACK_DNS_SUFFIX: &str = "api.aws";

/// How many characters the two generated labels of a channel-assembly host are written
/// as: the channel's own label, long enough that it names exactly one channel, and the
/// short subdomain label the account's channel-assembly endpoints share.
///
/// Both values are GENERATED, never part of the host's fixed text, and both are derived
/// from content (never from the clock or a random source) so a channel renders the same
/// host on every later read.
pub const CHANNEL_HOST_LABEL_CHARS: usize = 32;
pub const ASSEMBLY_SUBDOMAIN_LABEL_CHARS: usize = 6;

/// The characters a generated host label is written in: the lowercase letters and digits,
/// which is what a DNS label carries.
const LABEL_ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";

/// The two hosts one channel's playback URLs sit on — the single-stack one and its
/// dual-stack counterpart — each an authority with no scheme and no path.
#[derive(Debug, Clone)]
pub struct PlaybackHosts {
    pub single_stack: String,
    pub dual_stack: String,
}

impl PlaybackHosts {
    /// The hosts the channel `channel_arn` names is ANNOUNCED on to the request in `ctx`.
    ///
    /// See the module docs for the two cases. An emulator with an own address to announce
    /// announces it for BOTH members: this process answers on one address, whichever IP
    /// stack a client reaches it over, so the dual-stack member names that same reachable
    /// address rather than a second host nothing answers on.
    pub fn announced(
        ctx: &::smithy_server_runtime::OperationContext<'_>,
        region: &str,
        channel_arn: &str,
    ) -> Self {
        let base = ctx.endpoint();
        let own = ::service_common::data_plane_addressing::reached_emulator_endpoint_of_parts(
            ctx.parts(),
        )
        .or_else(|| {
            ::service_common::data_plane_addressing::served_endpoint_authority(base.as_str())
        });
        match own {
            Some(authority) => Self {
                single_stack: authority.clone(),
                dual_stack: authority,
            },
            None => Self {
                single_stack: channel_assembly_host(channel_arn, region, SINGLE_STACK_DNS_SUFFIX),
                dual_stack: channel_assembly_host(channel_arn, region, DUAL_STACK_DNS_SUFFIX),
            },
        }
    }

    /// The `(PlaybackUrl, DualStackPlaybackUrl)` pair of ONE output of the channel named
    /// `channel_name`: the two announced hosts, each carrying that output's own playback
    /// path ([`playback_path`]).
    pub fn output_urls(
        &self,
        channel_name: &str,
        output: &RequestOutputItem,
    ) -> (String, String) {
        let path = playback_path(channel_name, output);
        (
            format!("{PLAYBACK_SCHEME}{}{path}", self.single_stack),
            format!("{PLAYBACK_SCHEME}{}{path}", self.dual_stack),
        )
    }
}

/// The path ONE output of a channel is played back at: the plane's version, the form of
/// the playlist, the channel and the output's own manifest — so one path addresses exactly
/// one output of one channel, and it survives being moved off AWS's hostname onto this
/// emulator's address.
///
/// The manifest carries the extension of the playlist form the output asked for: an output
/// that asked for DASH alone is an `.mpd` manifest under the `dash` form, every other
/// output an `.m3u8` playlist under the `channel` form.
pub fn playback_path(channel_name: &str, output: &RequestOutputItem) -> String {
    let dash_only =
        output.dash_playlist_settings.is_some() && output.hls_playlist_settings.is_none();
    let (form, extension) = if dash_only {
        ("dash", "mpd")
    } else {
        ("channel", "m3u8")
    };
    format!(
        "/v1/{form}/{channel_name}/{}.{extension}",
        output.manifest_name
    )
}

/// The host AWS ITSELF publishes a channel's playback on, under the partition DNS suffix
/// `dns_suffix` — which is what this service announces only when it has no own address to
/// announce in its place (see the module docs).
///
/// Shaped like the real one: the channel's own generated label, the account's
/// channel-assembly subdomain label, then the fixed label sequence that names the
/// channel-assembly plane of this service in `region`, then the partition suffix. The
/// labels are DERIVED from the channel's ARN and from the account and Region, never minted
/// per call, so the host is stable for the life of the channel and distinct between
/// channels.
pub fn channel_assembly_host(channel_arn: &str, region: &str, dns_suffix: &str) -> String {
    let channel_label = channel_host_label(channel_arn);
    let subdomain = assembly_subdomain_label(region);
    format!(
        "{channel_label}.{subdomain}.{CHANNEL_ASSEMBLY_LABEL}.{SERVICE_DNS_LABEL}.{region}.{dns_suffix}"
    )
}

/// The generated label that names ONE channel inside the host AWS publishes its playback
/// on: hexadecimal, derived from the channel's ARN — so it names one channel of one account
/// and Region — and domain-separated from every other label this service derives from the
/// same ARN.
fn channel_host_label(channel_arn: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = hex::encode(Sha256::digest(
        format!("{channel_arn}#channel-assembly-host").as_bytes(),
    ));
    digest[..CHANNEL_HOST_LABEL_CHARS].to_string()
}

/// The short subdomain label every channel-assembly host of this account and Region
/// shares. It is the ACCOUNT's and not a channel's: too short to name one channel of many,
/// it sits between the per-channel label and the plane's own labels, so it is derived from
/// the account and the Region rather than from any one channel.
fn assembly_subdomain_label(region: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(
        format!(
            "{}/{region}#channel-assembly-subdomain",
            ::engine::emulator_account_id()
        )
        .as_bytes(),
    );
    digest
        .iter()
        .take(ASSEMBLY_SUBDOMAIN_LABEL_CHARS)
        .map(|byte| LABEL_ALPHABET[usize::from(*byte) % LABEL_ALPHABET.len()] as char)
        .collect()
}
