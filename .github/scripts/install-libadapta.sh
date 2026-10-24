#!/usr/bin/env bash
#
# Installe libAdapta dans l'environnement Arch Linux de la CI.
#
# Le paquet n'est pas dans les dépôts officiels : on construit la version
# épinglée de l'AUR une seule fois, puis on met le paquet en cache entre les
# exécutions GitHub Actions.

set -euo pipefail

LIBADAPTA_COMMIT="a797378f0282933d85e125ceaccc150bc3a00ed0" # AUR libadapta 1.5.0-4
CACHE_DIR="/var/cache/libadapta-ci"
BUILD_DIR="/tmp/libadapta-aur"
PKG_GLOB="libadapta-1.5.0-4-*.pkg.tar.zst"

if [[ $EUID -ne 0 ]]; then
    echo "install-libadapta.sh doit être exécuté en root (conteneur CI)." >&2
    exit 1
fi

if pacman -Q libadapta >/dev/null 2>&1; then
    echo "libadapta est déjà installé."
    pkg-config --modversion libadapta-1
    exit 0
fi

if compgen -G "$CACHE_DIR/$PKG_GLOB" >/dev/null; then
    echo "Installation de libadapta depuis le cache."
    pacman -U --noconfirm "$CACHE_DIR"/$PKG_GLOB
    pkg-config --modversion libadapta-1
    exit 0
fi

echo "Construction de libadapta ${LIBADAPTA_COMMIT} depuis l'AUR."
id builder >/dev/null 2>&1 || useradd -m builder

rm -rf "$BUILD_DIR"
git clone https://aur.archlinux.org/libadapta.git "$BUILD_DIR"
git -C "$BUILD_DIR" checkout --detach "$LIBADAPTA_COMMIT"
chown -R builder:builder "$BUILD_DIR"

su - builder -c "cd '$BUILD_DIR' && makepkg --nodeps --nocheck --noconfirm"

install -d -m 755 "$CACHE_DIR"
cp "$BUILD_DIR"/$PKG_GLOB "$CACHE_DIR"/
pacman -U --noconfirm "$CACHE_DIR"/$PKG_GLOB

pkg-config --modversion libadapta-1
