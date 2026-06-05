#!/bin/bash

# Seaport Installation Script
# Downloads and installs the Seaport CLI for your system.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/hyperterse/seaport/main/install.sh | bash
#
# Environment variables:
#   VERSION      - Version to install, without leading "v" (default: latest)
#   INSTALL_DIR  - Installation directory (default: ~/.local/bin)
#   BASE_URL     - Release base URL (default: GitHub releases)
#
# Release artifact naming:
#   seaport-<version>-<rust-target>.tar.gz

set -e

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

BINARY_NAME="seaport"
INSTALL_DIR="${INSTALL_DIR:-$HOME/.local/bin}"
VERSION="${VERSION:-latest}"
BASE_URL="${BASE_URL:-https://github.com/hyperterse/seaport/releases}"

info() {
    printf "%b\n" "${BLUE}i${NC} $1"
}

success() {
    printf "%b\n" "${GREEN}ok${NC} $1"
}

warning() {
    printf "%b\n" "${YELLOW}!${NC} $1"
}

error() {
    printf "%b\n" "${RED}x${NC} $1" >&2
}

command_exists() {
    command -v "$1" >/dev/null 2>&1
}

detect_target() {
    local os
    local arch

    os="$(uname -s | tr '[:upper:]' '[:lower:]')"
    arch="$(uname -m | tr '[:upper:]' '[:lower:]')"

    case "$os" in
        linux*)
            case "$arch" in
                x86_64|amd64)
                    echo "x86_64-unknown-linux-gnu"
                    ;;
                *)
                    error "Unsupported Linux architecture: $arch"
                    exit 1
                    ;;
            esac
            ;;
        darwin*)
            case "$arch" in
                x86_64|amd64)
                    echo "x86_64-apple-darwin"
                    ;;
                arm64|aarch64)
                    echo "aarch64-apple-darwin"
                    ;;
                *)
                    error "Unsupported macOS architecture: $arch"
                    exit 1
                    ;;
            esac
            ;;
        msys*|cygwin*|mingw*)
            case "$arch" in
                x86_64|amd64)
                    echo "x86_64-pc-windows-msvc"
                    ;;
                *)
                    error "Unsupported Windows architecture: $arch"
                    exit 1
                    ;;
            esac
            ;;
        *)
            error "Unsupported operating system: $os"
            exit 1
            ;;
    esac
}

binary_name_for_target() {
    case "$1" in
        *windows*)
            echo "${BINARY_NAME}.exe"
            ;;
        *)
            echo "$BINARY_NAME"
            ;;
    esac
}

