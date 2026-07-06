//! GTA San Andreas map geometry: the physical world the game walks on.
//!
//! This crate parses the *collision* half of the SA map — the meshes the engine uses for physics,
//! independent of the visual models — so a headless client can query real ground height and obstacles
//! instead of teleporting along straight lines.
//!
//! Two byte-exact readers, both verified against golden vectors:
//! - [`img`] — the `VER2` IMG archive (`gta3.img`, SA-MP's `SAMPCOL.img`): a flat table of named
//!   entries at 2048-byte sector offsets. Extract a `.col` blob by name.
//! - [`col`] — the `COL2`/`COL3` collision format: per-model bounding volumes + a triangle mesh
//!   (int16/128 fixed-point vertices, u16 face indices). One `.col` blob holds many models.
//!
//! The placement layer (IPL instances → world-space triangle soup → `ground_z(x, y)` raycast) builds
//! on these; it lives in later modules so the readers can be validated on their own first.

pub mod col;
pub mod ide;
pub mod img;
pub mod ipl;
pub mod load;
pub mod world;

pub use col::{ColBox, ColModel, ColSphere, ColTriangle};
pub use img::ImgArchive;
pub use ipl::Instance;
pub use world::Mesh;

/// A 3D point / vector in SA world units (metres). Right-handed, Z up — the game's own axes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
}

/// Errors from parsing an IMG archive or a COL model.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unexpected end of data: need {need} bytes at offset {at}, have {have}")]
    Truncated { at: usize, need: usize, have: usize },
    #[error("bad magic: expected {expected:?}, found {found:?}")]
    BadMagic {
        expected: &'static str,
        found: [u8; 4],
    },
    #[error("entry {0:?} not found in archive")]
    NotFound(String),
    #[error("unsupported collision version {0:?} (only COL2/COL3 are supported)")]
    UnsupportedCol([u8; 4]),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Little-endian byte reader over a borrowed slice, with bounds-checked primitives. Every multi-byte
/// field in both formats is little-endian, so a shared cursor keeps the parsers small and safe.
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    pub(crate) fn at(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    pub(crate) fn pos(&self) -> usize {
        self.pos
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).ok_or(Error::Truncated {
            at: self.pos,
            need: n,
            have: self.data.len(),
        })?;
        let slice = self.data.get(self.pos..end).ok_or(Error::Truncated {
            at: self.pos,
            need: n,
            have: self.data.len().saturating_sub(self.pos),
        })?;
        self.pos = end;
        Ok(slice)
    }

    pub(crate) fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16> {
        let b = self.take(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn i16(&mut self) -> Result<i16> {
        Ok(self.u16()? as i16)
    }

    pub(crate) fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn f32(&mut self) -> Result<f32> {
        let b = self.take(4)?;
        Ok(f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub(crate) fn vec3(&mut self) -> Result<Vec3> {
        Ok(Vec3::new(self.f32()?, self.f32()?, self.f32()?))
    }

    pub(crate) fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        self.take(n)
    }

    pub(crate) fn magic(&mut self) -> Result<[u8; 4]> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }
}

/// Trim a fixed-size, null-padded name field to a `String` (both formats pad names with `\0`).
pub(crate) fn fixed_name(raw: &[u8]) -> String {
    let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
    String::from_utf8_lossy(&raw[..end]).into_owned()
}
