//! Bibliothèque interne partagée par l'application et les harnais de fuzzing.
//!
//! Le binaire graphique conserve ses propres modules UI (GTK). Cette bibliothèque
//! n'expose que le backend et les messages, ce qui permet de compiler les parseurs
//! purs sans dépendre de GTK.

pub mod backend;
pub mod error;
pub mod ui {
    pub mod messages;
}
