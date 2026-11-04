//! Cópia de segurança do valor anterior de uma chave antes de toda escrita,
//! histórico dessas mudanças e o botão de "volta atrás": grava o valor
//! inteiro (sem truncar) antes de aplicar `WriteOp`, guarda a linha num
//! SQLite compartilhado com o índice de busca da Task 7, e sabe desfazer
//! exatamente o que foi feito — inclusive recriar uma chave apagada, com o
//! TTL que ela tinha.
//!
//! Banco em `<dir de dados do app>/redis-desk.sqlite`, aberto sempre com
//! `PRAGMA journal_mode = WAL` e `PRAGMA busy_timeout = 5000`: o mesmo
//! arquivo é usado pelo índice de busca (Task 7) e, a partir da Task 15, por
//! um segundo processo do SO (o servidor MCP) — sem WAL e um busy_timeout
//! generoso, os dois processos se atropelam com `SQLITE_BUSY` em uso normal.

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::config::{Env, Host};
use super::conn::{ConnError, Pool};
use super::value::{Bytes, Value, ValueLimit, ValueView, WriteOp, apply_write, op_summary, read_value};

/// Limite passado a `read_value` ao tirar a cópia de segurança: grande o
/// bastante para nunca truncar um valor de uso prático (1 GiB / 1 milhão de
/// elementos), ao contrário do `ValueLimit::default()` usado pela tela de
/// detalhes (256 KB / 500 elementos) — um backup que corta a cauda de um
/// valor grande só seria descoberto como incompleto no momento em que a
/// pessoa precisasse da volta atrás, e aí já seria tarde.
const FULL_READ_LIMIT: ValueLimit = ValueLimit {
    offset: 0,
    max_bytes: 1 << 30,
    max_elements: 1_000_000,
};

/// Quem fez a mudança: uma pessoa pela UI, ou a LLM (Task 16) através das
/// ferramentas MCP (Task 15). Guardado em toda entrada do histórico para que
/// a UI possa distinguir e, por exemplo, destacar mudanças feitas pela LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Actor {
    Human,
    Llm,
}

/// Uma entrada do histórico: a mudança em si (`summary`, `before_json`,
/// `after_json`), quem fez (`actor`) e quando (`at`, epoch em milissegundos),
/// além dos dados do host/banco para a UI listar sem precisar resolver o id.
/// `before_json` é `None` quando a chave não existia antes da escrita (ex.:
/// a própria criação da chave) — nesse caso, "desfazer" apaga a chave de
/// novo. `rolled_back` marca que esta entrada já foi revertida, para a UI
/// não oferecer desfazer de novo sobre algo que já voltou atrás.
#[derive(Debug, Clone, PartialEq)]
pub struct HistoryEntry {
    pub id: i64,
    pub host_id: String,
    pub host_name: String,
    pub env: Env,
    pub db: u8,
    pub key: String,
    pub summary: String,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub actor: Actor,
    pub at: i64,
    pub rolled_back: bool,
}

