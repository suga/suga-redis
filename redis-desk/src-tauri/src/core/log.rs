//! Anel de log em memória: as últimas conexões, falhas e operações lentas,
//! para a UI poder distinguir "o banco está vazio" de "a conexão falhou" —
//! hoje as duas telas são idênticas porque o app não registra nada disso em
//! lugar nenhum. Guarda só o suficiente para diagnosticar (host, operação,
//! duração, uma mensagem de erro já classificada): nunca o valor de uma
//! chave, nunca uma senha — a struct não tem campo para isso de propósito
//! (ver `an_entry_has_no_room_for_a_value_or_a_password`), então um chamador
//! não pode vazar esse dado aqui mesmo por engano.
//!
//! Sem relógio aqui dentro: `push` recebe `at_ms` já pronto (ver o campo
//! `LogEntry::at_ms`), para o anel em si não depender de `SystemTime` e os
//! testes poderem montar cenários determinísticos.

use std::collections::VecDeque;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

/// Quantas entradas o anel guarda. Além disso, a mais antiga é descartada a
/// cada `push` — ver `LogRing::push`.
pub const CAPACITY: usize = 1000;

/// A partir de quantos milissegundos uma operação é considerada lenta —
/// usado pelos chamadores (Task 3) para decidir quando registrar um
/// `LogKind::Slow`; o anel em si não aplica este limite.
pub const SLOW_MS: u64 = 1000;

/// O que gerou a entrada: uma conexão (aberta ou perdida), uma falha de
/// operação, ou uma operação que passou de `SLOW_MS`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogKind {
    Connection,
    Failure,
    Slow,
}

/// Uma entrada do anel. Deliberadamente sem campo para o valor de uma chave
/// ou para uma senha: `detail` é só uma mensagem de erro já classificada
/// (texto curto, nunca o dado bruto que estava sendo lido ou escrito).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Epoch em milissegundos, calculado por quem chama `push` — ver o
    /// comentário do módulo.
    pub at_ms: u64,
    pub kind: LogKind,
    /// Nome do host (ex.: "Redis de produção"), não o id interno — é o que
    /// a UI mostra, sem precisar resolver nada.
    pub host: String,
    pub db: Option<u8>,
    /// Ex.: "scan_tree", "list_dbs", "connect".
    pub operation: String,
    pub duration_ms: Option<u64>,
    pub detail: Option<String>,
}

/// Anel em memória: `push` empurra pela frente e corta a cauda em
/// `CAPACITY`, então `snapshot` já devolve mais recente primeiro sem
/// precisar inverter nada. `Mutex` (não `RwLock`): as duas operações são
/// baratas (um `push_front`/`truncate`, ou um clone do deque inteiro), não
/// compensa a complexidade de leitores concorrentes.
pub struct LogRing {
    entries: Mutex<VecDeque<LogEntry>>,
}

impl LogRing {
    pub fn new() -> LogRing {
        LogRing {
            entries: Mutex::new(VecDeque::with_capacity(CAPACITY)),
        }
    }

    /// Adiciona `entry` como a mais recente, descartando a mais antiga se o
    /// anel já estiver em `CAPACITY`.
    pub fn push(&self, entry: LogEntry) {
        let mut entries = self.entries.lock().unwrap();
        entries.push_front(entry);
        entries.truncate(CAPACITY);
    }

    /// Cópia de todas as entradas, mais recente primeiro.
    pub fn snapshot(&self) -> Vec<LogEntry> {
        let entries = self.entries.lock().unwrap();
        entries.iter().cloned().collect()
    }
}

impl Default for LogRing {
    fn default() -> LogRing {
        LogRing::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(operation: &str) -> LogEntry {
        LogEntry {
            at_ms: 0,
            kind: LogKind::Connection,
            host: "host-de-teste".to_string(),
            db: Some(0),
            operation: operation.to_string(),
            duration_ms: None,
            detail: None,
        }
    }

    #[test]
    fn snapshot_puts_the_newest_first() {
        let ring = LogRing::new();
        ring.push(entry("a"));
        ring.push(entry("b"));

        let snapshot = ring.snapshot();
        assert_eq!(snapshot[0].operation, "b");
        assert_eq!(snapshot[1].operation, "a");
    }

    #[test]
    fn the_ring_drops_the_oldest_past_capacity() {
        let ring = LogRing::new();
        for i in 0..=CAPACITY {
            ring.push(entry(&i.to_string()));
        }

        let snapshot = ring.snapshot();
        assert_eq!(snapshot.len(), CAPACITY);
        assert_eq!(
            snapshot[0].operation,
            CAPACITY.to_string(),
            "a entrada mais nova precisa estar na frente"
        );
        assert!(
            snapshot.iter().all(|e| e.operation != "0"),
            "a primeira entrada empurrada precisa ter sido descartada"
        );
    }

    #[test]
    fn an_entry_has_no_room_for_a_value_or_a_password() {
        let entry = LogEntry {
            at_ms: 1_700_000_000_000,
            kind: LogKind::Failure,
            host: "host-de-teste".to_string(),
            db: Some(3),
            operation: "connect".to_string(),
            duration_ms: Some(42),
            detail: Some("tempo de conexão esgotado".to_string()),
        };

        let json = serde_json::to_string(&entry).unwrap();

        assert!(
            !json.contains("password"),
            "não pode ter campo de senha: {json}"
        );
        assert!(
            !json.contains("senha"),
            "não pode ter campo de senha: {json}"
        );
        assert!(
            !json.contains("value"),
            "não pode ter campo de valor: {json}"
        );
    }
}
