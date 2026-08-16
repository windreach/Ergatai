//! Build script to download rmux-daemon binary for bundling with ergatai.
//!
//! This script downloads pre-built rmux-daemon binaries from GitHub releases
//! and places them in the `resources/{platform}/` directory next to the crate.
//!
//! The downloaded binaries are used at runtime by `ergatai-binary` to locate
//! the rmux-daemon without requiring users to install it separately.
//!
//! # Supply-chain hardening
//!
//! Every download is SHA-256 verified against the pinned hashes in
//! `RMUX_SHA256` before extraction. Archive entries are validated against
//! path traversal (no `..` components, no absolute paths, no symlinks).

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// rmux version to download
const RMUX_VERSION: &str = "0.10.0";

/// GitHub release download URL template
const RMUX_DOWNLOAD_URL: &str = "https://github.com/Helvesec/rmux/releases/download";

/// Pinned SHA-256 hashes for rmux release archives, keyed by archive filename.
///
/// Regenerate with:
/// ```sh
/// curl -sL https://github.com/Helvesec/rmux/releases/download/v<VER>/<archive> | sha256sum
/// ```
///
/// When bumping `RMUX_VERSION`, update these hashes by re-downloading the new
/// release's archives and replacing the values below.
const RMUX_SHA256: &[(&str, &str)] = &[
    (
        "rmux-0.10.0-linux-x86_64.tar.gz",
        "1bec11eff08c3313c3a400196e7a93d00b8ad4a24f81ef13debb03355c2696c5",
    ),
    (
        "rmux-0.10.0-linux-aarch64.tar.gz",
        "7e916560ea0fb90864b8c24e5d0f81b4e3e0b013b8aad5ab53839d7e8e5e1926",
    ),
    (
        "rmux-0.10.0-macos-x86_64.tar.gz",
        "b897898eadc4d96c6d555b79affd834bd488013c44f8c6f815bb5195eafd1e0a",
    ),
    (
        "rmux-0.10.0-macos-aarch64.tar.gz",
        "aac857519071f680be53aa9a328dc0cd04c2abe66ec726f78aa9e26337c5ef7b",
    ),
    (
        "rmux-0.10.0-windows-x86_64.zip",
        "e315e2d51d927ba9621732812c0f932c862d05f4b677dbf3cab76f0d27372a70",
    ),
];

fn main() {
    // Only run on ergatai-api builds (the main binary that needs rmux)
    // Skip for library-only builds to avoid unnecessary downloads
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=ERGATAI_SKIP_RMUX_DOWNLOAD");

    // Allow skipping download for development/testing
    if env::var("ERGATAI_SKIP_RMUX_DOWNLOAD").is_ok() {
        println!("cargo:warning=Skipping rmux-daemon download (ERGATAI_SKIP_RMUX_DOWNLOAD set)");
        return;
    }

    let target = env::var("TARGET").unwrap_or_else(|_| env::var("HOST").unwrap_or_default());
    let (platform, archive_name, binary_name) = match_platform(&target);

    let out_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let resources_dir = out_dir.join("resources").join(platform);

    let binary_path = resources_dir.join(binary_name);

    // Skip if already exists
    if binary_path.exists() {
        println!(
            "cargo:warning=rmux binary already exists at {}",
            binary_path.display()
        );
        return;
    }

    // Create resources directory
    if let Err(e) = fs::create_dir_all(&resources_dir) {
        println!("cargo:warning=Failed to create resources dir: {}", e);
        return;
    }

    let download_url = format!("{}/v{}/{}", RMUX_DOWNLOAD_URL, RMUX_VERSION, archive_name);

    println!("cargo:warning=Downloading rmux from {}", download_url);

    match download_and_extract(&download_url, &archive_name, &resources_dir, binary_name) {
        Ok(path) => {
            println!("cargo:warning=rmux binary installed to {}", path.display());
            // Make executable on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
            }
        }
        Err(e) => {
            println!(
                "cargo:warning=Failed to download rmux-daemon: {}. \
                 Ergatai will fall back to tmux backend or system rmux-daemon.",
                e
            );
        }
    }
}