/// Erros deste módulo: falha do SQLite, falha ao (des)serializar um valor
/// para JSON, erro de conexão do Redis propagado de `read_value`/
/// `apply_write`, e os dois casos específicos deste módulo (entrada
/// inexistente, tipo de valor sem suporte a volta atrás).
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("falha de E/S ao abrir o histórico: {0}")]
    Io(#[from] std::io::Error),

    #[error("falha no banco de histórico: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("falha ao (des)serializar o valor da chave para o histórico: {0}")]
    Serde(#[from] serde_json::Error),

    #[error(transparent)]
    Conn(#[from] ConnError),

    #[error("registro de histórico {0} não encontrado")]
    NotFound(i64),

    #[error("dado de histórico corrompido: {0}")]
    Corrupt(String),

    #[error("tipo de valor \"{0}\" não tem suporte a volta atrás")]
    UnsupportedKind(String),
}

/// Resultado padrão das operações deste módulo.
pub type Result<T> = std::result::Result<T, HistoryError>;

/// Cria a tabela (e o índice usado por `list`/`prune`) se ainda não existir.
const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id TEXT NOT NULL,
    host_name TEXT NOT NULL,
    env TEXT NOT NULL,
    db INTEGER NOT NULL,
    key TEXT NOT NULL,
    summary TEXT NOT NULL,
    before_json TEXT,
    after_json TEXT,
    actor TEXT NOT NULL,
    at INTEGER NOT NULL,
    rolled_back INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_history_host_at ON history(host_id, at);
";

/// Um valor binário capturado para o histórico — mesma forma de `Bytes`
/// (texto quando UTF-8 válido, base64 quando não), mas serializável: `Bytes`
/// (em `core::value`) não deriva `Serialize`/`Deserialize` de propósito (não
/// é um tipo de transporte), então este módulo guarda sua própria cópia
/// espelhada só para ida e volta pelo JSON do histórico.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct BytesSnapshot {
    text: Option<String>,
    base64: Option<String>,
}

impl BytesSnapshot {
    fn from_bytes(bytes: &Bytes) -> BytesSnapshot {
        BytesSnapshot {
            text: bytes.text.clone(),
            base64: bytes.base64.clone(),
        }
    }

    /// Inverso de `from_bytes`: prefere `text` (o caminho comum) e só
    /// decodifica `base64` quando não há texto — mesma regra de
    /// `Bytes::to_raw` em `core::value`, reimplementada aqui porque aquele
    /// método é privado ao módulo `value`.
    fn to_raw(&self) -> Vec<u8> {
        if let Some(text) = &self.text {
            text.as_bytes().to_vec()
        } else if let Some(encoded) = &self.base64 {
            BASE64.decode(encoded).unwrap_or_default()
        } else {
            Vec::new()
        }
    }
}

/// Um elemento dentro do valor capturado: `field` só é usado por hash,
/// `score` só por sorted set — `None` nos dois para string/lista/set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ElementSnapshot {
    field: Option<String>,
    value: BytesSnapshot,
    score: Option<f64>,
}

/// O conteúdo completo de uma chave no momento da cópia de segurança,
/// serializado como JSON em `before_json`/`after_json`. `kind` decide como
/// `elements` é interpretado ao restaurar — ver `restore_value`. Chaves dos
/// tipos somente-leitura (`set`, `stream`, RedisJSON) também podem aparecer
/// aqui (uma escrita pode falhar contra uma chave desse tipo, e o valor já
/// foi lido antes de `apply_write` recusar — ver `write_with_backup`), mas
/// `restore_value` nunca precisa recriá-las: `apply_write` só tem sucesso
/// sobre tipos editáveis, então uma entrada bem-sucedida do histórico nunca
/// carrega um desses tipos em `before_json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct ValueSnapshot {
    kind: String,
    ttl_ms: Option<i64>,
    elements: Vec<ElementSnapshot>,
}

fn snapshot_from_view(view: &ValueView) -> ValueSnapshot {
    let ttl_ms = view.ttl_ms;
    let (kind, elements) = match &view.value {
        Value::Str(bytes) => (
            "string",
            vec![ElementSnapshot {
                field: None,
                value: BytesSnapshot::from_bytes(bytes),
                score: None,
            }],
        ),
        Value::Hash(pairs) => (
            "hash",
            pairs
                .iter()
                .map(|(field, bytes)| ElementSnapshot {
                    field: Some(field.clone()),
                    value: BytesSnapshot::from_bytes(bytes),
                    score: None,
                })
                .collect(),
        ),
        Value::List(items) => (
            "list",
            items
                .iter()
                .map(|bytes| ElementSnapshot {
                    field: None,
                    value: BytesSnapshot::from_bytes(bytes),
                    score: None,
                })
                .collect(),
        ),
        Value::ZSet(items) => (
            "zset",
            items
                .iter()
                .map(|(bytes, score)| ElementSnapshot {
                    field: None,
                    value: BytesSnapshot::from_bytes(bytes),
                    score: Some(*score),
                })
                .collect(),
        ),
        Value::Set(items) => (
            "set",
            items
                .iter()
                .map(|bytes| ElementSnapshot {
                    field: None,
                    value: BytesSnapshot::from_bytes(bytes),
                    score: None,
                })
                .collect(),
        ),
        Value::Stream(_) | Value::ReadOnlyText(_) => ("other", Vec::new()),
    };

    ValueSnapshot {
        kind: kind.to_string(),
        ttl_ms,
        elements,
    }
}

