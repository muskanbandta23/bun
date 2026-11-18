//! The values of the `ca` key.

/// AvailableCalendars
pub(crate) const AVAILABLE: [&str; 16] = [
    "buddhist",
    "chinese",
    "coptic",
    "dangi",
    "ethioaa",
    "ethiopic",
    "gregory",
    "hebrew",
    "indian",
    "islamic-civil",
    "islamic-tbla",
    "islamic-umalqura",
    "iso8601",
    "japanese",
    "persian",
    "roc",
];

pub(crate) fn bcp47_name(icu_name: &[u8]) -> &[u8] {
    match icu_name {
        b"ethiopic-amete-alem" => b"ethioaa",
        b"gregorian" => b"gregory",
        same => same,
    }
}

pub(crate) fn icu_name(bcp47_name: &[u8]) -> &[u8] {
    match bcp47_name {
        b"ethioaa" => b"ethiopic-amete-alem",
        b"gregory" => b"gregorian",
        same => same,
    }
}
