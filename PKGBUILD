# Maintainer: Chomiam <axel.valens@ik.me>
pkgname=noos-package-manager
pkgver=0.1.17
pkgrel=1
pkgdesc="Gestionnaire de paquets GTK4/libAdapta et outil CLI (noos-up) pour Arch Linux (Pacman, AUR via Yay, Flatpak et AppImages)"
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
makedepends=(
  'cargo'
  'cargo-auditable'
  'rust'
  'meson'
  'ninja'
  'sassc'
  'gettext'
  'pkgconf'
)
source=(
  "$pkgname-$pkgver.tar.gz::https://github.com/Chomiam/noos-package-manager/archive/refs/tags/v$pkgver.tar.gz"
  "libadapta-$_libadapta_ver.tar.gz::https://github.com/xapp-project/libadapta/archive/$_libadapta_ver.tar.gz"
)
# Rempli après la création du tag (sommes BLAKE2b des deux tarballs sources).
b2sums=(
  'b91a5e90b5b8c303f30bbec2a8e6456cbc00216ccfc444143351e01aa03d6b11a4cb21cd39971f747d16eb9f52cdb6308ffc6f0c6d50697010780bda50891312'
  '7b7305e300701f91baddbe7da80b006ce6e058cf6ce22bd352d03aff4ed902e6ea166682890cbc5f476ac237c72fb18051f69f523b67f560df81245db4e9f56e'
)

prepare() {
  cd "$srcdir/libadapta-$_libadapta_ver"
  # Renomme la bibliothèque pour qu'elle puisse coexister avec le paquet AUR
  # libadapta sans conflit de fichiers ni de soname.
  sed -i "s/'adapta-' + apiversion/'noos-adapta-' + apiversion/" src/meson.build
  grep -q "'noos-adapta-' + apiversion" src/meson.build || {
    echo "Le renommage de libadapta a échoué." >&2
    return 1
  }

  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  cargo fetch --locked --target "$(rustc -vV | sed -n 's/host: //p')"
}

build() {
  local _stage="$srcdir/libadapta-stage"
  local _build="$srcdir/libadapta-build"

  rm -rf "$_build" "$_stage"

  # libAdapta est construit sans tests : le check() du paquet AUR échoue sur
  # CachyOS quand gtk-application-prefer-dark-theme est actif. L'application
  # n'a besoin que de la bibliothèque.
  meson setup "$_build" "$srcdir/libadapta-$_libadapta_ver" \
    --prefix=/usr \
    --libdir=lib \
    --buildtype=plain \
    --wrap-mode=nodownload \
    -Dtests=false \
    -Dexamples=false \
    -Dintrospection=disabled \
    -Dvapi=false \
    -Dgtk_doc=false \
    -Dlocaledir=share/noos-package-manager/locale \
    -Ddatadir=share/noos-package-manager
  meson compile -C "$_build"
  DESTDIR="$_stage" meson install -C "$_build"

  # pkg-config doit pointer vers la bibliothèque de staging, jamais vers une
  # éventuelle copie système de libadapta.
  sed -i \
    -e "s|^libdir=.*|libdir=$_stage/usr/lib|" \
    -e "s|^includedir=.*|includedir=$_stage/usr/include|" \
    "$_stage/usr/lib/pkgconfig/libadapta-1.pc"

  cd "$srcdir/$pkgname-$pkgver"
  export RUSTUP_TOOLCHAIN=stable
  export CARGO_TARGET_DIR=target
  export PKG_CONFIG_PATH="$_stage/usr/lib/pkgconfig"
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
  export PKG_CONFIG_PATH="$srcdir/libadapta-stage/usr/lib/pkgconfig"
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

  # Bibliothèque libAdapta privée, renommée pour ne pas entrer en conflit avec
  # le paquet AUR libadapta. Le binaire est lié à ce soname.
  install -Dm755 "$srcdir/libadapta-stage/usr/lib/libnoos-adapta-1.so.0" \
    "$pkgdir/usr/lib/libnoos-adapta-1.so.0"

  # Traductions de libAdapta, dans le LOCALEDIR privé compilé dans la
  # bibliothèque.
  if [[ -d "$srcdir/libadapta-stage/usr/share/noos-package-manager/locale" ]]; then
    install -d "$pkgdir/usr/share/noos-package-manager"
    cp -a "$srcdir/libadapta-stage/usr/share/noos-package-manager/locale" \
      "$pkgdir/usr/share/noos-package-manager/"
  fi
}