fn env_to_str(env: Env) -> &'static str {
    match env {
        Env::Prod => "prod",
        Env::Homolog => "homolog",
        Env::Dev => "dev",
    }
}

fn env_from_str(value: &str) -> Result<Env> {
    match value {
        "prod" => Ok(Env::Prod),
        "homolog" => Ok(Env::Homolog),
        "dev" => Ok(Env::Dev),
        other => Err(HistoryError::Corrupt(format!("env desconhecido: {other}"))),
    }
}

fn actor_to_str(actor: Actor) -> &'static str {
    match actor {
        Actor::Human => "human",
        Actor::Llm => "llm",
    }
}

fn actor_from_str(value: &str) -> Result<Actor> {
    match value {
        "human" => Ok(Actor::Human),
        "llm" => Ok(Actor::Llm),
        other => Err(HistoryError::Corrupt(format!("actor desconhecido: {other}"))),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("relógio do sistema antes de 1970")
        .as_millis() as i64
}

pub struct History {
    conn: Mutex<Connection>,
}

impl History {
    /// Abre (ou cria) o banco em `db_path`, sempre com `journal_mode = WAL`
    /// e `busy_timeout = 5000` — este arquivo é compartilhado com o índice
    /// de busca (Task 7) e, a partir da Task 15, com o processo do servidor
    /// MCP, então nunca pode ser aberto sem essas duas pragmas.
    pub fn open(db_path: &Path) -> Result<History> {
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;
        // `journal_mode` sempre devolve uma linha com o modo resultante —
        // ao contrário da maioria das PRAGMAs, não dá para gravar com
        // `pragma_update` puro.
        conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| {
            row.get::<_, String>(0)
        })?;
        conn.pragma_update(None, "busy_timeout", 5000i64)?;
        conn.execute_batch(SCHEMA)?;

        Ok(History {
            conn: Mutex::new(conn),
        })
    }

    /// Grava uma linha nova do histórico e devolve seu id. Privado: usado
    /// por `write_with_backup` e, nos testes deste módulo, para montar
    /// cenários de `prune` sem precisar de uma escrita real no Redis.
    fn insert_entry(
        &self,
        host: &Host,
        db: u8,
        key: &str,
        summary: &str,
        before_json: Option<&str>,
        actor: Actor,
        at: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO history
                (host_id, host_name, env, db, key, summary, before_json, after_json, actor, at, rolled_back)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, ?8, ?9, 0)",
            params![
                host.id,
                host.name,
                env_to_str(host.env),
                db as i64,
                key,
                summary,
                before_json,
                actor_to_str(actor),
                at,
            ],
        )?;
        Ok(conn.last_insert_rowid())
    }

    fn update_after_json(&self, id: i64, after_json: Option<&str>) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE history SET after_json = ?1 WHERE id = ?2",
            params![after_json, id],
        )?;
        Ok(())
    }

    fn delete_row(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM history WHERE id = ?1", params![id])?;
        Ok(())
    }

    fn mark_rolled_back(&self, id: i64) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE history SET rolled_back = 1 WHERE id = ?1",
            params![id],
        )?;
        Ok(())
    }

    /// Lê `key` por inteiro (sem truncar — ver `FULL_READ_LIMIT`), grava a
    /// linha do histórico com esse valor em `before_json` e só então aplica
    /// `op`. Se a escrita falhar, a linha recém-gravada é removida: uma
    /// entrada de histórico que registra uma mudança que nunca aconteceu de
    /// verdade é pior que não ter entrada nenhuma. Se a escrita tiver
    /// sucesso, o valor resultante é lido de novo e guardado em
    /// `after_json` (melhor esforço: uma falha nessa releitura não desfaz a
    /// escrita, que já aconteceu — só deixa `after_json` em `None`).
    pub async fn write_with_backup(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        db: u8,
        key: &str,
        op: &WriteOp,
        actor: Actor,
    ) -> Result<i64> {
        let before = match read_value(pool, host, pw, db, key, FULL_READ_LIMIT).await {
            Ok(view) => Some(snapshot_from_view(&view)),
            Err(ConnError::KeyGone(_)) => None,
            Err(err) => return Err(HistoryError::Conn(err)),
        };
        let before_json = match &before {
            Some(snapshot) => Some(serde_json::to_string(snapshot)?),
            None => None,
        };

        let summary = op_summary(key, op);
        let at = now_unix_ms();
        let id = self.insert_entry(host, db, key, &summary, before_json.as_deref(), actor, at)?;

        if let Err(err) = apply_write(pool, host, pw, db, key, op).await {
            self.delete_row(id)?;
            return Err(HistoryError::Conn(err));
        }

        let after_json = match read_value(pool, host, pw, db, key, FULL_READ_LIMIT).await {
            Ok(view) => serde_json::to_string(&snapshot_from_view(&view)).ok(),
            Err(_) => None,
        };
        self.update_after_json(id, after_json.as_deref())?;

        Ok(id)
    }

    /// As `limit` entradas mais recentes do host, mais novas primeiro.
    pub fn list(&self, host_id: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, host_id, host_name, env, db, key, summary, before_json, after_json, actor, at, rolled_back
             FROM history WHERE host_id = ?1 ORDER BY at DESC LIMIT ?2",
        )?;
        let rows = stmt
            .query_map(params![host_id, limit as i64], row_to_raw_entry)?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        rows.into_iter().map(RawEntry::into_entry).collect()
    }

    /// Uma entrada específica, por id.
    pub fn get(&self, id: i64) -> Result<HistoryEntry> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, host_id, host_name, env, db, key, summary, before_json, after_json, actor, at, rolled_back
             FROM history WHERE id = ?1",
        )?;
        let raw = stmt
            .query_row(params![id], row_to_raw_entry)
            .optional()?
            .ok_or(HistoryError::NotFound(id))?;

        raw.into_entry()
    }

    /// Desfaz a entrada `entry_id`: regrava `before_json` (ou apaga a
    /// chave, se ela não existia antes da escrita original) e marca a
    /// entrada como revertida. A entrada já guarda o `db` em que a escrita
    /// original aconteceu, então `rollback` não recebe `db` como parâmetro —
    /// ele vem do próprio registro, não do que estiver selecionado na UI no
    /// momento do clique em "desfazer".
    pub async fn rollback(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        entry_id: i64,
    ) -> Result<()> {
        let entry = self.get(entry_id)?;

        let snapshot = match &entry.before_json {
            Some(json) => Some(serde_json::from_str::<ValueSnapshot>(json)?),
            None => None,
        };

        restore_value(pool, host, pw, entry.db, &entry.key, &snapshot).await?;
        self.mark_rolled_back(entry_id)?;

        Ok(())
    }

    /// Remove entradas do host que tenham mais de `keep_days` dias OU que
    /// estejam além das `keep_entries` mais recentes — o que vier primeiro
    /// elimina a entrada. Devolve quantas linhas foram removidas.
    pub fn prune(&self, host_id: &str, keep_days: i64, keep_entries: usize) -> Result<usize> {
        let cutoff = now_unix_ms() - keep_days * 24 * 60 * 60 * 1000;
        let conn = self.conn.lock().unwrap();
        let removed = conn.execute(
            "DELETE FROM history
             WHERE host_id = ?1
               AND (
                   at < ?2
                   OR id NOT IN (
                       SELECT id FROM history WHERE host_id = ?1 ORDER BY at DESC LIMIT ?3
                   )
               )",
            params![host_id, cutoff, keep_entries as i64],
        )?;
        Ok(removed)
    }
}

