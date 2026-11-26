// SPDX-License-Identifier: CC0-1.0

//! The deprecation date an AMI carries: the ONE stored reading of `Image$DeprecationTime`
//! that a deprecation change writes and a cancellation takes away.
//!
//! "Enables deprecation of the specified AMI at the specified date and time"
//! (`EnableImageDeprecation`) and "cancels the deprecation of the specified AMI"
//! (`DisableImageDeprecation`) are the two ends of one stored fact: the `DeprecationTime`
//! the stored [`Image`] itself carries, which is the member every read of that image
//! reports and the one [`super::image_is_deprecated`] decides a deprecated AMI by. So the
//! write lives on the store beside the images
//! ([`ComAmazonawsEc2Amazonec2State::set_image_deprecation_time`]) rather than in either
//! handler, and no two operations can disagree about where the date is kept.
//!
//! What a request may ask for is bounded here too, because both ends read the same bounds:
//! the date is rounded the way EC2 rounds it
//! ([`deprecation_instant`]), it has to be in the future, it cannot be further out than
//! the limit the image's own visibility fixes ([`image_deprecation_limit`]), and only the
//! account that OWNS the AMI may change it at all ([`require_own_image`]).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::ops::types::Image;
use crate::scaffolded::Scaffolded;

use super::{
    ComAmazonawsEc2Amazonec2State, INVALID_AMI_ID_NOT_FOUND, echoed_resource_id, iso8601_utc,
    parse_iso8601_timestamp,
};

/// The minute an AMI's deprecation date is rounded to, in seconds.
const SECONDS_PER_MINUTE: i64 = 60;

/// Half a second, in nanoseconds: the point at which the sub-second part of a requested
/// instant counts as the next whole second before the minute rounding below.
const NANOS_PER_HALF_SECOND: u32 = 500_000_000;

/// The months in a year, for turning the two published limits into calendar arithmetic.
const MONTHS_PER_YEAR: u32 = 12;

/// How far ahead an AMI's deprecation date may be set: "the upper limit for DeprecateAt is
/// 10 years from now" (`EnableImageDeprecationRequest$DeprecateAt`).
pub const MAX_IMAGE_DEPRECATION_YEARS: u32 = 10;

/// How far ahead a PUBLIC AMI's deprecation date may be set, and what it is measured from:
/// "except for public AMIs, where the upper limit is 2 years from the creation date"
/// (`EnableImageDeprecationRequest$DeprecateAt`).
pub const MAX_PUBLIC_IMAGE_DEPRECATION_YEARS: u32 = 2;

/// The instant an AMI's deprecation is stored at, in epoch seconds: the instant the request
/// named, rounded to the nearest minute.
///
/// "If you specify a value for seconds, Amazon EC2 rounds the seconds to the nearest
/// minute" (`EnableImageDeprecationRequest$DeprecateAt`), and `Image$DeprecationTime` says
/// the same of what a read reports — so a request naming half a minute or more past a
/// minute is stored at the NEXT minute and one naming less at that minute, and the stored
/// date is a whole minute whatever precision the caller sent.
pub fn deprecation_instant(requested: ::aws_smithy_types::DateTime) -> i64 {
    let seconds = requested
        .secs()
        .saturating_add(i64::from(requested.subsec_nanos() >= NANOS_PER_HALF_SECOND));
    seconds
        .saturating_add(SECONDS_PER_MINUTE / 2)
        .div_euclid(SECONDS_PER_MINUTE)
        .saturating_mul(SECONDS_PER_MINUTE)
}

/// `epoch_seconds` as the `DeprecationTime` string an image is stored with — the crate's one
/// UTC rendering ([`iso8601_utc`]), so a date a deprecation stores reads back the way every
/// other stored instant does — or `None` for an instant this emulator cannot render.
pub fn deprecation_time_rendering(epoch_seconds: i64) -> Option<String> {
    let seconds = u64::try_from(epoch_seconds).ok()?;
    UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .map(iso8601_utc)
}

/// `at` as epoch seconds, or `None` for an instant before the epoch.
fn epoch_seconds(at: SystemTime) -> Option<i64> {
    i64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

/// `from` plus `years` CALENDAR years, in epoch seconds.
///
/// The whole civil-calendar conversion is `chrono`'s, like every other calendar arithmetic
/// in this crate, so a decade covers its own leap days rather than a fixed count of
/// seconds.
fn plus_years(from: i64, years: u32) -> Option<i64> {
    ::chrono::DateTime::from_timestamp(from, 0)?
        .checked_add_months(::chrono::Months::new(years * MONTHS_PER_YEAR))
        .map(|at| at.timestamp())
}

/// The latest instant the AMI `image` may be deprecated at, in epoch seconds, for a call
/// made at `now`.
///
/// "The upper limit for DeprecateAt is 10 years from now, except for public AMIs, where the
/// upper limit is 2 years from the creation date"
/// (`EnableImageDeprecationRequest$DeprecateAt`): WHICH limit holds is a fact of the image —
/// whether it has public launch permissions ([`Image::public`]) — and so is what each is
/// measured from, the call's own instant for a private AMI and the image's `CreationDate`
/// for a public one. `None` is a limit with no representable instant, which leaves the
/// request unbounded rather than refused over arithmetic.
pub fn image_deprecation_limit(image: &Image, now: SystemTime) -> Option<i64> {
    if image.public.unwrap_or(false) {
        let created_at = parse_iso8601_timestamp(image.creation_date.as_deref())?;
        plus_years(created_at.secs(), MAX_PUBLIC_IMAGE_DEPRECATION_YEARS)
    } else {
        plus_years(epoch_seconds(now)?, MAX_IMAGE_DEPRECATION_YEARS)
    }
}

/// Refuse an AMI id this surface can read but the account can see no image for: HTTP 400
/// `InvalidAMIID.NotFound`, worded the way the deprecation operations word that code.
///
/// EC2's per-operation validators do not agree about how they WORD an id they cannot
/// resolve: the describes of the account's images answer the bracketed-list sentence
/// ([`super::invalid_ami_id_not_found`]) and the strict readers answer their own
/// ([`super::unresolvable_image_id`]). Which one an operation answers is a fact of that
/// operation, recorded by its caller — the recorded `DisableImageDeprecation` of a
/// fabricated long-form id is answered THIS sentence — so it lives beside the two
/// operations that answer it. Its one variable part is the id the request sent.
pub fn image_id_not_found_for_deprecation<E>(image_id: &str) -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_AMI_ID_NOT_FOUND,
        400,
        format!("Invalid ImageId: {}", echoed_resource_id(image_id)),
    )
}

