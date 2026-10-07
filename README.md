# rust-nano

L'éditeur Rust du terminal — un IDE TUI sobre et rapide, thème « Minuit »
profond, fenêtres arrondies façon lazy.nvim. Jumeau de c-nano, calibré Rust :
`rustc` pour la vérification, `rustfmt` à la sauvegarde, `.rs` en pervenche
dans l'explorateur, brand orange rouille.

## Build (Windows / Linux / macOS)

```sh
cargo build --release
```

Le binaire est `target/release/rust-nano` (`rust-nano.exe` sous Windows).

Prérequis : Rust stable (`rustup`) — et comme tu édites du Rust, `rustc` et
`rustfmt` sont déjà là : `^B` vérifie avec rustc, chaque sauvegarde vérifie
le formatage. Optionnel : `git` (branche + marqueurs dans l'explorateur).

## Gestes

| Touche | Action |
|---|---|
| `rust-nano .` | ouvre l'explorateur sur le dossier |
| `Alt+Tab` / `Ctrl+Tab` / `Shift+Tab` | change de panneau |
| `^O` | recherche de fichiers flottante (filtre flou) |
| `^T` | explorateur |
| `^B` | vérifier (rustc), erreurs en marge, survol façon LSP |
| `^N` / `^P` | diagnostic suivant / précédent |
| `^S` / `^Q` | sauvegarder / quitter |
| `^F` `^R` `^G` | chercher · remplacer · aller à la ligne |
| `^K` `^U` `^Z` | couper · coller · annuler |
| `Tab` | 4 espaces |

Auto-paires, notifications toast, statusline segmentée, barre IDE avec
branche git et breadcrumb. Zéro configuration.

Les icônes de fichiers utilisent les devicons d'une Nerd Font (JetBrainsMono
Nerd Font recommandée) ; sans elle,  revient aux lettres cerclées.
