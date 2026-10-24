use std::collections::VecDeque;
use std::io::{self, IsTerminal};
use std::process::Stdio;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, LineGauge, Paragraph};
use ratatui::{Terminal, TerminalOptions, Viewport};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Pacman,
    Yay,
    Flatpak,
}

impl SourceKind {
    fn badge_label(self) -> &'static str {
        match self {
            Self::Pacman => " PACMAN ",
            Self::Yay => "  YAY   ",
            Self::Flatpak => "FLATPAK ",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::Pacman => "Pacman (Dépôts officiels)",
            Self::Yay => "AUR (Yay)",
            Self::Flatpak => "Flatpak (Flathub)",
        }
    }

    fn color(self) -> Color {
        match self {
            Self::Pacman => Color::Rgb(53, 132, 228),
            Self::Yay => Color::Rgb(192, 97, 203),
            Self::Flatpak => Color::Rgb(46, 194, 126),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum StageState {
    Checking,
    UpToDate,
    Pending(usize),
    Updating { done: usize, total: usize },
    Done(usize),
    Failed(String),
}

#[derive(Debug, Clone)]
struct PendingUpdate {
    source: SourceKind,
    name: String,
    old_version: String,
    new_version: String,
}

#[derive(Debug)]
enum WorkerEvent {
    StyledHistoryLine(Line<'static>),
    StageChanged(SourceKind, StageState),
    PhaseChanged {
        phase: String,
        progress: f64,
    },
    LogLine(String),
    NeedSudoAuth {
        reply: tokio::sync::oneshot::Sender<bool>,
    },
    Finished {
        pacman_updated: usize,
        yay_updated: usize,
        flatpak_updated: usize,
        had_errors: bool,
    },
}

struct DashboardState {
    phase: String,
    progress: f64,
    pacman_state: StageState,
    yay_state: StageState,
    flatpak_state: StageState,
    recent_logs: VecDeque<String>,
    spinner_idx: usize,
    started_at: Instant,
    finished: Option<(usize, usize, usize, bool)>,
}

impl Default for DashboardState {
    fn default() -> Self {
        Self {
            phase: String::from("Synchronisation et recherche des mises à jour…"),
            progress: 0.04,
            pacman_state: StageState::Checking,
            yay_state: StageState::Checking,
            flatpak_state: StageState::Checking,
            recent_logs: VecDeque::with_capacity(6),
            spinner_idx: 0,
            started_at: Instant::now(),
            finished: None,
        }
    }
}

impl DashboardState {
    fn push_log(&mut self, line: String) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.recent_logs.len() >= 4 {
            self.recent_logs.pop_front();
        }
        self.recent_logs.push_back(trimmed.to_string());
    }
}

fn print_help() {
    println!(
        "noos-up v{VERSION} — Utilitaire CLI stylisé de mise à jour globale (Pacman, Yay & Flatpak)\n\n\
         UTILISATION :\n  \
           noos-up [OPTIONS]\n\n\
         OPTIONS :\n  \
           -c, --check     Vérifie et affiche les mises à jour disponibles sans les installer\n  \
           -h, --help      Affiche cet écran d'aide\n  \
           -V, --version   Affiche la version de noos-up"
    );
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut check_only = false;

    for arg in &args {
        match arg.as_str() {
            "-h" | "--help" => {
                print_help();
                return;
            }
            "-V" | "--version" => {
                println!("noos-up {VERSION}");
                return;
            }
            "-c" | "--check" => {
                check_only = true;
            }
            other => {
                eprintln!("Option inconnue : {other}\nUtilisez `noos-up --help` pour l'aide.");
                std::process::exit(2);
            }
        }
    }

    if !io::stdout().is_terminal() {
        run_non_tty_fallback(check_only).await;
        return;
    }

    let _ = crossterm::terminal::enable_raw_mode();
    let backend = CrosstermBackend::new(io::stdout());
    let terminal_res = Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(13),
        },
    );

    let Ok(mut terminal) = terminal_res else {
        let _ = crossterm::terminal::disable_raw_mode();
        run_non_tty_fallback(check_only).await;
        return;
    };

    let (tx, mut rx) = mpsc::unbounded_channel::<WorkerEvent>();
    let worker_handle = tokio::spawn(async move {
        run_update_pipeline(check_only, tx).await;
    });

    let mut state = DashboardState::default();
    let mut tick = tokio::time::interval(Duration::from_millis(80));
    let mut cancelled = false;

    let _ = insert_banner_line(&mut terminal, check_only);

    loop {
        let _ = terminal.draw(|frame| {
            render_dashboard(frame, frame.area(), &state, check_only);
        });

        if state.finished.is_some() {
            break;
        }

        if event::poll(Duration::from_millis(5)).unwrap_or(false) {
            if let Ok(Event::Key(key)) = event::read() {
                if key.code == KeyCode::Char('q')
                    || (key.code == KeyCode::Char('c')
                        && key.modifiers.contains(KeyModifiers::CONTROL))
                {
                    cancelled = true;
                    worker_handle.abort();
                    break;
                }
            }
        }

        tokio::select! {
            _ = tick.tick() => {
                state.spinner_idx = (state.spinner_idx + 1) % SPINNER_FRAMES.len();
            }
            maybe_ev = rx.recv() => {
                let Some(ev) = maybe_ev else {
                    break;
                };
                match ev {
                    WorkerEvent::StyledHistoryLine(line) => {
                        let _ = terminal.insert_before(1, |buf| {
                            Paragraph::new(line).render_ref(buf.area, buf);
                        });
                    }
                    WorkerEvent::StageChanged(source, st) => {
                        match source {
                            SourceKind::Pacman => state.pacman_state = st,
                            SourceKind::Yay => state.yay_state = st,
                            SourceKind::Flatpak => state.flatpak_state = st,
                        }
                    }
                    WorkerEvent::PhaseChanged { phase, progress } => {
                        state.phase = phase;
                        state.progress = progress.clamp(0.0, 1.0);
                    }
                    WorkerEvent::LogLine(line) => {
                        state.push_log(line);
                    }
                    WorkerEvent::NeedSudoAuth { reply } => {
                        let _ = terminal.clear();
                        let _ = crossterm::terminal::disable_raw_mode();
                        println!("\n  🔐 Authentification administrateur (sudo) requise pour mettre à jour les paquets système :");
                        println!("     Une seule saisie couvrira les mises à jour Pacman et AUR.\n");
                        let ok = std::process::Command::new(sudo_bin())
                            .arg("-v")
                            .stdin(Stdio::inherit())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit())
                            .status()
                            .map(|s| s.success())
                            .unwrap_or(false);
                        let _ = crossterm::terminal::enable_raw_mode();
                        let _ = terminal.clear();
                        let _ = reply.send(ok);
                    }
                    WorkerEvent::Finished {
                        pacman_updated,
                        yay_updated,
                        flatpak_updated,
                        had_errors,
                    } => {
                        state.progress = 1.0;
                        state.finished = Some((
                            pacman_updated,
                            yay_updated,
                            flatpak_updated,
                            had_errors,
                        ));
                    }
                }
            }
        }
    }

    let _ = terminal.draw(|frame| {
        render_dashboard(frame, frame.area(), &state, check_only);
    });
    let _ = crossterm::terminal::disable_raw_mode();
    println!();

    if cancelled {
        println!("  ⚠ Opération interrompue par l'utilisateur.");
        std::process::exit(130);
    }

    if let Some((_, _, _, true)) = state.finished {
        std::process::exit(1);
    }
}

