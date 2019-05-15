//< Helpers that are useful for tests and doctests.

use super::arch::*;
use super::loader;
use super::workspace::Workspace;

/// Helper to construct a 32-bit Windows shellcode workspace from raw bytes.
///
/// It may panic when the workspace cannot be created/loaded.
/// Therefore, this is best used for tests.
///
/// ```
/// use lancelot::test;
///
/// let ws = test::get_shellcode32_workspace(b"\xEB\xFE");
/// assert_eq!(ws.read_u8(0x0).unwrap(), 0xEB);
/// ```
pub fn get_shellcode32_workspace(buf: &[u8]) -> Workspace<Arch32> {
    Workspace::<Arch32>::from_bytes("foo.bin", buf)
        .with_loader(Box::new(loader::ShellcodeLoader::<Arch32>::new(
            loader::Platform::Windows,
        )))
        .load()
        .unwrap()
}
