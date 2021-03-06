use bitflags::bitflags;
use failure::{Error, Fail};
use log::info;
use strum_macros::Display;

use super::{
    analysis::Analyzer,
    arch::{Arch, RVA, VA},
    config::Config,
    loaders::{pe::PELoader, sc::ShellcodeLoader},
    pagemap::PageMap,
};

#[derive(Debug, Fail)]
pub enum LoaderError {
    #[fail(display = "The given buffer is not supported (arch/plat/file format)")]
    NotSupported,
    #[fail(display = "The given buffer uses a bitness incompatible with the architecture")]
    MismatchedBitness,
}

#[derive(Display, Clone, Copy)]
pub enum FileFormat {
    Raw, // shellcode
    PE,
}

#[derive(Display, Clone, Copy)]
pub enum Platform {
    Windows,
}

bitflags! {
    pub struct Permissions: u8 {
        const R = 0b0000_0001;
        const W = 0b0000_0010;
        const X = 0b0000_0100;
        const RW = Self::R.bits | Self::W.bits;
        const RX =  Self::R.bits | Self::X.bits;
        const WX =  Self::W.bits | Self::X.bits;
        const RWX =  Self::R.bits | Self::W.bits | Self::X.bits;
    }
}

#[derive(Debug)]
pub struct Section {
    pub addr:  RVA,
    pub size:  u32,
    pub perms: Permissions,
    pub name:  String,
}

impl Section {
    pub fn contains(self: &Section, rva: RVA) -> bool {
        if rva < self.addr {
            return false;
        }

        let max = self.addr + self.size as i32; // danger
        if rva >= max {
            return false;
        }

        true
    }

    pub fn is_executable(&self) -> bool {
        self.perms.intersects(Permissions::X)
    }

    pub fn end(&self) -> RVA {
        self.addr + RVA::from(self.size as i64)
    }
}

pub struct LoadedModule {
    pub base_address:  VA,
    pub sections:      Vec<Section>,
    pub address_space: PageMap<u8>,
}

impl LoadedModule {
    pub fn max_address(&self) -> RVA {
        self.sections.iter().map(|sec| sec.end()).max().unwrap() // danger: assume there's at least one section.
    }
}

pub trait Loader {
    /// Fetch the number of bits for a pointer in this architecture.
    fn get_arch(&self) -> Arch;
    fn get_plat(&self) -> Platform;
    fn get_file_format(&self) -> FileFormat;

    fn get_name(&self) -> String {
        return format!("{}/{}/{}", self.get_plat(), self.get_arch(), self.get_file_format());
    }

    /// Returns True if this Loader knows how to load the given bytes.
    fn taste(&self, config: &Config, buf: &[u8]) -> bool;

    /// Load the given bytes into a Module and suggest the appropriate
    /// Analyzers.
    ///
    /// While the loader is parsing a file, it should determine what
    ///  the most appropriate analyzers are, e.g. a PE loader may inspect the
    /// headers  to determine if there is Control Flow Guard metadata that
    /// can be analyzed.
    fn load(&self, config: &Config, buf: &[u8]) -> Result<(LoadedModule, Vec<Box<dyn Analyzer>>), Error>;
}

pub fn default_loaders() -> Vec<Box<dyn Loader>> {
    // we might like these to come from a lazy_static global,
    //  however, then these have to be Sync.
    // I'm not sure if that's a good idea yet.
    let mut loaders: Vec<Box<dyn Loader>> = vec![];
    // the order here matters!
    // the default `load` routine will pick the first matching loader,
    //  so the earlier entries here have higher precedence.

    loaders.push(Box::new(PELoader::new(Arch::X32)));
    loaders.push(Box::new(PELoader::new(Arch::X64)));
    loaders.push(Box::new(ShellcodeLoader::new(Platform::Windows, Arch::X32)));
    loaders.push(Box::new(ShellcodeLoader::new(Platform::Windows, Arch::X64)));

    loaders
}

/// Find the loaders that support loading the given sample.
///
/// The result is an iterator so that the caller can use the first
///  matching result without waiting for all Loaders to taste the bytes.
///
/// Loaders are tasted in the order defined in `default_loaders`.
///
/// Example:
///
/// ```
/// use lancelot::arch::*;
/// use lancelot::config::*;
/// use lancelot::loader::*;
///
/// match taste(&Config::default(), b"\xEB\xFE").nth(0) {
///   Some(loader) => assert_eq!(loader.get_name(), "Windows/x32/Raw"),
///   None => panic!("no matching loaders"),
/// };
/// ```
pub fn taste<'a>(config: &'a Config, buf: &'a [u8]) -> impl Iterator<Item = Box<dyn Loader>> + 'a {
    default_loaders()
        .into_iter()
        .filter(move |loader| loader.taste(config, buf))
}

/// Load the given sample using the first matching loader from
/// `default_loaders`.
///
/// Example:
///
/// ```
/// use lancelot::arch::*;
/// use lancelot::config::*;
/// use lancelot::loader::*;
///
/// load(&Config::default(), b"\xEB\xFE")
///   .map(|(loader, module, analyzers)| {
///     assert_eq!(loader.get_name(),       "Windows/x32/Raw");
///     assert_eq!(module.base_address,     VA(0x0));
///     assert_eq!(module.sections[0].name, "raw");
///   })
///   .map_err(|e| panic!(e));
/// ```
#[allow(clippy::type_complexity)]
pub fn load(config: &Config, buf: &[u8]) -> Result<(Box<dyn Loader>, LoadedModule, Vec<Box<dyn Analyzer>>), Error> {
    match taste(config, buf).nth(0) {
        Some(loader) => {
            info!("auto-detected loader: {}", loader.get_name());
            loader
                .load(config, buf)
                .map(|(module, analyzers)| (loader, module, analyzers))
        }
        None => Err(LoaderError::NotSupported.into()),
    }
}
