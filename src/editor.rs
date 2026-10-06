//! rust-nano — éditeur de code TUI pour Rust, thème « Minuit » profond.
//!
//! Fenêtres arrondies façon lazy.nvim, explorateur de fichiers, recherche
//! flottante, vérification rustc en un raccourci, rustfmt à la sauvegarde,
//! toasts, statusline segmentée. Tab = 4 espaces, toujours.

use crate::explorer::{Explorer, FileSearch};
use crate::highlight;
use crate::norme::Severity;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use unicode_width::UnicodeWidthStr;


/// Largeur du panneau explorateur (bordure comprise).
const EXPL_W: u16 = 30;

// ------------------------------------------------------------------ thème

/// Palette « Minuit » — Apple sobre : neutres charbon + accents pastel.
struct Ed;

#[allow(dead_code)]
impl Ed {
    /// Fond de l'éditeur — charbon profond, plus sombre que lazy.nvim.
    fn bg() -> Color {
        Color::Rgb(0x14, 0x14, 0x16)
    }
    /// Fond des barres haute/basse — un cran plus sombre.
    fn bar_bg() -> Color {
        Color::Rgb(0x0F, 0x0F, 0x10)
    }
    /// Ligne courante — presque imperceptible.
    fn cur_line() -> Color {
        Color::Rgb(0x1E, 0x1E, 0x21)
    }
    /// Sélection de l'explorateur — une vraie bande : on navigue, on la voit.
    fn sel_row() -> Color {
        Color::Rgb(0x2E, 0x2E, 0x33)
    }
    fn text() -> Color {
        Color::Rgb(0xF5, 0xF5, 0xF7)
    }
    fn dim() -> Color {
        Color::Rgb(0x85, 0x85, 0x8A)
    }
    fn gutter() -> Color {
        Color::Rgb(0x3E, 0x3E, 0x42)
    }
    /// Corail — signature du thème (mot-clés, indicateur modifié).
    fn accent() -> Color {
        Color::Rgb(0xFF, 0x7A, 0x93)
    }
    /// Orange rouille — la marque rust-nano (bloc brand, titre du modal).
    fn brand() -> Color {
        Color::Rgb(0xDE, 0xA5, 0x84)
    }
    /// Cyan Apple — touches, IA en vol.
    fn cyan() -> Color {
        Color::Rgb(0x64, 0xD2, 0xFF)
    }
    /// Vert tendre — succès (norme ✓, build propre).
    fn green() -> Color {
        Color::Rgb(0xA6, 0xDA, 0x95)
    }
    /// Ambre Apple — avertissements, norme mineure.
    fn amber() -> Color {
        Color::Rgb(0xFF, 0xD6, 0x0A)
    }
    /// Rouge Apple — erreurs, norme majeure.
    fn red() -> Color {
        Color::Rgb(0xFF, 0x45, 0x3A)
    }

    /// Règle de la colonne 80 — un filet, rien de plus.
    fn ruler() -> Color {
        Color::Rgb(0x2E, 0x2E, 0x31)
    }
}

// ------------------------------------------------------------------ diagnostics

/// Un diagnostic de compilation (gcc), ligne/colonne 1-based.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Diag {
    line: usize,
    col: usize,
    is_error: bool,
    msg: String,
}

/// Parse la sortie stderr de rustc (`--error-format=short`) :
/// ne garde que les lignes du fichier courant, au format
/// `fichier:ligne:col: error[E0308]|warning: message`.
fn parse_diagnostics(stderr: &str, path: &str) -> Vec<Diag> {
    let prefix = format!("{path}:");
    let mut out = Vec::new();
    for l in stderr.lines() {
        let Some(rest) = l.strip_prefix(&prefix) else {
            continue;
        };
        for (tag, is_error) in [(": error", true), (": warning", false)] {
            if let Some(pos) = rest.find(tag) {
                let head = &rest[..pos]; // "ligne:col"
                // après « error » : «[E0308]: msg » ou «: msg »
                let after = &rest[pos + tag.len()..];
                let msg = if after.starts_with('[') {
                    after
                        .find(": ")
                        .map(|i| after[i + 2..].trim().to_string())
                        .unwrap_or_default()
                } else {
                    after.strip_prefix(": ").unwrap_or(after).trim().to_string()
                };
                let mut it = head.split(':');
                let Ok(line) = it.next().unwrap_or("0").trim().parse::<usize>() else {
                    break;
                };
                let col = it
                    .next()
                    .and_then(|c| c.trim().parse().ok())
                    .unwrap_or(1);
                if line > 0 {
                    out.push(Diag {
                        line,
                        col,
                        is_error,
                        msg,
                    });
                }
                break;
            }
        }
    }
    out
}

// ------------------------------------------------------------------ focus & notifications

/// Zone qui détient le focus — Alt-Tab les fait défiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Editor,
    Explorer,
    Search,
}

/// Niveau d'une notification toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Level {
    Info,
    Ok,
    Warn,
    Err,
}

/// Notification flottante façon nvim-notify : éphémère, empilée en haut
/// à droite, colorée par niveau.
struct Toast {
    level: Level,
    text: String,
    at: Instant,
}

// ------------------------------------------------------------------ éditeur

pub struct Editor {
    lines: Vec<String>,
    cx: usize,
    cy: usize,
    scroll_x: usize,
    scroll_y: usize,
    file: Option<PathBuf>,
    modified: bool,
    status: String,
    clipboard: String,
    should_quit: bool,
    confirm_quit: bool,
    /// dernier bilan norme (après sauvegarde) + marques de marge
    norme_note: Option<String>,
    norme_marks: Vec<(usize, Severity)>,
    norme_rx: Option<Receiver<(String, Vec<(usize, Severity)>, Vec<(usize, Severity, String)>)>>,
    /// diagnostics gcc du dernier build (^B) + navigation ^N/^P
    diags: Vec<Diag>,
    diag_rx: Option<Receiver<Result<Vec<Diag>, String>>>,
    diag_idx: Option<usize>,
    /// saisie de recherche (^F) ou goto (^G) en cours
    prompt: Option<(char, String)>,
    /// extension du fichier (pour la coloration)
    ext: String,
    /// explorateur de fichiers (^T, ou `c-nano <dossier>`)
    explorer: Option<Explorer>,
    /// recherche de fichiers flottante (^O, façon Telescope)
    search: Option<FileSearch>,
    /// zone qui a le focus (Alt-Tab cycle : éditeur → explorateur → recherche)
    focus: Focus,
    /// notifications toast empilées (haut à droite)
    toasts: Vec<Toast>,
    /// branche git du projet (barre haute)
    git_branch: Option<String>,
    /// findings de norme détaillés (ligne, sévérité, message) — survol
    norme_details: Vec<(usize, Severity, String)>,
    /// historique pour l'undo (Ctrl+Z)
    history: Vec<(Vec<String>, usize, usize)>,
}

