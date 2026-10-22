//! A reboot count stored with its complement across digital-core resets.

/// Encodes a reboot count so a reset during the write is detected.
#[must_use]
pub const fn encode_resets(count: u32) -> [u32; 2] {
    [count, !count]
}

/// Decodes a stored reboot count; invalid contents read as zero.
#[must_use]
pub const fn decode_resets([count, check]: [u32; 2]) -> u32 {
    if check == !count { count } else { 0 }
}
