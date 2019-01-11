extern crate log;
extern crate simplelog;

use goblin::Object;
use log::{debug, error, info, trace, warn};
use rayon::prelude::*;
use std::env;
use std::fmt;
use std::fs;
use std::io::prelude::*;
use zydis;

// enabled only during testing.
// supports reaching into the resources dir for test data.
// TODO: doesn't work nicely work vscode-rls (which doesn't pass along the features=test)
// #[cfg(feature = "test")]
pub mod rsrc;

pub mod analysis;

pub struct Config {
    pub filename: String,
}

impl Config {
    pub fn from_args(args: env::Args) -> Result<Config, &'static str> {
        let args: Vec<String> = args.collect();

        if args.len() < 2 {
            return Err("not enough arguments");
        }

        let filename = args[1].clone();
        trace!("config: parsed filename: {:?}", filename);

        Ok(Config { filename })
    }
}

pub fn setup_logging(_args: &Config) {
    simplelog::TermLogger::init(simplelog::LevelFilter::Info, simplelog::Config::default())
        .expect("failed to setup logging");
}

#[derive(Debug)]
pub enum Error {
    FileAccess,
    FileFormat,
    NotImplemented(&'static str),
    InvalidRva,
}

/// Static cast the given 64-bit unsigned integer to a 64-bit signed integer.
/// This is probably only useful when some other code provides you a u64
///  that is meant to be an i64 (aka. uncommon).
///
/// In C: `*(int64_t *)&i`
///
/// # Examples
///
/// ```
/// use lancelot::*;
/// assert_eq!(0, u64_i64(0));
/// assert_eq!(1, u64_i64(0x1));
/// assert_eq!(-1, u64_i64(0xFFFF_FFFF_FFFF_FFFF));
/// ```
pub fn u64_i64(i: u64) -> i64 {
    if i & 1 << 63 > 0 {
        // TODO: there's probably some elegant rust-way to do this.
        // in the meantime, manually compute twos-complement.
        let bits = i & 0x7FFF_FFFF_FFFF_FFFF;
        let bits = !bits;
        let bits = bits + 1;
        let bits = bits & 0x7FFF_FFFF_FFFF_FFFF;
        -(bits as i64)
    } else {
        i as i64
    }
}

/// Round the given value up to the next multiple of the given base.
///
/// # Panics
///
///   - Base `b` must be at least `2`.
///
/// # Examples
///
/// ```
/// use lancelot::*;
/// assert_eq!(align(0, 2), 0);
/// assert_eq!(align(1, 2), 2);
/// assert_eq!(align(2, 2), 2);
/// assert_eq!(align(3, 2), 4);
/// assert_eq!(align(4, 2), 4);
/// ```
pub fn align(i: usize, b: usize) -> usize {
    if b < 2 {
        panic!("base `b` must be at least: 2");
    }
    let rem = i % b;
    if rem == 0 {
        i
    } else {
        i + (b - rem)
    }
}

pub fn hexdump_ascii(b: u8) -> char {
    if b.is_ascii_graphic() || b == b' ' {
        b as char
    } else {
        '.'
    }
}

pub fn hexdump(buf: &[u8], offset: usize) -> String {
    // 01234567:  00 01 02 03 04 05 06 07  ...............
    // <prefix>   <hex col>                <ascii col>

    let padding = "  ";

    let padding_size = 2;
    let hex_col_size = 3;
    let ascii_col_size = 1;
    let prefix_size = 8 + 1;
    let newline_size = 1;
    let line_size = prefix_size
        + padding_size
        + 16 * hex_col_size
        + padding_size
        + 16 * ascii_col_size
        + newline_size;
    let line_count = align(buf.len(), 0x10) / 0x10;

    let mut ret = String::with_capacity(line_count * line_size);

    let mut line = String::with_capacity(line_size);
    let mut remaining_count = buf.len();
    for line_index in 0..line_count {
        let line_elem_count = 0x10.min(remaining_count);
        let padding_elem_count = 0x10 - line_elem_count;

        // 01234567:  00 01 02 03 04 05 06 07  ...............
        // ^^^^^^^^^
        line.push_str(format!("{:08x}:", offset + 0x10 * line_index).as_str());

        // 01234567:  00 01 02 03 04 05 06 07  ...............
        //          ^^
        line.push_str(padding);

        // 01234567:  00 01 02 03 04 05 06 07  ...............
        //            ^^^
        for elem in &buf[line_index..line_index + line_elem_count] {
            line.push_str(format!("{:02x} ", elem).as_str());
        }
        for _ in 0..padding_elem_count {
            line.push_str("   ");
        }

        // 01234567:  00 01 02 03 04 05 06 07  ...............
        //                                   ^^
        line.push_str(padding);

        // 01234567:  00 01 02 03 04 05 06 07  ...............
        //                                     ^
        for elem in &buf[line_index..line_index + line_elem_count] {
            line.push(hexdump_ascii(*elem))
        }
        for _ in 0..padding_elem_count {
            line.push(' ');
        }
        line.push_str(padding);

        // 01234567:  00 01 02 03 04 05 06 07  ...............
        //                                                    ^
        line.push('\n');

        ret.push_str(line.as_str());
        line.truncate(0x0);
        remaining_count -= line_elem_count;
    }

    ret
}

pub fn read_file(filename: &str) -> Result<Vec<u8>, Error> {
    debug!("read_file: {:?}", filename);

    let mut buf = Vec::new();
    {
        debug!("reading file: {}", filename);
        let mut f = match fs::File::open(filename) {
            Ok(f) => f,
            Err(_) => {
                error!("failed to open file: {}", filename);
                return Err(Error::FileAccess);
            }
        };
        let bytes_read = match f.read_to_end(&mut buf) {
            Ok(c) => c,
            Err(_) => {
                error!("failed to read entire file: {}", filename);
                return Err(Error::FileAccess);
            }
        };
        debug!("read {} bytes", bytes_read);
        if bytes_read < 0x10 {
            error!("file too small: {}", filename);
            return Err(Error::FileFormat);
        }
    }

    Ok(buf)
}

type Rva = u64;

#[derive(Debug, Clone)]
pub enum XrefType {
    // mov eax, eax
    // push ebp
    Fallthrough,
    // call [0x401000]
    Call,
    // call [eax]
    //IndirectCall { src: Rva },
    // jmp 0x401000
    UnconditionalJump,
    // jmp eax
    //UnconditionalIndirectJump { src: Rva, dst: Rva },
    // jnz 0x401000
    ConditionalJump,
    // jnz eax
    //ConditionalIndirectJump { src: Rva },
    // cmov 0x1
    ConditionalMove,
}

#[derive(Debug, Clone)]
pub struct Xref {
    src: Rva,
    dst: Rva,
    typ: XrefType,
}

pub struct Xrefs {
    from: Vec<Xref>,
    to: Vec<Xref>,
}

pub struct Location {
    addr: Rva,
    xrefs: Xrefs,
}

pub enum Instruction<'a> {
    Invalid {
        loc: &'a Location,
    },
    Valid {
        loc: &'a Location,
        insn: zydis::ffi::DecodedInstruction,
    },
}