/// Dossier de travail : celui du fichier ouvert, sinon le répertoire courant.
fn file_dir(path: Option<&Path>) -> PathBuf {
    path.and_then(|p| p.parent().map(Path::to_path_buf))
        .filter(|p| !p.as_os_str().is_empty())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Branche git du dossier (None hors dépôt — jamais bloquant).
fn detect_git_branch(dir: PathBuf) -> Option<String> {
    let out = Command::new("git")
        .args(["-C"])
        .arg(&dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() || name == "HEAD" {
        None
    } else {
        Some(name)
    }
}

impl Editor {
    pub fn open(path: Option<&Path>) -> io::Result<Self> {
        let (lines, file) = match path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(p)?;
                let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                if lines.is_empty() {
                    lines.push(String::new());
                }
                (lines, Some(p.to_path_buf()))
            }
            Some(p) => (vec![String::new()], Some(p.to_path_buf())),
            None => (vec![String::new()], None),
        };
        let ext = file
            .as_ref()
            .and_then(|p| p.extension())
            .and_then(|e| e.to_str())
            .unwrap_or("rs")
            .to_string();
        let _ = file; // l'extension est déjà extraite
        Ok(Self {
            lines,
            cx: 0,
            cy: 0,
            scroll_x: 0,
            scroll_y: 0,
            file,
            modified: false,
            status: "Alt+Tab panneaux · ^O rechercher · ^B vérifier · ^S sauver · ^Q quitter".to_string(),
            clipboard: String::new(),
            should_quit: false,
            confirm_quit: false,
            norme_note: None,
            norme_marks: Vec::new(),
            norme_rx: None,
            diags: Vec::new(),
            diag_rx: None,
            diag_idx: None,
            prompt: None,
            history: Vec::new(),
            ext,
            explorer: None,
            search: None,
            focus: Focus::Editor,
            toasts: Vec::new(),
            git_branch: detect_git_branch(file_dir(path)),
            norme_details: Vec::new(),
        })
    }

    // -------------------------------------------------------------- texte

    fn line(&self) -> &str {
        &self.lines[self.cy]
    }

    /// Écran d'accueil : aucun fichier, buffer vierge et jamais modifié.
    fn is_welcome(&self) -> bool {
        self.file.is_none()
            && !self.modified
            && self.lines.len() == 1
            && self.lines[0].is_empty()
    }

    /// Sauvegarde l'état avant une modification (pour Ctrl+Z).
    fn snapshot(&mut self) {
        self.history
            .push((self.lines.clone(), self.cx, self.cy));
        if self.history.len() > 200 {
            self.history.remove(0);
        }
    }

    fn undo(&mut self) {
        if let Some((lines, cx, cy)) = self.history.pop() {
            self.lines = lines;
            self.cx = cx;
            self.cy = cy;
            self.modified = true;
            self.status = "annulé".into();
        }
    }

    fn insert_char(&mut self, c: char) {
        self.snapshot();
        let line = &mut self.lines[self.cy];
        line.insert(self.cx, c);
        self.cx += 1;
        self.modified = true;
    }

    /// Fermante associée à une ouvrante (auto-paires).
    fn pair_for(c: char) -> Option<char> {
        Some(match c {
            '(' => ')',
            '[' => ']',
            '{' => '}',
            '"' => '"',
            '\'' => '\'',
            _ => return None,
        })
    }

    /// Frappe d'un caractère : auto-paires à la Xcode —
    /// l'ouvrante insère la paire (curseur au milieu), la fermante déjà
    /// présente est survolée, tout le reste est inséré normalement.
    fn type_char(&mut self, c: char) {
        let next = self.lines[self.cy].as_bytes().get(self.cx).copied();
        // survol : la fermante tapée alors qu'elle est déjà sous le curseur
        if matches!(c, ')' | ']' | '}' | '"' | '\'') && next == Some(c as u8) {
            self.cx += 1;
            return;
        }
        if let Some(close) = Self::pair_for(c) {
            self.snapshot();
            let line = &mut self.lines[self.cy];
            line.insert(self.cx, close);
            line.insert(self.cx, c);
            self.cx += 1;
            self.modified = true;
            return;
        }
        self.insert_char(c);
    }

    fn insert_newline(&mut self) {
        self.snapshot();
        let rest = self.lines[self.cy].split_off(self.cx);
        // indentation automatique : reprend l'indentation de la ligne courante
        let indent: String = self.lines[self.cy]
            .chars()
            .take_while(|c| c.is_whitespace())
            .collect();
        let indent_len = indent.len();
        self.cy += 1;
        self.lines.insert(self.cy, indent + &rest);
        self.cx = indent_len;
        self.modified = true;
    }

    fn backspace(&mut self) {
        self.snapshot();
        if self.cx > 0 {
            // paire vide sous le curseur : « (|) » → les deux partent
            let pair = {
                let b = self.lines[self.cy].as_bytes();
                self.cx < b.len()
                    && Self::pair_for(b[self.cx - 1] as char) == Some(b[self.cx] as char)
            };
            let line = &mut self.lines[self.cy];
            line.remove(self.cx - 1);
            if pair {
                line.remove(self.cx - 1); // la fermante a glissé d'un cran
            }
            self.cx -= 1;
            self.modified = true;
        } else if self.cy > 0 {
            let cur = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.lines[self.cy].len();
            self.lines[self.cy].push_str(&cur);
            self.modified = true;
        }
    }

    fn move_left(&mut self) {
        if self.cx > 0 {
            self.cx -= 1;
        } else if self.cy > 0 {
            self.cy -= 1;
            self.cx = self.lines[self.cy].len();
        }
    }

    fn move_right(&mut self) {
        if self.cx < self.line().len() {
            self.cx += 1;
        } else if self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = 0;
        }
    }

    fn move_up(&mut self) {
        if self.cy > 0 {
            self.cy -= 1;
            self.cx = self.cx.min(self.lines[self.cy].len());
        }
    }

    fn move_down(&mut self) {
        if self.cy + 1 < self.lines.len() {
            self.cy += 1;
            self.cx = self.cx.min(self.lines[self.cy].len());
        }
    }

    /// Remplace toutes les occurrences de « motif→remplacement » (séparateur →).
    fn replace_all(&mut self, spec: &str) {
        let Some((needle, repl)) = spec.split_once('→').or_else(|| spec.split_once("->")) else {
            self.status = "syntaxe : motif→remplacement".into();
            return;
        };
        if needle.is_empty() {
            return;
        }
        self.snapshot();
        let mut count = 0;
        for line in &mut self.lines {
            if line.contains(needle) {
                let n = line.matches(needle).count();
                *line = line.replace(needle, repl);
                count += n;
            }
        }
        self.modified = count > 0;
        self.status = format!("{count} remplacement(s)");
    }

    fn find(&mut self, needle: &str) {
        if needle.is_empty() {
            return;
        }
        let n = self.lines.len();
        for k in 0..n {
            let row = (self.cy + k) % n;
            if let Some(col) = self.lines[row].find(needle) {
                if row == self.cy && col <= self.cx && k == 0 {
                    continue; // déjà dessus, cherche la suivante
                }
                self.cy = row;
                self.cx = col;
                self.status = format!("« {needle} » — ligne {}", row + 1);
                return;
            }
        }
        self.status = format!("« {needle} » introuvable");
    }

    fn save(&mut self) {
        let Some(path) = self.file.clone() else {
            self.status = "pas de nom de fichier — lance avec : c-nano <fichier>".into();
            return;
        };
        let mut text = self.lines.join("\n");
        text.push('\n'); // C-A3 : newline final, toujours
        match std::fs::write(&path, text) {
            Ok(()) => {
                self.modified = false;
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("fichier")
                    .to_string();
                self.notify(Level::Ok, format!("{name} — sauvegardé ✓"));
                // rustfmt après chaque sauvegarde (non bloquant) :
                // la note remonte dans la barre — pas de marques de marge
                let p = path.clone();
                let (tx, rx) = channel();
                std::thread::spawn(move || {
                    let payload = match Command::new("rustfmt")
                        .args(["--check", "--edition", "2021"])
                        .arg(&p)
                        .output()
                    {
                        Ok(o) if o.status.success() => {
                            ("rustfmt ✓".to_string(), Vec::new(), Vec::new())
                        }
                        Ok(_) => ("rustfmt : à formater".to_string(), Vec::new(), Vec::new()),
                        Err(_) => (String::new(), Vec::new(), Vec::new()),
                    };
                    let _ = tx.send(payload);
                });
                self.norme_rx = Some(rx);
            }
            Err(e) => self.status = format!("erreur d'écriture : {e}"),
        }
    }

    // -------------------------------------------------------------- build

    /// ^B : compile le fichier courant avec gcc (syntaxe seule, warnings
    /// complets) et verse les diagnostics dans la marge. Non bloquant.
    fn build(&mut self) {
        let Some(path) = self.file.clone() else {
            self.status = "pas de fichier — lance avec : rust-nano <fichier.rs>".into();
            return;
        };
        if self.modified {
            self.save();
        }
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let out = Command::new("rustc")
                .args([
                    "--edition",
                    "2021",
                    "--crate-type",
                    "lib",
                    "--emit=metadata",
                    "--error-format=short",
                    "--out-dir",
                ])
                .arg(std::env::temp_dir())
                .arg(&path)
                .output();
            let res = match out {
                Ok(o) => Ok(parse_diagnostics(
                    &String::from_utf8_lossy(&o.stderr),
                    &path.display().to_string(),
                )),
                Err(e) => Err(format!("rustc introuvable ({e})")),
            };
            let _ = tx.send(res);
        });
        self.diag_rx = Some(rx);
        self.status = "vérification…".into();
    }

    fn poll_diag(&mut self) {
        let Some(rx) = &self.diag_rx else {
            return;
        };
        if let Ok(res) = rx.try_recv() {
            self.diag_rx = None;
            match res {
                Ok(diags) => {
                    self.diags = diags;
                    if self.diags.is_empty() {
                        self.diag_idx = None;
                        self.notify(Level::Ok, "build ✓ — propre");
                    } else {
                        let errs = self.diags.iter().filter(|d| d.is_error).count();
                        let warns = self.diags.len() - errs;
                        self.notify(
                            Level::Err,
                            format!("build : {errs} erreur(s), {warns} avert. — ^N/^P naviguer"),
                        );
                        self.diag_idx = None;
                        self.diag_jump(1); // saute au premier problème
                    }
                }
                Err(e) => self.notify(Level::Err, e),
            }
        }
    }

    /// ^N / ^P : saute au diagnostic suivant / précédent (circulaire).
    fn diag_jump(&mut self, dir: i32) {
        if self.diags.is_empty() {
            self.status = "aucun diagnostic — ^B pour compiler".into();
            return;
        }
        let n = self.diags.len();
        let idx = match self.diag_idx {
            Some(i) => (i as i32 + dir).rem_euclid(n as i32) as usize,
            None => 0,
        };
        self.diag_idx = Some(idx);
        let d = self.diags[idx].clone();
        self.cy = (d.line - 1).min(self.lines.len() - 1);
        self.cx = d.col.saturating_sub(1).min(self.lines[self.cy].len());
        let kind = if d.is_error { "✗" } else { "⚠" };
        let msg: String = d.msg.chars().take(72).collect();
        self.status = format!("{kind} {}/{n} · {}:{} · {msg}", idx + 1, d.line, d.col);
    }

    fn poll_norme(&mut self) {
        let Some(rx) = &self.norme_rx else {
            return;
        };
        if let Ok((note, marks, details)) = rx.try_recv() {
            if !note.is_empty() {
                let level = if note.ends_with('✓') { Level::Ok } else { Level::Warn };
                self.notify(level, note.clone());
                self.norme_note = Some(note);
                self.norme_marks = marks;
                self.norme_details = details;
            }
            self.norme_rx = None;
        }
    }

    /// Notification toast (coin haut-droit) + le statut garde le dernier état.
    fn notify(&mut self, level: Level, text: impl Into<String>) {
        let text = text.into();
        self.status = text.clone();
        self.toasts.push(Toast {
            level,
            text,
            at: Instant::now(),
        });
        if self.toasts.len() > 8 {
            self.toasts.remove(0);
        }
    }

    // -------------------------------------------------------------- focus

    /// Alt-Tab : éditeur → explorateur → recherche → éditeur.
    /// Les panneaux absents sont sautés ; sans aucun panneau, ouvre
    /// l'explorateur (le geste sert toujours).
    fn cycle_focus(&mut self) {
        let next = match self.focus {
            Focus::Editor => {
                if self.explorer.is_some() {
                    Some(Focus::Explorer)
                } else if self.search.is_some() {
                    Some(Focus::Search)
                } else {
                    self.toggle_explorer();
                    None
                }
            }
            Focus::Explorer => {
                if self.search.is_some() {
                    Some(Focus::Search)
                } else {
                    Some(Focus::Editor)
                }
            }
            Focus::Search => Some(Focus::Editor),
        };
        if let Some(f) = next {
            self.focus = f;
        }
    }

    // -------------------------------------------------------------- recherche

    /// ^O : la recherche de fichiers flottante (façon Telescope).
    fn toggle_search(&mut self) {
        if self.search.take().is_some() {
            if self.focus == Focus::Search {
                self.focus = Focus::Editor;
            }
            return;
        }
        let root = self
            .explorer
            .as_ref()
            .map(|e| e.root.clone())
            .unwrap_or_else(|| file_dir(self.file.as_deref()));
        self.search = Some(FileSearch::new(root));
        self.focus = Focus::Search;
        self.status = "tapez pour filtrer · ↑↓ naviguer · Enter ouvrir · Échap fermer".into();
    }

    /// Touches quand le focus est sur la recherche flottante.
    fn search_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('x') => self.request_quit(),
                KeyCode::Char('s') => self.save(),
                KeyCode::Char('b') => self.build(),
                KeyCode::Char('t') => self.toggle_explorer(),
                KeyCode::Char('o') => self.toggle_search(),
                _ => {}
            }
            return;
        }
        let Some(fs) = &mut self.search else { return };
        match key.code {
            KeyCode::Up => fs.move_up(),
            KeyCode::Down => fs.move_down(),
            KeyCode::Home => fs.home(),
            KeyCode::End => fs.end(),
            KeyCode::Backspace => fs.pop(),
            KeyCode::Esc => {
                self.search = None;
                self.focus = Focus::Editor;
            }
            KeyCode::Enter => {
                if let Some(path) = fs.sel_path() {
                    self.search = None;
                    self.focus = Focus::Editor;
                    self.open_from_explorer(path);
                }
            }
            KeyCode::Char(c) => fs.push(c),
            _ => {}
        }
    }

    // -------------------------------------------------------------- explorateur

    /// Quitter : confirmation si le buffer est modifié.
    fn request_quit(&mut self) {
        if self.modified {
            self.confirm_quit = true;
            self.status = "modifié — o quitter sans sauver · s sauver+quitter · autre: rester".into();
        } else {
            self.should_quit = true;
        }
    }

    /// ^T : ouvre/ferme le panneau de fichiers (racine : dossier du fichier,
    /// sinon le répertoire courant).
    fn toggle_explorer(&mut self) {
        if self.explorer.take().is_some() {
            if self.focus == Focus::Explorer {
                self.focus = Focus::Editor;
            }
            self.status = "explorateur fermé".into();
            return;
        }
        let root = self
            .file
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .filter(|p| !p.as_os_str().is_empty())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        self.explorer = Some(Explorer::new(root));
        self.focus = Focus::Explorer;
        self.status =
            "↑↓ naviguer · Enter ouvrir · →/← plier · Home/End extrémités · tapez pour filtrer · ^T fermer".into();
    }

    /// Ouvre le fichier choisi dans l'explorateur (jamais par-dessus un
    /// buffer modifié — on sauvegarde d'abord, la donnée est sacrée).
    fn open_from_explorer(&mut self, path: PathBuf) {
        if self.modified {
            self.status = "buffer modifié — ^S pour sauvegarder d'abord".into();
            return;
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
                if lines.is_empty() {
                    lines.push(String::new());
                }
                self.lines = lines;
                self.ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("rs")
                    .to_string();
                self.cx = 0;
                self.cy = 0;
                self.scroll_x = 0;
                self.scroll_y = 0;
                self.history.clear();
                self.diags.clear();
                self.diag_idx = None;
                self.norme_marks.clear();
                self.norme_note = None;
                self.focus = Focus::Editor;
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("fichier")
                    .to_string();
                self.notify(Level::Info, format!("{name} — ouvert"));
                self.file = Some(path);
            }
            Err(e) => self.notify(Level::Err, format!("lecture impossible : {e}")),
        }
    }

    /// Touches quand le focus est sur l'explorateur : les globales (^Q, ^S,
    /// ^B, ^T) passent partout, le reste pilote l'arbre et le filtre.
    fn explorer_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl {
            match key.code {
                KeyCode::Char('q') | KeyCode::Char('x') => self.request_quit(),
                KeyCode::Char('s') => self.save(),
                KeyCode::Char('b') => self.build(),
                KeyCode::Char('t') => self.toggle_explorer(),
                _ => {}
            }
            return;
        }
        let Some(ex) = &mut self.explorer else { return };
        match key.code {
            KeyCode::Up => ex.move_up(),
            KeyCode::Down => ex.move_down(),
            KeyCode::Home => ex.home(),
            KeyCode::End => ex.end(),
            KeyCode::Left => ex.collapse_or_parent(),
            KeyCode::Right => ex.expand(),
            KeyCode::Enter => {
                if let Some(path) = ex.enter() {
                    self.open_from_explorer(path);
                }
            }
            KeyCode::Backspace => ex.pop_filter(),
            KeyCode::Esc => {
                if !ex.clear_filter() {
                    self.focus = Focus::Editor;
                    self.status =
                        "Alt+Tab panneaux · ^O rechercher · ^B compiler · ^S sauver".into();
                }
            }
            // la frappe libre filtre le projet en flou (réflexe Telescope)
            KeyCode::Char(c) => ex.push_filter(c),
            _ => {}
        }
    }

    // -------------------------------------------------------------- boucle

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);

        if self.confirm_quit {
            match key.code {
                KeyCode::Char('o') | KeyCode::Char('y') => self.should_quit = true,
                KeyCode::Char('s') => {
                    self.save();
                    self.should_quit = true;
                }
                _ => self.confirm_quit = false,
            }
            return;
        }

        // saisie de recherche (^F) ou goto (^G) en cours
        if self.prompt.is_some() {
            match key.code {
                KeyCode::Esc => self.prompt = None,
                KeyCode::Enter => {
                    let (kind, text) = self.prompt.take().unwrap();
                    if kind == 'f' {
                        self.find(&text);
                    } else if kind == 'r' {
                        self.replace_all(&text);
                    } else if kind == 'g' {
                        if let Ok(n) = text.trim().parse::<usize>() {
                            if n >= 1 && n <= self.lines.len() {
                                self.cy = n - 1;
                                self.cx = 0;
                                self.status = format!("ligne {n}");
                            } else {
                                self.status = format!("ligne {n} hors limites");
                            }
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some((_, t)) = &mut self.prompt {
                        t.pop();
                    }
                }
                KeyCode::Char(c) => {
                    if let Some((_, t)) = &mut self.prompt {
                        t.push(c);
                    }
                }
                _ => {}
            }
            return;
        }

        // Alt-Tab / Ctrl-Tab / Shift-Tab (selon ce que livre le terminal) :
        // cycle du focus entre panneaux
        if key.code == KeyCode::BackTab || (key.code == KeyCode::Tab && (alt || ctrl)) {
            self.cycle_focus();
            return;
        }
        if ctrl && key.code == KeyCode::Char('o') {
            self.toggle_search();
            return;
        }
        // focus recherche / explorateur : touches dédiées (les globales passent)
        if self.focus == Focus::Search && self.search.is_some() {
            self.search_key(key);
            return;
        }
        if self.focus == Focus::Explorer && self.explorer.is_some() {
            self.explorer_key(key);
            return;
        }

        match (key.code, ctrl, alt) {
            // quitter : ^Q ou ^X (réflexe nano) — PAS ^C, trop de missclicks
            (KeyCode::Char('q'), true, _) | (KeyCode::Char('x'), true, _) => {
                self.request_quit()
            }
            (KeyCode::Char('t'), true, _) => self.toggle_explorer(),
            (KeyCode::Char('s'), true, _) => self.save(),
            (KeyCode::Char('b'), true, _) => self.build(),
            (KeyCode::Char('n'), true, _) => self.diag_jump(1),
            (KeyCode::Char('p'), true, _) => self.diag_jump(-1),
            (KeyCode::Char('z'), true, _) => self.undo(),
            (KeyCode::Char('f'), true, _) => {
                self.prompt = Some(('f', String::new()));
                self.status = "chercher :".into();
            }
            (KeyCode::Char('r'), true, _) => {
                // chercher-remplacer : « chercher → remplacer » en une saisie
                self.prompt = Some(('r', String::new()));
                self.status = "remplacer « motif » par « texte » (motif→texte) :".into();
            }
            (KeyCode::Char('g'), true, _) => {
                self.prompt = Some(('g', String::new()));
                self.status = "aller à la ligne :".into();
            }
            (KeyCode::Char('k'), true, _) => {
                self.snapshot();
                self.clipboard = self.lines.remove(self.cy);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.cy = self.cy.min(self.lines.len() - 1);
                self.cx = self.cx.min(self.line().len());
                self.modified = true;
                self.status = "ligne coupée".into();
            }
            (KeyCode::Char('u'), true, _) => {
                if !self.clipboard.is_empty() {
                    self.snapshot();
                    let clip = self.clipboard.clone();
                    self.lines.insert(self.cy, clip);
                    self.modified = true;
                    self.status = "ligne collée".into();
                }
            }
            (KeyCode::Right, _, true) | (KeyCode::Right, true, _) => self.move_right(),
            (KeyCode::Left, _, _) => self.move_left(),
            (KeyCode::Right, _, _) => self.move_right(),
            (KeyCode::Up, _, _) => self.move_up(),
            (KeyCode::Down, _, _) => self.move_down(),
            (KeyCode::Home, _, _) => self.cx = 0,
            (KeyCode::End, _, _) => self.cx = self.line().len(),
            (KeyCode::Backspace, _, _) => self.backspace(),
            (KeyCode::Delete, _, _) => {
                if self.cx < self.line().len() {
                    self.lines[self.cy].remove(self.cx);
                    self.modified = true;
                }
            }
            (KeyCode::Enter, _, _) => self.insert_newline(),
            // Tab : 4 espaces, toujours
            (KeyCode::Tab, _, _) | (KeyCode::Char('i'), true, _) => {
                for _ in 0..4 {
                    self.insert_char(' ');
                }
            }
            (KeyCode::Char(c), false, false) => self.type_char(c),
            _ => {}
        }
    }

    fn keep_cursor_visible(&mut self, height: usize, width: usize) {
        if self.cy < self.scroll_y {
            self.scroll_y = self.cy;
        } else if self.cy >= self.scroll_y + height {
            self.scroll_y = self.cy - height + 1;
        }
        let gutter = 6;
        if self.cx < self.scroll_x {
            self.scroll_x = self.cx;
        } else if self.cx >= self.scroll_x + width.saturating_sub(gutter) {
            self.scroll_x = self.cx - width.saturating_sub(gutter) + 1;
        }
    }

    /// Diagnostics de la ligne du curseur (gcc + norme, avec messages) —
    /// le survol façon LSP. `true` = erreur, `false` = avertissement.
    fn line_diagnostics(&self) -> Vec<(bool, String)> {
        let l = self.cy + 1;
        let mut v: Vec<(bool, String)> = self
            .diags
            .iter()
            .filter(|d| d.line == l)
            .map(|d| (d.is_error, d.msg.clone()))
            .collect();
        for (line, sev, msg) in &self.norme_details {
            if *line == l {
                v.push((matches!(sev, Severity::Major), msg.clone()));
            }
        }
        v
    }

    /// Marqueurs de gouttière par ligne (1-based) — diagnostics gcc et
    /// findings de norme fusionnés : le plus sévère gagne.
    fn gutter_marks(&self) -> HashMap<usize, Color> {
        let mut rank: HashMap<usize, (u8, Color)> = HashMap::new();
        let mut put = |line: usize, r: u8, c: Color| {
            rank.entry(line)
                .and_modify(|e| {
                    if r > e.0 {
                        *e = (r, c);
                    }
                })
                .or_insert((r, c));
        };
        for (line, sev) in &self.norme_marks {
            match sev {
                Severity::Major => put(*line, 2, Ed::red()),
                Severity::Minor => put(*line, 1, Ed::amber()),
                Severity::Info => {}
            }
        }
        for d in &self.diags {
            if d.is_error {
                put(d.line, 4, Ed::red());
            } else {
                put(d.line, 3, Ed::amber());
            }
        }
        rank.into_iter().map(|(l, (_, c))| (l, c)).collect()
    }
}