/// Forma crua de uma linha, antes de `env`/`actor` virarem seus enums — a
/// conversão fica fora do closure de `query_map` porque ele só pode falhar
/// com `rusqlite::Error`, não com `HistoryError::Corrupt`.
struct RawEntry {
    id: i64,
    host_id: String,
    host_name: String,
    env: String,
    db: u8,
    key: String,
    summary: String,
    before_json: Option<String>,
    after_json: Option<String>,
    actor: String,
    at: i64,
    rolled_back: bool,
}

impl RawEntry {
    fn into_entry(self) -> Result<HistoryEntry> {
        Ok(HistoryEntry {
            id: self.id,
            host_id: self.host_id,
            host_name: self.host_name,
            env: env_from_str(&self.env)?,
            db: self.db,
            key: self.key,
            summary: self.summary,
            before_json: self.before_json,
            after_json: self.after_json,
            actor: actor_from_str(&self.actor)?,
            at: self.at,
            rolled_back: self.rolled_back,
        })
    }
}

fn row_to_raw_entry(row: &rusqlite::Row) -> rusqlite::Result<RawEntry> {
    Ok(RawEntry {
        id: row.get(0)?,
        host_id: row.get(1)?,
        host_name: row.get(2)?,
        env: row.get(3)?,
        db: row.get::<_, i64>(4)? as u8,
        key: row.get(5)?,
        summary: row.get(6)?,
        before_json: row.get(7)?,
        after_json: row.get(8)?,
        actor: row.get(9)?,
        at: row.get(10)?,
        rolled_back: row.get::<_, i64>(11)? != 0,
    })
}

