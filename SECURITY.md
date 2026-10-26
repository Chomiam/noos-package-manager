# Politique de sécurité

## Versions prises en charge

Seule la dernière version publiée sur l'AUR et sur GitHub reçoit des correctifs
de sécurité.

## Signaler une vulnérabilité

Merci de **ne pas ouvrir d'issue publique** pour une vulnérabilité.

- Utilisez l'onglet **Security → Advisories → Report a vulnerability** du dépôt
  GitHub : <https://github.com/Chomiam/noos-package-manager/security/advisories/new>
- Ou écrivez à `axel.valens@ik.me` avec les détails, la version concernée et un
  moyen de reproduction.

Un accusé de réception est envoyé sous 72 heures. Une fois le correctif publié,
l'advisory est rendu public avec le crédit du rapporteur si celui-ci le souhaite.

## Vérifier une release

Chaque release GitHub contient, en plus du paquet Arch :

| Fichier | Rôle |
| --- | --- |
| `noos-package-manager-<version>-1-x86_64.pkg.tar.zst` | paquet construit par la CI |
| `noos-package-manager-<tag>.intoto.jsonl` | provenance SLSA Build L3 |
| `sbom.spdx.json` | SBOM SPDX 2.3 des dépendances compilées |
| `hashes.txt` | empreintes SHA-256 utilisées par la provenance SLSA |
| `b2sums.txt` | empreintes BLAKE2b du paquet et du SBOM |

Les sommes du **code source** sont dans le `PKGBUILD` de l'AUR, au format
`b2sums` (BLAKE2b-512). Elles couvrent le tarball de l'application et celui de
libAdapta embarqué. `makepkg` les vérifie automatiquement pendant l'installation
depuis l'AUR.

### Vérification de la provenance

Installez [`slsa-verifier`](https://github.com/slsa-framework/slsa-verifier) :

```bash
# Depuis l'AUR
yay -S slsa-verifier

# Ou avec Go
go install github.com/slsa-framework/slsa-verifier/v2/cli/slsa-verifier@latest
```

Puis, dans le dossier contenant les fichiers téléchargés :

```bash
TAG=v0.1.17
VERSION=0.1.17

slsa-verifier verify-artifact \
  "noos-package-manager-${VERSION}-1-x86_64.pkg.tar.zst" \
  --provenance-path "noos-package-manager-${TAG}.intoto.jsonl" \
  --source-uri github.com/Chomiam/noos-package-manager \
  --source-tag "$TAG"
```

La provenance est générée par le workflow officiel
[`slsa-github-generator`](https://github.com/slsa-framework/slsa-github-generator)
sur un runner GitHub hébergé, à partir du commit tagué. Elle atteste le workflow,
le dépôt, le tag et les empreintes des artefacts.

### Signatures cosign

Les assets signés par le workflow `sign-release.yml` possèdent un bundle
`.sigstore.json` (signature keyless Sigstore). Exemple pour v0.1.13 :

```bash
cosign verify-blob \
  --bundle noos-package-manager-0.1.13-1-x86_64.pkg.tar.zst.sigstore.json \
  --certificate-identity-regexp \
    'https://github.com/Chomiam/noos-package-manager/.github/workflows/sign-release.yml@refs/heads/main' \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com \
  noos-package-manager-0.1.13-1-x86_64.pkg.tar.zst
```

La signature est liée au workflow GitHub et au tag ; elle ne remplace pas la
provenance SLSA, elle la complète.

### Vérification des sommes

```bash
b2sum -c b2sums.txt
sha256sum -c hashes.txt
```

### SBOM

Le SBOM SPDX 2.3 liste les bibliothèques Rust et système compilées :

```bash
jq '.packages | length' sbom.spdx.json
syft scan sbom.spdx.json -o table
```

Il peut être analysé avec `grype` ou `trivy` :

```bash
grype sbom:sbom.spdx.json
```

### libAdapta embarqué

Depuis 0.1.17, le paquet construit libAdapta 1.5.0 depuis sa source officielle
et l'installe sous le nom `libnoos-adapta-1.so.0`. La somme BLAKE2b du tarball
libadapta est vérifiée par `makepkg` comme celle du code source de
l'application. La bibliothèque est distribuée sous **LGPL-2.1-or-later** ; sa
licence est installée dans
`/usr/share/licenses/noos-package-manager/LICENSE-libadapta`. Le renommage du
soname évite tout conflit avec le paquet AUR `libadapta`.

## Contrôles automatiques

Le workflow [`ci.yml`](.github/workflows/ci.yml) exécute à chaque push et pull
request :

- `cargo test --frozen --all-features` ;
- `cargo clippy --all-targets -- -D warnings` ;
- `cargo audit --deny warnings` sur `Cargo.lock` ;
- `cargo audit bin` sur le binaire construit avec `cargo-auditable` ;
- `namcap` sur le `PKGBUILD` et sur le paquet généré ;
- `checksec` : RELRO complet, PIE, NX, stack canary, absence de RPATH/RUNPATH
  et binaire strippé.

Le workflow de release vérifie en plus la somme BLAKE2b du tarball source avant
de publier sur l'AUR, génère le SBOM SPDX et la provenance SLSA.

### Durcissement

Le `PKGBUILD` construit avec `cargo auditable` et force :

- `-C relro-level=full` ;
- `-Wl,-z,noexecstack` ;
- `-Wl,--as-needed` ;
- les `CFLAGS`/`LDFLAGS` d'Arch Linux pour les dépendances C (PIE, stack
  protector, `FORTIFY_SOURCE=3`, RELRO).

`checksec` doit afficher `Full RELRO`, `Canary Found`, `NX enabled` et
`PIE Enabled`. `FORTIFY_SOURCE` ne s'applique qu'au code C/C++ : le code Rust
n'appelle pas les variantes `__*_chk` de la libc, donc `fortify_source` reste
à `No` dans `checksec` ; les dépendances C bénéficient des drapeaux de makepkg.

### Publication AUR

La publication AUR est effectuée par GitHub Actions après validation du tag et
de la somme BLAKE2b. Elle utilise le secret d'environnement
`AUR_SSH_PRIVATE_KEY`, restreint aux tags `v*` par la politique de déploiement
de l'environnement GitHub `aur`.

Une clé SSH dédiée à la CI a été générée pour remplacer la clé personnelle du
mainteneur. Pour l'activer :

1. Ajouter la clé publique suivante au compte AUR
   (My Account → SSH Keys) :

```text
ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIDDxsRZnjYTZqp6fq1xe6QVV/1n5p0W59GZoakk1EDx7 noos-package-manager CI
```

2. Remplacer le secret GitHub :

```bash
gh secret set AUR_SSH_PRIVATE_KEY --env aur \
  --repo Chomiam/noos-package-manager < ~/.ssh/aur-noos-ci
```

Tant que cette activation n'est pas terminée, la clé personnelle existante est
utilisée ; la restriction de l'environnement aux tags `v*` limite son exposition.

## Recommandations pour le dépôt

Pour maximiser le score OpenSSF Scorecard :

- protéger la branche `main` (pull request obligatoire, checks CI requis,
  interdiction du force-push) ;
- conserver les actions GitHub épinglées par SHA, sauf le générateur SLSA qui doit rester référencé par tag ;
- activer Dependabot pour Cargo et GitHub Actions (`.github/dependabot.yml`) ;
- examiner les alertes Code Scanning générées par Scorecard.
