#!/bin/sh
# Ferrix installer.
#
#   curl -fsSL https://arthurlunkes.github.io/ferrix/install.sh | sh
#
# Picks the native way for your system: the APT repository on Debian/Ubuntu (so
# `apt upgrade` keeps Ferrix up to date), the .rpm on Fedora/RHEL/openSUSE, the
# Arch package on Arch-based systems, and the plain binary plus a systemd unit
# anywhere else. Every download is checked against the release's SHA256SUMS.
#
# Options (environment variables):
#   FERRIX_VERSION=0.2.0   install this version instead of the latest
#   FERRIX_METHOD=binary   skip the package manager and install the plain binary
set -eu

REPO="arthurlunkes/ferrix"
SITE="https://arthurlunkes.github.io/ferrix"

say() { printf '\033[1;33m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

[ "$(uname -s)" = "Linux" ] || die "this installer is for Linux; see https://github.com/$REPO/releases for other systems"

if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
elif have sudo; then
    SUDO="sudo"
else
    die "run as root or install sudo"
fi

if have curl; then
    fetch() { curl -fsSL "$1" -o "$2"; }
    fetch_text() { curl -fsSL "$1"; }
elif have wget; then
    fetch() { wget -qO "$2" "$1"; }
    fetch_text() { wget -qO- "$1"; }
else
    die "curl or wget is required"
fi

case "$(uname -m)" in
    x86_64 | amd64) DEB_ARCH=amd64; RPM_ARCH=x86_64; TRIPLE=x86_64-unknown-linux-musl ;;
    aarch64 | arm64) DEB_ARCH=arm64; RPM_ARCH=aarch64; TRIPLE=aarch64-unknown-linux-musl ;;
    *) die "unsupported CPU architecture: $(uname -m)" ;;
esac

VERSION="${FERRIX_VERSION:-}"
if [ -z "$VERSION" ]; then
    VERSION=$(fetch_text "https://api.github.com/repos/$REPO/releases/latest" |
        sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$VERSION" ] || die "could not find the latest Ferrix version"
fi
BASE="https://github.com/$REPO/releases/download/v$VERSION"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT INT TERM

# Downloads one release file into $TMP and checks it against SHA256SUMS.
download() {
    [ -f "$TMP/SHA256SUMS" ] || fetch "$BASE/SHA256SUMS" "$TMP/SHA256SUMS" ||
        die "could not download the checksums of v$VERSION"
    say "Downloading $1"
    fetch "$BASE/$1" "$TMP/$1" || die "could not download $1"
    expected=$(grep " $1\$" "$TMP/SHA256SUMS" | cut -d ' ' -f 1)
    [ -n "$expected" ] || die "$1 is not listed in SHA256SUMS"
    if have sha256sum; then
        actual=$(sha256sum "$TMP/$1" | cut -d ' ' -f 1)
    else
        actual=$(shasum -a 256 "$TMP/$1" | cut -d ' ' -f 1)
    fi
    [ "$expected" = "$actual" ] || die "checksum mismatch for $1"
}

install_apt() {
    say "Adding the Ferrix APT repository"
    fetch "$SITE/apt/ferrix-archive-keyring.gpg" "$TMP/keyring.gpg" || return 1
    $SUDO install -d -m 0755 /usr/share/keyrings
    $SUDO install -m 0644 "$TMP/keyring.gpg" /usr/share/keyrings/ferrix-archive-keyring.gpg
    echo "deb [signed-by=/usr/share/keyrings/ferrix-archive-keyring.gpg] $SITE/apt stable main" |
        $SUDO tee /etc/apt/sources.list.d/ferrix.list >/dev/null
    $SUDO apt-get update -o Dir::Etc::sourcelist=sources.list.d/ferrix.list \
        -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 || return 1
    if [ -n "${FERRIX_VERSION:-}" ]; then
        $SUDO apt-get install -y "ferrix=$VERSION"
    else
        $SUDO apt-get install -y ferrix
    fi
}

install_deb_file() {
    file="ferrix_${VERSION}_${DEB_ARCH}.deb"
    download "$file"
    $SUDO apt-get install -y "$TMP/$file"
}

install_rpm() {
    file="ferrix-${VERSION}-1.${RPM_ARCH}.rpm"
    download "$file"
    if have dnf; then
        $SUDO dnf install -y "$TMP/$file"
    elif have zypper; then
        $SUDO zypper --non-interactive install --allow-unsigned-rpm "$TMP/$file"
    else
        $SUDO yum install -y "$TMP/$file"
    fi
}

install_arch() {
    file="ferrix-${VERSION}-1-${RPM_ARCH}.pkg.tar.zst"
    download "$file"
    $SUDO pacman -U --noconfirm "$TMP/$file"
}

install_binary() {
    archive="ferrix-v${VERSION}-${TRIPLE}.tar.gz"
    download "$archive"
    tar -xzf "$TMP/$archive" -C "$TMP"
    dir="$TMP/ferrix-v${VERSION}-${TRIPLE}"
    $SUDO install -m 0755 "$dir/ferrix" /usr/local/bin/ferrix
    if [ -d "$dir/ui" ]; then
        $SUDO mkdir -p /usr/share/ferrix/dashboard
        $SUDO cp -r "$dir/ui/." /usr/share/ferrix/dashboard/
    fi
    if have systemctl && [ -d /run/systemd/system ]; then
        if ! getent passwd ferrix >/dev/null 2>&1; then
            $SUDO useradd --system --user-group --home-dir /etc/ferrix --no-create-home \
                --shell /usr/sbin/nologin ferrix
        fi
        $SUDO install -d -m 0750 -o ferrix -g ferrix /etc/ferrix
        fetch "https://raw.githubusercontent.com/$REPO/v$VERSION/packaging/ferrix.service" "$TMP/ferrix.service"
        sed 's#/usr/bin/ferrix#/usr/local/bin/ferrix#' "$TMP/ferrix.service" |
            $SUDO tee /etc/systemd/system/ferrix.service >/dev/null
        $SUDO systemctl daemon-reload
        $SUDO systemctl enable --now ferrix ||
            say "Ferrix did not start; see: journalctl -u ferrix (is another server using port 80?)"
    else
        $SUDO install -d -m 0755 /etc/ferrix
        say "No systemd found: start Ferrix with: sudo ferrix run"
    fi
}

say "Installing Ferrix v$VERSION ($(uname -m))"
METHOD="${FERRIX_METHOD:-auto}"
if [ "$METHOD" = "binary" ]; then
    install_binary
elif have apt-get; then
    install_apt || { say "APT repository unavailable, installing the .deb directly"; install_deb_file; }
elif have dnf || have yum || have zypper; then
    install_rpm
elif have pacman; then
    install_arch
else
    install_binary
fi

cat <<EOF

Ferrix v$VERSION is installed.
  Open:          http://localhost/
  Configuration: /etc/ferrix/ferrix.yaml (every option is listed there, commented out)
  Apply changes: sudo systemctl reload ferrix
  Logs:          journalctl -u ferrix -f
EOF