fn match_platform(target: &str) -> (&'static str, String, &'static str) {
    let version = RMUX_VERSION;
    // Binary name must match what finder.rs searches for at runtime
    // (BinaryLocator { name: "rmux", ... } → binary_file_name("rmux"))
    if target.contains("linux") {
        if target.contains("aarch64") || target.contains("arm64") {
            (
                "linux-arm64",
                format!("rmux-{}-linux-aarch64.tar.gz", version),
                "rmux",
            )
        } else {
            (
                "linux-x86_64",
                format!("rmux-{}-linux-x86_64.tar.gz", version),
                "rmux",
            )
        }
    } else if target.contains("darwin") || target.contains("macos") {
        if target.contains("aarch64") || target.contains("arm64") {
            (
                "darwin-arm64",
                format!("rmux-{}-macos-aarch64.tar.gz", version),
                "rmux",
            )
        } else {
            (
                "darwin-x86_64",
                format!("rmux-{}-macos-x86_64.tar.gz", version),
                "rmux",
            )
        }
    } else if target.contains("windows") {
        (
            "win32-x86_64",
            format!("rmux-{}-windows-x86_64.zip", version),
            "rmux.exe",
        )
    } else {
        // Fallback - try Linux x86_64
        (
            "linux-x86_64",
            format!("rmux-{}-linux-x86_64.tar.gz", version),
            "rmux",
        )
    }
}

/// Look up the pinned SHA-256 for a given archive name.
fn expected_sha256(archive_name: &str) -> Option<&'static str> {
    RMUX_SHA256
        .iter()
        .find(|(name, _)| *name == archive_name)
        .map(|(_, hash)| *hash)
}

/// Tiny in-crate hex encoder so we don't need a `hex` build-dependency just
/// for one call site.
mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let bytes = bytes.as_ref();
        let mut out = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            out.push(HEX[(b >> 4) as usize] as char);
            out.push(HEX[(b & 0x0f) as usize] as char);
        }
        out
    }
}

/// Compute the lowercase hex SHA-256 digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(data);
    hex::encode(digest)
}

/// Verify the SHA-256 of `data` against the pinned hash for `archive_name`.
/// Fails the build if no pin exists for the archive or the hash mismatches.
fn verify_integrity(archive_name: &str, data: &[u8]) -> Result<(), String> {
    let expected = expected_sha256(archive_name).ok_or_else(|| {
        format!(
            "no SHA-256 pin for archive '{}' — add it to RMUX_SHA256 in build.rs",
            archive_name
        )
    })?;
    let actual = sha256_hex(data);
    if actual != expected {
        return Err(format!(
            "SHA-256 mismatch for {}:\n  expected: {}\n  actual:   {}\n\
             Refusing to extract. If this is an intentional rmux version bump, \
             update RMUX_SHA256 in build.rs.",
            archive_name, expected, actual
        ));
    }
    Ok(())
}

fn download_and_extract(
    url: &str,
    archive_name: &str,
    dest_dir: &Path,
    binary_name: &str,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // Download using ureq (simple HTTP client)
    let response = ureq::get(url).call()?;

    let mut bytes = Vec::new();
    response.into_reader().read_to_end(&mut bytes)?;

    // Verify integrity before touching disk.
    if let Err(msg) = verify_integrity(archive_name, &bytes) {
        return Err(msg.into());
    }

    let dest_path = dest_dir.join(binary_name);

    if url.ends_with(".tar.gz") || url.ends_with(".tgz") {
        // Extract full archive (preserves bin/, libexec/, share/ structure
        // that rmux needs at runtime), then copy the CLI binary to dest_path.
        extract_tar_gz(&bytes, dest_dir)?;
        copy_binary_from_extracted(dest_dir, binary_name, &dest_path)?;
    } else if url.ends_with(".zip") {
        extract_zip(&bytes, dest_dir, binary_name)?;
    } else {
        // Assume raw binary
        fs::write(&dest_path, &bytes)?;
    }

    Ok(dest_path)
}

