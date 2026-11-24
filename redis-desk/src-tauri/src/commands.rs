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

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use crate::approval::{
    ApprovalError, ApprovalQueue, ApprovalRequest, ApprovalVerdict, serve, socket_path,
};
use crate::core::bulk::enumerate as enumerate_keys;
use crate::core::config::{ConfigError, Env, Host, HostStore};
use crate::core::conn::{ConnError, Pool};
use crate::core::history::{
    Actor, BackupChoice, BulkDelete, FailedKey, History, HistoryEntry, HistoryError,
    RollbackOutcome,
};
use crate::core::index::{Index, IndexError, ScanProgress, ScanReport};
use crate::core::keys::{
    Alvo, KeyKind, KeyTrie, Level, MAX_CHILDREN, ScanSnapshot, ScanTick, expand_branch, scan_trie,
};
use crate::core::log::{LogEntry, LogKind, LogRing, SLOW_MS, now_ms};
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
const DB_FILE: &str = "suga-redis.sqlite";

/// Evento emitido durante `rescan`, um por chave processada, para a barra
/// de progresso da interface.
pub const RESCAN_PROGRESS_EVENT: &str = "rescan://progress";

/// Retratos da varredura da árvore, enquanto ela corre.
pub const TREE_PROGRESS_EVENT: &str = "tree://progress";

/// Um retrato de varredura ENDEREÇADO ao (host, banco) que o produziu.
///
/// O evento do Tauri é global: quem escuta recebe tudo, de qualquer
/// varredura. Sem o endereço, desconectar o host A no meio da varredura
/// dele e abrir o host B em seguida fazia os retratos de A — que continuam
/// chegando, porque a varredura segue correndo sobre o `Arc` da trie dela —
/// repovoarem a árvore de B. Num produto cujo dispositivo de segurança
/// central é nunca confundir um host de produção com um de
/// desenvolvimento, esse é o tipo errado de erro.
///
/// `flatten` porque o retrato continua chegando achatado na tela, do mesmo
/// jeito que ela já lia: o endereço é um acréscimo, não um envelope novo.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TreeProgress {
    pub host_id: String,
    pub db: u8,
    #[serde(flatten)]
    pub snapshot: ScanSnapshot,
}

/// Andamento da varredura de um galho só (`load_more_children`).
pub const BRANCH_PROGRESS_EVENT: &str = "branch://progress";

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

    #[error("a contagem \"{0}\" não está mais guardada — refaça a enumeração antes de apagar")]
    UnknownEnumeration(String),

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
            CommandError::Conn(ConnError::ClusterSingleDb { .. }) => "cluster_single_db",
            CommandError::Conn(ConnError::KeyGone(_)) => "key_gone",
            CommandError::Conn(ConnError::NotEditable { .. }) => "not_editable",
            CommandError::Conn(ConnError::Ambiguous { .. }) => "ambiguous",
            CommandError::Conn(ConnError::Protocol(_)) => "protocol",
            CommandError::Config(_) => "config",
            CommandError::Index(_) => "index",
            CommandError::History(_) => "history",
            CommandError::UnknownHost(_) => "unknown_host",
            CommandError::UnknownEnumeration(_) => "unknown_enumeration",
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
    /// A volta atrás de uma exclusão em massa: quantas chaves voltaram e
    /// quais não voltaram. `failed` vazio é o desfeito inteiro; qualquer
    /// coisa nele é um desfeito pela metade, e a tela precisa dizer isso —
    /// por isso a contagem e os nomes atravessam a fronteira, em vez de um
    /// "pronto" que esconderia o que ficou para trás.
    Restored {
        restored: u64,
        failed: Vec<FailedKey>,
    },
    /// Uma exclusão em massa que aconteceu: quantas chaves saíram de
    /// VERDADE e quantas a contagem tinha prometido.
    ///
    /// Os dois números, e não só um: enquanto não havia como desistir eles
    /// eram praticamente sempre o mesmo, e o que a tela mostrava era o da
    /// contagem. Com `cancel_delete` eles divergem exatamente no caso em
    /// que a diferença importa — "desistiu: 500 de 600" —, e a tela não
    /// pode ter que reabrir a entrada do histórico para saber disso.
    Deleted {
        entry_id: i64,
        deleted: u64,
        enumerated: u64,
    },
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
    /// A árvore de prefixos de cada (host, banco) aberto.
    ///
    /// É o que permite abrir um galho sem voltar ao servidor. Sem ela, cada
    /// clique custaria uma varredura do banco inteiro, porque o `MATCH` do
    /// `SCAN` não encurta o trabalho do Redis — só filtra o que volta.
    pub trees: Mutex<TreeCache>,
    /// As contagens de prefixo já feitas e ainda não confirmadas, por id.
    ///
    /// É aqui que a lista de nomes mora entre `enumerate_prefix` e
    /// `delete_prefix`: ela NÃO atravessa a fronteira do Tauri — a tela
    /// recebe um resumo e um id, nunca 100.000 strings — e é ela, e não o
    /// que o prefixo guardar no instante do clique, que `delete_prefix`
    /// apaga.
    pub enumerations: Mutex<HashMap<String, StoredEnumeration>>,
    /// A bandeira de desistência de cada exclusão em massa EM ANDAMENTO,
    /// pelo id da contagem que a originou.
    ///
    /// Separada de `enumerations` porque as duas têm vidas diferentes:
    /// `delete_prefix` tira a contagem do mapa assim que começa a apagar, e
    /// é justamente a partir daí que existe algo de que desistir. A entrada
    /// nasce quando o `UNLINK` vai começar e morre quando ele acaba — de
    /// qualquer jeito, inclusive por erro —, então `cancel_delete` devolver
    /// `false` quer dizer "não há exclusão correndo sob esse id", nunca
    /// "não deu certo".
    pub deletions: Mutex<HashMap<String, Arc<AtomicBool>>>,
    /// O diretório de dados do app, guardado porque `delete_prefix` precisa
    /// dele para gravar a cópia de segurança em arquivo. Pedir a
    /// `data_dir()` de dentro do comando faria os testes escreverem no
    /// diretório de dados de verdade de quem está desenvolvendo.
    pub dir: PathBuf,
    /// As últimas conexões, falhas e operações lentas — ver `core::log`.
    ///
    /// O MESMO `Arc` é entregue ao `Pool` (ver `AppState::open`): o anel tem
    /// dois pontos de entrada por desenho, o pool sabe quando um socket abre
    /// ou morre, a fachada sabe o NOME da operação que estava rodando. Um
    /// anel por componente contaria só metade da história.
    pub logs: Arc<LogRing>,
}

/// As árvores de prefixo, por (host, banco).
///
/// Um `Arc<Mutex<_>>` por árvore, e não uma trava só para o mapa: a
/// varredura escreve numa árvore a cada lote enquanto a tela lê dela para
/// abrir galhos, e uma trava única faria uma esperar pela outra.
pub type TreeCache = HashMap<(String, u8), Arc<Mutex<KeyTrie>>>;

impl AppState {
    /// Abre (ou cria) o estado no diretório de dados do app: `hosts.json`
    /// pelo `HostStore`, e o SQLite único compartilhado entre histórico e
    /// índice.
    pub fn open(dir: &Path) -> Result<AppState> {
        let db_path: PathBuf = dir.join(DB_FILE);
        // Um `Arc` só, entregue ao `Pool` logo abaixo — ver o comentário de
        // `AppState::logs`.
        let logs = Arc::new(LogRing::new());
        Ok(AppState {
            hosts: Mutex::new(HostStore::open(dir)?),
            pool: Pool::new().with_log(logs.clone()),
            index: Index::open(&db_path)?,
            history: History::open(&db_path)?,
            approvals: Arc::new(ApprovalQueue::new()),
            trees: Mutex::new(TreeCache::new()),
            enumerations: Mutex::new(HashMap::new()),
            deletions: Mutex::new(HashMap::new()),
            dir: dir.to_path_buf(),
            logs,
        })
    }

