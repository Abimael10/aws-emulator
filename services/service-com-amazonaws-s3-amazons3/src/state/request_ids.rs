// SPDX-License-Identifier: CC0-1.0

//! The two request-correlation ids ONE response carries, and the seam that lets the
//! operation that served a request and the serializer that answers it name the same
//! request.
//!
//! Every S3 response carries them: "`x-amz-request-id`: A value created by Amazon S3 that
//! uniquely identifies the request" and "`x-amz-id-2`: A special token that helps Amazon
//! Web Services troubleshoot problems" (Amazon S3 API Reference, *Common Response
//! Headers*). A client reads them back as `ResponseMetadata.RequestId` and
//! `ResponseMetadata.HostId`.
//!
//! They are not only a response header. An EVENT NOTIFICATION names the call it came of by
//! the SAME request id — the classic record's `responseElements` carries both headers and
//! the EventBridge detail carries `request-id` — so a consumer can tell which API call
//! produced which event. That is why the pair is minted ONCE per served request, here,
//! rather than in the serializer: the handler publishes its event under the pair and then
//! records it ([`RequestCorrelation::answer`]) as the pair its response carries, which the
//! serializer takes ([`answering`]) when it writes the headers.
//!
//! The record lives in a THREAD-LOCAL because the two sides of it are one poll apart: a
//! handler records the pair as the LAST thing it does, with nothing awaited after it, and
//! the request walk serializes its output immediately afterwards, so nothing can run on
//! this thread between the record and the read. Two things keep that from degrading
//! quietly: the serializer TAKES the pair before anything that can fail, and every request
//! CLEARS this thread's slot as it is deserialized ([`forget_any_unanswered`]) — so the
//! slot a handler records into is always empty, and a response is named either by its own
//! request's pair or by a freshly minted one, never by another request's. A response
//! nothing recorded a pair for — every operation that publishes no event — is answered
//! with a freshly minted pair, so no response ever goes out without them.

use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

/// How many correlation pairs this process has minted.
///
/// A request id "uniquely identifies the request", so two responses may never carry the
/// same one: the ids are derived from this monotonic counter rather than from anything
/// about the request, so two identical requests are still told apart. They are correlation
/// values and not security tokens, which is why a counter-derived value is the right thing
/// for them.
static MINTED_PAIRS: AtomicU64 = AtomicU64::new(0);

/// The characters an `x-amz-request-id` is written in: the ten digits and the twenty-six
/// UPPERCASE letters.
///
/// The token is opaque, so its ALPHABET and its WIDTH are all of it a client can read, and
/// both are part of what S3 publishes: every id that header carries is a fixed-width token
/// of digits and uppercase letters (Amazon S3 API Reference, *Common Response Headers*,
/// whose `x-amz-request-id` is "a value created by Amazon S3 that uniquely identifies the
/// request"). A token drawn from a NARROWER alphabet — hexadecimal digits, or the lowercase
/// and hyphens of a UUID — is one a client that parses, logs or length-checks the id reads
/// as a different kind of value.
const REQUEST_ID_ALPHABET: &[u8; 36] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// How many characters that token carries, and so how wide the `<RequestId>` of an error
/// document is: the fixed width of S3's own ids.
const REQUEST_ID_CHARS: u32 = 16;

/// How many distinct tokens [`REQUEST_ID_CHARS`] characters of [`REQUEST_ID_ALPHABET`]
/// hold.
const REQUEST_ID_SPACE: u128 = (REQUEST_ID_ALPHABET.len() as u128).pow(REQUEST_ID_CHARS);

/// The stride the mint walks [`REQUEST_ID_SPACE`] in, one draw per request.
///
/// [`REQUEST_ID_SPACE`] factors into powers of two and three alone, and this stride is a
/// multiple of neither, so it is coprime with the space and `draw -> token` is a BIJECTION:
/// a request id "uniquely identifies the request", and no two draws of this counter can
/// land on one token. A digest of the draw would only make a collision unlikely. The stride
/// is large and irregular, so consecutive draws land far apart and the token stays the
/// opaque value a client treats it as.
const REQUEST_ID_STRIDE: u128 = 1_442_695_040_888_963_407;

/// How many bytes the `x-amz-id-2` token encodes.
///
/// That header carries "a special token that helps Amazon Web Services troubleshoot
/// problems" (Amazon S3 API Reference, *Common Response Headers*), published as a long
/// base64 token — the documented one is 64 characters wide, which is what these bytes
/// encode to, four characters per three bytes and no padding.
const EXTENDED_REQUEST_ID_BYTES: usize = 48;

thread_local! {
    /// The pair the operation serving this thread's request recorded for its response.
    static ANSWERING: RefCell<Option<RequestCorrelation>> = const { RefCell::new(None) };
}