pub struct Section {
    pub name: String,
    pub addr: Rva,
    pub buf: Vec<u8>,
    pub locs: Vec<Location>,
}

impl Section {
    pub fn contains(self: &Section, rva: Rva) -> bool {
        if rva < self.addr {
            return false;
        }
        if rva >= self.addr + self.buf.len() as Rva {
            return false;
        }

        true
    }
}

pub struct Disassembler {
    pub decoder: zydis::ffi::Decoder,
    pub pc: zydis::enums::register::Register,
}

pub struct SectionLayout {
    pub addr: Rva,
    pub len: u64,
}

impl SectionLayout {
    pub fn contains(self: &SectionLayout, rva: Rva) -> bool {
        if rva < self.addr {
            return false;
        }
        if rva >= self.addr + self.len as Rva {
            return false;
        }
        true
    }
}

pub struct ModuleLayout {
    pub sections: Vec<SectionLayout>,
}

impl ModuleLayout {
    pub fn is_rva_valid(self: &ModuleLayout, rva: Rva) -> bool {
        self.sections.iter().any(|sec| sec.contains(rva))
    }
}

pub struct Workspace {
    pub filename: String,
    pub buf: Vec<u8>,
    pub sections: Vec<Section>,
    pub dis: Disassembler,
}

impl<'a> Instruction<'a> {
    pub fn from<'b>(
        decoder: &zydis::ffi::Decoder,
        buf: &[u8],
        loc: &'b Location,
    ) -> Instruction<'b> {
        match decoder.decode(buf) {
            Ok(Some(insn)) => Instruction::Valid { loc, insn },
            _ => Instruction::Invalid { loc },
        }
    }
}

