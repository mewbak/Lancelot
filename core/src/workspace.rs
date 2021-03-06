use std::collections::{HashMap, VecDeque};

use byteorder::{ByteOrder, LittleEndian};
use failure::{Error, Fail};
use log::{info, warn};
use zydis::{self, Decoder};

use super::{
    analysis::Analysis,
    arch::{Arch, RVA, VA},
    basicblock::BasicBlock,
    config::Config,
    loader::{self, LoadedModule, Loader, Permissions},
    util,
    xref::XrefType,
};

#[derive(Debug, Fail)]
pub enum WorkspaceError {
    #[fail(display = "The given buffer is not supported (arch/plat/file format)")]
    NotSupported,
    #[fail(display = "The given address is not mapped")]
    InvalidAddress,
    #[fail(display = "Buffer overrun")]
    BufferOverrun,
    #[fail(display = "The instruction at the given address is invalid")]
    InvalidInstruction,
}

pub struct WorkspaceBuilder {
    filename: String,
    buf:      Vec<u8>,
    config:   Config,

    loader: Option<Box<dyn Loader>>,

    should_analyze: bool,

    /// when true, the analysis failures should fail the loading of the module.
    strict_mode: bool,
}

impl WorkspaceBuilder {
    /// Override the default loader picker with the given loader.
    pub fn with_loader(self: WorkspaceBuilder, loader: Box<dyn Loader>) -> WorkspaceBuilder {
        info!("using explicitly chosen loader: {}", loader.get_name());
        WorkspaceBuilder {
            loader: Some(loader),
            ..self
        }
    }

    pub fn disable_analysis(self: WorkspaceBuilder) -> WorkspaceBuilder {
        info!("disabling analysis");
        WorkspaceBuilder {
            should_analyze: false,
            ..self
        }
    }

    pub fn enable_strict_mode(self: WorkspaceBuilder) -> WorkspaceBuilder {
        info!("enabling strict mode");
        WorkspaceBuilder {
            strict_mode: true,
            ..self
        }
    }

    pub fn with_config(self: WorkspaceBuilder, config: Config) -> WorkspaceBuilder {
        WorkspaceBuilder { config, ..self }
    }

    /// Construct a workspace with the given builder configuration.
    ///
    /// This invokes the loaders, analyzers, and another other logic,
    ///  resulting in an initialized Workspace instance.
    ///
    /// Example (with default loader):
    ///
    /// ```
    /// use lancelot::arch::*;
    /// use lancelot::workspace::Workspace;
    ///
    /// Workspace::from_bytes("foo.bin", b"\xEB\xFE")
    ///   .load()
    ///   .map(|ws| {
    ///     assert_eq!(ws.loader.get_name(),       "Windows/x32/Raw");
    ///     assert_eq!(ws.module.base_address,     VA(0x0));
    ///     assert_eq!(ws.module.sections[0].name, "raw");
    ///   })
    ///   .map_err(|e| panic!(e));
    /// ```
    ///
    /// Example (with specific loader):
    ///
    /// ```
    /// use lancelot::arch::*;
    /// use lancelot::loader::Platform;
    /// use lancelot::workspace::Workspace;
    /// use lancelot::loaders::sc::ShellcodeLoader;
    ///
    /// Workspace::from_bytes("foo.bin", b"\xEB\xFE")
    ///   .with_loader(Box::new(ShellcodeLoader::new(Platform::Windows, Arch::X32)))
    ///   .load()
    ///   .map(|ws| {
    ///     assert_eq!(ws.loader.get_name(),       "Windows/x32/Raw");
    ///     assert_eq!(ws.module.base_address,     VA(0x0));
    ///     assert_eq!(ws.module.sections[0].name, "raw");
    ///   })
    ///   .map_err(|e| panic!(e));
    /// ```
    pub fn load(self: WorkspaceBuilder) -> Result<Workspace, Error> {
        // if the user provided a loader, use that.
        // otherwise, use the default detected loader.
        let (ldr, module, analyzers) = match self.loader {
            // TODO: let users specify analyzers via builder
            Some(ldr) => {
                let (module, analyzers) = ldr.load(&self.config, &self.buf)?;
                (ldr, module, analyzers)
            }
            None => loader::load(&self.config, &self.buf)?,
        };

        info!("loaded {} sections:", module.sections.len());
        module.sections.iter().for_each(|sec| {
            info!("  - {:8} {:x}-{:x} {:?}", sec.name, sec.addr, sec.end(), sec.perms);
        });

        let analysis = Analysis::new(&module);

        let decoder = match ldr.get_arch() {
            Arch::X32 => Decoder::new(zydis::MachineMode::LEGACY_32, zydis::AddressWidth::_32).unwrap(),
            Arch::X64 => Decoder::new(zydis::MachineMode::LONG_64, zydis::AddressWidth::_64).unwrap(),
        };

        let mut ws = Workspace {
            filename: self.filename,
            buf: self.buf,

            loader: ldr,
            module,

            decoder,

            analysis,
        };

        if self.should_analyze {
            for analyzer in analyzers.iter() {
                info!("analyzing with {}", analyzer.get_name());
                if let Err(e) = analyzer.analyze(&mut ws) {
                    warn!("analyzer failed: {}: {}", analyzer.get_name(), e);
                    if self.strict_mode {
                        return Err(e);
                    }
                }
            }
        }

        Ok(ws)
    }
}