    /// Só o host, sem tocar no chaveiro do sistema: para quem precisa do
    /// NOME dele (a linha de log) e não da senha. `None` quando o host já
    /// não está cadastrado — a operação pode seguir, e a linha de log cai
    /// no `host_id` cru, que ainda diz de quem se trata.
    pub(crate) fn host_by_id(&self, host_id: &str) -> Option<Host> {
        self.hosts
            .lock()
            .unwrap()
            .list()
            .into_iter()
            .find(|host| host.id == host_id)
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

/// Cronometra `fut` e registra o resultado no anel de log de `state`:
/// `Failure` (com o `Display` do erro já classificado) quando dá errado,
/// `Slow` quando dá certo mas passa de `SLOW_MS`, e nada quando dá certo
/// rápido — um comando saudável não deixa rastro.
///
/// `host` é o host JÁ RESOLVIDO quando existe; `None` quando nem isso deu
/// certo (ex.: `host_id` que não está mais cadastrado). `host_id` é o
/// identificador cru que todo chamador já tinha em mãos — usado como nome
/// de exibição SÓ nesse caso de `None`, para `LogEntry::host` continuar
/// identificando qual host falhou mesmo sem um `Host` resolvido, em vez de
/// ficar em branco bem no caso em que mais importa mostrar algo.
///
/// Todo comando que fala com o núcleo passa por aqui, cada um decidindo
/// sozinho, no `match` de `host_and_password`, se chama com `Some` ou
/// `None`.
///
/// NENHUMA operação é dispensada de deixar a linha de falha, nem mesmo
/// `connect`, cuja falha o `Pool` também registra quando um socket nasce.
/// Essa dispensa chegou a existir, para poupar a linha repetida, e abriu um
/// buraco: o pool só fala quando um socket NASCE (`Pool::connect`, chamado
/// por `get` apenas sem conexão em cache) ou quando `with_retry` reconecta
/// — e ele só reconecta em `ConnError::Unreachable`. `Auth`,
/// `ClusterSingleDb` e `Protocol` sobre um socket já aberto não produziam
/// linha nenhuma, de lado nenhum. Silêncio é o defeito que este trabalho
/// inteiro veio eliminar; uma linha repetida é só uma linha repetida, e a
/// aba de Logs não depende mais do conteúdo da entrada para desenhá-la.
async fn registrar<T>(
    state: &AppState,
    operation: &str,
    host_id: &str,
    host: Option<&Host>,
    db: Option<u8>,
    fut: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let start = Instant::now();
    let result = fut.await;
    let duration_ms = start.elapsed().as_millis() as u64;
    let host_name = host
        .map(|h| h.name.clone())
        .unwrap_or_else(|| host_id.to_string());

    match &result {
        Err(err) => state.logs.push(LogEntry {
            at_ms: now_ms(),
            kind: LogKind::Failure,
            host: host_name,
            db,
            operation: operation.to_string(),
            duration_ms: Some(duration_ms),
            detail: Some(err.to_string()),
        }),
        Ok(_) if duration_ms >= SLOW_MS => state.logs.push(LogEntry {
            at_ms: now_ms(),
            kind: LogKind::Slow,
            host: host_name,
            db,
            operation: operation.to_string(),
            duration_ms: Some(duration_ms),
            detail: None,
        }),
        Ok(_) => {}
    }

    result
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

/// Conecta de verdade ao host: abre (ou reaproveita) o socket do `Pool` e
/// mede o tempo de ida e volta de um `PING`.
///
/// Reusa `pool.ping` — a mesma chamada de `test_host` — porque abrir a
/// conexão JÁ É o teste: não há um "conectar de mentira" que valha a pena
/// fazer antes. O que muda é o momento: antes desta função, o botão
/// "Conectar" só montava a área de trabalho, e a conexão nascia escondida
/// dentro da primeira varredura — ninguém via o host falhar antes de pedir
/// as chaves.
pub async fn connect(state: &AppState, host_id: &str) -> Result<u64> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            // A fachada registra a falha SEMPRE, mesmo quando o `Pool`
            // também registra. Quando o socket nasce aqui, as duas linhas
            // contam o mesmo evento — e é um preço baixo: o pool fica mudo
            // sempre que o socket já estava em cache, e só reconecta (e
            // portanto só volta a falar) em `ConnError::Unreachable`. Um
            // host que recusa a senha ou que é cluster com banco único,
            // sobre um socket já aberto, não teria quem o registrasse. Ver
            // a nota em `registrar`.
            registrar(state, "connect", host_id, Some(&host), None, async {
                let elapsed = state.pool.ping(&host, pw.as_deref()).await?;
                Ok(elapsed.as_millis() as u64)
            })
            .await
        }
        Err(err) => registrar(state, "connect", host_id, None, None, async { Err(err) }).await,
    }
}

/// Desconecta de verdade do host: derruba o socket cacheado no `Pool` e
/// esquece a árvore de prefixos de TODOS os bancos dele.
///
/// A segunda parte não é opcional: sem ela, `tree_level` continuaria
/// respondendo do cache em memória depois que o socket já caiu, e a tela
/// mostraria as chaves de um host que a pessoa já tinha deixado.
pub fn disconnect(state: &AppState, host_id: &str) -> Result<()> {
    let (host, _pw) = state.host_and_password(host_id)?;
    state.pool.drop_host(&host);
    state
        .trees
        .lock()
        .unwrap()
        .retain(|(id, _db), _tree| id != host_id);
    Ok(())
}

/// Testa a conexão com o host, devolvendo o tempo de resposta em
/// milissegundos.
pub async fn test_host(state: &AppState, host_id: &str) -> Result<u64> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(state, "test_host", host_id, Some(&host), None, async {
                let elapsed = state.pool.ping(&host, pw.as_deref()).await?;
                Ok(elapsed.as_millis() as u64)
            })
            .await
        }
        Err(err) => registrar(state, "test_host", host_id, None, None, async { Err(err) }).await,
    }
}

/// Os bancos do host que têm ao menos uma chave, com a contagem de cada um.
pub async fn list_dbs(state: &AppState, host_id: &str) -> Result<Vec<(u8, u64)>> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(state, "list_dbs", host_id, Some(&host), None, async {
                Ok(state.pool.db_sizes(&host, pw.as_deref()).await?)
            })
            .await
        }
        Err(err) => registrar(state, "list_dbs", host_id, None, None, async { Err(err) }).await,
    }
}

// ----------------------------------------------------------------- chaves

/// Varre o (host, banco) UMA vez e guarda a árvore de prefixos.
///
/// A árvore entra no cache ANTES de a varredura começar e é preenchida
/// enquanto ela corre: é isso que deixa a pessoa abrir um galho no meio do
/// caminho, com o que já chegou, em vez de esperar o banco inteiro.
///
/// `snapshot` é chamado ao longo da varredura com a raiz parcial, para a
/// tela mostrar os prefixos e as contagens crescendo.
pub async fn scan_tree(
    state: &AppState,
    host_id: &str,
    db: u8,
    snapshot: impl Fn(ScanSnapshot),
) -> Result<()> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(state, "scan_tree", host_id, Some(&host), Some(db), async {
                let trie = Arc::new(Mutex::new(KeyTrie::new()));
                state
                    .trees
                    .lock()
                    .unwrap()
                    .insert((host_id.to_string(), db), trie.clone());

                scan_trie(&state.pool, &host, pw.as_deref(), db, &trie, snapshot).await?;
                Ok(())
            })
            .await
        }
        Err(err) => {
            registrar(state, "scan_tree", host_id, None, Some(db), async {
                Err(err)
            })
            .await
        }
    }
}

/// Carrega mais filhos de um galho cuja lista saiu incompleta.
///
/// Varre de novo só aquele prefixo e acrescenta outra leva à lista. É a
/// resposta para "o nível tem milhões de nomes": em vez de esconder o que
/// não coube, a pessoa pede mais e recebe mais, com as contagens certas.
///
/// Cara de propósito — o servidor percorre o espaço de chaves inteiro
/// mesmo com `MATCH` —, por isso só acontece quando pedida.
pub async fn load_more_children(
    state: &AppState,
    host_id: &str,
    db: u8,
    prefix: &str,
    progress: impl Fn(crate::core::keys::ScanTick),
) -> Result<Level> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(
                state,
                "load_more_children",
                host_id,
                Some(&host),
                Some(db),
                async {
                    let trie = state
                        .trees
                        .lock()
                        .unwrap()
                        .get(&(host_id.to_string(), db))
                        .cloned()
                        .ok_or_else(|| CommandError::UnknownHost(host_id.to_string()))?;

                    // Quantos já estão à vista, mais uma leva.
                    let atual = trie
                        .lock()
                        .unwrap()
                        .level(prefix)
                        .map(|n| n.children.len())
                        .unwrap_or(0);

                    expand_branch(
                        &state.pool,
                        Alvo {
                            host: &host,
                            pw: pw.as_deref(),
                            db,
                        },
                        &trie,
                        prefix,
                        atual + MAX_CHILDREN,
                        progress,
                    )
                    .await?;

                    let nivel = trie.lock().unwrap().level(prefix);
                    nivel.ok_or_else(|| CommandError::UnknownHost(prefix.to_string()))
                },
            )
            .await
        }
        Err(err) => {
            registrar(
                state,
                "load_more_children",
                host_id,
                None,
                Some(db),
                async { Err(err) },
            )
            .await
        }
    }
}

/// Os filhos de um prefixo, lidos da árvore em memória.
///
/// `None` quando este (host, banco) ainda não começou a ser varrido, ou
/// quando a varredura ainda não chegou neste prefixo. Nunca vai ao
/// servidor: é essa consulta em memória que torna abrir um galho imediato.
pub fn tree_level(state: &AppState, host_id: &str, db: u8, prefix: &str) -> Option<Level> {
    // A trava do mapa é solta antes da trava da árvore, para a varredura —
    // que escreve na árvore a cada lote — não ficar presa atrás desta
    // leitura.
    let trie = state
        .trees
        .lock()
        .unwrap()
        .get(&(host_id.to_string(), db))
        .cloned()?;
    trie.lock().unwrap().level(prefix)
}

/// Tira a chave da árvore em memória daquele (host, banco).
///
/// A árvore na tela é desenhada da trie, não do servidor: uma chave que
/// deixou de existir precisa sair daqui, senão continua listada até alguém
/// desconectar — que é o que joga a trie fora.
///
/// Silencioso de propósito. Se não há trie aberta para esse par, ou a
/// chave nunca entrou nela (teto de filhos, teto de nós), não há nada a
/// consertar — e nada disso é erro que a pessoa possa resolver.
fn forget_key_in_tree(state: &AppState, host_id: &str, db: u8, key: &str) {
    forget_keys_in_tree(state, host_id, db, [key]);
}

/// A mesma coisa para uma lista — e a primitiva das duas.
///
/// Pega a trava do mapa e a da trie UMA vez para a lista inteira: uma
/// exclusão em massa carrega dezenas de milhares de nomes, e tomar as duas
/// travas por chave faria o laço disputá-las, nome a nome, com a varredura
/// que escreve na mesma trie a cada lote.
fn forget_keys_in_tree<'a>(
    state: &AppState,
    host_id: &str,
    db: u8,
    keys: impl IntoIterator<Item = &'a str>,
) {
    let trie = state
        .trees
        .lock()
        .unwrap()
        .get(&(host_id.to_string(), db))
        .cloned();
    if let Some(trie) = trie {
        let mut trie = trie.lock().unwrap();
        for key in keys {
            trie.remover(key);
        }
    }
}

