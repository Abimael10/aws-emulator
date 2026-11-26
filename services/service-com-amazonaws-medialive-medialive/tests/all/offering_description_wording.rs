// SPDX-License-Identifier: CC0-1.0

//! The PROSE an offering's `OfferingDescription` carries, end to end over the
//! restJson1 wire.
//!
//! The rule under test: `OfferingDescription` reads the reserved resource out in
//! words the way the member's own example reads one out ("HD AVC output at 10-20
//! Mbps, 30 fps, and standard VQ in US West (Oregon)") — that the resource is
//! reserved, its channel class, resolution, codec, what is reserved and the band
//! of bitrates it may run at, then the qualifiers its resource type carries, then
//! the Region by the display name AWS publishes it under. The structured
//! `ResourceSpecification` beside it is where a program reads the configuration
//! from, so the description names no enum constant.
//!
//! The suites are TABLES: the rows differ along the resource type (which decides
//! which qualifiers an offering carries), the bitrate band (every published one,
//! including both ends), the channel class, the framerate and the video quality,
//! and — for the Region — along the endpoint the catalog is read through,
//! including one Region no display name is published for. The offering the issue
//! cites is ONE row.
//!
//! Every test drives the service the way a client does, through
//! `MultiServiceRouter::dispatch` with real HTTP requests, so what is asserted is
//! the emulator's wire behaviour and not an internal function's return value.

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

use crate::wire::{call_in_region, medialive, DEFAULT_REGION};

/// The collection URI `ListOfferings` is bound to
/// (`smithy_query(query="operation", operation="ListOfferings")`: GET
/// /prod/offerings).
const OFFERINGS: &str = "/prod/offerings";

/// The ONE offering `filters` admits, read through `region`'s endpoint.
///
/// Every filter member of `ListOfferings` names one member of the reservable
/// configuration, so a query that pins every member a configuration of that
/// resource type carries admits exactly one offering — which is what makes the
/// description asserted below the description OF that configuration.
async fn only_offering(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    filters: &str,
) -> Value {
    let uri = format!("{OFFERINGS}?{filters}");
    let (status, body) = call_in_region(router, region, "GET", &uri, None).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected GET {uri} to answer 200, got {status}: {body}"
    );
    let offerings = body["offerings"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected an offerings list, got {body}"));
    assert_eq!(
        offerings.len(),
        1,
        "case {case:?}: expected the filters to admit exactly one offering, got {offerings:?}"
    );
    offerings[0].clone()
}