/// The two ids one served request is named by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestCorrelation {
    /// `x-amz-request-id`: the fixed-width token of digits and uppercase letters that
    /// header carries — [`REQUEST_ID_CHARS`] characters of [`REQUEST_ID_ALPHABET`].
    pub request_id: String,
    /// `x-amz-id-2`: the longer base64 token of the pair, [`EXTENDED_REQUEST_ID_BYTES`]
    /// bytes wide.
    pub extended_request_id: String,
}

/// The `x-amz-request-id` of the pair drawn at `drawn`: that draw's own point of
/// [`REQUEST_ID_SPACE`], written in [`REQUEST_ID_ALPHABET`] and zero-padded to the fixed
/// [`REQUEST_ID_CHARS`] width S3's ids have.
fn request_id(drawn: u64) -> String {
    let base = REQUEST_ID_ALPHABET.len() as u128;
    // A `u64` draw is far below `REQUEST_ID_SPACE` and the stride is coprime with it, so
    // distinct draws land on distinct points — and the product of two values of those
    // magnitudes is well inside a `u128`.
    let mut point = u128::from(drawn) * REQUEST_ID_STRIDE % REQUEST_ID_SPACE;
    // The characters of the token, least significant first: a remainder of `base` indexes
    // the alphabet it was taken modulo, so every index is one the alphabet holds.
    let mut characters: Vec<char> = Vec::with_capacity(REQUEST_ID_CHARS as usize);
    for _ in 0..REQUEST_ID_CHARS {
        characters.push(char::from(REQUEST_ID_ALPHABET[(point % base) as usize]));
        point /= base;
    }
    // Written most significant character first, the way any positional notation is, which
    // is also what pads a small point out to the fixed width.
    characters.iter().rev().collect()
}

/// The `x-amz-id-2` of that same draw: the base64 token of [`EXTENDED_REQUEST_ID_BYTES`]
/// bytes derived from it.
///
/// A digest of the draw, rather than the draw itself, because this token carries no
/// structure a reader reads — it is the opaque value a support request quotes — and the
/// digest is what makes one draw's two ids tell nothing about each other.
fn extended_request_id(drawn: u64) -> String {
    use ::base64::Engine as _;
    use ::sha2::Digest as _;

    let mut digest = ::sha2::Sha512::new();
    digest.update(b"s3-extended-request-id:");
    digest.update(drawn.to_be_bytes());
    ::base64::engine::general_purpose::STANDARD_NO_PAD
        .encode(&digest.finalize()[..EXTENDED_REQUEST_ID_BYTES])
}

impl RequestCorrelation {
    /// A fresh pair, naming a request no other response names.
    ///
    /// Both ids come from ONE draw on the counter, so the `x-amz-request-id` and the
    /// `x-amz-id-2` of a response name the same request.
    pub fn mint() -> Self {
        let drawn = MINTED_PAIRS.fetch_add(1, Ordering::Relaxed);
        Self {
            request_id: request_id(drawn),
            extended_request_id: extended_request_id(drawn),
        }
    }

    /// Record this pair as the one the response this thread is about to answer carries.
    ///
    /// Called by a handler as the LAST thing it does, with nothing awaited after it,
    /// because the request walk serializes the output in the same poll: see the module
    /// docs. The slot is empty by then — every request clears it as it is deserialized
    /// ([`forget_any_unanswered`]) — so a pair found here means a handler recorded twice
    /// and is replaced and reported rather than silently reused.
    pub fn answer(&self) {
        let recorded = self.clone();
        let left_behind = ANSWERING.with(|slot| slot.borrow_mut().replace(recorded));
        if let Some(left_behind) = left_behind {
            ::tracing::warn!(
                request_id = %left_behind.request_id,
                "this request recorded a second request-correlation pair; the first is \
                 discarded rather than named on another request's response"
            );
        }
    }
}

/// The pair recorded for the response now being serialized, TAKEN rather than read, so a
/// pair one response left behind can never name a later one.
pub(crate) fn answering() -> Option<RequestCorrelation> {
    ANSWERING.with(|slot| slot.borrow_mut().take())
}

/// Drop whatever pair this thread may still hold, called at the START of every request
/// this service deserializes.
///
/// The seam above is correct because a handler records its pair as the last thing it does
/// and the request walk serializes the output in the same poll. This closes the one way
/// that could stop being true: a pair recorded for a response that was never serialized
/// (a serializer that errored on its way to one) would otherwise sit in this thread's slot
/// until something took it, and the next response serialized on this thread would be named
/// by another request's id. Clearing it here means the slot a handler records into is
/// always empty, so [`answering`] either returns THIS request's pair or none at all.
pub(crate) fn forget_any_unanswered() {
    if let Some(left_behind) = answering() {
        ::tracing::warn!(
            request_id = %left_behind.request_id,
            "a request-correlation pair was left unanswered on this thread; it is \
             discarded rather than named on another request's response"
        );
    }
}