// ------------------------------------------------------------------ rendu

/// Barre basse, côté droit : la position, rien de plus.
fn pos_text(ed: &Editor) -> String {
    format!("Ln {}, Col {} ", ed.cy + 1, ed.cx + 1)
}

/// Remplit une zone d'une couleur unie (fond d'éditeur, barres).
fn fill(frame: &mut Frame, area: ratatui::layout::Rect, style: Style) {
    frame.render_widget(Paragraph::new("").style(style), area);
}

fn render_line(text: &str, palette_dim: Color, ext: &str) -> Vec<Span<'static>> {
    // coloration par langage (extension du fichier) via syntect — thème Minuit
    let hl = highlight::highlight_code(text, ext);
    if let Some(line) = hl.first() {
        line.iter()
            .map(|(style, t)| {
                Span::styled(
                    t.clone(),
                    Style::default().fg(Color::Rgb(
                        style.foreground.r,
                        style.foreground.g,
                        style.foreground.b,
                    )),
                )
            })
            .collect()
    } else {
        vec![Span::styled(text.to_string(), Style::default().fg(palette_dim))]
    }
}

/// Trace une boîte à bords arrondis (le langage visuel de lazy.nvim) :
/// ╭─ titre ──╮ … ╰──╯. Retourne le rectangle intérieur.
/// Bordure d'un panneau : cyan voilé s'il a le focus, gris sinon.
fn border_for(focused: bool) -> Color {
    if focused {
        Color::Rgb(0x3E, 0x6A, 0x7E) // cyan voilé — le focus se voit, sans crier
    } else {
        Ed::ruler()
    }
}

