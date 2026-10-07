//! Configuration utilisateur : ~/.config/c-man/config.toml
//!
//! Toutes les clés sont optionnelles ; les valeurs par défaut suivent la
//! piscine Epitech classique.

use crate::norme::NormeConfig;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Prénom Nom pour l'en-tête Epitech généré par `c-man fix`.
    pub name: Option<String>,
    /// Login Epitech (pour l'en-tête).
    pub login: Option<String>,
    #[serde(flatten)]
    pub norme: NormeSection,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NormeSection {
    pub max_columns: usize,
    pub max_function_lines: usize,
    pub max_functions_per_file: usize,
    pub forbid_for: bool,
    pub forbid_ternary: bool,
    pub forbid_switch: bool,
    pub forbid_goto: bool,
    pub return_parens: bool,
    pub comments_in_function: bool,
}

impl Default for NormeSection {
    fn default() -> Self {
        let c = NormeConfig::default();
        Self {
            max_columns: c.max_columns,
            max_function_lines: c.max_function_lines,
            max_functions_per_file: c.max_functions_per_file,
            forbid_for: c.forbid_for,
            forbid_ternary: c.forbid_ternary,
            forbid_switch: c.forbid_switch,
            forbid_goto: c.forbid_goto,
            return_parens: c.return_parens,
            comments_in_function: c.comments_in_function,
        }
    }
}

impl NormeSection {
    pub fn to_checker(&self) -> NormeConfig {
        NormeConfig {
            max_columns: self.max_columns,
            max_function_lines: self.max_function_lines,
            max_functions_per_file: self.max_functions_per_file,
            forbid_for: self.forbid_for,
            forbid_ternary: self.forbid_ternary,
            forbid_switch: self.forbid_switch,
            forbid_goto: self.forbid_goto,
            return_parens: self.return_parens,
            comments_in_function: self.comments_in_function,
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs_config().join("c-man").join("config.toml")
}

pub fn data_dir() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into()))
                .join(".local")
                .join("share")
        });
    base.join("c-man")
}

fn dirs_config() -> PathBuf {
    if cfg!(windows) {
        // Windows : %APPDATA% (Roaming) — XDG n'existe pas là-bas
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata);
        }
    }
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".into());
            PathBuf::from(home).join(".config")
        })
}

pub fn load() -> Config {
    let path = config_path();
    match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).unwrap_or_else(|e| {
            eprintln!("c-man: config {} invalide: {e}", path.display());
            Config::default()
        }),
        Err(_) => Config::default(),
    }
}


/// Crée le fichier de config avec les valeurs commentées si absent.
pub fn ensure_exists() -> std::io::Result<PathBuf> {
    let path = config_path();
    if path.exists() {
        return Ok(path);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, DEFAULT_CONFIG)?;
    Ok(path)
}

pub const DEFAULT_CONFIG: &str = r##"# Configuration c-man

# Identité pour l'en-tête Epitech généré par `c-man fix --header` :
# name = "Prénom Nom"
# login = "prenom.nom"          # login Epitech (pour <login@epitech.eu>)


# Règles de la Norme (valeurs = défauts piscine) :
# max_columns = 80
# max_function_lines = 25
# max_functions_per_file = 5
# forbid_for = true
# forbid_ternary = true
# forbid_switch = false
# forbid_goto = true
# return_parens = false
# comments_in_function = false
"##;
