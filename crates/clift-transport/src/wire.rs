//! SFTP version 3 as bytes: the requests Clift sends and the replies it reads.
//!
//! Nothing here performs I/O. A packet is a big-endian length followed by that
//! many bytes, the first of which is the type; everything else is fixed-width
//! integers and length-prefixed strings. Paths travel as those strings, so no
//! quoting, escaping or tokenising is involved at any point.
//!
//! Every reply comes from the remote host, which is not a trusted source of
//! bytes. Each read is bounds checked, a length that runs past the packet or
//! past [`MAX_PACKET`] is refused, and bytes left over at the end of a packet
//! are an error rather than something to skip: a reply that does not end where
//! it says it ends means the stream is no longer framed, and whatever follows
//! it cannot be trusted either.

use std::fmt;

/// The protocol version Clift speaks, and the one OpenSSH's server answers.
pub const VERSION: u32 = 3;

/// The largest packet accepted from the server.
///
/// OpenSSH's own client and server use the same figure. Nothing Clift asks
/// for comes close: the largest reply is a directory listing, which the server
/// sends in pieces of at most a hundred entries.
pub const MAX_PACKET: usize = 256 * 1024;

/// Bytes of file content per write request. The size OpenSSH's own client
/// uses by default, and far inside [`MAX_PACKET`].
pub const WRITE_CHUNK: usize = 32 * 1024;

const INIT: u8 = 1;
const VERSION_REPLY: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const FSTAT: u8 = 8;
const SETSTAT: u8 = 9;
const FSETSTAT: u8 = 10;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const RENAME: u8 = 18;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

const ATTR_SIZE: u32 = 0x0000_0001;
const ATTR_UIDGID: u32 = 0x0000_0002;
const ATTR_PERMISSIONS: u32 = 0x0000_0004;
const ATTR_ACMODTIME: u32 = 0x0000_0008;
const ATTR_EXTENDED: u32 = 0x8000_0000;

const OPEN_WRITE: u32 = 0x0000_0002;
const OPEN_CREATE: u32 = 0x0000_0008;
const OPEN_EXCLUSIVE: u32 = 0x0000_0020;

/// The status codes of protocol version 3.
pub mod status {
    pub const OK: u32 = 0;
    pub const EOF: u32 = 1;
    pub const NO_SUCH_FILE: u32 = 2;
    pub const PERMISSION_DENIED: u32 = 3;
    /// The only code version 3 has for most errors, "already exists" and
    /// "no space left" among them.
    pub const FAILURE: u32 = 4;
}

/// A reply the stream could not be read as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireError(String);

impl WireError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for WireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WireError {}

/// The metadata Clift reads out of an attribute block. The rest is parsed so
/// that the block's end can be found, and then dropped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub permissions: Option<u32>,
    /// Seconds since the epoch.
    pub mtime: Option<u32>,
}

/// One entry of a `NAME` reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    /// The raw bytes the server sent. Not necessarily UTF-8.
    pub filename: Vec<u8>,
    pub attrs: Attrs,
}

/// A reply to a request, identified by the request's id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reply {
    Status { id: u32, code: u32, message: String },
    Handle { id: u32, handle: Vec<u8> },
    Name { id: u32, entries: Vec<NameEntry> },
    Attrs { id: u32, attrs: Attrs },
}

impl Reply {
    #[must_use]
    pub const fn id(&self) -> u32 {
        match self {
            Reply::Status { id, .. }
            | Reply::Handle { id, .. }
            | Reply::Name { id, .. }
            | Reply::Attrs { id, .. } => *id,
        }
    }
}

/// Reads the length at the front of a packet.
///
/// # Errors
/// Fails for an empty packet, which cannot even carry its type, and for one
/// larger than [`MAX_PACKET`].
pub fn packet_length(header: [u8; 4]) -> Result<usize, WireError> {
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 {
        return Err(WireError::new("the server sent an empty SFTP packet"));
    }
    if length > MAX_PACKET {
        return Err(WireError::new(format!(
            "the server sent an SFTP packet of {length} bytes, more than the {MAX_PACKET} \
             Clift accepts"
        )));
    }
    Ok(length)
}

