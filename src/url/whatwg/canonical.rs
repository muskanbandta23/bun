//! One pass over the URLs that are met most: `http:`, `https:` and `file:`, written as they serialize.

use crate::authority::leading_port;
use crate::class::{
    self, BACKSLASH, DIGIT_OR_X, FRAGMENT, HOST_SCAN, PATH, SPECIAL_QUERY, TAIL_SCAN,
};
use crate::components::{Components, State};
use crate::path::PathSyntax;
use crate::{Punycode, Scheme, domain, ipv4};

pub(crate) enum Outcome {
    Canonical(Components),
    /// Canonical with a `/` behind it.
    LacksPath(Components),
    /// Canonical up to the first slash of the path. The end of the path and of the query are not filled in.
    CanonicalAuthority(Components),
    Unknown,
}

/// For a domain of which a label starts with a digit or an `x`.
#[inline(never)]
fn stays_as_it_is(domain: &[u8], punycode: Punycode) -> bool {
    if ipv4::ends_in_number(domain) {
        return ipv4::is_serialized(domain);
    }
    punycode == Punycode::Unchecked || domain::has_valid_ace_labels(domain)
}

/// Returns the stops of [`TAIL_SCAN`] from `host_start` on too.
#[inline(always)]
fn authority(input: &[u8], punycode: Punycode) -> Option<(Components, u64)> {
    let word = u64::from_le_bytes(*input.first_chunk()?);
    let is_https = word == u64::from_le_bytes(*b"https://");
    let is_http = word << 8 == u64::from_le_bytes(*b"\0http://");
    let is_file = word == u64::from_le_bytes(*b"file:///");
    if !(is_https | is_http | is_file) {
        return None;
    }
    let host_start = 7 + usize::from(is_https);
    let host_stops = HOST_SCAN.stops_in_32(input, host_start);
    let tail_stops = TAIL_SCAN.stops_in_64(input, host_start);

    let mut host_end = host_start + host_stops.trailing_zeros() as usize;
    let mut path_start = host_end;
    let mut port = 0;
    if input.get(host_end) != Some(&b'/') || host_stops == 0 {
        let mut stops = HOST_SCAN.scan(input, host_end.min(host_start + 32));
        let mut is_plain = true;
        host_end = loop {
            match stops.next() {
                Some(at) if class::host_traits(input[at]) & DIGIT_OR_X != 0 => is_plain = false,
                at => break at.unwrap_or(input.len()),
            }
        };
        if !is_plain && !stays_as_it_is(&input[host_start..host_end], punycode) {
            return None;
        }
        path_start = host_end;
        if let [b':', digits @ ..] = &input[host_end..] {
            let (count, value) = leading_port(digits);
            port = value?;
            if digits[0] == b'0' || port == if is_https { 443 } else { 80 } {
                return None;
            }
            path_start += 1 + count;
        }
        if input.get(path_start).is_some_and(|&byte| byte != b'/') {
            return None;
        }
    }
    if (host_end == host_start) != is_file || (is_file && input.get(9) == Some(&b'|')) {
        return None;
    }
    let parts = Components {
        scheme_end: host_start as u32 - 3,
        username_end: host_start as u32,
        host_start: host_start as u32,
        host_end: host_end as u32,
        path_start: path_start as u32,
        path_end: 0,
        query_end: 0,
        port,
        scheme: match (is_https, is_http) {
            (true, _) => Scheme::Https,
            (_, true) => Scheme::Http,
            _ => Scheme::File,
        },
        state: State::Hierarchical,
    };
    Some((parts, tail_stops))
}

/// Whether `input` is a valid URL whatever comes after its authority.
#[inline(always)]
pub(crate) fn has_canonical_authority(input: &[u8], punycode: Punycode) -> bool {
    authority(input, punycode).is_some()
}

#[inline(always)]
pub(crate) fn recognize(input: &[u8], punycode: Punycode) -> Outcome {
    const SYNTAX: PathSyntax = PathSyntax {
        set: PATH,
        backslash_is_slash: true,
    };
    let Some((mut parts, tail_stops)) = authority(input, punycode) else {
        return Outcome::Unknown;
    };
    let length = input.len();
    if parts.path_start as usize == length {
        parts.path_end = parts.path_start + 1;
        parts.query_end = parts.path_end;
        return Outcome::LacksPath(parts);
    }

    let mut scan = TAIL_SCAN.scan_with(input, parts.host_start as usize, tail_stops);
    scan.skip_to(parts.path_start as usize);
    let (mut path_end, mut query_end) = (length, length);
    let mut changes = PATH | BACKSLASH;
    for at in scan {
        let byte = input[at];
        if byte == b'?' && path_end == length {
            path_end = at;
            changes = SPECIAL_QUERY;
        } else if byte == b'#' && query_end == length {
            path_end = path_end.min(at);
            query_end = at;
            changes = FRAGMENT;
        } else if class::is_in(changes, byte)
            || (path_end == length
                && input[at - 1] == b'/'
                && SYNTAX.dot_segment(&input[at..]).is_some())
        {
            return Outcome::CanonicalAuthority(parts);
        }
    }
    parts.path_end = path_end as u32;
    parts.query_end = query_end as u32;
    Outcome::Canonical(parts)
}
