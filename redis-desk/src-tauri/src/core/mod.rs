// Módulos do núcleo da aplicação (independentes da camada Tauri).
pub mod config;
pub mod conn;
pub mod history;
pub mod index;
pub mod keys;
pub mod log;
pub mod search;
// Privado ao núcleo: só `history` e `index` precisam da abertura
// compartilhada do SQLite (ver o comentário em `sqlite::open_shared`).
mod sqlite;
pub mod value;
