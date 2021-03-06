use byteorder::{ByteOrder, LittleEndian};
use failure::{Error, Fail};
use goblin::Object;
use log::{debug, trace, warn};
use std::ops::Range;

use super::super::{
    super::{arch::RVA, loader::Permissions, workspace::Workspace},
    Analyzer,
};
use std::collections::HashSet;

#[derive(Debug, Fail)]
pub enum RelocAnalyzerError {
    #[fail(display = "Invalid relocation type")]
    InvalidRelocType,
    #[fail(display = "Relocation target not found in image")]
    InvalidTargetAddress,
}

pub struct RelocAnalyzer {}

impl RelocAnalyzer {
    #[allow(clippy::new_without_default)]
    pub fn new() -> RelocAnalyzer {
        RelocAnalyzer {}
    }
}

// TODO: make this much faster
fn is_in_insn(ws: &Workspace, rva: RVA) -> bool {
    let start: usize = rva.into();
    // TODO: underflow
    // TODO: remove harded max insn length
    let end: usize = start - 0x10;

    for i in (start..end).rev() {
        let i = RVA::from(i);
        if let Some(meta) = ws.get_meta(i) {
            if !meta.is_insn() {
                continue;
            }

            if let Ok(len) = meta.get_insn_length() {
                if i + len > rva {
                    return true;
                }
            }
        }
    }

    false
}

fn is_ptr(ws: &Workspace, rva: RVA) -> bool {
    if let Ok(ptr) = ws.read_va(rva) {
        if let Some(ptr) = ws.rva(ptr) {
            return ws.probe(ptr, 1, Permissions::R);
        }
    }

    false
}

fn is_zero(ws: &Workspace, rva: RVA) -> bool {
    if let Ok(v) = ws.read_u32(rva) {
        return v == 0;
    }

    false
}

#[derive(Debug)]
pub enum RelocationType {
    ImageRelBasedAbsolute,
    ImageRelBasedHigh,
    ImageRelBasedLow,
    ImageRelBasedHighLow,
    ImageRelBasedHighAdj,

    // ImageRelBasedMIPS_JmpAddr,
    // ImageRelBasedARM_MOV32,
    // ImageRelBasedRiscV_High20,
    ImageRelArch1,

    ImageRelReserved,

    // ImageRelBasedTHUMB_MOV32,
    // ImageRelBasedRiscV_Low12I,
    ImageRelArch2,

    ImageRelBasedRiscVLow12S,
    ImageRelBasedMIPSJmpAddr16,
    ImageRelBasedDir64,
}

pub struct Reloc {
    pub typ:    RelocationType,
    pub offset: RVA,
}

fn parse_reloc(base: RVA, entry: u16) -> Result<Reloc, Error> {
    let reloc_type = (entry & 0b1111_0000_0000_0000) >> 12;
    let reloc_offset = entry & 0b0000_1111_1111_1111;

    // TODO: this should probably be on `RelocationType` as a `From` trait.
    let reloc_type = match reloc_type {
        0 => RelocationType::ImageRelBasedAbsolute,
        1 => RelocationType::ImageRelBasedHigh,
        2 => RelocationType::ImageRelBasedLow,
        3 => RelocationType::ImageRelBasedHighLow,
        4 => RelocationType::ImageRelBasedHighAdj,
        5 => RelocationType::ImageRelArch1,
        6 => RelocationType::ImageRelReserved,
        7 => RelocationType::ImageRelArch2,
        8 => RelocationType::ImageRelBasedRiscVLow12S,
        9 => RelocationType::ImageRelBasedMIPSJmpAddr16,
        10 => RelocationType::ImageRelBasedDir64,
        _ => return Err(RelocAnalyzerError::InvalidRelocType.into()),
    };

    Ok(Reloc {
        typ:    reloc_type,
        offset: base + RVA::from(reloc_offset as i64),
    })
}

fn split_u32(e: u32) -> (u16, u16) {
    let m = (e & 0x0000_FFFF) as u16;
    let n = ((e & 0xFFFF_0000) >> 16) as u16;
    (m, n)
}

/// ```
/// use lancelot::rsrc::*;
/// use lancelot::arch::*;
/// use lancelot::workspace::Workspace;
/// use lancelot::analysis::pe::relocs;
///
/// let mut ws = Workspace::from_bytes("k32.dll", &get_buf(Rsrc::K32))
///    .disable_analysis()
///    .load().unwrap();
/// let relocs = relocs::get_relocs(&ws).unwrap();
/// assert_eq!(relocs[0].offset,   RVA(0x76008));
/// assert_eq!(relocs[277].offset, RVA(0xA8000));
/// ```
pub fn get_relocs(ws: &Workspace) -> Result<Vec<Reloc>, Error> {
    let pe = match Object::parse(&ws.buf) {
        Ok(Object::PE(pe)) => pe,
        _ => return Ok(vec![]),
    };

    let opt_header = match pe.header.optional_header {
        Some(opt_header) => opt_header,
        _ => return Ok(vec![]),
    };

    let reloc_directory = match opt_header.data_directories.get_base_relocation_table() {
        Some(reloc_directory) => reloc_directory,
        _ => return Ok(vec![]),
    };

    let dir_start = RVA::from(reloc_directory.virtual_address as i64);
    let buf = ws.read_bytes(dir_start, reloc_directory.size as usize)?;

    let entries: Vec<u32> = buf.chunks_exact(0x4).map(|b| LittleEndian::read_u32(b)).collect();

    let mut ret = vec![];
    let mut index = 0;
    loop {
        // parse base relocation chunk
        // until there's no data left

        let page_rva = entries.get(index);
        let block_size = entries.get(index + 1);
        let (page_rva, block_size) = match (page_rva, block_size) {
            (Some(0), _) => break,
            (_, Some(0)) => break,
            (Some(page_rva), Some(block_size)) => (RVA::from(*page_rva as i64), *block_size),
            _ => break,
        };

        // cast from u32 to usize
        let chunk_entries_count = ((block_size) / 4) as usize;

        for i in 2..chunk_entries_count {
            if let Some(&entry) = entries.get(index + i) {
                let (m, n) = split_u32(entry);

                let reloc1 = parse_reloc(page_rva, m)?;
                if !ws.probe(reloc1.offset, 4, Permissions::R) {
                    break;
                }
                ret.push(reloc1);

                let reloc2 = parse_reloc(page_rva, n)?;
                if !ws.probe(reloc2.offset, 4, Permissions::R) {
                    break;
                }
                ret.push(reloc2);
            } else {
                break;
            }
        }

        index += chunk_entries_count;
    }

    Ok(ret)
}