impl<'a> fmt::Display for Instruction<'a> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let formatter = zydis::Formatter::new(zydis::FormatterStyle::Intel).expect("formatter");
        let mut buffer = [0u8; 200];
        let mut buffer = zydis::OutputBuffer::new(&mut buffer[..]);
        match &self {
            Instruction::Valid { insn, loc } => {
                formatter
                    .format_instruction(&insn, &mut buffer, Some(loc.addr), None)
                    .expect("format");
                write!(f, "0x{:016X}: {}", loc.addr, buffer)
            }
            Instruction::Invalid { loc } => write!(f, "0x{:016X}: invalid instruction", loc.addr),
        }
    }
}

pub struct InstructionIterator<'a> {
    workspace: &'a Workspace,
    current_section: usize,
    current_address: Rva,
}

impl<'a> Iterator for InstructionIterator<'a> {
    type Item = Instruction<'a>;

    fn next(&mut self) -> Option<Instruction<'a>> {
        let current_address = self.current_address + 1;
        if self.workspace.sections[self.current_section].contains(current_address) {
            self.current_address = current_address;
            Some(self.workspace.get_insn(self.current_address).unwrap())
        } else if self.current_section + 1 < self.workspace.sections.len() {
            self.current_section += 1;
            self.current_address = self.workspace.sections[self.current_section].addr;
            Some(self.workspace.get_insn(self.current_address).unwrap())
        } else {
            None
        }
    }
}

impl Workspace {
    /// Parse the given file into its object.
    ///
    /// # Errors
    ///   - `Error::FileFormat`: when not able to be parsed by Goblin.
    ///
    /// # Examples
    ///
    /// ```
    /// use goblin::Object;
    /// use matches::matches;
    /// use lancelot::rsrc::*;
    /// let ws = get_workspace(Rsrc::K32);
    /// assert!(matches!(ws.get_obj().unwrap(), Object::PE(_)));
    /// ```
    ///
    /// you might be tempted to maintain a method `get_pe`,
    /// however, i don't think this is a good idea:
    /// its fragile because the file type may not be PE.
    /// therefore, force clients to be explicit:
    ///
    /// ```
    /// use goblin::Object;
    /// use lancelot::rsrc::*;
    /// let ws = get_workspace(Rsrc::K32);
    /// if let Object::PE(_) = ws.get_obj().unwrap() {
    ///     // everyone is happy!
    /// }
    /// ```
    ///
    /// TODO: demonstrate `Error::FileFormat`.
    pub fn get_obj(&self) -> Result<Object, Error> {
        let obj = match Object::parse(&self.buf) {
            Ok(o) => o,
            Err(e) => {
                error!("failed to parse file: {} error: {:?}", self.filename, e);
                return Err(Error::FileFormat);
            }
        };

        match obj {
            Object::Unknown(_) => {
                error!(
                    "unknown file format, magic: | {:02X} {:02X} | '{}{}' ",
                    self.buf[0],
                    self.buf[1],
                    hexdump_ascii(self.buf[0]),
                    hexdump_ascii(self.buf[1])
                );

                Err(Error::FileFormat)
            }
            _ => Ok(obj),
        }
    }

    /// Fetch the section that contains the given address.
    ///
    /// # Examples
    ///
    /// ```
    /// use lancelot::*;
    /// use lancelot::rsrc::*;
    /// let ws = rsrc::get_workspace(rsrc::Rsrc::K32);
    /// assert_eq!(ws.get_section(0x130C0).expect("section").name, ".text");
    /// ```
    pub fn get_section(self: &Workspace, rva: Rva) -> Result<&Section, Error> {
        let sec = self.sections.iter().find(|sec| sec.contains(rva));
        match sec {
            None => Err(Error::InvalidRva),
            Some(sec) => Ok(sec),
        }
    }

    fn get_section_mut(self: &mut Workspace, rva: Rva) -> Result<&mut Section, Error> {
        let sec = self.sections.iter_mut().find(|sec| sec.contains(rva));
        match sec {
            None => Err(Error::InvalidRva),
            Some(sec) => Ok(sec),
        }
    }

    pub fn get_section_layout(self: &Workspace) -> ModuleLayout {
        ModuleLayout {
            sections: self
                .sections
                .iter()
                .map(|sec| SectionLayout {
                    addr: sec.addr,
                    len: sec.locs.len() as u64,
                })
                .collect(),
        }
    }

    pub fn is_rva_valid(self: &Workspace, rva: Rva) -> bool {
        self.sections.iter().any(|sec| sec.contains(rva))
    }

