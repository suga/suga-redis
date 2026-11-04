//! As ferramentas que o Claude local enxerga.
//!
//! Todas são de LEITURA. Escrita pela LLM é a Task 16, e passa por um
//! caminho completamente diferente: um pedido ao app aberto, que o dono
//! libera na mão. Aqui não existe nenhum caminho que grave no Redis.
//!
//! As descrições das ferramentas estão em português porque é o texto que o
//! Claude lê para decidir qual chamar — é documentação de uso, não comentário
//! de código.
//!
//! O valor vai para a LLM EM CLARO, sem máscara. É decisão registrada do
//! dono (ver as Global Constraints do plano), não um descuido.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ErrorData, Json, ServerHandler, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::approval::{ApprovalRequest, ApprovalVerdict, request_approval};
use crate::commands::{AppState, CommandError};
use crate::core::config::Env;
use crate::core::history::Actor;
use crate::core::keys::{KeyMeta, scan_all};
use crate::core::search::{SearchHit, search};
use crate::core::value::{Bytes, ValueLimit, ValueView, WriteOp, op_summary, read_value};

/// Teto padrão de acertos devolvidos por `search_keys`.
const LIMITE_BUSCA_PADRAO: usize = 50;
/// Teto absoluto, mesmo que a LLM peça mais.
const LIMITE_BUSCA_MAXIMO: usize = 500;

/// A partir de quanto tempo o índice é considerado velho demais para
/// responder uma busca sem varrer antes. Cinco minutos: curto o bastante
/// para a LLM não raciocinar em cima de um retrato de ontem, longo o
/// bastante para várias perguntas seguidas não varrerem o banco a cada uma.
const INDICE_MAX_IDADE_SEGUNDOS: i64 = 300;

/// Teto de bytes por elemento numa leitura de valor pela LLM.
const LEITURA_MAX_BYTES: usize = 256 * 1024;
/// Teto de elementos por leitura.
const LEITURA_MAX_ELEMENTOS: usize = 2_000;

/// Traduz um erro do núcleo para o erro do protocolo, preservando a frase em
/// português — é ela que o Claude vai ler e repetir para a pessoa.
fn erro(err: CommandError) -> ErrorData {
    ErrorData::internal_error(err.to_string(), None)
}

