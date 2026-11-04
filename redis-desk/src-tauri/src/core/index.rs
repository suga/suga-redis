//! Índice local de busca: um retrato achatado, em texto, de todo valor do
//! banco Redis aberto, guardado numa tabela FTS5 no mesmo arquivo SQLite do
//! histórico (Task 6). É o que torna possível achar um texto em qualquer
//! lugar do banco — dentro de campo de hash, item de lista, membro de sorted
//! set ou um JSON guardado como string — em menos de um segundo: a Task 8
//! consulta esta tabela em vez de perguntar ao Redis toda vez.
//!
//! `rescan` é a varredura incremental: lê `scan_all` para a lista de chaves
//! do (host, db) aberto, lê cada valor por inteiro (até o teto de 64 KB por
//! chave) e troca o retrato inteiro daquele (host, db) de uma vez, dentro de
//! uma transação — nunca mistura chaves de uma varredura antiga com outra
//! nova. Só o host/banco aberto é varrido; os demais ficam como estavam até
//! serem abertos por sua vez.
//!
//! Tokenizer escolhido para a coluna `body`: `trigram`, nativo do FTS5 desde
//! o SQLite 3.34 (presente no SQLite empacotado por este projeto — ver
//! `Cargo.toml`, feature `bundled` do `rusqlite`, que compila o SQLite com
//! `-DSQLITE_ENABLE_FTS5`). Precisa ser decidido já na criação da tabela
//! (`CREATE VIRTUAL TABLE ... USING fts5(...)`) porque o FTS5 não permite
//! trocar o tokenizer de uma tabela existente sem recriá-la — e é exatamente
//! o tokenizer que faz a busca por SUBSTRING da Task 8 funcionar: ele indexa
//! trigramas de caracteres em vez de palavras inteiras, então `MATCH
//! 'arco@emp'` encontra `marco@empresa.com` sem precisar de `LIKE '%...%'`
//! (que não usaria nenhum índice). O tokenizer padrão do FTS5 (`unicode61`)
//! quebra em PALAVRAS, não serviria: a Task 8 precisa de correspondência por
//! pedaço de palavra, não só a palavra inteira.

use std::path::Path;
use std::sync::Mutex;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use super::config::Host;
use super::conn::{ConnError, Pool};
use super::keys::{KeyKind, scan_all};
use super::value::{Value, ValueLimit, ValueView, read_value};

/// Teto de bytes do texto achatado (`body`) guardado por chave — o valor
/// inteiro nunca é gravado no índice, não importa o quão grande seja no
/// Redis (este é um dos dois modos de falha avaliados nesta task: "um valor
/// de megabytes"). Também é o `max_bytes` passado a `read_value`: não faz
/// sentido pedir ao Redis mais bytes de um elemento do que o próprio teto do
/// índice vai aceitar.
const BODY_CAP_BYTES: usize = 64 * 1024;

/// Teto de elementos pedidos por chave (hash/lista/sorted set/set) numa
/// única varredura — generoso o bastante para não cortar coleções de uso
/// prático antes mesmo de `BODY_CAP_BYTES` entrar em ação, mas finito para
/// que uma coleção absurdamente grande não vire um pedido sem fim ao Redis.
const MAX_ELEMENTS: usize = 20_000;

/// Erros deste módulo: falha ao abrir/gravar no SQLite do índice ou erro de
/// conexão propagado de `scan_all`/`read_value`.
#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("falha de E/S ao abrir o índice: {0}")]
    Io(#[from] std::io::Error),

    #[error("falha no banco do índice: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error(transparent)]
    Conn(#[from] ConnError),
}

/// Mapeia o erro da abertura compartilhada (`core::sqlite::open_shared`)
/// para as variantes já existentes de `IndexError` — mesmo papel que a
/// conversão equivalente em `core::history`.
impl From<super::sqlite::OpenError> for IndexError {
    fn from(err: super::sqlite::OpenError) -> IndexError {
        match err {
            super::sqlite::OpenError::Io(err) => IndexError::Io(err),
            super::sqlite::OpenError::Sql(err) => IndexError::Sql(err),
        }
    }
}

/// Resultado padrão das operações deste módulo.
pub type Result<T> = std::result::Result<T, IndexError>;

/// Progresso de uma varredura em andamento: quantas chaves já foram
/// processadas e uma estimativa do total (o total real de `scan_all`, que já
/// é conhecido assim que a lista de chaves chega — não é uma estimativa no
/// sentido de incerta, é só nomeada assim porque a UI a trata como tal, já
/// que chaves podem aparecer/sumir no servidor durante a varredura).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    pub scanned: u64,
    pub total_estimate: u64,
}

