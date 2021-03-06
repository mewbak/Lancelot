//< Helpers that are useful for tests and doctests.

use super::{arch::Arch, loader, loaders::sc::ShellcodeLoader, rsrc::*, workspace::Workspace};

/// Helper to construct a 32-bit Windows shellcode workspace from raw bytes.
///
/// It may panic when the workspace cannot be created/loaded.
/// Therefore, this is best used for tests.
///
/// ```
/// use lancelot::test;
/// use lancelot::arch::*;
///
/// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
/// assert_eq!(ws.read_u8(RVA(0x0)).unwrap(), 0xEB);
/// ```
pub fn get_shellcode32_workspace(buf: &[u8]) -> Workspace {
    Workspace::from_bytes("foo.bin", buf)
        .with_loader(Box::new(ShellcodeLoader::new(loader::Platform::Windows, Arch::X32)))
        .load()
        .unwrap()
}

pub fn get_shellcode64_workspace(buf: &[u8]) -> Workspace {
    Workspace::from_bytes("foo.bin", buf)
        .with_loader(Box::new(ShellcodeLoader::new(loader::Platform::Windows, Arch::X64)))
        .load()
        .unwrap()
}

pub fn get_rsrc_workspace(rsrc: Rsrc) -> Workspace {
    Workspace::from_bytes("foo.bin", &get_buf(rsrc)).load().unwrap()
}

/// configure a global logger at level==DEBUG.
pub fn init_logging() {
    let log_level = log::LevelFilter::Debug;
    fern::Dispatch::new()
        .format(move |out, message, record| {
            out.finish(format_args!(
                "{} [{:5}] {} {}",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                if log_level == log::LevelFilter::Trace {
                    record.target()
                } else {
                    ""
                },
                message
            ))
        })
        .level(log_level)
        .chain(std::io::stderr())
        .filter(|metadata| !metadata.target().starts_with("goblin::pe"))
        .apply()
        .expect("failed to configure logging");
}