pub struct Workspace {
    // name or source of the file
    pub filename: String,
    // raw bytes of the file
    pub buf: Vec<u8>,

    pub loader: Box<dyn Loader>,
    pub module: LoadedModule,

    pub decoder: Decoder,

    // pub only so that we can split the impl
    pub analysis: Analysis,
}

impl Workspace {
    /// Create a workspace and load the given bytes.
    ///
    /// See example on `WorkspaceBuilder::load()`
    pub fn from_bytes(filename: &str, buf: &[u8]) -> WorkspaceBuilder {
        WorkspaceBuilder {
            filename:       filename.to_string(),
            buf:            buf.to_vec(),
            config:         Default::default(),
            loader:         None,
            should_analyze: true,
            strict_mode:    false,
        }
    }

    pub fn from_file(filename: &str) -> Result<WorkspaceBuilder, Error> {
        Ok(WorkspaceBuilder {
            filename:       filename.to_string(),
            buf:            util::read_file(filename)?,
            config:         Default::default(),
            loader:         None,
            should_analyze: true,
            strict_mode:    false,
        })
    }

    /// Read bytes from the given RVA.
    ///
    /// Errors:
    ///
    ///   - InvalidAddress - if the address is not mapped.
    ///   - BufferOverrun - if the requested region runs beyond the matching
    ///     section.
    ///
    /// Example:
    ///
    /// ```
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
    /// assert_eq!(ws.read_bytes(RVA(0x0), 0x1).unwrap(), b"\xEB");
    /// assert_eq!(ws.read_bytes(RVA(0x1), 0x1).unwrap(), b"\xFE");
    /// assert_eq!(ws.read_bytes(RVA(0x0), 0x2).unwrap(), b"\xEB\xFE");
    /// assert!(ws.read_bytes(RVA(0x0), 0xFFF).is_ok(), "read less than a page");
    /// assert!(ws.read_bytes(RVA(0x0), 0x1000).is_ok(), "read page");
    /// assert!(ws.read_bytes(RVA(0x0), 0x1001).is_err(), "read more than a page");
    /// assert!(ws.read_bytes(RVA(0x1), 0x1000).is_err(), "read unaligned page");
    /// ```
    pub fn read_bytes(&self, rva: RVA, length: usize) -> Result<Vec<u8>, Error> {
        self.module
            .address_space
            .slice(rva, rva + length)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
    }

