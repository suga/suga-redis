//! Abertura compartilhada do arquivo SQLite único (`suga-redis.sqlite`) usado
//! pelo histórico (Task 6) e, a partir desta task, também pelo índice de
//! busca: as mesmas duas pragmas (`journal_mode = WAL`, `busy_timeout`)
//! precisam valer para toda conexão que toca este arquivo — `History` e
//! `Index` abrem conexões SEPARADAS para o mesmo caminho (cada uma com seu
//! próprio `Mutex<Connection>`), e divergir essas pragmas entre elas
//! reintroduziria exatamente o travamento por `SQLITE_BUSY` que elas existem
//! para evitar. Por isso a abertura mora aqui, num único lugar, em vez de
//! duplicada em cada módulo.

use std::path::Path;

use rusqlite::Connection;

/// Tempo de espera, em milissegundos, antes do SQLite desistir com
/// `SQLITE_BUSY` quando outra conexão está escrevendo no mesmo arquivo —
/// generoso o bastante para o histórico, o índice e (a partir da Task 15) o
/// processo do servidor MCP coexistirem sem se atropelar em uso normal.
const BUSY_TIMEOUT_MS: i64 = 5000;

/// Erro ao abrir o arquivo compartilhado: falha de E/S (ex.: diretório sem
/// permissão) ou falha do próprio SQLite ao abrir/configurar a conexão.
#[derive(Debug, thiserror::Error)]
pub(crate) enum OpenError {
    #[error("falha de E/S ao abrir o banco: {0}")]
    Io(#[from] std::io::Error),

    #[error("falha ao abrir o banco: {0}")]
    Sql(#[from] rusqlite::Error),
}

/// Abre (ou cria) `db_path`, sempre com `journal_mode = WAL` e
/// `busy_timeout = 5000`. Não roda nenhum `CREATE TABLE`: cada chamador
/// aplica seu próprio schema (tabelas diferentes, no mesmo arquivo) logo após
/// chamar esta função.
pub(crate) fn open_shared(db_path: &Path) -> Result<Connection, OpenError> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(db_path)?;
    // `journal_mode` sempre devolve uma linha com o modo resultante — ao
    // contrário da maioria das PRAGMAs, não dá para gravar com
    // `pragma_update` puro.
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))?;
    conn.pragma_update(None, "busy_timeout", BUSY_TIMEOUT_MS)?;

    Ok(conn)
}
