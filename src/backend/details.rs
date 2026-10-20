use std::fs;
use std::path::Path;
use std::time::Duration;

use reqwest::Client;
use tokio::process::Command;

use crate::backend::appimage::{format_file_size, format_unix_timestamp, inspect_appimage};
use crate::backend::aur::{build_info_url, AurRpcResponse};
use crate::backend::flatpak::parse_flatpak_remote_info;
use crate::backend::models::{PackageItem, PackageSource};
use crate::error::AppResult;

#[derive(Debug, Clone)]
pub struct PackageDetailedInfo {
    pub item: PackageItem,
    pub repository: String,
    pub upstream_url: Option<String>,
    pub github_url: Option<String>,
    pub licenses: String,
    pub download_size: Option<String>,
    pub installed_size: Option<String>,
    pub maintainer_or_packager: Option<String>,
    pub build_or_modified_date: Option<String>,
    pub dependencies: Vec<String>,
    pub optional_deps: Vec<String>,
    pub readme_source_label: Option<String>,
    pub readme_markdown: Option<String>,
}

/// Résout un dépôt GitHub `(owner, repo)` à partir de l'URL amont ou du nom de paquet connu.
pub fn resolve_github_owner_repo(pkg_name: &str, upstream_url: Option<&str>) -> Option<(String, String)> {
    if let Some(url) = upstream_url {
        if let Some((owner, repo)) = extract_github_from_url(url) {
            return Some((owner, repo));
        }
    }

    let clean_name = pkg_name
        .trim()
        .to_lowercase()
        .trim_end_matches("-bin")
        .trim_end_matches("-git")
        .to_string();

    let known: Option<(&str, &str)> = match clean_name.as_str() {
        "neovim" => Some(("neovim", "neovim")),
        "alacritty" => Some(("alacritty", "alacritty")),
        "kitty" => Some(("kovidgoyal", "kitty")),
        "zed" => Some(("zed-industries", "zed")),
        "code" | "visual-studio-code" => Some(("microsoft", "vscode")),
        "btop" => Some(("aristocratos", "btop")),
        "htop" => Some(("htop-dev", "htop")),
        "fastfetch" => Some(("fastfetch-cli", "fastfetch")),
        "lazygit" => Some(("jesseduffield", "lazygit")),
        "mpv" => Some(("mpv-player", "mpv")),
        "obs-studio" => Some(("obsproject", "obs-studio")),
        "audacity" => Some(("audacity", "audacity")),
        "darktable" => Some(("darktable-org", "darktable")),
        "handbrake" => Some(("HandBrake", "HandBrake")),
        "foliate" => Some(("johnfactotum", "foliate")),
        "keepassxc" => Some(("keepassxreboot", "keepassxc")),
        "xournalpp" => Some(("xournalpp", "xournalpp")),
        "lutris" => Some(("lutris", "lutris")),
        "prismlauncher" => Some(("PrismLauncher", "PrismLauncher")),
        "mangohud" => Some(("flightlessmango", "MangoHud")),
        "gamemode" => Some(("FeralInteractive", "gamemode")),
        "winetricks" => Some(("Winetricks", "winetricks")),
        "retroarch" => Some(("libretro", "RetroArch")),
        "supertuxkart" => Some(("supertuxkart", "stk-code")),
        "timeshift" => Some(("linuxmint", "timeshift")),
        "flameshot" => Some(("flameshot-org", "flameshot")),
        "flatpak" => Some(("flatpak", "flatpak")),
        "fwupd" => Some(("fwupd", "fwupd")),
        "qbittorrent" => Some(("qbittorrent", "qBittorrent")),
        "telegram-desktop" => Some(("telegramdesktop", "tdesktop")),
        "signal-desktop" => Some(("signalapp", "Signal-Desktop")),
        "nextcloud-client" => Some(("nextcloud", "desktop")),
        "dbeaver" => Some(("dbeaver", "dbeaver")),
        "rustup" => Some(("rust-lang", "rustup")),
        "nodejs" => Some(("nodejs", "node")),
        "go" => Some(("golang", "go")),
        "docker" => Some(("moby", "moby")),
        "git" => Some(("git", "git")),
        "paru" => Some(("Morganamilo", "paru")),
        "yay" => Some(("Jguer", "yay")),
        _ => None,
    };

    known.map(|(o, r)| (o.to_string(), r.to_string()))
}