    pub fn read_bytes_into<'a>(&self, rva: RVA, buf: &'a mut [u8]) -> Result<&'a [u8], Error> {
        self.module
            .address_space
            .slice_into(rva, buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(Ok)
    }

    /// Is the given range mapped?
    ///
    /// Example:
    ///
    /// ```
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    /// use lancelot::loader::Permissions;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
    /// assert!( ws.probe(RVA(0x0), 1, Permissions::R));
    /// assert!( ws.probe(RVA(0x0), 1, Permissions::X));
    /// assert!( ws.probe(RVA(0x0), 1, Permissions::RX));
    /// assert!( ws.probe(RVA(0x0), 2, Permissions::R));
    /// assert!(!ws.probe(RVA(0x0), 0x1001, Permissions::R));
    /// assert!( ws.probe(RVA(0x1), 1, Permissions::R));
    /// assert!(!ws.probe(RVA(0x1), 0x1000, Permissions::R));
    /// ```
    #[allow(clippy::collapsible_if)]
    pub fn probe(&self, rva: RVA, length: usize, perms: Permissions) -> bool {
        if perms.intersects(Permissions::X) {
            // hack: flowmeta is only mapped for executable regions
            if !(self.get_meta(rva).is_some() && self.get_meta(rva + length).is_some()) {
                return false;
            }
        }

        if perms.intersects(Permissions::R) {
            if !(self.module.address_space.probe(rva) && self.module.address_space.probe(rva + length)) {
                return false;
            }
        }

        if perms.intersects(Permissions::W) {
            // TODO
        }

        true
    }

    /// Read a byte from the given RVA.
    ///
    /// Errors: same as `read_bytes`.
    ///
    /// Example:
    ///
    /// ```
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
    /// assert_eq!(ws.read_u8(RVA(0x0)).unwrap(), 0xEB);
    /// assert_eq!(ws.read_u8(RVA(0x1)).unwrap(), 0xFE);
    ///
    /// assert_eq!(ws.read_u8(RVA(0x1000)).is_err(), true);
    /// ```
    pub fn read_u8(&self, rva: RVA) -> Result<u8, Error> {
        let mut buf = [0u8; 1];
        self.module
            .address_space
            .slice_into(rva, &mut buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(|buf| Ok(buf[0]))
    }

    /// Read a word from the given RVA.
    ///
    /// Errors: same as `read_bytes`.
    ///
    /// Example:
    ///
    /// ```
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
    /// assert_eq!(ws.read_u16(RVA(0x0)).unwrap(), 0xFEEB);
    /// assert_eq!(ws.read_u16(RVA(0x1000)).is_err(), true);
    /// ```
    pub fn read_u16(&self, rva: RVA) -> Result<u16, Error> {
        let mut buf = [0u8; 2];
        self.module
            .address_space
            .slice_into(rva, &mut buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(|buf| Ok(LittleEndian::read_u16(buf)))
    }

    /// Read a dword from the given RVA.
    pub fn read_u32(&self, rva: RVA) -> Result<u32, Error> {
        let mut buf = [0u8; 4];
        self.module
            .address_space
            .slice_into(rva, &mut buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(|buf| Ok(LittleEndian::read_u32(buf)))
    }

    /// Read a qword from the given RVA.
    pub fn read_u64(&self, rva: RVA) -> Result<u64, Error> {
        let mut buf = [0u8; 8];
        self.module
            .address_space
            .slice_into(rva, &mut buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(|buf| Ok(LittleEndian::read_u64(buf)))
    }

    /// Read a dword from the given RVA.
    pub fn read_i32(&self, rva: RVA) -> Result<i32, Error> {
        let mut buf = [0u8; 4];
        self.module
            .address_space
            .slice_into(rva, &mut buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(|buf| Ok(LittleEndian::read_i32(buf)))
    }

    /// Read a qword from the given RVA.
    pub fn read_i64(&self, rva: RVA) -> Result<i64, Error> {
        let mut buf = [0u8; 8];
        self.module
            .address_space
            .slice_into(rva, &mut buf)
            .map_err(|_| WorkspaceError::InvalidAddress.into())
            .and_then(|buf| Ok(LittleEndian::read_i64(buf)))
    }

    /// Read an RVA from the given RVA.
    /// Note that the size of the read is dependent on the architecture.
    ///
    /// Errors: same as `read_bytes`.
    ///
    /// Example:
    ///
    /// ```
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\x00\x11\x22\x33");
    /// assert_eq!(ws.read_rva(RVA(0x0)).unwrap(), RVA(0x33221100));
    /// assert_eq!(ws.read_rva(RVA(0x1000)).is_err(), true);
    /// ```
    pub fn read_rva(&self, rva: RVA) -> Result<RVA, Error> {
        match self.loader.get_arch() {
            Arch::X32 => Ok(RVA::from(self.read_i32(rva)?)),
            Arch::X64 => Ok(RVA::from(self.read_i64(rva)?)),
        }
    }

    /// Read a VA from the given RVA.
    /// Note that the size of the read is dependent on the architecture.
    pub fn read_va(&self, rva: RVA) -> Result<VA, Error> {
        match self.loader.get_arch() {
            Arch::X32 => Ok(VA::from(self.read_u32(rva)?)),
            Arch::X64 => Ok(VA::from(self.read_u64(rva)?)),
        }
    }

    /// Decode an instruction at the given RVA.
    ///
    /// Errors:
    ///
    ///   - InvalidAddress - if the address is not mapped.
    ///   - InvalidInstruction - if an instruction cannot be decoded.
    ///
    /// Example:
    ///
    /// ```
    /// use zydis;
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
    /// assert_eq!(ws.read_insn(RVA(0x0)).is_ok(), true);
    /// assert_eq!(ws.read_insn(RVA(0x0)).unwrap().length, 2);
    /// assert_eq!(ws.read_insn(RVA(0x0)).unwrap().mnemonic, zydis::Mnemonic::JMP);
    /// ```
    #[allow(clippy::collapsible_if)]
    pub fn read_insn(&self, rva: RVA) -> Result<zydis::DecodedInstruction, Error> {
        let mut buf = [0u8; 0x10];

        // we expect instructions to be at most 0x10 bytes long.
        // so try to read that many and decode.
        // this should usually work, unless we're at the end of a section.
        // otherwise, keep trying smaller reads.
        // this is naive and slow, but also uncommon.
        for buflen in (0x0..0x10).rev() {
            let mut buf = &mut buf[..buflen];

            if self.module.address_space.slice_into(rva, &mut buf).is_ok() {
                return match self.decoder.decode(&buf) {
                    Ok(Some(insn)) => Ok(insn),
                    Ok(None) => Err(WorkspaceError::InvalidInstruction.into()),
                    Err(_) => Err(WorkspaceError::InvalidInstruction.into()),
                };
            }

            // if the first read fails,
            // ensure at least the first requested address is mapped.
            // this will avoid 15 subsequent failed attempts to read
            // when the target is not mapped at all.
            if buflen == 0x10 {
                if !self.module.address_space.probe(rva) {
                    return Err(WorkspaceError::InvalidAddress.into());
                }
            }
        }

        Err(WorkspaceError::InvalidAddress.into())
    }

    /// Read a utf-8 encoded string at the given RVA.
    /// Only strings less than 0x1000 bytes are currently recognized.
    ///
    /// Errors:
    ///
    ///   - InvalidAddress - if the address is not mapped.
    ///   - std::str::Utf8Error - if the data is not valid utf8.
    ///
    /// Example:
    ///
    /// ```
    /// use lancelot::test;
    /// use lancelot::arch::RVA;
    ///
    /// let ws = test::get_shellcode32_workspace(b"\x00\x41\x41\x00");
    /// assert_eq!(ws.read_utf8(RVA(0x1)).unwrap(), "AA");
    /// ```
    pub fn read_utf8(&self, rva: RVA) -> Result<String, Error> {
        let mut buf = [0u8; 0x1000];

        // ideally, we can just read 0x1000 bytes,
        // but if this doesn't work, then we read up until the end of the current
        // section. note: max read size is 0x1000 bytes.
        if self.module.address_space.slice_into(rva, &mut buf).is_err() {
            // read until the end of the section.
            self.module
                .sections
                .iter()
                .find(|section| section.contains(rva))
                .ok_or_else(|| WorkspaceError::InvalidAddress.into())
                .and_then(|section| {
                    let size: usize = (section.end() - rva).into();
                    let size = std::cmp::min(size, 0x1000);
                    self.module.address_space.slice_into(rva, &mut buf[..size])
                })?;
        }

        // when we split, we're guaranteed at have at least one entry,
        // so .next().unwrap() is safe.
        let sbuf = buf.split(|&b| b == 0x0).next().unwrap();
        Ok(std::str::from_utf8(sbuf)?.to_string())
    }

    pub fn rva(&self, va: VA) -> Option<RVA> {
        if va < self.module.base_address {
            return None;
        }
        va.rva(self.module.base_address)
    }

    pub fn va(&self, rva: RVA) -> Option<VA> {
        self.module.base_address.va(rva)
    }

    // API:
    //   get_insn
    //   get_xrefs_to
    //   get_xrefs_from
    //   get_functions

    // elsewhere:
    //   call graph
    //   control flow graph

    // see: `analysis::impl Workspace`

    pub fn get_basic_blocks(&self, rva: RVA) -> Result<Vec<BasicBlock>, Error> {
        let mut bbs: HashMap<RVA, BasicBlock> = HashMap::new();

        let mut queue: VecDeque<RVA> = VecDeque::new();
        queue.push_back(rva);

        while let Some(first_insn) = queue.pop_front() {
            if bbs.contains_key(&first_insn) {
                continue;
            }

            let mut current_bb = BasicBlock {
                addr:         first_insn,
                length:       0x0,
                predecessors: vec![],
                successors:   vec![],
                insns:        vec![],
            };

            let mut current_insn = first_insn;

            'insns: loop {
                let current_insn_length = self.get_insn_length(current_insn)?;

                current_bb.length += current_insn_length as u64;
                current_bb.insns.push(current_insn);

                // does the instruction fallthrough?
                let mut has_fallthrough = false;
                // does the instruction flow elsewhere (jnz, jmp, cmov)?
                let mut has_flow_from = false;
                for xref in self.get_xrefs_from(current_insn)?.iter() {
                    match xref.typ {
                        XrefType::Fallthrough => has_fallthrough = true,
                        XrefType::UnconditionalJump | XrefType::ConditionalJump | XrefType::ConditionalMove => {
                            has_flow_from = true;
                            current_bb.successors.push(xref.dst);
                        }
                        XrefType::Call => {}
                    }
                }

                let next_insn = current_insn + current_insn_length;
                if !has_fallthrough {
                    // end of basic block
                    //
                    // case:
                    //
                    //     +----------------+
                    //     | ...            |
                    //     | RETN           |
                    //     +----------------+
                    //
                    // case:
                    //
                    //     +----------------+
                    //     | ...            |
                    //     | JMP 0x401000   |--+
                    //     +----------------+  |
                    //                         v

                    // flow successors were already added above,
                    // when enumerating the xrefs-from.
                    break 'insns;
                } else if has_flow_from {
                    // end of basic block
                    //
                    // case:
                    //
                    //     +----------------+
                    //     | ...            |
                    //     | JMP 0x401000   |--+
                    //     +----------------+  |
                    //                         v
                    // case:
                    //
                    //     +----------------+
                    //     | ...            |
                    //     | JNZ 0x401000   |--+
                    //     +----------------+  |
                    //           |             |
                    //           v             v

                    current_bb.successors.push(next_insn);

                    // flow successors were already added above,
                    // when enumerating the xrefs-from.

                    break 'insns;
                } else if has_fallthrough {
                    // does the next instruction have non-fallthrough flow to it?
                    let mut has_flow_to = false;
                    for xref in self.get_xrefs_to(next_insn)?.iter() {
                        match xref.typ {
                            XrefType::UnconditionalJump => has_flow_to = true,
                            XrefType::ConditionalJump => has_flow_to = true,
                            XrefType::ConditionalMove => has_flow_to = true,
                            XrefType::Fallthrough => {}
                            XrefType::Call => {}
                        }
                    }

                    if has_flow_to {
                        // end of the basic block.
                        // the next instruction has non-fallthrough flow to it.
                        //
                        // case:
                        //
                        //     +----------------+
                        //     | ...            |
                        //     | MOV eax, ebx   |
                        //     +----------------+
                        //           |           +--+
                        //           |          /   |
                        //           v         v    |
                        //     +----------------+   |
                        //     | ...            |   |
                        //     +----------------+   |
                        //

                        current_bb.successors.push(next_insn);

                        break 'insns;
                    } else {
                        // common path: not the end of a basic block.
                        // we'll continue by inspecting the next instruction.
                        //
                        // case:
                        //
                        //     +----------------+
                        //     | ...            |
                        //     | MOV eax, ebx   |
                        //     | ...            |
                        //     +----------------+
                        //

                        current_insn = next_insn;
                        continue 'insns;
                    }
                } else {
                    unreachable!();
                }
            }
            // handle end of basic block

            // enqueue the successors
            for successor in current_bb.successors.iter() {
                queue.push_back(*successor);
            }

            // commit the current basic block
            bbs.insert(current_bb.addr, current_bb);
        }

        let mut predecessors: HashMap<RVA, Vec<RVA>> = HashMap::new();
        for bb in bbs.values() {
            for successor in bb.successors.iter() {
                predecessors.entry(*successor).or_insert_with(Vec::new).push(bb.addr);
            }
        }

        for (successor, preds) in predecessors.iter() {
            if let Some(bb) = bbs.get_mut(successor) {
                for pred in preds.iter() {
                    bb.predecessors.push(*pred);
                }
            }
        }

        Ok(bbs.values().cloned().collect())
    }
}
