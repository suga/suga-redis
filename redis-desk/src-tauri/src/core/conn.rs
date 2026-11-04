//! Pool de conexões Redis por (host, banco) e as duas chamadas de metadados
//! (ping e tamanho dos bancos) usadas pelo resto da aplicação. Toda conexão
//! e toda resposta tem um timeout de 5 segundos: sem isso, uma VPN caindo
//! no meio travaria a interface indefinidamente.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use redis::aio::MultiplexedConnection;
use redis::{AsyncConnectionConfig, Client};

use super::config::Host;

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
}

impl Default for Pool {
    fn default() -> Pool {
        Pool::new()
    }
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
        }
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
    pub fn drop_host(&self, id: &str) {
        self.connections
            .lock()
            .unwrap()
            .retain(|(host_id, _db), _conn| host_id != id);
    }

    async fn connect(
        &self,
        host: &Host,
        db: u8,
        password: Option<&str>,
    ) -> Result<MultiplexedConnection> {
        let url = build_url(host, db, password);
        let client = Client::open(url).map_err(|err| classify(host, err))?;

        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(self.connection_timeout))
            .set_response_timeout(Some(self.response_timeout));

        client
            .get_multiplexed_async_connection_with_config(&config)
            .await
            .map_err(|err| classify(host, err))
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
                    self.drop_host(&host.id);
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
                    self.drop_host(&host.id);
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
        parse_keyspace(&info)
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
