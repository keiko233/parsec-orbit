//! The one supported Mach-O edit: inserting one `LC_LOAD_DYLIB` command.
//!
//! The measured fixture (`scripts/macos-stage-bootstrap-audit.py`) proved the
//! arm64 vendor stub carries zeroed header padding between its load commands
//! and the first file-backed section. The installer writes the loader
//! dependency and the displaced trailing command (the code-signature command)
//! into that padding only; the file-backed sections keep their offsets and
//! bytes, and the file length does not change. Everything else refuses:
//! a foreign magic/CPU/file type, an already patched image, a missing or
//! non-trailing code-signature command, nonzero padding, or insufficient
//! padding for the command.
//!
//! The parser is portable and unit-tested on every host; only the caller
//! reads and writes files.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::digest::hex;

/// 64-bit Mach-O magic, little-endian arm64 files.
pub const MH_MAGIC_64: u32 = 0xFEED_FACF;
/// `CPU_TYPE_ARM64`.
pub const CPU_TYPE_ARM64: u32 = 0x0100_000C;
/// `MH_EXECUTE`.
pub const MH_EXECUTE: u32 = 0x2;

const LC_SEGMENT_64: u32 = 0x19;
const LC_LOAD_DYLIB: u32 = 0x0C;
const LC_LOAD_WEAK_DYLIB: u32 = 0x8000_0018;
const LC_CODE_SIGNATURE: u32 = 0x1D;

const HEADER_BYTES: usize = 32;
const SEGMENT_COMMAND_BYTES: usize = 72;
const SECTION_BYTES: usize = 80;
const DYLIB_COMMAND_BYTES: usize = 24;

/// One file-backed section digest taken before mutation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionDigest {
    pub offset: u64,
    pub size: u64,
    pub sha256: String,
}

/// What [`patch`] changed, for the transaction record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatchReport {
    pub commands_before: u32,
    pub dependency_bytes: u32,
    pub first_section_offset: u64,
    pub file_backed_sections: Vec<SectionDigest>,
}

struct Parsed {
    ncmds: u32,
    sizeofcmds: u32,
    /// End of the load-command region (`32 + sizeofcmds`).
    end: usize,
    /// Lowest file offset of a file-backed section, or the file length.
    first_section: usize,
    sections: Vec<SectionDigest>,
    signature: Option<(usize, u32)>,
    dependency_present: bool,
}