/// Fond des floats (modal, recherche, toasts, survol) — un demi-ton au-dessus.
fn float_bg() -> Color {
    Color::Rgb(0x1B, 0x1B, 0x1D)
}

/// Écrit un segment de barre (fond + texte) et retourne la colonne suivante.
fn put_seg(
    buf: &mut ratatui::buffer::Buffer,
    x: u16,
    y: u16,
    text: &str,
    fg: Color,
    bg: Color,
    bold: bool,
) -> u16 {
    let mut st = Style::default().fg(fg).bg(bg);
    if bold {
        st = st.add_modifier(Modifier::BOLD);
    }
    buf.set_string(x, y, text, st);
    x + UnicodeWidthStr::width(text) as u16
}

fn draw_box(
    frame: &mut Frame,
    area: ratatui::layout::Rect,
    title: &[(String, Color)],
    border: Color,
) -> ratatui::layout::Rect {
    if area.width < 4 || area.height < 3 {
        return area;
    }
    let bgc = Ed::bg();
    let right = area.right() - 1;
    let bottom = area.bottom() - 1;
    let buf = frame.buffer_mut();
    for y in area.y..=bottom {
        buf[(area.x, y)].set_symbol("│").set_fg(border).set_bg(bgc);
        buf[(right, y)].set_symbol("│").set_fg(border).set_bg(bgc);
    }
    for x in area.x..=right {
        buf[(x, bottom)].set_symbol("─").set_fg(border).set_bg(bgc);
    }
    buf[(area.x, area.y)].set_symbol("╭").set_fg(border).set_bg(bgc);
    buf[(right, area.y)].set_symbol("╮").set_fg(border).set_bg(bgc);
    buf[(area.x, bottom)].set_symbol("╰").set_fg(border).set_bg(bgc);
    buf[(right, bottom)].set_symbol("╯").set_fg(border).set_bg(bgc);
    // haut : ╭─ titre ────╮ (titre tronqué si la boîte est étroite)
    let mut x = area.x + 1;
    buf[(x, area.y)].set_symbol("─").set_fg(border).set_bg(bgc);
    x += 1;
    let budget = (right - 2).saturating_sub(area.x + 2);
    for (txt, color) in title {
        for ch in txt.chars() {
            if x >= area.x + 2 + budget {
                break;
            }
            buf[(x, area.y)].set_symbol(&ch.to_string()).set_fg(*color).set_bg(bgc);
            x += 1;
        }
    }
    while x < right {
        buf[(x, area.y)].set_symbol("─").set_fg(border).set_bg(bgc);
        x += 1;
    }
    ratatui::layout::Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width - 2,
        height: area.height - 2,
    }
}

/// Écran d'accueil : modal flottant centré, sections à puces — le float
/// d'accueil de lazy.nvim, transposé aux gestes de c-nano. Aucune tagline.
fn draw_welcome_float(frame: &mut Frame, area: ratatui::layout::Rect) {
    let w = area.width.saturating_sub(4).min(46);
    let h = area.height.saturating_sub(2).min(23);
    if w < 20 || h < 10 {
        return;
    }
    let modal = ratatui::layout::Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    // fond du modal : un demi-ton au-dessus de l'éditeur (le float flotte)
    fill(frame, modal, Style::default().bg(float_bg()));
    let inner = draw_box(
        frame,
        modal,
        &[(" r".into(), Ed::brand()), ("ust-nano ".into(), Ed::text())],
        border_for(true),
    );
    let bullet = || Span::styled("● ", Style::default().fg(Ed::cyan()));
    let section = |s: &'static str| {
        Line::from(Span::styled(
            format!(" {s}"),
            Style::default().fg(Ed::text()).add_modifier(Modifier::BOLD),
        ))
    };
    let entry = |k: &'static str, label: &'static str| {
        Line::from(vec![
            Span::raw("  "),
            bullet(),
            Span::styled(
                format!("{k:<5}"),
                Style::default().fg(Ed::cyan()),
            ),
            Span::styled(label, Style::default().fg(Ed::dim())),
        ])
    };
    let lines = vec![
        Line::from(""),
        section("Fichiers"),
        entry("^T", "explorateur"),
        entry("^O", "rechercher un fichier"),
        entry("⎇⇥", "changer de panneau"),
        entry("^S", "sauvegarder"),
        entry("^Q", "quitter"),
        Line::from(""),
        section("Code"),
        entry("^B", "compiler"),
        entry("^F", "chercher"),
        entry("^R", "remplacer"),
        entry("^G", "aller à la ligne"),
        Line::from(""),
        section("Buffer"),
        entry("^K", "couper la ligne"),
        entry("^U", "coller"),
        entry("^Z", "annuler"),
        Line::from(""),
        Line::from(Span::styled(
            "  rust-nano <fichier> pour ouvrir — ^T explore le projet",
            Style::default().fg(Ed::gutter()),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw(frame: &mut Frame, ed: &mut Editor) {
    let area = frame.area();
    // fond unifié sur toute la surface — la signature « Minuit »
    fill(frame, area, Style::default().bg(Ed::bg()));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // barre haute
            Constraint::Min(3),    // corps
            Constraint::Length(1), // barre basse
        ])
        .split(area);
    fill(frame, chunks[0], Style::default().bg(Ed::bar_bg()));
    fill(frame, chunks[2], Style::default().bg(Ed::bar_bg()));

    draw_topbar(frame, ed, chunks[0]);

    // ── corps : boîtes arrondies façon lazy.nvim — explorateur à gauche,
    // éditeur à droite, le focus teinte la bordure ──
    let edit_zone = if ed.explorer.is_some() {
        let sp = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(EXPL_W),
                Constraint::Length(1), // souffle entre les boîtes
                Constraint::Min(10),
            ])
            .split(chunks[1]);
        let root_name = ed
            .explorer
            .as_ref()
            .and_then(|ex| ex.root.file_name().and_then(|n| n.to_str()))
            .unwrap_or("projet")
            .to_string();
        let inner = draw_box(
            frame,
            sp[0],
            &[(" ".into(), Ed::ruler()), (root_name, Ed::text()), (" ".into(), Ed::ruler())],
            border_for(ed.focus == Focus::Explorer),
        );
        if let Some(ex) = &mut ed.explorer {
            draw_explorer(frame, ex, inner);
        }
        sp[2]
    } else {
        chunks[1]
    };
    // titre de la boîte éditeur : le fichier, ● corail si modifié
    let mut title: Vec<(String, Color)> = Vec::new();
    match &ed.file {
        Some(p) => {
            title.push((" ".into(), Ed::ruler()));
            title.push((p.display().to_string(), Ed::text()));
            if ed.modified {
                title.push((" ●".into(), Ed::accent()));
            }
            title.push((" ".into(), Ed::ruler()));
        }
        None => title.push((" rust-nano ".into(), Ed::dim())),
    }
    let inner = draw_box(frame, edit_zone, &title, border_for(ed.focus != Focus::Explorer));
    if ed.is_welcome() {
        draw_welcome_float(frame, inner);
    } else {
        draw_body(frame, ed, inner);
        draw_diagnostics(frame, ed, edit_zone);
    }
    // floats par-dessus tout : recherche, puis toasts
    if let Some(fs) = &mut ed.search {
        draw_search(frame, fs, chunks[1], ed.focus == Focus::Search);
    }
    draw_toasts(frame, ed, area);

    draw_statusbar(frame, ed, chunks[2]);
}