    /// Fetch the instruction at the given RVA.
    ///
    /// # Result
    ///
    /// Err: if the RVA does not fall within a section.
    /// None: if there is not a valid instruction at the given RVA.
    /// _: valid instruction.
    ///
    /// # Examples
    ///
    /// ```
    /// use zydis::*;
    /// use lancelot::*;
    /// use lancelot::rsrc::*;
    /// use matches::matches;
    /// let ws = rsrc::get_workspace(rsrc::Rsrc::K32);
    /// let insn = ws.get_insn(0x130C0).unwrap().unwrap();
    /// assert!(matches!(insn.mnemonic, zydis::enums::mnemonic::Mnemonic::MOV));
    /// ```
    pub fn get_insn(self: &Workspace, rva: Rva) -> Result<Instruction, Error> {
        let sec = self.get_section(rva)?;
        // hack: the typing here.
        let index: usize = rva as usize - sec.addr as usize;
        let loc = &sec.locs[index];
        let buf = &sec.buf[index..];
        Ok(Instruction::from(&self.dis.decoder, buf, loc))
    }

    pub fn iter_insns(self: &Workspace) -> InstructionIterator {
        InstructionIterator {
            workspace: self,
            current_section: 0,
            // TODO: ensure there are some sections
            current_address: self.sections[0].addr,
        }
    }

    fn get_loc_mut(self: &mut Workspace, rva: Rva) -> Result<&mut Location, Error> {
        let sec = self.get_section_mut(rva)?;
        let index: usize = rva as usize - sec.addr as usize;
        Ok(&mut sec.locs[index])
    }

    pub fn get_loc(self: &Workspace, rva: Rva) -> Result<&Location, Error> {
        let sec = self.get_section(rva)?;
        let index: usize = rva as usize - sec.addr as usize;
        Ok(&sec.locs[index])
    }

    fn get_first_operand(
        insn: &zydis::ffi::DecodedInstruction,
    ) -> Option<&zydis::ffi::DecodedOperand> {
        insn.operands
            .iter()
            .find(|op| op.visibility == zydis::enums::OperandVisibility::Explicit)
    }

    fn add_xref(self: &mut Workspace, xref: Xref) -> Result<(), Error> {
        if let Ok(loc) = self.get_loc_mut(xref.dst) {
            loc.xrefs.to.push(xref.clone());
        }
        if let Ok(loc) = self.get_loc_mut(xref.src) {
            loc.xrefs.from.push(xref);
        }
        Ok(())
    }

    fn analyze_xrefs(self: &mut Workspace) -> Result<(), Error> {
        let sec_layout = self.get_section_layout();
        let pc = self.dis.pc;

        let xrefs: Vec<Vec<Xref>> = self
            .iter_insns()
            // empirically, this doesn't seem to be worth it.
            // i wonder if perhaps par_bridge is pretty slow.
            //.par_bridge()
            .map(|insn| {
                if let Instruction::Valid { insn, loc, .. } = insn {
                    analysis::analyze_insn_xrefs(&sec_layout, pc, loc.addr, &insn).unwrap()
                } else {
                    Vec::new()
                }
            })
            .collect();

        for xrefs_ in xrefs {
            for xref in xrefs_ {
                self.add_xref(xref)?;
            }
        }

        Ok(())
    }

    fn load_disassembler(self: &mut Workspace) -> Result<(), Error> {
        match self.get_obj()? {
            Object::PE(pe) => {
                if !pe.is_64 {
                    self.dis = Disassembler {
                        decoder: zydis::Decoder::new(
                            zydis::MachineMode::LongCompat32,
                            zydis::AddressWidth::_32,
                        )
                        .unwrap(),
                        pc: zydis::enums::register::Register::EAX,
                    }
                }
                Ok(())
            }
            _ => Err(Error::NotImplemented("disassembler for non-PE module")),
        }
    }