/// Resultado de uma varredura concluída, também o que fica persistido (e é
/// devolvido depois por `last_scan`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanReport {
    pub host_id: String,
    pub db: u8,
    pub keys: u64,
    pub finished_at: i64,
    pub duration_ms: u64,
}

/// Cria a tabela FTS5 do índice (ver o comentário do módulo sobre o
/// tokenizer) e a tabela comum que guarda o relatório da última varredura
/// por (host, db), se ainda não existirem.
const SCHEMA: &str = "
CREATE VIRTUAL TABLE IF NOT EXISTS key_index USING fts5(
    host_id UNINDEXED,
    db UNINDEXED,
    key UNINDEXED,
    kind UNINDEXED,
    ttl_ms UNINDEXED,
    body,
    truncated UNINDEXED,
    binary UNINDEXED,
    tokenize = 'trigram'
);
CREATE TABLE IF NOT EXISTS scan_report (
    host_id TEXT NOT NULL,
    db INTEGER NOT NULL,
    keys INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    duration_ms INTEGER NOT NULL,
    PRIMARY KEY (host_id, db)
);
";

pub struct Index {
    conn: Mutex<Connection>,
}

impl Index {
    /// Abre (ou cria) o índice em `db_path` — o MESMO arquivo do histórico
    /// (Task 6), numa conexão própria (ver o comentário de
    /// `core::sqlite::open_shared`).
    pub fn open(db_path: &Path) -> Result<Index> {
        let conn = super::sqlite::open_shared(db_path)?;
        conn.execute_batch(SCHEMA)?;

        Ok(Index {
            conn: Mutex::new(conn),
        })
    }

