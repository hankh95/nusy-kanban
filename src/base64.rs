//! Minimal base64 encode/decode — no external crate dependency.
//!
//! Used by both `nk source` CLI (main.rs) and the kanban-server handlers
//! for git bundle transport over NATS.

const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const DECODE_TABLE: [u8; 128] = {
    let mut t = [255u8; 128];
    let mut i = 0u8;
    while i < 26 {
        t[(b'A' + i) as usize] = i;
        t[(b'a' + i) as usize] = i + 26;
        i += 1;
    }
    let mut i = 0u8;
    while i < 10 {
        t[(b'0' + i) as usize] = i + 52;
        i += 1;
    }
    t[b'+' as usize] = 62;
    t[b'/' as usize] = 63;
    t
};

/// Encode bytes to base64 string.
pub fn encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// Decode base64 string to bytes.
pub fn decode(input: &str) -> Vec<u8> {
    let bytes: Vec<u8> = input
        .bytes()
        .filter(|&b| b != b'=' && b != b'\n' && b != b'\r')
        .collect();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);

    for chunk in bytes.chunks(4) {
        if chunk.len() < 2 {
            break;
        }
        let b0 = DECODE_TABLE.get(chunk[0] as usize).copied().unwrap_or(0) as u32;
        let b1 = DECODE_TABLE.get(chunk[1] as usize).copied().unwrap_or(0) as u32;
        let b2 = if chunk.len() > 2 {
            DECODE_TABLE.get(chunk[2] as usize).copied().unwrap_or(0) as u32
        } else {
            0
        };
        let b3 = if chunk.len() > 3 {
            DECODE_TABLE.get(chunk[3] as usize).copied().unwrap_or(0) as u32
        } else {
            0
        };
        let triple = (b0 << 18) | (b1 << 12) | (b2 << 6) | b3;
        out.push(((triple >> 16) & 0xFF) as u8);
        if chunk.len() > 2 {
            out.push(((triple >> 8) & 0xFF) as u8);
        }
        if chunk.len() > 3 {
            out.push((triple & 0xFF) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_round_trip_empty() {
        let data = b"";
        assert_eq!(decode(&encode(data)), data.to_vec());
    }

    #[test]
    fn test_round_trip_hello() {
        let data = b"Hello, World!";
        assert_eq!(decode(&encode(data)), data.to_vec());
    }

    #[test]
    fn test_round_trip_binary() {
        let data: Vec<u8> = (0..=255).collect();
        assert_eq!(decode(&encode(&data)), data);
    }

    #[test]
    fn test_round_trip_1_byte() {
        assert_eq!(decode(&encode(b"A")), b"A".to_vec());
    }

    #[test]
    fn test_round_trip_2_bytes() {
        assert_eq!(decode(&encode(b"AB")), b"AB".to_vec());
    }

    #[test]
    fn test_round_trip_3_bytes() {
        assert_eq!(decode(&encode(b"ABC")), b"ABC".to_vec());
    }

    #[test]
    fn test_known_encoding() {
        // "Man" → "TWFu" (standard base64)
        assert_eq!(encode(b"Man"), "TWFu");
    }

    #[test]
    fn test_padding() {
        // "M" → "TQ==" (2 padding chars)
        assert_eq!(encode(b"M"), "TQ==");
        // "Ma" → "TWE=" (1 padding char)
        assert_eq!(encode(b"Ma"), "TWE=");
    }
}
