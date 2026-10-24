# libadwaita-sys — patched for libAdapta

Ce dossier est une copie de la crate [`libadwaita-sys` 0.7.2](https://crates.io/crates/libadwaita-sys/0.7.2)
(générée par [gir](https://github.com/gtk-rs/gir), licence MIT, voir `LICENCE`).

Le projet utilise **libAdapta** (fork AUR de libadwaita 1.5, API C `adap_*`)
à la place de libadwaita. La crate originale ne peut pas être utilisée telle
quelle car elle recherche `libadwaita-1` via pkg-config et déclare les symboles
`adw_*`.

Deux modifications ont été appliquées :

1. `Cargo.toml` : la dépendance système pointe vers `libadapta-1` au lieu de
   `libadwaita-1`.
2. `src/lib.rs` : chaque déclaration FFI `adw_*` reçoit un attribut
   `#[link_name = "adap_*"]`, ce qui permet de conserver les bindings Rust de
   haut niveau (`libadwaita` 0.7.2) tout en liant la bibliothèque libAdapta.

La fonction `adw_alert_dialog_get_prefer_wide_layout` n'existe pas dans
libAdapta 1.5 ; elle est laissée sans `link_name` et n'est pas utilisée par
l'application. Le binaire ne référence donc pas ce symbole.

## Mise à jour

Pour régénérer ce dossier depuis une nouvelle version de `libadwaita-sys` :

```sh
cp -a ~/.cargo/registry/src/*/libadwaita-sys-<version> vendor/libadwaita-sys
# puis réappliquer les deux modifications décrites ci-dessus
cargo update -p libadwaita-sys
```

Le binaire final doit lier `libadapta-1.so.0` ; vérifier avec :

```sh
ldd target/release/noos-package-manager | grep adapta
```