pub fn extract_github_from_url(url: &str) -> Option<(String, String)> {
    let trimmed = url.trim().trim_end_matches('/');
    let after_host = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))?;

    let mut segs = after_host.split('/');
    let owner = segs.next()?.trim();
    let repo_raw = segs.next()?.trim();
    let repo = repo_raw.strip_suffix(".git").unwrap_or(repo_raw);

    if owner.is_empty() || repo.is_empty() {
        return None;
    }

    Some((owner.to_string(), repo.to_string()))
}

/// Récupère de manière asynchrone la fiche détaillée d'un paquet (métadonnées complètes + README GitHub/amont).
pub async fn fetch_package_detailed_info(item: &PackageItem) -> AppResult<PackageDetailedInfo> {
    let http = Client::builder()
        .user_agent("noos-package-manager/0.1.0 (Arch Linux; GTK4/Libadwaita)")
        .timeout(Duration::from_secs(10))
        .connect_timeout(Duration::from_secs(5))
        .build()?;

    let mut info = match &item.source {
        PackageSource::Pacman => fetch_pacman_details(item).await?,
        PackageSource::Aur => fetch_aur_details(item, &http).await?,
        PackageSource::Flatpak => fetch_flatpak_details(item, &http).await?,
        PackageSource::AppImage(path) => fetch_appimage_details(item, path)?,
    };

    if let Some((owner, repo)) =
        resolve_github_owner_repo(&item.name, info.upstream_url.as_deref())
    {
        let gh_url = format!("https://github.com/{owner}/{repo}");
        info.github_url = Some(gh_url);

        if let Some(readme_text) = fetch_github_readme(&http, &owner, &repo).await {
            info.readme_source_label = Some(format!("README GitHub ({owner}/{repo})"));
            info.readme_markdown = Some(readme_text);
        }
    }

    // Repli sur GitLab si l'URL amont pointe vers gitlab.com ou gitlab.gnome.org / invent.kde.org
    if info.readme_markdown.is_none() {
        if let Some(ref up_url) = info.upstream_url {
            if let Some((label, readme_text)) = fetch_gitlab_like_readme(&http, up_url).await {
                info.readme_source_label = Some(label);
                info.readme_markdown = Some(readme_text);
            }
        }
    }

    // Repli local éventuel dans /usr/share/doc/<pkgname>/README*
    if info.readme_markdown.is_none() {
        if let Some((label, local_readme)) = read_local_doc_readme(&item.name) {
            info.readme_source_label = Some(label);
            info.readme_markdown = Some(local_readme);
        }
    }

    Ok(info)
}

async fn fetch_pacman_details(item: &PackageItem) -> AppResult<PackageDetailedInfo> {
    let flag = if item.is_installed { "-Qi" } else { "-Si" };
    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg(flag)
        .arg(&item.name)
        .output()
        .await?;

    let stdout = if output.status.success() {
        String::from_utf8(output.stdout)?
    } else {
        let fallback = Command::new("pacman")
            .env("LC_ALL", "C")
            .arg("-Si")
            .arg(&item.name)
            .output()
            .await?;
        String::from_utf8(fallback.stdout)?
    };

    Ok(parse_pacman_detail_fields(item, &stdout))
}

