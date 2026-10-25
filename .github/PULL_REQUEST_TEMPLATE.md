## Résumé

Décrivez brièvement le changement et son objectif.

## Vérifications

- [ ] `cargo test --frozen --all-features`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `cargo audit --deny warnings`
- [ ] `actionlint .github/workflows/*.yml`
- [ ] Aucun binaire ou secret ajouté au dépôt
- [ ] Aucune version installée localement pour tester
- [ ] Documentation mise à jour si nécessaire

## Revue

- [ ] Un reviewer a relu les changements (obligatoire pour le score OpenSSF Code-Review)
- [ ] Les checks CI sont verts