/// O contrário de `forget_keys_in_tree`: repõe na árvore em memória as
/// chaves que voltaram ao Redis.
///
/// Sem isto, uma volta atrás de exclusão em massa deixa o banco com as
/// chaves de novo e a tela sem nenhuma delas até alguém desconectar — que é
/// o que joga a trie fora. Silencioso pela mesma razão da irmã: não há trie
/// aberta para esse par quando ninguém abriu a árvore, e isso não é erro.
fn remember_keys_in_tree<'a>(
    state: &AppState,
    host_id: &str,
    db: u8,
    keys: impl IntoIterator<Item = &'a str>,
) {
    let trie = state
        .trees
        .lock()
        .unwrap()
        .get(&(host_id.to_string(), db))
        .cloned();
    if let Some(trie) = trie {
        let mut trie = trie.lock().unwrap();
        for key in keys {
            trie.inserir(key);
        }
    }
}

/// Refaz o índice de busca do (host, banco). `progress` é chamado a cada
/// chave para a barra de progresso.
pub async fn rescan(
    state: &AppState,
    host_id: &str,
    db: u8,
    progress: impl Fn(ScanProgress),
) -> Result<ScanReport> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(state, "rescan", host_id, Some(&host), Some(db), async {
                Ok(state
                    .index
                    .rescan(&state.pool, &host, pw.as_deref(), db, progress)
                    .await?)
            })
            .await
        }
        Err(err) => registrar(state, "rescan", host_id, None, Some(db), async { Err(err) }).await,
    }
}

/// De quando é o índice deste (host, banco) — `None` se nunca foi varrido.
///
/// Passa pelo anel como todo o resto: era um dos dois `catch` mudos que o
/// desenho nomeou por linha, e consertar só o lado da tela deixava a falha
/// visível no momento e perdida logo depois. `async` só por isso — a
/// pergunta em si é um `SELECT` local.
///
/// Resolve o host sem tocar no chaveiro: aqui não há senha a usar, e
/// exigi-la faria um host recém-removido do cadastro passar a falhar numa
/// pergunta que o índice ainda sabe responder.
pub async fn scan_status(state: &AppState, host_id: &str, db: u8) -> Result<Option<ScanReport>> {
    let host = state.host_by_id(host_id);
    registrar(
        state,
        "scan_status",
        host_id,
        host.as_ref(),
        Some(db),
        async { Ok(state.index.last_scan(host_id, db)?) },
    )
    .await
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
    // Nome próprio para a busca de continuação: sem isso, a linha do log
    // não distingue a primeira página de uma página seguinte — e são
    // custos e causas diferentes.
    let operation = if offset == 0 {
        "read_value"
    } else {
        "read_value_more"
    };

    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(state, operation, host_id, Some(&host), Some(db), async {
                let limit = ValueLimit {
                    offset,
                    max_bytes: PAGE_BYTES,
                    max_elements: PAGE_ELEMENTS,
                };
                let lido =
                    read_value_from_redis(&state.pool, &host, pw.as_deref(), db, key, limit).await;

                // A chave sumiu entre a varredura e o clique — expirou por
                // TTL, quase sempre. Esta leitura é a primeira notícia que
                // temos disso, e a árvore em memória precisa saber: sem
                // isto a chave continua listada, e clicar nela só repete o
                // mesmo erro.
                if matches!(lido, Err(ConnError::KeyGone(_))) {
                    forget_key_in_tree(state, host_id, db, key);
                }

                Ok(lido?)
            })
            .await
        }
        Err(err) => {
            registrar(state, operation, host_id, None, Some(db), async {
                Err(err)
            })
            .await
        }
    }
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
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(
                state,
                "write_value",
                host_id,
                Some(&host),
                Some(db),
                async {
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

                    // O índice sabia desta chave com o valor ANTIGO. Apagar a
                    // linha é o conserto honesto e barato: a busca para de
                    // devolver um texto que não está mais lá, e a próxima
                    // varredura reindexa a chave com o valor novo. Reindexar só
                    // esta chave aqui exigiria relê-la do Redis — custo de uma
                    // ida e volta a mais em toda escrita, para adiantar o que a
                    // varredura faz de qualquer jeito.
                    state.index.forget_key(host_id, db, key)?;

                    // A árvore é cache igual ao índice, e `DeleteKey` é a
                    // única operação que faz a chave deixar de existir.
                    if matches!(op, WriteOp::DeleteKey) {
                        forget_key_in_tree(state, host_id, db, key);
                    }

                    Ok(WriteOutcome::Applied { entry_id })
                },
            )
            .await
        }
        Err(err) => {
            registrar(state, "write_value", host_id, None, Some(db), async {
                Err(err)
            })
            .await
        }
    }
}

// ------------------------------------------------------ exclusão em massa

/// Andamento da enumeração de um prefixo, enquanto ela corre.
pub const BULK_SCAN_PROGRESS_EVENT: &str = "bulk://scan-progress";

/// Andamento da exclusão em massa já confirmada: quantas chaves já saíram.
pub const BULK_DELETE_PROGRESS_EVENT: &str = "bulk://delete-progress";

/// Subdiretório do diretório de dados onde ficam as cópias de segurança
/// gravadas em arquivo (`BackupRequest::File`).
const BACKUP_DIR: &str = "backups";

/// O resumo de uma enumeração — e só o resumo.
///
/// A lista de nomes fica em `AppState` sob `id` e não atravessa a fronteira
/// do Tauri: um prefixo no teto viraria 100.000 strings no processo da tela
/// para desenhar um número e uma tabela de tipos.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumerationHandle {
    pub id: String,
    pub count: u64,
    pub kinds: Vec<(KeyKind, u64)>,
    pub truncated: bool,
    /// Quantas são de tipo sem volta atrás (set, stream, RedisJSON).
    pub unrestorable: u64,
}

/// O que a pessoa escolheu no diálogo sobre a cópia de segurança.
///
/// É a forma de `BackupChoice` que atravessa a fronteira, e a variante de
/// arquivo não carrega caminho nenhum de propósito: quem decide ONDE o
/// arquivo mora é a fachada, não a tela.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupRequest {
    Inline,
    File,
    None,
}

/// Uma enumeração à espera de confirmação: o alvo e os nomes achados.
pub struct StoredEnumeration {
    pub host_id: String,
    pub db: u8,
    pub prefixes: Vec<String>,
    pub keys: Vec<String>,
}

/// A frase que a pessoa lê antes de digitar o nome do host: quantas chaves
/// saem, de onde e de qual host — os três dados de que a decisão depende.
fn bulk_summary(host: &Host, db: u8, prefixes: &[String], count: u64) -> String {
    let alvo = match prefixes {
        [] => "o banco inteiro".to_string(),
        [unico] => format!("\"{unico}\""),
        [primeiro, resto @ ..] => format!("\"{primeiro}\" e mais {}", resto.len()),
    };
    format!(
        "Apagar {count} chaves sob {alvo} no host {} (banco {db})",
        host.name
    )
}

/// Conta e classifica toda chave sob `prefixes`, guarda a lista sob um id e
/// devolve só o resumo.
///
/// Enumerar e apagar são DUAS chamadas de propósito: é o que permite mostrar
/// uma contagem conferida antes de perguntar, e é o que faz valer a promessa
/// de `delete_prefix` — o que sai é a lista enumerada, não o que o prefixo
/// guardar no instante do clique.
pub async fn enumerate_prefix(
    state: &AppState,
    host_id: &str,
    db: u8,
    prefixes: Vec<String>,
    progress: impl Fn(ScanTick),
) -> Result<EnumerationHandle> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(
                state,
                "enumerate_prefix",
                host_id,
                Some(&host),
                Some(db),
                async {
                    let alvo = Alvo {
                        host: &host,
                        pw: pw.as_deref(),
                        db,
                    };
                    let achado = enumerate_keys(&state.pool, alvo, &prefixes, progress).await?;

                    // `editable` é o mesmo predicado que decide se o editor
                    // de valor aceita o tipo, e é o mesmo conjunto que
                    // `history::build_restore_command` sabe repor: um tipo
                    // que o editor não escreve é um tipo que a volta atrás
                    // não traz de volta.
                    let unrestorable = achado
                        .kinds
                        .iter()
                        .filter(|(kind, _)| !kind.editable())
                        .map(|(_, quantas)| *quantas)
                        .sum();

                    let handle = EnumerationHandle {
                        id: uuid::Uuid::new_v4().to_string(),
                        count: achado.keys.len() as u64,
                        kinds: achado.kinds,
                        truncated: achado.truncated,
                        unrestorable,
                    };

                    let mut guardadas = state.enumerations.lock().unwrap();
                    // Uma contagem viva por (host, banco). A tela confirma
                    // um diálogo de cada vez, e a contagem abandonada da
                    // anterior é peso morto segurando até 100.000 nomes.
                    guardadas
                        .retain(|_, guardada| guardada.host_id != host_id || guardada.db != db);
                    guardadas.insert(
                        handle.id.clone(),
                        StoredEnumeration {
                            host_id: host_id.to_string(),
                            db,
                            prefixes,
                            keys: achado.keys,
                        },
                    );

                    Ok(handle)
                },
            )
            .await
        }
        Err(err) => {
            registrar(state, "enumerate_prefix", host_id, None, Some(db), async {
                Err(err)
            })
            .await
        }
    }
}

