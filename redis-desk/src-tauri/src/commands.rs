//! Fachada de comandos: a única porta entre a interface e o núcleo.
//!
//! Cada função pública aqui tem uma gêmea `#[tauri::command]` logo abaixo
//! dela, e uma terceira gêmea em `src/lib/api.ts`. A separação entre as duas
//! primeiras existe para que a lógica seja testável: um `#[tauri::command]`
//! recebe `State<'_, AppState>`, que só existe dentro de um app Tauri de
//! verdade, enquanto a função de dentro recebe `&AppState` e roda num teste
//! comum. O comando é casca fina — resolve o `State` e delega.
//!
//! # A trava de produção
//!
//! Toda escrita passa por `write_value` ou `rollback`, e as duas obedecem à
//! mesma regra: host de ambiente `prod` com `confirmed == false` NÃO escreve.
//! Devolve `WriteOutcome::NeedsConfirmation` com a frase que a pessoa vai
//! ler no diálogo. A decisão mora aqui, de propósito, e não na interface: a
//! interface pode ter um botão que esquece de perguntar, mas nenhum caminho
//! chega ao Redis sem passar por esta função.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::approval::{
    ApprovalError, ApprovalQueue, ApprovalRequest, ApprovalVerdict, serve, socket_path,
};
use crate::core::config::{ConfigError, Env, Host, HostStore};
use crate::core::conn::{ConnError, Pool};
use crate::core::history::{Actor, History, HistoryEntry, HistoryError};
use crate::core::index::{Index, IndexError, ScanProgress, ScanReport};
use crate::core::keys::{TreeNode, build_tree, scan_all};
use crate::core::search::{SearchOutcome, search as search_index};
use crate::core::value::{
    ValueLimit, ValueView, WriteOp, op_summary, read_value as read_value_from_redis,
};

/// Teto de bytes por elemento numa leitura de valor para a tela. Igual ao
/// teto do índice (Task 7) por coerência: a pessoa que achou um texto na
/// busca vê, ao abrir a chave, pelo menos o mesmo que a busca enxergou.
const PAGE_BYTES: usize = 64 * 1024;

/// Quantos elementos de uma coleção vêm por página. `read_value_more` traz
/// a próxima fatia a partir de um `offset`.
const PAGE_ELEMENTS: usize = 500;

/// Teto de acertos devolvidos por uma busca. A interface mostra uma lista
/// para a pessoa percorrer com o olho, não um relatório: além disso o que
/// falta é refinar a agulha, não rolar mais.
const SEARCH_LIMIT: usize = 200;

/// Nome do arquivo SQLite compartilhado por histórico e índice, dentro do
/// diretório de dados do app.
const DB_FILE: &str = "redis-desk.sqlite";

/// Evento emitido durante `rescan`, um por chave processada, para a barra
/// de progresso da interface.
pub const RESCAN_PROGRESS_EVENT: &str = "rescan://progress";

