//! Moving a file to the Recycle Bin (restorable), as Explorer does.

use std::path::Path;

use windows::core::PCWSTR;
use windows::Win32::UI::Shell::{SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FO_DELETE, SHFILEOPSTRUCTW};

/// A path in the form SHFileOperation understands: backslashes only and no `\\?\` prefix
/// (otherwise it answers with error 124).
fn shell_path(path: &Path) -> Vec<u16> {
    let s = path.to_string_lossy().replace('/', "\\");
    let s = match s.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s),
    };
    s.encode_utf16().collect()
}

pub fn recycle(path: &Path) -> Result<(), String> {
    let mut from = shell_path(path);
    from.push(0);
    from.push(0);
    let mut op = SHFILEOPSTRUCTW {
        wFunc: FO_DELETE,
        pFrom: PCWSTR(from.as_ptr()),
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI).0 as u16,
        ..Default::default()
    };
    let r = unsafe { SHFileOperationW(&mut op) };
    if r == 0 && !op.fAnyOperationsAborted.as_bool() {
        Ok(())
    } else {
        Err(format!("error code {r}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("ir-recycle-{}-{tag}.txt", std::process::id()));
        std::fs::write(&p, b"x").unwrap();
        p
    }

    #[test]
    fn recycle_accepts_plain_slash_and_extended_paths() {
        let p = temp_file("plain");
        recycle(&p).unwrap();
        assert!(!p.exists());

        let p = temp_file("slash");
        recycle(Path::new(&p.to_string_lossy().replace('\\', "/"))).unwrap();
        assert!(!p.exists());

        let p = temp_file("ext");
        let ext = std::fs::canonicalize(&p).unwrap();
        assert!(ext.to_string_lossy().starts_with(r"\\?\"));
        recycle(&ext).unwrap();
        assert!(!p.exists());
    }

    #[test]
    fn missing_file_is_an_error() {
        assert!(recycle(&std::env::temp_dir().join("ir-recycle-does-not-exist.mp4")).is_err());
    }
}