/// Apaga a lista enumerada sob `enumeration_id`, com cópia de segurança e
/// UMA entrada de histórico — a irmã em massa de `write_value`.
///
/// A trava de produção mora aqui, como a de `write_value`, e em host `prod`
/// ela pede mais que um clique: `typed_host_name` tem que bater EXATAMENTE
/// com `host.name`, sem aparar espaços e sem ignorar maiúsculas — a mesma
/// regra que `approval::ApprovalQueue::resolve` aplica à escrita pedida pela
/// LLM, pela mesma razão: o custo de errar não cabe num clique. Fora de
/// produção o nome digitado é ignorado.
///
/// A enumeração só sai do mapa quando a exclusão de fato começa: uma recusa
/// por falta de nome deixa o id valendo, para a pessoa digitar e tentar de
/// novo sem pagar outra varredura — e sem que a segunda tentativa apague uma
/// lista diferente da que ela acabou de ler na tela.
pub async fn delete_prefix(
    state: &AppState,
    enumeration_id: &str,
    backup: BackupRequest,
    typed_host_name: Option<&str>,
    progress: impl Fn(u64),
) -> Result<WriteOutcome> {
    // Só o endereço e o tamanho, sem clonar a lista: a trava precisa do host
    // e a frase do diálogo precisa da contagem.
    let alvo = {
        let guardadas = state.enumerations.lock().unwrap();
        guardadas.get(enumeration_id).map(|guardada| {
            (
                guardada.host_id.clone(),
                guardada.db,
                guardada.prefixes.clone(),
                guardada.keys.len() as u64,
            )
        })
    };

    let Some((host_id, db, prefixes, count)) = alvo else {
        // Sem enumeração não há host a nomear; a linha de log fica no id que
        // a tela tinha em mãos, como `rollback` faz com a entrada sumida.
        let nome = format!("contagem {enumeration_id}");
        return registrar(state, "delete_prefix", &nome, None, None, async {
            Err(CommandError::UnknownEnumeration(enumeration_id.to_string()))
        })
        .await;
    };

    match state.host_and_password(&host_id) {
        Ok((host, pw)) => {
            registrar(
                state,
                "delete_prefix",
                &host_id,
                Some(&host),
                Some(db),
                async {
                    // A trava de produção, e nela a exigência a mais desta
                    // operação: em `prod` o nome digitado tem que bater
                    // EXATAMENTE com o do host. Fora de produção
                    // `needs_confirmation` ignora o que vier aqui, como
                    // ignora o `confirmed` de `write_value`.
                    let confirmed = typed_host_name == Some(host.name.as_str());
                    if needs_confirmation(host.env, confirmed) {
                        return Ok(WriteOutcome::NeedsConfirmation {
                            summary: bulk_summary(&host, db, &prefixes, count),
                        });
                    }

                    // Daqui em diante a contagem sai do mapa para sempre,
                    // inclusive se a exclusão falhar no meio: uma lista
                    // parcialmente apagada já não descreve o que está no
                    // banco, e oferecê-la de novo seria oferecer um número
                    // que deixou de ser verdade.
                    let Some(guardada) = state.enumerations.lock().unwrap().remove(enumeration_id)
                    else {
                        return Err(CommandError::UnknownEnumeration(enumeration_id.to_string()));
                    };

                    let destino = state.dir.join(BACKUP_DIR);
                    let escolha = match backup {
                        BackupRequest::Inline => BackupChoice::Inline,
                        BackupRequest::None => BackupChoice::None,
                        BackupRequest::File => {
                            std::fs::create_dir_all(&destino).map_err(HistoryError::Io)?;
                            BackupChoice::File(&destino)
                        }
                    };

                    // A bandeira entra no mapa ANTES da primeira ida ao
                    // servidor e sai dele por todos os caminhos — é ela que
                    // `cancel_delete` acha, e um id que ficasse para trás
                    // aqui seria um botão "desistir" apontando para uma
                    // exclusão que já acabou.
                    let cancel = Arc::new(AtomicBool::new(false));
                    state
                        .deletions
                        .lock()
                        .unwrap()
                        .insert(enumeration_id.to_string(), cancel.clone());

                    let resultado = state
                        .history
                        .delete_prefix_with_backup(
                            &state.pool,
                            &host,
                            pw.as_deref(),
                            db,
                            BulkDelete {
                                prefixes: &guardada.prefixes,
                                keys: &guardada.keys,
                                backup: escolha,
                                cancel: &cancel,
                            },
                            progress,
                        )
                        .await;

                    state.deletions.lock().unwrap().remove(enumeration_id);
                    // Erro aqui sobe sem mexer em cache nenhum: o parcial diz
                    // QUANTAS saíram, nunca QUAIS — ao contrário da
                    // desistência, em que os lotes pararam numa fronteira
                    // conhecida. Ver o comentário mais abaixo.
                    let feito = resultado?;

                    // Os dois caches precisam esquecer, pela mesma razão
                    // que `write_value` já os faz esquecer num `DeleteKey`:
                    // a árvore é desenhada da trie e a busca por valor é
                    // lida do índice, e nenhum dos dois volta ao servidor
                    // para conferir. Sem isto a tela mostra contagens de
                    // coisas que não existem e a busca devolve texto que já
                    // não está lá.
                    //
                    // Correu até o fim: esquece a lista INTEIRA, não só as
                    // que o servidor disse ter apagado. A diferença entre as
                    // duas são chaves que sumiram sozinhas entre a contagem e
                    // o `UNLINK` — e essas também não existem mais.
                    //
                    // Se houve desistência, não: as que sobraram continuam no
                    // banco, e tirá-las da árvore as esconderia da tela, que é
                    // indistinguível de tê-las apagado. Aí só esquece as
                    // `deleted` PRIMEIRAS, que é o único subconjunto
                    // demonstrável — os lotes correm na ordem da lista, então
                    // toda chave dentro desse corte ou saiu agora ou já não
                    // existia. O resto espera a próxima varredura: uma chave
                    // a mais na árvore se conserta sozinha no primeiro clique
                    // (`read_value` tira a que sumiu), uma a menos, não.
                    let apagadas: &[String] = if cancel.load(Ordering::Relaxed) {
                        // `min` e não fé: o corte vem de um número que o
                        // servidor mandou, e um índice fora da lista aqui
                        // derrubaria a fachada no fim de uma exclusão que
                        // deu certo.
                        let corte = (feito.deleted as usize).min(guardada.keys.len());
                        &guardada.keys[..corte]
                    } else {
                        &guardada.keys
                    };
                    forget_keys_in_tree(state, &host_id, db, apagadas.iter().map(String::as_str));
                    state.index.forget_keys(&host_id, db, apagadas)?;

                    Ok(WriteOutcome::Deleted {
                        entry_id: feito.entry_id,
                        deleted: feito.deleted,
                        enumerated: guardada.keys.len() as u64,
                    })
                },
            )
            .await
        }
        Err(err) => {
            registrar(state, "delete_prefix", &host_id, None, Some(db), async {
                Err(err)
            })
            .await
        }
    }
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
    // Sem a entrada não há host a nomear, e esta leitura também pode
    // falhar: a linha fica no nome do alvo que a pessoa tinha em mãos.
    // Desfazer é uma ESCRITA contra um banco possivelmente de produção —
    // nenhum pedaço dela pode falhar em silêncio.
    let entry = match state.history.get(entry_id) {
        Ok(entry) => entry,
        Err(err) => {
            let alvo = format!("histórico #{entry_id}");
            return registrar(state, "rollback", &alvo, None, None, async {
                Err(err.into())
            })
            .await;
        }
    };

    let host_id = entry.host_id.clone();
    let db = entry.db;

    match state.host_and_password(&host_id) {
        Ok((host, pw)) => {
            registrar(state, "rollback", &host_id, Some(&host), Some(db), async {
                // A trava de produção fica DENTRO da cronometragem, como em
                // `write_value`: recusar por falta de confirmação não é
                // falha, é resposta — e sai daqui como `Ok`, sem linha.
                if needs_confirmation(entry.env, confirmed) {
                    return Ok(WriteOutcome::NeedsConfirmation {
                        summary: format!("Desfazer em {}: {}", entry.key, entry.summary),
                    });
                }

                let desfeito = state
                    .history
                    .rollback(&state.pool, &host, pw.as_deref(), entry_id)
                    .await?;

                Ok(match desfeito {
                    RollbackOutcome::Key => {
                        // Mesma razão de `write_value`: o índice sabia desta
                        // chave com o valor que acabou de ser desfeito.
                        state.index.forget_key(&host_id, entry.db, &entry.key)?;
                        WriteOutcome::Applied { entry_id }
                    }
                    RollbackOutcome::Bulk { restored, failed } => {
                        // As chaves voltaram ao Redis; a árvore em memória é
                        // cache e precisa saber disso. Sem esta linha o
                        // diálogo diz "1.395 restauradas" e a árvore fica
                        // vazia até alguém desconectar — que é o que joga a
                        // trie fora.
                        //
                        // O índice de busca por VALOR, não: repor uma linha
                        // nele exigiria reler cada chave do Redis, e a
                        // política deste produto para toda escrita (ver
                        // `write_value`) é deixar isso para a próxima
                        // varredura. Até lá as chaves repostas aparecem na
                        // árvore e na busca por NOME, que é ao vivo, mas não
                        // na busca por conteúdo.
                        remember_keys_in_tree(
                            state,
                            &host_id,
                            entry.db,
                            restored.iter().map(String::as_str),
                        );
                        WriteOutcome::Restored {
                            restored: restored.len() as u64,
                            failed,
                        }
                    }
                })
            })
            .await
        }
        Err(err) => {
            registrar(state, "rollback", &host_id, None, Some(db), async {
                Err(err)
            })
            .await
        }
    }
}

// ------------------------------------------------------------------ busca

pub async fn search(
    state: &AppState,
    host_id: &str,
    db: u8,
    needle: &str,
) -> Result<SearchOutcome> {
    match state.host_and_password(host_id) {
        Ok((host, pw)) => {
            registrar(state, "search", host_id, Some(&host), Some(db), async {
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
            })
            .await
        }
        Err(err) => registrar(state, "search", host_id, None, Some(db), async { Err(err) }).await,
    }
}

// -------------------------------------------------------------------- log