trait RenderParagraphRef {
    fn render_ref(&self, area: Rect, buf: &mut ratatui::buffer::Buffer);
}

impl RenderParagraphRef for Paragraph<'_> {
    fn render_ref(&self, area: Rect, buf: &mut ratatui::buffer::Buffer) {
        use ratatui::widgets::Widget;
        self.clone().render(area, buf);
    }
}

fn insert_banner_line(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    check_only: bool,
) -> io::Result<()> {
    let subtitle = if check_only {
        "Mode vérification uniquement (--check)"
    } else {
        "Mise à jour unifiée Pacman · Yay (AUR) · Flatpak"
    };

    terminal.insert_before(2, |buf| {
        let lines = vec![
            Line::from(vec![
                Span::styled(
                    " ╭── ",
                    Style::default().fg(Color::Rgb(53, 132, 228)),
                ),
                Span::styled(
                    format!("NOOS-UP v{VERSION}"),
                    Style::default()
                        .fg(Color::White)
                        .bg(Color::Rgb(53, 132, 228))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    subtitle,
                    Style::default()
                        .fg(Color::Rgb(160, 195, 255))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                " │",
                Style::default().fg(Color::Rgb(53, 132, 228)),
            )),
        ];
        Paragraph::new(lines).render_ref(buf.area, buf);
    })
}

fn render_dashboard(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    state: &DashboardState,
    check_only: bool,
) {
    let spinner = SPINNER_FRAMES[state.spinner_idx];
    let elapsed = state.started_at.elapsed().as_secs_f32();

    let border_color = match state.finished {
        Some((_, _, _, false)) => Color::Rgb(46, 194, 126),
        Some((_, _, _, true)) => Color::Rgb(224, 27, 36),
        None => Color::Rgb(53, 132, 228),
    };

    let title_text = if state.finished.is_some() {
        format!(" ✔ Noos-Up terminé en {elapsed:.1}s ")
    } else if check_only {
        format!(" {spinner} Noos-Up · Vérification en cours ({elapsed:.1}s) ")
    } else {
        format!(
            " {spinner} Noos-Up · Mise à jour globale ({elapsed:.1}s) · [q] Annuler "
        )
    };

    let outer_block = Block::default()
        .title(Span::styled(
            title_text,
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color));

    let inner = outer_block.inner(area);
    frame.render_widget(outer_block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(2),
            Constraint::Min(4),
        ])
        .split(inner);

    // 1. Lignes d'état des 3 gestionnaires (Pacman, Yay, Flatpak)
    let stages_lines = vec![
        format_stage_line(SourceKind::Pacman, &state.pacman_state, spinner),
        format_stage_line(SourceKind::Yay, &state.yay_state, spinner),
        format_stage_line(SourceKind::Flatpak, &state.flatpak_state, spinner),
    ];
    frame.render_widget(Paragraph::new(stages_lines), chunks[0]);

    // 2. Barre de progression stylisée
    let gauge_color = match state.finished {
        Some((_, _, _, false)) => Color::Rgb(46, 194, 126),
        Some((_, _, _, true)) => Color::Rgb(224, 27, 36),
        None => Color::Rgb(53, 132, 228),
    };
    let gauge = LineGauge::default()
        .ratio(state.progress.clamp(0.0, 1.0))
        .label(Span::styled(
            format!(" {:>3.0}% · {}", state.progress * 100.0, state.phase),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ))
        .filled_style(
            Style::default()
                .fg(gauge_color)
                .add_modifier(Modifier::BOLD),
        )
        .unfilled_style(Style::default().fg(Color::DarkGray));
    frame.render_widget(gauge, chunks[1]);

    // 3. Journaux temps réel
    let mut log_lines: Vec<Line<'static>> = Vec::with_capacity(4);
    if state.recent_logs.is_empty() {
        log_lines.push(Line::from(Span::styled(
            "  En attente des flux de sortie…",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        for l in &state.recent_logs {
            log_lines.push(Line::from(vec![
                Span::styled("  › ", Style::default().fg(Color::Rgb(53, 132, 228))),
                Span::styled(l.clone(), Style::default().fg(Color::Gray)),
            ]));
        }
    }
    frame.render_widget(Paragraph::new(log_lines), chunks[2]);
}

fn format_stage_line(
    source: SourceKind,
    st: &StageState,
    spinner: &str,
) -> Line<'static> {
    let badge = Span::styled(
        source.badge_label(),
        Style::default()
            .fg(Color::White)
            .bg(source.color())
            .add_modifier(Modifier::BOLD),
    );
    let name_span = Span::styled(
        format!(" {:<28}", source.title()),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );

    let status_span = match st {
        StageState::Checking => Span::styled(
            format!("{spinner} Recherche des mises à jour…"),
            Style::default().fg(Color::Yellow),
        ),
        StageState::UpToDate => Span::styled(
            "✔ Aucun paquet à mettre à jour".to_string(),
            Style::default().fg(Color::Rgb(46, 194, 126)),
        ),
        StageState::Pending(count) => Span::styled(
            format!("⏳ {count} mise(s) à jour détectée(s)"),
            Style::default()
                .fg(Color::Rgb(245, 194, 17))
                .add_modifier(Modifier::BOLD),
        ),
        StageState::Updating { done, total } => Span::styled(
            format!("{spinner} Mise à jour en cours ({done}/{total})…"),
            Style::default()
                .fg(Color::Rgb(120, 185, 255))
                .add_modifier(Modifier::BOLD),
        ),
        StageState::Done(count) => Span::styled(
            format!("✔ {count} paquet(s) mis à jour avec succès"),
            Style::default()
                .fg(Color::Rgb(46, 194, 126))
                .add_modifier(Modifier::BOLD),
        ),
        StageState::Failed(err) => Span::styled(
            format!("✖ Échec : {err}"),
            Style::default()
                .fg(Color::Rgb(224, 27, 36))
                .add_modifier(Modifier::BOLD),
        ),
    };

    Line::from(vec![
        Span::raw(" "),
        badge,
        name_span,
        status_span,
    ])
}

fn make_update_item_line(upd: &PendingUpdate) -> Line<'static> {
    Line::from(vec![
        Span::styled(" ├─ ", Style::default().fg(Color::Rgb(53, 132, 228))),
        Span::styled(
            upd.source.badge_label(),
            Style::default()
                .fg(Color::White)
                .bg(upd.source.color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!("{:<28}", upd.name),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("{:>16}", upd.old_version),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(" ──▶ ", Style::default().fg(Color::Rgb(245, 194, 17))),
        Span::styled(
            upd.new_version.clone(),
            Style::default()
                .fg(Color::Rgb(46, 194, 126))
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn make_completed_step_line(source: SourceKind, message: String, ok: bool) -> Line<'static> {
    let icon = if ok { "✔" } else { "✖" };
    let icon_color = if ok {
        Color::Rgb(46, 194, 126)
    } else {
        Color::Rgb(224, 27, 36)
    };
    Line::from(vec![
        Span::styled(" │  ", Style::default().fg(Color::Rgb(53, 132, 228))),
        Span::styled(
            format!("{icon} "),
            Style::default().fg(icon_color).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            source.badge_label(),
            Style::default()
                .fg(Color::White)
                .bg(source.color())
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(message, Style::default().fg(Color::White)),
    ])
}

async fn run_update_pipeline(check_only: bool, tx: mpsc::UnboundedSender<WorkerEvent>) {
    let _ = tx.send(WorkerEvent::PhaseChanged {
        phase: String::from(
            "Vérification parallèle des dépôts Pacman, de l'AUR (yay) et de Flathub…",
        ),
        progress: 0.08,
    });

    let (pacman_upds, yay_upds, flatpak_upds) = tokio::join!(
        detect_pacman_updates(),
        detect_yay_updates(),
        detect_flatpak_updates()
    );

    if pacman_upds.is_empty() {
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Pacman,
            StageState::UpToDate,
        ));
    } else {
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Pacman,
            StageState::Pending(pacman_upds.len()),
        ));
    }

    if yay_upds.is_empty() {
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Yay,
            StageState::UpToDate,
        ));
    } else {
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Yay,
            StageState::Pending(yay_upds.len()),
        ));
    }

    if flatpak_upds.is_empty() {
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Flatpak,
            StageState::UpToDate,
        ));
    } else {
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Flatpak,
            StageState::Pending(flatpak_upds.len()),
        ));
    }

    let total_updates = pacman_upds.len() + yay_upds.len() + flatpak_upds.len();

    for upd in pacman_upds
        .iter()
        .chain(yay_upds.iter())
        .chain(flatpak_upds.iter())
    {
        let _ = tx.send(WorkerEvent::StyledHistoryLine(make_update_item_line(upd)));
    }

    if total_updates == 0 {
        let _ = tx.send(WorkerEvent::StyledHistoryLine(Line::from(vec![
            Span::styled(" ╰─ ", Style::default().fg(Color::Rgb(53, 132, 228))),
            Span::styled(
                "✔ Votre système (Pacman, Yay & Flatpak) est intégralement à jour !",
                Style::default()
                    .fg(Color::Rgb(46, 194, 126))
                    .add_modifier(Modifier::BOLD),
            ),
        ])));
        let _ = tx.send(WorkerEvent::PhaseChanged {
            phase: String::from("Système entièrement à jour."),
            progress: 1.0,
        });
        let _ = tx.send(WorkerEvent::Finished {
            pacman_updated: 0,
            yay_updated: 0,
            flatpak_updated: 0,
            had_errors: false,
        });
        return;
    }

    if check_only {
        let _ = tx.send(WorkerEvent::StyledHistoryLine(Line::from(vec![
            Span::styled(" ╰─ ", Style::default().fg(Color::Rgb(53, 132, 228))),
            Span::styled(
                format!(
                    "ℹ {total_updates} mise(s) à jour disponible(s) ({} Pacman, {} Yay, {} Flatpak). Lancez `noos-up` sans `--check` pour les appliquer.",
                    pacman_upds.len(),
                    yay_upds.len(),
                    flatpak_upds.len()
                ),
                Style::default()
                    .fg(Color::Rgb(245, 194, 17))
                    .add_modifier(Modifier::BOLD),
            ),
        ])));
        let _ = tx.send(WorkerEvent::PhaseChanged {
            phase: format!("{total_updates} mise(s) à jour détectée(s)."),
            progress: 1.0,
        });
        let _ = tx.send(WorkerEvent::Finished {
            pacman_updated: pacman_upds.len(),
            yay_updated: yay_upds.len(),
            flatpak_updated: flatpak_upds.len(),
            had_errors: false,
        });
        return;
    }

    // Si des mises à jour Pacman ou Yay sont présentes, authentifier sudo une seule fois
    // pour toute l'opération (Pacman + AUR).
    let mut use_sudo = false;
    if !pacman_upds.is_empty() || !yay_upds.is_empty() {
        use_sudo = sudo_is_usable();
        let authenticated = if use_sudo {
            let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
            let _ = tx.send(WorkerEvent::NeedSudoAuth { reply: reply_tx });
            reply_rx.await.unwrap_or(false)
        } else {
            true
        };

        if !authenticated {
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Pacman,
                StageState::Failed(String::from("Authentification administrateur annulée")),
            ));
            let _ = tx.send(WorkerEvent::PhaseChanged {
                phase: String::from("Échec de l'authentification administrateur."),
                progress: 1.0,
            });
            let _ = tx.send(WorkerEvent::Finished {
                pacman_updated: 0,
                yay_updated: 0,
                flatpak_updated: 0,
                had_errors: true,
            });
            return;
        }
        cleanup_stale_pacman_lock(use_sudo).await;
    }

    let mut had_errors = false;
    let mut pacman_done = 0usize;
    let mut yay_done = 0usize;
    let mut flatpak_done = 0usize;

    // 1. Mise à jour Pacman
    if !pacman_upds.is_empty() {
        let total = pacman_upds.len();
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Pacman,
            StageState::Updating { done: 0, total },
        ));
        let _ = tx.send(WorkerEvent::PhaseChanged {
            phase: format!(
                "Mise à jour des {total} paquet(s) Pacman (`pacman -Syu`)…"
            ),
            progress: 0.20,
        });

        let mut cmd = if use_sudo {
            let mut cmd = Command::new(sudo_bin());
            cmd.args(["-n", "pacman", "-Syu", "--noconfirm"]);
            cmd
        } else {
            let mut cmd = Command::new("pkexec");
            cmd.args(["pacman", "-Syu", "--noconfirm"]);
            cmd
        };
        let ok = run_streaming_command(
            &mut cmd,
            SourceKind::Pacman,
            total,
            0.20,
            0.50,
            &tx,
        )
        .await;

        if ok {
            pacman_done = total;
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Pacman,
                StageState::Done(total),
            ));
            let _ = tx.send(WorkerEvent::StyledHistoryLine(make_completed_step_line(
                SourceKind::Pacman,
                format!("{total} paquet(s) officiel(s) mis à jour"),
                true,
            )));
        } else {
            had_errors = true;
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Pacman,
                StageState::Failed(String::from("Erreur lors de `pacman -Syu`")),
            ));
            let _ = tx.send(WorkerEvent::StyledHistoryLine(make_completed_step_line(
                SourceKind::Pacman,
                String::from("Échec de la mise à jour Pacman"),
                false,
            )));
        }
    }

    // 2. Mise à jour AUR via Yay
    if !yay_upds.is_empty() {
        let total = yay_upds.len();
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Yay,
            StageState::Updating { done: 0, total },
        ));
        let _ = tx.send(WorkerEvent::PhaseChanged {
            phase: format!("Mise à jour des {total} paquet(s) AUR (`yay -Sua`)…"),
            progress: 0.52,
        });

        let mut cmd = Command::new("yay");
        cmd.args([
            "-Sua",
            "--noconfirm",
            "--answerclean",
            "None",
            "--answerdiff",
            "None",
            "--answeredit",
            "None",
        ]);
        if use_sudo {
            // Réutilise le ticket sudo obtenu une seule fois avant la mise à jour.
            cmd.env_remove("PACMAN_AUTH");
            cmd.arg("--sudo").arg(sudo_bin());
            cmd.arg("--sudoloop");
        }
        let ok = run_streaming_command(
            &mut cmd,
            SourceKind::Yay,
            total,
            0.52,
            0.80,
            &tx,
        )
        .await;

        if ok {
            yay_done = total;
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Yay,
                StageState::Done(total),
            ));
            let _ = tx.send(WorkerEvent::StyledHistoryLine(make_completed_step_line(
                SourceKind::Yay,
                format!("{total} paquet(s) AUR mis à jour via Yay"),
                true,
            )));
        } else {
            had_errors = true;
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Yay,
                StageState::Failed(String::from("Erreur lors de `yay -Sua`")),
            ));
            let _ = tx.send(WorkerEvent::StyledHistoryLine(make_completed_step_line(
                SourceKind::Yay,
                String::from("Échec de la mise à jour AUR (Yay)"),
                false,
            )));
        }
    }

    // 3. Mise à jour Flatpak
    if !flatpak_upds.is_empty() {
        let total = flatpak_upds.len();
        let _ = tx.send(WorkerEvent::StageChanged(
            SourceKind::Flatpak,
            StageState::Updating { done: 0, total },
        ));
        let _ = tx.send(WorkerEvent::PhaseChanged {
            phase: format!("Mise à jour des {total} application(s) Flatpak (`flatpak update`)…"),
            progress: 0.82,
        });

        let mut cmd = Command::new("flatpak");
        cmd.args(["update", "-y", "--noninteractive"]);
        let ok = run_streaming_command(
            &mut cmd,
            SourceKind::Flatpak,
            total,
            0.82,
            0.98,
            &tx,
        )
        .await;

        if ok {
            flatpak_done = total;
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Flatpak,
                StageState::Done(total),
            ));
            let _ = tx.send(WorkerEvent::StyledHistoryLine(make_completed_step_line(
                SourceKind::Flatpak,
                format!("{total} application(s) Flatpak mise(s) à jour"),
                true,
            )));
        } else {
            had_errors = true;
            let _ = tx.send(WorkerEvent::StageChanged(
                SourceKind::Flatpak,
                StageState::Failed(String::from("Erreur lors de `flatpak update`")),
            ));
            let _ = tx.send(WorkerEvent::StyledHistoryLine(make_completed_step_line(
                SourceKind::Flatpak,
                String::from("Échec de la mise à jour Flatpak"),
                false,
            )));
        }
    }

    let summary_text = if !had_errors {
        format!(
            "✔ Mise à jour terminée avec succès : {} Pacman · {} Yay (AUR) · {} Flatpak",
            pacman_done, yay_done, flatpak_done
        )
    } else {
        format!(
            "⚠ Mise à jour terminée avec des erreurs ({} Pacman · {} Yay · {} Flatpak)",
            pacman_done, yay_done, flatpak_done
        )
    };

    let _ = tx.send(WorkerEvent::StyledHistoryLine(Line::from(vec![
        Span::styled(" ╰─ ", Style::default().fg(Color::Rgb(53, 132, 228))),
        Span::styled(
            summary_text.clone(),
            Style::default()
                .fg(if had_errors {
                    Color::Rgb(224, 27, 36)
                } else {
                    Color::Rgb(46, 194, 126)
                })
                .add_modifier(Modifier::BOLD),
        ),
    ])));

    let _ = tx.send(WorkerEvent::PhaseChanged {
        phase: summary_text,
        progress: 1.0,
    });
    let _ = tx.send(WorkerEvent::Finished {
        pacman_updated: pacman_done,
        yay_updated: yay_done,
        flatpak_updated: flatpak_done,
        had_errors,
    });
}