/// Reads the server's answer to `INIT`, returning the version it speaks.
///
/// The extension pairs that follow are read to the end, so that a malformed
/// one is caught, and otherwise ignored: Clift uses no extension.
///
/// # Errors
/// Fails when the packet is not a well-formed `VERSION`.
pub fn decode_version(body: &[u8]) -> Result<u32, WireError> {
    let mut reader = Reader::new(body);
    let kind = reader.u8()?;
    if kind != VERSION_REPLY {
        return Err(WireError::new(format!(
            "the server answered the SFTP greeting with packet type {kind}"
        )));
    }
    let version = reader.u32()?;
    while !reader.is_empty() {
        reader.bytes()?;
        reader.bytes()?;
    }
    Ok(version)
}

/// Reads one reply packet, without its length.
///
/// # Errors
/// Fails when the packet is truncated, carries bytes past its end, or has a
/// type Clift never asks for.
pub fn decode_reply(body: &[u8]) -> Result<Reply, WireError> {
    let mut reader = Reader::new(body);
    let kind = reader.u8()?;
    let id = reader.u32()?;
    let reply = match kind {
        STATUS => {
            let code = reader.u32()?;
            // The message and its language tag are both required by the
            // draft, and some servers leave them out anyway. Absent is
            // accepted; present and malformed is not.
            let message = if reader.is_empty() {
                String::new()
            } else {
                String::from_utf8_lossy(reader.bytes()?).into_owned()
            };
            if !reader.is_empty() {
                reader.bytes()?;
            }
            Reply::Status { id, code, message }
        }
        HANDLE => Reply::Handle {
            id,
            handle: reader.bytes()?.to_vec(),
        },
        NAME => {
            let count = reader.u32()?;
            // Each entry is at least twelve bytes, so a count the packet cannot
            // hold is refused before anything is allocated for it.
            if count as usize > reader.remaining() / 12 {
                return Err(WireError::new(format!(
                    "the server claimed {count} names in a packet too small to hold them"
                )));
            }
            let mut entries = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let filename = reader.bytes()?.to_vec();
                let _longname = reader.bytes()?;
                let attrs = reader.attrs()?;
                entries.push(NameEntry { filename, attrs });
            }
            Reply::Name { id, entries }
        }
        ATTRS => Reply::Attrs {
            id,
            attrs: reader.attrs()?,
        },
        other => {
            return Err(WireError::new(format!(
                "the server sent SFTP packet type {other}, which Clift never asks for"
            )));
        }
    };
    if !reader.is_empty() {
        return Err(WireError::new(format!(
            "the server's SFTP reply of type {kind} carried {} bytes past its end",
            reader.remaining()
        )));
    }
    Ok(reply)
}

/// The greeting. It has no request id: it is the one packet that is not a
/// request.
#[must_use]
pub fn init() -> Vec<u8> {
    let mut packet = Packet::new(INIT);
    packet.u32(VERSION);
    packet.finish()
}

#[must_use]
pub fn realpath(id: u32, path: &str) -> Vec<u8> {
    Packet::with_id(REALPATH, id)
        .string(path.as_bytes())
        .finish()
}

/// Metadata for a path, without following a symbolic link at the end of it.
#[must_use]
pub fn lstat(id: u32, path: &str) -> Vec<u8> {
    Packet::with_id(LSTAT, id).string(path.as_bytes()).finish()
}

#[must_use]
pub fn fstat(id: u32, handle: &[u8]) -> Vec<u8> {
    Packet::with_id(FSTAT, id).string(handle).finish()
}

/// Creates a directory, asking for `mode` at the same time.
///
/// The server applies its umask to what is asked for, so the result can only
/// be stricter than `mode`, never looser. Whether a server honours the request
/// at all is not guaranteed, which is why the caller reads the mode back.
#[must_use]
pub fn mkdir(id: u32, path: &str, mode: u32) -> Vec<u8> {
    Packet::with_id(MKDIR, id)
        .string(path.as_bytes())
        .permissions(mode)
        .finish()
}

#[must_use]
pub fn setstat_mode(id: u32, path: &str, mode: u32) -> Vec<u8> {
    Packet::with_id(SETSTAT, id)
        .string(path.as_bytes())
        .permissions(mode)
        .finish()
}

#[must_use]
pub fn fsetstat_mode(id: u32, handle: &[u8], mode: u32) -> Vec<u8> {
    Packet::with_id(FSETSTAT, id)
        .string(handle)
        .permissions(mode)
        .finish()
}

