# AGENT.md — Guide des agents et du workflow GitHub

Ce document décrit le fonctionnement du dépôt, des workflows GitHub Actions et
les règles à respecter pour préparer, tester et publier une version de
**Noos Package Manager**.

## Règles absolues

1. **Ne jamais installer une version localement pour tester.** Pas de
   `pacman -U`, `yay -S`, `makepkg -i` ni copie dans `/usr/bin`. Le test se fait
   avec `./target/release/noos-package-manager`, `/usr/bin/noos-package-manager`
   (déjà installé) ou un binaire de test lancé directement.
2. **Ne jamais pousser directement sur `main`.** La branche est protégée :
   passer par `testing` puis une pull request avec le check CI obligatoire.
   Les administrateurs peuvent contourner la protection, mais ce n'est pas la
   voie normale.
3. **Ne jamais déplacer, supprimer ou réécrire un tag publié.** Une release
   publiée doit rester reproductible.
4. **Ne jamais modifier les `b2sums` sans avoir créé le tag correspondant.**
   La somme est calculée sur le tarball GitHub du tag.
5. **Ne jamais committer de secret.** La clé AUR est stockée dans
   l'environnement GitHub `aur`, pas dans le dépôt.
6. **Ne pas modifier `~/.local/bin`, `~/.cargo/bin` ou les fichiers `.desktop`
   de l'utilisateur sans son accord.** Ces fichiers peuvent masquer `/usr/bin`
   et lancer une ancienne version.

## Rôles des branches

- `testing` : branche de développement et de test. Les commits de travail y
  sont faits.
- `main` : branche stable protégée. Les changements arrivent par pull request.
- Les releases sont des tags `vX.Y.Z` sur un commit de `main`.

## Workflows GitHub Actions

### `.github/workflows/ci.yml`

Déclencheurs : push sur `main`/`testing` et pull requests.

Le job `Build, tests et contrôles de sécurité` tourne dans un conteneur
`archlinux:base-devel` **épinglé par digest**. Il installe les outils, construit
`libadapta` depuis l'AUR à un commit épinglé, puis exécute :

- `cargo fetch --locked` ;
- `cargo test --frozen --all-features` ;
- `cargo clippy --all-targets -- -D warnings` ;
- `cargo audit --deny warnings` ;
- construction du paquet avec un tarball local créé par `tar` (pas `git archive`,
  qui échoue dans le conteneur) ;
- `namcap` sur le PKGBUILD et le paquet (seules les erreurs `E:` sont bloquantes) ;
- `checksec` sur le binaire du paquet : RELRO complet, PIE, NX, canary, pas de
  RPATH/RUNPATH, binaire strippé ;
- `cargo audit bin` sur le binaire construit avec `cargo-auditable`.

Le job est le **check requis** par la protection de `main`.

### `.github/workflows/release.yml`

Déclencheur : `release: published` (release GitHub déjà créée sur un tag `v*`).

Jobs :

1. **`build`** : checkout de `main` (PKGBUILD avec les `b2sums` à jour), vérifie
   que `pkgver` correspond au tag, construit le tarball source depuis le tag via
   `makepkg`, puis exécute namcap, checksec, `cargo audit`/`cargo audit bin` et
   génère le **SBOM SPDX 2.3** avec `syft`.
2. **`provenance`** : appelle le workflow officiel
   `slsa-framework/slsa-github-generator/.../generator_generic_slsa3.yml`.
   **Le générateur doit être référencé par un tag (`v2.1.0`), jamais par un SHA** :
   son action interne utilise ce ref pour télécharger le builder signé.
3. **`release-assets`** : attache le paquet, le SBOM, les empreintes SHA-256 et
   BLAKE2b à la release GitHub.
4. **`publish-aur`** : vérifie le tag, la version et la somme BLAKE2b du tarball,
   clone l'AUR, copie `PKGBUILD` et `.SRCINFO`, puis pousse le commit
   `upgpkg: noos-package-manager X.Y.Z-1`.

L'environnement GitHub `aur` est restreint aux tags `v*` et contient le secret
`AUR_SSH_PRIVATE_KEY`.

### `.github/workflows/scorecard.yml`

Exécute **OpenSSF Scorecard v2.5.0** (image GHCR ; les anciennes images GCR
v2.4.0 échouent avec une erreur de facturation Google), publie les résultats et
téléverse le SARIF dans GitHub Code Scanning.

### `.github/dependabot.yml`

Met à jour chaque semaine les dépendances Cargo et les actions GitHub. Les
actions sont épinglées par SHA ; Dependabot met à jour le SHA et le commentaire
de version.

## Protection de `main` et environnements

- Pull request obligatoire, check CI requis, force-push et suppression
  interdits, résolution des conversations requise.
- `enforce_admins` est désactivé : un administrateur peut encore pousser
  directement, mais la voie recommandée reste la PR.
- Environnement `aur` : secret `AUR_SSH_PRIVATE_KEY`, restreint aux tags `v*`.
- Une clé SSH dédiée à la CI est générée ; sa clé publique et la procédure
  d'activation sont dans [`SECURITY.md`](SECURITY.md#publication-aur).

## Procédure de release

1. Développer et tester sur `testing`.
2. Mettre à jour la version :
   - `Cargo.toml` ;
   - `PKGBUILD` (`pkgver`, `b2sums=('SKIP')`) ;
   - `.SRCINFO` via `makepkg --printsrcinfo` ;
   - `Cargo.lock` via `cargo check`.