/// Erro de qualquer comando. Existe para dar à interface um DISCRIMINANTE
/// estável (`kind`) ao lado da frase em português (`message`): a tela
/// precisa reagir diferente a "essa chave não existe mais" (tirar a chave da
/// árvore — Review Focus 3) do que a "o host não responde" (manter tudo na
/// tela e deixar tentar de novo), e decidir isso lendo o texto da mensagem
/// seria casar a interface com a redação dela.
#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error(transparent)]
    Config(#[from] ConfigError),

    #[error(transparent)]
    Conn(#[from] ConnError),

    #[error(transparent)]
    Index(#[from] IndexError),

    #[error(transparent)]
    History(#[from] HistoryError),

    #[error("host \"{0}\" não está cadastrado")]
    UnknownHost(String),

    #[error(transparent)]
    Approval(#[from] ApprovalError),
}

/// A forma como um `CommandError` chega na interface.
#[derive(Debug, Serialize)]
struct WireError {
    kind: &'static str,
    message: String,
}

impl CommandError {
    /// O discriminante estável lido pela interface. Os erros de conexão são
    /// abertos variante a variante porque é exatamente entre eles que a tela
    /// precisa distinguir.
    fn kind(&self) -> &'static str {
        match self {
            CommandError::Conn(ConnError::Unreachable { .. }) => "unreachable",
            CommandError::Conn(ConnError::Auth { .. }) => "auth",
            CommandError::Conn(ConnError::KeyGone(_)) => "key_gone",
            CommandError::Conn(ConnError::NotEditable { .. }) => "not_editable",
            CommandError::Conn(ConnError::Ambiguous { .. }) => "ambiguous",
            CommandError::Conn(ConnError::Protocol(_)) => "protocol",
            CommandError::Config(_) => "config",
            CommandError::Index(_) => "index",
            CommandError::History(_) => "history",
            CommandError::UnknownHost(_) => "unknown_host",
            CommandError::Approval(ApprovalError::NameMismatch) => "name_mismatch",
            CommandError::Approval(_) => "approval",
        }
    }
}

impl Serialize for CommandError {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        WireError {
            kind: self.kind(),
            message: self.to_string(),
        }
        .serialize(serializer)
    }
}

pub type Result<T> = std::result::Result<T, CommandError>;

/// O que aconteceu com um pedido de escrita.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum WriteOutcome {
    /// A escrita aconteceu; `entry_id` é a entrada de histórico que a
    /// registra, e é por ele que um "desfazer" futuro a encontra.
    Applied { entry_id: i64 },
    /// A escrita NÃO aconteceu: é produção e ninguém confirmou ainda.
    /// `summary` é a frase do diálogo.
    NeedsConfirmation { summary: String },
}

/// Tudo que os comandos precisam, montado uma vez na subida do app.
pub struct AppState {
    /// `Mutex` e não `RwLock`: `HostStore` guarda as entradas do chaveiro em
    /// `RefCell` e por isso nem a leitura é compartilhável.
    pub hosts: Mutex<HostStore>,
    pub pool: Pool,
    pub index: Index,
    pub history: History,
    /// Os pedidos de escrita que a LLM mandou e que esperam o dono.
    /// `Arc` porque o servidor do socket roda numa tarefa própria e precisa
    /// da MESMA fila que os comandos da tela leem.
    pub approvals: Arc<ApprovalQueue>,
}

impl AppState {
    /// Abre (ou cria) o estado no diretório de dados do app: `hosts.json`
    /// pelo `HostStore`, e o SQLite único compartilhado entre histórico e
    /// índice.
    pub fn open(dir: &Path) -> Result<AppState> {
        let db_path: PathBuf = dir.join(DB_FILE);
        Ok(AppState {
            hosts: Mutex::new(HostStore::open(dir)?),
            pool: Pool::new(),
            index: Index::open(&db_path)?,
            history: History::open(&db_path)?,
            approvals: Arc::new(ApprovalQueue::new()),
        })
    }

    /// O host e a senha dele, resolvidos juntos — nenhuma operação precisa
    /// de um sem o outro, e separá-los daria duas chances de esquecer a
    /// senha.
    pub(crate) fn host_and_password(&self, host_id: &str) -> Result<(Host, Option<String>)> {
        let hosts = self.hosts.lock().unwrap();
        let host = hosts
            .list()
            .into_iter()
            .find(|host| host.id == host_id)
            .ok_or_else(|| CommandError::UnknownHost(host_id.to_string()))?;
        let password = hosts.password(host_id)?;
        Ok((host, password))
    }
}

// ---------------------------------------------------------------- hosts

pub fn list_hosts(state: &AppState) -> Result<Vec<Host>> {
    Ok(state.hosts.lock().unwrap().list())
}

pub fn add_host(state: &AppState, host: Host, password: Option<String>) -> Result<String> {
    Ok(state.hosts.lock().unwrap().add(host, password.as_deref())?)
}

pub fn update_host(state: &AppState, host: Host, password: Option<String>) -> Result<()> {
    state
        .hosts
        .lock()
        .unwrap()
        .update(host, password.as_deref())?;
    Ok(())
}

pub fn remove_host(state: &AppState, host_id: &str) -> Result<()> {
    state.hosts.lock().unwrap().remove(host_id)?;
    Ok(())
}

/// Testa a conexão com o host, devolvendo o tempo de resposta em
/// milissegundos.
pub async fn test_host(state: &AppState, host_id: &str) -> Result<u64> {
    let (host, pw) = state.host_and_password(host_id)?;
    let elapsed = state.pool.ping(&host, pw.as_deref()).await?;
    Ok(elapsed.as_millis() as u64)
}

/// Os bancos do host que têm ao menos uma chave, com a contagem de cada um.
pub async fn list_dbs(state: &AppState, host_id: &str) -> Result<Vec<(u8, u64)>> {
    let (host, pw) = state.host_and_password(host_id)?;
    Ok(state.pool.db_sizes(&host, pw.as_deref()).await?)
}

// ----------------------------------------------------------------- chaves

pub async fn load_tree(state: &AppState, host_id: &str, db: u8) -> Result<Vec<TreeNode>> {
    let (host, pw) = state.host_and_password(host_id)?;
    let keys = scan_all(&state.pool, &host, pw.as_deref(), db).await?;
    Ok(build_tree(&keys))
}

/// Refaz o índice de busca do (host, banco). `progress` é chamado a cada
/// chave para a barra de progresso.
pub async fn rescan(
    state: &AppState,
    host_id: &str,
    db: u8,
    progress: impl Fn(ScanProgress),
) -> Result<ScanReport> {
    let (host, pw) = state.host_and_password(host_id)?;
    Ok(state
        .index
        .rescan(&state.pool, &host, pw.as_deref(), db, progress)
        .await?)
}

/// De quando é o índice deste (host, banco) — `None` se nunca foi varrido.
pub fn scan_status(state: &AppState, host_id: &str, db: u8) -> Result<Option<ScanReport>> {
    Ok(state.index.last_scan(host_id, db)?)
}

// ----------------------------------------------------------------- valor

pub async fn read_value(state: &AppState, host_id: &str, db: u8, key: &str) -> Result<ValueView> {
    read_value_more(state, host_id, db, key, 0).await
}

/// A próxima fatia de uma coleção grande, a partir de `offset`.
pub async fn read_value_more(
    state: &AppState,
    host_id: &str,
    db: u8,
    key: &str,
    offset: usize,
) -> Result<ValueView> {
    let (host, pw) = state.host_and_password(host_id)?;
    let limit = ValueLimit {
        offset,
        max_bytes: PAGE_BYTES,
        max_elements: PAGE_ELEMENTS,
    };
    Ok(read_value_from_redis(&state.pool, &host, pw.as_deref(), db, key, limit).await?)
}

/// Aplica `op` em `key`, com cópia de segurança e entrada de histórico.
///
/// Em host de ambiente `prod` com `confirmed == false`, NÃO escreve: devolve
/// a frase do diálogo e sai. Ver "A trava de produção" no topo do módulo.
pub async fn write_value(
    state: &AppState,
    host_id: &str,
    db: u8,
    key: &str,
    op: WriteOp,
    confirmed: bool,
) -> Result<WriteOutcome> {
    let (host, pw) = state.host_and_password(host_id)?;

    if needs_confirmation(host.env, confirmed) {
        return Ok(WriteOutcome::NeedsConfirmation {
            summary: op_summary(key, &op),
        });
    }

    let entry_id = state
        .history
        .write_with_backup(
            &state.pool,
            &host,
            pw.as_deref(),
            db,
            key,
            &op,
            Actor::Human,
        )
        .await?;

    // O índice sabia desta chave com o valor ANTIGO. Apagar a linha é o
    // conserto honesto e barato: a busca para de devolver um texto que não
    // está mais lá, e a próxima varredura reindexa a chave com o valor novo.
    // Reindexar só esta chave aqui exigiria relê-la do Redis — custo de uma
    // ida e volta a mais em toda escrita, para adiantar o que a varredura
    // faz de qualquer jeito.
    state.index.forget_key(host_id, db, key)?;

    Ok(WriteOutcome::Applied { entry_id })
}

// -------------------------------------------------------------- histórico

pub fn list_history(state: &AppState, host_id: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
    Ok(state.history.list(host_id, limit)?)
}

/// Desfaz a entrada `entry_id`.
///
/// Obedece à MESMA trava de produção de `write_value` — desfazer é uma
/// escrita como outra qualquer. O ambiente consultado é o da ENTRADA, não o
/// do host aberto na tela: quem desfaz uma mudança feita em produção está
/// mexendo em produção, independente de onde esteja olhando agora.
pub async fn rollback(state: &AppState, entry_id: i64, confirmed: bool) -> Result<WriteOutcome> {
    let entry = state.history.get(entry_id)?;

    if needs_confirmation(entry.env, confirmed) {
        return Ok(WriteOutcome::NeedsConfirmation {
            summary: format!("Desfazer em {}: {}", entry.key, entry.summary),
        });
    }

    let (host, pw) = state.host_and_password(&entry.host_id)?;
    state
        .history
        .rollback(&state.pool, &host, pw.as_deref(), entry_id)
        .await?;
    state
        .index
        .forget_key(&entry.host_id, entry.db, &entry.key)?;

    Ok(WriteOutcome::Applied { entry_id })
}

// ------------------------------------------------------------------ busca

pub async fn search(
    state: &AppState,
    host_id: &str,
    db: u8,
    needle: &str,
) -> Result<SearchOutcome> {
    let (host, pw) = state.host_and_password(host_id)?;
    Ok(search_index(
        &state.index,
        &state.pool,
        &host,
        pw.as_deref(),
        db,
        needle,
        SEARCH_LIMIT,
    )
    .await?)
}

// ------------------------------------------------- escrita pedida pela LLM

/// Os pedidos de escrita à espera de uma decisão do dono.
pub fn pending_approvals(state: &AppState) -> Vec<ApprovalRequest> {
    state.approvals.pending()
}

/// Libera uma escrita pedida pela LLM. `typed_host_name` tem que ser
/// exatamente o nome do host; qualquer outra coisa devolve erro e o pedido
/// continua esperando.
pub fn resolve_approval(
    state: &AppState,
    request_id: &str,
    typed_host_name: &str,
) -> Result<ApprovalVerdict> {
    Ok(state.approvals.resolve(request_id, typed_host_name)?)
}

/// Recusa uma escrita pedida pela LLM.
pub fn deny_approval(state: &AppState, request_id: &str) -> Result<ApprovalVerdict> {
    Ok(state.approvals.deny(request_id)?)
}

/// A regra da trava, num lugar só, para `write_value` e `rollback` não
/// poderem divergir.
fn needs_confirmation(env: Env, confirmed: bool) -> bool {
    env == Env::Prod && !confirmed
}

// ===================================================================
// Os comandos propriamente ditos: casca fina sobre as funções acima.
// ===================================================================

#[tauri::command]
pub fn cmd_list_hosts(state: tauri::State<'_, AppState>) -> Result<Vec<Host>> {
    list_hosts(&state)
}

#[tauri::command]
pub fn cmd_add_host(
    state: tauri::State<'_, AppState>,
    host: Host,
    password: Option<String>,
) -> Result<String> {
    add_host(&state, host, password)
}

#[tauri::command]
pub fn cmd_update_host(
    state: tauri::State<'_, AppState>,
    host: Host,
    password: Option<String>,
) -> Result<()> {
    update_host(&state, host, password)
}

#[tauri::command]
pub fn cmd_remove_host(state: tauri::State<'_, AppState>, host_id: String) -> Result<()> {
    remove_host(&state, &host_id)
}

#[tauri::command]
pub async fn cmd_test_host(state: tauri::State<'_, AppState>, host_id: String) -> Result<u64> {
    test_host(&state, &host_id).await
}

#[tauri::command]
pub async fn cmd_list_dbs(
    state: tauri::State<'_, AppState>,
    host_id: String,
) -> Result<Vec<(u8, u64)>> {
    list_dbs(&state, &host_id).await
}

#[tauri::command]
pub async fn cmd_load_tree(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
) -> Result<Vec<TreeNode>> {
    load_tree(&state, &host_id, db).await
}

#[tauri::command]
pub async fn cmd_rescan(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
) -> Result<ScanReport> {
    rescan(&state, &host_id, db, move |progress| {
        // Um evento perdido só atrasa a barra de progresso; nunca pode
        // derrubar a varredura que já está em andamento.
        let _ = app.emit(RESCAN_PROGRESS_EVENT, progress);
    })
    .await
}

#[tauri::command]
pub fn cmd_scan_status(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
) -> Result<Option<ScanReport>> {
    scan_status(&state, &host_id, db)
}

#[tauri::command]
pub async fn cmd_read_value(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    key: String,
) -> Result<ValueView> {
    read_value(&state, &host_id, db, &key).await
}

#[tauri::command]
pub async fn cmd_read_value_more(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    key: String,
    offset: usize,
) -> Result<ValueView> {
    read_value_more(&state, &host_id, db, &key, offset).await
}

#[tauri::command]
pub async fn cmd_write_value(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    key: String,
    op: WriteOp,
    confirmed: bool,
) -> Result<WriteOutcome> {
    write_value(&state, &host_id, db, &key, op, confirmed).await
}

#[tauri::command]
pub fn cmd_list_history(
    state: tauri::State<'_, AppState>,
    host_id: String,
    limit: usize,
) -> Result<Vec<HistoryEntry>> {
    list_history(&state, &host_id, limit)
}

#[tauri::command]
pub async fn cmd_rollback(
    state: tauri::State<'_, AppState>,
    entry_id: i64,
    confirmed: bool,
) -> Result<WriteOutcome> {
    rollback(&state, entry_id, confirmed).await
}

#[tauri::command]
pub async fn cmd_search(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    needle: String,
) -> Result<SearchOutcome> {
    search(&state, &host_id, db, &needle).await
}

#[tauri::command]
pub fn cmd_pending_approvals(state: tauri::State<'_, AppState>) -> Vec<ApprovalRequest> {
    pending_approvals(&state)
}

#[tauri::command]
pub fn cmd_resolve_approval(
    state: tauri::State<'_, AppState>,
    request_id: String,
    typed_host_name: String,
) -> Result<ApprovalVerdict> {
    resolve_approval(&state, &request_id, &typed_host_name)
}

#[tauri::command]
pub fn cmd_deny_approval(
    state: tauri::State<'_, AppState>,
    request_id: String,
) -> Result<ApprovalVerdict> {
    deny_approval(&state, &request_id)
}

/// Evento avisando que a fila de pedidos mudou.
pub const APPROVALS_EVENT: &str = "approvals://changed";

/// Identificador do app, o mesmo de `tauri.conf.json`.
const APP_IDENTIFIER: &str = "com.sugamele.redisdesk";

/// Variável de ambiente que reposiciona o diretório de dados. Existe para
/// rodar uma segunda instalação lado a lado (e para os testes não
/// encostarem nos dados de verdade de quem está desenvolvendo).
const DATA_DIR_ENV: &str = "REDIS_DESK_DATA_DIR";

/// O diretório onde moram `hosts.json` e o SQLite.
///
/// Calculado aqui, e não pedido ao Tauri, porque o processo do servidor MCP
/// (`--mcp`) não tem um `AppHandle` para perguntar — e os dois processos
/// PRECISAM chegar ao mesmo caminho, senão o Claude leria um cadastro de
/// hosts diferente do que a janela mostra.
pub fn data_dir() -> std::io::Result<PathBuf> {
    if let Some(custom) = std::env::var_os(DATA_DIR_ENV) {
        return Ok(PathBuf::from(custom));
    }

    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::other("HOME não está definida no ambiente"))?;
    let base = if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support")
    } else {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(home).join(".local/share"))
    };

    Ok(base.join(APP_IDENTIFIER))
}