// ------------------------------------------------------- o que entra

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HostArgs {
    /// O id do host, como devolvido por `list_hosts`.
    pub host_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SearchKeysArgs {
    pub host_id: String,
    /// O número do banco (0 a 15).
    pub db: u8,
    /// O texto procurado. Casa como pedaço, em qualquer posição.
    pub needle: String,
    /// Quantos acertos no máximo. Padrão 50, teto 500.
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReadKeyArgs {
    pub host_id: String,
    pub db: u8,
    /// O nome completo da chave.
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct PrefixArgs {
    pub host_id: String,
    pub db: u8,
    /// Prefixo do nome da chave. String vazia conta o banco inteiro.
    pub prefix: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SummarizeArgs {
    pub host_id: String,
    pub db: u8,
    /// Quantos níveis de `:` agrupar. 1 agrupa por `sessao:`, 2 por
    /// `sessao:ativa:`, e assim por diante.
    pub depth: usize,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct WriteKeyArgs {
    pub host_id: String,
    pub db: u8,
    pub key: String,
    /// O texto a gravar. Só vale para chaves do tipo `string`.
    pub value: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteKeyArgs {
    pub host_id: String,
    pub db: u8,
    pub key: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetTtlArgs {
    pub host_id: String,
    pub db: u8,
    pub key: String,
    /// Prazo em milissegundos, ou nulo para a chave deixar de expirar.
    pub ttl_ms: Option<i64>,
}

// ------------------------------------------------------- o que sai

/// Um host cadastrado, SEM senha. A senha mora no chaveiro do sistema e não
/// tem representação aqui — nem para ser omitida.
#[derive(Debug, Serialize, JsonSchema)]
pub struct HostInfo {
    pub id: String,
    pub name: String,
    pub env: String,
    pub addr: String,
    pub port: u16,
    pub default_db: u8,
    pub tls: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct DatabaseInfo {
    pub db: u8,
    pub keys: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
    /// Quantas chaves do índice foram conferidas para montar esta resposta.
    pub scanned: u64,
    pub elapsed_ms: u64,
    /// Chaves que o índice conhecia e que já não existem mais no servidor.
    pub stale_keys_removed: u64,
    /// Idade do índice em segundos; -1 se nunca foi varrido.
    pub index_age_secs: i64,
    /// Esta chamada varreu o banco antes de responder, porque o índice
    /// estava velho ou não existia.
    pub scanned_first: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct CountResult {
    pub prefix: String,
    pub keys: u64,
    /// Soma do tamanho em memória das chaves contadas.
    pub bytes: u64,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct TtlResult {
    pub prefix: String,
    /// Quantas chaves sob o prefixo têm prazo para expirar.
    pub with_ttl: u64,
    /// Quantas não expiram nunca.
    pub persistent: u64,
    pub min_ms: Option<i64>,
    pub max_ms: Option<i64>,
    pub median_ms: Option<i64>,
}

/// O que uma escrita liberada produziu.
#[derive(Debug, Serialize, JsonSchema)]
pub struct WriteResult {
    /// A entrada de histórico que registra a mudança, com o valor anterior
    /// guardado. É por este id que o dono desfaz a mudança no aplicativo.
    pub entry_id: i64,
    pub summary: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PrefixSummary {
    pub prefix: String,
    pub keys: u64,
    pub bytes: u64,
}

// ------------------------------------------------------- o servidor

#[derive(Clone)]
pub struct Tools {
    state: Arc<AppState>,
    /// Onde o aplicativo aberto escuta os pedidos de escrita.
    socket: PathBuf,
    /// Quanto esperar por uma resposta humana. Parâmetro, e não constante,
    /// para o teste da espera estourada não levar dois minutos.
    approval_timeout: Duration,
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Tools>,
}

#[tool_router]
impl Tools {
    pub fn new(state: Arc<AppState>, socket: PathBuf, approval_timeout: Duration) -> Tools {
        Tools {
            state,
            socket,
            approval_timeout,
            tool_router: Self::tool_router(),
        }
    }

    /// O estado compartilhado, para quem precisa auditar o que a LLM fez.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Lista os hosts Redis cadastrados no aplicativo, com o ambiente de cada
    /// um (prod, homolog ou dev). Use o `id` devolvido aqui nas outras
    /// ferramentas. A senha nunca é devolvida.
    #[tool(
        description = "Lista os hosts Redis cadastrados, com id, nome, ambiente e endereço. Comece por aqui: todas as outras ferramentas pedem o id de um host."
    )]
    pub async fn list_hosts(&self) -> Result<Json<Vec<HostInfo>>, ErrorData> {
        let hosts = self.state.hosts.lock().unwrap().list();
        Ok(Json(
            hosts
                .into_iter()
                .map(|host| HostInfo {
                    id: host.id,
                    name: host.name,
                    env: match host.env {
                        Env::Prod => "prod",
                        Env::Homolog => "homolog",
                        Env::Dev => "dev",
                    }
                    .to_string(),
                    addr: host.addr,
                    port: host.port,
                    default_db: host.db,
                    tls: host.tls,
                })
                .collect(),
        ))
    }

    /// Bancos com ao menos uma chave.
    #[tool(
        description = "Lista os bancos de um host que têm alguma chave, com a contagem de cada um."
    )]
    pub async fn list_databases(
        &self,
        Parameters(args): Parameters<HostArgs>,
    ) -> Result<Json<Vec<DatabaseInfo>>, ErrorData> {
        let sizes = crate::commands::list_dbs(&self.state, &args.host_id)
            .await
            .map_err(erro)?;
        Ok(Json(
            sizes
                .into_iter()
                .map(|(db, keys)| DatabaseInfo { db, keys })
                .collect(),
        ))
    }

    /// Busca um texto dentro dos valores e dos nomes das chaves.
    #[tool(
        description = "Procura um pedaço de texto em qualquer lugar do banco: dentro de campo de hash, item de lista, membro de sorted set, JSON guardado como string, e também no nome da chave. Casa em qualquer posição, não só palavra inteira. Se o índice local estiver velho ou não existir, varre o banco antes e avisa no campo scanned_first."
    )]
    pub async fn search_keys(
        &self,
        Parameters(args): Parameters<SearchKeysArgs>,
    ) -> Result<Json<SearchResult>, ErrorData> {
        let (host, pw) = self.state.host_and_password(&args.host_id).map_err(erro)?;

        // O índice é um retrato. Responder uma pergunta da LLM com um
        // retrato velho faria o raciocínio dela sair de um dado que já não
        // vale — e, ao contrário de uma pessoa, ela não tem como desconfiar.
        let scanned_first = match self
            .state
            .index
            .last_scan(&args.host_id, args.db)
            .map_err(|e| erro(e.into()))?
        {
            Some(report) => {
                let idade = (crate::core::index::now_unix_ms() - report.finished_at) / 1000;
                idade > INDICE_MAX_IDADE_SEGUNDOS
            }
            None => true,
        };

        if scanned_first {
            self.state
                .index
                .rescan(&self.state.pool, &host, pw.as_deref(), args.db, |_| {})
                .await
                .map_err(|e| erro(e.into()))?;
        }

        let limite = args
            .limit
            .unwrap_or(LIMITE_BUSCA_PADRAO)
            .min(LIMITE_BUSCA_MAXIMO);

        let outcome = search(
            &self.state.index,
            &self.state.pool,
            &host,
            pw.as_deref(),
            args.db,
            &args.needle,
            limite,
        )
        .await
        .map_err(|e| erro(e.into()))?;

        Ok(Json(SearchResult {
            hits: outcome.hits,
            scanned: outcome.scanned,
            elapsed_ms: outcome.elapsed_ms,
            stale_keys_removed: outcome.stale_keys_removed,
            index_age_secs: outcome.index_age_secs,
            scanned_first,
        }))
    }

    /// Lê o valor inteiro de uma chave.
    #[tool(
        description = "Lê o conteúdo de uma chave, com o tipo, o tamanho e o prazo de expiração. Valores grandes vêm truncados, com o campo truncated marcado."
    )]
    pub async fn read_key(
        &self,
        Parameters(args): Parameters<ReadKeyArgs>,
    ) -> Result<Json<ValueView>, ErrorData> {
        let (host, pw) = self.state.host_and_password(&args.host_id).map_err(erro)?;
        let view = read_value(
            &self.state.pool,
            &host,
            pw.as_deref(),
            args.db,
            &args.key,
            ValueLimit {
                offset: 0,
                max_bytes: LEITURA_MAX_BYTES,
                max_elements: LEITURA_MAX_ELEMENTOS,
            },
        )
        .await
        .map_err(|e| erro(e.into()))?;
        Ok(Json(view))
    }

    /// Conta as chaves sob um prefixo.
    #[tool(
        description = "Conta quantas chaves existem sob um prefixo e quanto ocupam em memória. Prefixo vazio conta o banco inteiro."
    )]
    pub async fn count_keys(
        &self,
        Parameters(args): Parameters<PrefixArgs>,
    ) -> Result<Json<CountResult>, ErrorData> {
        let metas = self
            .sob_prefixo(&args.host_id, args.db, &args.prefix)
            .await?;
        Ok(Json(CountResult {
            prefix: args.prefix,
            keys: metas.len() as u64,
            bytes: metas.iter().map(|meta| meta.size_bytes).sum(),
        }))
    }

    /// Estatística dos prazos de expiração sob um prefixo.
    #[tool(
        description = "Resume os prazos de expiração das chaves sob um prefixo: menor, maior, mediana (em milissegundos) e quantas nunca expiram. Útil para descobrir se um conjunto de chaves está acumulando sem prazo."
    )]
    pub async fn key_ttls(
        &self,
        Parameters(args): Parameters<PrefixArgs>,
    ) -> Result<Json<TtlResult>, ErrorData> {
        let metas = self
            .sob_prefixo(&args.host_id, args.db, &args.prefix)
            .await?;

        let mut prazos: Vec<i64> = metas.iter().filter_map(|meta| meta.ttl_ms).collect();
        let persistent = (metas.len() - prazos.len()) as u64;
        prazos.sort_unstable();

        // Mediana de uma lista par é a média dos dois do meio — sem isso, um
        // conjunto de duas chaves reportaria a de baixo como "a do meio".
        let median_ms = if prazos.is_empty() {
            None
        } else {
            let meio = prazos.len() / 2;
            Some(if prazos.len().is_multiple_of(2) {
                (prazos[meio - 1] + prazos[meio]) / 2
            } else {
                prazos[meio]
            })
        };

        Ok(Json(TtlResult {
            prefix: args.prefix,
            with_ttl: prazos.len() as u64,
            persistent,
            min_ms: prazos.first().copied(),
            max_ms: prazos.last().copied(),
            median_ms,
        }))
    }

    /// Mapa do banco por prefixo.
    #[tool(
        description = "Agrupa as chaves do banco pelos primeiros níveis do nome (separados por ':') e devolve quantas chaves e quantos bytes há em cada grupo. É o jeito de entender a forma de um banco desconhecido sem ler chave por chave."
    )]
    pub async fn summarize_prefixes(
        &self,
        Parameters(args): Parameters<SummarizeArgs>,
    ) -> Result<Json<Vec<PrefixSummary>>, ErrorData> {
        let metas = self.sob_prefixo(&args.host_id, args.db, "").await?;
        let profundidade = args.depth.max(1);

        let mut grupos: std::collections::BTreeMap<String, (u64, u64)> =
            std::collections::BTreeMap::new();
        for meta in &metas {
            let partes: Vec<&str> = meta.key.split(':').collect();
            let grupo = if partes.len() <= profundidade {
                meta.key.clone()
            } else {
                partes[..profundidade].join(":")
            };
            let entrada = grupos.entry(grupo).or_insert((0, 0));
            entrada.0 += 1;
            entrada.1 += meta.size_bytes;
        }

        let mut resumo: Vec<PrefixSummary> = grupos
            .into_iter()
            .map(|(prefix, (keys, bytes))| PrefixSummary {
                prefix,
                keys,
                bytes,
            })
            .collect();
        // Do maior para o menor: quem pergunta a forma de um banco quer ver
        // primeiro o que ocupa mais espaço nele.
        resumo.sort_by(|a, b| b.keys.cmp(&a.keys).then_with(|| a.prefix.cmp(&b.prefix)));
        Ok(Json(resumo))
    }

    // ------------------------------------------------- escrita

    /// Grava um texto numa chave do tipo string.
    #[tool(
        description = "Grava um texto numa chave do tipo string. NÃO acontece sozinha: o dono do banco precisa liberar a escrita na janela do aplicativo redis-desk, digitando o nome do host. Se ninguém liberar em 120 segundos, nada é escrito. O valor anterior é guardado e pode ser desfeito pelo aplicativo."
    )]
    pub async fn write_key(
        &self,
        Parameters(args): Parameters<WriteKeyArgs>,
    ) -> Result<Json<WriteResult>, ErrorData> {
        self.pedir_e_escrever(
            &args.host_id,
            args.db,
            &args.key,
            WriteOp::SetString {
                value: Bytes {
                    text: Some(args.value),
                    base64: None,
                    truncated: false,
                },
            },
        )
        .await
    }

    /// Apaga uma chave.
    #[tool(
        description = "Apaga uma chave inteira. NÃO acontece sozinha: o dono do banco precisa liberar na janela do aplicativo redis-desk, digitando o nome do host. O valor apagado é guardado e pode ser desfeito pelo aplicativo."
    )]
    pub async fn delete_key(
        &self,
        Parameters(args): Parameters<DeleteKeyArgs>,
    ) -> Result<Json<WriteResult>, ErrorData> {
        self.pedir_e_escrever(&args.host_id, args.db, &args.key, WriteOp::DeleteKey)
            .await
    }

    /// Muda ou tira o prazo de expiração de uma chave.
    #[tool(
        description = "Muda o prazo de expiração de uma chave, ou o remove (ttl_ms nulo) para ela deixar de expirar. NÃO acontece sozinha: o dono do banco precisa liberar na janela do aplicativo redis-desk, digitando o nome do host."
    )]
    pub async fn set_ttl(
        &self,
        Parameters(args): Parameters<SetTtlArgs>,
    ) -> Result<Json<WriteResult>, ErrorData> {
        self.pedir_e_escrever(
            &args.host_id,
            args.db,
            &args.key,
            WriteOp::SetTtl {
                ttl_ms: args.ttl_ms,
            },
        )
        .await
    }
}