pub fn parse_pacman_detail_fields(item: &PackageItem, raw: &str) -> PackageDetailedInfo {
    let mut repository = String::from("Officiel (Pacman)");
    let mut upstream_url = None;
    let mut licenses = String::from("Non spécifiée");
    let mut download_size = None;
    let mut installed_size = None;
    let mut maintainer_or_packager = None;
    let mut build_or_modified_date = None;
    let mut dependencies = Vec::new();
    let mut optional_deps = Vec::new();
    let mut in_opt_deps = false;

    for line in raw.lines() {
        if (line.starts_with(' ') || line.starts_with('\t')) && in_opt_deps {
            let trimmed_opt = line.trim();
            if !trimmed_opt.is_empty() {
                optional_deps.push(trimmed_opt.to_string());
            }
            continue;
        }

        if let Some((key, val)) = line.split_once(':') {
            let k = key.trim();
            let v = val.trim();
            in_opt_deps = false;

            match k {
                "Repository" if !v.is_empty() => repository = format!("Dépôt officiel « {v} »"),
                "URL" if !v.is_empty() && v != "None" => upstream_url = Some(v.to_string()),
                "Licenses" if !v.is_empty() => licenses = v.to_string(),
                "Download Size" if !v.is_empty() => download_size = Some(v.to_string()),
                "Installed Size" if !v.is_empty() => installed_size = Some(v.to_string()),
                "Packager" if !v.is_empty() => maintainer_or_packager = Some(v.to_string()),
                "Build Date" if !v.is_empty() => build_or_modified_date = Some(v.to_string()),
                "Depends On" if !v.is_empty() && v != "None" => {
                    dependencies = v
                        .split_whitespace()
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(ToString::to_string)
                        .collect();
                }
                "Optional Deps" if !v.is_empty() && v != "None" => {
                    optional_deps.push(v.to_string());
                    in_opt_deps = true;
                }
                _ => {}
            }
        }
    }

    PackageDetailedInfo {
        item: item.clone(),
        repository,
        upstream_url,
        github_url: None,
        licenses,
        download_size,
        installed_size,
        maintainer_or_packager,
        build_or_modified_date,
        dependencies,
        optional_deps,
        readme_source_label: None,
        readme_markdown: None,
    }
}

async fn fetch_aur_details(item: &PackageItem, http: &Client) -> AppResult<PackageDetailedInfo> {
    let url = build_info_url(&[item.name.as_str()]);
    let resp = http.get(&url).send().await?.error_for_status()?;
    let body = resp.text().await?;
    let parsed: AurRpcResponse = serde_json::from_str(&body)?;

    if let Some(pkg) = parsed.results.into_iter().next() {
        let licenses = if pkg.license.is_empty() {
            String::from("Non spécifiée")
        } else {
            pkg.license.join(", ")
        };
        let modified_str = pkg.last_modified.map(format_unix_timestamp);

        let mut enriched_item = item.clone();
        enriched_item.aur_meta = Some(crate::backend::models::AurPackageMeta {
            votes: pkg.num_votes.unwrap_or(0),
            popularity: pkg.popularity.unwrap_or(0.0),
            maintainer: pkg.maintainer.clone(),
            last_modified: pkg.last_modified,
            out_of_date: pkg.out_of_date,
        });

        Ok(PackageDetailedInfo {
            item: enriched_item,
            repository: String::from("Arch User Repository (AUR RPC v5)"),
            upstream_url: pkg.url,
            github_url: None,
            licenses,
            download_size: None,
            installed_size: None,
            maintainer_or_packager: pkg.maintainer,
            build_or_modified_date: modified_str,
            dependencies: pkg.depends,
            optional_deps: pkg.opt_depends,
            readme_source_label: None,
            readme_markdown: None,
        })
    } else {
        Ok(PackageDetailedInfo {
            item: item.clone(),
            repository: String::from("Arch User Repository (AUR)"),
            upstream_url: Some(format!("https://aur.archlinux.org/packages/{}", item.name)),
            github_url: None,
            licenses: String::from("Non spécifiée"),
            download_size: None,
            installed_size: None,
            maintainer_or_packager: None,
            build_or_modified_date: None,
            dependencies: Vec::new(),
            optional_deps: Vec::new(),
            readme_source_label: None,
            readme_markdown: None,
        })
    }
}

