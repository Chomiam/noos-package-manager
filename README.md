# Noos Package Manager

**Noos Package Manager** est un gestionnaire de paquets graphique moderne et unifié pour **Arch Linux**, écrit intégralement en **Rust** avec **GTK4** et **Libadwaita**.

Il réunit au sein d'une seule interface fluide (compatible avec les thèmes GNOME et WhiteSur) les **dépôts officiels Pacman**, l'**Arch User Repository (AUR RPC v5 + `yay`/`paru`)**, les applications **Flatpak / Flathub** ainsi que la gestion locale complète des **AppImages**.

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

- **Dépôts officiels Pacman (`pacman` & `checkupdates`)** :
  - Recommandations organisées par catégories (*Navigateurs & Internet*, *Multimédia, Audio & Création*, *Bureautique & Productivité*, *Développement & Programmation*, *Jeux & Gaming*, *Système & Utilitaires*) avec les paquets non installés présentés en priorité.
  - Résolution automatique des logos d'applications (WhiteSur, hicolor, breeze, Flathub appstream, `.desktop`).
- **Arch User Repository — AUR (`RPC v5` + `yay` / `paru`)** :
  - Affichage tabulaire aligné en colonnes : **Votes (`★`)**, **Popularité**, **Mainteneur** (avec détection des paquets orphelins), **Date de dernière mise à jour**, **Version** et badge d'alerte **Obsolète (`Out-of-Date`)**.
  - Sélecteur de tri dédié dans l'onglet AUR : *Popularité (par défaut)*, *Votes*, *Mis à jour récemment* et *Nom (A → Z)*.
- **Flatpak & Flathub (`flatpak` + API Flathub v2)** :
  - Recommandations Flathub par catégories combinant le cache local AppStream et l'API Flathub v2.
  - Résolution automatique des moteurs d'exécution (*Runtimes* `org.freedesktop.Platform`, `org.gnome.Platform`, etc.) et des tailles de téléchargement.
- **Gestion locale des AppImages (Glisser-Déposer)** :
  - Zone de **Glisser-Déposer (Drag & Drop)** dans l'onglet *AppImages* : déplace automatiquement les fichiers `.AppImage` dans `~/Applications` (créé automatiquement), applique la permission d'exécution (`chmod +x`), extrait l'icône embarquée et crée le raccourci `.desktop` dans le menu démarrer (`~/.local/share/applications`).
  - Vérification et installation des mises à jour d'AppImages via **GitHub Releases** et suppression propre (binaire + raccourci du menu démarrer).
- **Sélection multiple, Dépendances optionnelles & Gestionnaire d'installation temps réel** :
  - Cases à cocher permettant de sélectionner plusieurs paquets (Pacman, AUR, Flatpak) et de lancer une installation groupée via le bouton **`Installer (N)`**.
  - **Fenêtre contextuelle de dépendances optionnelles (`Optional Deps` Pacman & `OptDepends` AUR)** : lors de l'installation d'un paquet possédant des dépendances optionnelles non installées, une fenêtre permet de cocher à la carte les fonctionnalités et paquets complémentaires souhaités avant de lancer la transaction.
  - **Gestionnaire d'installation** dédié affichant la résolution des dépendances, la taille totale de téléchargement, la barre de progression en direct, l'état individuel de chaque dépendance, les journaux console, un bouton **Annuler l'installation** avec confirmation, ainsi qu'une alerte de sécurité si l'utilisateur tente de fermer l'application pendant une transaction.
- **Mises à jour globales unifiées & catégorisées** :
  - Vérification simultanée et affichage par sections dédiées (**Pacman**, **AUR**, **Flatpak**, **AppImages**) des mises à jour disponibles (`checkupdates`, `yay -Qua` / `paru -Qua`, `flatpak remote-ls --updates`, GitHub Releases).
- **Fiches détaillées & README intégrés** :
  - Bouton d'information `ℹ` sur chaque paquet ouvrant une fenêtre contextuelle avec métadonnées complètes, liens directs vers le **Site officiel** et **GitHub**, et rendu natif du **README** (GitHub / GitLab / Flathub / documentation locale).

---

## Installation

### Depuis l'AUR (Arch Linux)

Avec `yay` :
```bash
yay -S noos-package-manager
```

Avec `paru` :
```bash
paru -S noos-package-manager
```

Manuellement avec `makepkg` :
```bash
git clone https://aur.archlinux.org/noos-package-manager.git
cd noos-package-manager
makepkg -si
```

### Compilation depuis les sources

#### Prérequis système (Arch Linux)
```bash
sudo pacman -S --needed rust cargo gtk4 libadwaita pacman-contrib flatpak polkit git
```

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
cargo test
cargo clippy --all-targets -- -D warnings
```

---

## Licence

Distribué sous licence **GPL-3.0-or-later**. Voir le fichier [LICENSE](LICENSE) pour plus de détails.