/// As últimas conexões, falhas e operações lentas — mais recente primeiro.
/// É o que deixa a tela distinguir "o banco está vazio" de "a conexão
/// falhou", em vez de mostrar a mesma lista vazia para os dois casos.
pub fn logs(state: &AppState) -> Vec<LogEntry> {
    state.logs.snapshot()
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

/// Manda a exclusão em massa de `enumeration_id` parar.
///
/// Devolve se havia o que parar: `false` quer dizer que a exclusão já tinha
/// terminado quando o clique chegou, e isso não é erro nenhum — é a corrida
/// normal entre um botão e um laço que acaba.
pub fn cancel_delete(state: &AppState, enumeration_id: &str) -> bool {
    match state.deletions.lock().unwrap().get(enumeration_id) {
        Some(bandeira) => {
            // `unlink_em_lotes` confere a bandeira ENTRE lotes, nunca
            // dentro de um: o lote corrente sempre termina. É de propósito —
            // parar no meio de um `UNLINK` deixaria a exclusão pela metade
            // dentro do próprio lote, e o número registrado deixaria de
            // descrever o que aconteceu.
            bandeira.store(true, Ordering::Relaxed);
            true
        }
        None => false,
    }
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
pub async fn cmd_connect(state: tauri::State<'_, AppState>, host_id: String) -> Result<u64> {
    connect(&state, &host_id).await
}

#[tauri::command]
pub fn cmd_disconnect(state: tauri::State<'_, AppState>, host_id: String) -> Result<()> {
    disconnect(&state, &host_id)
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
pub async fn cmd_scan_tree(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
) -> Result<()> {
    let alvo = host_id.clone();
    scan_tree(&state, &host_id, db, move |retrato| {
        // Um retrato perdido só atrasa o que a tela mostra; nunca pode
        // derrubar a varredura que já está em andamento.
        let _ = app.emit(
            TREE_PROGRESS_EVENT,
            TreeProgress {
                host_id: alvo.clone(),
                db,
                snapshot: retrato,
            },
        );
    })
    .await
}

#[tauri::command]
pub async fn cmd_load_more_children(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    prefix: String,
) -> Result<Level> {
    load_more_children(&state, &host_id, db, &prefix, move |tick| {
        let _ = app.emit(BRANCH_PROGRESS_EVENT, tick);
    })
    .await
}

#[tauri::command]
pub fn cmd_tree_level(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    prefix: String,
) -> Option<Level> {
    tree_level(&state, &host_id, db, &prefix)
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
pub async fn cmd_scan_status(
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
) -> Result<Option<ScanReport>> {
    scan_status(&state, &host_id, db).await
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

/// `prefixes` chega da tela como lista porque o menu de contexto da árvore
/// deixa marcar mais de um galho; a varredura é UMA só para todos eles (ver
/// `core::bulk::enumerate`).
#[tauri::command]
pub async fn cmd_enumerate_prefix(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    host_id: String,
    db: u8,
    prefixes: Vec<String>,
) -> Result<EnumerationHandle> {
    enumerate_prefix(&state, &host_id, db, prefixes, move |tick| {
        // Um retrato perdido só atrasa a barra; nunca pode derrubar a
        // contagem que já está em andamento.
        let _ = app.emit(BULK_SCAN_PROGRESS_EVENT, tick);
    })
    .await
}

#[tauri::command]
pub async fn cmd_delete_prefix(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppState>,
    enumeration_id: String,
    backup: BackupRequest,
    typed_host_name: Option<String>,
) -> Result<WriteOutcome> {
    delete_prefix(
        &state,
        &enumeration_id,
        backup,
        typed_host_name.as_deref(),
        move |apagadas| {
            let _ = app.emit(BULK_DELETE_PROGRESS_EVENT, apagadas);
        },
    )
    .await
}

/// Devolve se havia exclusão correndo sob esse id. `false` não é falha: a
/// exclusão acabou antes de o clique chegar.
#[tauri::command]
pub fn cmd_cancel_delete(state: tauri::State<'_, AppState>, enumeration_id: String) -> bool {
    cancel_delete(&state, &enumeration_id)
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
pub fn cmd_logs(state: tauri::State<'_, AppState>) -> Vec<LogEntry> {
    logs(&state)
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
const APP_IDENTIFIER: &str = "com.sugamele.sugaredis";

/// Variável de ambiente que reposiciona o diretório de dados. Existe para
/// rodar uma segunda instalação lado a lado (e para os testes não
/// encostarem nos dados de verdade de quem está desenvolvendo).
const DATA_DIR_ENV: &str = "SUGA_REDIS_DATA_DIR";

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
            eprintln!("suga-redis: o socket de aprovação caiu: {err}");
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

    /// Banco só dos testes de `connect`/`disconnect` (0–12 e 15 já estão
    /// tomados por outros testes).
    const CONNECT_TEST_DB: u8 = 13;

    /// Banco dos testes de exclusão em massa. É o mesmo 13 de `core::bulk`,
    /// e a suíte roda em paralelo: por isso todo teste daqui vive sob um
    /// prefixo só dele, nenhum enumera o banco inteiro e nenhum confere
    /// `DBSIZE` — o que prova que a exclusão não aconteceu é a própria
    /// chave continuar lá, não a contagem do banco, que é de todo mundo.
    const BULK_TEST_DB: u8 = 13;

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

    /// O mesmo host, numa porta escolhida — para o teste que põe um
    /// atravessador TCP entre a fachada e o Redis de teste.
    fn host_spec_on_port(env: Env, port: u16) -> Host {
        Host {
            port,
            ..host_spec(env)
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
        raw_conn_db(TEST_DB).await
    }

    /// A mesma coisa, num banco escolhido — os testes de exclusão em massa
    /// vivem no 13, não no 10.
    async fn raw_conn_db(db: u8) -> MultiplexedConnection {
        redis::Client::open(format!("redis://127.0.0.1:6399/{db}"))
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
    async fn connect_answers_with_the_ping_time() {
        let (_dir, state) = test_state();
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        let elapsed_ms = connect(&state, &host_id).await.unwrap();

        // `u64` nunca é negativo, mas um PING local que levasse segundos
        // seria sinal de que isto não está medindo o que diz medir.
        assert!(
            elapsed_ms < 5_000,
            "um PING num Redis local não devia levar {elapsed_ms}ms"
        );

        let logged = logs(&state)
            .into_iter()
            .any(|entry| entry.kind == LogKind::Connection && entry.operation == "connect");
        assert!(
            logged,
            "conectar precisa deixar uma entrada de conexão no anel de log"
        );
    }

    #[tokio::test]
    async fn disconnect_forgets_the_tree_of_every_database_of_that_host() {
        let (_dir, state) = test_state();
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        scan_tree(&state, &host_id, CONNECT_TEST_DB, |_| {})
            .await
            .unwrap();
        assert!(
            tree_level(&state, &host_id, CONNECT_TEST_DB, "").is_some(),
            "a varredura precisava ter deixado a árvore em cache antes do teste valer"
        );

        disconnect(&state, &host_id).unwrap();

        assert!(
            tree_level(&state, &host_id, CONNECT_TEST_DB, "").is_none(),
            "desconectar precisa esquecer a árvore cacheada — senão a tela continuaria \
             mostrando as chaves de um host que a pessoa já deixou"
        );
    }

    #[tokio::test]
    async fn disconnecting_a_host_that_was_never_connected_is_not_an_error() {
        let (_dir, state) = test_state();
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        disconnect(&state, &host_id)
            .expect("desconectar um host que nunca chegou a conectar não pode falhar");
    }

    #[tokio::test]
    async fn a_arvore_pode_ser_explorada_enquanto_a_varredura_corre() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;
        let chaves = [
            "andamento_rs_test:api:itens:1",
            "andamento_rs_test:api:lotes:1",
        ];
        for chave in chaves {
            set_string(&mut conn, chave, "x").await;
        }
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        // Durante a varredura (retrato com `done: false`), abrir um galho
        // já precisa responder — é a promessa inteira deste desenho: ver e
        // explorar sem esperar o banco acabar de ser lido.
        let durante: std::sync::Mutex<Option<crate::core::keys::Level>> =
            std::sync::Mutex::new(None);
        scan_tree(&state, &host_id, TEST_DB, |retrato| {
            if !retrato.done && durante.lock().unwrap().is_none() {
                *durante.lock().unwrap() = tree_level(&state, &host_id, TEST_DB, "");
            }
        })
        .await
        .unwrap();

        for chave in chaves {
            del(&mut conn, chave).await;
        }

        let raiz = durante
            .into_inner()
            .unwrap()
            .expect("o nível já tinha que responder no meio da varredura");
        assert!(
            raiz.children.iter().any(|b| b.label == "andamento_rs_test"),
            "veio {:?}",
            raiz.children.iter().map(|b| &b.label).collect::<Vec<_>>()
        );
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

    #[tokio::test]
    async fn a_failed_command_leaves_a_line_in_the_log() {
        let (_dir, state) = test_state();

        list_dbs(&state, "host-que-nao-existe")
            .await
            .expect_err("host inexistente precisa falhar");

        let falha = logs(&state)
            .into_iter()
            .find(|e| e.kind == LogKind::Failure && e.operation == "list_dbs")
            .expect("a falha de list_dbs precisava estar no anel");
        assert!(
            falha
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("não está cadastrado")),
            "o detail precisa ser a mensagem já classificada do erro, veio {:?}",
            falha.detail
        );
        assert_eq!(
            falha.host, "host-que-nao-existe",
            "sem um Host resolvido, o host_id precisa aparecer no lugar do nome — \
             em branco seria indistinguível de dado faltando na tela"
        );
    }

    #[tokio::test]
    async fn a_command_that_works_fast_leaves_no_line() {
        let (_dir, state) = test_state();
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        list_dbs(&state, &host_id).await.unwrap();

        let entries = logs(&state);
        assert!(
            entries
                .iter()
                .all(|e| e.kind != LogKind::Failure && e.kind != LogKind::Slow),
            "uma operação rápida e bem-sucedida não podia deixar Failure nem Slow, veio {entries:?}"
        );
    }

    #[test]
    fn the_log_a_command_hands_over_is_newest_first() {
        let (_dir, state) = test_state();

        tokio::runtime::Runtime::new().unwrap().block_on(async {
            list_dbs(&state, "primeiro-inexistente")
                .await
                .expect_err("precisa falhar");
            list_dbs(&state, "segundo-inexistente")
                .await
                .expect_err("precisa falhar");
        });

        let entries = logs(&state);
        assert!(
            entries[0]
                .detail
                .as_deref()
                .is_some_and(|d| d.contains("segundo-inexistente")),
            "a entrada mais na frente precisa ser a SEGUNDA falha, veio {:?}",
            entries[0].detail
        );
    }

    #[tokio::test]
    async fn a_failed_connect_always_leaves_a_failure_line() {
        let (_dir, state) = test_state();
        let mut spec = host_spec(Env::Dev);
        spec.port = 1; // ninguém atende aqui
        let host_id = add_host(&state, spec, None).unwrap();

        connect(&state, &host_id)
            .await
            .expect_err("a porta 1 não podia aceitar conexão");

        // A linha é da FACHADA, e tem de existir independentemente de o
        // `Pool` ter registrado a dele. O pool só fala quando um socket
        // nasce ou quando `with_retry` reconecta — e ele só reconecta em
        // `ConnError::Unreachable`. Dispensar a fachada porque "o pool já
        // registra" deixava `Auth`, `ClusterSingleDb` e `Protocol` sobre um
        // socket em cache sem linha nenhuma, de lado nenhum. O clique em
        // Conectar num host já conectado passa exatamente por aí.
        let falha = logs(&state)
            .into_iter()
            .find(|entry| entry.kind == LogKind::Failure && entry.operation == "connect")
            .expect("uma conexão que falhou precisa deixar uma linha de falha");
        assert!(
            falha.detail.is_some_and(|d| !d.is_empty()),
            "a linha de falha precisa dizer por quê"
        );
    }

    #[tokio::test]
    async fn a_failed_rollback_leaves_a_line_in_the_log() {
        let (_dir, state) = test_state();

        rollback(&state, 999_999, false)
            .await
            .expect_err("não existe entrada 999999");

        let logged = logs(&state)
            .into_iter()
            .any(|entry| entry.kind == LogKind::Failure && entry.operation == "rollback");
        assert!(
            logged,
            "desfazer é uma ESCRITA contra um banco possivelmente de produção: falhar não pode ser mudo"
        );
    }

    #[tokio::test]
    async fn a_rollback_against_a_vanished_host_is_logged_with_the_host() {
        let (_dir, state) = test_state();
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        let key = "commands:rollback:log";
        let mut conn = raw_conn().await;
        del(&mut conn, key).await;

        let outcome = write_value(
            &state,
            &host_id,
            TEST_DB,
            key,
            WriteOp::SetString {
                value: text_bytes("antes"),
            },
            false,
        )
        .await
        .unwrap();
        let WriteOutcome::Applied { entry_id } = outcome else {
            panic!("a escrita em dev não devia pedir confirmação");
        };

        // O host sai do cadastro; a entrada de histórico continua lá,
        // apontando para ele.
        remove_host(&state, &host_id).unwrap();

        rollback(&state, entry_id, false)
            .await
            .expect_err("o host já não existe");

        let entry = logs(&state)
            .into_iter()
            .find(|entry| entry.operation == "rollback")
            .expect("a falha precisava estar no anel");
        assert_eq!(entry.kind, LogKind::Failure);
        assert_eq!(
            entry.host, host_id,
            "sem o Host resolvido, a linha ainda precisa dizer de qual host se trata"
        );
        assert_eq!(entry.db, Some(TEST_DB));

        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn paging_a_value_is_logged_under_its_own_name() {
        let (_dir, state) = test_state();

        read_value(&state, "host-que-nao-existe", TEST_DB, "k")
            .await
            .expect_err("host inexistente");
        read_value_more(&state, "host-que-nao-existe", TEST_DB, "k", 50)
            .await
            .expect_err("host inexistente");

        let nomes: Vec<String> = logs(&state)
            .into_iter()
            .filter(|entry| entry.kind == LogKind::Failure)
            .map(|entry| entry.operation)
            .collect();
        // Sem nomes distintos, a linha do log não distingue a primeira
        // leitura de uma busca de continuação — custos e causas diferentes.
        assert!(
            nomes.contains(&"read_value".to_string())
                && nomes.contains(&"read_value_more".to_string()),
            "esperava as duas operações nomeadas separadamente, veio {nomes:?}"
        );
    }

    #[tokio::test]
    async fn asking_for_the_index_report_passes_through_the_log() {
        let (_dir, state) = test_state();
        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();

        // Um relatório de índice que responde rápido e sem erro não deixa
        // rastro — é a mesma regra de todo comando saudável. O que esta
        // passagem garante é que, quando ele FALHAR, a falha tem por onde
        // sair: era um dos dois `catch` mudos que o desenho nomeou.
        scan_status(&state, &host_id, TEST_DB).await.unwrap();

        let barulho = logs(&state)
            .into_iter()
            .any(|entry| entry.operation == "scan_status");
        assert!(
            !barulho,
            "uma pergunta rápida e bem-sucedida não podia deixar linha nenhuma"
        );
    }

    #[test]
    fn a_tree_snapshot_says_which_host_and_database_it_came_from() {
        let payload = TreeProgress {
            host_id: "h1".to_string(),
            db: 7,
            snapshot: ScanSnapshot {
                scanned: 3,
                total_estimate: 10,
                root: Vec::new(),
                done: false,
            },
        };

        let json = serde_json::to_value(&payload).unwrap();

        // Sem o endereçamento, um retrato de uma varredura que continua
        // correndo depois de um `disconnect` repovoa a árvore do PRÓXIMO
        // host aberto — num produto cujo dispositivo de segurança central é
        // nunca confundir produção com desenvolvimento.
        assert_eq!(json["host_id"], "h1");
        assert_eq!(json["db"], 7);
        // E o retrato continua achatado no mesmo nível, como a tela já lia.
        assert_eq!(json["scanned"], 3);
        assert_eq!(json["done"], false);
    }

    /// Apagar uma chave tem de tirá-la da ÁRVORE, não só do Redis.
    ///
    /// A árvore na tela sai da trie em memória. Sem isto, a chave apagada
    /// continuava listada até a pessoa desconectar e conectar de novo —
    /// que é o único caminho que jogava a trie fora.
    #[tokio::test]
    async fn deleting_a_key_takes_it_off_the_tree() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;

        let chave = "arvore_rs_test:apagada:alvo";
        let vizinha = "arvore_rs_test:apagada:fica";
        set_string(&mut conn, chave, "x").await;
        set_string(&mut conn, vizinha, "y").await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        scan_tree(&state, &host_id, TEST_DB, |_| {}).await.unwrap();

        let antes = tree_level(&state, &host_id, TEST_DB, "arvore_rs_test:apagada:").unwrap();
        assert_eq!(antes.children.len(), 2, "as duas estão na árvore antes");

        write_value(&state, &host_id, TEST_DB, chave, WriteOp::DeleteKey, false)
            .await
            .unwrap();

        let depois = tree_level(&state, &host_id, TEST_DB, "arvore_rs_test:apagada:").unwrap();
        let rotulos: Vec<&str> = depois.children.iter().map(|c| c.label.as_str()).collect();

        del(&mut conn, vizinha).await;

        assert_eq!(
            rotulos,
            vec!["fica"],
            "a apagada não pode continuar na árvore"
        );
    }

    /// Uma chave que expirou entre a varredura e o clique também sai da
    /// árvore: a leitura já descobriu que ela não existe mais.
    #[tokio::test]
    async fn reading_a_vanished_key_takes_it_off_the_tree() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn().await;

        let chave = "arvore_rs_test:expirada:alvo";
        set_string(&mut conn, chave, "x").await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        scan_tree(&state, &host_id, TEST_DB, |_| {}).await.unwrap();
        assert!(
            tree_level(&state, &host_id, TEST_DB, "arvore_rs_test:expirada:").is_some(),
            "a chave entrou na árvore"
        );

        // O que o TTL faria, só que na hora.
        del(&mut conn, chave).await;

        let erro = read_value(&state, &host_id, TEST_DB, chave).await;
        assert!(erro.is_err(), "a leitura precisa falhar com a chave sumida");

        assert!(
            tree_level(&state, &host_id, TEST_DB, "arvore_rs_test:expirada:").is_none(),
            "e o galho vazio não fica para trás"
        );
    }

    // ----------------------------------------------- exclusão em massa

    /// Semeia `quantas` chaves de texto sob `prefixo`, devolvendo os nomes.
    async fn seed_bulk(
        conn: &mut MultiplexedConnection,
        prefixo: &str,
        quantas: usize,
    ) -> Vec<String> {
        let mut nomes = Vec::with_capacity(quantas);
        for i in 0..quantas {
            let nome = format!("{prefixo}{i}");
            set_string(conn, &nome, "valor").await;
            nomes.push(nome);
        }
        nomes
    }

    async fn still_there(conn: &mut MultiplexedConnection, key: &str) -> bool {
        get_string(conn, key).await.is_some()
    }

    /// Se o índice de busca ainda conhece esta chave. Pergunta direto à
    /// tabela porque é exatamente ela que `Index::forget_keys` mexe, e
    /// perguntar pela busca não distinguiria "saiu do índice" de "a chave
    /// não existe mais no Redis".
    fn indexed(state: &AppState, host_id: &str, db: u8, key: &str) -> bool {
        state
            .index
            .with_conn(|conn| {
                conn.query_row(
                    "SELECT COUNT(*) FROM key_index WHERE host_id = ?1 AND db = ?2 AND key = ?3",
                    rusqlite::params![host_id, db as i64, key],
                    |row| row.get::<_, i64>(0),
                )
            })
            .unwrap()
            > 0
    }

    /// Enumera e devolve o id da contagem, falhando com uma frase útil se o
    /// número achado não for o esperado — todo teste daqui depende de a
    /// varredura ter visto exatamente as chaves que ele semeou.
    async fn enumerate_exactly(
        state: &AppState,
        host_id: &str,
        prefixo: &str,
        esperadas: u64,
    ) -> String {
        let handle = enumerate_prefix(
            state,
            host_id,
            BULK_TEST_DB,
            vec![prefixo.to_string()],
            |_| {},
        )
        .await
        .unwrap();
        assert_eq!(
            handle.count, esperadas,
            "a enumeração de {prefixo:?} precisava achar {esperadas} chaves"
        );
        assert!(!handle.truncated, "o teste não semeia perto do teto");
        handle.id
    }

    #[tokio::test]
    async fn prod_without_the_typed_name_deletes_nothing() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:sem_nome:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        let outcome = delete_prefix(&state, &id, BackupRequest::Inline, None, |_| {})
            .await
            .unwrap();

        let WriteOutcome::NeedsConfirmation { summary } = outcome else {
            panic!(
                "apagar em massa em produção sem o nome digitado precisava ser recusado, veio {outcome:?}"
            )
        };
        assert!(
            summary.contains(prefixo) && summary.contains('3'),
            "o resumo é o que a pessoa lê antes de digitar o nome: precisa dizer quantas chaves \
             saem e de onde, veio {summary:?}"
        );

        for chave in &chaves {
            assert!(
                still_there(&mut conn, chave).await,
                "recusar a exclusão e ainda assim apagar {chave} seria o pior dos dois mundos"
            );
            del(&mut conn, chave).await;
        }
    }

    #[tokio::test]
    async fn prod_with_the_wrong_name_deletes_nothing() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:nome_errado:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        // "redis de teste" não é "Redis de teste". A diferença de uma caixa
        // alta é a diferença entre ler o que vai acontecer e clicar no que
        // apareceu na frente.
        let outcome = delete_prefix(
            &state,
            &id,
            BackupRequest::Inline,
            Some("redis de teste"),
            |_| {},
        )
        .await
        .unwrap();

        assert!(
            matches!(outcome, WriteOutcome::NeedsConfirmation { .. }),
            "o nome quase certo não pode valer como confirmação, veio {outcome:?}"
        );

        for chave in &chaves {
            assert!(
                still_there(&mut conn, chave).await,
                "{chave} não podia ter saído"
            );
            del(&mut conn, chave).await;
        }
    }

    #[tokio::test]
    async fn prod_with_the_exact_name_deletes() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:nome_exato:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        // Uma recusa não pode consumir a contagem: a pessoa digita o nome e
        // tenta de novo com O MESMO id, sem pagar outra varredura e sem que
        // a segunda tentativa apague uma lista diferente da que ela leu.
        let recusado = delete_prefix(&state, &id, BackupRequest::Inline, None, |_| {})
            .await
            .unwrap();
        assert!(matches!(recusado, WriteOutcome::NeedsConfirmation { .. }));

        let outcome = delete_prefix(
            &state,
            &id,
            BackupRequest::Inline,
            Some("Redis de teste"),
            |_| {},
        )
        .await
        .unwrap();

        let WriteOutcome::Deleted {
            entry_id,
            deleted,
            enumerated,
        } = outcome
        else {
            panic!("com o nome exato a exclusão precisava passar, veio {outcome:?}")
        };
        assert_eq!(
            (deleted, enumerated),
            (3, 3),
            "sem desistência os dois números são o mesmo"
        );

        for chave in &chaves {
            assert!(
                !still_there(&mut conn, chave).await,
                "{chave} precisava ter saído"
            );
        }

        let entrada = state.history.get(entry_id).unwrap();
        assert_eq!(entrada.db, BULK_TEST_DB);
        assert!(
            entrada.summary.contains("3 de 3"),
            "a entrada única precisa dizer quantas de quantas saíram, veio {:?}",
            entrada.summary
        );
    }

    #[tokio::test]
    async fn dev_does_not_need_a_typed_name() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:dev:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        let outcome = delete_prefix(&state, &id, BackupRequest::Inline, None, |_| {})
            .await
            .unwrap();

        assert!(
            matches!(outcome, WriteOutcome::Deleted { .. }),
            "fora de produção o nome digitado é ignorado, veio {outcome:?}"
        );
        for chave in &chaves {
            assert!(
                !still_there(&mut conn, chave).await,
                "{chave} precisava ter saído"
            );
        }
    }

    #[tokio::test]
    async fn deleting_forgets_the_tree_and_the_index() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:caches:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        scan_tree(&state, &host_id, BULK_TEST_DB, |_| {})
            .await
            .unwrap();
        rescan(&state, &host_id, BULK_TEST_DB, |_| {})
            .await
            .unwrap();

        assert!(
            tree_level(&state, &host_id, BULK_TEST_DB, prefixo).is_some(),
            "as chaves precisavam estar na árvore antes de o teste valer"
        );
        assert!(
            indexed(&state, &host_id, BULK_TEST_DB, &chaves[0]),
            "e no índice de busca também"
        );

        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;
        delete_prefix(&state, &id, BackupRequest::Inline, None, |_| {})
            .await
            .unwrap();

        assert!(
            tree_level(&state, &host_id, BULK_TEST_DB, prefixo).is_none(),
            "a árvore é desenhada da trie: um galho que não existe mais não pode continuar lá"
        );
        for chave in &chaves {
            assert!(
                !indexed(&state, &host_id, BULK_TEST_DB, chave),
                "a busca não pode devolver o texto de {chave}, que não existe mais"
            );
        }
    }

    #[tokio::test]
    async fn a_key_created_after_enumeration_survives() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:depois:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        // Nasce ENTRE as duas chamadas. O que sai é a lista enumerada, não o
        // que o prefixo guardar no instante do clique: é isso que faz o
        // número mostrado no diálogo ser o número apagado.
        let recem_nascida = format!("{prefixo}c:9");
        set_string(&mut conn, &recem_nascida, "nova").await;

        delete_prefix(&state, &id, BackupRequest::Inline, None, |_| {})
            .await
            .unwrap();

        for chave in &chaves {
            assert!(
                !still_there(&mut conn, chave).await,
                "{chave} estava na lista e precisava sair"
            );
        }
        assert!(
            still_there(&mut conn, &recem_nascida).await,
            "a chave criada depois da contagem não foi prometida a ninguém e precisa continuar lá"
        );

        del(&mut conn, &recem_nascida).await;
    }

    #[tokio::test]
    async fn an_unknown_enumeration_id_is_an_error() {
        let (_dir, state) = test_state();

        let erro = delete_prefix(
            &state,
            "contagem-que-nunca-existiu",
            BackupRequest::None,
            None,
            |_| {},
        )
        .await
        .unwrap_err();

        assert_eq!(
            erro.kind(),
            "unknown_enumeration",
            "a tela precisa distinguir uma contagem vencida de uma falha de conexão, veio {erro:?}"
        );
        assert!(
            logs(&state)
                .into_iter()
                .any(|linha| linha.kind == LogKind::Failure && linha.operation == "delete_prefix"),
            "nenhum pedaço de uma exclusão em massa pode falhar em silêncio"
        );
    }

    /// Um atravessador TCP para o Redis de teste, que morre quando mandarem.
    ///
    /// É a única forma honesta de fazer o SEGUNDO lote de `UNLINK` falhar:
    /// derrubar o socket não basta (`Pool::with_retry` reconecta), e mexer
    /// no Redis de teste de verdade quebraria os outros testes, que rodam em
    /// paralelo contra o mesmo contêiner. Aqui o estrago cabe inteiro dentro
    /// deste teste.
    async fn redis_proxy(parar: tokio::sync::watch::Receiver<bool>) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let porta = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            loop {
                let mut aviso = parar.clone();
                let aceito = tokio::select! {
                    _ = aviso.wait_for(|morre| *morre) => break,
                    aceito = listener.accept() => aceito,
                };
                let Ok((mut cliente, _)) = aceito else { break };

                let mut aviso = parar.clone();
                tokio::spawn(async move {
                    let Ok(mut servidor) = tokio::net::TcpStream::connect("127.0.0.1:6399").await
                    else {
                        return;
                    };
                    tokio::select! {
                        _ = aviso.wait_for(|morre| *morre) => {}
                        _ = tokio::io::copy_bidirectional(&mut cliente, &mut servidor) => {}
                    }
                });
            }
        });

        porta
    }

    #[tokio::test]
    async fn a_dead_connection_still_records_what_went() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:conexao_morta:";

        // Mais que um lote de `UNLINK` (`core::bulk::UNLINK_BATCH`), para
        // haver um segundo lote a falhar.
        let quantas = crate::core::bulk::UNLINK_BATCH + 100;
        let chaves = seed_bulk(&mut conn, prefixo, quantas).await;

        let (morre, aviso) = tokio::sync::watch::channel(false);
        let porta = redis_proxy(aviso).await;

        let host_id = add_host(&state, host_spec_on_port(Env::Dev, porta), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, quantas as u64).await;

        // Sem cópia: o que este teste tem a provar é o parcial na linha, e
        // ler 600 retratos pelo atravessador só atrasaria isso.
        let erro = delete_prefix(&state, &id, BackupRequest::None, None, move |saidas| {
            // Primeiro lote inteiro no chão; o atravessador morre antes do
            // segundo.
            if saidas >= crate::core::bulk::UNLINK_BATCH as u64 {
                let _ = morre.send(true);
            }
        })
        .await
        .unwrap_err();

        assert!(
            matches!(erro, CommandError::History(_)),
            "a falha do segundo lote precisa subir, não virar sucesso, veio {erro:?}"
        );

        let entrada = state
            .history
            .list(&host_id, 10)
            .unwrap()
            .into_iter()
            .next()
            .expect("o que já saiu precisa estar registrado, mesmo com o erro subindo");
        assert!(
            entrada
                .summary
                .contains(&format!("{} de {quantas}", crate::core::bulk::UNLINK_BATCH)),
            "a entrada precisa dizer o número de VERDADE do que saiu, veio {:?}",
            entrada.summary
        );

        for chave in &chaves {
            del(&mut conn, chave).await;
        }
    }

    #[tokio::test]
    async fn rolling_a_prod_bulk_back_needs_confirmation() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:desfazer_prod:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;
        let outcome = delete_prefix(
            &state,
            &id,
            BackupRequest::Inline,
            Some("Redis de teste"),
            |_| {},
        )
        .await
        .unwrap();
        let WriteOutcome::Deleted { entry_id, .. } = outcome else {
            panic!("a exclusão de partida precisava ter passado, veio {outcome:?}")
        };

        // O ambiente consultado é o da ENTRADA, não o do host aberto na
        // tela: quem desfaz uma exclusão feita em produção mexe em produção.
        let recusado = rollback(&state, entry_id, false).await.unwrap();
        assert!(
            matches!(recusado, WriteOutcome::NeedsConfirmation { .. }),
            "desfazer em massa em produção sem confirmar precisava ser recusado, veio {recusado:?}"
        );
        for chave in &chaves {
            assert!(
                !still_there(&mut conn, chave).await,
                "o desfazer recusado não podia ter reposto {chave}"
            );
        }

        let feito = rollback(&state, entry_id, true).await.unwrap();
        let WriteOutcome::Restored { restored, failed } = feito else {
            panic!("desfazer uma exclusão em massa devolve a contagem reposta, veio {feito:?}")
        };
        assert_eq!(restored, 3);
        assert!(
            failed.is_empty(),
            "nenhuma devia ter ficado para trás: {failed:?}"
        );

        for chave in &chaves {
            assert!(
                still_there(&mut conn, chave).await,
                "{chave} precisava ter voltado"
            );
            del(&mut conn, chave).await;
        }
    }

    /// Quem decide ONDE a cópia em arquivo fica é a fachada, não a tela: o
    /// diálogo só diz "em arquivo", e o caminho sai do diretório de dados do
    /// app. Sem isto a tela escolheria um caminho, e um caminho vindo da
    /// tela é um caminho que pode apontar para qualquer lugar.
    #[tokio::test]
    async fn a_backup_in_a_file_lands_in_the_app_data_dir() {
        let (dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:copia_em_arquivo:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        let outcome = delete_prefix(&state, &id, BackupRequest::File, None, |_| {})
            .await
            .unwrap();
        let WriteOutcome::Deleted { entry_id, .. } = outcome else {
            panic!("a exclusão precisava ter passado, veio {outcome:?}")
        };

        let copias: Vec<_> = std::fs::read_dir(dir.path().join("backups"))
            .expect("o diretório de cópias precisa ter sido criado")
            .map(|entrada| entrada.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            copias.len(),
            1,
            "uma cópia, dentro do diretório de dados do app: {copias:?}"
        );
        assert!(
            copias[0].ends_with(".json"),
            "e com o nome montado pelo núcleo, veio {:?}",
            copias[0]
        );

        // A cópia num arquivo vale tanto quanto a embutida: é por ela que o
        // desfazer encontra os retratos.
        let feito = rollback(&state, entry_id, true).await.unwrap();
        let WriteOutcome::Restored { restored, failed } = feito else {
            panic!("desfazer devolve a contagem reposta, veio {feito:?}")
        };
        assert_eq!(restored, 3);
        assert!(failed.is_empty(), "{failed:?}");

        for chave in &chaves {
            del(&mut conn, chave).await;
        }
    }

    /// A volta atrás repõe as chaves no Redis — e na árvore em memória, que
    /// é de onde a tela desenha. Sem isto a pessoa lê "3 chaves restauradas"
    /// e olha para uma árvore vazia.
    #[tokio::test]
    async fn a_bulk_rollback_puts_the_keys_back_in_the_tree() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:desfazer_arvore:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        scan_tree(&state, &host_id, BULK_TEST_DB, |_| {})
            .await
            .unwrap();

        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;
        let outcome = delete_prefix(&state, &id, BackupRequest::Inline, None, |_| {})
            .await
            .unwrap();
        let WriteOutcome::Deleted { entry_id, .. } = outcome else {
            panic!("a exclusão de partida precisava ter passado, veio {outcome:?}")
        };
        assert!(
            tree_level(&state, &host_id, BULK_TEST_DB, prefixo).is_none(),
            "a exclusão precisava ter tirado o galho da árvore"
        );

        rollback(&state, entry_id, true).await.unwrap();

        let galho = tree_level(&state, &host_id, BULK_TEST_DB, prefixo)
            .expect("as chaves voltaram ao Redis: o galho precisa voltar à árvore também");
        assert_eq!(
            galho.children.len(),
            3,
            "e com as três chaves, não com um galho pela metade"
        );

        for chave in &chaves {
            del(&mut conn, chave).await;
        }
    }

    /// Todo nome de evento desta fachada usa a forma `assunto://coisa`.
    ///
    /// Está num teste porque a convenção é invisível no ponto de uso: quem
    /// acrescenta um evento copia a linha do `pub const` ao lado, e a
    /// primeira versão destes dois saiu como `bulk-scan-progress`, com
    /// hífen, sem nada reclamar. Um nome fora da forma só aparece do outro
    /// lado da fronteira, na tela, quando já é tarde para trocá-lo.
    #[test]
    fn every_event_name_follows_the_house_convention() {
        for nome in [
            TREE_PROGRESS_EVENT,
            BRANCH_PROGRESS_EVENT,
            RESCAN_PROGRESS_EVENT,
            APPROVALS_EVENT,
            BULK_SCAN_PROGRESS_EVENT,
            BULK_DELETE_PROGRESS_EVENT,
        ] {
            let (assunto, coisa) = nome
                .split_once("://")
                .unwrap_or_else(|| panic!("o evento {nome:?} não está na forma assunto://coisa"));
            assert!(
                !assunto.is_empty() && !coisa.is_empty(),
                "o evento {nome:?} tem um dos dois lados vazio"
            );
        }
    }

    /// Desistir para DEPOIS do lote corrente, nunca no meio dele — e o que
    /// já saiu continua saído, com a tela sabendo o número de verdade.
    #[tokio::test]
    async fn cancelling_stops_after_the_current_batch() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:desistir:";

        let lote = crate::core::bulk::UNLINK_BATCH;
        let quantas = lote + 100;
        let chaves = seed_bulk(&mut conn, prefixo, quantas).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, quantas as u64).await;

        // O clique em "desistir" chega quando o primeiro lote acabou de cair.
        let alvo = id.clone();
        let outcome = delete_prefix(&state, &id, BackupRequest::None, None, |saidas| {
            if saidas >= lote as u64 {
                cancel_delete(&state, &alvo);
            }
        })
        .await
        .unwrap();

        let WriteOutcome::Deleted {
            entry_id,
            deleted,
            enumerated,
        } = outcome
        else {
            panic!(
                "desistir não é falhar: a exclusão parcial precisa voltar como Deleted, veio {outcome:?}"
            )
        };
        assert_eq!(
            (deleted, enumerated),
            (lote as u64, quantas as u64),
            "é exatamente aqui que os dois números divergem, e é por isso que os dois atravessam \
             a fronteira"
        );

        // Quais chaves sobraram é a ordem do `SCAN`, não a da semeadura;
        // o que o teste tem a provar é QUANTAS, e que a conta fecha com o
        // número que a tela recebeu.
        let mut sobraram = 0;
        for chave in &chaves {
            if still_there(&mut conn, chave).await {
                sobraram += 1;
            }
            del(&mut conn, chave).await;
        }
        assert_eq!(
            sobraram,
            quantas - lote,
            "o banco precisa concordar com o número que voltou pela fronteira"
        );

        let entrada = state.history.get(entry_id).unwrap();
        assert!(
            entrada.summary.contains(&format!("{lote} de {quantas}")),
            "a entrada única registra o parcial, não o pedido, veio {:?}",
            entrada.summary
        );
    }

    /// Uma chave poupada pela desistência continua na árvore.
    ///
    /// É a metade perigosa do conserto de cache: tirar a lista inteira da
    /// trie depois de uma desistência esconderia chaves que continuam no
    /// banco — e some-da-tela é exatamente como "foi apagada" se parece.
    #[tokio::test]
    async fn cancelling_does_not_hide_the_keys_it_spared() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:desistir_arvore:";

        let lote = crate::core::bulk::UNLINK_BATCH;
        let quantas = lote + 100;
        let chaves = seed_bulk(&mut conn, prefixo, quantas).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        scan_tree(&state, &host_id, BULK_TEST_DB, |_| {})
            .await
            .unwrap();

        let id = enumerate_exactly(&state, &host_id, prefixo, quantas as u64).await;
        let alvo = id.clone();
        delete_prefix(&state, &id, BackupRequest::None, None, |saidas| {
            if saidas >= lote as u64 {
                cancel_delete(&state, &alvo);
            }
        })
        .await
        .unwrap();

        let galho = tree_level(&state, &host_id, BULK_TEST_DB, prefixo)
            .expect("as 100 poupadas continuam no banco: o galho tem que continuar na árvore");
        assert_eq!(
            galho.children.len(),
            quantas - lote,
            "a árvore precisa mostrar exatamente as que sobraram"
        );

        for chave in &chaves {
            del(&mut conn, chave).await;
        }
    }

    /// Desistir de uma exclusão que já acabou não é erro — é a corrida
    /// normal entre um botão e um laço que termina.
    #[tokio::test]
    async fn cancelling_a_delete_that_already_ended_is_not_an_error() {
        let (_dir, state) = test_state();
        let mut conn = raw_conn_db(BULK_TEST_DB).await;
        let prefixo = "commands_rs_bulk:desistir_tarde:";
        let chaves = seed_bulk(&mut conn, prefixo, 3).await;

        let host_id = add_host(&state, host_spec(Env::Dev), None).unwrap();
        let id = enumerate_exactly(&state, &host_id, prefixo, 3).await;

        assert!(
            !cancel_delete(&state, &id),
            "antes de a exclusão começar não há o que parar"
        );

        delete_prefix(&state, &id, BackupRequest::None, None, |_| {})
            .await
            .unwrap();

        assert!(
            !cancel_delete(&state, &id),
            "e depois que ela acabou, também não"
        );

        for chave in &chaves {
            assert!(
                !still_there(&mut conn, chave).await,
                "a desistência tardia não podia ter poupado {chave}"
            );
        }
    }
}
