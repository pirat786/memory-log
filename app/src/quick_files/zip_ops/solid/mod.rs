mod pack;
mod unpack;

pub use pack::{pack_solid, pack_solid_with_level, ULTRA_SOLID_ZSTD_LEVEL};
pub use unpack::{unpack_gz, unpack_solid, unpack_tar_gz};

pub(self) const SOLID_SMALL_FILE_MAX: u64 = 2 * 1024 * 1024;