/// Refuse a deprecation date that is not in the future: HTTP 400 `InvalidParameterValue`,
/// the code EC2 answers for a parameter whose value is outside what the member documents.
///
/// "You can't specify a date in the past" (`EnableImageDeprecationRequest$DeprecateAt`),
/// which the operation words as the failure of the call and its cause — the sentence the
/// recorded `EnableImageDeprecation` of a past date is answered, and wholly fixed text: the
/// value at fault is named by no part of it.
pub fn deprecation_date_in_the_past<E>() -> Scaffolded<E> {
    Scaffolded::invalid_parameter_value(
        "EnableImageDeprecation failed. Caused by specified deprecation time in the past",
    )
}

/// Refuse a deprecation date beyond the upper limit the image's own visibility fixes: HTTP
/// 400 `InvalidParameterValue`.
///
/// "The upper limit for DeprecateAt is 10 years from now, except for public AMIs, where the
/// upper limit is 2 years from the creation date"
/// (`EnableImageDeprecationRequest$DeprecateAt`) — so WHICH limit the refusal names follows
/// the image it was asked about. Worded as the failure of the call and its cause, the way
/// this operation's recorded refusal of a date outside the member's range is worded, with
/// the published figure read off the constant that bounds it rather than written a second
/// time.
pub fn deprecation_date_beyond_limit<E>(public: bool) -> Scaffolded<E> {
    let cause = if public {
        format!(
            "more than {MAX_PUBLIC_IMAGE_DEPRECATION_YEARS} years from the creation date of a \
             public AMI"
        )
    } else {
        format!("more than {MAX_IMAGE_DEPRECATION_YEARS} years in the future")
    };
    Scaffolded::invalid_parameter_value(format!(
        "EnableImageDeprecation failed. Caused by specified deprecation time {cause}"
    ))
}

/// Refuse a request to change an AMI the calling account does not own: HTTP 400
/// `AuthFailure`, worded the way EC2 words an image request the caller is not authorized
/// for — the ids it was not authorized for, as a bracketed list.
pub fn not_authorized_for_image<E>(image_id: &str) -> Scaffolded<E> {
    Scaffolded::auth_failure(format!(
        "Not authorized for images: [{}]",
        echoed_resource_id(image_id)
    ))
}

/// Resolve the AMI a deprecation change names, refusing one this account may not change.
///
/// "Only the AMI owner can enable or disable AMI deprecation" (Deprecate an Amazon EC2 AMI):
/// the crate's one owner rule ([`super::require_own_image_by`], where its three causes and
/// their order are worded), answered with the NOT-FOUND sentence the deprecation operations
/// were recorded answering ([`image_id_not_found_for_deprecation`]).
pub fn require_own_image<'store, E>(
    store: &'store ComAmazonawsEc2Amazonec2State,
    image_id: &str,
) -> Result<&'store Image, Scaffolded<E>> {
    super::require_own_image_by(store, image_id, image_id_not_found_for_deprecation)
}

impl ComAmazonawsEc2Amazonec2State {
    /// Write `deprecation_time` onto the AMI `image_id` — `None` takes the date away — and
    /// answer whether there was such an image to write to.
    ///
    /// The ONE writer of that member, so enabling and cancelling a deprecation keep the date
    /// in the same place and every read of the image reports what the last change left. The
    /// bytes the stored rendering retains are charged against the same budget every other
    /// stored string is charged against, as a NET change: the date that leaves is refunded
    /// and the one that arrives is charged, so repeated changes cannot leak the budget.
    pub fn set_image_deprecation_time(
        &mut self,
        image_id: &str,
        deprecation_time: Option<String>,
    ) -> bool {
        let Some(image) = self.images.get_mut(image_id) else {
            return false;
        };
        let refund = image.deprecation_time.as_deref().map_or(0, str::len);
        let charge = deprecation_time.as_deref().map_or(0, str::len);
        image.deprecation_time = deprecation_time;
        self.release_stored_bytes(refund);
        self.add_stored_bytes(charge);
        true
    }

    /// The `DeprecationTime` the AMI `image_id` carries, as the store holds it: `None` for an
    /// image with no deprecation date, or no image at all.
    ///
    /// The read-back every deprecation change answers from, so what a caller is told is the
    /// date a later `DescribeImages` of the same image reports.
    pub fn image_deprecation_time(&self, image_id: &str) -> Option<String> {
        self.images
            .get(image_id)
            .and_then(|image| image.deprecation_time.clone())
    }
}