/// Barre haute : bloc brand corail + branche git + breadcrumb du fichier —
/// la tabline d'un IDE, version Minuit profond.
fn draw_topbar(frame: &mut Frame, ed: &Editor, area: ratatui::layout::Rect) {
    fill(frame, area, Style::default().bg(Ed::bar_bg()));
    let seg_bg = Color::Rgb(0x21, 0x21, 0x24);
    let buf = frame.buffer_mut();
    let mut x = area.x;
    // brand
    x = put_seg(buf, x, area.y, " rust-nano ", Ed::bg(), Ed::brand(), true) + 1;
    // branche git
    if let Some(br) = &ed.git_branch {
        put_seg(buf, x, area.y, &format!(" ⎇ {br} "), Ed::text(), seg_bg, false);
    }
    // norme à droite
    if let Some(note) = &ed.norme_note {
        let color = if note == "norme ✓" { Ed::green() } else { Ed::amber() };
        let seg = format!(" {note} ");
        let w = UnicodeWidthStr::width(seg.as_str()) as u16;
        put_seg(buf, area.right().saturating_sub(w), area.y, &seg, color, seg_bg, false);
    }
    // breadcrumb centré : dossier › fichier ●
    if let Some(p) = &ed.file {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        let dir = p.parent().and_then(|d| d.file_name()).and_then(|n| n.to_str());
        let crumb = match dir {
            Some(d) if !d.is_empty() => format!("{d} › {name}"),
            _ => name.to_string(),
        };
        let dirty_w = if ed.modified { 2 } else { 0 };
        let total = UnicodeWidthStr::width(crumb.as_str()) as u16 + dirty_w;
        let cx = area.x + area.width.saturating_sub(total) / 2;
        let nx = put_seg(buf, cx, area.y, &crumb, Ed::text(), Ed::bar_bg(), false);
        if ed.modified {
            put_seg(buf, nx, area.y, " ●", Ed::accent(), Ed::bar_bg(), false);
        }
    }
}

/// Statusline segmentée façon lualine : badge de focus, fichier, message,
/// diagnostics, norme, position, progression.
fn draw_statusbar(frame: &mut Frame, ed: &Editor, area: ratatui::layout::Rect) {
    fill(frame, area, Style::default().bg(Ed::bar_bg()));
    let seg_bg = Color::Rgb(0x21, 0x21, 0x24);
    let buf = frame.buffer_mut();
    let mut x = area.x;

    // badge de focus (le « mode » de lualine)
    let (label, color) = match ed.focus {
        Focus::Editor => (" ÉDITEUR ", Ed::cyan()),
        Focus::Explorer => (" EXPLORER ", Ed::green()),
        Focus::Search => (" RECHERCHE ", Color::Rgb(0xC6, 0xA0, 0xF6)),
    };
    x = put_seg(buf, x, area.y, label, Ed::bg(), color, true) + 1;
    // segment fichier
    if let Some(p) = &ed.file {
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("?");
        x = put_seg(buf, x, area.y, &format!(" {name} "), Ed::text(), seg_bg, false);
        if ed.modified {
            x = put_seg(buf, x, area.y, "● ", Ed::accent(), seg_bg, false);
        }
        x += 1;
    }

    // segments de droite : diagnostics, norme, position, progression
    let mut right: Vec<(String, Color)> = Vec::new();
    let errs = ed.diags.iter().filter(|d| d.is_error).count();
    let warns = ed.diags.len() - errs;
    if errs > 0 || warns > 0 {
        right.push((format!("✗{errs}"), Ed::red()));
        right.push((format!("⚠{warns}"), Ed::amber()));
    }
    if let Some(note) = &ed.norme_note {
        let color = if note == "norme ✓" { Ed::green() } else { Ed::amber() };
        right.push((note.clone(), color));
    }
    right.push((pos_text(ed).trim().to_string(), Ed::dim()));
    let pct = ((ed.cy + 1) * 100) / ed.lines.len().max(1);
    right.push((format!("{pct}%"), Ed::dim()));
    let total: u16 = right
        .iter()
        .map(|(s, _)| UnicodeWidthStr::width(s.as_str()) as u16 + 2)
        .sum();
    let mut rx = area.right().saturating_sub(total);
    for (text, color) in right {
        rx = put_seg(buf, rx, area.y, &format!(" {text} "), color, seg_bg, false);
    }

    // centre : prompt en cours ou dernier statut
    let mid = if let Some((k, t)) = &ed.prompt {
        let label = match k {
            'f' => "chercher : ",
            'r' => "remplacer : ",
            _ => "ligne : ",
        };
        format!("{label}{t}▌")
    } else {
        ed.status.clone()
    };
    let avail = (rx.saturating_sub(x + 1)) as usize;
    let mid: String = mid.chars().take(avail.saturating_sub(1)).collect();
    put_seg(buf, x, area.y, &format!(" {mid}"), Ed::dim(), Ed::bar_bg(), false);
}

/// Recherche de fichiers flottante — le float Telescope : prompt en haut,
/// séparateur fin, résultats sous le curseur.
fn draw_search(frame: &mut Frame, fs: &mut FileSearch, zone: ratatui::layout::Rect, focused: bool) {
    let w = zone.width.saturating_sub(8).min(62);
    let rows_h = (fs.len().min(9) as u16).max(1);
    let h = (rows_h + 4).min(zone.height.saturating_sub(2)); // input + séparateur + bords
    if w < 24 || h < 6 {
        return;
    }
    let float = ratatui::layout::Rect {
        x: zone.x + (zone.width - w) / 2,
        y: zone.y + (zone.height / 8).min(3),
        width: w,
        height: h,
    };
    fill(frame, float, Style::default().bg(float_bg()));
    let title = format!(" Recherche · {} ", fs.len());
    let inner = draw_box(
        frame,
        float,
        &[(title, Ed::text())],
        border_for(focused),
    );
    // prompt
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Ed::cyan())),
            Span::styled(
                format!("{}▌", fs.filter()),
                Style::default().fg(if focused { Ed::text() } else { Ed::dim() }),
            ),
        ])),
        ratatui::layout::Rect { height: 1, ..inner },
    );
    // séparateur fin entre prompt et résultats
    let sep_y = inner.y + 1;
    for cx in inner.x..inner.right() {
        let cell = &mut frame.buffer_mut()[(cx, sep_y)];
        cell.set_symbol("─").set_fg(Ed::ruler()).set_bg(float_bg());
    }
    // résultats
    let list_h = (inner.height as usize).saturating_sub(2);
    let (start, paths) = fs.window(list_h);
    let sel = fs.sel();
    let lines: Vec<Line> = if paths.is_empty() {
        vec![Line::from(Span::styled(
            "  aucun fichier",
            Style::default().fg(Ed::gutter()),
        ))]
    } else {
        paths
            .iter()
            .enumerate()
            .map(|(i, path)| {
                let rel = fs.rel(path);
                let selected = start + i == sel;
                let mut spans = vec![Span::raw(if selected { "▎" } else { " " })];
                match rel.rsplit_once('/') {
                    Some((dir, name)) => {
                        spans.push(Span::styled(format!("{dir}/"), Style::default().fg(Ed::gutter())));
                        let mut st = Style::default().fg(file_color(name, false));
                        if selected {
                            st = st.add_modifier(Modifier::BOLD);
                        }
                        spans.push(Span::styled(name.to_string(), st));
                    }
                    None => spans.push(Span::styled(rel, Style::default().fg(Ed::text()))),
                }
                let mut line = Line::from(spans);
                if selected {
                    line = line.style(Style::default().bg(Ed::sel_row()));
                }
                line
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines),
        ratatui::layout::Rect {
            y: inner.y + 2,
            height: inner.height.saturating_sub(2),
            ..inner
        },
    );
}