/// Opens a file for writing that must not exist yet.
///
/// Exclusive creation is what makes the name random rather than merely
/// unlikely to collide: if something is already there, the server refuses
/// instead of writing into it.
#[must_use]
pub fn open_exclusive(id: u32, path: &str, mode: u32) -> Vec<u8> {
    let mut packet = Packet::with_id(OPEN, id);
    packet.bytes_field(path.as_bytes());
    packet.u32(OPEN_WRITE | OPEN_CREATE | OPEN_EXCLUSIVE);
    packet.permissions(mode).finish()
}

#[must_use]
pub fn write(id: u32, handle: &[u8], offset: u64, data: &[u8]) -> Vec<u8> {
    let mut packet = Packet::with_id(WRITE, id);
    packet.bytes_field(handle);
    packet.u64(offset);
    packet.string(data).finish()
}

#[must_use]
pub fn close(id: u32, handle: &[u8]) -> Vec<u8> {
    Packet::with_id(CLOSE, id).string(handle).finish()
}

#[must_use]
pub fn opendir(id: u32, path: &str) -> Vec<u8> {
    Packet::with_id(OPENDIR, id)
        .string(path.as_bytes())
        .finish()
}

#[must_use]
pub fn readdir(id: u32, handle: &[u8]) -> Vec<u8> {
    Packet::with_id(READDIR, id).string(handle).finish()
}

/// Removes a file or a symbolic link. A link is unlinked, never followed.
#[must_use]
pub fn remove(id: u32, path: &str) -> Vec<u8> {
    Packet::with_id(REMOVE, id).string(path.as_bytes()).finish()
}

#[must_use]
pub fn rmdir(id: u32, path: &str) -> Vec<u8> {
    Packet::with_id(RMDIR, id).string(path.as_bytes()).finish()
}

/// Renames without replacing: version 3's `RENAME` fails when the new name is
/// already taken, which is the behaviour Clift wants.
#[must_use]
pub fn rename(id: u32, from: &str, to: &str) -> Vec<u8> {
    let mut packet = Packet::with_id(RENAME, id);
    packet.bytes_field(from.as_bytes());
    packet.string(to.as_bytes()).finish()
}

/// A packet under construction, length left for last.
struct Packet {
    body: Vec<u8>,
}

impl Packet {
    fn new(kind: u8) -> Self {
        Self { body: vec![kind] }
    }

    fn with_id(kind: u8, id: u32) -> Self {
        let mut packet = Self::new(kind);
        packet.u32(id);
        packet
    }

    fn u32(&mut self, value: u32) {
        self.body.extend_from_slice(&value.to_be_bytes());
    }

    fn u64(&mut self, value: u64) {
        self.body.extend_from_slice(&value.to_be_bytes());
    }

    fn bytes_field(&mut self, value: &[u8]) {
        // Every value passed here is a path, a handle the server issued, or at
        // most one write chunk, all far below four gigabytes.
        self.u32(value.len() as u32);
        self.body.extend_from_slice(value);
    }

    fn string(mut self, value: &[u8]) -> Self {
        self.bytes_field(value);
        self
    }

    fn permissions(mut self, mode: u32) -> Self {
        self.u32(ATTR_PERMISSIONS);
        self.u32(mode);
        self
    }

    fn finish(self) -> Vec<u8> {
        let mut packet = Vec::with_capacity(self.body.len() + 4);
        packet.extend_from_slice(&(self.body.len() as u32).to_be_bytes());
        packet.extend_from_slice(&self.body);
        packet
    }
}

