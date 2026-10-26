# OpenSSF Best Practices — préparation du badge

Le check **CII-Best-Practices** d'OpenSSF Scorecard interroge
`bestpractices.dev` à partir de l'URL du dépôt. Tant qu'aucun projet n'est
enregistré pour `https://github.com/Chomiam/noos-package-manager`, le score
reste à **0**.

## Créer le projet

1. Se connecter sur <https://www.bestpractices.dev/en> avec GitHub.
2. Créer un projet : <https://www.bestpractices.dev/en/projects/new>.
3. Renseigner l'URL du dépôt :
   `https://github.com/Chomiam/noos-package-manager`.
4. Répondre au questionnaire. Un badge **Passing** vaut **5/10** dans
   Scorecard, **Silver** 7/10, **Gold** 10/10, **InProgress** 2/10.
5. Ajouter le badge au README une fois le projet créé.

## Réponses et preuves

### Bases

- Nom : **Noos Package Manager**
- Description : gestionnaire de paquets GTK4/libAdapta pour Arch Linux
  (Pacman, AUR, Flatpak, AppImages).
- Site : <https://github.com/Chomiam/noos-package-manager>
- Licence : **GPL-3.0-or-later** (`LICENSE`).
- Documentation : `README.md`, `AGENT.md`, `SECURITY.md`.
- Dépôt : GitHub, public.
- Suivi des bugs : <https://github.com/Chomiam/noos-package-manager/issues>
- Vulnérabilités : `SECURITY.md`, GitHub Security Advisories et
  `axel.valens@ik.me`.

### Contrôle des changements

- Développement sur `testing`, pull request vers `main` protégée.
- CI obligatoire : tests, Clippy, `cargo audit`, `cargo auditable`, namcap,
  checksec.
- Versions taguées `vX.Y.Z`.
- Releases avec provenance **SLSA Build L3**, **SBOM SPDX** et signatures
  **cosign**.
- Journal des changements dans les notes de release GitHub.

### Signalement et sécurité

- `SECURITY.md` décrit le signalement privé et les délais.
- GitHub Security Advisories activé.
- Aucun secret dans le dépôt ; `AUR_SSH_PRIVATE_KEY` est un secret
  d'environnement GitHub.
- Analyse statique **CodeQL** (Rust, `build-mode: none`).
- Audit des dépendances Rust (`cargo audit`) et binaire (`cargo audit bin`).
- Fuzzing `cargo-fuzz` : `fuzz/fuzz_targets/aur_rpc_json.rs` et
  `fuzz/fuzz_targets/pacman_ss.rs`.
- Durcissement : RELRO complet, PIE, NX, stack canary ; dépendances C avec
  `FORTIFY_SOURCE=3`.

### Qualité

- 33 tests unitaires et d'intégration PTY.
- CI sur chaque push et pull request.
- Clippy sans avertissement (`-D warnings`).
- CodeQL et fuzzing.
- Revue de code via pull request ; CODEOWNERS et modèle de PR fournis.

### Analyse

- CodeQL pour Rust.
- `cargo audit` et `cargo auditable`.
- `namcap` et `checksec` pour le paquet Arch.
- libAdapta 1.5.0 embarqué, construit depuis sa source officielle et distribué
  sous LGPL-2.1-or-later (licence incluse).
- SBOM SPDX attaché aux releases.

## Après création

Ajouter dans le README :

```markdown
[![OpenSSF Best Practices](https://www.bestpractices.dev/projects/PROJECT_ID/badge)](https://www.bestpractices.dev/projects/PROJECT_ID)
```

Remplacer `PROJECT_ID` par l'identifiant du projet. Ne pas inventer d'URL :
Scorecard vérifie le projet auprès de l'API.
