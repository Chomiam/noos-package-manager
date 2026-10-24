# Noos Package Manager

**Noos Package Manager** est un gestionnaire de paquets graphique moderne et unifié pour **Arch Linux**, écrit intégralement en **Rust** avec **GTK4** et **libAdapta**.

Il réunit au sein d'une seule interface fluide (compatible avec les thèmes GNOME et WhiteSur) les **dépôts officiels Pacman**, l'**Arch User Repository (AUR RPC v5 + `yay`)**, les applications **Flatpak / Flathub** ainsi que la gestion locale complète des **AppImages**, et inclut l'outil en ligne de commande **`noos-up`** propulsé par **Ratatui**.

[![CI](https://github.com/Chomiam/noos-package-manager/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Chomiam/noos-package-manager/actions/workflows/ci.yml)
[![OpenSSF Scorecard](https://api.securityscorecards.dev/projects/github.com/Chomiam/noos-package-manager/badge)](https://scorecard.dev/viewer/?uri=github.com/Chomiam/noos-package-manager)
[![AUR](https://img.shields.io/aur/version/noos-package-manager)](https://aur.archlinux.org/packages/noos-package-manager)
[![Release](https://img.shields.io/github/v/release/Chomiam/noos-package-manager?display_name=tag)](https://github.com/Chomiam/noos-package-manager/releases)
[![License](https://img.shields.io/github/license/Chomiam/noos-package-manager)](LICENSE)
[![SLSA Build L3](https://img.shields.io/badge/SLSA-Build%20L3-4c1)](SECURITY.md)

---

## Aperçu Visuel (Captures d'écran)

### 1. Recommandations Pacman par catégories & Logos natifs
![Recommandations Pacman](assets/screenshots/pacman-recommendations.png)

### 2. Catalogue AUR en colonnes alignées & Tri dynamique
![Catalogue AUR en colonnes](assets/screenshots/aur-columns.png)

### 3. Intégration Flatpak & Recommandations Flathub
![Recommandations Flatpak Flathub](assets/screenshots/flatpak-flathub.png)

### 4. Gestionnaire d'AppImages avec Glisser-Déposer (`~/Applications` + Menu Démarrer)
![Gestion des AppImages](assets/screenshots/appimages-drag-drop.png)

### 5. Vue d'ensemble unifiée
![Vue d'ensemble Noos Package Manager](assets/screenshots/overview.png)

---

## Fonctionnalités Principales

### Dépôts officiels Pacman (`pacman` & `checkupdates`)
- Recommandations organisées par catégories (*Navigateurs & Internet*, *Multimédia, Audio & Création*, *Bureautique & Productivité*, *Développement & Programmation*, *Jeux & Gaming*, *Système & Utilitaires*) avec les paquets non installés présentés en priorité.
- Résolution automatique des logos d'applications (WhiteSur, hicolor, breeze, Flathub appstream, `.desktop`).
- Réparation automatique du verrou de base de données Pacman (`/var/lib/pacman/db.lck`) via Polkit.

### Arch User Repository — AUR (`RPC v5` + `yay`)
- Affichage tabulaire aligné en colonnes : **Votes (`★`)**, **Popularité**, **Mainteneur** (avec détection des paquets orphelins), **Date de dernière mise à jour**, **Version** et badge d'alerte **Obsolète (`Out-of-Date`)**.
- Sélecteur de tri dédié dans l'onglet AUR : *Popularité (par défaut)*, *Votes*, *Mis à jour récemment* et *Nom (A → Z)*.
- Découverte paginée par vagues thématiques (wayland, hyprland, browser, vulkan, nvidia, gaming, etc.).
- **Historique des versions** : un clic sur la version d'un paquet AUR ouvre la liste des versions précédentes extraites de l'historique git, avec installation directe de la version choisie (`yay -Bi`).
- **Choix de build interactif** : les menus de fournisseurs (GTK3/GTK4, bibliothèques, etc.) et les questions des PKGBUILD s'affichent dans une fenêtre GTK dédiée ; Pacman reste non interactif pendant la transaction.

### Flatpak & Flathub (`flatpak` + API Flathub v2)
- Recommandations Flathub par catégories combinant le cache local AppStream et l'API Flathub v2.
- Résolution automatique des moteurs d'exécution (*Runtimes* `org.freedesktop.Platform`, `org.gnome.Platform`, etc.) et des tailles de téléchargement.

### Gestion locale des AppImages (Glisser-Déposer)
- Zone de **Glisser-Déposer (Drag & Drop)** dans l'onglet *AppImages* : déplace automatiquement les fichiers `.AppImage` dans `~/Applications` (créé automatiquement), applique la permission d'exécution (`chmod +x`), extrait l'icône embarquée et crée le raccourci `.desktop` dans le menu démarrer (`~/.local/share/applications`).
- Vérification et installation des mises à jour d'AppImages via **GitHub Releases** et suppression propre (binaire + raccourci du menu démarrer).

### Sélection multiple, Dépendances optionnelles & Gestionnaire d'installation temps réel
- Cases à cocher permettant de sélectionner plusieurs paquets (Pacman, AUR, Flatpak) et de lancer une installation groupée via le bouton **`Installer (N)`**.
- **Fenêtre contextuelle de dépendances optionnelles (`Optional Deps` Pacman & `OptDepends` AUR)** : lors de l'installation d'un paquet possédant des dépendances optionnelles non installées, une fenêtre permet de cocher à la carte les fonctionnalités et paquets complémentaires souhaités avant de lancer la transaction.
- **Gestionnaire d'installation** dédié affichant la résolution des dépendances, la taille totale de téléchargement, la barre de progression en direct, l'état individuel de chaque dépendance, les journaux console, un bouton **Annuler l'installation** avec confirmation, ainsi qu'une alerte de sécurité si l'utilisateur tente de fermer l'application pendant une transaction.

### Mises à jour globales unifiées, Cache local & Auto-redémarrage
- **Cache local instantané & Différentiel incrémental** : démarrage immédiat depuis `~/.cache/noos-package-manager/catalog_cache_v1.json`, calcul uniquement des différences locales (`pacman -Q` / `-Qqe` / `-Qqm`) et actualisation asynchrone des mises à jour en arrière-plan.
- Vérification simultanée et affichage par sections dédiées (**Pacman**, **AUR**, **Flatpak**, **AppImages**) des mises à jour disponibles (`checkupdates`, `yay -Qua`, `flatpak remote-ls --updates`, GitHub Releases).
- Détection automatique des mises à jour de `noos-package-manager` lui-même sur l'AUR avec synchronisation atomique des binaires et **redémarrage automatique** de l'application une fois sa mise à jour appliquée.
- **Mises à jour ignorées** : bouton pour ignorer un paquet dans l'onglet Mises à jour, liste dédiée des paquets ignorés et réactivation à la demande. La sélection est conservée dans `~/.config/noos-package-manager/settings.json` et exclue de « Tout mettre à jour ».
- **Services à redémarrer** : après une transaction Pacman/AUR, `needrestart -b` détecte les services utilisant d'anciennes bibliothèques. L'application propose de les redémarrer, liste les échecs avec leur statut et conseille un redémarrage complet si le noyau a été mis à jour.

### Paramètres intégrés (volet gauche)
- **Miroirs Pacman via Reflector** : analyse des miroirs récents, mesure du débit, tri par vitesse, tableau de 10 lignes défilable, application des 5 meilleurs ou d'une sélection manuelle dans `/etc/pacman.d/mirrorlist`.
- **Cache Pacman** : affichage de l'espace utilisé par `/var/cache/pacman/pkg` et nettoyage avec `paccache -r` (les 3 dernières versions de chaque paquet sont conservées).
- **Pacman** : réglage de `ParallelDownloads` (1 à 20) directement dans `/etc/pacman.conf`, avec sauvegarde et vérification par `pacman-conf`.
- **Yay** : compilation multi-cœur (`MAKEFLAGS`), désactivation de la compression (`PKGEXT`), compilation en RAM (`BUILDDIR`) et interception des choix de build, appliqués par variables d'environnement à chaque transaction AUR.

### Commande CLI `noos-up` (Ratatui)
- Utilitaire en ligne de commande avec interface **Ratatui** (`Viewport::Inline`) permettant de vérifier (`noos-up --check`) et d'appliquer (`noos-up`) en une seule commande toutes les mises à jour **Pacman**, **Yay (AUR)** et **Flatpak** avec badges colorés, barre de progression et flux temps réel.
- Mode non-TTY avec fallback textuel pour l'exécution dans des scripts ou via SSH.
- Élévation de privilèges **unique par opération** : `sudo` est validé une seule fois via un assistant `askpass` graphique intégré (aucun mot de passe n'est stocké sur disque), puis réutilisé pour toutes les commandes Pacman et AUR de l'installation ou de la mise à jour. Les transactions mixtes Pacman + AUR sont exécutées en une seule commande `yay --sudoloop`. Repli automatique sur `pkexec` (Polkit) si `sudo` est absent.

### Fiches détaillées & README intégrés
- Bouton d'information `ℹ` sur chaque paquet ouvrant une fenêtre contextuelle avec métadonnées complètes, liens directs vers le **Site officiel** et **GitHub**, et rendu natif du **README** (GitHub / GitLab / Flathub / documentation locale).

### Notifications & Logging
- **Cloche de notification** dans la barre supérieure : affiche un historique des erreurs, des succès d'installation et des mises à jour terminées. Un badge indique le nombre de notifications non lues.
- **Journal interne** (`~/.cache/noos-package-manager/noos.log`) : consigne toutes les opérations et erreurs pour le débogage.

---

## Installation

### Depuis l'AUR (Arch Linux)

Avec `yay` :
```bash
yay -S noos-package-manager
```

Manuellement avec `makepkg` :
```bash
git clone https://aur.archlinux.org/noos-package-manager.git
cd noos-package-manager
makepkg -si
```

### Utilisation en ligne de commande (`noos-up`)

Mettre à jour tous les paquets **Pacman**, **Yay (AUR)** et **Flatpak** avec l'interface CLI stylisée **Ratatui** :
```bash
noos-up
```

Vérifier uniquement les mises à jour disponibles sans les installer :
```bash
noos-up --check
```

Afficher l'aide :
```bash
noos-up --help
```

### Compilation depuis les sources

#### Prérequis système (Arch Linux)
```bash
yay -S --needed rust cargo gtk4 libadapta pacman-contrib flatpak polkit sudo reflector git
```

`needrestart` (détection des services à redémarrer) est disponible dans l'AUR / chaotic-aur ; il est déclaré comme dépendance du paquet AUR.

#### Compilation et exécution
```bash
git clone https://github.com/Chomiam/noos-package-manager.git
cd noos-package-manager
cargo build --release
./target/release/noos-package-manager
```

---

## Tests & Qualité du Code

Le projet respecte des règles strictes d'ingénierie Rust (aucun blocage du thread principal GTK4, gestion d'erreurs typée avec `thiserror`, zéro avertissement Clippy) :

```bash
cargo test --frozen --all-features
cargo clippy --all-targets -- -D warnings
cargo audit --deny warnings
```

La CI Arch Linux vérifie également `namcap` (PKGBUILD et paquet), `checksec`
(Full RELRO, PIE, NX, stack canary) et `cargo audit bin` sur le binaire construit
avec `cargo-auditable`.

## Sécurité et provenance

- **b2sums BLAKE2b-512** dans le `PKGBUILD` de l'AUR, vérifiées par `makepkg`.
- **Provenance SLSA Build L3** générée par GitHub Actions et vérifiable avec
  `slsa-verifier` pour les artefacts de release.
- **SBOM SPDX 2.3** attaché à chaque release (`sbom.spdx.json`), exploitable
  avec `syft`, `grype` ou `trivy`.
- **Publication AUR automatisée** après validation de la version et de la somme
  BLAKE2b du tarball source.
- **Actions GitHub épinglées par SHA**, image de build Arch épinglée par digest,
  Dependabot pour Cargo et GitHub Actions, et workflow OpenSSF Scorecard.
- **Durcissement** : RELRO complet, PIE, NX, stack canary et `FORTIFY_SOURCE=3`
  pour les dépendances C.

Les commandes de vérification (provenance, sommes, SBOM) sont détaillées dans
[SECURITY.md](SECURITY.md#vérifier-une-release).

---

## Structure du Projet

```
src/
├── main.rs                 # Point d'entrée GTK4/libAdapta
├── error.rs                # Gestion d'erreurs typée (AppError / AppResult)
├── backend/
│   ├── mod.rs              # Déclaration des sous-modules et réexports
│   ├── models.rs           # Modèles de données (PackageItem, PackageSource, etc.)
│   ├── logging.rs          # Journal interne (mémoire + fichier)
│   ├── catalog.rs          # Chargement, rafraîchissement et différentiel du catalogue
│   ├── install.rs          # Installation, mise à jour, suppression, dépendances optionnelles
│   ├── search.rs           # Recherche combinée Pacman / AUR / Flatpak / AppImage
│   ├── pacman.rs           # Interactions avec pacman / checkupdates
│   ├── elevation.rs        # Session sudo unique + assistant askpass par opération
│   ├── aur.rs              # Client AUR RPC v5 partagé + intégration yay
│   ├── aur_versions.rs     # Historique git des versions AUR et installation
│   ├── needrestart.rs      # Détection/redémarrage des services après mise à jour
│   ├── reflector.rs        # Analyse et application des miroirs Pacman
│   ├── settings.rs         # Configuration locale (mises à jour ignorées)
│   ├── flatpak.rs          # Intégration Flatpak / Flathub
│   ├── appimage.rs         # Gestion locale des AppImages
│   ├── details.rs          # Fiches détaillées des paquets
│   ├── cache.rs            # Cache local différentiel (catalog_cache_v1.json)
│   └── tests.rs            # Tests unitaires
└── ui/
    ├── mod.rs
    ├── window.rs           # Fenêtre principale (GTK4 / libAdapta)
    ├── askpass.rs          # Assistant graphique de mot de passe (sudo -A)
    ├── settings_page.rs    # Page Paramètres (Reflector, cache Pacman)
    ├── aur_versions_dialog.rs # Fenêtre de choix des versions antérieures AUR
    ├── sidebar.rs          # Volet latéral
    ├── row.rs              # Lignes de paquets et colonnes AUR
    ├── icons.rs            # Résolution des icônes (WhiteSur, hicolor, breeze)
    ├── style.rs            # CSS de l'application
    ├── messages.rs         # Messages échangés entre UI et backend
    ├── install_dialog.rs   # Gestionnaire d'installation et progression
    └── details_dialog.rs   # Fenêtre de détails d'un paquet
└── bin/
    └── noos_up.rs          # Outil CLI Ratatui (noos-up)
```

---

## Licence

Distribué sous licence **GPL-3.0-or-later**. Voir le fichier [LICENSE](LICENSE) pour plus de détails.
