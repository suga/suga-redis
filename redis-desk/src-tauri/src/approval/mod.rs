//! A trava de escrita da LLM.
//!
//! # Por que existe
//!
//! O servidor MCP (Task 15) lê sozinho. Escrever, não: ele não tem como
//! gravar no Redis por conta própria em caminho nenhum. Para escrever, ele
//! monta um PEDIDO, manda para o aplicativo aberto por um socket unix, e
//! fica esperando. O aplicativo mostra o pedido na tela e o dono libera
//! digitando o nome do host. Só então a escrita acontece.
//!
//! Dois processos, e de propósito: o processo do servidor MCP não tem
//! janela, e portanto não tem como perguntar nada a ninguém. Quem pergunta é
//! o aplicativo. Sem aplicativo aberto não há a quem perguntar, e a escrita
//! simplesmente não acontece — essa é a propriedade que o desenho inteiro
//! existe para garantir.
//!
//! # A trava vale em qualquer ambiente
//!
//! Inclusive em `dev`. A confirmação de produção (Task 9) e esta trava
//! respondem a perguntas diferentes: aquela é sobre ONDE a mudança cai, esta
//! é sobre QUEM a pediu. Uma LLM pedindo uma escrita é sempre alguém a mais
//! na sala, não importa o ambiente.

pub mod client;
pub mod server;

use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio::sync::oneshot;

pub use client::request_approval;
pub use server::{serve, socket_path};

use crate::core::config::Env;
use crate::core::value::WriteOp;

/// Tempo que um pedido espera por uma resposta humana antes de desistir.
pub const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Um pedido de escrita feito pela LLM, à espera de gente.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: String,
    pub host_id: String,
    /// O nome do host. É exatamente este texto que o dono terá que digitar.
    pub host_name: String,
    pub env: Env,
    pub db: u8,
    pub key: String,
    /// A frase que a pessoa lê para decidir.
    pub summary: String,
    pub op: WriteOp,
    /// Quando o pedido chegou (epoch em milissegundos).
    pub asked_at: i64,
}

/// O desfecho de um pedido.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalVerdict {
    Allowed,
    Denied,
    TimedOut,
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("não existe pedido de escrita com o id \"{0}\"")]
    Unknown(String),

    #[error("o nome digitado não confere com o nome do host")]
    NameMismatch,

    #[error(
        "nenhum aplicativo redis-desk está aberto: abra o aplicativo e deixe-o aberto \
         para liberar esta escrita"
    )]
    AppNotRunning,

    #[error("o aplicativo fechou antes de responder ao pedido")]
    AppWentAway,

    #[error("falha ao falar com o aplicativo: {0}")]
    Io(#[from] std::io::Error),

    #[error("resposta malformada do aplicativo: {0}")]
    Protocol(String),
}

pub type Result<T> = std::result::Result<T, ApprovalError>;

/// Um pedido esperando, junto com o canal que devolve o veredito a quem
/// está do outro lado do socket.
struct Pendente {
    request: ApprovalRequest,
    responder: oneshot::Sender<ApprovalVerdict>,
}

/// A fila de pedidos que o aplicativo mostra na tela.
///
/// Vive no `AppState` da janela. O processo do MCP NÃO tem uma destas: ele
/// só fala pelo socket.
#[derive(Default)]
pub struct ApprovalQueue {
    pendentes: Mutex<Vec<Pendente>>,
}

impl ApprovalQueue {
    pub fn new() -> ApprovalQueue {
        ApprovalQueue::default()
    }

    /// Os pedidos à espera, do mais antigo para o mais novo.
    pub fn pending(&self) -> Vec<ApprovalRequest> {
        self.pendentes
            .lock()
            .unwrap()
            .iter()
            .map(|p| p.request.clone())
            .collect()
    }

    /// Põe um pedido na fila e devolve por onde o veredito vai voltar.
    pub(crate) fn submit(&self, request: ApprovalRequest) -> oneshot::Receiver<ApprovalVerdict> {
        let (responder, espera) = oneshot::channel();
        self.pendentes
            .lock()
            .unwrap()
            .push(Pendente { request, responder });
        espera
    }

