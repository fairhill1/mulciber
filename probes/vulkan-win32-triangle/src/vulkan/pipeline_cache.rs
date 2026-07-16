use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::vk;

use super::ProbeError;

const HEADER_SIZE: usize = 32;
const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct PipelineCacheIdentity {
    pub(super) vendor_id: u32,
    pub(super) device_id: u32,
    pub(super) uuid: [u8; 16],
}

pub(super) fn uuid_hex(identity: PipelineCacheIdentity) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut uuid = String::with_capacity(32);
    for byte in identity.uuid {
        uuid.push(char::from(HEX[usize::from(byte >> 4)]));
        uuid.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    uuid
}

pub(super) fn default_path(identity: PipelineCacheIdentity) -> PathBuf {
    let uuid = uuid_hex(identity);
    PathBuf::from("target").join(format!("mulciber-vulkan-pipeline-{uuid}.bin"))
}

pub(super) fn validate_header(bytes: &[u8], identity: PipelineCacheIdentity) -> Result<(), String> {
    if bytes.len() < HEADER_SIZE {
        return Err(format!(
            "truncated header: {} bytes, expected at least {HEADER_SIZE}",
            bytes.len()
        ));
    }
    let field = |offset| u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
    let header_size = usize::try_from(field(0)).expect("u32 fits usize");
    if header_size != HEADER_SIZE {
        return Err(format!(
            "header size {header_size} does not match {HEADER_SIZE}"
        ));
    }
    let header_version = field(4);
    if header_version != vk::VK_PIPELINE_CACHE_HEADER_VERSION_ONE as u32 {
        return Err(format!("unsupported header version {header_version}"));
    }
    if field(8) != identity.vendor_id {
        return Err("vendor ID does not match the selected adapter".into());
    }
    if field(12) != identity.device_id {
        return Err("device ID does not match the selected adapter".into());
    }
    if bytes[16..32] != identity.uuid {
        return Err("pipeline cache UUID does not match the selected adapter".into());
    }
    Ok(())
}

pub(super) fn replace_file_atomically(path: &Path, bytes: &[u8]) -> Result<(), ProbeError> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            ProbeError(format!(
                "could not create pipeline cache directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let file_name = path.file_name().ok_or_else(|| {
        ProbeError(format!(
            "pipeline cache path has no file name: {}",
            path.display()
        ))
    })?;
    let mut temporary_name = file_name.to_os_string();
    temporary_name.push(format!(".tmp-{}", std::process::id()));
    let temporary = path.with_file_name(temporary_name);
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temporary)
            .map_err(|error| {
                ProbeError(format!(
                    "could not open pipeline cache temporary file {}: {error}",
                    temporary.display()
                ))
            })?;
        file.write_all(bytes).map_err(|error| {
            ProbeError(format!(
                "could not write pipeline cache temporary file {}: {error}",
                temporary.display()
            ))
        })?;
        file.sync_all().map_err(|error| {
            ProbeError(format!(
                "could not flush pipeline cache temporary file {}: {error}",
                temporary.display()
            ))
        })?;
        drop(file);
        let temporary_wide = temporary
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        let path_wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        // SAFETY: Both paths are NUL-terminated UTF-16 strings and name sibling files.
        if unsafe {
            MoveFileExW(
                temporary_wide.as_ptr(),
                path_wide.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } == 0
        {
            return Err(ProbeError(format!(
                "could not atomically replace pipeline cache {}: {}",
                path.display(),
                std::io::Error::last_os_error()
            )));
        }
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity() -> PipelineCacheIdentity {
        PipelineCacheIdentity {
            vendor_id: 0x10de,
            device_id: 0x2489,
            uuid: *b"0123456789abcdef",
        }
    }

    fn header(identity: PipelineCacheIdentity) -> Vec<u8> {
        let mut bytes = vec![0_u8; HEADER_SIZE];
        bytes[0..4].copy_from_slice(
            &u32::try_from(HEADER_SIZE)
                .expect("header size fits u32")
                .to_le_bytes(),
        );
        bytes[4..8]
            .copy_from_slice(&(vk::VK_PIPELINE_CACHE_HEADER_VERSION_ONE as u32).to_le_bytes());
        bytes[8..12].copy_from_slice(&identity.vendor_id.to_le_bytes());
        bytes[12..16].copy_from_slice(&identity.device_id.to_le_bytes());
        bytes[16..32].copy_from_slice(&identity.uuid);
        bytes
    }

    #[test]
    fn header_requires_exact_device_identity() {
        let identity = identity();
        let bytes = header(identity);
        assert_eq!(validate_header(&bytes, identity), Ok(()));
        assert!(validate_header(&bytes[..31], identity).is_err());

        let mut wrong_vendor = bytes.clone();
        wrong_vendor[8] ^= 1;
        assert!(validate_header(&wrong_vendor, identity).is_err());

        let mut wrong_uuid = bytes;
        wrong_uuid[31] ^= 1;
        assert!(validate_header(&wrong_uuid, identity).is_err());
    }

    #[test]
    fn default_path_is_uuid_specific() {
        assert_eq!(
            default_path(identity()),
            PathBuf::from("target")
                .join("mulciber-vulkan-pipeline-30313233343536373839616263646566.bin")
        );
    }
}