impl Tools {
    /// O ÚNICO caminho pelo qual uma escrita pedida pela LLM chega ao Redis.
    ///
    /// Monta o pedido, manda para o aplicativo e espera. Só um veredito
    /// `Allowed` segue adiante: qualquer outro desfecho — negado, espera
    /// estourada, aplicativo fechado — devolve erro sem ter tocado em nada.
    ///
    /// A escrita em si passa por `write_with_backup` com `Actor::Llm`, igual
    /// a qualquer escrita humana: mesma cópia de segurança, mesmo histórico,
    /// mesmo desfazer. A diferença fica registrada em quem pediu, que é o
    /// que permite auditar depois.
    async fn pedir_e_escrever(
        &self,
        host_id: &str,
        db: u8,
        key: &str,
        op: WriteOp,
    ) -> Result<Json<WriteResult>, ErrorData> {
        let (host, pw) = self.state.host_and_password(host_id).map_err(erro)?;
        let summary = op_summary(key, &op);

        let pedido = ApprovalRequest {
            id: uuid::Uuid::new_v4().to_string(),
            host_id: host.id.clone(),
            host_name: host.name.clone(),
            env: host.env,
            db,
            key: key.to_string(),
            summary: summary.clone(),
            op: op.clone(),
            asked_at: crate::core::index::now_unix_ms(),
        };

        match request_approval(&self.socket, pedido, self.approval_timeout).await {
            Ok(ApprovalVerdict::Allowed) => {}
            Ok(ApprovalVerdict::Denied) => {
                return Err(ErrorData::invalid_request(
                    "o dono do banco recusou esta escrita na janela do aplicativo".to_string(),
                    None,
                ));
            }
            Ok(ApprovalVerdict::TimedOut) => {
                return Err(ErrorData::invalid_request(
                    "ninguém liberou a escrita a tempo na janela do aplicativo: \
                     nada foi alterado"
                        .to_string(),
                    None,
                ));
            }
            Err(err) => return Err(ErrorData::invalid_request(err.to_string(), None)),
        }

        let entry_id = self
            .state
            .history
            .write_with_backup(
                &self.state.pool,
                &host,
                pw.as_deref(),
                db,
                key,
                &op,
                Actor::Llm,
            )
            .await
            .map_err(|e| erro(e.into()))?;

        // O índice conhecia esta chave com o valor antigo; a busca não pode
        // continuar devolvendo um texto que já não está lá.
        let _ = self.state.index.forget_key(&host.id, db, key);

        Ok(Json(WriteResult { entry_id, summary }))
    }