    /// Refaz o retrato do índice para um (host, db) inteiro: lista as
    /// chaves com `scan_all`, lê o valor de cada uma (até `BODY_CAP_BYTES`)
    /// e troca o conteúdo antigo daquele (host, db) pelo novo, tudo dentro
    /// de uma única transação — nenhuma consulta feita por Task 8 em
    /// paralelo pode ver um estado parcialmente trocado.
    ///
    /// A leitura de cada chave acontece FORA da transação (conversas com o
    /// Redis, por natureza lentas e de duração variável, não podem segurar
    /// um lock do SQLite); só a escrita final é atômica. `progress` é
    /// chamado ao menos uma vez (antes do laço, com `scanned = 0`) mesmo
    /// quando o banco está vazio, e mais uma vez a cada chave processada.
    ///
    /// Uma chave que suma entre o `scan_all` e a leitura do valor
    /// (`ConnError::KeyGone`) ou que falhe por qualquer outro motivo de
    /// conexão é simplesmente pulada — uma chave problemática não pode
    /// derrubar a varredura do banco inteiro.
    pub async fn rescan(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        db: u8,
        progress: impl Fn(ScanProgress),
    ) -> Result<ScanReport> {
        let started = Instant::now();
        let metas = scan_all(pool, host, pw, db).await?;
        let total = metas.len() as u64;
        progress(ScanProgress {
            scanned: 0,
            total_estimate: total,
        });

        let mut rows = Vec::with_capacity(metas.len());
        for (i, meta) in metas.iter().enumerate() {
            let limit = ValueLimit {
                offset: 0,
                max_bytes: BODY_CAP_BYTES,
                max_elements: MAX_ELEMENTS,
            };
            match read_value(pool, host, pw, db, &meta.key, limit).await {
                Ok(view) => rows.push(build_row(&meta.key, &view)),
                Err(ConnError::KeyGone(_)) => {
                    // Sumiu entre o SCAN (dentro de `scan_all`) e esta
                    // leitura — não entra no retrato novo, sem erro.
                }
                Err(_) => {
                    // Qualquer outro erro ao ler esta chave específica não
                    // pode abortar a varredura do banco inteiro (mesmo
                    // espírito de `scan_all` degradando `size_bytes` para 0
                    // quando `MEMORY USAGE` falha).
                }
            }
            progress(ScanProgress {
                scanned: (i + 1) as u64,
                total_estimate: total,
            });
        }

        let finished_at = now_unix_ms();
        let duration_ms = started.elapsed().as_millis() as u64;
        let keys = rows.len() as u64;

        {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction()?;
            tx.execute(
                "DELETE FROM key_index WHERE host_id = ?1 AND db = ?2",
                params![host.id, db as i64],
            )?;
            {
                let mut stmt = tx.prepare(
                    "INSERT INTO key_index (host_id, db, key, kind, ttl_ms, body, truncated, binary) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )?;
                for row in &rows {
                    stmt.execute(params![
                        host.id,
                        db as i64,
                        row.key,
                        row.kind,
                        row.ttl_ms,
                        row.body,
                        row.truncated as i64,
                        row.binary as i64,
                    ])?;
                }
            }
            tx.execute(
                "INSERT INTO scan_report (host_id, db, keys, finished_at, duration_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
                 ON CONFLICT(host_id, db) DO UPDATE SET \
                     keys = excluded.keys, \
                     finished_at = excluded.finished_at, \
                     duration_ms = excluded.duration_ms",
                params![
                    host.id,
                    db as i64,
                    keys as i64,
                    finished_at,
                    duration_ms as i64
                ],
            )?;
            tx.commit()?;
        }

        Ok(ScanReport {
            host_id: host.id.clone(),
            db,
            keys,
            finished_at,
            duration_ms,
        })
    }

    /// O relatório da última `rescan` bem-sucedida para (host_id, db), ou
    /// `None` se nenhuma varredura ainda rodou para esse par.
    pub fn last_scan(&self, host_id: &str, db: u8) -> Result<Option<ScanReport>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT keys, finished_at, duration_ms FROM scan_report \
             WHERE host_id = ?1 AND db = ?2",
        )?;
        let report = stmt
            .query_row(params![host_id, db as i64], |row| {
                Ok(ScanReport {
                    host_id: host_id.to_string(),
                    db,
                    keys: row.get::<_, i64>(0)? as u64,
                    finished_at: row.get(1)?,
                    duration_ms: row.get::<_, i64>(2)? as u64,
                })
            })
            .optional()?;
        Ok(report)
    }

    /// Remove uma única chave do índice, sem esperar a próxima varredura
    /// completa — para quando uma escrita (Task 9+) sabe que apagou a chave
    /// e quer que a busca pare de encontrá-la imediatamente.
    pub fn forget_key(&self, host_id: &str, db: u8, key: &str) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "DELETE FROM key_index WHERE host_id = ?1 AND db = ?2 AND key = ?3",
            params![host_id, db as i64, key],
        )?;
        Ok(())
    }
}

/// Uma linha pronta para inserir no índice — tudo que `build_row` calcula a
/// partir de um `ValueView`, antes de `host_id`/`db` (conhecidos só por
/// `rescan`, iguais para toda a leva) entrarem no `INSERT`.
struct IndexedRow {
    key: String,
    kind: &'static str,
    ttl_ms: Option<i64>,
    body: String,
    truncated: bool,
    binary: bool,
}

/// Nome do tipo (`kind`) gravado no índice — mesmo vocabulário usado por
/// `core::history` (`ValueSnapshot::kind`), para quem for ler as duas
/// tabelas não ter que aprender dois vocabulários diferentes para a mesma
/// ideia.
fn kind_label(kind: &KeyKind) -> &'static str {
    match kind {
        KeyKind::String => "string",
        KeyKind::Hash => "hash",
        KeyKind::List => "list",
        KeyKind::ZSet => "zset",
        KeyKind::Set => "set",
        KeyKind::Stream => "stream",
        KeyKind::Json => "json",
        KeyKind::Unknown(_) => "unknown",
    }
}

