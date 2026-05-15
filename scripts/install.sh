#!/bin/bash
set -euo pipefail

# Peri Install Script
# Usage: curl -fsSL https://raw.githubusercontent.com/konghayao/peri/main/scripts/install.sh | bash
#
# Options:
#   PERI_INSTALL_VERSION   Specific version tag (e.g. agent-v1.17), empty = latest
#   PERI_INSTALL_DIR       Install directory (default: $HOME/.peri)
#   GITHUB_PROXY           GitHub download proxy prefix (replaces https://github.com in download URL)
#   GITHUB_TOKEN           GitHub personal access token (bypasses API rate limiting)
#   PERI_NO_PATH_HINT      Set to 1 to skip PATH hint
#   PERI_INSTALL_PLATFORM  Override platform detection (e.g. linux-x86_64, macos-aarch64)
#   PERI_SKIP_CHECKSUM     Set to 1 to skip SHA256 verification
#
# Example:
#   PERI_INSTALL_VERSION=agent-v1.17 bash install.sh
#   GITHUB_PROXY=https://ghproxy.com/https://github.com curl ... | bash
#   GITHUB_TOKEN=ghp_xxx curl ... | bash

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
NC='\033[0m'

info()    { echo -e "${GREEN}[INFO]${NC}  $*"; }
warn()    { echo -e "${YELLOW}[WARN]${NC}  $*"; }
error()   { echo -e "${RED}[ERROR]${NC} $*" >&2; }
step()    { echo -e "${CYAN}[STEP]${NC}  $*"; }

# --- Platform Detection ---
detect_platform() {
    local os arch platform

    # Allow manual override
    if [[ -n "${PERI_INSTALL_PLATFORM:-}" ]]; then
        # Validate format: os-arch
        if [[ ! "${PERI_INSTALL_PLATFORM}" =~ ^(macos|linux|windows)-(x86_64|aarch64|riscv64)$ ]]; then
            error "Invalid PERI_INSTALL_PLATFORM: ${PERI_INSTALL_PLATFORM}"
            echo "  Expected: macos-x86_64 | macos-aarch64 | linux-x86_64 | linux-aarch64 | linux-riscv64 | windows-x86_64"
            exit 1
        fi
        info "Platform (manual): ${PERI_INSTALL_PLATFORM}"
        echo "${PERI_INSTALL_PLATFORM}"
        return
    fi

    case "$(uname -s)" in
        Darwin)  os="macos" ;;
        Linux)   os="linux" ;;
        *)       error "Unsupported OS: $(uname -s)"; exit 1 ;;
    esac

    case "$(uname -m)" in
        x86_64|amd64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        riscv64)       arch="riscv64" ;;
        *)             error "Unsupported arch: $(uname -m)"; exit 1 ;;
    esac

    platform="${os}-${arch}"
    info "Detected platform: ${platform}"
    echo "${platform}"
}

# --- Download with optional proxy ---
get_download_url() {
    local url="$1"
    local proxy="${GITHUB_PROXY:-}"
    if [[ -n "${proxy}" ]]; then
        echo "${url/https:\/\/github.com/${proxy}}"
    else
        echo "${url}"
    fi
}

# --- GitHub API request (with optional token) ---
github_api() {
    local url="$1"
    local auth_header=""
    if [[ -n "${GITHUB_TOKEN:-}" ]]; then
        auth_header="-H Authorization: Bearer ${GITHUB_TOKEN}"
    fi
    curl -fsSL ${auth_header:-} "${url}" 2>/dev/null
}