    /// As chaves do banco cujo nome começa com `prefix`.
    ///
    /// Filtra em Rust depois de um `SCAN` completo em vez de mandar o padrão
    /// ao servidor: o `MATCH` do `SCAN` filtra por lote, não encurta a
    /// varredura, e a contagem precisa de todos os lotes de qualquer jeito.
    async fn sob_prefixo(
        &self,
        host_id: &str,
        db: u8,
        prefix: &str,
    ) -> Result<Vec<KeyMeta>, ErrorData> {
        let (host, pw) = self.state.host_and_password(host_id).map_err(erro)?;
        let metas = scan_all(&self.state.pool, &host, pw.as_deref(), db)
            .await
            .map_err(|e| erro(e.into()))?;
        Ok(metas
            .into_iter()
            .filter(|meta| meta.key.starts_with(prefix))
            .collect())
    }
}

#[tool_handler(
    router = self.tool_router,
    name = "redis-desk",
    instructions = "Ferramentas de LEITURA sobre os bancos Redis cadastrados no aplicativo redis-desk. \
Comece sempre por `list_hosts` para descobrir os ids e, principalmente, o AMBIENTE de cada host. \
Um host marcado como `prod` é um banco de produção de verdade: trate o que você lê dele como dado real de pessoas. \
Nenhuma ferramenta aqui altera o que quer que seja — escrever exige que o dono libere na janela do aplicativo."
)]
impl ServerHandler for Tools {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AppState, add_host};
    use crate::core::config::{Env, Host};
    use redis::aio::MultiplexedConnection;

    /// Banco só destes testes (0–10 já são usados pelos outros módulos).
    const TEST_DB: u8 = 11;

    fn host_spec() -> Host {
        Host {
            id: String::new(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            db: TEST_DB,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    /// Estado novo em diretório temporário, com um host já cadastrado.
    fn montar() -> (tempfile::TempDir, Tools, String) {
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(dir.path()).unwrap();
        let host_id = add_host(&state, host_spec(), None).unwrap();
        // Socket inexistente de propósito: nenhuma ferramenta DE LEITURA
        // pode precisar do aplicativo aberto.
        let socket = dir.path().join("sem-app.sock");
        (
            dir,
            Tools::new(
                std::sync::Arc::new(state),
                socket,
                std::time::Duration::from_millis(50),
            ),
            host_id,
        )
    }

    async fn raw_conn() -> MultiplexedConnection {
        redis::Client::open(format!("redis://127.0.0.1:6399/{TEST_DB}"))
            .unwrap()
            .get_multiplexed_async_connection()
            .await
            .unwrap()
    }

    async fn set(conn: &mut MultiplexedConnection, key: &str, value: &str) {
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg(value)
            .query_async(conn)
            .await
            .unwrap();
    }

    async fn expire(conn: &mut MultiplexedConnection, key: &str, secs: i64) {
        let _: () = redis::cmd("EXPIRE")
            .arg(key)
            .arg(secs)
            .query_async(conn)
            .await
            .unwrap();
    }

    async fn del(conn: &mut MultiplexedConnection, keys: &[&str]) {
        for key in keys {
            let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
        }
    }

    #[tokio::test]
    async fn list_hosts_never_returns_a_password() {
        let (_dir, tools, _id) = montar();

        let hosts = tools.list_hosts().await.unwrap().0;

        assert_eq!(hosts.len(), 1);
        let bruto = serde_json::to_string(&hosts).unwrap();
        // Nenhum caminho até a senha: nem o valor, nem um campo que a
        // prometa. O chaveiro existe para a senha não sair dele.
        assert!(
            !bruto.contains("password") && !bruto.contains("senha"),
            "a lista de hosts não pode expor senha de jeito nenhum, veio {bruto}"
        );
    }

    #[tokio::test]
    async fn search_keys_finds_value_text() {
        let (_dir, tools, host_id) = montar();
        let mut conn = raw_conn().await;
        let key = "mcp_test:busca:alvo";
        let needle = "alvo-mcp-busca-4b2c";

        del(&mut conn, &[key]).await;
        set(&mut conn, key, &format!("registro {needle}")).await;

        let achados = tools
            .search_keys(Parameters(SearchKeysArgs {
                host_id: host_id.clone(),
                db: TEST_DB,
                needle: needle.to_string(),
                limit: Some(20),
            }))
            .await
            .unwrap()
            .0;

        del(&mut conn, &[key]).await;
        assert!(
            achados.hits.iter().any(|hit| hit.key == key),
            "a busca precisava achar a chave pelo texto do valor, veio {:?}",
            achados.hits
        );
    }

    #[tokio::test]
    async fn read_key_returns_the_whole_value_unmasked() {
        let (_dir, tools, host_id) = montar();
        let mut conn = raw_conn().await;
        let key = "mcp_test:leitura:cartao";
        // Decisão explícita do dono: a LLM recebe o valor em claro.
        let segredo = "cartao 4111111111111111 cvv 737";

        del(&mut conn, &[key]).await;
        set(&mut conn, key, segredo).await;

        let view = tools
            .read_key(Parameters(ReadKeyArgs {
                host_id: host_id.clone(),
                db: TEST_DB,
                key: key.to_string(),
            }))
            .await
            .unwrap()
            .0;

        del(&mut conn, &[key]).await;
        let crate::core::value::Value::Str(bytes) = view.value else {
            panic!("esperava uma string, veio {:?}", view.value)
        };
        assert_eq!(bytes.text.as_deref(), Some(segredo));
    }

    #[tokio::test]
    async fn count_keys_counts_by_prefix() {
        let (_dir, tools, host_id) = montar();
        let mut conn = raw_conn().await;
        let dentro = ["mcp_test:conta:a", "mcp_test:conta:b", "mcp_test:conta:c"];
        let fora = "mcp_test:outro:z";

        del(&mut conn, &dentro).await;
        del(&mut conn, &[fora]).await;
        for key in dentro {
            set(&mut conn, key, "x").await;
        }
        set(&mut conn, fora, "x").await;

        let total = tools
            .count_keys(Parameters(PrefixArgs {
                host_id: host_id.clone(),
                db: TEST_DB,
                prefix: "mcp_test:conta:".to_string(),
            }))
            .await
            .unwrap()
            .0;

        del(&mut conn, &dentro).await;
        del(&mut conn, &[fora]).await;
        assert_eq!(total.keys, 3);
    }

    #[tokio::test]
    async fn key_ttls_reports_min_max_median_and_persistent_count() {
        let (_dir, tools, host_id) = montar();
        let mut conn = raw_conn().await;
        let prefixo = "mcp_test:prazo:";
        let com_prazo = [
            ("mcp_test:prazo:curto", 100),
            ("mcp_test:prazo:medio", 200),
            ("mcp_test:prazo:longo", 300),
        ];
        let eterna = "mcp_test:prazo:eterna";

        del(&mut conn, &[eterna]).await;
        for (key, _) in com_prazo {
            del(&mut conn, &[key]).await;
        }
        for (key, segundos) in com_prazo {
            set(&mut conn, key, "x").await;
            expire(&mut conn, key, segundos).await;
        }
        set(&mut conn, eterna, "x").await;

        let prazos = tools
            .key_ttls(Parameters(PrefixArgs {
                host_id: host_id.clone(),
                db: TEST_DB,
                prefix: prefixo.to_string(),
            }))
            .await
            .unwrap()
            .0;

        del(&mut conn, &[eterna]).await;
        for (key, _) in com_prazo {
            del(&mut conn, &[key]).await;
        }

        assert_eq!(
            prazos.persistent, 1,
            "a chave sem prazo precisa ser contada à parte"
        );
        assert_eq!(prazos.with_ttl, 3);
        // Tolerância de um segundo: o relógio anda entre o EXPIRE e o PTTL.
        let perto = |valor: Option<i64>, esperado: i64| {
            let valor = valor.expect("esperava um prazo");
            (valor - esperado).abs() < 1500
        };
        assert!(
            perto(prazos.min_ms, 100_000),
            "mínimo veio {:?}",
            prazos.min_ms
        );
        assert!(
            perto(prazos.max_ms, 300_000),
            "máximo veio {:?}",
            prazos.max_ms
        );
        assert!(
            perto(prazos.median_ms, 200_000),
            "mediana veio {:?}",
            prazos.median_ms
        );
    }

    #[tokio::test]
    async fn reading_without_a_recent_scan_scans_first_and_says_so() {
        let (_dir, tools, host_id) = montar();
        let mut conn = raw_conn().await;
        let key = "mcp_test:sem_varredura:alvo";
        let needle = "alvo-sem-varredura-9f1a";

        del(&mut conn, &[key]).await;
        set(&mut conn, key, &format!("registro {needle}")).await;

        // Índice novo em folha: nunca varrido para este (host, banco).
        let primeira = tools
            .search_keys(Parameters(SearchKeysArgs {
                host_id: host_id.clone(),
                db: TEST_DB,
                needle: needle.to_string(),
                limit: Some(20),
            }))
            .await
            .unwrap()
            .0;

        assert!(
            primeira.scanned_first,
            "sem varredura recente, a ferramenta precisa varrer antes e avisar"
        );
        assert!(
            primeira.hits.iter().any(|hit| hit.key == key),
            "e a varredura que ela mesma fez precisa valer para esta resposta"
        );

        // Agora existe varredura recente: não varre de novo.
        let segunda = tools
            .search_keys(Parameters(SearchKeysArgs {
                host_id: host_id.clone(),
                db: TEST_DB,
                needle: needle.to_string(),
                limit: Some(20),
            }))
            .await
            .unwrap()
            .0;

        del(&mut conn, &[key]).await;
        assert!(!segunda.scanned_first);
    }
}