fn fetch_appimage_details(item: &PackageItem, path: &Path) -> AppResult<PackageDetailedInfo> {
    let meta = inspect_appimage(path)?;
    Ok(PackageDetailedInfo {
        item: item.clone(),
        repository: format!("Fichier AppImage local ({})", path.display()),
        upstream_url: None,
        github_url: None,
        licenses: String::from("Bundle portable AppImage"),
        download_size: None,
        installed_size: Some(format_file_size(meta.size_bytes)),
        maintainer_or_packager: None,
        build_or_modified_date: Some(format_unix_timestamp(meta.modified_unix_secs)),
        dependencies: Vec::new(),
        optional_deps: Vec::new(),
        readme_source_label: None,
        readme_markdown: None,
    })
}

#[derive(serde::Deserialize)]
struct FlathubAppstreamUrls {
    homepage: Option<String>,
    vcs_browser: Option<String>,
}

#[derive(serde::Deserialize)]
struct FlathubAppstreamBundle {
    runtime: Option<String>,
    sdk: Option<String>,
}

#[derive(serde::Deserialize)]
struct FlathubAppstreamDetail {
    developer_name: Option<String>,
    project_license: Option<String>,
    description: Option<String>,
    urls: Option<FlathubAppstreamUrls>,
    bundle: Option<FlathubAppstreamBundle>,
}

fn strip_simple_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    let mut tag_buf = String::new();

    for ch in html.chars() {
        if ch == '<' {
            in_tag = true;
            tag_buf.clear();
        } else if ch == '>' {
            in_tag = false;
            let t = tag_buf.trim().to_lowercase();
            if t == "li" {
                out.push_str("\n- ");
            } else if t == "/p" || t == "br" || t == "br/" || t == "/li" {
                out.push('\n');
            }
        } else if in_tag {
            tag_buf.push(ch);
        } else {
            out.push(ch);
        }
    }

    out.trim().to_string()
}

async fn fetch_flatpak_details(
    item: &PackageItem,
    http: &Client,
) -> AppResult<PackageDetailedInfo> {
    let app_id = item.flatpak_app_id().unwrap_or(&item.name);

    let mut download_size = None;
    let mut installed_size = None;
    let mut licenses = String::from("Non spécifiée");
    let mut dependencies = Vec::new();
    let mut optional_deps = Vec::new();

    if let Ok(out) = Command::new("flatpak")
        .env("LC_ALL", "C")
        .args(["remote-info", "flathub", app_id])
        .output()
        .await
    {
        if out.status.success() {
            if let Ok(stdout) = String::from_utf8(out.stdout) {
                let info = parse_flatpak_remote_info(app_id, &stdout);
                if info.download_size_bytes > 0 {
                    download_size = Some(format_file_size(info.download_size_bytes));
                }
                if info.installed_size_bytes > 0 {
                    installed_size = Some(format_file_size(info.installed_size_bytes));
                }
                if !info.license.is_empty() {
                    licenses = info.license;
                }
                if let Some(rt) = info.runtime {
                    dependencies.push(format!("Runtime : {rt}"));
                }
                if let Some(sdk) = info.sdk {
                    optional_deps.push(format!("SDK : {sdk}"));
                }
            }
        }
    }

    let mut upstream_url = Some(format!("https://flathub.org/apps/{app_id}"));
    let mut github_url = None;
    let mut developer = None;
    let mut readme_label = None;
    let mut readme_md = None;

    let api_url = format!("https://flathub.org/api/v2/appstream/{app_id}");
    if let Ok(resp) = http.get(&api_url).send().await {
        if let Ok(detail) = resp.json::<FlathubAppstreamDetail>().await {
            if let Some(dev) = detail.developer_name.filter(|s| !s.is_empty()) {
                developer = Some(dev);
            }
            if let Some(lic) = detail.project_license.filter(|s| !s.is_empty()) {
                licenses = lic;
            }
            if let Some(urls) = detail.urls {
                if let Some(hp) = urls.homepage.filter(|s| !s.is_empty()) {
                    upstream_url = Some(hp);
                }
                if let Some(vcs) = urls.vcs_browser.filter(|s| !s.is_empty()) {
                    if extract_github_from_url(&vcs).is_some() {
                        github_url = Some(vcs);
                    } else if upstream_url.is_none() {
                        upstream_url = Some(vcs);
                    }
                }
            }
            if dependencies.is_empty() {
                if let Some(bundle) = detail.bundle {
                    if let Some(rt) = bundle.runtime {
                        dependencies.push(format!("Runtime : {rt}"));
                    }
                    if let Some(sdk) = bundle.sdk {
                        optional_deps.push(format!("SDK : {sdk}"));
                    }
                }
            }
            if let Some(desc_html) = detail.description.filter(|s| !s.is_empty()) {
                let cleaned = strip_simple_html(&desc_html);
                if !cleaned.is_empty() {
                    readme_label = Some(format!("Description officielle Flathub ({app_id})"));
                    readme_md = Some(cleaned);
                }
            }
        }
    }

    Ok(PackageDetailedInfo {
        item: item.clone(),
        repository: format!("Flathub ({app_id})"),
        upstream_url,
        github_url,
        licenses,
        download_size,
        installed_size,
        maintainer_or_packager: developer,
        build_or_modified_date: None,
        dependencies,
        optional_deps,
        readme_source_label: readme_label,
        readme_markdown: readme_md,
    })
}

