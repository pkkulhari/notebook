#!/bin/sh
# Installs or updates Notebook from the latest GitHub release, on Debian or Ubuntu (amd64):
#   curl -fsSL https://raw.githubusercontent.com/pkkulhari/notebook/master/install.sh | sh
set -eu

REPO=pkkulhari/notebook

fail() {
    echo "install.sh: $*" >&2
    exit 1
}

# Everything runs from main, so a piped script is read in full before apt runs.
main() {
    command -v apt-get >/dev/null 2>&1 || fail "Notebook's package needs Debian or Ubuntu, with apt."
    arch=$(dpkg --print-architecture)
    [ "$arch" = amd64 ] || fail "Notebook's package is for amd64, and this system is $arch."

    sudo=
    if [ "$(id -u)" -ne 0 ]; then
        command -v sudo >/dev/null 2>&1 || fail "Run this as root, or install sudo."
        sudo=sudo
    fi

    url=$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" |
        grep -o '"browser_download_url": *"[^"]*_amd64\.deb"' | head -n 1 | cut -d '"' -f 4)
    [ -n "$url" ] || fail "Couldn't find a Debian package in the latest release."
    file=${url##*/}
    version=${file#notebook_}
    version=${version%_amd64.deb}

    installed=$(dpkg-query -W -f '${db:Status-Status} ${Version}' notebook 2>/dev/null || true)
    case $installed in
        "installed "*) installed=${installed#installed } ;;
        *) installed= ;;
    esac
    if [ "$installed" = "$version" ]; then
        echo "Notebook $version is already installed."
        return
    fi

    dir=$(mktemp -d)
    trap 'rm -rf "$dir"' EXIT
    echo "Downloading Notebook $version..."
    curl -fsSL -o "$dir/$file" "$url"
    curl -fsSL -o "$dir/$file.sha256" "$url.sha256"
    (cd "$dir" && sha256sum -c --quiet "$file.sha256") || fail "The download doesn't match its checksum."
    # apt reads local packages as its unprivileged _apt user.
    chmod 755 "$dir"
    chmod 644 "$dir/$file"

    $sudo apt-get install -y "$dir/$file" </dev/null
    if [ -n "$installed" ]; then
        echo "Updated Notebook from $installed to $version."
    else
        echo "Installed Notebook $version. Open it from your applications."
    fi
}

main "$@"