fn sections_contain(sections: &[Range<RVA>], rva: RVA) -> bool {
    sections.iter().any(|section| section.contains(&rva))
}

impl Analyzer for RelocAnalyzer {
    fn get_name(&self) -> String {
        "PE relocation analyzer".to_string()
    }

    /// ```
    /// use lancelot::rsrc::*;
    /// use lancelot::arch::*;
    /// use lancelot::analysis::Analyzer;
    /// use lancelot::workspace::Workspace;
    /// use lancelot::analysis::pe::RelocAnalyzer;
    /// lancelot::test::init_logging();
    ///
    /// let mut ws = Workspace::from_bytes("k32.dll", &get_buf(Rsrc::K32))
    ///    .disable_analysis()
    ///    .load().unwrap();
    ///
    /// let anal = RelocAnalyzer::new();
    /// anal.analyze(&mut ws).unwrap();
    ///
    /// assert!(ws.get_meta(RVA(0xC7F0)).unwrap().is_insn());
    /// ```
    fn analyze(&self, ws: &mut Workspace) -> Result<(), Error> {
        let x_sections: Vec<Range<RVA>> = ws
            .module
            .sections
            .iter()
            .filter(|section| section.perms.intersects(Permissions::X))
            .map(|section| Range {
                start: section.addr,
                end:   section.end(),
            })
            .collect();

        let relocs: Vec<Reloc> = get_relocs(ws)?
            .into_iter()
            .filter(|r| match &r.typ {
                RelocationType::ImageRelBasedHighLow => true,
                RelocationType::ImageRelBasedDir64 => true,
                reloc_type => {
                    // all other reloc types are currently unsupported (uncommon)
                    warn!("ignoring relocation with unsupported type: {:?}", reloc_type);
                    false
                }
            })
            .collect();
        debug!("found {} total relocs", relocs.len());

        for reloc in relocs.iter() {
            // a relocation fixes up an existing pointer (hardcoded virtual address).
            // so we'll validate that the pointer actually points to something in the image.
            //
            // we might want to simply assume an invalid pointer means the reloc table is
            // invalid; however,
            // legit `Windows.Media.Protection.PlayReady.dll` has some invalid relocs (not
            // sure way).
            let ptr = match ws.read_va(reloc.offset) {
                Ok(ptr) => ptr,
                Err(_) => {
                    warn!("reloc fixes up an invalid pointer: {}", reloc.offset);
                    continue;
                }
            };

            let target = match ws.rva(ptr) {
                Some(target) => target,
                None => {
                    warn!("reloc fixes up a pointer to unmapped data: {} {}", reloc.offset, ptr);
                    continue;
                }
            };

            trace!(
                "found reloc {} -> {} {}",
                reloc.offset,
                target,
                if sections_contain(&x_sections, target) {
                    "(code)"
                } else {
                    ""
                }
            );
        }

        // scan for relocations to code.
        //
        // a relocation is a hardcoded offset that must be fixed up if the desired base
        // address  cannot be used.
        // for example, the function pointer passed to CreateThread will be a hardcoded
        // address  of the start of the function.
        //
        // relocated pointers may point to the .data section, e.g. strings or other
        // constants. we want to ignore these.
        // we are only interested in targets in executable sections.
        // we assume these are pointers to instructions/code.
        //
        // looking for pointers into the .text section
        // to things that
        //   1. are not already in an instruction
        //   2. don't appear to be a pointer
        // and assume this is code.

        let mut unique_targets: HashSet<RVA> = HashSet::new();
        unique_targets.extend(
            relocs
                .iter()
                .map(|reloc| reloc.offset)
                .map(|rva| ws.read_va(rva))
                .filter_map(Result::ok)
                .filter_map(|va| ws.rva(va)),
        );

        debug!("reduced to {} unique reloc targets", unique_targets.len());

        let o: Vec<RVA> = unique_targets
            .iter()
            .filter(|&&rva| sections_contain(&x_sections, rva))
            .filter(|&&rva| !is_in_insn(ws, rva))
            .filter(|&&rva| !is_ptr(ws, rva))
            .filter(|&&rva| !is_zero(ws, rva))
            .copied()
            // TODO: maybe ensure that the insn decodes.
            .collect();

        debug!("found {} relocs that point to instructions", o.len());

        o.iter().for_each(|&rva| {
            debug!(
                "found pointer via relocations from executable section to {} (code)",
                rva
            );

            // TODO: consume result
            ws.make_insn(rva).unwrap();
            // TODO: consume result
            ws.analyze().unwrap();
        });
        Ok(())
    }
}