fn sudo_bin() -> std::path::PathBuf {
    ["/usr/bin/sudo", "/usr/local/bin/sudo", "/bin/sudo"]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| std::path::PathBuf::from("sudo"))
}

fn sudo_is_usable() -> bool {
    let output = std::process::Command::new(sudo_bin())
        .env("LC_ALL", "C")
        .args(["-n", "-l"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
            stderr.contains("password")
                || stderr.contains("tty")
                || stderr.contains("askpass")
                || stderr.contains("terminal")
        }
        Err(_) => false,
    }
}

async fn cleanup_stale_pacman_lock(use_sudo: bool) {
    if !std::path::Path::new("/var/lib/pacman/db.lck").exists() {
        return;
    }
    let pacman_running = Command::new("pgrep")
        .args(["-x", "pacman"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if !pacman_running {
        let mut cmd = if use_sudo {
            let mut cmd = Command::new(sudo_bin());
            cmd.args(["-n", "rm", "-f", "/var/lib/pacman/db.lck"]);
            cmd
        } else {
            let mut cmd = Command::new("pkexec");
            cmd.args(["rm", "-f", "/var/lib/pacman/db.lck"]);
            cmd
        };
        let _ = cmd
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

async fn run_streaming_command(
    cmd: &mut Command,
    source: SourceKind,
    total_items: usize,
    start_progress: f64,
    end_progress: f64,
    tx: &mpsc::UnboundedSender<WorkerEvent>,
) -> bool {
    let Ok(mut child) = cmd
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    else {
        return false;
    };

    let (line_tx, mut line_rx) = mpsc::unbounded_channel::<String>();

    if let Some(stdout) = child.stdout.take() {
        let tx_out = line_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_out.send(line);
            }
        });
    }

    if let Some(stderr) = child.stderr.take() {
        let tx_err = line_tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let _ = tx_err.send(line);
            }
        });
    }
    drop(line_tx);

    let mut completed = 0usize;
    while let Some(line) = line_rx.recv().await {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _ = tx.send(WorkerEvent::LogLine(trimmed.to_string()));

        let lower = trimmed.to_lowercase();
        if lower.contains("upgrading ")
            || lower.contains("installing ")
            || lower.contains("updating ")
        {
            completed = (completed + 1).min(total_items);
            let _ = tx.send(WorkerEvent::StageChanged(
                source,
                StageState::Updating {
                    done: completed,
                    total: total_items,
                },
            ));
            let ratio = if total_items > 0 {
                completed as f64 / total_items as f64
            } else {
                0.5
            };
            let current_prog = start_progress + ratio * (end_progress - start_progress);
            let _ = tx.send(WorkerEvent::PhaseChanged {
                phase: format!("{} : {trimmed}", source.title()),
                progress: current_prog,
            });
        }
    }

    child
        .wait()
        .await
        .map(|status| status.success())
        .unwrap_or(false)
}

