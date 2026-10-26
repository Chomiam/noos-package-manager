# Maintainer: Chomiam <axel.valens@ik.me>
pkgname=noos-package-manager
pkgver=0.1.18
pkgrel=1
pkgdesc="Gestionnaire de paquets GTK4/libAdapta (libAdapta inclus) et outil CLI (noos-up) pour Arch Linux (Pacman, AUR via Yay, Flatpak et AppImages)"
arch=('x86_64' 'aarch64')
url="https://github.com/Chomiam/noos-package-manager"
license=('GPL-3.0-or-later' 'LGPL-2.1-or-later')
options=(!lto !debug)
_libadapta_ver=1.5.0

depends=(
  'gtk4'
  'glib2'
  'glibc'
  'gcc-libs'
  'appstream'
  'fribidi'
  'graphene'
  'pango'
  'pacman'
  'pacman-contrib'
  'sudo'
  'yay'
  'flatpak'
  'polkit'
  'hicolor-icon-theme'
  'needrestart'
  'reflector'
  'git'
)
provides=('libadapta=1.5.0' 'libadapta-1.so=0-64')
conflicts=('libadapta')
makedepends=(
  'cargo'
  'cargo-auditable'
  'rust'
  'meson'
  'ninja'
  'sassc'
  'gettext'
  'pkgconf'
  'glib2-devel'
  'gobject-introspection'
  'vala'
)
source=(
  "$pkgname-$pkgver.tar.gz::https://github.com/Chomiam/noos-package-manager/archive/refs/tags/v$pkgver.tar.gz"
  "libadapta-$_libadapta_ver.tar.gz::https://github.com/xapp-project/libadapta/archive/$_libadapta_ver.tar.gz"
)
# Rempli après la création du tag (sommes BLAKE2b des deux tarballs sources).
b2sums=(
  'cccdeefdde7f67d81c8deea51fb861f36a77777b6b6e5a969e9e0a4e983e40043c4a32969e1b4e28481a24b18e5d342b7060be1d5d326faefbe9a89fac66f007'
  '7b7305e300701f91baddbe7da80b006ce6e058cf6ce22bd352d03aff4ed902e6ea166682890cbc5f476ac237c72fb18051f69f523b67f560df81245db4e9f56e'
)

prepare() {
  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  local _stage="$srcdir/libadapta-stage"
  local _build="$srcdir/libadapta-build"
  local _pc="$srcdir/libadapta-pc"

  rm -rf "$_build" "$_stage" "$_pc"

  # libAdapta est construit sans tests : le check() du paquet AUR échoue sur
  # CachyOS quand gtk-application-prefer-dark-theme est actif. La bibliothèque
  # est installée sous son nom d'origine (libadapta-1.so.0), avec ses en-têtes,
  # son GIR, son VAPI et son pkg-config, pour remplacer complètement le paquet
  # AUR libadapta.
  meson setup "$_build" "$srcdir/libadapta-$_libadapta_ver" \
    --prefix=/usr \
    --libdir=lib \
    --buildtype=plain \
    --wrap-mode=nodownload \
    -Dtests=false \
    -Dexamples=false \
    -Dgtk_doc=false \
    -Dintrospection=enabled \
    -Dvapi=true
  meson compile -C "$_build"
  DESTDIR="$_stage" meson install -C "$_build"

  # Copie du pkg-config pour la compilation de l'application : elle doit
  # pointer vers la bibliothèque de staging, jamais vers une copie système.
  install -d "$_pc"
  sed -e "s|^libdir=.*|libdir=$_stage/usr/lib|" \
    -e "s|^includedir=.*|includedir=$_stage/usr/include|" \
    "$_stage/usr/lib/pkgconfig/libadapta-1.pc" > "$_pc/libadapta-1.pc"

  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  export PKG_CONFIG_PATH="$_pc"
  export LD_LIBRARY_PATH="$_stage/usr/lib"
  # Durcissement explicite : RELRO complet, NX, liaison « as-needed ».
  # Les CFLAGS de makepkg apportent déjà PIE, stack protector et FORTIFY_SOURCE
  # aux dépendances C.
  export RUSTFLAGS="${RUSTFLAGS:-} -C relro-level=full \
    -C link-arg=-Wl,-O1 -C link-arg=-Wl,--as-needed \
    -C link-arg=-Wl,-z,noexecstack"
  # cargo-auditable embarque la liste des dépendances Rust dans le binaire,
  # ce qui permet `cargo audit bin` et l'analyse par Syft/Trivy.
  cargo auditable build --frozen --release --all-features
}

check() {
  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export PKG_CONFIG_PATH="$srcdir/libadapta-pc"
  export LD_LIBRARY_PATH="$srcdir/libadapta-stage/usr/lib"
  cargo test --frozen --all-features
}

package() {
  cd "$srcdir/$pkgname-$pkgver"
  install -Dm755 "target/release/$pkgname" "$pkgdir/usr/bin/$pkgname"
  install -Dm755 "target/release/noos-up" "$pkgdir/usr/bin/noos-up"
  install -Dm644 "assets/$pkgname.desktop" "$pkgdir/usr/share/applications/$pkgname.desktop"
  install -Dm644 "assets/$pkgname.svg" "$pkgdir/usr/share/icons/hicolor/scalable/apps/$pkgname.svg"
  install -Dm644 "LICENSE" "$pkgdir/usr/share/licenses/$pkgname/LICENSE"
  install -Dm644 "$srcdir/libadapta-$_libadapta_ver/COPYING" \
    "$pkgdir/usr/share/licenses/$pkgname/LICENSE-libadapta"
  install -Dm644 "README.md" "$pkgdir/usr/share/doc/$pkgname/README.md"

  # libAdapta complet (bibliothèque, en-têtes, GIR, VAPI, pkg-config,
  # traductions et thème d'exemple), sous son nom d'origine. Le paquet déclare
  # provides/conflicts=libadapta pour remplacer le paquet AUR du même nom.
  cp -a "$srcdir/libadapta-stage/usr/." "$pkgdir/usr/"
}