/// Achata um `Value` lido do Redis no texto que vai para `body`: string é
/// ela mesma; hash vira uma linha `campo valor` por par; lista, set e sorted
/// set viram um elemento por linha (só o membro, sem o score do sorted set —
/// o score não é texto livre que alguém procuraria); um JSON nativo
/// (RedisJSON) ou um tipo não reconhecido, lidos como texto somente-leitura
/// por `core::value`, entram como o próprio texto.
///
/// Um elemento cujo `Bytes::text` é `None` (não é UTF-8 válido) é OMITIDO do
/// texto — nunca entra como base64: um base64 de bytes binários é lixo de
/// trigramas que poluiria a busca por acidente (ver o Review Focus 4 desta
/// task), sem ajudar ninguém a achar nada. O segundo valor devolvido avisa
/// que isso aconteceu em pelo menos um elemento, para a coluna `binary`.
fn flatten_body(value: &Value) -> (String, bool) {
    match value {
        Value::Str(bytes) => match &bytes.text {
            Some(text) => (text.clone(), false),
            None => (String::new(), true),
        },
        Value::Hash(pairs) => {
            let mut has_binary = false;
            let mut lines = Vec::with_capacity(pairs.len());
            for (field, bytes) in pairs {
                match &bytes.text {
                    Some(text) => lines.push(format!("{field} {text}")),
                    None => has_binary = true,
                }
            }
            (lines.join("\n"), has_binary)
        }
        Value::List(items) | Value::Set(items) => {
            let mut has_binary = false;
            let mut lines = Vec::with_capacity(items.len());
            for bytes in items {
                match &bytes.text {
                    Some(text) => lines.push(text.clone()),
                    None => has_binary = true,
                }
            }
            (lines.join("\n"), has_binary)
        }
        Value::ZSet(items) => {
            let mut has_binary = false;
            let mut lines = Vec::with_capacity(items.len());
            for (bytes, _score) in items {
                match &bytes.text {
                    Some(text) => lines.push(text.clone()),
                    None => has_binary = true,
                }
            }
            (lines.join("\n"), has_binary)
        }
        Value::Stream(entries) => {
            let mut has_binary = false;
            let mut lines = Vec::with_capacity(entries.len());
            for entry in entries {
                for (field, bytes) in &entry.fields {
                    match &bytes.text {
                        Some(text) => lines.push(format!("{field} {text}")),
                        None => has_binary = true,
                    }
                }
            }
            (lines.join("\n"), has_binary)
        }
        Value::ReadOnlyText(text) => (text.clone(), false),
    }
}

/// Corta `body` em `max_bytes`, sem partir um caractere multibyte ao meio —
/// mesma técnica de `core::value::char_boundary_prefix`, reimplementada
/// aqui porque aquela função é privada ao módulo `value`.
fn cap_body(body: String, max_bytes: usize) -> (String, bool) {
    if body.len() <= max_bytes {
        return (body, false);
    }
    let mut end = max_bytes;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (body[..end].to_string(), true)
}