async fn detect_pacman_updates() -> Vec<PendingUpdate> {
    let output_res = Command::new("checkupdates")
        .env("LC_ALL", "C")
        .output()
        .await;

    let raw = match output_res {
        Ok(out) if out.status.success() || out.status.code() == Some(2) => {
            String::from_utf8_lossy(&out.stdout).to_string()
        }
        _ => {
            let Ok(qu) = Command::new("pacman")
                .env("LC_ALL", "C")
                .arg("-Qu")
                .output()
                .await
            else {
                return Vec::new();
            };
            String::from_utf8_lossy(&qu.stdout).to_string()
        }
    };

    parse_arrow_updates(&raw, SourceKind::Pacman)
}

async fn detect_yay_updates() -> Vec<PendingUpdate> {
    let Ok(out) = Command::new("yay")
        .env("LC_ALL", "C")
        .arg("-Qua")
        .output()
        .await
    else {
        return Vec::new();
    };

    if out.status.success() || out.status.code() == Some(1) {
        let raw = String::from_utf8_lossy(&out.stdout);
        parse_arrow_updates(&raw, SourceKind::Yay)
    } else {
        Vec::new()
    }
}

async fn detect_flatpak_updates() -> Vec<PendingUpdate> {
    let Ok(out) = Command::new("flatpak")
        .args([
            "remote-ls",
            "--updates",
            "--app",
            "--columns=application,version,branch",
        ])
        .output()
        .await
    else {
        return Vec::new();
    };

    if !out.status.success() {
        return Vec::new();
    }

    let raw = String::from_utf8_lossy(&out.stdout);
    let mut list = Vec::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let cols: Vec<&str> = trimmed.split('\t').collect();
        let Some(app_id) = cols.first().map(|s| s.trim()) else {
            continue;
        };
        if app_id.is_empty() || app_id.eq_ignore_ascii_case("Application ID") {
            continue;
        }
        let ver = cols.get(1).map(|s| s.trim()).unwrap_or("");
        let branch = cols.get(2).map(|s| s.trim()).unwrap_or("stable");
        let new_version = if !ver.is_empty() {
            ver.to_string()
        } else {
            branch.to_string()
        };
        list.push(PendingUpdate {
            source: SourceKind::Flatpak,
            name: app_id.to_string(),
            old_version: branch.to_string(),
            new_version,
        });
    }
    list
}

