# Harnais de fuzzing

Deux cibles couvrent les parseurs réseau et Pacman :

- `aur_rpc_json` : `parse_aur_rpc_json` (réponses JSON de l'API AUR RPC v5) ;
- `pacman_ss` : `parse_pacman_ss_output` (sortie de `pacman -Ss`).

## Lancer

```bash
cargo install cargo-fuzz --locked
cargo +nightly fuzz run aur_rpc_json -- -max_total_time=60
cargo +nightly fuzz run pacman_ss -- -max_total_time=60
```

Les corpus et artefacts sont ignorés par Git. Le code de la bibliothèque
`noos-package-manager` est réutilisé via `src/lib.rs`, sans dépendance GTK.
