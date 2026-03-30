use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub fn parse_rfc3339_timestamp_ms(s: &str) -> Option<u64> {
    let odt = OffsetDateTime::parse(s, &Rfc3339).ok()?;
    let unix_ms = odt.unix_timestamp_nanos() / 1_000_000;
    u64::try_from(unix_ms).ok()
}