fn parse_arrow_updates(raw: &str, source: SourceKind) -> Vec<PendingUpdate> {
    let mut updates = Vec::new();
    for line in raw.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 4 && parts[2] == "->" {
            updates.push(PendingUpdate {
                source,
                name: parts[0].to_string(),
                old_version: parts[1].to_string(),
                new_version: parts[3].to_string(),
            });
        }
    }
    updates
}

async fn run_non_tty_fallback(check_only: bool) {
    println!("╭── NOOS-UP v{VERSION} (Pacman · Yay · Flatpak) ──╮");
    let (pacman_upds, yay_upds, flatpak_upds) = tokio::join!(
        detect_pacman_updates(),
        detect_yay_updates(),
        detect_flatpak_updates()
    );
    let total = pacman_upds.len() + yay_upds.len() + flatpak_upds.len();
    for u in pacman_upds
        .iter()
        .chain(yay_upds.iter())
        .chain(flatpak_upds.iter())
    {
        println!(
            " ├─ [{}] {:<26} {} ──▶ {}",
            u.source.badge_label().trim(),
            u.name,
            u.old_version,
            u.new_version
        );
    }
    if total == 0 {
        println!("╰── ✔ Tous vos paquets Pacman, Yay et Flatpak sont à jour.");
        return;
    }
    if check_only {
        println!(
            "╰── ℹ {total} mise(s) à jour disponible(s) ({} Pacman, {} Yay, {} Flatpak).",
            pacman_upds.len(),
            yay_upds.len(),
            flatpak_upds.len()
        );
        return;
    }
    if !pacman_upds.is_empty() {
        let _ = Command::new("pkexec")
            .args(["pacman", "-Syu", "--noconfirm"])
            .status()
            .await;
    }
    if !yay_upds.is_empty() {
        let _ = Command::new("yay")
            .args([
                "-Sua",
                "--noconfirm",
                "--answerclean",
                "None",
                "--answerdiff",
                "None",
                "--answeredit",
                "None",
            ])
            .status()
            .await;
    }
    if !flatpak_upds.is_empty() {
        let _ = Command::new("flatpak")
            .args(["update", "-y", "--noninteractive"])
            .status()
            .await;
    }
    println!("╰── ✔ Mise à jour globale terminée.");
}
