//! Self-update mechanism for the Peri binary.
//!
//! Checks GitHub Releases for a newer version, downloads the tarball,
//! verifies its SHA256 checksum, extracts, and replaces the current binary.
//! Uses shell commands (curl, tar, sha256sum/shasum) — no extra Rust deps.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

const GITHUB_API: &str = "https://api.github.com/repos/konghayao/peri";
const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Run the full self-update flow. Returns Ok(Some(new_tag)) if updated, Ok(None) if already latest.
pub async fn run_self_update() -> Result<Option<String>> {
    let install_dir = install_dir();
    let current_tag = read_current_version(&install_dir);

    println!("Peri self-update");
    println!(
        "  Current version: {} ({})",
        PKG_VERSION,
        current_tag.as_deref().unwrap_or("unknown")
    );
    println!("  Install dir: {}", install_dir.display());

    // Detect platform
    let platform = detect_platform()?;
    println!("  Platform: {platform}");

    // Fetch latest release
    let (tag, assets) = fetch_latest_release().await?;
    println!("  Latest release: {tag}");

    // Check if already on latest
    if current_tag.as_deref() == Some(&tag) {
        println!("\nAlready up to date ({tag}).");
        return Ok(None);
    }

    // Find matching asset
    let asset_name = format!("peri-{platform}.tar.gz");
    let download_url = assets
        .iter()
        .find_map(|a| {
            if a.name == asset_name {
                Some(a.browser_download_url.clone())
            } else {
                None
            }
        })
        .with_context(|| format!("No asset found for platform '{platform}' in release {tag}"))?;

    // Find checksums URL
    let checksums_url = assets.iter().find_map(|a| {
        if a.name == "checksums.txt" {
            Some(a.browser_download_url.clone())
        } else {
            None
        }
    });

    // Create version directory
    let version_dir = install_dir.join(&tag);
    std::fs::create_dir_all(&version_dir)
        .with_context(|| format!("Failed to create directory: {}", version_dir.display()))?;

    // Download tarball
    println!("\nDownloading {asset_name}...");
    let tarball_path = version_dir.join(&asset_name);
    download_with_curl(&download_url, &tarball_path)?;
    println!("  Downloaded to {}", tarball_path.display());

    // Verify checksum
    if let Some(ref checksums_url) = checksums_url {
        println!("Verifying checksum...");
        let checksums_path = version_dir.join("checksums.txt");
        download_with_curl(checksums_url, &checksums_path)?;

        if !verify_checksum_shell(&checksums_path, &asset_name, &tarball_path)? {
            let _ = std::fs::remove_file(&tarball_path);
            let _ = std::fs::remove_file(&checksums_path);
            anyhow::bail!("Checksum verification FAILED. The downloaded file may be corrupted.");
        }
        let _ = std::fs::remove_file(&checksums_path);
        println!("  Checksum OK");
    }

    // Extract
    println!("Extracting...");
    extract_tarball_shell(&tarball_path, &version_dir)?;
    let _ = std::fs::remove_file(&tarball_path);

    let binary_path = version_dir.join("peri");

    // Make executable (Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&binary_path)
            .with_context(|| format!("Failed to stat {}", binary_path.display()))?
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary_path, perms)?;
    }

    println!("  Extracted to {}", binary_path.display());

    // Update symlink
    let symlink_path = install_dir.join("peri");
    let _ = std::fs::remove_file(&symlink_path);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&binary_path, &symlink_path)?;
    #[cfg(windows)]
    std::fs::copy(&binary_path, &symlink_path)?;

    // Write current version
    std::fs::write(install_dir.join("current-version.txt"), &tag)?;

    println!("\nUpdated to {tag}");
    println!("  Binary: {}", binary_path.display());
    println!("  Symlink: {} -> peri", symlink_path.display());
    println!("\nRun 'peri' to start the new version.");

    Ok(Some(tag))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn install_dir() -> PathBuf {
    dirs_next::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".peri")
}

