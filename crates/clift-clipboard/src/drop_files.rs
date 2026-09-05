//! The `CF_HDROP` payload, as bytes.
//!
//! Platform-neutral on purpose. What Windows receives for "here is a file you
//! can paste into a folder" is a `DROPFILES` header followed by a list of
//! UTF-16 paths, and building that list correctly is the part worth testing on
//! every machine, including the ones that will never run it. The Win32 call
//! that hands the bytes over is three lines in the Windows module; the bytes
//! are here, where a test on any developer's laptop can read them back.
//!
//! This is the same split as the Startup script in `clift-inject`, and for the
//! same reason: a rule that can only be tested on Windows is a rule that goes
//! untested until a user finds it.

/// Bytes of the `DROPFILES` structure that precedes the path list.
///
/// `DWORD pFiles; POINT pt; BOOL fNC; BOOL fWide` = 4 + 8 + 4 + 4.
const HEADER_LEN: u32 = 20;

/// Builds the `CF_HDROP` payload for `paths`.
///
/// The list is UTF-16, each path NUL-terminated, with one further NUL closing
/// the list. `fWide` is set, which is what tells the receiver to read UTF-16
/// rather than ANSI; getting that wrong yields a paste of mojibake rather than
/// an error, so it is asserted in the tests below rather than trusted.
///
/// Returns `None` when a path cannot be represented: an interior NUL would
/// truncate the list silently and take the rest of the paths with it.
#[cfg_attr(not(windows), allow(dead_code))]
pub fn payload(paths: &[&str]) -> Option<Vec<u8>> {
    if paths.is_empty() || paths.iter().any(|path| path.contains('\0')) {
        return None;
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(&HEADER_LEN.to_le_bytes()); // pFiles
    bytes.extend_from_slice(&0_i32.to_le_bytes()); // pt.x
    bytes.extend_from_slice(&0_i32.to_le_bytes()); // pt.y
    bytes.extend_from_slice(&0_i32.to_le_bytes()); // fNC
    bytes.extend_from_slice(&1_i32.to_le_bytes()); // fWide: the list is UTF-16

    for path in paths {
        for unit in path.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&0_u16.to_le_bytes());
    }
    // The list itself is terminated by an empty entry.
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads a payload back the way Windows would, so the test states the
    /// format rather than repeating the code that wrote it.
    fn parse(bytes: &[u8]) -> (u32, bool, Vec<String>) {
        let offset = u32::from_le_bytes(bytes[0..4].try_into().expect("four bytes"));
        let wide = i32::from_le_bytes(bytes[16..20].try_into().expect("four bytes")) != 0;

        let (pairs, _) = bytes[offset as usize..].as_chunks::<2>();
        let units: Vec<u16> = pairs.iter().copied().map(u16::from_le_bytes).collect();
        let mut paths = Vec::new();
        let mut current = Vec::new();
        for unit in units {
            if unit == 0 {
                if current.is_empty() {
                    break; // the empty entry that closes the list
                }
                paths.push(String::from_utf16_lossy(&current));
                current.clear();
            } else {
                current.push(unit);
            }
        }
        (offset, wide, paths)
    }

    #[test]
    fn one_path_round_trips_through_the_documented_layout() {
        let path = r"C:\Users\jinfe\AppData\Local\Clift\inbox\notes.md";
        let bytes =
            payload(&[path]).unwrap_or_else(|| panic!("a plain path must be representable"));
        let (offset, wide, paths) = parse(&bytes);

        assert_eq!(offset, HEADER_LEN);
        assert!(wide, "fWide must be set or the receiver reads ANSI");
        assert_eq!(paths, vec![path.to_string()]);
        // Header, the path, its NUL, and the NUL that closes the list. Derived
        // from the path rather than written as a number: a constant here would
        // only ever agree with whatever the code happened to produce.
        assert_eq!(bytes.len(), 20 + (path.encode_utf16().count() + 2) * 2);
    }

    #[test]
    fn several_paths_stay_separate() {
        let bytes =
            payload(&[r"C:\a\one.png", r"C:\a\two.png"]).unwrap_or_else(|| panic!("representable"));
        let (_, _, paths) = parse(&bytes);
        assert_eq!(paths, vec![r"C:\a\one.png", r"C:\a\two.png"]);
    }

    /// Characters outside the basic plane become surrogate pairs. A path with a
    /// name from an agent's output is not hypothetical.
    #[test]
    fn non_ascii_survives_as_utf16() {
        let name = r"C:\a\报告-🚀.png";
        let bytes = payload(&[name]).unwrap_or_else(|| panic!("representable"));
        let (_, _, paths) = parse(&bytes);
        assert_eq!(paths, vec![name.to_string()]);
    }

    #[test]
    fn nothing_representable_is_refused_rather_than_truncated() {
        assert!(payload(&[]).is_none());
        // An interior NUL would end the entry early and drop everything after
        // it, which would paste a different file from the one asked for.
        assert!(payload(&["C:\\a\0b.png"]).is_none());
    }
}