# --- Main ---
main() {
    INSTALL_DIR="${PERI_INSTALL_DIR:-${HOME}/.peri}"
    GITHUB_API="https://api.github.com/repos/konghayao/peri"

    echo ""
    info "Peri Agent Installer"
    info "-------------------------------"

    PLATFORM=$(detect_platform)
    ASSET_NAME="peri-${PLATFORM}.tar.gz"

    # Fetch release info
    if [[ -n "${PERI_INSTALL_VERSION:-}" ]]; then
        VERSION_TAG="${PERI_INSTALL_VERSION}"
        step "Fetching release: ${VERSION_TAG}..."
        RELEASE_JSON=$(github_api "${GITHUB_API}/releases/tags/${VERSION_TAG}") || {
            error "Failed to fetch release '${VERSION_TAG}'. Does this tag exist?"
            exit 1
        }
    else
        step "Fetching latest agent release..."
        RELEASES_JSON=$(github_api "${GITHUB_API}/releases?per_page=30") || {
            error "Failed to fetch releases from GitHub."
            exit 1
        }
        # Find latest agent-* tag
        VERSION_TAG=$(echo "${RELEASES_JSON}" | grep -oE '"tag_name": *"agent-[^"]+"' | head -1 | cut -d'"' -f4)
        if [[ -z "${VERSION_TAG}" ]]; then
            error "No agent release found."
            exit 1
        fi

        # Fetch the specific release for asset list
        RELEASE_JSON=$(github_api "${GITHUB_API}/releases/tags/${VERSION_TAG}") || {
            error "Failed to fetch release '${VERSION_TAG}'."
            exit 1
        }
    fi

    info "Found release: ${VERSION_TAG}"

    # Find matching asset
    ASSET_DOWNLOAD_URL=$(echo "${RELEASE_JSON}" | grep -oE "\"browser_download_url\": *\"[^\"]*${ASSET_NAME}[^\"]*\"" | head -1 | cut -d'"' -f4)

    if [[ -z "${ASSET_DOWNLOAD_URL}" ]]; then
        error "No binary found for platform '${PLATFORM}'."
        echo ""
        echo "Available assets:"
        echo "${RELEASE_JSON}" | grep -oE '"browser_download_url": *"[^"]+"' | sed 's/"browser_download_url": *"/  - /;s/"//'
        exit 1
    fi

    info "Binary: ${ASSET_NAME}"

    # Create install directory
    VERSION_DIR="${INSTALL_DIR}/${VERSION_TAG}"
    mkdir -p "${VERSION_DIR}"

    TARGET="${VERSION_DIR}/peri"
    TARBALL="${VERSION_DIR}/${ASSET_NAME}"

    # Download tarball
    FINAL_URL=$(get_download_url "${ASSET_DOWNLOAD_URL}")
    if [[ "${FINAL_URL}" != "${ASSET_DOWNLOAD_URL}" ]]; then
        info "Using proxy: ${FINAL_URL}"
    fi

    step "Downloading..."
    curl -fSL --progress-bar "${FINAL_URL}" -o "${TARBALL}" || {
        error "Download failed."
        exit 1
    }

    # --- SHA256 Verification ---
    if [[ "${PERI_SKIP_CHECKSUM:-}" != "1" ]]; then
        step "Verifying checksum..."

        # Find checksums.txt download URL from the same release
        CHECKSUMS_URL=$(echo "${RELEASE_JSON}" | grep -oE "\"browser_download_url\": *\"[^\"]*checksums\.txt[^\"]*\"" | head -1 | cut -d'"' -f4)
        CHECKSUMS_FILE="${VERSION_DIR}/checksums.txt"

        if [[ -n "${CHECKSUMS_URL}" ]]; then
            CHECKSUMS_FINAL=$(get_download_url "${CHECKSUMS_URL}")
            curl -fsSL "${CHECKSUMS_FINAL}" -o "${CHECKSUMS_FILE}" 2>/dev/null || {
                warn "Failed to download checksums.txt, skipping verification."
            }

            if [[ -f "${CHECKSUMS_FILE}" ]]; then
                # Extract just the line for our tarball and verify
                pushd "${VERSION_DIR}" > /dev/null
                if grep -F "${ASSET_NAME}" "${CHECKSUMS_FILE}" | sha256sum -c --quiet 2>/dev/null; then
                    info "Checksum verified OK"
                else
                    error "Checksum verification FAILED! The downloaded file may be corrupted."
                    error "Expected:"
                    grep -F "${ASSET_NAME}" "${CHECKSUMS_FILE}" || echo "  (no checksum entry found for ${ASSET_NAME})"
                    error "Got:"
                    sha256sum "${ASSET_NAME}" 2>/dev/null || shasum -a 256 "${ASSET_NAME}" 2>/dev/null
                    rm -f "${TARBALL}"
                    exit 1
                fi
                popd > /dev/null
                rm -f "${CHECKSUMS_FILE}"
            fi
        else
            warn "No checksums.txt found in release, skipping verification."
        fi
    fi

    # Extract tarball
    step "Extracting..."
    tar -xzf "${TARBALL}" -C "${VERSION_DIR}" || {
        error "Extraction failed."
        exit 1
    }
    rm -f "${TARBALL}"

    # Make executable
    chmod +x "${TARGET}"
    info "Installed to: ${TARGET}"

    # Create symlink for convenience
    LINK="${INSTALL_DIR}/peri"
    rm -f "${LINK}"
    ln -sf "${TARGET}" "${LINK}"

    # Write current version
    echo "${VERSION_TAG}" > "${INSTALL_DIR}/current-version.txt"

    # --- PATH Check ---
    if [[ "${PERI_NO_PATH_HINT:-}" != "1" ]]; then
        BIN_LINK="${INSTALL_DIR}/peri"
        SHELL_PROFILE=""
        case "${SHELL:-}" in
            */zsh)  SHELL_PROFILE="${HOME}/.zshrc" ;;
            */bash) SHELL_PROFILE="${HOME}/.bashrc" ;;
            */fish) SHELL_PROFILE="${HOME}/.config/fish/config.fish" ;;
        esac

        if [[ -n "${SHELL_PROFILE}" ]] && ! grep -qF "${INSTALL_DIR}" "${SHELL_PROFILE}" 2>/dev/null; then
            echo ""
            warn "The install directory is not in your PATH."
            echo ""
            echo "  To add it now, run:"
            echo ""
            if [[ "${SHELL}" == */fish ]]; then
                echo "    echo 'set -gx PATH ${INSTALL_DIR} \$PATH' >> ${SHELL_PROFILE}"
            else
                echo "    echo 'export PATH=\"${INSTALL_DIR}:\$PATH\"' >> ${SHELL_PROFILE}"
            fi
            echo "    source ${SHELL_PROFILE}"
            echo ""
        elif [[ -z "${SHELL_PROFILE}" ]]; then
            echo ""
            warn "Unknown shell. Add this directory to your PATH manually:"
            echo "    ${INSTALL_DIR}"
            echo ""
        fi
    fi

    echo ""
    info "Installation complete! Version: ${VERSION_TAG}"
    echo ""

    if command -v "${BIN_LINK}" &>/dev/null || [[ -x "${BIN_LINK}" ]]; then
        info "Run 'peri' to start."
    else
        info "Run: ${BIN_LINK}"
    fi
    echo ""
}

main
