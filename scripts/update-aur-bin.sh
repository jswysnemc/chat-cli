#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 2 ]]; then
    echo "usage: $0 <tag-or-version> <aur-checkout-dir>" >&2
    exit 2
fi

input_version="$1"
aur_dir="$2"
pkgver="${input_version#refs/tags/}"
pkgver="${pkgver#v}"
tag="v${pkgver}"

pkgname="chat-cli-bin"
pkgrel="${AUR_PKGREL:-1}"
project_url="https://github.com/jswysnemc/chat-cli"
asset="chat-cli-${tag}-linux-x86_64.tar.gz"
asset_url="${project_url}/releases/download/${tag}/${asset}"
source_name="${pkgname}-${pkgver}.tar.gz"
pkgdesc="Configurable LLM chat CLI"

if [[ ! -d "$aur_dir/.git" ]]; then
    echo "AUR checkout not found: $aur_dir" >&2
    exit 2
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

curl -fsSL -o "${tmpdir}/${asset}" "$asset_url"
sha256="$(sha256sum "${tmpdir}/${asset}" | awk '{print $1}')"

cat >"${aur_dir}/PKGBUILD" <<EOF
# Maintainer: Snemc-s <snemc@snemc.cn>
pkgname=${pkgname}
pkgver=${pkgver}
pkgrel=${pkgrel}
pkgdesc="${pkgdesc}"
arch=('x86_64')
url="${project_url}"
license=('MIT')
depends=('gcc-libs' 'glibc' 'ripgrep')
optdepends=(
    'fzf: shell helper integration and quick switching'
    'uv: MCP servers and helper scripts that use Python tooling'
    'wl-clipboard: Wayland clipboard image/text input'
    'xclip: X11 clipboard image/text input'
)
provides=("chat-cli=\${pkgver}")
conflicts=('chat-cli' 'chat-cli-git')
source=("${source_name}::\${url}/releases/download/v\${pkgver}/${asset}")
sha256sums=('${sha256}')

package() {
    install -Dm755 chat "\${pkgdir}/usr/bin/chat"
    install -Dm644 README.md "\${pkgdir}/usr/share/doc/\${pkgname}/README.md"
    install -Dm644 README_zh.md "\${pkgdir}/usr/share/doc/\${pkgname}/README_zh.md"
    install -Dm644 LICENSE "\${pkgdir}/usr/share/licenses/\${pkgname}/LICENSE"
}
EOF

cat >"${aur_dir}/.SRCINFO" <<EOF
pkgbase = ${pkgname}
	pkgdesc = ${pkgdesc}
	pkgver = ${pkgver}
	pkgrel = ${pkgrel}
	url = ${project_url}
	arch = x86_64
	license = MIT
	depends = gcc-libs
	depends = glibc
	depends = ripgrep
	optdepends = fzf: shell helper integration and quick switching
	optdepends = uv: MCP servers and helper scripts that use Python tooling
	optdepends = wl-clipboard: Wayland clipboard image/text input
	optdepends = xclip: X11 clipboard image/text input
	provides = chat-cli=${pkgver}
	conflicts = chat-cli
	conflicts = chat-cli-git
	source = ${source_name}::${asset_url}
	sha256sums = ${sha256}

pkgname = ${pkgname}
EOF

echo "Updated ${pkgname} to ${pkgver}"