/// Monta o `AppState`, entrega ao Tauri e abre o socket de aprovação.
pub fn setup(app: &tauri::App) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let dir = data_dir()?;
    std::fs::create_dir_all(&dir)?;

    let state = AppState::open(&dir)?;
    let queue = state.approvals.clone();
    app.manage(state);

    // O socket só existe enquanto a janela estiver aberta. É essa a
    // propriedade que faz "sem aplicativo aberto, a LLM não escreve" valer
    // por construção, e não por disciplina de quem escreveu o código.
    let socket = socket_path()?;
    tauri::async_runtime::spawn(async move {
        if let Err(err) = serve(queue, &socket).await {
            // stderr, nunca stdout: no modo MCP stdout é o protocolo, e um
            // hábito de imprimir ali vaza para o lugar errado.
            eprintln!("redis-desk: o socket de aprovação caiu: {err}");
        }
    });

    vigiar_pedidos(app.handle().clone());
    Ok(())
}

/// Acorda a janela quando um pedido novo chega.
///
/// Pedido que ninguém vê é pedido que expira, e uma expiração silenciosa
/// parece, do lado do Claude, a mesma coisa que uma recusa. Por isso a
/// janela se levanta sozinha: a decisão é da pessoa, mas a chance de decidir
/// não pode depender de ela estar olhando para a tela certa.
fn vigiar_pedidos(app: tauri::AppHandle) {
    use tauri::{Emitter, Manager};

    tauri::async_runtime::spawn(async move {
        let mut anterior = 0usize;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            let Some(state) = app.try_state::<AppState>() else {
                continue;
            };
            let atual = state.approvals.pending().len();
            if atual == anterior {
                continue;
            }

            let chegou_pedido = atual > anterior;
            anterior = atual;
            let _ = app.emit(APPROVALS_EVENT, atual);

            if let Some(janela) = app.get_webview_window("main").filter(|_| chegou_pedido) {
                let _ = janela.unminimize();
                let _ = janela.set_focus();
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Env, Host};
    use crate::core::value::{Bytes, WriteOp};
    use redis::aio::MultiplexedConnection;

    /// Banco só destes testes (0–9 já são usados pelos testes do núcleo).
    const TEST_DB: u8 = 10;

    fn host_spec(env: Env) -> Host {
        Host {
            // `add_host` gera o id; o que vier aqui é descartado.
            id: String::new(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            db: TEST_DB,
            username: None,
            tls: false,
            env,
        }
    }

    fn test_state() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(dir.path()).unwrap();
        (dir, state)
    }

    fn text_bytes(text: &str) -> Bytes {
        Bytes {
            text: Some(text.to_string()),
            base64: None,
            truncated: false,
        }
    }

    /// Conexão crua, por fora da fachada: é ela que diz o que REALMENTE está
    /// no Redis, independente do que a fachada afirme ter feito.
    async fn raw_conn() -> MultiplexedConnection {
        redis::Client::open(format!("redis://127.0.0.1:6399/{TEST_DB}"))
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap()
    }

    async fn get_string(conn: &mut MultiplexedConnection, key: &str) -> Option<String> {
        redis::cmd("GET").arg(key).query_async(conn).await.unwrap()
    }

    async fn set_string(conn: &mut MultiplexedConnection, key: &str, value: &str) {
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .query_async(conn)
            .await
            .unwrap();
    }

    async fn del(conn: &mut MultiplexedConnection, key: &str) {
        let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
    }

    #[tokio::test]
    async fn prod_write_without_confirmation_is_refused() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;
        let key = "commands_rs_test:prod_sem_confirmar";

        set_string(&mut conn, key, "antes").await;
        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();

        let outcome = write_value(
            &state,
            &host_id,
            TEST_DB,
            key,
            WriteOp::SetString {
                value: text_bytes("depois"),
            },
            false,
        )
        .await
        .unwrap();

        let WriteOutcome::NeedsConfirmation { summary } = outcome else {
            panic!("escrita em produção sem confirmar precisava ser recusada, veio {outcome:?}")
        };
        assert!(
            summary.contains(key),
            "o resumo é o que a pessoa lê antes de autorizar: precisa dizer qual chave muda, \
             veio {summary:?}"
        );
        assert_eq!(
            get_string(&mut conn, key).await.as_deref(),
            Some("antes"),
            "recusar a escrita e ainda assim escrever seria o pior dos dois mundos"
        );

        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn prod_write_with_confirmation_goes_through() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;
        let key = "commands_rs_test:prod_confirmado";

        set_string(&mut conn, key, "antes").await;
        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();

        let outcome = write_value(
            &state,
            &host_id,
            TEST_DB,
            key,
            WriteOp::SetString {
                value: text_bytes("depois"),
            },
            true,
        )
        .await
        .unwrap();

        assert!(
            matches!(outcome, WriteOutcome::Applied { .. }),
            "com confirmação, a escrita em produção precisa passar, veio {outcome:?}"
        );
        assert_eq!(get_string(&mut conn, key).await.as_deref(), Some("depois"));

        // E a escrita passou pelo histórico, não por um caminho paralelo.
        let entries = list_history(&state, &host_id, 10).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].key, key);

        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn dev_write_needs_no_confirmation() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;
        let key = "commands_rs_test:dev_direto";

        set_string(&mut conn, key, "antes").await;
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        let outcome = write_value(
            &state,
            &host_id,
            TEST_DB,
            key,
            WriteOp::SetString {
                value: text_bytes("depois"),
            },
            false,
        )
        .await
        .unwrap();

        assert!(
            matches!(outcome, WriteOutcome::Applied { .. }),
            "fora de produção não há diálogo de confirmar, veio {outcome:?}"
        );
        assert_eq!(get_string(&mut conn, key).await.as_deref(), Some("depois"));

        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn prod_rollback_follows_the_same_rule() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;
        let key = "commands_rs_test:prod_desfazer";

        set_string(&mut conn, key, "antes").await;
        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();

        let applied = write_value(
            &state,
            &host_id,
            TEST_DB,
            key,
            WriteOp::SetString {
                value: text_bytes("depois"),
            },
            true,
        )
        .await
        .unwrap();
        let WriteOutcome::Applied { entry_id } = applied else {
            panic!("a escrita de partida precisava ter passado, veio {applied:?}")
        };

        // Desfazer é uma escrita como outra qualquer: em produção, sem
        // confirmar, não acontece.
        let outcome = rollback(&state, entry_id, false).await.unwrap();
        let WriteOutcome::NeedsConfirmation { summary } = outcome else {
            panic!("desfazer em produção sem confirmar precisava ser recusado, veio {outcome:?}")
        };
        assert!(
            summary.contains(key),
            "o resumo do desfazer precisa dizer qual chave volta, veio {summary:?}"
        );
        assert_eq!(
            get_string(&mut conn, key).await.as_deref(),
            Some("depois"),
            "o desfazer recusado não podia ter mexido no valor"
        );

        let outcome = rollback(&state, entry_id, true).await.unwrap();
        assert!(
            matches!(outcome, WriteOutcome::Applied { .. }),
            "com confirmação, o desfazer precisa passar, veio {outcome:?}"
        );
        assert_eq!(
            get_string(&mut conn, key).await.as_deref(),
            Some("antes"),
            "o desfazer confirmado precisa devolver o valor anterior"
        );

        del(&mut conn, key).await;
    }
}