    /// Libera o pedido, se `typed` for EXATAMENTE o nome do host.
    ///
    /// Exatamente mesmo: sem aparar espaços, sem ignorar maiúsculas. Digitar
    /// o nome é o ato deliberado que separa "eu li o que vai acontecer" de
    /// "eu cliquei no que apareceu na frente"; aceitar uma aproximação
    /// devolveria isso a um clique.
    ///
    /// Nome errado NÃO recusa o pedido: devolve erro e deixa tudo como
    /// estava. Um erro de digitação não pode valer como decisão.
    pub fn resolve(&self, id: &str, typed: &str) -> Result<ApprovalVerdict> {
        let mut pendentes = self.pendentes.lock().unwrap();
        let posicao = pendentes
            .iter()
            .position(|p| p.request.id == id)
            .ok_or_else(|| ApprovalError::Unknown(id.to_string()))?;

        if pendentes[posicao].request.host_name != typed {
            return Err(ApprovalError::NameMismatch);
        }

        let pendente = pendentes.remove(posicao);
        drop(pendentes);

        // Um `Err` aqui só quer dizer que quem pediu já foi embora (a espera
        // estourou do outro lado). Não há nada a consertar: ninguém escreve.
        let _ = pendente.responder.send(ApprovalVerdict::Allowed);
        Ok(ApprovalVerdict::Allowed)
    }

    /// Recusa o pedido. Não pede nome digitado: dizer não é sempre barato.
    pub fn deny(&self, id: &str) -> Result<ApprovalVerdict> {
        let mut pendentes = self.pendentes.lock().unwrap();
        let posicao = pendentes
            .iter()
            .position(|p| p.request.id == id)
            .ok_or_else(|| ApprovalError::Unknown(id.to_string()))?;
        let pendente = pendentes.remove(posicao);
        drop(pendentes);

        let _ = pendente.responder.send(ApprovalVerdict::Denied);
        Ok(ApprovalVerdict::Denied)
    }