    // this must be called *after* `load_disassembler`.
    fn load_sections(self: &mut Workspace) -> Result<(), Error> {
        match self.get_obj()? {
            Object::PE(pe) => {
                let buf = &self.buf;

                // TODO: load PE header, too

                self.sections
                    .extend(pe.sections.iter().map(|section| -> Section {
                        // TODO: i'm sure this can be abused.
                        // TODO: add tests for weird section names.
                        let name = String::from_utf8_lossy(&section.name[..])
                            .into_owned()
                            .trim_end_matches("\u{0}")
                            .trim_end()
                            .to_string();

                        // TODO: figure out if we will work with usize, or u64, or what,
                        // then assert usize is ok.
                        // ref: `usize::max_value()`
                        let virtual_size: u32 = align(section.virtual_size as usize, 0x200) as u32;
                        let mut secbuf = vec![0; virtual_size as usize];

                        {
                            let secsize = section.size_of_raw_data as usize;
                            let rawbuf = &mut secbuf[..secsize];
                            let pstart = section.pointer_to_raw_data as usize;
                            rawbuf.copy_from_slice(&buf[pstart..pstart + secsize]);
                        }

                        info!(
                            "loaded section: {}\t{:x}\t{:x}",
                            name,
                            section.virtual_address,
                            section.virtual_address + virtual_size
                        );
                        Section {
                            name: name,
                            addr: section.virtual_address as Rva,
                            buf: secbuf,
                            locs: (section.virtual_address..section.virtual_address + virtual_size)
                                .map(|addr| Location {
                                    addr: addr.into(),
                                    xrefs: Xrefs {
                                        to: vec![],
                                        from: vec![],
                                    },
                                })
                                .collect(),
                        }
                    }));

                Ok(())
            }
            Object::Elf(_) => Err(Error::NotImplemented("load sections for ELF module")),
            Object::Mach(_) => Err(Error::NotImplemented("load sections for MachO module")),
            Object::Archive(_) => Err(Error::NotImplemented("load sections for archive module")),
            Object::Unknown(_) => Err(Error::NotImplemented("load sections for unknown module")),
        }
    }

    /// Construct a workspace from the module with the given contents.
    ///
    /// # Errors
    ///   - `Error::FileFormat`: when not able to be parsed by Goblin.
    ///   - `Error::NotImplemented`: when not a PE file.
    ///
    /// # Examples
    ///
    /// ```
    /// use lancelot::*;
    /// use lancelot::rsrc::*;
    /// let buf = get_buf(Rsrc::K32);
    /// let ws = Workspace::from_buf("kernel32.dll", buf).unwrap();
    /// ```
    ///
    /// TODO: demonstrate ELF file behavior.
    /// TODO: demonstrate MachO file behavior.
    /// TODO: demonstrate unknown file behavior.
    pub fn from_buf(filename: &str, buf: Vec<u8>) -> Result<Workspace, Error> {
        let mut ws = Workspace {
            filename: filename.to_string(),
            buf: buf,
            sections: vec![],
            dis: Disassembler {
                // default disassembly settings.
                // will be updated subsequently if 32-bit.
                decoder: zydis::Decoder::new(zydis::MachineMode::Long64, zydis::AddressWidth::_64)
                    .unwrap(),
                pc: zydis::enums::register::Register::RIP,
            },
        };

        ws.load_disassembler()?;
        ws.load_sections()?;
        ws.analyze_xrefs()?;

        Ok(ws)
    }

    /// Construct a workspace from the module at given a file path.
    ///
    /// ```
    /// use lancelot::*;
    /// use lancelot::rsrc::*;
    /// let path = get_path(Rsrc::K32);
    /// // This test resource file is mangled. Needs to be fixed before parsing.
    /// // Otherwise, the following would work:
    /// // let ws = Workspace::from_file(&path).unwrap();
    /// ```
    pub fn from_file(filename: &str) -> Result<Workspace, Error> {
        let buf = read_file(filename)?;
        Workspace::from_buf(filename, buf)
    }
}

pub fn run(args: &Config) -> Result<(), Error> {
    debug!("filename: {:?}", args.filename);
    let ws = Workspace::from_file(&args.filename)?;

    if let Object::PE(pe) = ws.get_obj()? {
        let oep: Rva = pe
            .header
            .optional_header
            .unwrap()
            .standard_fields
            .address_of_entry_point;

        println!("entrypoint: {:}", ws.get_insn(oep)?);
    }

    println!("roots: {:}", analysis::find_roots(&ws).expect("foo").len());
    println!(
        "call targets: {:}",
        analysis::find_call_targets(&ws).expect("foo").len()
    );
    println!(
        "branch targets: {:}",
        analysis::find_branch_targets(&ws).expect("foo").len()
    );
    println!(
        "entry points: {:}",
        analysis::find_entrypoints(&ws).expect("foo").len()
    );
    println!(
        "runtime functions: {:}",
        analysis::find_runtime_functions(&ws).expect("foo").len()
    );

    Ok(())
}