3. Lancer localement `cargo test`, `cargo clippy --all-targets -- -D warnings`,
   `cargo audit`, `actionlint .github/workflows/*.yml`.
4. Committer sur `testing`, pousser, ouvrir une pull request vers `main`.
5. Attendre le check CI, puis merger.
6. Créer et pousser le tag :
   ```bash
   git checkout main && git pull --ff-only
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```
7. Calculer la somme BLAKE2b du tarball du tag :
   ```bash
   curl -L -o source.tar.gz \
     "https://github.com/Chomiam/noos-package-manager/archive/refs/tags/vX.Y.Z.tar.gz"
   b2sum source.tar.gz
   ```
8. Remplacer `b2sums=('SKIP')` et la valeur dans `.SRCINFO`, committer et
   pousser sur `main` (commit `chore(aur): update b2sums for vX.Y.Z`).
9. Créer la release GitHub publiée :
   ```bash
   gh release create vX.Y.Z --title "..." --notes-file ...
   ```
   Cela déclenche `release.yml`.
10. Vérifier :
    - succès du workflow Release, y compris `Provenance SLSA / final` ;
    - AUR à la bonne version ;
    - `slsa-verifier verify-artifact ... --source-tag vX.Y.Z` ;
    - `b2sum -c b2sums.txt` et `sha256sum -c hashes.txt` ;
    - SBOM présent ;
    - `checksec` sur le binaire du paquet.
11. Resynchroniser `testing` sur `main` :
    ```bash
    git checkout testing && git reset --hard main
    git push --force-with-lease origin testing
    ```

## Version affichée et mises à jour de l'application

- `SELF_PACKAGE_VERSION` vaut `env!("CARGO_PKG_VERSION")`, c'est-à-dire la
  version **du binaire en cours d'exécution**. Le label en bas à gauche affiche
  cette valeur.
- La vérification des mises à jour AUR utilise `yay -Qua`, donc la version
  **installée dans la base Pacman**, pas la version du binaire lancé.
- Si la base Pacman contient déjà `0.1.15` mais qu'un ancien binaire `0.1.14`
  est lancé, l'application affiche `v0.1.14` et ne propose aucune mise à jour :
  c'est cohérent du point de vue Pacman, mais déroutant.
- `~/.local/bin` et `~/.cargo/bin` peuvent précéder `/usr/bin` dans le `PATH`.
  Un ancien binaire local peut donc masquer la version installée par le paquet.
  Vérifier avec :
  ```bash
  which -a noos-package-manager
  pacman -Q noos-package-manager
  ```
- Le fichier `~/.local/share/applications/noos-package-manager.desktop` peut
  aussi lancer `Exec=noos-package-manager` (donc l'ancien binaire local). Pour
  utiliser la version installée :
  ```bash
  sed -i 's|^Exec=.*|Exec=/usr/bin/noos-package-manager|' \
    ~/.local/share/applications/noos-package-manager.desktop
  ```
  ou lancer directement `/usr/bin/noos-package-manager`.
- `sync_self_installed_binaries()` synchronise `/usr/bin` vers les copies
  locales après une **auto-mise à jour de l'application**, pas après une mise à
  jour Pacman/AUR. Une mise à jour par le paquet ne répare donc pas les copies
  locales.
- **Ne pas installer de version locale.** Pour tester une version de
  développement, lancer directement le binaire `target/release`.

## Dépannage

| Symptôme | Cause | Correction |
| --- | --- | --- |
| CI `cargo test --frozen` : `no matching package` | dépôt local vide | ajouter `cargo fetch --locked` avant les tests |
| CI `fatal: not a git repository` | `git archive` dans le conteneur | créer l'archive avec `tar` |
| Scorecard : `docker pull failed ... billing` | image GCR v2.4.0 | utiliser Scorecard v2.5.0 (GHCR) |
| SLSA : `Invalid ref ... Expected refs/tags/vX.Y.Z` | générateur épinglé par SHA | référencer `@v2.1.0` |
| `ring` : symboles x25519 manquants | LTO de makepkg | `options=(!lto)` |
| AUR : refus de push | secret absent ou tag hors `v*` | vérifier `AUR_SSH_PRIVATE_KEY` et la politique de l'environnement `aur` |
| AUR : `b2sums` invalides | tarball du tag modifié | recalculer la somme et republier |
| App affiche une ancienne version | ancien binaire dans `~/.local/bin`/`~/.cargo/bin` | lancer `/usr/bin/noos-package-manager` ou corriger le `Exec` du `.desktop` |
| Aucune mise à jour affichée | paquet déjà plus récent que le binaire lancé | redémarrer la version installée ; ne pas installer manuellement |

## Vérifications de sécurité

```bash
slsa-verifier verify-artifact \
  noos-package-manager-X.Y.Z-1-x86_64.pkg.tar.zst \
  --provenance-path noos-package-manager-vX.Y.Z.intoto.jsonl \
  --source-uri github.com/Chomiam/noos-package-manager \
  --source-tag vX.Y.Z

b2sum -c b2sums.txt
sha256sum -c hashes.txt
checksec file /usr/bin/noos-package-manager
```

Le binaire doit afficher : `Full RELRO`, `Canary Found`, `NX enabled`,
`PIE Enabled`, aucun RPATH/RUNPATH et aucun symbole. `FORTIFY_SOURCE` ne
s'applique qu'aux dépendances C ; il est normal que `fortify_source` reste à
`No` pour du code Rust pur.
