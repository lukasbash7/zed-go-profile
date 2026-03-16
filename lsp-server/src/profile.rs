use flate2::read::GzDecoder;
use prost::Message;
use std::io::Read;
use std::path::Path;

/// Include prost-generated protobuf types.
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/perftools.profiles.rs"));
}

/// Errors that can occur during profile parsing.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("Empty profile data")]
    Empty,
}

/// Check if data starts with gzip magic bytes (0x1f 0x8b).
fn is_gzip(data: &[u8]) -> bool {
    data.len() >= 2 && data[0] == 0x1f && data[1] == 0x8b
}

/// Parse a pprof profile from raw bytes.
/// Handles both gzip-compressed and raw protobuf formats.
pub fn parse_profile(data: &[u8]) -> Result<proto::Profile, ParseError> {
    if data.is_empty() {
        return Err(ParseError::Empty);
    }

    let bytes = if is_gzip(data) {
        let mut decoder = GzDecoder::new(data);
        let mut decompressed = Vec::new();
        decoder.read_to_end(&mut decompressed)?;
        decompressed
    } else {
        data.to_vec()
    };

    let profile = proto::Profile::decode(bytes.as_slice())?;
    Ok(profile)
}

/// Parse a pprof profile from a file path.
pub fn parse_profile_file(path: &Path) -> Result<proto::Profile, ParseError> {
    let data = std::fs::read(path)?;
    parse_profile(&data)
}

/// Resolve a string from the profile's string table by index.
/// Returns empty string if index is out of bounds or negative.
pub fn resolve_string(profile: &proto::Profile, index: i64) -> &str {
    if index < 0 {
        return "";
    }
    profile
        .string_table
        .get(index as usize)
        .map(|s| s.as_str())
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_gzip_true() {
        assert!(is_gzip(&[0x1f, 0x8b, 0x08, 0x00]));
    }

    #[test]
    fn test_is_gzip_false() {
        assert!(!is_gzip(&[0x0a, 0x0b, 0x0c]));
    }

    #[test]
    fn test_is_gzip_too_short() {
        assert!(!is_gzip(&[0x1f]));
        assert!(!is_gzip(&[]));
    }

    #[test]
    fn test_parse_empty_returns_error() {
        assert!(matches!(parse_profile(&[]), Err(ParseError::Empty)));
    }

    #[test]
    fn test_parse_raw_protobuf() {
        // Encode a minimal profile and parse it back.
        let profile = proto::Profile {
            string_table: vec!["".to_string(), "cpu".to_string(), "nanoseconds".to_string()],
            sample_type: vec![proto::ValueType { r#type: 1, unit: 2 }],
            ..Default::default()
        };
        let encoded = profile.encode_to_vec();
        let parsed = parse_profile(&encoded).unwrap();
        assert_eq!(parsed.string_table.len(), 3);
        assert_eq!(parsed.string_table[1], "cpu");
        assert_eq!(parsed.sample_type.len(), 1);
    }

    #[test]
    fn test_parse_gzipped_protobuf() {
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;

        let profile = proto::Profile {
            string_table: vec!["".to_string(), "heap".to_string(), "bytes".to_string()],
            sample_type: vec![proto::ValueType { r#type: 1, unit: 2 }],
            ..Default::default()
        };
        let encoded = profile.encode_to_vec();

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&encoded).unwrap();
        let gzipped = encoder.finish().unwrap();

        let parsed = parse_profile(&gzipped).unwrap();
        assert_eq!(parsed.string_table[1], "heap");
    }

    #[test]
    fn test_resolve_string() {
        let profile = proto::Profile {
            string_table: vec![
                "".to_string(),
                "main.go".to_string(),
                "runtime.mallocgc".to_string(),
            ],
            ..Default::default()
        };
        assert_eq!(resolve_string(&profile, 0), "");
        assert_eq!(resolve_string(&profile, 1), "main.go");
        assert_eq!(resolve_string(&profile, 2), "runtime.mallocgc");
        assert_eq!(resolve_string(&profile, 99), ""); // out of bounds
        assert_eq!(resolve_string(&profile, -1), ""); // negative
    }
}