async fn fetch_github_readme(http: &Client, owner: &str, repo: &str) -> Option<String> {
    let candidate_urls = [
        format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/README.md"),
        format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/Readme.md"),
        format!("https://raw.githubusercontent.com/{owner}/{repo}/HEAD/readme.md"),
        format!("https://api.github.com/repos/{owner}/{repo}/readme"),
    ];

    for url in candidate_urls {
        let mut req = http.get(&url);
        if url.contains("api.github.com") {
            req = req.header("Accept", "application/vnd.github.raw+json");
        }
        if let Ok(resp) = req.send().await {
            if resp.status().is_success() {
                if let Ok(text) = resp.text().await {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        return Some(truncate_readme(trimmed, 12_000));
                    }
                }
            }
        }
    }

    None
}

async fn fetch_gitlab_like_readme(http: &Client, upstream_url: &str) -> Option<(String, String)> {
    let trimmed = upstream_url.trim().trim_end_matches('/');
    for host in ["https://gitlab.com/", "https://gitlab.gnome.org/", "https://invent.kde.org/", "https://gitlab.freedesktop.org/"] {
        if trimmed.starts_with(host) {
            let raw_url = format!("{trimmed}/-/raw/HEAD/README.md");
            if let Ok(resp) = http.get(&raw_url).send().await {
                if resp.status().is_success() {
                    if let Ok(text) = resp.text().await {
                        let clean = text.trim();
                        if !clean.is_empty() {
                            return Some((
                                format!("README ({trimmed})"),
                                truncate_readme(clean, 12_000),
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}

fn read_local_doc_readme(pkg_name: &str) -> Option<(String, String)> {
    let doc_dir = Path::new("/usr/share/doc").join(pkg_name);
    if !doc_dir.is_dir() {
        return None;
    }
    for name in ["README.md", "README", "README.rst", "README.txt", "readme.md"] {
        let p = doc_dir.join(name);
        if let Ok(content) = fs::read_to_string(&p) {
            let trimmed = content.trim();
            if !trimmed.is_empty() {
                return Some((
                    format!("Documentation locale ({})", p.display()),
                    truncate_readme(trimmed, 12_000),
                ));
            }
        }
    }
    None
}

fn truncate_readme(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        text.to_string()
    } else {
        let mut s: String = text.chars().take(max_chars).collect();
        s.push_str("\n\n[… README tronqué pour l'affichage …]");
        s
    }
}

/// Convertit un texte Markdown (README GitHub / GitLab) en balisage Pango lisible pour GTK4.
pub fn markdown_to_pango_markup(md: &str) -> String {
    let mut out = String::with_capacity(md.len() + 512);
    let mut in_code_block = false;

    for raw_line in md.lines() {
        let trimmed = raw_line.trim();

        // Ignorer les lignes constituées uniquement de badges HTML/Markdown ou balises <img>/<p>/<div
        if trimmed.starts_with("[![")
            || trimmed.starts_with("<p ")
            || trimmed.starts_with("</p>")
            || trimmed.starts_with("<div")
            || trimmed.starts_with("</div>")
            || trimmed.starts_with("<img ")
            || trimmed.starts_with("<a ")
            || trimmed.starts_with("</a>")
            || trimmed.starts_with("<!--")
        {
            continue;
        }

        if trimmed.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if in_code_block {
            let esc = gtk4::glib::markup_escape_text(raw_line);
            out.push_str("<tt>");
            out.push_str(esc.as_str());
            out.push_str("</tt>\n");
            continue;
        }

        if let Some(h1) = trimmed.strip_prefix("# ") {
            let esc = gtk4::glib::markup_escape_text(&clean_inline_markdown(h1));
            out.push_str("\n<span size=\"large\" weight=\"bold\">");
            out.push_str(esc.as_str());
            out.push_str("</span>\n");
            continue;
        }
        if let Some(h2) = trimmed.strip_prefix("## ") {
            let esc = gtk4::glib::markup_escape_text(&clean_inline_markdown(h2));
            out.push_str("\n<span weight=\"bold\">");
            out.push_str(esc.as_str());
            out.push_str("</span>\n");
            continue;
        }
        if let Some(h3) = trimmed
            .strip_prefix("### ")
            .or_else(|| trimmed.strip_prefix("#### "))
        {
            let esc = gtk4::glib::markup_escape_text(&clean_inline_markdown(h3));
            out.push_str("\n<b>");
            out.push_str(esc.as_str());
            out.push_str("</b>\n");
            continue;
        }

        if let Some(bullet) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let esc = gtk4::glib::markup_escape_text(&clean_inline_markdown(bullet));
            out.push_str("  • ");
            out.push_str(esc.as_str());
            out.push('\n');
            continue;
        }

        let cleaned = clean_inline_markdown(trimmed);
        let esc = gtk4::glib::markup_escape_text(&cleaned);
        out.push_str(esc.as_str());
        out.push('\n');
    }

    out.trim().to_string()
}

fn clean_inline_markdown(line: &str) -> String {
    // Remplace les liens Markdown `[texte](url)` par `texte (url)` ou `texte` pour une lecture fluide
    let mut result = String::with_capacity(line.len());
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        if chars[i] == '!' && i + 1 < chars.len() && chars[i + 1] == '[' {
            // Image Markdown ![alt](url) -> ignorer
            if let Some(close_bracket) = chars[i + 2..].iter().position(|&c| c == ']') {
                let after_b = i + 2 + close_bracket + 1;
                if after_b < chars.len() && chars[after_b] == '(' {
                    if let Some(close_paren) = chars[after_b + 1..].iter().position(|&c| c == ')') {
                        i = after_b + 1 + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        if chars[i] == '[' {
            if let Some(close_bracket) = chars[i + 1..].iter().position(|&c| c == ']') {
                let label: String = chars[i + 1..i + 1 + close_bracket].iter().collect();
                let after_b = i + 1 + close_bracket + 1;
                if after_b < chars.len() && chars[after_b] == '(' {
                    if let Some(close_paren) = chars[after_b + 1..].iter().position(|&c| c == ')') {
                        result.push_str(&label);
                        i = after_b + 1 + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        result.push(chars[i]);
        i += 1;
    }

    result.replace("**", "").replace('`', "")
}