/// Monta a linha do índice para `key` a partir do valor já lido. `truncated`
/// é verdadeiro se o corte em 64 KB cortou o texto achatado OU se o próprio
/// `ValueView` já vinha truncado (elemento individual cortado por
/// `BODY_CAP_BYTES`, ou coleção maior que `MAX_ELEMENTS`) — os dois são
/// formas de "o que está aqui não é o valor completo".
fn build_row(key: &str, view: &ValueView) -> IndexedRow {
    let (body, has_binary) = flatten_body(&view.value);
    let (body, body_was_cut) = cap_body(body, BODY_CAP_BYTES);

    IndexedRow {
        key: key.to_string(),
        kind: kind_label(&view.kind),
        ttl_ms: view.ttl_ms,
        body,
        truncated: body_was_cut || view.truncated,
        binary: has_binary,
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("relógio do sistema antes de 1970")
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::super::config::{Env, Host};
    use super::super::conn::Pool;
    use super::*;
    use redis::aio::MultiplexedConnection;
    use rusqlite::params;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_host() -> Host {
        Host {
            id: "index-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            // db5: db0-4 já são usados pelos testes de conn/keys/value/history;
            // a suíte roda em paralelo contra o mesmo container.
            db: 5,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    fn open_index() -> (tempfile::TempDir, Index) {
        let dir = tempfile::TempDir::new().unwrap();
        let index = Index::open(&dir.path().join("redis-desk.sqlite")).unwrap();
        (dir, index)
    }

    async fn del(conn: &mut MultiplexedConnection, keys: &[&str]) {
        for key in keys {
            let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
        }
    }

    /// Lê uma linha do índice diretamente via SQL — os testes acessam o
    /// campo privado `conn` de `Index` porque este módulo de testes é
    /// descendente do módulo que o declara (mesmo padrão de `history.rs`).
    struct RowSnapshot {
        kind: String,
        ttl_ms: Option<i64>,
        body: String,
        truncated: bool,
        binary: bool,
    }

    fn fetch_row(index: &Index, host_id: &str, db: u8, key: &str) -> Option<RowSnapshot> {
        use rusqlite::OptionalExtension;
        let conn = index.conn.lock().unwrap();
        conn.query_row(
            "SELECT kind, ttl_ms, body, truncated, binary FROM key_index \
             WHERE host_id = ?1 AND db = ?2 AND key = ?3",
            params![host_id, db as i64, key],
            |row| {
                Ok(RowSnapshot {
                    kind: row.get(0)?,
                    ttl_ms: row.get(1)?,
                    body: row.get(2)?,
                    truncated: row.get::<_, i64>(3)? != 0,
                    binary: row.get::<_, i64>(4)? != 0,
                })
            },
        )
        .optional()
        .unwrap()
    }

    fn now_unix_ms_for_test() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
    }

    #[tokio::test]
    async fn rescan_indexes_every_type_into_body() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let prefix = "index_rs_test:every_type";
        let string_key = format!("{prefix}:string");
        let hash_key = format!("{prefix}:hash");
        let list_key = format!("{prefix}:list");
        let zset_key = format!("{prefix}:zset");

        let _: () = redis::cmd("SET")
            .arg(&string_key)
            .arg("contato: marco@empresa.com")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("HSET")
            .arg(&hash_key)
            .arg("email")
            .arg("marco@empresa.com")
            .arg("nome")
            .arg("Marco")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("RPUSH")
            .arg(&list_key)
            .arg("item-um")
            .arg("marco@empresa.com")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("ZADD")
            .arg(&zset_key)
            .arg(1)
            .arg("marco@empresa.com")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("EXPIRE")
            .arg(&string_key)
            .arg(300)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        del(&mut conn, &[&string_key, &hash_key, &list_key, &zset_key]).await;

        let string_row =
            fetch_row(&index, &host.id, host.db, &string_key).expect("chave string não indexada");
        assert_eq!(string_row.kind, "string");
        assert!(string_row.body.contains("marco@empresa.com"));
        assert!(
            matches!(string_row.ttl_ms, Some(ms) if ms > 0),
            "TTL da chave string precisava ter sido indexado, veio {:?}",
            string_row.ttl_ms
        );
        assert!(!string_row.binary);

        let hash_row =
            fetch_row(&index, &host.id, host.db, &hash_key).expect("chave hash não indexada");
        assert_eq!(hash_row.kind, "hash");
        assert!(hash_row.body.contains("email marco@empresa.com"));
        assert!(hash_row.body.contains("nome Marco"));
        assert_eq!(hash_row.ttl_ms, None);

        let list_row =
            fetch_row(&index, &host.id, host.db, &list_key).expect("chave list não indexada");
        assert_eq!(list_row.kind, "list");
        assert!(list_row.body.contains("item-um"));
        assert!(list_row.body.contains("marco@empresa.com"));

        let zset_row =
            fetch_row(&index, &host.id, host.db, &zset_key).expect("chave zset não indexada");
        assert_eq!(zset_row.kind, "zset");
        assert!(zset_row.body.contains("marco@empresa.com"));
    }

    #[tokio::test]
    async fn body_is_capped_at_64kb_and_flagged() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "index_rs_test:huge_value";

        // Padrão de dígitos repetidos, não um único byte repetido, para que
        // um corte que embaralhasse ou pulasse um pedaço não passasse
        // despercebido por coincidência.
        let huge_value: String = (0..200_000)
            .map(|i| (b'0' + (i % 10) as u8) as char)
            .collect();

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(&huge_value)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        del(&mut conn, &[key]).await;

        let row = fetch_row(&index, &host.id, host.db, key).expect("chave não indexada");

        assert!(
            row.truncated,
            "um valor de 200KB precisa ser marcado truncated"
        );
        assert!(
            row.body.len() <= 64 * 1024,
            "body não pode passar de 64KB, veio {} bytes",
            row.body.len()
        );
        assert_eq!(
            row.body,
            huge_value[..row.body.len()],
            "o prefixo indexado precisa bater com o início do valor real"
        );
    }

    #[tokio::test]
    async fn binary_value_is_indexed_as_binary_not_text() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let binary_key = "index_rs_test:binary_value";
        let text_key = "index_rs_test:binary_sibling_text";
        let raw: &[u8] = &[0xff, 0xfe, 0x00, 0x01];

        let _: () = redis::cmd("SET")
            .arg(binary_key)
            .arg(raw)
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(text_key)
            .arg("chave normal depois da binária")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        let report = index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        del(&mut conn, &[binary_key, text_key]).await;

        // A varredura não pode abortar por causa da chave binária — a outra
        // chave, que vem logo depois dela, precisa ter sido indexada também.
        assert!(report.keys >= 2);

        let binary_row =
            fetch_row(&index, &host.id, host.db, binary_key).expect("chave binária não indexada");
        assert!(binary_row.binary, "deveria estar marcada como binary");
        assert_eq!(
            binary_row.body, "",
            "body de valor binário precisa ser vazio"
        );

        let text_row =
            fetch_row(&index, &host.id, host.db, text_key).expect("chave de texto não indexada");
        assert!(!text_row.binary);
        assert!(text_row.body.contains("chave normal depois da binária"));
    }

    #[tokio::test]
    async fn rescan_replaces_the_previous_snapshot() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let survives_key = "index_rs_test:replace_snapshot:survives";
        let removed_key = "index_rs_test:replace_snapshot:removed";

        let _: () = redis::cmd("SET")
            .arg(survives_key)
            .arg("fica")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(removed_key)
            .arg("vai sumir")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        assert!(fetch_row(&index, &host.id, host.db, survives_key).is_some());
        assert!(fetch_row(&index, &host.id, host.db, removed_key).is_some());

        let _: () = redis::cmd("DEL")
            .arg(removed_key)
            .query_async(&mut conn)
            .await
            .unwrap();

        index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();

        del(&mut conn, &[survives_key]).await;

        assert!(
            fetch_row(&index, &host.id, host.db, survives_key).is_some(),
            "chave que ainda existe precisa continuar indexada"
        );
        assert!(
            fetch_row(&index, &host.id, host.db, removed_key).is_none(),
            "chave apagada no servidor precisa sumir do índice após a nova varredura"
        );
    }

    #[tokio::test]
    async fn last_scan_reports_time_and_count() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key1 = "index_rs_test:last_scan:a";
        let key2 = "index_rs_test:last_scan:b";
        let _: () = redis::cmd("SET")
            .arg(key1)
            .arg("valor")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(key2)
            .arg("valor")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();

        assert!(
            index.last_scan(&host.id, host.db).unwrap().is_none(),
            "sem varredura ainda, não pode haver relatório"
        );

        let before = now_unix_ms_for_test();
        let report = index
            .rescan(&pool, &host, None, host.db, |_| {})
            .await
            .unwrap();
        let after = now_unix_ms_for_test();

        del(&mut conn, &[key1, key2]).await;

        assert!(
            report.keys >= 2,
            "esperava pelo menos as 2 chaves criadas, veio {}",
            report.keys
        );
        assert!(
            report.finished_at >= before && report.finished_at <= after,
            "finished_at ({}) precisa estar entre {before} e {after}",
            report.finished_at
        );

        let stored = index
            .last_scan(&host.id, host.db)
            .unwrap()
            .expect("relatório ausente após rescan");
        assert_eq!(
            stored.keys, report.keys,
            "last_scan precisa refletir exatamente o que rescan devolveu"
        );
        assert_eq!(stored.finished_at, report.finished_at);
        assert_eq!(stored.duration_ms, report.duration_ms);

        assert!(fetch_row(&index, &host.id, host.db, key1).is_some());
        assert!(fetch_row(&index, &host.id, host.db, key2).is_some());
    }

    #[tokio::test]
    async fn progress_callback_is_called_at_least_once() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let key = "index_rs_test:progress_callback";
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor")
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, index) = open_index();
        let calls = std::sync::Mutex::new(Vec::new());
        index
            .rescan(&pool, &host, None, host.db, |p| {
                calls.lock().unwrap().push(p)
            })
            .await
            .unwrap();

        del(&mut conn, &[key]).await;

        let calls = calls.into_inner().unwrap();
        assert!(
            !calls.is_empty(),
            "progress precisa ser chamado pelo menos uma vez"
        );
        let last = calls.last().unwrap();
        assert_eq!(
            last.scanned, last.total_estimate,
            "a última chamada deveria reportar a varredura completa"
        );
        assert!(
            last.total_estimate >= 1,
            "o db tinha pelo menos a chave criada por este teste — total_estimate não pode ser 0"
        );
    }
}
