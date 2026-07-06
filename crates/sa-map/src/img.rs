//! `VER2` IMG archive reader — the container both `gta3.img` and SA-MP's `SAMPCOL.img` use.
//!
//! Layout (little-endian):
//! ```text
//!   0   char[4]  "VER2"
//!   4   u32      entry count
//!   8   entry[]  32 bytes each:
//!                  u32   offset   (in 2048-byte sectors from the file start)
//!                  u16   streaming size (sectors)   ── the size we use
//!                  u16   size in archive (sectors)  ── usually 0
//!                  char[24] name (null-padded)
//! ```
//! Entry data lives at `offset * 2048` for `streaming_size * 2048` bytes (sector-padded — the real
//! payload, e.g. a `.col`, carries its own length, so reading the whole span is safe).

use crate::{fixed_name, Error, Reader, Result};

const SECTOR: usize = 2048;

/// One named blob in the archive, located by 2048-byte sector offset and length.
#[derive(Debug, Clone)]
pub struct ImgEntry {
    pub name: String,
    /// Byte offset into the archive (`sector * 2048`).
    pub offset: usize,
    /// Byte length (`sectors * 2048`), clamped to the archive on read.
    pub size: usize,
}

/// A parsed `VER2` IMG archive owning its bytes, with a directory of entries.
pub struct ImgArchive {
    data: Vec<u8>,
    entries: Vec<ImgEntry>,
}

impl ImgArchive {
    /// Read and parse an archive from disk.
    pub fn open<P: AsRef<std::path::Path>>(path: P) -> Result<Self> {
        Self::parse(std::fs::read(path)?)
    }

    /// Parse an archive from an owned byte buffer.
    pub fn parse(data: Vec<u8>) -> Result<Self> {
        let mut r = Reader::new(&data);
        let magic = r.magic()?;
        if &magic != b"VER2" {
            return Err(Error::BadMagic {
                expected: "VER2",
                found: magic,
            });
        }
        let count = r.u32()? as usize;
        let mut entries = Vec::with_capacity(count);
        for _ in 0..count {
            let offset = r.u32()? as usize * SECTOR;
            let streaming = r.u16()? as usize; // sectors — the size the streamer reads
            let _in_archive = r.u16()?; // usually 0; streaming size is authoritative
            let name = fixed_name(r.bytes(24)?);
            entries.push(ImgEntry {
                name,
                offset,
                size: streaming * SECTOR,
            });
        }
        Ok(Self { data, entries })
    }

    /// All directory entries, in archive order.
    pub fn entries(&self) -> &[ImgEntry] {
        &self.entries
    }

    /// The raw bytes of an entry (sector-padded), clamped to the archive.
    pub fn read(&self, entry: &ImgEntry) -> &[u8] {
        let start = entry.offset.min(self.data.len());
        let end = entry.offset.saturating_add(entry.size).min(self.data.len());
        &self.data[start..end]
    }

    /// Find an entry by name, case-insensitively (SA filenames are case-insensitive), and return its
    /// bytes. `None` if no entry matches.
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.name.eq_ignore_ascii_case(name))?;
        Some(self.read(entry))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal but byte-exact `VER2` archive with two entries whose payloads are distinct
    /// sector-aligned blobs, then assert the directory and payload lookups round-trip.
    fn sample_archive() -> Vec<u8> {
        let mut buf = Vec::new();
        // Header: "VER2", entry count = 2.
        buf.extend_from_slice(b"VER2");
        buf.extend_from_slice(&2u32.to_le_bytes());

        // Two 32-byte directory entries. Data starts after the header+directory, sector-aligned:
        // header 8 + 2*32 = 72 bytes → first data sector is sector 1 (2048).
        let mk_entry = |offset_sec: u32, size_sec: u16, name: &str| {
            let mut e = Vec::new();
            e.extend_from_slice(&offset_sec.to_le_bytes());
            e.extend_from_slice(&size_sec.to_le_bytes());
            e.extend_from_slice(&0u16.to_le_bytes());
            let mut nm = [0u8; 24];
            nm[..name.len()].copy_from_slice(name.as_bytes());
            e.extend_from_slice(&nm);
            e
        };
        buf.extend_from_slice(&mk_entry(1, 1, "alpha.col"));
        buf.extend_from_slice(&mk_entry(2, 1, "beta.col"));

        // Pad to sector 1, then the two payloads (each one full sector).
        buf.resize(SECTOR, 0);
        let mut alpha = vec![0u8; SECTOR];
        alpha[..5].copy_from_slice(b"ALPHA");
        buf.extend_from_slice(&alpha);
        let mut beta = vec![0u8; SECTOR];
        beta[..4].copy_from_slice(b"BETA");
        buf.extend_from_slice(&beta);
        buf
    }

    #[test]
    fn parses_directory_and_reads_payloads() {
        let archive = ImgArchive::parse(sample_archive()).expect("parse");
        assert_eq!(archive.entries().len(), 2);
        assert_eq!(archive.entries()[0].name, "alpha.col");
        assert_eq!(archive.entries()[1].name, "beta.col");

        // Case-insensitive lookup, correct payload per entry.
        assert!(archive.get("ALPHA.COL").unwrap().starts_with(b"ALPHA"));
        assert!(archive.get("beta.col").unwrap().starts_with(b"BETA"));
        assert!(archive.get("missing.col").is_none());
    }

    #[test]
    fn rejects_bad_magic() {
        let mut bytes = sample_archive();
        bytes[0] = b'X';
        assert!(matches!(
            ImgArchive::parse(bytes),
            Err(Error::BadMagic {
                expected: "VER2",
                ..
            })
        ));
    }
}