fn u32_at(image: &[u8], offset: usize) -> Result<u32, String> {
    let bytes = image
        .get(offset..offset + 4)
        .ok_or_else(|| format!("truncated Mach-O at byte {offset}"))?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn u64_at(image: &[u8], offset: usize) -> Result<u64, String> {
    let bytes = image
        .get(offset..offset + 8)
        .ok_or_else(|| format!("truncated Mach-O at byte {offset}"))?;
    let mut array = [0_u8; 8];
    array.copy_from_slice(bytes);
    Ok(u64::from_le_bytes(array))
}

fn parse(image: &[u8], dependency: &str) -> Result<Parsed, String> {
    if image.len() < HEADER_BYTES {
        return Err("the stub is shorter than a Mach-O header".into());
    }
    if u32_at(image, 0)? != MH_MAGIC_64 {
        return Err("the stub is not a 64-bit little-endian Mach-O image".into());
    }
    if u32_at(image, 4)? != CPU_TYPE_ARM64 {
        return Err("the stub is not an arm64 image".into());
    }
    if u32_at(image, 12)? != MH_EXECUTE {
        return Err("the stub is not a Mach-O executable".into());
    }

    let ncmds = u32_at(image, 16)?;
    let sizeofcmds = u32_at(image, 20)?;
    let end = HEADER_BYTES
        .checked_add(sizeofcmds as usize)
        .ok_or_else(|| "the load-command size overflows".to_string())?;
    if end > image.len() {
        return Err("the load commands extend past the end of the stub".into());
    }

    let mut cursor = HEADER_BYTES;
    let mut first_section = image.len();
    let mut sections = Vec::new();
    let mut signature = None;
    let mut dependency_present = false;

    for _ in 0..ncmds {
        let command = u32_at(image, cursor)?;
        let count = u32_at(image, cursor + 4)? as usize;
        if count < 8 || !count.is_multiple_of(8) || cursor + count > end {
            return Err(format!(
                "invalid load command at byte {cursor}: size {count} does not fit"
            ));
        }
        match command {
            LC_SEGMENT_64 => {
                let nsects = u32_at(image, cursor + 64)? as usize;
                if count != SEGMENT_COMMAND_BYTES + nsects * SECTION_BYTES {
                    return Err(format!(
                        "the segment command at byte {cursor} does not match its section count"
                    ));
                }
                for index in 0..nsects {
                    let section = cursor + SEGMENT_COMMAND_BYTES + index * SECTION_BYTES;
                    let size = u64_at(image, section + 40)?;
                    let offset = u32_at(image, section + 48)? as u64;
                    if offset == 0 || size == 0 {
                        continue;
                    }
                    let file_end = offset
                        .checked_add(size)
                        .ok_or_else(|| "a section extent overflows".to_string())?;
                    let (start, stop) = (offset as usize, file_end as usize);
                    if file_end > image.len() as u64 {
                        return Err(format!(
                            "the file-backed section at byte {offset} extends past the stub"
                        ));
                    }
                    first_section = first_section.min(start);
                    sections.push(SectionDigest {
                        offset,
                        size,
                        sha256: hex(&Sha256::digest(&image[start..stop])),
                    });
                }
            }
            LC_LOAD_DYLIB | LC_LOAD_WEAK_DYLIB => {
                let name_offset = u32_at(image, cursor + 8)? as usize;
                if name_offset < DYLIB_COMMAND_BYTES || name_offset >= count {
                    return Err(format!(
                        "invalid dylib name offset in the command at byte {cursor}"
                    ));
                }
                let name_bytes = &image[cursor + name_offset..cursor + count];
                let terminate = name_bytes
                    .iter()
                    .position(|byte| *byte == 0)
                    .ok_or_else(|| format!("unterminated dylib name at byte {cursor}"))?;
                if &name_bytes[..terminate] == dependency.as_bytes() {
                    dependency_present = true;
                }
            }
            LC_CODE_SIGNATURE => {
                if signature.is_some() || cursor + count != end {
                    return Err("the code-signature command must be unique and last".to_string());
                }
                signature = Some((cursor, count as u32));
            }
            _ => {}
        }
        cursor += count;
    }
    if cursor != end {
        return Err(format!(
            "the load commands cover {cursor} bytes but the header records {end}"
        ));
    }

    Ok(Parsed {
        ncmds,
        sizeofcmds,
        end,
        first_section,
        sections,
        signature,
        dependency_present,
    })
}

/// Inserts one `LC_LOAD_DYLIB` for `dependency` into the listed header
/// padding. Returns the patched image and the pre-patch section digests.
pub fn patch(image: &[u8], dependency: &str) -> Result<(Vec<u8>, PatchReport), String> {
    if dependency.contains('\0') || dependency.is_empty() {
        return Err("the loader dependency must be a nonempty path without NUL".into());
    }
    let parsed = parse(image, dependency)?;
    if parsed.dependency_present {
        return Err("refusing an image that already names the loader dependency".into());
    }
    let Some((signature_offset, _)) = parsed.signature else {
        return Err("the stub has no code-signature command".into());
    };

    let name = dependency.as_bytes();
    let command_bytes = DYLIB_COMMAND_BYTES + name.len() + 1;
    let count = (command_bytes + 7) & !7;
    if parsed.end + count > parsed.first_section {
        return Err(format!(
            "the stub has only {} bytes of header padding before its first \
             file-backed section; the {count}-byte load command does not fit",
            parsed.first_section - parsed.end
        ));
    }
    if image[parsed.end..parsed.first_section]
        .iter()
        .any(|byte| *byte != 0)
    {
        return Err("the header padding is not zeroed; refusing to overwrite it".into());
    }

    let mut addition = vec![0_u8; count];
    addition[0..4].copy_from_slice(&LC_LOAD_DYLIB.to_le_bytes());
    addition[4..8].copy_from_slice(&(count as u32).to_le_bytes());
    addition[8..12].copy_from_slice(&(DYLIB_COMMAND_BYTES as u32).to_le_bytes());
    addition[DYLIB_COMMAND_BYTES..DYLIB_COMMAND_BYTES + name.len()].copy_from_slice(name);

    let mut patched = image.to_vec();
    patched[signature_offset..signature_offset + count].copy_from_slice(&addition);
    patched[signature_offset + count..parsed.end + count]
        .copy_from_slice(&image[signature_offset..parsed.end]);
    patched[16..20].copy_from_slice(&(parsed.ncmds + 1).to_le_bytes());
    patched[20..24].copy_from_slice(&(parsed.sizeofcmds + count as u32).to_le_bytes());

    let after = parse(&patched, dependency)?;
    if !after.dependency_present || after.ncmds != parsed.ncmds + 1 {
        return Err("the patched image does not parse with the inserted command".into());
    }

    Ok((
        patched,
        PatchReport {
            commands_before: parsed.ncmds,
            dependency_bytes: count as u32,
            first_section_offset: parsed.first_section as u64,
            file_backed_sections: parsed.sections,
        },
    ))
}

/// Requires that `image` carries `dependency` and still parses cleanly.
pub fn verify_dependency(image: &[u8], dependency: &str) -> Result<(), String> {
    let parsed = parse(image, dependency)?;
    if !parsed.dependency_present {
        return Err("the staged stub does not name the loader dependency".into());
    }
    Ok(())
}

/// Re-measures the recorded file-backed sections of a signed image.
pub fn sections_preserved(image: &[u8], sections: &[SectionDigest]) -> Result<(), String> {
    for section in sections {
        let start = section.offset as usize;
        let stop = start + section.size as usize;
        let bytes = image.get(start..stop).ok_or_else(|| {
            format!(
                "the file-backed section at byte {} no longer fits the stub",
                section.offset
            )
        })?;
        if hex(&Sha256::digest(bytes)) != section.sha256 {
            return Err(format!(
                "the file-backed section at byte {} changed during patch or signing",
                section.offset
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEPENDENCY: &str = "@executable_path/../Frameworks/libparsec_orbit_loader.dylib";

    fn put32(image: &mut [u8], offset: usize, value: u32) {
        image[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn put64(image: &mut [u8], offset: usize, value: u64) {
        image[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    /// A minimal valid image: header, one `__TEXT` segment with one section,
    /// then the trailing code-signature command and `padding` zero bytes
    /// before the section.
    fn synthetic(padding: usize) -> Vec<u8> {
        let segment_bytes = SEGMENT_COMMAND_BYTES + SECTION_BYTES;
        let ncmds = 2_u32;
        let sizeofcmds = (segment_bytes + 16) as u32;
        let end = HEADER_BYTES + sizeofcmds as usize;
        let first_section = end + padding;
        let section_size = 0x40_usize;
        let signature_bytes = 32_usize;
        let mut image = vec![0_u8; first_section + section_size + signature_bytes];

        put32(&mut image, 0, MH_MAGIC_64);
        put32(&mut image, 4, CPU_TYPE_ARM64);
        put32(&mut image, 12, MH_EXECUTE);
        put32(&mut image, 16, ncmds);
        put32(&mut image, 20, sizeofcmds);

        put32(&mut image, 32, LC_SEGMENT_64);
        put32(&mut image, 36, segment_bytes as u32);
        image[40..46].copy_from_slice(b"__TEXT");
        put64(&mut image, 32 + 48, (first_section + section_size) as u64);
        put32(&mut image, 32 + 64, 1);

        let section = 32 + SEGMENT_COMMAND_BYTES;
        image[section..section + 6].copy_from_slice(b"__text");
        image[section + 16..section + 22].copy_from_slice(b"__TEXT");
        put64(&mut image, section + 40, section_size as u64);
        put32(&mut image, section + 48, first_section as u32);

        let signature = end - 16;
        put32(&mut image, signature, LC_CODE_SIGNATURE);
        put32(&mut image, signature + 4, 16);
        put32(
            &mut image,
            signature + 8,
            (first_section + section_size) as u32,
        );
        put32(&mut image, signature + 12, signature_bytes as u32);

        for byte in &mut image[first_section..first_section + section_size] {
            *byte = 0x90;
        }
        for byte in &mut image[first_section + section_size..] {
            *byte = 0xAB;
        }
        image
    }

    #[test]
    fn patch_inserts_the_dependency_and_keeps_the_sections() {
        let image = synthetic(0x200);
        let (patched, report) = patch(&image, DEPENDENCY).unwrap();
        assert_eq!(patched.len(), image.len());
        assert_eq!(report.commands_before, 2);
        assert_eq!(report.file_backed_sections.len(), 1);
        verify_dependency(&patched, DEPENDENCY).unwrap();
        sections_preserved(&patched, &report.file_backed_sections).unwrap();
        assert_eq!(u32_at(&patched, 16).unwrap(), 3);
        assert_eq!(
            u32_at(&patched, 20).unwrap(),
            u32_at(&image, 20).unwrap() + report.dependency_bytes
        );
    }

    #[test]
    fn patch_refuses_a_second_patch() {
        let image = synthetic(0x200);
        let (patched, _) = patch(&image, DEPENDENCY).unwrap();
        assert!(patch(&patched, DEPENDENCY)
            .unwrap_err()
            .contains("already names"));
    }

    #[test]
    fn patch_refuses_nonzero_padding_and_missing_room() {
        let mut dirty = synthetic(0x200);
        let end = HEADER_BYTES + u32_at(&dirty, 20).unwrap() as usize;
        dirty[end + 8] = 1;
        assert!(patch(&dirty, DEPENDENCY)
            .unwrap_err()
            .contains("padding is not zeroed"));
        assert!(patch(&synthetic(8), DEPENDENCY)
            .unwrap_err()
            .contains("does not fit"));
    }

    #[test]
    fn patch_refuses_a_signature_that_is_not_last_and_foreign_images() {
        let image = synthetic(0x200);
        let end = HEADER_BYTES + u32_at(&image, 20).unwrap() as usize;
        let signature = end - 16;
        // Duplicate the code-signature command directly after itself.
        let tail: Vec<u8> = image[signature..end].to_vec();
        let mut longer = Vec::new();
        longer.extend_from_slice(&image[..signature]);
        longer.extend_from_slice(&tail);
        longer.extend_from_slice(&tail);
        longer.extend_from_slice(&image[end..]);
        put32(&mut longer, 16, 3);
        put32(&mut longer, 20, u32_at(&image, 20).unwrap() + 16);
        assert!(patch(&longer, DEPENDENCY).unwrap_err().contains("last"));

        let mut foreign = synthetic(0x200);
        put32(&mut foreign, 4, 0x0100_0007);
        assert!(patch(&foreign, DEPENDENCY).unwrap_err().contains("arm64"));
    }

    #[test]
    fn sections_preserved_detects_a_changed_byte() {
        let image = synthetic(0x200);
        let (mut patched, report) = patch(&image, DEPENDENCY).unwrap();
        sections_preserved(&patched, &report.file_backed_sections).unwrap();
        let offset = report.file_backed_sections[0].offset as usize;
        patched[offset] ^= 0xFF;
        assert!(sections_preserved(&patched, &report.file_backed_sections).is_err());
    }
}