    /// Tira o pedido da fila sem veredito — quem pediu desistiu (a espera
    /// estourou, ou o processo do MCP morreu). Sem isso, a tela mostraria
    /// para sempre um pedido que ninguém mais está esperando.
    pub(crate) fn forget(&self, id: &str) {
        self.pendentes
            .lock()
            .unwrap()
            .retain(|p| p.request.id != id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{AppState, add_host};
    use crate::core::config::{Env, Host};
    use crate::core::value::WriteOp;
    use crate::mcp::tools::{DeleteKeyArgs, SetTtlArgs, Tools, WriteKeyArgs};
    use redis::aio::MultiplexedConnection;
    use rmcp::handler::server::wrapper::Parameters;
    use std::sync::Arc;
    use std::time::Duration;

    /// Banco só destes testes.
    const TEST_DB: u8 = 12;
    /// Espera curta: o teste não pode levar os 120 s de produção.
    const ESPERA_CURTA: Duration = Duration::from_millis(400);

    fn host_spec(env: Env) -> Host {
        Host {
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

    fn pedido(host_name: &str) -> ApprovalRequest {
        ApprovalRequest {
            id: "pedido-1".to_string(),
            host_id: "h1".to_string(),
            host_name: host_name.to_string(),
            env: Env::Prod,
            db: TEST_DB,
            key: "chave".to_string(),
            summary: "Definir o valor de chave".to_string(),
            op: WriteOp::DeleteKey,
            asked_at: 0,
        }
    }

    /// App montado com um host, o socket aberto e as ferramentas apontando
    /// para ele — o arranjo completo, como em produção.
    struct Cenario {
        _dir: tempfile::TempDir,
        tools: Tools,
        queue: Arc<ApprovalQueue>,
        host_id: String,
    }

    async fn montar(env: Env) -> Cenario {
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(dir.path()).unwrap();
        let host_id = add_host(&state, host_spec(env), None).unwrap();

        let socket = dir.path().join("redis-desk.sock");
        let queue = Arc::new(ApprovalQueue::new());
        let servindo = queue.clone();
        let caminho = socket.clone();
        tokio::spawn(async move { serve(servindo, &caminho).await });
        // Deixa o socket chegar a escutar antes de alguém tentar conectar.
        tokio::time::sleep(Duration::from_millis(60)).await;

        Cenario {
            _dir: dir,
            tools: Tools::new(Arc::new(state), socket, ESPERA_CURTA),
            queue,
            host_id,
        }
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

    async fn get(conn: &mut MultiplexedConnection, key: &str) -> Option<String> {
        redis::cmd("GET").arg(key).query_async(conn).await.unwrap()
    }

    async fn del(conn: &mut MultiplexedConnection, key: &str) {
        let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
    }

    /// Espera o pedido aparecer na fila e devolve o id dele.
    async fn esperar_pedido(queue: &ApprovalQueue) -> String {
        for _ in 0..100 {
            if let Some(req) = queue.pending().first() {
                return req.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("o pedido nunca chegou à fila")
    }

    #[tokio::test]
    async fn llm_write_waits_for_the_human() {
        let cenario = montar(Env::Prod).await;
        let mut conn = raw_conn().await;
        let key = "approval_test:espera";
        del(&mut conn, key).await;

        let tools = cenario.tools.clone();
        let host_id = cenario.host_id.clone();
        let escrita = tokio::spawn(async move {
            tools
                .write_key(Parameters(WriteKeyArgs {
                    host_id,
                    db: TEST_DB,
                    key: "approval_test:espera".to_string(),
                    value: "escrito pela llm".to_string(),
                }))
                .await
        });

        // Enquanto ninguém responde, o pedido fica na fila e NADA foi escrito.
        let id = esperar_pedido(&cenario.queue).await;
        assert_eq!(get(&mut conn, key).await, None);

        let pendentes = cenario.queue.pending();
        assert_eq!(pendentes.len(), 1);
        assert_eq!(pendentes[0].key, key);
        assert_eq!(pendentes[0].host_name, "Redis de teste");

        cenario.queue.resolve(&id, "Redis de teste").unwrap();
        escrita.await.unwrap().unwrap();

        assert_eq!(
            get(&mut conn, key).await.as_deref(),
            Some("escrito pela llm")
        );
        del(&mut conn, key).await;
    }

    #[test]
    fn wrong_host_name_does_not_unlock() {
        let queue = ApprovalQueue::new();
        let _espera = queue.submit(pedido("Redis pagamentos"));
        let id = queue.pending()[0].id.clone();

        // Quase certo não é certo.
        assert!(queue.resolve(&id, "redis pagamentos").is_err());
        assert!(queue.resolve(&id, "Redis pagamento").is_err());
        assert!(queue.resolve(&id, " Redis pagamentos").is_err());
        assert!(queue.resolve(&id, "").is_err());

        // E o pedido continua de pé, esperando: um erro de digitação não
        // pode valer como recusa.
        assert_eq!(queue.pending().len(), 1);
    }

    #[test]
    fn exact_host_name_unlocks() {
        let queue = ApprovalQueue::new();
        let _espera = queue.submit(pedido("Redis pagamentos"));
        let id = queue.pending()[0].id.clone();

        assert_eq!(
            queue.resolve(&id, "Redis pagamentos").unwrap(),
            ApprovalVerdict::Allowed
        );
        assert!(queue.pending().is_empty());
    }

    #[tokio::test]
    async fn no_answer_in_120s_times_out_and_does_not_write() {
        let cenario = montar(Env::Prod).await;
        let mut conn = raw_conn().await;
        let key = "approval_test:expirado";
        del(&mut conn, key).await;
        set(&mut conn, key, "intocado").await;

        // Ninguém responde: a espera estoura.
        let resultado = cenario
            .tools
            .write_key(Parameters(WriteKeyArgs {
                host_id: cenario.host_id.clone(),
                db: TEST_DB,
                key: key.to_string(),
                value: "nao devia entrar".to_string(),
            }))
            .await;

        assert!(
            resultado.is_err(),
            "sem resposta, a escrita não pode acontecer"
        );
        assert_eq!(get(&mut conn, key).await.as_deref(), Some("intocado"));
        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn denied_request_leaves_redis_untouched() {
        let cenario = montar(Env::Prod).await;
        let mut conn = raw_conn().await;
        let key = "approval_test:negado";
        del(&mut conn, key).await;
        set(&mut conn, key, "intocado").await;

        let tools = cenario.tools.clone();
        let host_id = cenario.host_id.clone();
        let escrita = tokio::spawn(async move {
            tools
                .delete_key(Parameters(DeleteKeyArgs {
                    host_id,
                    db: TEST_DB,
                    key: "approval_test:negado".to_string(),
                }))
                .await
        });

        let id = esperar_pedido(&cenario.queue).await;
        cenario.queue.deny(&id).unwrap();

        assert!(escrita.await.unwrap().is_err());
        assert_eq!(get(&mut conn, key).await.as_deref(), Some("intocado"));
        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn approved_write_is_recorded_with_actor_llm() {
        let cenario = montar(Env::Prod).await;
        let mut conn = raw_conn().await;
        let key = "approval_test:registrado";
        del(&mut conn, key).await;
        set(&mut conn, key, "antes").await;

        let tools = cenario.tools.clone();
        let host_id = cenario.host_id.clone();
        let escrita = tokio::spawn(async move {
            tools
                .set_ttl(Parameters(SetTtlArgs {
                    host_id,
                    db: TEST_DB,
                    key: "approval_test:registrado".to_string(),
                    ttl_ms: Some(60_000),
                }))
                .await
        });

        let id = esperar_pedido(&cenario.queue).await;
        cenario.queue.resolve(&id, "Redis de teste").unwrap();
        escrita.await.unwrap().unwrap();

        // Quem mexeu foi a LLM, e o histórico precisa dizer isso: é o que
        // permite auditar depois o que foi mudança de gente e o que não foi.
        let entradas =
            crate::commands::list_history(cenario.tools.state(), &cenario.host_id, 10).unwrap();
        assert_eq!(entradas.len(), 1);
        assert_eq!(entradas[0].actor, crate::core::history::Actor::Llm);
        assert_eq!(entradas[0].key, key);

        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn dev_host_is_gated_too() {
        // A trava da LLM não é sobre produção: é sobre a LLM. Em dev também
        // é o dono quem libera.
        let cenario = montar(Env::Dev).await;
        let mut conn = raw_conn().await;
        let key = "approval_test:dev";
        del(&mut conn, key).await;

        let tools = cenario.tools.clone();
        let host_id = cenario.host_id.clone();
        let escrita = tokio::spawn(async move {
            tools
                .write_key(Parameters(WriteKeyArgs {
                    host_id,
                    db: TEST_DB,
                    key: "approval_test:dev".to_string(),
                    value: "em dev".to_string(),
                }))
                .await
        });

        let id = esperar_pedido(&cenario.queue).await;
        assert_eq!(cenario.queue.pending()[0].env, Env::Dev);
        assert_eq!(
            get(&mut conn, key).await,
            None,
            "nem em dev escreve sem liberar"
        );

        cenario.queue.resolve(&id, "Redis de teste").unwrap();
        escrita.await.unwrap().unwrap();
        assert_eq!(get(&mut conn, key).await.as_deref(), Some("em dev"));
        del(&mut conn, key).await;
    }

    #[tokio::test]
    async fn write_tool_without_a_running_app_explains_how_to_fix_it() {
        // Sem app aberto não existe socket para conectar.
        let dir = tempfile::TempDir::new().unwrap();
        let state = AppState::open(dir.path()).unwrap();
        let host_id = add_host(&state, host_spec(Env::Prod), None).unwrap();
        let tools = Tools::new(
            Arc::new(state),
            dir.path().join("nao-existe.sock"),
            ESPERA_CURTA,
        );

        let erro = tools
            .write_key(Parameters(WriteKeyArgs {
                host_id,
                db: TEST_DB,
                key: "approval_test:sem_app".to_string(),
                value: "x".to_string(),
            }))
            .await
            .err()
            .expect("sem aplicativo aberto, a escrita tem que ser recusada");

        // A mensagem precisa dizer o que fazer, não só que falhou: quem lê
        // é o Claude, e é ele quem vai explicar isso para a pessoa.
        let texto = format!("{erro:?}");
        assert!(
            texto.contains("abra o aplicativo"),
            "a recusa precisa ensinar a destravar, veio {texto}"
        );
    }
}