/// Notifications toast — cartes flottantes empilées en haut à droite,
/// colorées par niveau, évanouissement après 3,5 s.
fn draw_toasts(frame: &mut Frame, ed: &mut Editor, area: ratatui::layout::Rect) {
    ed.toasts
        .retain(|t| t.at.elapsed() < Duration::from_millis(3500));
    let shown: Vec<&Toast> = ed.toasts.iter().rev().take(3).collect();
    for (i, toast) in shown.into_iter().enumerate() {
        let text_w = UnicodeWidthStr::width(toast.text.as_str());
        let w = ((text_w + 6).clamp(14, 46)) as u16;
        let x = area.right().saturating_sub(w + 1);
        let y = area.y + 1 + i as u16 * 4;
        if y + 3 >= area.bottom() {
            break;
        }
        let rect = ratatui::layout::Rect { x, y, width: w, height: 3 };
        fill(frame, rect, Style::default().bg(float_bg()));
        let (icon, color) = match toast.level {
            Level::Ok => ("✓", Ed::green()),
            Level::Err => ("✗", Ed::red()),
            Level::Warn => ("⚠", Ed::amber()),
            Level::Info => ("ℹ", Ed::cyan()),
        };
        let inner = draw_box(frame, rect, &[], color);
        let text: String = toast.text.chars().take((w as usize).saturating_sub(5)).collect();
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(text, Style::default().fg(Ed::text())),
            ])),
            inner,
        );
    }
}

/// Survol des diagnostics de la ligne courante — le hover LSP : petit
/// flottant en bas à droite de l'éditeur, bordure rouge ou ambre.
fn draw_diagnostics(frame: &mut Frame, ed: &Editor, zone: ratatui::layout::Rect) {
    let items = ed.line_diagnostics();
    if items.is_empty() {
        return;
    }
    let shown: Vec<&(bool, String)> = items.iter().take(4).collect();
    let wmax = shown
        .iter()
        .map(|(_, m)| UnicodeWidthStr::width(m.as_str()))
        .max()
        .unwrap_or(8);
    let w = ((wmax + 6).clamp(18, 52)) as u16;
    let h = shown.len() as u16 + 2;
    let rect = ratatui::layout::Rect {
        x: zone.right().saturating_sub(w + 1),
        y: zone.bottom().saturating_sub(h + 1),
        width: w,
        height: h,
    };
    fill(frame, rect, Style::default().bg(float_bg()));
    let any_err = items.iter().any(|(e, _)| *e);
    let border = if any_err { Ed::red() } else { Ed::amber() };
    let title = format!(" ligne {} ", ed.cy + 1);
    let inner = draw_box(frame, rect, &[(title, Ed::dim())], border);
    let lines: Vec<Line> = shown
        .iter()
        .map(|(is_err, msg)| {
            let (icon, color) = if *is_err { ("✗", Ed::red()) } else { ("⚠", Ed::amber()) };
            let text: String = msg.chars().take((w as usize).saturating_sub(5)).collect();
            Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(text, Style::default().fg(Ed::text())),
            ])
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), inner);
}

/// Couleur d'un fichier selon son type — chaque famille a sa teinte.
fn file_color(name: &str, is_dir: bool) -> Color {
    if is_dir {
        return Ed::cyan();
    }
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Color::Rgb(0x82, 0xA8, 0xF2),         // pervenche — le Rust d'abord
        "toml" | "json" | "yaml" | "yml" | "lock" | "cfg" => Color::Rgb(0xC6, 0xA0, 0xF6),
        "md" | "txt" => Color::Rgb(0xA6, 0xDA, 0x95), // vert tendre
        "c" | "h" | "py" | "js" | "ts" | "sh" | "css" | "html" => Ed::text(),
        "o" | "a" | "so" | "bin" | "rlib" => Ed::gutter(),
        _ if lower == "makefile" || ext == "mk" => Color::Rgb(0xFF, 0xB8, 0x6C),
        _ => Ed::dim(),
    }
}

/// Panneau explorateur : arbre + filtre, séparateur fin, sélection teintée.
fn draw_explorer(frame: &mut Frame, ex: &mut Explorer, area: ratatui::layout::Rect) {
    // `area` est l'intérieur de la boîte (le cadre est déjà tracé)
    let content = area;

    // en-tête : la racine du panneau
    let root_name = ex
        .root
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("projet");
    let header = Line::from(vec![
        Span::styled("▾ ", Style::default().fg(Ed::cyan())),
        Span::styled(
            root_name.to_string(),
            Style::default().fg(Ed::text()).add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(header),
        ratatui::layout::Rect { height: 1, ..content },
    );

    // ligne du bas : le filtre en cours, ou l'invitation
    let filter_line = if ex.filter().is_empty() {
        Line::from(Span::styled(
            " tapez pour filtrer",
            Style::default().fg(Ed::dim()),
        ))
    } else {
        Line::from(vec![
            Span::styled("❯ ", Style::default().fg(Ed::cyan())),
            Span::styled(
                format!("{}▌", ex.filter()),
                Style::default().fg(Ed::text()),
            ),
        ])
    };
    frame.render_widget(
        Paragraph::new(filter_line),
        ratatui::layout::Rect {
            y: content.bottom().saturating_sub(1),
            height: 1,
            ..content
        },
    );

    // lignes de l'arbre (entre en-tête et ligne de filtre)
    let tree_h = (content.height as usize).saturating_sub(2);
    let rows_area = ratatui::layout::Rect {
        y: content.y + 1,
        height: tree_h as u16,
        ..content
    };
    // on clone la fenêtre (quelques dizaines de lignes) pour libérer
    // l'emprunt : git_mark/sel restent accessibles pendant le rendu
    let (start, visible) = {
        let (s, rows) = ex.window(tree_h);
        (s, rows.to_vec())
    };
    let sel = ex.sel();
    let filtering = !ex.filter().is_empty();
    let lines: Vec<Line> = visible
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let selected = start + i == sel;
            let mut spans = Vec::new();
            // barre d'accent corail sur le bord — la sélection se lit d'un coup d'œil
            if selected {
                spans.push(Span::styled("▎", Style::default().fg(Ed::accent())));
            } else {
                spans.push(Span::raw(" "));
            }
            if filtering {
                // mode filtre : chemin relatif, dossier en sourdine
                match row.name.rsplit_once('/') {
                    Some((dir, name)) => {
                        spans.push(Span::styled(
                            format!(" {dir}/"),
                            Style::default().fg(Ed::gutter()),
                        ));
                        spans.push(Span::styled(
                            name.to_string(),
                            Style::default().fg(file_color(name, false)),
                        ));
                    }
                    None => spans.push(Span::styled(
                        format!(" {}", row.name),
                        Style::default().fg(file_color(&row.name, false)),
                    )),
                }
            } else {
                spans.push(Span::raw("  ".repeat(row.depth)));
                if row.is_dir {
                    let arrow = if row.expanded { "▾ " } else { "▸ " };
                    spans.push(Span::styled(arrow, Style::default().fg(Ed::cyan())));
                } else {
                    spans.push(Span::raw("  "));
                }
                let mut name_style = Style::default().fg(file_color(&row.name, row.is_dir));
                if selected {
                    name_style = name_style.add_modifier(Modifier::BOLD);
                }
                spans.push(Span::styled(row.name.clone(), name_style));
            }
            // marqueur git : ● ambre modifié, ● vert nouveau
            if let Some(mark) = ex.git_mark(&row.path) {
                let color = if mark == 'M' { Ed::amber() } else { Ed::green() };
                spans.push(Span::styled(" ●", Style::default().fg(color)));
            }
            let mut line = Line::from(spans);
            if selected {
                line = line.style(Style::default().bg(Ed::sel_row()));
            }
            line
        })
        .collect();
    frame.render_widget(Paragraph::new(lines), rows_area);
}