/// The `offeringDescription` of one reported offering.
fn description(case: &str, offering: &Value) -> String {
    offering["offeringDescription"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a description, got {offering}"))
        .to_string()
}

/// The filters that pin ONE reserved input configuration — the one the recorded
/// catalog reads out as "Reserved Single Pipeline UHD AVC input at <10mbps" —
/// used by the Region suite, which is about the trailing Region name alone.
const ONE_INPUT: &str = "resourceType=INPUT&codec=AVC&resolution=UHD\
                         &maximumBitrate=MAX_10_MBPS&channelClass=SINGLE_PIPELINE";

/// Rule: an offering's description reads its configuration out in prose — the
/// channel class, resolution and codec in the catalog's own words, the resource
/// reserved in lower case, the band of bitrates it may run at, then the
/// qualifiers its resource type carries, then the Region's display name.
///
/// The rows differ along the resource type, the bitrate band (all three
/// published ones), the channel class, and the qualifiers the type carries (an
/// output's framerate and video quality, a channel's special feature, and a
/// multiplex's none).
#[tokio::test]
async fn a_description_reads_the_reserved_configuration_out_in_words() {
    let router = medialive();

    // (case, the filters that pin one configuration, the description it reads as)
    let table: [(&str, &str, &str); 6] = [
        (
            "a single pipeline UHD AVC input in the lowest bitrate band",
            ONE_INPUT,
            "Reserved Single Pipeline UHD AVC input at <10mbps in US West (Oregon)",
        ),
        (
            "a standard HD HEVC output in the middle band, at 30 fps and standard VQ",
            "resourceType=OUTPUT&codec=HEVC&resolution=HD&maximumBitrate=MAX_20_MBPS\
             &channelClass=STANDARD&maximumFramerate=MAX_30_FPS&videoQuality=STANDARD",
            "Reserved Standard HD HEVC output at 10-20mbps, 30 fps, and standard VQ \
             in US West (Oregon)",
        ),
        (
            "a standard UHD AV1 output in the top band, at 60 fps and premium VQ",
            "resourceType=OUTPUT&codec=AV1&resolution=UHD&maximumBitrate=MAX_50_MBPS\
             &channelClass=STANDARD&maximumFramerate=MAX_60_FPS&videoQuality=PREMIUM",
            "Reserved Standard UHD AV1 output at 20-50mbps, 60 fps, and premium VQ \
             in US West (Oregon)",
        ),
        (
            "a reserved channel carrying a special feature",
            "resourceType=CHANNEL&codec=MPEG2&resolution=SD&maximumBitrate=MAX_10_MBPS\
             &channelClass=STANDARD&specialFeature=ADVANCED_AUDIO",
            "Reserved Standard SD MPEG2 channel at <10mbps, advanced audio \
             in US West (Oregon)",
        ),
        (
            "a reserved multiplex, which carries none of the scoped members",
            "resourceType=MULTIPLEX&codec=LINK&resolution=FHD&maximumBitrate=MAX_20_MBPS\
             &channelClass=SINGLE_PIPELINE",
            "Reserved Single Pipeline FHD LINK multiplex at 10-20mbps in US West (Oregon)",
        ),
        (
            "a single pipeline SD AUDIO input in the lowest band",
            "resourceType=INPUT&codec=AUDIO&resolution=SD&maximumBitrate=MAX_10_MBPS\
             &channelClass=SINGLE_PIPELINE",
            "Reserved Single Pipeline SD AUDIO input at <10mbps in US West (Oregon)",
        ),
    ];

    for (case, filters, expected) in table {
        let offering = only_offering(&router, case, DEFAULT_REGION, filters).await;
        assert_eq!(
            description(case, &offering),
            expected,
            "case {case:?}: expected the published wording, got {:?}",
            offering["offeringDescription"]
        );
    }
}

/// Rule: a description names the Region it reserves capacity in by the display
/// name AWS publishes that Region under, and names the Region by its code when
/// no display name is published for it — while the configuration it reads out is
/// the same through every endpoint, because an offering id names one
/// configuration everywhere.
///
/// The rows differ along the Region the catalog is read through: three Regions
/// with published display names (none of them the one the member's example
/// names, apart from the default this suite's other rows use) and one code no
/// display name is published for.
#[tokio::test]
async fn a_description_names_the_region_it_reserves_capacity_in() {
    let router = medialive();

    // (the Region the read is signed for, the name its description ends in)
    let table: [(&str, &str); 4] = [
        (DEFAULT_REGION, "US West (Oregon)"),
        ("eu-west-1", "Europe (Ireland)"),
        ("ap-northeast-1", "Asia Pacific (Tokyo)"),
        // A Region code no display name is published for: it names itself,
        // rather than borrowing another Region's name.
        ("zz-nowhere-1", "zz-nowhere-1"),
    ];

    let mut configurations: Vec<(String, String)> = Vec::new();
    for (region, display_name) in table {
        let case = format!("read through {region}");
        let offering = only_offering(&router, &case, region, ONE_INPUT).await;
        let reported = description(&case, &offering);
        let tail = format!(" in {display_name}");
        assert!(
            reported.ends_with(&tail),
            "case {case:?}: expected the description to end in {tail:?}, got {reported:?}"
        );
        let configuration = reported
            .strip_suffix(&tail)
            .unwrap_or(&reported)
            .to_string();
        configurations.push((case, configuration));
    }

    let (first_case, first) = configurations[0].clone();
    for (case, configuration) in &configurations {
        assert_eq!(
            configuration, &first,
            "case {case:?}: expected the same configuration as {first_case:?} reads out, \
             got {configuration:?} against {first:?}"
        );
    }
}