/// Restaura `key` para o estado descrito por `snapshot` (ou apaga a chave,
/// se `snapshot` é `None` — ela não existia antes da escrita original):
/// `DEL` seguido da recriação do tipo certo, tudo num `MULTI`/`EXEC`
/// (`Pipeline::atomic`) para que nenhuma outra escrita concorrente veja a
/// chave num estado intermediário (apagada, ou só parcialmente recriada).
/// Passa por `Pool::with_retry`: a sequência inteira é idempotente (rodar
/// `DEL` + recriação de novo produz exatamente o mesmo estado final), ao
/// contrário das operações de lista de `apply_write` que usam `run_once`.
async fn restore_value(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    key: &str,
    snapshot: &Option<ValueSnapshot>,
) -> Result<()> {
    if let Some(snap) = snapshot
        && !matches!(snap.kind.as_str(), "string" | "hash" | "list" | "zset")
    {
        return Err(HistoryError::UnsupportedKind(snap.kind.clone()));
    }

    let key_owned = key.to_string();
    let snapshot_owned = snapshot.clone();

    pool.with_retry(host, db, pw, move |mut conn| {
        let key = key_owned.clone();
        let snapshot = snapshot_owned.clone();
        async move {
            let pipe = build_restore_pipeline(&key, &snapshot);
            pipe.query_async::<()>(&mut conn).await
        }
    })
    .await
    .map_err(HistoryError::Conn)
}

