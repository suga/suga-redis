//! Pool de conexões Redis por (host, banco) e as duas chamadas de metadados
//! (ping e tamanho dos bancos) usadas pelo resto da aplicação. Toda conexão
//! e toda resposta tem um timeout de 5 segundos: sem isso, uma VPN caindo
//! no meio travaria a interface indefinidamente.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use redis::aio::MultiplexedConnection;
use redis::{AsyncConnectionConfig, Client};

use super::config::Host;
use super::log::{LogEntry, LogKind, LogRing, now_ms};

/// Tempo máximo de espera para abrir uma conexão ou receber uma resposta do
/// Redis.
const TIMEOUT: Duration = Duration::from_secs(5);

/// Erros de conexão e protocolo. `Display` é em português e sempre cita o
/// nome do host envolvido, para que a UI mostre uma mensagem útil sem travar.
#[derive(Debug, thiserror::Error)]
pub enum ConnError {
    #[error("não foi possível conectar ao host \"{host}\": {detail}")]
    Unreachable { host: String, detail: String },

    #[error("o host \"{host}\" recusou a autenticação")]
    Auth { host: String },

    #[error(
        "o host \"{host}\" está em modo cluster, onde só existe o banco 0 \
         (é o caso de todo ElastiCache/Valkey serverless da AWS) — \
         edite o host e troque o banco para 0"
    )]
    ClusterSingleDb { host: String },

    #[error("erro de protocolo do Redis: {0}")]
    Protocol(String),

    #[error("a chave \"{0}\" não existe mais")]
    KeyGone(String),

    #[error("a chave \"{key}\" é do tipo \"{kind}\", que não é editável neste produto")]
    NotEditable { key: String, kind: String },

    #[error(
        "não foi possível confirmar se a última operação na chave \"{key}\" foi aplicada \
         (a conexão caiu durante a escrita: {detail}) — confira o estado atual da chave antes \
         de tentar de novo"
    )]
    Ambiguous { key: String, detail: String },
}

/// Resultado padrão das operações deste módulo.
pub type Result<T> = std::result::Result<T, ConnError>;

/// Pool de conexões multiplexadas, uma por (host, banco). `get` abre a
/// conexão na primeira chamada e reaproveita a mesma depois disso.
///
/// A chave do cache é `(host.id, db)`, não só `host.id`: uma
/// `MultiplexedConnection` é um único socket físico compartilhado por quem
/// quer que a use, e qual banco está selecionado é estado desse socket
/// (`SELECT`). Se duas chamadas lógicas quisessem bancos diferentes do
/// mesmo host sobre a mesma conexão cacheada, um `SELECT` de uma
/// interferiria na outra no meio de qualquer operação — a app varre bancos
/// de 0 a 15 e várias tasks recebem `db` como parâmetro explícito, então
/// isso não é hipotético.
pub struct Pool {
    connections: Mutex<HashMap<(String, u8), MultiplexedConnection>>,
    connection_timeout: Duration,
    response_timeout: Duration,
    /// Anel onde `connect`/`drop_host` registram o que acontece com o
    /// socket. `new`/`with_timeouts` dão ao pool um anel próprio e
    /// descartável — só `with_log` troca por um compartilhado (ver seu
    /// comentário).
    logs: Arc<LogRing>,
}

impl Default for Pool {
    fn default() -> Pool {
        Pool::new()
    }
}