fn read_current_version(install_dir: &Path) -> Option<String> {
    std::fs::read_to_string(install_dir.join("current-version.txt"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn detect_platform() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        "windows" => "windows",
        other => anyhow::bail!("Unsupported OS: {other}"),
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        "riscv64" => "riscv64",
        other => anyhow::bail!("Unsupported architecture: {other}"),
    };
    Ok(format!("{os}-{arch}"))
}

#[derive(serde::Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(serde::Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

async fn fetch_latest_release() -> Result<(String, Vec<GitHubAsset>)> {
    let client = reqwest::Client::new();
    let mut req = client
        .get(format!("{GITHUB_API}/releases?per_page=30"))
        .header("Accept", "application/vnd.github.v3+json")
        .header("User-Agent", "peri-cli");

    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        req = req.header("Authorization", format!("Bearer {token}"));
    }

    let releases: Vec<GitHubRelease> = req.send().await?.json().await?;

    let release = releases
        .into_iter()
        .find(|r| r.tag_name.starts_with("agent-"))
        .context("No agent release found")?;

    Ok((release.tag_name, release.assets))
}

/// Download a file using curl (no extra Rust HTTP deps needed for static binary).
/// Falls back to reqwest if curl is not available.
fn download_with_curl(url: &str, dest: &Path) -> Result<()> {
    // Apply GITHUB_PROXY if set
    let url = if let Ok(proxy) =
        std::env::var("PERI_GITHUB_PROXY").or_else(|_| std::env::var("GITHUB_PROXY"))
    {
        url.replace("https://github.com", &proxy)
    } else {
        url.to_string()
    };

    // Try curl first (most systems have it)
    let status = Command::new("curl")
        .args(["-fSL", "--progress-bar", &url, "-o"])
        .arg(dest)
        .status();

    match status {
        Ok(s) if s.success() => return Ok(()),
        _ => {} // fall through to reqwest
    }

    // Fallback: use reqwest (but requires tokio runtime — we're already in one)
    let rt = tokio::runtime::Handle::current();
    let bytes = rt.block_on(async {
        let client = reqwest::Client::new();
        let resp = client.get(&url).send().await?.bytes().await?;
        Ok::<_, anyhow::Error>(resp)
    })?;
    std::fs::write(dest, &bytes)?;
    Ok(())
}

/// Verify checksum using sha256sum or shasum command.
fn verify_checksum_shell(
    checksums_path: &Path,
    asset_name: &str,
    _tarball_path: &Path,
) -> Result<bool> {
    // Method 1: grep + sha256sum -c
    // cd to the directory containing the tarball, grep the checksum line, pipe to sha256sum -c
    let dir = checksums_path.parent().unwrap();
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd '{}' && grep -F '{}' '{}' | sha256sum -c --quiet 2>/dev/null",
            dir.display(),
            asset_name,
            checksums_path.file_name().unwrap().to_string_lossy(),
        ))
        .output();

    match output {
        Ok(o) if o.status.success() => return Ok(true),
        _ => {} // try shasum fallback
    }

    // Method 2: shasum -a 256 (macOS fallback, sha256sum not always present)
    let output = Command::new("sh")
        .arg("-c")
        .arg(format!(
            "cd '{}' && grep -F '{}' '{}' | shasum -a 256 -c 2>/dev/null",
            dir.display(),
            asset_name,
            checksums_path.file_name().unwrap().to_string_lossy(),
        ))
        .output();

    match output {
        Ok(o) if o.status.success() => Ok(true),
        Ok(_) => Ok(false), // command ran but verification failed
        Err(_) => {
            // Neither sha256sum nor shasum available — skip verification
            eprintln!("  Warning: sha256sum/shasum not found, skipping checksum verification");
            Ok(true)
        }
    }
}

/// Extract tar.gz using system tar command.
fn extract_tarball_shell(tarball_path: &Path, dest: &Path) -> Result<()> {
    let status = Command::new("tar")
        .args(["-xzf"])
        .arg(tarball_path)
        .arg("-C")
        .arg(dest)
        .status()
        .context("Failed to run tar. Is tar installed?")?;

    if !status.success() {
        anyhow::bail!("tar extraction failed");
    }
    Ok(())
}