/// After extracting a tar.gz archive, locate the CLI binary inside the
/// versioned directory (e.g., `rmux-0.10.0-linux-x86_64/bin/rmux`) and copy
/// it to `dest_path` so the runtime finder can locate it at a stable path.
fn copy_binary_from_extracted(
    dest_dir: &Path,
    binary_name: &str,
    dest_path: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    // Find the top-level archive directory (e.g., rmux-0.10.0-linux-x86_64)
    let archive_dir = fs::read_dir(dest_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.is_dir());

    let archive_dir = archive_dir.ok_or("No directory found in extracted archive")?;

    // Check for binary in bin/ subdir first, then at archive root
    let candidates = [
        archive_dir.join("bin").join(binary_name),
        archive_dir.join(binary_name),
    ];

    for source in &candidates {
        if source.exists() {
            fs::copy(source, dest_path)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(dest_path, fs::Permissions::from_mode(0o755));
            }
            return Ok(());
        }
    }

    Err(format!(
        "Binary '{}' not found in archive (searched: {})",
        binary_name,
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
    .into())
}

/// Returns `true` if `entry_name` is a safe path to extract under `dest_dir`.
///
/// Rejects:
/// - absolute paths (`/etc/passwd`, `C:\Windows\...`)
/// - paths containing `..` components (`foo/../../etc/passwd`)
/// - empty paths
fn is_safe_archive_path(entry_name: &str) -> bool {
    if entry_name.is_empty() {
        return false;
    }
    let path = Path::new(entry_name);
    if path.is_absolute() {
        return false;
    }
    for component in path.components() {
        if matches!(component, std::path::Component::ParentDir) {
            return false;
        }
    }
    true
}

fn extract_tar_gz(data: &[u8], dest_dir: &Path) -> Result<(), Box<dyn std::error::Error>> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let decoder = GzDecoder::new(data);
    let mut archive = Archive::new(decoder);

    // Iterate entries manually so we can reject suspicious paths
    // (defense-in-depth against tar-slip / path-traversal attacks).
    for entry in archive.entries()? {
        let mut entry = entry?;
        let raw_path = entry.path()?.into_owned();
        let entry_name = raw_path.to_string_lossy().into_owned();

        if !is_safe_archive_path(&entry_name) {
            return Err(format!(
                "refusing to extract tar entry with unsafe path: {:?}",
                entry_name
            )
            .into());
        }

        let dest_path = dest_dir.join(&raw_path);

        // Extract based on entry type
        let entry_type = entry.header().entry_type();
        if entry_type.is_dir() {
            fs::create_dir_all(&dest_path)?;
        } else if entry_type.is_file() {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut out_file = fs::File::create(&dest_path)?;
            io::copy(&mut entry, &mut out_file)?;
            // Preserve executable bit on Unix
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = entry.header().mode().unwrap_or(0o644);
                let _ = fs::set_permissions(&dest_path, fs::Permissions::from_mode(mode));
            }
        } else if entry_type.is_symlink() {
            // Reject symlinks — rmux archives shouldn't need them, and they're
            // a classic path-traversal vector.
            return Err(format!("refusing to extract symlink tar entry: {:?}", entry_name).into());
        }
        // Skip other entry types (hardlinks, char/block devices, etc.)
    }

    Ok(())
}

fn extract_zip(
    data: &[u8],
    dest_dir: &Path,
    binary_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::io::Cursor;
    use zip::ZipArchive;

    let cursor = Cursor::new(data);
    let mut archive = ZipArchive::new(cursor)?;

    // Determine the base name without extension for matching.
    // binary_name is "rmux.exe" on Windows, "rmux" elsewhere.
    let base_name = binary_name.strip_suffix(".exe").unwrap_or(binary_name);

    for i in 0..archive.len() {
        let mut file = archive.by_index(i)?;
        let name = file.name().to_string();

        // Validate path before touching disk.
        if !is_safe_archive_path(&name) {
            return Err(
                format!("refusing to extract zip entry with unsafe path: {:?}", name).into(),
            );
        }

        // Match the CLI binary (not rmux-daemon) by filename.
        // Archive entries may be "rmux.exe", "rmux", "bin/rmux.exe", etc.
        let entry_base = std::path::Path::new(&name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("");

        if entry_base == base_name && !name.contains("rmux-daemon") {
            let dest_path = dest_dir.join(binary_name);
            let mut out_file = fs::File::create(&dest_path)?;
            io::copy(&mut file, &mut out_file)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = fs::set_permissions(&dest_path, fs::Permissions::from_mode(0o755));
            }
            return Ok(());
        }
    }

    Err(format!("{} binary not found in zip archive", base_name).into())
}