/// Corps de l'éditeur : gouttière à marqueurs + texte coloré.
fn draw_body(frame: &mut Frame, ed: &Editor, zone: ratatui::layout::Rect) {
    let digits = ed.lines.len().to_string().len().max(2);
    let gutter_w = digits + 2; // marqueur + numéro + espace
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(gutter_w as u16), Constraint::Min(10)])
        .split(zone);

    let marks = ed.gutter_marks();
    let inner_h = zone.height as usize;
    let gutter_lines: Vec<Line> = (0..inner_h)
        .map(|i| {
            let n = ed.scroll_y + i + 1; // 1-based
            if n <= ed.lines.len() {
                let is_cur = n == ed.cy + 1;
                let mut spans = Vec::new();
                // marqueur : diagnostic gcc / norme, le plus sévère
                match marks.get(&n) {
                    Some(color) => spans.push(Span::styled("●", Style::default().fg(*color))),
                    None => spans.push(Span::raw(" ")),
                }
                spans.push(Span::styled(
                    format!("{:>w$} ", n, w = digits),
                    Style::default().fg(if is_cur { Ed::text() } else { Ed::gutter() }),
                ));
                let mut line = Line::from(spans);
                if is_cur {
                    line = line.style(Style::default().bg(Ed::cur_line()));
                }
                line
            } else {
                Line::from(" ".repeat(gutter_w))
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(gutter_lines), body[0]);

    let text_lines: Vec<Line> = (0..inner_h)
        .map(|i| {
            let n = ed.scroll_y + i;
            if n < ed.lines.len() {
                let raw = &ed.lines[n];
                let visible: String = raw.chars().skip(ed.scroll_x).collect();
                let spans = render_line(&visible, Ed::dim(), &ed.ext);
                let mut line = Line::from(spans);
                if n == ed.cy {
                    // ligne courante teintée — le regard se pose instantanément
                    line = line.style(Style::default().bg(Ed::cur_line()));
                }
                line
            } else {
                Line::from("")
            }
        })
        .collect();
    frame.render_widget(Paragraph::new(text_lines), body[1]);

    // repère subtil à la colonne 80 (la limite de la norme) — un filet discret
    let ruler_x = body[1].x + 80;
    if ruler_x < body[1].right() {
        for row in body[1].top()..body[1].bottom() {
            let cell = &mut frame.buffer_mut()[(ruler_x, row)];
            // n'écrase pas le code : uniquement sur une case vide,
            // et préserve le fond (ligne courante comprise)
            if cell.symbol().trim().is_empty() {
                cell.set_symbol("·").set_fg(Ed::ruler());
            }
        }
    }

    // curseur : seulement quand l'éditeur a le focus (dans l'explorateur,
    // c'est la ligne sélectionnée qui porte le regard)
    if ed.focus == Focus::Editor {
        let cur_x = body[1].x + (ed.cx - ed.scroll_x) as u16;
        let cur_y = body[1].y + (ed.cy - ed.scroll_y) as u16;
        frame.set_cursor_position((cur_x.min(body[1].width.saturating_sub(1) + body[1].x), cur_y));
    }
}

/// Lance l'éditeur. `path` : fichier à ouvrir/créer.
pub fn run(path: Option<PathBuf>) -> io::Result<()> {
    // NO_COLOR est une convention pensée pour les logs, pas pour un éditeur :
    // posée dans l'environnement, elle fait disparaître TOUTE la coloration
    // (crossterm émet alors des séquences vides). Un éditeur de code sans
    // couleurs est un bug, pas une préférence — c-nano la retire de SON
    // processus (l'environnement du shell reste intact).
    std::env::remove_var("NO_COLOR");
    // un dossier en argument (`c-nano .`) → l'explorateur s'ouvre dessus,
    // le buffer reste vierge jusqu'au premier Enter
    let dir = path.as_ref().filter(|p| p.is_dir()).cloned();
    let file = path.filter(|p| !p.is_dir());
    let mut ed = Editor::open(file.as_deref())?;
    if let Some(d) = dir {
        ed.explorer = Some(Explorer::new(d));
        ed.focus = Focus::Explorer;
        ed.status =
            "↑↓ naviguer · Enter ouvrir · →/← plier · tapez pour filtrer · ^T fermer".into();
    }
    crossterm::terminal::enable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::terminal::EnterAlternateScreen)?;
    let mut terminal = ratatui::DefaultTerminal::new(ratatui::backend::CrosstermBackend::new(
        io::stdout(),
    ))?;
    let result = loop_run(&mut terminal, &mut ed);
    crossterm::terminal::disable_raw_mode()?;
    crossterm::execute!(io::stdout(), crossterm::terminal::LeaveAlternateScreen)?;
    result
}

fn loop_run(
    terminal: &mut ratatui::DefaultTerminal,
    ed: &mut Editor,
) -> io::Result<()> {
    while !ed.should_quit {
        ed.poll_norme();
        ed.poll_diag();
        let size = terminal.size()?;
        let text_w = if ed.explorer.is_some() {
            size.width.saturating_sub(EXPL_W)
        } else {
            size.width
        };
        ed.keep_cursor_visible(size.height.saturating_sub(2) as usize, text_w as usize);
        terminal.draw(|frame| draw(frame, ed))?;
        // poll avec timeout : la boucle doit se réveiller pour lire les
        // réponses IA/norme/build qui arrivent en tâche de fond
        if event::poll(std::time::Duration::from_millis(120))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == crossterm::event::KeyEventKind::Press {
                    ed.on_key(key);
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod robustness_tests {
    use super::*;

    /// L'éditeur ne doit JAMAIS paniquer, quelles que soient les touches.
    #[test]
    fn aucune_panique_sur_touches_limites() {
        let mut ed = Editor::open(None).unwrap();
        // une rafale de touches bizarres
        let cles = [
            KeyCode::Up, KeyCode::Down, KeyCode::Left, KeyCode::Right,
            KeyCode::Home, KeyCode::End, KeyCode::Backspace, KeyCode::Delete,
            KeyCode::Enter, KeyCode::Tab, KeyCode::Char('a'), KeyCode::Char('x'),
        ];
        for _ in 0..3 {
            for c in &cles {
                let key = KeyEvent::new(*c, KeyModifiers::empty());
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ed.on_key(key)));
                assert!(r.is_ok(), "panique sur la touche {c:?}");
            }
        }
    }

    /// Le undo sur un buffer vide / initial ne panique pas.
    #[test]
    fn undo_sur_vide_ne_panique_pas() {
        let mut ed = Editor::open(None).unwrap();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| ed.undo()));
        assert!(r.is_ok());
        // et le undo restaure vraiment
        ed.on_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty()));
        ed.undo();
        assert_eq!(ed.lines[0], "");
    }

    /// goto/recherche sur des entrées bizarres ne paniquent pas.
    #[test]
    fn goto_et_recherche_limites() {
        let mut ed = Editor::open(None).unwrap();
        ed.lines = vec!["ligne un".into(), "ligne deux".into()];
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ed.find("");           // vide
            ed.find("inexistant"); // absent
            ed.find("ligne");      // trouvé
        }));
        assert!(r.is_ok());
    }
}

#[cfg(test)]
mod pair_tests {
    use super::*;

    fn type_keys(ed: &mut Editor, s: &str) {
        for c in s.chars() {
            ed.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
    }

    /// Une ouvrante insère la paire, curseur au milieu.
    #[test]
    fn ouvrante_insere_la_paire() {
        let mut ed = Editor::open(None).unwrap();
        type_keys(&mut ed, "(");
        assert_eq!(ed.lines[0], "()");
        assert_eq!(ed.cx, 1);
        type_keys(&mut ed, "{");
        assert_eq!(ed.lines[0], "({})");
        assert_eq!(ed.cx, 2);
    }

    /// Les quotes se ferment aussi (char et string du C).
    #[test]
    fn quotes_se_ferment_seules() {
        let mut ed = Editor::open(None).unwrap();
        type_keys(&mut ed, "\"");
        assert_eq!(ed.lines[0], "\"\"");
        assert_eq!(ed.cx, 1);
    }

    /// Taper la fermante déjà présente la survole, sans doubler.
    #[test]
    fn fermante_presente_est_survolee() {
        let mut ed = Editor::open(None).unwrap();
        type_keys(&mut ed, "(x)");
        assert_eq!(ed.lines[0], "(x)");
        assert_eq!(ed.cx, 3, "le ) final survole au lieu de doubler");
    }

    /// Backspace entre une paire vide supprime les deux moitiés.
    #[test]
    fn backspace_sur_paire_vide_efface_les_deux() {
        let mut ed = Editor::open(None).unwrap();
        type_keys(&mut ed, "(");
        ed.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
        assert_eq!(ed.lines[0], "");
        assert_eq!(ed.cx, 0);
    }

    /// Backspace sur du texte normal reste intact.
    #[test]
    fn backspace_classique_inchange() {
        let mut ed = Editor::open(None).unwrap();
        type_keys(&mut ed, "ab");
        ed.on_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::empty()));
        assert_eq!(ed.lines[0], "a");
    }
}

#[cfg(test)]
mod diag_tests {
    use super::*;

    /// La sortie courte de rustc est parsée : error[E…], warning ; le reste ignoré.
    #[test]
    fn parse_rustc_basique() {
        let out = "\
src/lib.rs:12:5: error[E0308]: mismatched types
src/lib.rs:20:1: warning: unused variable: `i`
src/lib.rs:7:2: note: previous definition is here
other.rs:1:1: error: pas notre fichier
";
        let d = parse_diagnostics(out, "src/lib.rs");
        assert_eq!(d.len(), 2, "notes et autres fichiers ignorés");
        assert_eq!(d[0], Diag { line: 12, col: 5, is_error: true, msg: "mismatched types".into() });
        assert!(!d[1].is_error);
        assert_eq!(d[1].line, 20);
        assert_eq!(d[1].msg, "unused variable: `i`");
    }

    /// Erreur sans code [E…] : le message est quand même lu.
    #[test]
    fn parse_rustc_sans_code() {
        let d = parse_diagnostics("a.rs:7:3: error: boom\n", "a.rs");
        assert_eq!(d.len(), 1);
        assert_eq!((d[0].line, d[0].col), (7, 3));
        assert_eq!(d[0].msg, "boom");
    }

    /// La marge fusionne norme + gcc : le plus sévère gagne.
    #[test]
    fn marqueurs_le_plus_severe_gagne() {
        let mut ed = Editor::open(None).unwrap();
        ed.norme_marks = vec![(3, Severity::Minor), (4, Severity::Major)];
        ed.diags = vec![
            Diag { line: 3, col: 1, is_error: true, msg: "boom".into() },
            Diag { line: 5, col: 1, is_error: false, msg: "warn".into() },
        ];
        let m = ed.gutter_marks();
        assert_eq!(m[&3], Ed::red(), "l'erreur gcc écrase la norme mineure");
        assert_eq!(m[&4], Ed::red(), "norme majeure = rouge");
        assert_eq!(m[&5], Ed::amber(), "warning gcc = ambre");
        assert!(!m.contains_key(&1));
    }

    /// ^N/^P naviguent en circulaire et placent le curseur sur la faute.
    #[test]
    fn navigation_circulaire() {
        let mut ed = Editor::open(None).unwrap();
        ed.lines = (1..=10).map(|i| format!("ligne {i}")).collect();
        ed.diags = vec![
            Diag { line: 2, col: 1, is_error: true, msg: "a".into() },
            Diag { line: 9, col: 3, is_error: false, msg: "b".into() },
        ];
        ed.diag_jump(1);
        assert_eq!(ed.cy, 1, "premier diagnostic");
        ed.diag_jump(1);
        assert_eq!(ed.cy, 8);
        assert_eq!(ed.cx, 2);
        ed.diag_jump(1);
        assert_eq!(ed.cy, 1, "ça boucle");
        ed.diag_jump(-1);
        assert_eq!(ed.cy, 8, "en arrière aussi");
    }