/// A bounds-checked cursor over one packet.
struct Reader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    const fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    const fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    const fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], WireError> {
        let end = self
            .position
            .checked_add(count)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| {
                WireError::new(format!(
                    "the server's SFTP reply ended {} bytes early",
                    count - self.remaining()
                ))
            })?;
        let slice = &self.data[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, WireError> {
        let mut bytes = [0_u8; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_be_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, WireError> {
        let mut bytes = [0_u8; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_be_bytes(bytes))
    }

    fn bytes(&mut self) -> Result<&'a [u8], WireError> {
        let length = self.u32()? as usize;
        self.take(length)
    }

    fn attrs(&mut self) -> Result<Attrs, WireError> {
        let flags = self.u32()?;
        let mut attrs = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            attrs.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            self.u32()?;
            self.u32()?;
        }
        if flags & ATTR_PERMISSIONS != 0 {
            attrs.permissions = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            self.u32()?;
            attrs.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            let count = self.u32()?;
            for _ in 0..count {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(attrs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds reply bodies the way a server would, for the decoder to read.
    fn body(kind: u8, parts: &[&[u8]]) -> Vec<u8> {
        let mut body = vec![kind];
        for part in parts {
            body.extend_from_slice(part);
        }
        body
    }

    fn be32(value: u32) -> Vec<u8> {
        value.to_be_bytes().to_vec()
    }

    fn string(value: &[u8]) -> Vec<u8> {
        let mut out = be32(value.len() as u32);
        out.extend_from_slice(value);
        out
    }

    /// A full attribute block, the shape OpenSSH's server sends for `lstat`.
    fn full_attrs(size: u64, mode: u32, mtime: u32) -> Vec<u8> {
        let mut out = be32(ATTR_SIZE | ATTR_UIDGID | ATTR_PERMISSIONS | ATTR_ACMODTIME);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend(be32(1000));
        out.extend(be32(1000));
        out.extend(be32(mode));
        out.extend(be32(mtime - 5));
        out.extend(be32(mtime));
        out
    }

    /// The same bytes a real Windows `ssh.exe` carried to a real server and
    /// back, in the diagnostic that decided this design.
    #[test]
    fn the_greeting_and_a_realpath_are_the_bytes_a_real_server_answered() {
        assert_eq!(init(), vec![0, 0, 0, 5, 1, 0, 0, 0, 3]);
        assert_eq!(
            realpath(1, "."),
            vec![0, 0, 0, 10, 16, 0, 0, 0, 1, 0, 0, 0, 1, 0x2e]
        );
    }

    #[test]
    fn a_path_travels_as_its_own_bytes_with_nothing_quoted() {
        let awkward = "/home/dev/it's \"a\" back\\slash *?[x] $HOME `id` 截图";
        let packet = lstat(7, awkward);
        let length = packet_length([packet[0], packet[1], packet[2], packet[3]]).unwrap();
        assert_eq!(length, packet.len() - 4);
        assert_eq!(packet[4], LSTAT);
        assert_eq!(&packet[5..9], &7_u32.to_be_bytes());
        assert_eq!(&packet[9..13], &(awkward.len() as u32).to_be_bytes());
        assert_eq!(&packet[13..], awkward.as_bytes());
    }

    #[test]
    fn a_new_file_is_opened_exclusively_with_its_mode_attached() {
        let packet = open_exclusive(3, "/a", 0o600);
        let mut expected = vec![OPEN];
        expected.extend(be32(3));
        expected.extend(string(b"/a"));
        expected.extend(be32(OPEN_WRITE | OPEN_CREATE | OPEN_EXCLUSIVE));
        expected.extend(be32(ATTR_PERMISSIONS));
        expected.extend(be32(0o600));
        assert_eq!(&packet[4..], expected.as_slice());
    }

    #[test]
    fn a_write_carries_its_offset_and_its_bytes() {
        let packet = write(9, b"h1", 1 << 33, b"xyz");
        let mut expected = vec![WRITE];
        expected.extend(be32(9));
        expected.extend(string(b"h1"));
        expected.extend_from_slice(&(1_u64 << 33).to_be_bytes());
        expected.extend(string(b"xyz"));
        assert_eq!(&packet[4..], expected.as_slice());
    }

    #[test]
    fn a_status_is_read_with_or_without_its_optional_tail() {
        let full = body(
            STATUS,
            &[
                &be32(4),
                &be32(status::NO_SUCH_FILE),
                &string(b"No such file"),
                &string(b""),
            ],
        );
        assert_eq!(
            decode_reply(&full).unwrap(),
            Reply::Status {
                id: 4,
                code: status::NO_SUCH_FILE,
                message: "No such file".to_string()
            }
        );
        let bare = body(STATUS, &[&be32(4), &be32(status::OK)]);
        assert_eq!(
            decode_reply(&bare).unwrap(),
            Reply::Status {
                id: 4,
                code: status::OK,
                message: String::new()
            }
        );
    }

    #[test]
    fn a_name_reply_keeps_raw_names_and_their_metadata() {
        let reply = body(
            NAME,
            &[
                &be32(2),
                &be32(2),
                &string(b"shot 1.png"),
                &string(b"-rw------- 1 dev dev 10 Sep 14 12:00 shot 1.png"),
                &full_attrs(10, 0o100_600, 1_788_000_000),
                &string(b"\xff\xfe"),
                &string(b""),
                &be32(ATTR_PERMISSIONS),
                &be32(0o040_700),
            ],
        );
        let Reply::Name { id, entries } = decode_reply(&reply).unwrap() else {
            panic!("not a name reply");
        };
        assert_eq!(id, 2);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].filename, b"shot 1.png");
        assert_eq!(
            entries[0].attrs,
            Attrs {
                size: Some(10),
                permissions: Some(0o100_600),
                mtime: Some(1_788_000_000)
            }
        );
        assert_eq!(entries[1].filename, b"\xff\xfe", "not UTF-8, kept as bytes");
        assert_eq!(entries[1].attrs.permissions, Some(0o040_700));
        assert_eq!(entries[1].attrs.size, None);
    }

    #[test]
    fn extended_attributes_are_stepped_over_to_find_the_end() {
        let mut attrs = be32(ATTR_PERMISSIONS | ATTR_EXTENDED);
        attrs.extend(be32(0o040_700));
        attrs.extend(be32(1));
        attrs.extend(string(b"acl@example"));
        attrs.extend(string(b"whatever"));
        let reply = body(ATTRS, &[&be32(5), &attrs]);
        assert_eq!(
            decode_reply(&reply).unwrap(),
            Reply::Attrs {
                id: 5,
                attrs: Attrs {
                    size: None,
                    permissions: Some(0o040_700),
                    mtime: None
                }
            }
        );
    }

    /// Every prefix of a valid reply is refused, not read as something shorter.
    #[test]
    fn a_truncated_reply_is_refused_at_every_length() {
        let replies = [
            body(HANDLE, &[&be32(1), &string(b"handle-bytes")]),
            body(ATTRS, &[&be32(1), &full_attrs(9, 0o100_600, 1_788_000_000)]),
            body(
                NAME,
                &[
                    &be32(1),
                    &be32(1),
                    &string(b"a"),
                    &string(b"long a"),
                    &full_attrs(1, 0o100_600, 1_788_000_000),
                ],
            ),
        ];
        for reply in replies {
            assert!(decode_reply(&reply).is_ok());
            for cut in 0..reply.len() {
                assert!(
                    decode_reply(&reply[..cut]).is_err(),
                    "a reply cut to {cut} of {} bytes was accepted",
                    reply.len()
                );
            }
        }
    }

    #[test]
    fn bytes_past_the_end_of_a_reply_are_refused() {
        let mut reply = body(HANDLE, &[&be32(1), &string(b"h")]);
        reply.push(0);
        assert!(decode_reply(&reply).is_err());
    }

    #[test]
    fn a_name_count_the_packet_cannot_hold_is_refused_before_allocating() {
        let reply = body(NAME, &[&be32(1), &be32(u32::MAX)]);
        let error = decode_reply(&reply).unwrap_err();
        assert!(error.to_string().contains("too small"), "{error}");
    }

    #[test]
    fn a_string_longer_than_its_packet_is_refused() {
        let reply = body(HANDLE, &[&be32(1), &be32(u32::MAX), b"h"]);
        assert!(decode_reply(&reply).is_err());
    }

    #[test]
    fn a_packet_type_clift_never_asks_for_is_refused() {
        // DATA (103) answers a READ, and Clift never reads a remote file.
        let reply = body(103, &[&be32(1), &string(b"bytes")]);
        assert!(decode_reply(&reply).is_err());
    }

    #[test]
    fn a_packet_length_must_be_non_zero_and_within_the_limit() {
        assert!(packet_length(0_u32.to_be_bytes()).is_err());
        assert_eq!(packet_length(1_u32.to_be_bytes()).unwrap(), 1);
        assert_eq!(
            packet_length((MAX_PACKET as u32).to_be_bytes()).unwrap(),
            MAX_PACKET
        );
        assert!(packet_length((MAX_PACKET as u32 + 1).to_be_bytes()).is_err());
        assert!(packet_length(u32::MAX.to_be_bytes()).is_err());
    }

    #[test]
    fn the_version_reply_is_read_through_its_extensions() {
        let reply = body(
            VERSION_REPLY,
            &[
                &be32(3),
                &string(b"posix-rename@openssh.com"),
                &string(b"1"),
                &string(b"limits@openssh.com"),
                &string(b"1"),
            ],
        );
        assert_eq!(decode_version(&reply).unwrap(), 3);
        assert!(decode_version(&reply[..reply.len() - 1]).is_err());
        assert!(decode_version(&body(STATUS, &[&be32(3)])).is_err());
    }
}
