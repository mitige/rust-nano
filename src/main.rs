//! rust-nano — éditeur de code TUI, thème Minuit, fenêtres arrondies.

use std::path::PathBuf;

fn main() {
    let file = std::env::args().nth(1).map(PathBuf::from);
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("-h") | Some("--help") | Some("-V") | Some("--version")
    ) {
        println!(
            "rust-nano — l'éditeur Rust du terminal\n\n\
             Usage: rust-nano [fichier]\n\n\
             Tab        4 espaces, toujours\n\
             Ctrl+S     sauvegarder (rustfmt vérifié à chaque sauvegarde)\n\
             Ctrl+B     vérifier (rustc) — erreurs et warnings dans la marge\n\
             Ctrl+N/P   diagnostic suivant / précédent\n\
             Ctrl+Q     quitter\n\
             Ctrl+K/U   couper / coller une ligne\n\
             Ctrl+F/R/G chercher · remplacer · aller à la ligne\n\
             Ctrl+T     explorateur de fichiers (rust-nano . ouvre sur le dossier)\n\
             Ctrl+O     recherche de fichiers flottante (filtre flou)\n\
             F2         changer de panneau (éditeur → explorateur → recherche → terminal)\n\
             F3         terminal intégré · F4 en-tête de fichier auto\n\
             (dans l'explorateur : ^N nouveau · ^R renommer · ^D supprimer)\n\
             Échap      annuler / revenir"
        );
        return;
    }
    if let Err(e) = rust_nano::editor::run(file) {
        eprintln!("rust-nano: {e}");
        std::process::exit(1);
    }
}