    /// La barre basse n'affiche que la position — aucun badge, jamais.
    #[test]
    fn pied_de_page_position_seule() {
        let ed = Editor::open(None).unwrap();
        assert_eq!(pos_text(&ed).trim(), "Ln 1, Col 1");
        assert!(!pos_text(&ed).contains('·'));
    }

    /// ^T ouvre et ferme l'explorateur (racine = répertoire courant).
    #[test]
    fn toggle_explorateur() {
        let mut ed = Editor::open(None).unwrap();
        assert!(ed.explorer.is_none());
        ed.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(ed.explorer.is_some());
        assert_eq!(ed.focus, Focus::Explorer);
        ed.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert!(ed.explorer.is_none());
        assert_eq!(ed.focus, Focus::Editor);
    }

    /// Enter sur un fichier de l'explorateur l'ouvre dans l'éditeur.
    #[test]
    fn ouvrir_depuis_explorateur() {
        let dir = std::env::temp_dir().join(format!("cnano-ed-{}-a", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("solo.c");
        std::fs::write(&f, "int solo;
").unwrap();

        let mut ed = Editor::open(None).unwrap();
        ed.explorer = Some(Explorer::new(dir.clone()));
        ed.focus = Focus::Explorer;
        ed.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(ed.file.as_deref(), Some(f.as_path()));
        assert_eq!(ed.lines, vec!["int solo;"]);
        assert_eq!(ed.focus, Focus::Editor, "le focus revient à l'éditeur");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Un buffer modifié ne se fait JAMAIS écraser par l'explorateur.
    #[test]
    fn buffer_modifie_bloque_le_changement() {
        let dir = std::env::temp_dir().join(format!("cnano-ed-{}-b", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.c"), "int a;
").unwrap();

        let mut ed = Editor::open(None).unwrap();
        ed.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty())); // modifié
        ed.explorer = Some(Explorer::new(dir.clone()));
        ed.focus = Focus::Explorer;
        ed.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert!(ed.file.is_none(), "pas de changement de fichier");
        assert_eq!(ed.lines[0], "x", "le buffer est intact");
        assert!(ed.status.contains("^S"));
        let _ = std::fs::remove_dir_all(dir);
    }

    /// La frappe libre dans l'explorateur filtre (et ne tape pas dans le code).
    #[test]
    fn frappe_dans_explorateur_filtre_sans_touchr_au_buffer() {
        let mut ed = Editor::open(None).unwrap();
        ed.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        ed.on_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::empty()));
        assert_eq!(ed.lines[0], "", "le buffer n'a pas bougé");
        assert_eq!(ed.explorer.as_ref().unwrap().filter(), "z");
    }

    /// Cadre de test : dessine l'UI sur un backend virtuel, retourne le texte.
    fn render_text(ed: &mut Editor, w: u16, h: u16) -> String {
        let backend = ratatui::backend::TestBackend::new(w, h);
        let mut term = ratatui::Terminal::new(backend).unwrap();
        term.draw(|f| draw(f, ed)).unwrap();
        term.backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// L'accueil est une boîte arrondie façon lazy.nvim — et ne mentionne
    /// plus jamais rien d'autre que les gestes.
    #[test]
    fn accueil_boite_arrondie_pure() {
        let mut ed = Editor::open(None).unwrap();
        let text = render_text(&mut ed, 90, 28);
        for corner in ["╭", "╮", "╰", "╯"] {
            assert!(text.contains(corner), "coin {corner} présent");
        }
        assert!(text.contains("rust-nano"), "le titre vit dans la bordure");
        assert!(text.contains("Fichiers") && text.contains("Code") && text.contains("Buffer"));
        assert!(!text.contains("IA"), "aucune mention de l'IA");
        assert!(!text.contains("piscine — sobre"), "plus de tagline");
    }

    /// Un fichier ouvert : son nom trône dans la bordure de la boîte éditeur.
    #[test]
    fn nom_de_fichier_dans_la_bordure() {
        let mut ed = Editor::open(None).unwrap();
        ed.file = Some(std::path::PathBuf::from("/tmp/demo.c"));
        ed.modified = true;
        ed.lines = vec!["int x;".into()];
        let text = render_text(&mut ed, 90, 28);
        assert!(text.contains("/tmp/demo.c"), "le nom est dans la bordure");
        assert!(text.contains("●"), "l'indicateur modifié suit");
    }

    /// Explorateur ouvert : deux boîtes arrondies côte à côte.
    #[test]
    fn deux_boites_quand_explorateur() {
        let mut ed = Editor::open(None).unwrap();
        ed.on_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL));
        let text = render_text(&mut ed, 110, 30);
        assert!(text.matches('╭').count() >= 2, "une boîte par panneau");
        assert!(text.contains("tapez pour filtrer"));
    }

    /// Alt-Tab : éditeur → explorateur → éditeur ; ouvre l'explorateur
    /// si rien n'est ouvert (le geste sert toujours).
    #[test]
    fn alt_tab_cycle_les_panneaux() {
        let mut ed = Editor::open(None).unwrap();
        assert_eq!(ed.focus, Focus::Editor);
        // sans explorateur : Alt-Tab l'ouvre et le focusse
        ed.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::ALT));
        assert_eq!(ed.focus, Focus::Explorer);
        assert!(ed.explorer.is_some());
        // encore : retour éditeur — Ctrl+Tab marche pareil
        ed.on_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::CONTROL));
        assert_eq!(ed.focus, Focus::Editor);
        // avec la recherche ouverte, elle est dans le cycle
        ed.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(ed.focus, Focus::Search);
        ed.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty())); // Shift+Tab = repli
        assert_eq!(ed.focus, Focus::Editor);
        ed.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()));
        assert_eq!(ed.focus, Focus::Explorer);
        ed.on_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty()));
        assert_eq!(ed.focus, Focus::Search);
    }

    /// ^O : la frappe filtre, Enter ouvre le fichier, Échap referme.
    #[test]
    fn recherche_flottante_ouvre_un_fichier() {
        let dir = std::env::temp_dir().join(format!("cnano-srch-{}-a", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("cible.c");
        std::fs::write(&f, "int cible;
").unwrap();

        let mut ed = Editor::open(None).unwrap();
        ed.explorer = Some(Explorer::new(dir.clone())); // racine connue
        ed.focus = Focus::Explorer;
        ed.on_key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
        assert_eq!(ed.focus, Focus::Search);
        for c in "cible".chars() {
            ed.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::empty()));
        }
        assert_eq!(ed.search.as_ref().unwrap().len(), 1);
        ed.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::empty()));
        assert_eq!(ed.file.as_deref(), Some(f.as_path()));
        assert_eq!(ed.lines, vec!["int cible;"]);
        assert!(ed.search.is_none(), "la recherche se referme");
        assert_eq!(ed.focus, Focus::Editor);
        // la frappe n'a jamais touché le buffer
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Les toasts s'affichent puis s'évanouissent (3,5 s).
    #[test]
    fn toasts_pousses_puis_expires() {
        let mut ed = Editor::open(None).unwrap();
        ed.notify(Level::Ok, "sauvegardé ✓");
        assert_eq!(ed.toasts.len(), 1);
        assert_eq!(ed.status, "sauvegardé ✓", "le statut reflète le toast");
        let text = render_text(&mut ed, 100, 30);
        assert!(text.contains("sauvegardé ✓"), "la carte est dessinée");
        assert!(text.contains("✓"), "icône du niveau");
        // un toast vieux de 10 s est purgé au prochain rendu
        ed.toasts.push(Toast {
            level: Level::Err,
            text: "périmé".into(),
            at: std::time::Instant::now() - std::time::Duration::from_secs(10),
        });
        let _ = render_text(&mut ed, 100, 30);
        assert_eq!(ed.toasts.len(), 1, "le toast expiré a été purgé");
    }

    /// Le survol affiche gcc + norme de la ligne courante, avec messages.
    #[test]
    fn survol_diagnostics_de_la_ligne() {
        let mut ed = Editor::open(None).unwrap();
        ed.lines = vec!["int x = ;".into()];
        ed.modified = true; // pas d'écran d'accueil
        ed.diags = vec![Diag { line: 1, col: 9, is_error: true, msg: "expected expression".into() }];
        ed.norme_details = vec![(1, Severity::Minor, "espace en fin de ligne (N-TRAILSPACE)".into())];
        let both = ed.line_diagnostics();
        assert_eq!(both.len(), 2, "gcc et norme fusionnés");
        let text = render_text(&mut ed, 100, 30);
        assert!(text.contains("expected expression"), "le message gcc flotte");
        assert!(text.contains("N-TRAILSPACE"), "le finding norme aussi");
        assert!(text.contains("ligne 1"), "le titre porte la ligne");
    }

    /// La statusline segmentée : badge de focus, position, progression.
    #[test]
    fn statusline_a_ses_segments() {
        let mut ed = Editor::open(None).unwrap();
        let text = render_text(&mut ed, 100, 30);
        assert!(text.contains("ÉDITEUR"), "badge de focus");
        assert!(text.contains("Ln 1, Col 1"), "position");
        assert!(text.contains("100%"), "progression");
    }

    /// L'écran d'accueil n'apparaît que sur un buffer vierge sans fichier.
    #[test]
    fn accueil_uniquement_sur_vierge() {
        let mut ed = Editor::open(None).unwrap();
        assert!(ed.is_welcome());
        ed.on_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::empty()));
        assert!(!ed.is_welcome(), "dès la première frappe, l'accueil s'efface");
    }
}