fn build_restore_pipeline(key: &str, snapshot: &Option<ValueSnapshot>) -> redis::Pipeline {
    let mut pipe = redis::pipe();
    pipe.atomic();
    pipe.cmd("DEL").arg(key);

    if let Some(snap) = snapshot {
        match snap.kind.as_str() {
            "string" => {
                if let Some(element) = snap.elements.first() {
                    pipe.cmd("SET").arg(key).arg(element.value.to_raw());
                }
            }
            "hash" => {
                if !snap.elements.is_empty() {
                    let cmd = pipe.cmd("HSET");
                    cmd.arg(key);
                    for element in &snap.elements {
                        cmd.arg(element.field.clone().unwrap_or_default());
                        cmd.arg(element.value.to_raw());
                    }
                }
            }
            "list" => {
                if !snap.elements.is_empty() {
                    let cmd = pipe.cmd("RPUSH");
                    cmd.arg(key);
                    for element in &snap.elements {
                        cmd.arg(element.value.to_raw());
                    }
                }
            }
            "zset" => {
                if !snap.elements.is_empty() {
                    let cmd = pipe.cmd("ZADD");
                    cmd.arg(key);
                    for element in &snap.elements {
                        cmd.arg(element.score.unwrap_or(0.0));
                        cmd.arg(element.value.to_raw());
                    }
                }
            }
            _ => {}
        }

        if let Some(ttl_ms) = snap.ttl_ms {
            pipe.cmd("PEXPIRE").arg(key).arg(ttl_ms);
        }
    }

    pipe
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::conn::Pool;
    use redis::aio::MultiplexedConnection;

    fn test_host() -> Host {
        Host {
            id: "history-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            // db4: db0-3 já são usados pelos testes de conn/keys/value; a
            // suíte roda em paralelo contra o mesmo container.
            db: 4,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    fn text(value: &str) -> super::super::value::Bytes {
        super::super::value::Bytes {
            text: Some(value.to_string()),
            base64: None,
            truncated: false,
        }
    }

    async fn del(conn: &mut MultiplexedConnection, keys: &[&str]) {
        for key in keys {
            let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
        }
    }

    fn open_history() -> (tempfile::TempDir, History) {
        let dir = tempfile::TempDir::new().unwrap();
        let history = History::open(&dir.path().join("redis-desk.sqlite")).unwrap();
        (dir, history)
    }

    #[tokio::test]
    async fn backup_is_taken_before_the_write() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:backup_before_write";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-antigo")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let op = WriteOp::SetString {
            value: text("valor-novo"),
        };

        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();

        let current: String = redis::cmd("GET").arg(key).query_async(&mut conn).await.unwrap();
        del(&mut conn, &[key]).await;

        assert_eq!(current, "valor-novo", "a escrita real precisa ter sido aplicada");

        let entry = history.get(id).unwrap();
        assert_eq!(entry.host_id, host.id);
        assert_eq!(entry.db, host.db);
        assert_eq!(entry.key, key);
        assert_eq!(entry.actor, Actor::Human);
        assert!(!entry.rolled_back);
        assert_eq!(entry.summary, op_summary(key, &op));

        let before: serde_json::Value =
            serde_json::from_str(entry.before_json.as_deref().expect("before_json ausente"))
                .unwrap();
        assert_eq!(
            before["elements"][0]["value"]["text"], "valor-antigo",
            "o backup precisa conter o valor de ANTES da escrita, não o de depois: {before}"
        );
    }

    #[tokio::test]
    async fn rollback_restores_the_previous_value() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_restores";

        let _: () = redis::cmd("HSET")
            .arg(key)
            .arg("campo1")
            .arg("valor1")
            .arg("campo2")
            .arg("valor2")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let op = WriteOp::SetHashField {
            field: "campo1".to_string(),
            value: text("valor1-mudado"),
        };

        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();

        let changed: String = redis::cmd("HGET")
            .arg(key)
            .arg("campo1")
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(changed, "valor1-mudado");

        history.rollback(&pool, &host, None, id).await.unwrap();

        let restored1: String = redis::cmd("HGET")
            .arg(key)
            .arg("campo1")
            .query_async(&mut conn)
            .await
            .unwrap();
        let restored2: String = redis::cmd("HGET")
            .arg(key)
            .arg("campo2")
            .query_async(&mut conn)
            .await
            .unwrap();

        del(&mut conn, &[key]).await;

        assert_eq!(restored1, "valor1", "campo alterado precisa voltar ao valor original");
        assert_eq!(
            restored2, "valor2",
            "campo não envolvido na escrita precisa sobreviver à volta atrás"
        );

        let entry = history.get(id).unwrap();
        assert!(entry.rolled_back, "a entrada precisa ser marcada como revertida");
    }

    #[tokio::test]
    async fn rollback_of_a_delete_recreates_the_key_with_its_ttl() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_delete_ttl";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-com-ttl")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg(key)
            .arg(300)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let id = history
            .write_with_backup(
                &pool,
                &host,
                None,
                host.db,
                key,
                &WriteOp::DeleteKey,
                Actor::Human,
            )
            .await
            .unwrap();

        let exists_after_delete: bool = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(!exists_after_delete, "a chave precisa ter sido apagada");

        history.rollback(&pool, &host, None, id).await.unwrap();

        let value: String = redis::cmd("GET").arg(key).query_async(&mut conn).await.unwrap();
        let pttl: i64 = redis::cmd("PTTL").arg(key).query_async(&mut conn).await.unwrap();

        del(&mut conn, &[key]).await;

        assert_eq!(value, "valor-com-ttl", "o conteúdo precisa voltar");
        assert!(
            pttl > 0 && pttl <= 300_000,
            "o TTL precisa ter sido recriado (~300s), veio {pttl}"
        );
    }

    #[tokio::test]
    async fn failed_write_leaves_no_history_row() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:failed_write";

        // Chave do tipo Set não é editável — qualquer `WriteOp` sobre ela
        // falha em `apply_write` com `ConnError::NotEditable`, de forma
        // determinística (sem depender de simular queda de rede).
        let _: () = redis::cmd("SADD")
            .arg(key)
            .arg("membro")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let op = WriteOp::SetHashField {
            field: "campo".to_string(),
            value: text("valor"),
        };

        let result = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await;

        del(&mut conn, &[key]).await;

        assert!(result.is_err(), "esperava erro por tipo não editável");
        let entries = history.list(&host.id, 100).unwrap();
        assert!(
            entries.is_empty(),
            "uma escrita que falhou não pode deixar linha no histórico: {entries:?}"
        );
    }

    #[tokio::test]
    async fn backup_of_a_huge_value_restores_it_byte_for_byte() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:huge_value_roundtrip";

        // Maior que `ValueLimit::default().max_bytes` (256 KB) — o limite
        // usado pela tela de detalhes. Um padrão de dígitos repetidos, não
        // um único byte repetido, para que um bug que embaralhasse ou
        // cortasse pedaços do valor não passasse despercebido por engano
        // (um valor feito só de "a"s esconderia exatamente esse tipo de
        // erro).
        let huge_value: String = (0..600_000).map(|i| (b'0' + (i % 10) as u8) as char).collect();
        assert!(huge_value.len() > ValueLimit::default().max_bytes);

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&huge_value)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let op = WriteOp::SetString {
            value: text("valor-pequeno-substituto"),
        };

        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();

        // Confere que a escrita real de fato truncaria se fosse exibida
        // (prova que o valor original era grande o bastante para expor um
        // backup que cortasse a cauda), antes de restaurar.
        let after_write: String = redis::cmd("GET").arg(key).query_async(&mut conn).await.unwrap();
        assert_eq!(after_write, "valor-pequeno-substituto");

        history.rollback(&pool, &host, None, id).await.unwrap();

        let restored: String = redis::cmd("GET").arg(key).query_async(&mut conn).await.unwrap();

        del(&mut conn, &[key]).await;

        assert_eq!(
            restored.len(),
            huge_value.len(),
            "a volta atrás não pode truncar o valor restaurado"
        );
        assert_eq!(
            restored, huge_value,
            "a volta atrás precisa restaurar o valor grande byte a byte, não só no tamanho"
        );
    }

    #[test]
    fn prune_keeps_30_days_or_500_entries_whichever_comes_first() {
        let (_dir, history) = open_history();
        let host = test_host();
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        let now = now_unix_ms_for_test();

        // Host A: algumas entradas recentes e algumas com mais de 30 dias —
        // só as antigas devem sumir.
        let mut recent_ids = Vec::new();
        for i in 0..10 {
            let id = history
                .insert_entry(
                    &host,
                    host.db,
                    &format!("chave_recente_{i}"),
                    "resumo",
                    None,
                    Actor::Human,
                    now - i * 1000,
                )
                .unwrap();
            recent_ids.push(id);
        }
        let mut old_ids = Vec::new();
        for i in 0..5 {
            let id = history
                .insert_entry(
                    &host,
                    host.db,
                    &format!("chave_antiga_{i}"),
                    "resumo",
                    None,
                    Actor::Human,
                    now - 40 * DAY_MS - i * 1000,
                )
                .unwrap();
            old_ids.push(id);
        }

        // Host B: 510 entradas recentes (nenhuma com mais de 30 dias) — só
        // as 500 mais novas devem sobreviver ao limite de contagem.
        let other_host = Host {
            id: "history-test-host-b".to_string(),
            ..host.clone()
        };
        let mut host_b_ids = Vec::new();
        for i in 0..510 {
            // `at` decrescente: i=0 é a mais nova.
            let id = history
                .insert_entry(
                    &other_host,
                    other_host.db,
                    &format!("chave_b_{i}"),
                    "resumo",
                    None,
                    Actor::Human,
                    now - i * 1000,
                )
                .unwrap();
            host_b_ids.push(id);
        }

        let removed_a = history.prune(&host.id, 30, 500).unwrap();
        assert_eq!(removed_a, 5, "só as 5 entradas com mais de 30 dias deveriam sumir");

        for id in &recent_ids {
            assert!(history.get(*id).is_ok(), "entrada recente não deveria ser removida");
        }
        for id in &old_ids {
            assert!(
                matches!(history.get(*id), Err(HistoryError::NotFound(_))),
                "entrada com mais de 30 dias deveria ter sido removida"
            );
        }

        let removed_b = history.prune(&other_host.id, 30, 500).unwrap();
        assert_eq!(removed_b, 10, "das 510 entradas, as 10 mais antigas deveriam sumir");

        let remaining_b = history.list(&other_host.id, 1000).unwrap();
        assert_eq!(remaining_b.len(), 500);
        // As 500 mais novas são i = 0..500 (at = now - i*1000); a mais
        // antiga sobrevivente é i = 499.
        assert!(history.get(host_b_ids[499]).is_ok());
        assert!(matches!(
            history.get(host_b_ids[500]),
            Err(HistoryError::NotFound(_))
        ));
    }

    #[test]
    fn entry_records_whether_the_llm_or_the_human_did_it() {
        let (_dir, history) = open_history();
        let host = test_host();
        let now = now_unix_ms_for_test();

        let human_id = history
            .insert_entry(&host, host.db, "chave", "resumo", None, Actor::Human, now)
            .unwrap();
        let llm_id = history
            .insert_entry(&host, host.db, "chave", "resumo", None, Actor::Llm, now)
            .unwrap();

        assert_eq!(history.get(human_id).unwrap().actor, Actor::Human);
        assert_eq!(history.get(llm_id).unwrap().actor, Actor::Llm);
    }

    /// Mesma lógica de `now_unix_ms` (privada), usada pelos testes
    /// síncronos para montar `at` sem depender do relógio no meio do teste.
    fn now_unix_ms_for_test() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }
}