latest_version() {
    local api_url
    local latest_tag

    api_url="${BASE_URL%/releases}/releases/latest"

    if command_exists curl; then
        latest_tag="$(curl -fsSL "$api_url" | grep '"tag_name":' | sed -E 's/.*"tag_name": "([^"]+)".*/\1/' | head -1)"
    elif command_exists wget; then
        latest_tag="$(wget -qO- "$api_url" | grep '"tag_name":' | sed -E 's/.*"tag_name": "([^"]+)".*/\1/' | head -1)"
    else
        error "Neither curl nor wget is installed. Please install one of them."
        exit 1
    fi

    if [ -z "$latest_tag" ]; then
        error "Could not resolve latest Seaport release."
        exit 1
    fi

    echo "${latest_tag#v}"
}

download_url() {
    local version=$1
    local target=$2

    echo "${BASE_URL}/download/v${version}/seaport-${version}-${target}.tar.gz"
}

download_file() {
    local url=$1
    local output=$2

    info "Downloading $url"

    if command_exists curl; then
        curl -fL --progress-bar -o "$output" "$url"
    elif command_exists wget; then
        wget -q --show-progress -O "$output" "$url"
    else
        error "Neither curl nor wget is installed. Please install one of them."
        exit 1
    fi
}

download_text() {
    local url=$1

    if command_exists curl; then
        curl -fsSL "$url"
    elif command_exists wget; then
        wget -qO- "$url"
    else
        return 1
    fi
}

sha256_of() {
    local file=$1

    if command_exists sha256sum; then
        sha256sum "$file" | awk '{print $1}'
    elif command_exists shasum; then
        shasum -a 256 "$file" | awk '{print $1}'
    else
        return 1
    fi
}

verify_checksum() {
    local version=$1
    local archive=$2
    local archive_name=$3
    local checksums_url
    local expected
    local actual

    checksums_url="${BASE_URL}/download/v${version}/checksums.txt"

    if ! command_exists sha256sum && ! command_exists shasum; then
        warning "No checksum tool found; skipping checksum verification."
        return 0
    fi

    if ! expected="$(download_text "$checksums_url" | awk -v name="$archive_name" '$0 ~ name {print $1; exit}')"; then
        warning "Could not download checksums; skipping checksum verification."
        return 0
    fi

    if [ -z "$expected" ]; then
        warning "No checksum found for $archive_name; skipping checksum verification."
        return 0
    fi

    actual="$(sha256_of "$archive")"

    if [ "$actual" != "$expected" ]; then
        error "Checksum mismatch for $archive_name"
        error "Expected: $expected"
        error "Actual:   $actual"
        exit 1
    fi

    success "Checksum verified."
}

add_to_path() {
    local install_dir=$1
    local shell_config=""
    local path_line=""

    if [ -n "$ZSH_VERSION" ] || [ -n "$ZSH" ]; then
        shell_config="${HOME}/.zshrc"
        path_line="export PATH=\"\$PATH:$install_dir\""
    elif [ -n "$BASH_VERSION" ]; then
        if [ -f "${HOME}/.bash_profile" ]; then
            shell_config="${HOME}/.bash_profile"
        else
            shell_config="${HOME}/.bashrc"
        fi
        path_line="export PATH=\"\$PATH:$install_dir\""
    else
        case "$SHELL" in
            *zsh*)
                shell_config="${HOME}/.zshrc"
                path_line="export PATH=\"\$PATH:$install_dir\""
                ;;
            *bash*)
                if [ -f "${HOME}/.bash_profile" ]; then
                    shell_config="${HOME}/.bash_profile"
                else
                    shell_config="${HOME}/.bashrc"
                fi
                path_line="export PATH=\"\$PATH:$install_dir\""
                ;;
            *fish*)
                shell_config="${HOME}/.config/fish/config.fish"
                path_line="set -gx PATH \$PATH $install_dir"
                ;;
        esac
    fi

    if [ -z "$shell_config" ]; then
        return 1
    fi

    if printf "%s" "$PATH" | grep -Fq "$install_dir"; then
        return 0
    fi

    if [ -f "$shell_config" ] && grep -Fq "$install_dir" "$shell_config" 2>/dev/null; then
        return 0
    fi

    mkdir -p "$(dirname "$shell_config")"
    touch "$shell_config"

    {
        echo ""
        echo "# Seaport"
        echo "$path_line"
    } >> "$shell_config"

    return 0
}

install_binary() {
    local binary_path=$1
    local binary_name=$2
    local install_path="$INSTALL_DIR/$binary_name"

    mkdir -p "$INSTALL_DIR"
    cp "$binary_path" "$install_path"
    chmod +x "$install_path"

    success "Installed $binary_name to $install_path"

    if printf "%s" "$PATH" | grep -Fq "$INSTALL_DIR"; then
        success "Installation complete. Run 'seaport --help' to get started."
        return 0
    fi

    info "Adding Seaport to PATH..."

    if add_to_path "$INSTALL_DIR"; then
        success "Added Seaport to your shell configuration."
        info "For this terminal session, run:"
        printf "  %b\n" "${GREEN}export PATH=\"\$PATH:$INSTALL_DIR\"${NC}"
    else
        warning "Could not automatically add Seaport to PATH."
        info "Add this to your shell profile:"
        printf "  %b\n" "${GREEN}export PATH=\"\$PATH:$INSTALL_DIR\"${NC}"
        info "Or run Seaport directly:"
        printf "  %b\n" "${GREEN}$install_path${NC}"
    fi
}

main() {
    local version
    local target
    local binary_name
    local archive_name
    local archive_path
    local download
    local temp_dir
    local extracted_binary

    echo ""
    info "Seaport Installation Script"
    echo ""

    target="$(detect_target)"
    binary_name="$(binary_name_for_target "$target")"

    if [ "$VERSION" = "latest" ]; then
        version="$(latest_version)"
    else
        version="${VERSION#v}"
    fi

    archive_name="seaport-${version}-${target}.tar.gz"
    download="$(download_url "$version" "$target")"

    info "Version: $version"
    info "Target:  $target"
    echo ""

    temp_dir="$(mktemp -d)"
    trap 'rm -rf "$temp_dir"' EXIT

    archive_path="$temp_dir/$archive_name"

    if ! download_file "$download" "$archive_path"; then
        error "Failed to download Seaport from $download"
        echo ""
        info "This might mean:"
        echo "  1. The binary for your platform is not available"
        echo "  2. The version does not exist"
        echo "  3. There is a network connectivity issue"
        echo ""
        info "You can build from source instead:"
        echo "  git clone https://github.com/hyperterse/seaport.git"
        echo "  cd seaport && cargo install --path ."
        exit 1
    fi

    verify_checksum "$version" "$archive_path" "$archive_name"

    tar -xzf "$archive_path" -C "$temp_dir"
    extracted_binary="$(find "$temp_dir" -type f -name "$binary_name" | head -1)"

    if [ -z "$extracted_binary" ] || [ ! -s "$extracted_binary" ]; then
        error "Archive did not contain $binary_name"
        exit 1
    fi

    if [ -t 0 ] && [ -z "$SKIP_INSTALL_PROMPT" ]; then
        printf "Install to %s? [Y/n]: " "$INSTALL_DIR"
        read -r response
        if printf "%s" "$response" | grep -Eq '^[Nn]$'; then
            info "Downloaded archive: $archive_path"
            exit 0
        fi
    else
        info "Installing to $INSTALL_DIR..."
    fi

    install_binary "$extracted_binary" "$binary_name"

    echo ""
    success "Seaport installed successfully."
    info "Get started by running:"
    printf "  %b\n" "${GREEN}seaport --help${NC}"
    echo ""
}

main "$@"