/// Instala o provedor de criptografia do `rustls`, uma vez por processo.
///
/// O `rustls` 0.23 não escolhe um sozinho: ele exige que o programa diga
/// qual usar, e ENTRA EM PÂNICO ao montar a primeira conexão se ninguém
/// disser. O `redis` habilita um provedor só nas dependências de teste
/// dele, então quem usa a biblioteca precisa trazer o seu. Sem isso, todo
/// host marcado com TLS era impossível de conectar — e como o Redis de
/// teste fala em texto puro, nenhum teste encostava nesse caminho.
///
/// `ring` e não `aws-lc-rs`: o segundo precisa de cmake e de um compilador
/// C, o que complicaria o build do linux dentro do contêiner sem nada em
/// troca aqui.
///
/// Mora aqui, na primeira conexão TLS, e não no `main`: assim vale igual
/// para a janela, para o servidor MCP e para os testes, sem depender de
/// ninguém lembrar de chamá-lo na ordem certa.
fn garantir_provedor_tls() {
    static UMA_VEZ: std::sync::Once = std::sync::Once::new();
    UMA_VEZ.call_once(|| {
        // Falha se outro já instalou um provedor; nesse caso o que está lá
        // serve, e insistir seria pior.
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

impl Pool {
    /// Pool com o timeout padrão de 5 segundos para conectar e para
    /// esperar resposta — o valor que deve ser usado em produção.
    pub fn new() -> Pool {
        Pool::with_timeouts(TIMEOUT, TIMEOUT)
    }

    /// Pool com timeouts customizados. Existe para que os testes possam
    /// injetar timeouts curtos e exercitar de verdade o caminho de "host
    /// parou de responder", sem esperar 5 segundos de verdade.
    pub fn with_timeouts(connection_timeout: Duration, response_timeout: Duration) -> Pool {
        Pool {
            connections: Mutex::new(HashMap::new()),
            connection_timeout,
            response_timeout,
            logs: Arc::new(LogRing::new()),
        }
    }

    /// Troca o anel de log do pool pelo `logs` informado, descartando o anel
    /// próprio que `new`/`with_timeouts` criaram. Existe para a Task 3: o
    /// `AppState` guarda um único `Arc<LogRing>` e o entrega aqui, para que
    /// as entradas de conexão deste pool caiam no mesmo anel que a fachada
    /// usa para falhas e operações lentas — sem isso, cada um teria seu
    /// próprio histórico e a UI veria metade da história.
    pub fn with_log(mut self, logs: Arc<LogRing>) -> Pool {
        self.logs = logs;
        self
    }

    /// Devolve a conexão multiplexada do (host, banco), abrindo-a (e
    /// cacheando) se ainda não existir uma. O `SELECT` desse banco acontece
    /// uma vez, embutido na URL de conexão (ver `build_url`), no momento em
    /// que a conexão é criada — não a cada operação.
    pub async fn get(
        &self,
        host: &Host,
        db: u8,
        password: Option<&str>,
    ) -> Result<MultiplexedConnection> {
        let key = (host.id.clone(), db);
        if let Some(conn) = self.connections.lock().unwrap().get(&key) {
            return Ok(conn.clone());
        }

        let conn = self.connect(host, db, password).await?;

        // Outra chamada concorrente pode ter aberto a conexão primeiro
        // enquanto esperávamos `connect`; nesse caso ficamos com a que já
        // está no mapa, para que nunca haja duas conexões vivas por
        // (host, banco).
        let mut connections = self.connections.lock().unwrap();
        Ok(connections.entry(key).or_insert(conn).clone())
    }

    /// Descarta todas as conexões cacheadas do host (se houver, em
    /// qualquer banco), forçando conexões novas na próxima chamada a
    /// `get`. Evicta por host inteiro, não só o `(host, db)` que falhou,
    /// porque a classificação de erro de conexão (`ConnError::Unreachable`)
    /// normalmente indica que o socket caiu — e um socket é compartilhado
    /// por todos os bancos abertos daquele host.
    ///
    /// Recebe o `Host` inteiro, não só o id: o id é a chave usada para
    /// achar as conexões no mapa, mas o nome (`host.name`) é o que vai para
    /// o anel de log quando havia algo para descartar — ver `LogEntry::host`
    /// em `core::log`.
    pub fn drop_host(&self, host: &Host) {
        // Verifica se havia algo a descartar antes de mexer no mapa: sem
        // isso, todo `drop_host` (inclusive os chamados por `with_retry`
        // antes de uma conexão sequer ter sido aberta) empurraria uma
        // entrada "disconnect" vazia no anel.
        let had_connections = {
            let mut connections = self.connections.lock().unwrap();
            let had_connections = connections.keys().any(|(host_id, _db)| host_id == &host.id);
            connections.retain(|(host_id, _db), _conn| host_id != &host.id);
            had_connections
        };

        if had_connections {
            self.logs.push(LogEntry {
                at_ms: now_ms(),
                kind: LogKind::Connection,
                host: host.name.clone(),
                db: None,
                operation: "disconnect".to_string(),
                duration_ms: None,
                detail: None,
            });
        }
    }

    async fn connect(
        &self,
        host: &Host,
        db: u8,
        password: Option<&str>,
    ) -> Result<MultiplexedConnection> {
        if host.tls {
            garantir_provedor_tls();
        }

        let url = build_url(host, db, password);
        let start = Instant::now();

        let client = match Client::open(url) {
            Ok(client) => client,
            Err(err) => {
                let classified = classify(host, err);
                self.log_connect(host, db, None, Some(classified.to_string()));
                return Err(classified);
            }
        };

        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(self.connection_timeout))
            .set_response_timeout(Some(self.response_timeout));

        match client
            .get_multiplexed_async_connection_with_config(&config)
            .await
        {
            Ok(conn) => {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                self.log_connect(host, db, Some(elapsed_ms), None);
                Ok(conn)
            }
            Err(err) => {
                let classified = classify(host, err);
                self.log_connect(host, db, None, Some(classified.to_string()));
                Err(classified)
            }
        }
    }

    /// Registra uma tentativa de abrir o socket de `(host, db)` no anel de
    /// log: `duration_ms` preenchido e `detail` ausente numa conexão bem
    /// sucedida, ou o inverso numa falha. `detail` é sempre o `Display` do
    /// `ConnError` já classificado (`classify`), nunca a URL de conexão —
    /// que embute a senha (ver `build_url`) — nem o erro cru do driver, que
    /// poderia carregar o mesmo texto.
    fn log_connect(&self, host: &Host, db: u8, duration_ms: Option<u64>, detail: Option<String>) {
        self.logs.push(LogEntry {
            at_ms: now_ms(),
            kind: LogKind::Connection,
            host: host.name.clone(),
            db: Some(db),
            operation: "connect".to_string(),
            duration_ms,
            detail,
        });
    }

    /// Roda uma operação assíncrona arbitrária sobre a conexão cacheada do
    /// host, entregando-lhe uma cópia da conexão multiplexada (clonar é só
    /// clonar o handle, não abrir socket novo). Se a operação falhar por um
    /// erro de conexão (a conexão cacheada morreu — ex.: a rede caiu depois
    /// que ela foi aberta, e ninguém chamou `drop_host` para avisar o pool
    /// disso), descarta essa conexão e tenta de novo exatamente uma vez com
    /// uma conexão nova. Erros lógicos do Redis (ex.: um tipo errado de
    /// comando para a chave) não disparam esse retry — eles não dizem nada
    /// sobre a saúde da conexão.
    ///
    /// Generaliza o que antes só `run` fazia para um `redis::Cmd` único: a
    /// varredura de chaves (`scan_all`, em `core::keys`) precisa de várias
    /// idas e vindas sobre a mesma tentativa de conexão — o cursor do
    /// `SCAN` e lotes de metadados em pipeline — então a operação aqui é
    /// uma closure arbitrária, não um comando isolado. `pub(crate)` porque
    /// `core::keys` precisa chamá-la diretamente.
    pub(crate) async fn with_retry<T, F, Fut>(
        &self,
        host: &Host,
        db: u8,
        password: Option<&str>,
        operation: F,
    ) -> Result<T>
    where
        F: Fn(MultiplexedConnection) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, redis::RedisError>>,
    {
        let conn = self.get(host, db, password).await?;

        match operation(conn).await {
            Ok(value) => Ok(value),
            Err(err) => match classify(host, err) {
                ConnError::Unreachable { .. } => {
                    self.drop_host(host);
                    let conn = self.get(host, db, password).await?;
                    operation(conn).await.map_err(|err| classify(host, err))
                }
                classified => Err(classified),
            },
        }
    }

    /// Como `with_retry`, mas sem o retry: roda `operation` exatamente uma
    /// vez. Existe para operações NÃO idempotentes, onde reenviar o mesmo
    /// comando depois de um erro de classe conexão pode causar dano maior
    /// que relatar o erro — ex. `core::value::WriteOp::RemoveListItem`
    /// (um `LSET` por índice reenviado sobre uma lista que já mudou de
    /// tamanho marca e apaga o elemento errado) e `PushListItem` (reenviar
    /// um `LPUSH`/`RPUSH` que na verdade já tinha sido aplicado empurra o
    /// item duas vezes). Um erro de classe conexão (`Unreachable`) vira
    /// `ConnError::Ambiguous`: a conexão cacheada é descartada (para que a
    /// próxima chamada, de qualquer operação, não reuse um socket morto),
    /// mas esta chamada específica não tenta de novo — quem chamou só sabe
    /// que não há certeza se a operação foi aplicada, e isso é informação
    /// mais honesta que arriscar aplicar errado ou duas vezes. `pub(crate)`
    /// pela mesma razão de `with_retry`: `core::value` precisa chamá-la
    /// diretamente.
    pub(crate) async fn run_once<T, F, Fut>(
        &self,
        host: &Host,
        db: u8,
        password: Option<&str>,
        key: &str,
        operation: F,
    ) -> Result<T>
    where
        F: FnOnce(MultiplexedConnection) -> Fut,
        Fut: std::future::Future<Output = std::result::Result<T, redis::RedisError>>,
    {
        let conn = self.get(host, db, password).await?;

        match operation(conn).await {
            Ok(value) => Ok(value),
            Err(err) => match classify(host, err) {
                ConnError::Unreachable { detail, .. } => {
                    self.drop_host(host);
                    Err(ConnError::Ambiguous {
                        key: key.to_string(),
                        detail,
                    })
                }
                classified => Err(classified),
            },
        }
    }

    /// Roda um único comando no (host, banco), reaproveitando a conexão
    /// cacheada; a lógica de resiliência (descartar e tentar de novo uma
    /// vez) fica em `with_retry`.
    async fn run<T: redis::FromRedisValue>(
        &self,
        host: &Host,
        db: u8,
        password: Option<&str>,
        cmd: &redis::Cmd,
    ) -> Result<T> {
        self.with_retry(host, db, password, move |mut conn| async move {
            cmd.query_async::<T>(&mut conn).await
        })
        .await
    }

    /// Mede o tempo de ida e volta de um `PING` no host, no banco padrão
    /// dele (`host.db`) — `PING` não depende de banco, mas toda operação
    /// passa por uma conexão de algum banco específico.
    pub async fn ping(&self, host: &Host, pw: Option<&str>) -> Result<Duration> {
        let start = Instant::now();
        self.run::<String>(host, host.db, pw, &redis::cmd("PING"))
            .await?;
        Ok(start.elapsed())
    }

    /// Lê `INFO keyspace` e devolve, por banco, o índice e o número de
    /// chaves. Bancos vazios não aparecem em `INFO keyspace` e por isso não
    /// aparecem no resultado.
    pub async fn db_sizes(&self, host: &Host, pw: Option<&str>) -> Result<Vec<(u8, u64)>> {
        let mut cmd = redis::cmd("INFO");
        cmd.arg("keyspace");

        let info: String = self.run(host, host.db, pw, &cmd).await?;
        let sizes = parse_keyspace(&info)?;
        if !sizes.is_empty() {
            return Ok(sizes);
        }

        // Alguns servidores gerenciados (o Valkey serverless da AWS, entre
        // eles) respondem `INFO keyspace` vazio. Sem este recuo, o seletor
        // de bancos da tela ficaria sem contagem nenhuma — e a contagem é
        // justamente o que diz à pessoa onde há dado.
        let dbsize = redis::cmd("DBSIZE");
        let total: u64 = self.run(host, host.db, pw, &dbsize).await?;
        Ok(if total > 0 {
            vec![(host.db, total)]
        } else {
            Vec::new()
        })
    }
}

/// Monta a URL de conexão a partir do host: `rediss://` quando `tls`,
/// senão `redis://`, com usuário/senha (se houver) e `db` como o banco
/// selecionado (não necessariamente `host.db` — quem chama escolhe, ver
/// `Pool::get`).
fn build_url(host: &Host, db: u8, password: Option<&str>) -> String {
    let scheme = if host.tls { "rediss" } else { "redis" };
    let mut url = format!("{scheme}://");

    if host.username.is_some() || password.is_some() {
        if let Some(username) = &host.username {
            url.push_str(&url_encode(username));
        }
        if let Some(password) = password {
            url.push(':');
            url.push_str(&url_encode(password));
        }
        url.push('@');
    }

    url.push_str(&host.addr);
    url.push(':');
    url.push_str(&host.port.to_string());
    url.push('/');
    url.push_str(&db.to_string());

    url
}

/// Percent-encoding mínimo para usuário/senha dentro da URL de conexão, para
/// que caracteres como `@`, `:` ou `/` numa senha não quebrem o parsing dela.
fn url_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Classifica um erro do driver Redis num `ConnError`, sempre citando o nome
/// do host (`host.name`), nunca o endereço ou a senha.
///
/// `is_io_error()` cobre tanto recusa de conexão quanto timeout de conexão
/// ou de resposta: o redis-rs embrulha qualquer `std::io::Error` com
/// `ErrorKind::Io`, então checar `is_connection_refusal()`/`is_timeout()`
/// à parte seria redundante.
fn classify(host: &Host, err: redis::RedisError) -> ConnError {
    if err.is_io_error() {
        return ConnError::Unreachable {
            host: host.name.clone(),
            detail: err.to_string(),
        };
    }

    if err.kind() == redis::ErrorKind::AuthenticationFailed {
        return ConnError::Auth {
            host: host.name.clone(),
        };
    }

    // Um servidor em modo cluster recusa o `SELECT` que o driver manda para
    // qualquer banco diferente de 0. O texto cru do protocolo não diz à
    // pessoa o que fazer; esta variante diz.
    if err
        .to_string()
        .contains("SELECT is not allowed in cluster mode")
    {
        return ConnError::ClusterSingleDb {
            host: host.name.clone(),
        };
    }

    ConnError::Protocol(err.to_string())
}

/// Faz o parsing da resposta de `INFO keyspace`, linhas no formato
/// `db0:keys=3,expires=0,avg_ttl=0`.
fn parse_keyspace(info: &str) -> Result<Vec<(u8, u64)>> {
    let mut sizes = Vec::new();

    for line in info.lines() {
        let Some(rest) = line.strip_prefix("db") else {
            continue;
        };
        let Some((index, fields)) = rest.split_once(':') else {
            continue;
        };

        let index: u8 = index.parse().map_err(|_| {
            ConnError::Protocol(format!("índice de banco inválido em INFO keyspace: {line}"))
        })?;

        let keys = fields
            .split(',')
            .find_map(|field| field.strip_prefix("keys="))
            .ok_or_else(|| {
                ConnError::Protocol(format!("campo \"keys\" ausente em INFO keyspace: {line}"))
            })?;

        let keys: u64 = keys.parse().map_err(|_| {
            ConnError::Protocol(format!(
                "contagem de chaves inválida em INFO keyspace: {line}"
            ))
        })?;

        sizes.push((index, keys));
    }

    Ok(sizes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_PORT: u16 = 6399;
    const CLOSED_PORT: u16 = 6555;

    fn test_host(name: &str, port: u16) -> Host {
        Host {
            id: format!("conn-test-{name}"),
            name: name.to_string(),
            addr: "127.0.0.1".to_string(),
            port,
            db: 0,
            username: None,
            tls: false,
            env: super::super::config::Env::Dev,
        }
    }

    #[tokio::test]
    async fn ping_returns_a_duration() {
        let pool = Pool::new();
        let host = test_host("Redis de teste", TEST_PORT);

        let elapsed = pool.ping(&host, None).await.unwrap();

        // Só nos importa que o PING completou e mediu algo plausível; não
        // travou nem estourou o timeout de 5s.
        assert!(elapsed < Duration::from_secs(5));
    }

    /// Lê quantas chaves `db_sizes` reporta para o db0 agora mesmo (`0` se
    /// o banco nem aparecer em `INFO keyspace`).
    fn db0_count(sizes: &[(u8, u64)]) -> u64 {
        sizes
            .iter()
            .find(|(index, _)| *index == 0)
            .map(|(_, count)| *count)
            .unwrap_or(0)
    }

    #[tokio::test]
    async fn db_sizes_counts_keys_in_db0() {
        let pool = Pool::new();
        let host = test_host("Redis de teste", TEST_PORT);
        let keys = [
            "conn_rs_test:db_sizes:a",
            "conn_rs_test:db_sizes:b",
            "conn_rs_test:db_sizes:c",
        ];

        // Conta antes de escrever, em vez de assumir que db0 está vazio: o
        // container de teste é compartilhado por toda a suíte, que roda em
        // paralelo por padrão — outro teste pode ter alguma chave própria
        // ali no mesmo instante. O que este teste garante é que db_sizes
        // enxerga exatamente as 3 chaves que ele mesmo criou, não o total
        // absoluto do banco.
        let before = pool.db_sizes(&host, None).await.unwrap();
        let before_count = db0_count(&before);

        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        for key in keys {
            let _: () = redis::cmd("SET")
                .arg(key)
                .arg("1")
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let after = pool.db_sizes(&host, None).await;

        // Limpa as chaves que criamos antes de verificar o resultado, para
        // que uma falha na asserção não deixe `db0` sujo para a próxima vez
        // que este teste rodar.
        for key in keys {
            let _: () = redis::cmd("DEL")
                .arg(key)
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let after_count = db0_count(&after.unwrap());
        assert_eq!(
            after_count - before_count,
            3,
            "esperava +3 chaves em db0 depois do SET, veio before={before_count} after={after_count}"
        );
    }

    #[tokio::test]
    async fn connections_are_isolated_per_db() {
        let pool = Pool::new();
        let host = test_host("Redis de teste", TEST_PORT);

        let mut conn_db0 = pool.get(&host, 0, None).await.unwrap();
        let id0: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn_db0)
            .await
            .unwrap();

        let mut conn_db1 = pool.get(&host, 1, None).await.unwrap();
        let id1: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn_db1)
            .await
            .unwrap();

        // Dois `get()` para o mesmo host, mas bancos diferentes, não podem
        // cair na mesma conexão cacheada — senão um `SELECT` de um dos
        // lados vazaria para o outro.
        assert_ne!(
            id0, id1,
            "conexões de bancos diferentes do mesmo host devem ser sockets diferentes"
        );

        let key = "conn_rs_test:db_isolation:key";
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-db1")
            .query_async(&mut conn_db1)
            .await
            .unwrap();

        let exists_in_db0: bool = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut conn_db0)
            .await
            .unwrap();

        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut conn_db1)
            .await
            .unwrap();

        assert!(
            !exists_in_db0,
            "uma chave escrita via a conexão do db1 não pode aparecer na conexão do db0"
        );
    }

    #[tokio::test]
    async fn unreachable_host_names_itself_in_the_error() {
        let pool = Pool::new();
        let host = test_host("Servidor Inacessível", CLOSED_PORT);

        let err = pool.ping(&host, None).await.unwrap_err();

        match &err {
            ConnError::Unreachable { host: name, .. } => {
                assert_eq!(name, "Servidor Inacessível");
            }
            other => panic!("esperava ConnError::Unreachable, veio {other:?}"),
        }
        assert!(err.to_string().contains("Servidor Inacessível"));
    }

    #[tokio::test]
    async fn pool_reuses_the_same_connection() {
        let pool = Pool::new();
        let host = test_host("Redis de teste", TEST_PORT);

        let mut conn1 = pool.get(&host, host.db, None).await.unwrap();
        let id1: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn1)
            .await
            .unwrap();

        let mut conn2 = pool.get(&host, host.db, None).await.unwrap();
        let id2: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn2)
            .await
            .unwrap();

        // Dois `get()` seguidos devolvem a mesma conexão multiplexada: o
        // Redis enxerga as duas chamadas vindo do mesmo client id, ou seja,
        // nenhuma conexão nova foi aberta (o que `CLIENT LIST` também
        // mostraria, com uma linha a mais por conexão nova).
        assert_eq!(id1, id2);
    }

    #[tokio::test]
    async fn opening_a_connection_is_logged() {
        let ring = Arc::new(LogRing::new());
        let pool = Pool::new().with_log(ring.clone());
        let mut host = test_host("Redis de teste", TEST_PORT);
        host.db = 13;

        pool.get(&host, host.db, None).await.unwrap();

        let snapshot = ring.snapshot();
        let entry = snapshot
            .iter()
            .find(|e| e.operation == "connect")
            .expect("esperava uma entrada de conexão no anel");

        assert_eq!(entry.kind, LogKind::Connection);
        assert_eq!(entry.host, "Redis de teste");
        assert_eq!(entry.db, Some(13));
        assert!(
            entry.duration_ms.is_some(),
            "duration_ms devia estar preenchido para uma conexão aberta com sucesso"
        );
        assert!(entry.detail.is_none());
    }

    #[tokio::test]
    async fn a_refused_connection_is_logged_with_the_reason() {
        let ring = Arc::new(LogRing::new());
        let pool = Pool::new().with_log(ring.clone());
        // Porta 1 em loopback: ninguém escuta ali, então a recusa é
        // imediata, sem precisar do padrão de "porta fechada" usado em
        // `unreachable_host_names_itself_in_the_error`.
        let host = test_host("Servidor Inacessível", 1);
        let password = "senha-super-secreta";

        let err = pool.get(&host, host.db, Some(password)).await.unwrap_err();

        assert!(
            matches!(err, ConnError::Unreachable { .. }),
            "esperava ConnError::Unreachable, veio {err:?}"
        );

        let snapshot = ring.snapshot();
        let entry = snapshot
            .iter()
            .find(|e| e.operation == "connect")
            .expect("esperava uma entrada de conexão no anel");

        assert_eq!(entry.kind, LogKind::Connection);
        let detail = entry.detail.as_deref().unwrap_or("");
        assert_eq!(
            detail,
            err.to_string(),
            "detail precisa ser o texto já classificado de ConnError, não a URL crua"
        );
        assert!(
            !detail.contains(password),
            "a senha não pode aparecer no detail: {detail}"
        );
    }

    #[tokio::test]
    async fn dropping_a_host_is_logged() {
        let ring = Arc::new(LogRing::new());
        let pool = Pool::new().with_log(ring.clone());
        let mut host = test_host("Redis de teste", TEST_PORT);
        host.db = 13;

        pool.get(&host, host.db, None).await.unwrap();
        pool.drop_host(&host);

        let snapshot = ring.snapshot();
        let entry = snapshot
            .iter()
            .find(|e| e.operation == "disconnect")
            .expect("esperava uma entrada de desconexão no anel");

        assert_eq!(entry.kind, LogKind::Connection);
        assert_eq!(
            entry.host, "Redis de teste",
            "o campo host precisa ser o nome de exibição, não o id interno do host"
        );
    }

    #[tokio::test]
    async fn dropping_a_host_with_nothing_cached_logs_nothing() {
        let ring = Arc::new(LogRing::new());
        let pool = Pool::new().with_log(ring.clone());
        let host = test_host("Host Nunca Conectado", 1);

        pool.drop_host(&host);

        assert!(
            ring.snapshot().is_empty(),
            "não havia nada para descartar, então o anel deveria continuar vazio"
        );
    }

    #[tokio::test]
    async fn a_tls_host_fails_with_an_error_not_a_panic() {
        // O `rustls` exige um provedor de criptografia instalado no
        // processo; sem ele, ele entra em PÂNICO ao montar a conexão, antes
        // de qualquer byte na rede. O `redis` só habilita um provedor nas
        // dependências de teste dele, então quem usa a biblioteca precisa
        // trazer o seu — e durante um bom tempo este app não trouxe, o que
        // deixou TODO host com TLS impossível de conectar.
        //
        // Aqui o alvo é o Redis de teste, que NÃO fala TLS: o handshake tem
        // que falhar com um erro tratado. Se o provedor sumir de novo, este
        // teste volta a quebrar com um pânico em vez de um `Err`.
        let pool = Pool::new();
        let mut host = test_host("Redis de teste", 6399);
        host.tls = true;

        let resultado = pool.ping(&host, None).await;

        assert!(
            resultado.is_err(),
            "um servidor sem TLS não pode aceitar uma conexão TLS"
        );
    }

    #[tokio::test]
    async fn connecting_times_out_when_the_host_never_responds() {
        // Liga numa porta efêmera e aceita a conexão, mas nunca escreve nada
        // nela: simula uma VPN caindo no meio do caminho, onde os pacotes
        // somem sem RST nem FIN. Sem o timeout de conexão, isso travaria
        // `ping` (e a UI) para sempre.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.unwrap();
            // Mantém a conexão aceita (e o socket) vivos, sem responder
            // nada, até a task morrer com o fim do runtime do teste.
            std::future::pending::<()>().await
        });

        let pool = Pool::with_timeouts(Duration::from_millis(200), Duration::from_millis(200));
        let host = test_host("Host Mudo", port);

        let started = Instant::now();
        let err = pool.ping(&host, None).await.unwrap_err();
        let elapsed = started.elapsed();

        match &err {
            ConnError::Unreachable { host: name, .. } => assert_eq!(name, "Host Mudo"),
            other => panic!("esperava ConnError::Unreachable, veio {other:?}"),
        }
        assert!(
            elapsed < Duration::from_secs(1),
            "levou {elapsed:?} — o timeout de 200ms injetado não foi respeitado"
        );
    }

    #[tokio::test]
    async fn ping_evicts_a_dead_cached_connection_and_retries() {
        let pool = Pool::new();
        let host = test_host("Redis de teste", TEST_PORT);

        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let id1: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn)
            .await
            .unwrap();

        // Mata a conexão cacheada a partir de uma segunda conexão (aberta
        // diretamente, fora do pool), simulando uma queda de rede: a
        // conexão em `pool` fica morta, mas ninguém chamou `drop_host`.
        let killer_client =
            redis::Client::open(format!("redis://127.0.0.1:{TEST_PORT}/0")).unwrap();
        let mut killer = killer_client
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let _: () = redis::cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(id1)
            .query_async(&mut killer)
            .await
            .unwrap();

        // `ping` precisa perceber sozinho que a conexão cacheada morreu,
        // descartá-la e abrir uma nova — não basta devolver o erro.
        pool.ping(&host, None).await.unwrap();

        let mut conn2 = pool.get(&host, host.db, None).await.unwrap();
        let id2: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn2)
            .await
            .unwrap();

        assert_ne!(id1, id2, "o pool deveria ter aberto uma conexão nova");
    }

    #[tokio::test]
    async fn run_once_reports_ambiguous_without_retrying_a_dead_connection() {
        // Mesma simulação de queda de rede de `ping_evicts_a_dead_cached_connection_and_retries`
        // logo acima, mas contra `run_once`: ao contrário de `with_retry`,
        // que se cura sozinho e tenta de novo, `run_once` existe para
        // operações não idempotentes (ver `core::value::WriteOp::
        // RemoveListItem`/`PushListItem`) onde reenviar o mesmo comando
        // depois de um erro de classe conexão pode aplicar a operação
        // errada ou duas vezes — então aqui o certo é devolver
        // `ConnError::Ambiguous` e deixar para quem chamou decidir,
        // nunca tentar de novo sozinho.
        let pool = Pool::new();
        let host = test_host("Redis de teste", TEST_PORT);

        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let id1: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn)
            .await
            .unwrap();

        let killer_client =
            redis::Client::open(format!("redis://127.0.0.1:{TEST_PORT}/0")).unwrap();
        let mut killer = killer_client
            .get_multiplexed_async_connection()
            .await
            .unwrap();
        let _: () = redis::cmd("CLIENT")
            .arg("KILL")
            .arg("ID")
            .arg(id1)
            .query_async(&mut killer)
            .await
            .unwrap();

        let result = pool
            .run_once(
                &host,
                host.db,
                None,
                "run-once-probe",
                |mut conn| async move { redis::cmd("PING").query_async::<String>(&mut conn).await },
            )
            .await;

        match result {
            Err(ConnError::Ambiguous { key, .. }) => assert_eq!(key, "run-once-probe"),
            other => panic!("esperava ConnError::Ambiguous, veio {other:?}"),
        }

        // Mesmo sem tentar de novo, `run_once` ainda descarta a conexão
        // morta — a próxima chamada (de qualquer operação) não pode herdar
        // esse socket morto.
        let mut conn2 = pool.get(&host, host.db, None).await.unwrap();
        let id2: i64 = redis::cmd("CLIENT")
            .arg("ID")
            .query_async(&mut conn2)
            .await
            .unwrap();
        assert_ne!(
            id1, id2,
            "run_once deveria ter descartado a conexão morta mesmo sem tentar de novo"
        );
    }
}
