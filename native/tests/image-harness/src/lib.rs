#![allow(dead_code)]

#[path = "../../../src/atomic_file.rs"]
mod atomic_file;

mod images {
    // Linux has no production desktop target. Supply only the same filesystem guard
    // used by macOS, then compile the unchanged production module and its tests.
    #[cfg(target_os = "linux")]
    fn is_reparse_point(path: &std::path::Path) -> bool {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata.file_type().is_symlink(),
            Err(error) => error.kind() != std::io::ErrorKind::NotFound,
        }
    }

    include!("../../../src/images.rs");
}
