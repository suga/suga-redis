//! Cópia de segurança do valor anterior de uma chave antes de toda escrita,
//! histórico dessas mudanças e o botão de "volta atrás": grava o valor
//! inteiro (sem truncar) antes de aplicar `WriteOp`, guarda a linha num
//! SQLite compartilhado com o índice de busca da Task 7, e sabe desfazer
//! exatamente o que foi feito — inclusive recriar uma chave apagada, com o
//! TTL que ela tinha. Uma exclusão em massa (`delete_prefix_with_backup`)
//! entra como UMA entrada com os retratos de todas as chaves juntos, na
//! própria linha ou num arquivo à parte.
//!
//! Banco em `<dir de dados do app>/suga-redis.sqlite`, aberto sempre com
//! `PRAGMA journal_mode = WAL` e `PRAGMA busy_timeout = 5000`: o mesmo
//! arquivo é usado pelo índice de busca (Task 7) e, a partir da Task 15, por
//! um segundo processo do SO (o servidor MCP) — sem WAL e um busy_timeout
//! generoso, os dois processos se atropelam com `SQLITE_BUSY` em uso normal.

use std::future::Future;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::bulk::{BulkError, unlink_keys};
use super::config::{Env, Host};
use super::conn::{ConnError, Pool};
use super::keys::{Alvo, KeyKind};
use super::value::{
    BackupKind, Bytes, Value, ValueLimit, ValueView, WriteOp, apply_write, op_summary, read_value,
};

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

/// Teto do tamanho da cópia de segurança de uma exclusão em massa, em bytes
/// do JSON montado. Existe porque a cópia inteira passa pela memória antes
/// de qualquer chave ser apagada: um prefixo com dezenas de milhares de
/// valores gordos não pode virar um `String` do tamanho do banco só porque
/// alguém pediu a cópia. Batido o teto, a exclusão é RECUSADA (ver
/// `delete_prefix_with_backup`) — nunca feita sem a cópia que a pessoa
/// pediu.
pub const MAX_BACKUP_BYTES: usize = 64 * 1024 * 1024;

/// O que a pessoa escolheu no diálogo de confirmação da exclusão em massa:
/// guardar a cópia junto da entrada do histórico, gravá-la num arquivo à
/// parte, ou apagar sem cópia nenhuma.
#[derive(Debug, Clone, Copy)]
pub enum BackupChoice<'a> {
    Inline,
    /// Diretório onde gravar; o nome do arquivo é montado aqui.
    File(&'a Path),
    None,
}

/// Uma exclusão em massa já confirmada: os prefixos que a pessoa escolheu,
/// as chaves que a varredura (`core::bulk::enumerate`) achou sob eles, onde
/// a cópia de segurança deve ficar e a bandeira de cancelamento que a tela
/// levanta se desistir no meio.
pub struct BulkDelete<'a> {
    pub prefixes: &'a [String],
    pub keys: &'a [String],
    pub backup: BackupChoice<'a>,
    pub cancel: &'a AtomicBool,
}

/// O que sobrou de uma exclusão em massa: a entrada única de histórico que
/// a registra e quantas chaves saíram de verdade.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BulkOutcome {
    pub entry_id: i64,
    pub deleted: u64,
}

/// Uma chave que a volta atrás de uma exclusão em massa não conseguiu
/// trazer de volta, com o motivo já em texto — é o que a tela mostra a quem
/// clicou em "desfazer" e precisa saber o que ficou para trás.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedKey {
    pub key: String,
    pub reason: String,
}

/// O que uma volta atrás desfez. As duas formas existem porque as duas
/// entradas são diferentes de verdade: uma escrita de chave única volta
/// inteira ou não volta (a falha sobe como `Err`), enquanto uma exclusão em
/// massa volta chave a chave e pode voltar pela metade.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RollbackOutcome {
    /// A entrada mirava uma chave só, e ela voltou ao que era.
    Key,
    /// A entrada era uma exclusão em massa: QUAIS chaves voltaram e quais
    /// não voltaram, com o motivo de cada uma.
    ///
    /// Os nomes, e não só a contagem: quem chamou precisa repor cada chave
    /// nos caches que a exclusão fez esquecer (a trie da árvore, em
    /// `commands::rollback`), e um número não diz quais repor.
    Bulk {
        restored: Vec<String>,
        failed: Vec<FailedKey>,
    },
}

/// Quem fez a mudança: uma pessoa pela UI, ou a LLM (Task 16) através das
/// ferramentas MCP (Task 15). Guardado em toda entrada do histórico para que
/// a UI possa distinguir e, por exemplo, destacar mudanças feitas pela LLM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub id: i64,
    pub host_id: String,
    pub host_name: String,
    pub env: Env,
    pub db: u8,
    pub key: String,
    pub summary: String,
    /// A operação que gerou a entrada, para a tela montar a frase no idioma
    /// de quem lê. `None` nas entradas gravadas antes desta coluna existir —
    /// nelas só resta a frase em português de `summary`.
    pub op: Option<WriteOp>,
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

    #[error(
        "a chave \"{key}\" mudou de tipo desde a cópia de segurança (era \"{expected}\", agora \
         é \"{found}\") — a volta atrás foi recusada para não apagar dados de um tipo diferente"
    )]
    TypeChanged {
        key: String,
        expected: String,
        found: String,
    },

    #[error(
        "a chave \"{key}\" já tinha expirado sozinha antes da volta atrás — nada foi restaurado"
    )]
    ExpiredBeforeRollback { key: String },

    #[error(
        "a cópia de segurança da exclusão em massa passou do teto de {bytes} bytes — \
         nenhuma chave foi apagada"
    )]
    BackupTooLarge { bytes: usize },

    #[error(
        "a exclusão em massa sob \"{prefix}\" foi feita sem cópia de segurança — não há o que \
         restaurar"
    )]
    NoBackup { prefix: String },

    #[error("falha ao ler o arquivo da cópia de segurança \"{path}\": {source}")]
    BackupUnreadable {
        path: String,
        source: std::io::Error,
    },

    #[error("falha ao gravar o arquivo da cópia de segurança: {0}")]
    BackupIo(std::io::Error),
}

/// Mapeia o erro da abertura compartilhada (`core::sqlite::open_shared`) para
/// as variantes já existentes de `HistoryError` — ela já tinha `Io` e `Sql`
/// com o mesmo sentido, então não precisa de uma variante nova.
impl From<super::sqlite::OpenError> for HistoryError {
    fn from(err: super::sqlite::OpenError) -> HistoryError {
        match err {
            super::sqlite::OpenError::Io(err) => HistoryError::Io(err),
            super::sqlite::OpenError::Sql(err) => HistoryError::Sql(err),
        }
    }
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
    /// Desestrutura `Bytes` por nome, não por acesso a campo (`bytes.text`,
    /// `bytes.base64`): um campo novo em `Bytes` vira erro de compilação
    /// aqui (padrão não-exaustivo) em vez de simplesmente nunca chegar ao
    /// histórico — exatamente o jeito silencioso como esta garantia de
    /// backup pararia de valer. `truncated` é descartado de propósito: com
    /// `FULL_READ_LIMIT`, o backup nunca deveria vir truncado, mas o campo
    /// ainda precisa aparecer no padrão para a exaustividade funcionar.
    fn from_bytes(bytes: &Bytes) -> BytesSnapshot {
        let Bytes {
            text,
            base64,
            truncated: _,
        } = bytes;
        BytesSnapshot {
            text: text.clone(),
            base64: base64.clone(),
        }
    }

    /// Inverso de `from_bytes`: prefere `text` (o caminho comum) e só
    /// decodifica `base64` quando não há texto — mesma regra de
    /// `Bytes::to_raw` em `core::value`, reimplementada aqui porque aquele
    /// método é privado ao módulo `value`. Ao contrário daquele método, um
    /// base64 inválido aqui não pode virar silenciosamente um valor vazio:
    /// um `before_json` corrompido precisa ser recusado com erro, não
    /// restaurado como se a chave estivesse vazia — isso seria perder o
    /// dado duas vezes (uma na corrupção, outra fingindo que não houve
    /// nada para restaurar).
    fn to_raw(&self) -> Result<Vec<u8>> {
        if let Some(text) = &self.text {
            Ok(text.as_bytes().to_vec())
        } else if let Some(encoded) = &self.base64 {
            BASE64.decode(encoded).map_err(|err| {
                HistoryError::Corrupt(format!("base64 inválido no histórico: {err}"))
            })
        } else {
            Ok(Vec::new())
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
    /// Instante absoluto (epoch em milissegundos) em que a chave expira, ou
    /// `None` se ela era persistente. Guardado como instante, não como TTL
    /// relativo (`PTTL`) no momento da cópia: um TTL relativo reaplicado
    /// depois, via `PEXPIRE`, conta a partir de agora — o que faria uma
    /// volta atrás tardia esticar a vida real da chave em vez de restaurar
    /// o prazo que ela já tinha. Ver `restore_value`/`build_restore_command`,
    /// que usam `PEXPIREAT` com este instante.
    expires_at_ms: Option<i64>,
    elements: Vec<ElementSnapshot>,
}

/// Um retrato de `ValueSnapshot` com o nome da chave junto — a forma de
/// cada item da cópia de uma exclusão em massa, onde um `before_json`
/// guarda N chaves em vez de uma. `flatten` deixa o JSON de cada item com a
/// mesma forma do retrato de uma chave só, mais o campo `key`: quem for
/// restaurar (Tarefa 5) lê o mesmo formato nos dois caminhos.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct KeySnapshot {
    key: String,
    #[serde(flatten)]
    value: ValueSnapshot,
}

/// Converte o valor lido (`ValueView`) para a forma serializável do
/// histórico. `now_ms` é o instante da leitura (não um instante recalculado
/// aqui dentro), usado só para transformar o TTL relativo de `PTTL` em um
/// instante absoluto — ver o comentário de `ValueSnapshot::expires_at_ms`.
///
/// Desestruturado por nome (`let ValueView { .. } = view`), não por acesso a
/// campo: um campo novo em `ValueView` vira erro de compilação aqui em vez
/// de silenciosamente nunca chegar ao backup.
fn snapshot_from_view(view: &ValueView, now_ms: i64) -> ValueSnapshot {
    let ValueView {
        key: _,
        kind: _,
        ttl_ms,
        size_bytes: _,
        value,
        total_elements: _,
        truncated: _,
    } = view;
    let expires_at_ms = ttl_ms.map(|remaining_ms| now_ms + remaining_ms);
    let (kind, elements) = match value {
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
        expires_at_ms,
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
        other => Err(HistoryError::Corrupt(format!(
            "actor desconhecido: {other}"
        ))),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("relógio do sistema antes de 1970")
        .as_millis() as i64
}

/// Lê o valor inteiro de cada chave de `keys` e devolve os retratos já
/// montados como o JSON que vai para `before_json` (ou para o arquivo da
/// cópia). Um JSON por vez, com o tamanho somado a cada retrato: assim o
/// teto recusa a cópia grande demais SEM antes carregar tudo na memória —
/// que é justamente o que ele existe para evitar.
///
/// Uma chave que sumiu entre a varredura e a cópia (expirou, ou outra
/// conexão apagou) é pulada em silêncio: não há retrato a tirar de algo que
/// já não existe, e isso não é motivo para recusar a exclusão inteira.
async fn read_snapshots(
    pool: &Pool,
    alvo: Alvo<'_>,
    keys: &[String],
    now_ms: i64,
    max_bytes: usize,
) -> Result<String> {
    let Alvo { host, pw, db } = alvo;
    let mut parts: Vec<String> = Vec::with_capacity(keys.len());
    // Os dois colchetes do vetor já contam para o teto.
    let mut bytes = 2usize;

    for key in keys {
        let view = match read_value(pool, host, pw, db, key, FULL_READ_LIMIT).await {
            Ok(view) => view,
            Err(ConnError::KeyGone(_)) => continue,
            Err(err) => return Err(HistoryError::Conn(err)),
        };
        let part = serde_json::to_string(&KeySnapshot {
            key: key.clone(),
            value: snapshot_from_view(&view, now_ms),
        })?;
        // +1 pela vírgula que separa este retrato do anterior.
        bytes += part.len() + 1;
        if bytes > max_bytes {
            return Err(HistoryError::BackupTooLarge { bytes });
        }
        parts.push(part);
    }

    Ok(format!("[{}]", parts.join(",")))
}

/// `AAAA-MM-DD-hhmmss-<host>-db<N>-<prefixo>.json`, com host e prefixo
/// saneados para nome de arquivo. Só o PRIMEIRO prefixo entra no nome: um
/// pedido com vários deixaria o nome tão longo quanto a operação que
/// descreve, e a lista completa já está no `op_json` da entrada.
fn backup_file_name(host: &Host, db: u8, prefixes: &[String], at_ms: i64) -> String {
    let prefix = prefixes.first().map(String::as_str).unwrap_or("todas");
    format!(
        "{}-{}-db{}-{}.json",
        timestamp_for_name(at_ms),
        sanitize_for_name(&host.name),
        db,
        sanitize_for_name(prefix),
    )
}

/// Tudo que não for letra ASCII, dígito, `-` ou `_` vira `_`, e só então o
/// resultado é cortado em 40 caracteres: o nome do host e o prefixo são
/// texto de pessoa (`:`, `/`, acento, espaço) e entram num nome de arquivo.
///
/// O corte é do NOME, não do texto de entrada — cortar antes de sanear
/// mediria um comprimento que não é o que vai para o disco. Ele também
/// significa que dois prefixos com os mesmos 40 primeiros caracteres caem no
/// mesmo nome; quem garante que eles não caem no mesmo ARQUIVO é
/// `write_backup_file`, não este saneamento.
fn sanitize_for_name(text: &str) -> String {
    let clean: String = text
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    if clean.is_empty() {
        "_".to_string()
    } else {
        clean
    }
}

/// Quantos nomes `write_backup_file` tenta antes de desistir. Alto o
/// bastante para qualquer colisão real (mesmo host, mesmo banco, mesmo
/// segundo), baixo o bastante para um diretório inesperadamente cheio virar
/// erro em vez de laço infinito.
const MAX_BACKUP_FILE_TRIES: u32 = 1_000;

/// Grava `json` em `dir` com o nome `name`, ou — se já houver um arquivo
/// com esse nome — com `-2`, `-3` … antes do `.json`. Devolve o caminho
/// realmente usado.
///
/// `create_new` em vez de `std::fs::write` porque `write` TRUNCA: dois
/// pedidos com o mesmo host, banco, prefixo e segundo montam o mesmo nome, e
/// o segundo apagaria a cópia do primeiro — bem na hora em que ela mais
/// importa, a da repetição depois de uma falha parcial. Dois prefixos
/// diferentes que saneiam igual (`app:cache:` e `app/cache/`), ou que
/// compartilham os 40 primeiros caracteres, colidem pelo mesmo caminho. O
/// `create_new` recusa a sobrescrita no próprio sistema de arquivos, sem
/// janela entre conferir e gravar.
fn write_backup_file(dir: &Path, name: &str, json: &str) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write as _;

    std::fs::create_dir_all(dir)?;
    let stem = name.strip_suffix(".json").unwrap_or(name);

    for attempt in 1..=MAX_BACKUP_FILE_TRIES {
        let path = dir.join(if attempt == 1 {
            name.to_string()
        } else {
            format!("{stem}-{attempt}.json")
        });
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(json.as_bytes())?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        format!("já existem {MAX_BACKUP_FILE_TRIES} cópias com o nome \"{name}\" neste diretório"),
    ))
}

/// `AAAA-MM-DD-hhmmss` em UTC, a partir do epoch em milissegundos.
///
/// UTC, e não a hora de quem clicou: não há dependência de calendário neste
/// binário, e o nome do arquivo só precisa ordenar e não se repetir. O
/// instante local da exclusão continua gravado em `at`, na entrada do
/// histórico.
fn timestamp_for_name(at_ms: i64) -> String {
    let seconds = at_ms.div_euclid(1000);
    let (year, month, day) = civil_from_days(seconds.div_euclid(86_400));
    let in_day = seconds.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02}-{:02}{:02}{:02}",
        in_day / 3600,
        (in_day % 3600) / 60,
        in_day % 60,
    )
}

/// Ano, mês e dia do calendário gregoriano a partir do número de dias desde
/// 1970-01-01 — o `civil_from_days` de Howard Hinnant, que desloca a época
/// para 0000-03-01 para que o dia bissexto caia no FIM do ciclo de 400 anos
/// e a conta vire aritmética inteira sem tabela de meses.
fn civil_from_days(days: i64) -> (i64, u64, u64) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// A coluna `key` de uma entrada de exclusão em massa. Ela mira prefixos,
/// não uma chave — fica o primeiro prefixo, que é o que a lista do
/// histórico mostra; a lista inteira está no `op_json`.
fn bulk_entry_key(prefixes: &[String]) -> &str {
    prefixes.first().map(String::as_str).unwrap_or("")
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
        let conn = super::sqlite::open_shared(db_path)?;
        conn.execute_batch(SCHEMA)?;
        // Bancos criados antes desta coluna existirem continuam abrindo: o
        // erro de "duplicate column name" é o único sinal que o SQLite dá
        // de que a migração já rodou, e ignorá-lo é o idioma da casa.
        let _ = conn.execute("ALTER TABLE history ADD COLUMN op_json TEXT", []);

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
        op_json: Option<&str>,
        before_json: Option<&str>,
        actor: Actor,
        at: i64,
    ) -> Result<i64> {
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO history
                (host_id, host_name, env, db, key, summary, op_json, before_json, after_json, actor, at, rolled_back)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10, 0)",
            params![
                host.id,
                host.name,
                env_to_str(host.env),
                db as i64,
                key,
                summary,
                op_json,
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

    /// Regrava a operação (e a frase que a descreve) de uma entrada já
    /// inserida. Só a exclusão em massa precisa disto: a contagem real de
    /// chaves apagadas só existe DEPOIS do `UNLINK`, e a linha é gravada
    /// antes dele — ver `delete_prefix_com_teto`.
    fn update_op(&self, id: i64, key: &str, op: &WriteOp) -> Result<()> {
        let summary = op_summary(key, op);
        let op_json = serde_json::to_string(op)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "UPDATE history SET summary = ?1, op_json = ?2 WHERE id = ?3",
            params![summary, op_json, id],
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
        // Um único instante serve tanto para converter o TTL lido (relativo)
        // em `expires_at_ms` (absoluto) quanto para `at`: os dois descrevem
        // o mesmo momento, o da cópia de segurança.
        let now = now_unix_ms();
        let before = match read_value(pool, host, pw, db, key, FULL_READ_LIMIT).await {
            Ok(view) => Some(snapshot_from_view(&view, now)),
            Err(ConnError::KeyGone(_)) => None,
            Err(err) => return Err(HistoryError::Conn(err)),
        };
        let before_json = match &before {
            Some(snapshot) => Some(serde_json::to_string(snapshot)?),
            None => None,
        };

        let summary = op_summary(key, op);
        // A operação vai junto da frase: a frase é o registro do que foi
        // feito, a operação é o que deixa a tela redizê-lo noutro idioma.
        let op_json = serde_json::to_string(op).ok();
        let id = self.insert_entry(
            host,
            db,
            key,
            &summary,
            op_json.as_deref(),
            before_json.as_deref(),
            actor,
            now,
        )?;

        if let Err(err) = apply_write(pool, host, pw, db, key, op).await {
            self.delete_row(id)?;
            return Err(HistoryError::Conn(err));
        }

        let after_json = match read_value(pool, host, pw, db, key, FULL_READ_LIMIT).await {
            Ok(view) => serde_json::to_string(&snapshot_from_view(&view, now_unix_ms())).ok(),
            Err(_) => None,
        };
        self.update_after_json(id, after_json.as_deref())?;

        Ok(id)
    }

    /// Apaga toda chave de `plan.keys` e registra a exclusão inteira como
    /// UMA entrada de histórico, com os retratos de todas elas na cópia de
    /// segurança — a irmã em massa de `write_with_backup`.
    ///
    /// A ordem é a garantia do método, não um detalhe de implementação: lê
    /// os retratos, **confere o teto**, grava a linha, apaga e só então
    /// corrige a linha com a contagem real. Conferir o teto depois de
    /// apagar faria a recusa chegar com as chaves já no chão e sem cópia.
    ///
    /// `BackupChoice::None` pula a leitura dos retratos inteira — é o que
    /// torna rápido o caminho de quem decidiu apagar sem cópia; lê-los para
    /// jogar fora seria o pior dos dois mundos.
    ///
    /// O ator é sempre `Human`: a LLM não tem ferramenta de exclusão em
    /// massa, e se um dia tiver, ela passa por aqui com o ator explícito,
    /// não por omissão.
    pub async fn delete_prefix_with_backup(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        db: u8,
        plan: BulkDelete<'_>,
        progress: impl Fn(u64),
    ) -> Result<BulkOutcome> {
        let alvo = Alvo { host, pw, db };
        let keys = plan.keys;
        let cancel = plan.cancel;

        self.delete_prefix_com_teto(pool, alvo, plan, MAX_BACKUP_BYTES, move || {
            unlink_keys(pool, alvo, keys, cancel, progress)
        })
        .await
    }

    /// `delete_prefix_with_backup` com o teto como parâmetro, para o teste
    /// do caso de corte não precisar criar 64 MiB de valor — mesmo padrão
    /// de `bulk::enumerate_com_teto`.
    ///
    /// `apagar` é a costura que de fato apaga `plan.keys`. A pública passa
    /// `bulk::unlink_keys`; o teste passa um fechamento que forja a conexão
    /// caindo no meio, que é a única forma de provar que o parcial de
    /// `BulkError` chega à linha ANTES de o erro subir.
    async fn delete_prefix_com_teto<F, Fut>(
        &self,
        pool: &Pool,
        alvo: Alvo<'_>,
        plan: BulkDelete<'_>,
        max_backup_bytes: usize,
        apagar: F,
    ) -> Result<BulkOutcome>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<u64, BulkError>>,
    {
        let Alvo { host, db, .. } = alvo;
        // Um único instante serve para converter os TTLs lidos (relativos)
        // em instantes absolutos, para o `at` da entrada e para o nome do
        // arquivo da cópia: os três descrevem o mesmo momento.
        let now = now_unix_ms();

        // Primeiro os retratos e o teto — tudo que pode recusar a operação
        // acontece antes de a primeira chave sair.
        let (backup, before_json) = match plan.backup {
            BackupChoice::None => (BackupKind::None, None),
            BackupChoice::Inline => {
                let json = read_snapshots(pool, alvo, plan.keys, now, max_backup_bytes).await?;
                (BackupKind::Inline, Some(json))
            }
            BackupChoice::File(dir) => {
                let json = read_snapshots(pool, alvo, plan.keys, now, max_backup_bytes).await?;
                let name = backup_file_name(host, db, plan.prefixes, now);
                let path = write_backup_file(dir, &name, &json).map_err(HistoryError::BackupIo)?;
                (
                    BackupKind::File {
                        path: path.to_string_lossy().into_owned(),
                    },
                    None,
                )
            }
        };

        let key = bulk_entry_key(plan.prefixes);
        let enumerated = plan.keys.len() as u64;
        let entry_op = |deleted: u64| WriteOp::DeletePrefix {
            prefixes: plan.prefixes.to_vec(),
            deleted,
            enumerated,
            backup: backup.clone(),
        };

        // A linha nasce com `deleted: 0` — é o número verdadeiro enquanto o
        // `UNLINK` não rodou. Nascer com a contagem pedida faria a linha
        // prometer, durante toda a exclusão, uma exclusão que ainda não
        // aconteceu.
        let op = entry_op(0);
        let id = self.insert_entry(
            host,
            db,
            key,
            &op_summary(key, &op),
            serde_json::to_string(&op).ok().as_deref(),
            before_json.as_deref(),
            Actor::Human,
            now,
        )?;

        match apagar().await {
            Ok(deleted) => {
                self.update_op(id, key, &entry_op(deleted))?;
                Ok(BulkOutcome {
                    entry_id: id,
                    deleted,
                })
            }
            // Nada saiu: a linha registraria uma mudança que não aconteceu,
            // e isso é pior do que não ter linha — mesma decisão de
            // `write_with_backup`.
            Err(err) if err.deleted == 0 => {
                self.delete_row(id)?;
                Err(HistoryError::Conn(err.source))
            }
            // O que já saiu, saiu. A linha precisa dizer o número de
            // verdade ANTES de o erro subir, senão o histórico promete de
            // volta chaves que não existem mais.
            Err(err) => {
                self.update_op(id, key, &entry_op(err.deleted))?;
                Err(HistoryError::Conn(err.source))
            }
        }
    }

    /// As `limit` entradas mais recentes do host, mais novas primeiro.
    pub fn list(&self, host_id: &str, limit: usize) -> Result<Vec<HistoryEntry>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, host_id, host_name, env, db, key, summary, op_json, before_json, after_json, actor, at, rolled_back
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
            "SELECT id, host_id, host_name, env, db, key, summary, op_json, before_json, after_json, actor, at, rolled_back
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
    ///
    /// O despacho é pela OPERAÇÃO da entrada: uma exclusão em massa volta
    /// pelo caminho de `rollback_bulk` (um retrato por chave), e todo o
    /// resto — inclusive as entradas antigas, gravadas antes de a coluna
    /// `op_json` existir, que não têm operação nenhuma — segue o caminho de
    /// chave única de sempre.
    ///
    /// Note que a exclusão em massa NÃO é desfeita aplicando a operação
    /// inversa: `WriteOp::DeletePrefix` não passa por `apply_write` (ver o
    /// comentário dessa variante em `core::value`). Restaurar é repor cada
    /// retrato pelo mesmo `restore_value` que desfaz uma escrita de chave
    /// única.
    pub async fn rollback(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        entry_id: i64,
    ) -> Result<RollbackOutcome> {
        let entry = self.get(entry_id)?;

        if let Some(WriteOp::DeletePrefix { backup, .. }) = &entry.op {
            return self.rollback_bulk(pool, host, pw, &entry, backup).await;
        }

        let snapshot = match &entry.before_json {
            Some(json) => Some(serde_json::from_str::<ValueSnapshot>(json)?),
            None => None,
        };

        // Só precisamos saber o tipo criado quando não há snapshot de
        // "antes" — ver o comentário de `restore_value`. `after_json` é
        // best-effort (ver `write_with_backup`), então um JSON ausente ou
        // corrompido aqui vira `None` (a checagem de tipo é pulada), não um
        // erro que travaria a volta atrás inteira.
        let created_kind = if snapshot.is_none() {
            entry
                .after_json
                .as_deref()
                .and_then(|json| serde_json::from_str::<ValueSnapshot>(json).ok())
                .map(|snap| snap.kind)
        } else {
            None
        };

        restore_value(
            pool,
            host,
            pw,
            entry.db,
            &entry.key,
            &snapshot,
            created_kind.as_deref(),
        )
        .await?;
        self.mark_rolled_back(entry_id)?;

        Ok(RollbackOutcome::Key)
    }

    /// Repõe, uma a uma, as chaves que uma exclusão em massa apagou — a
    /// volta atrás de `delete_prefix_with_backup`.
    ///
    /// Melhor esforço por chave, na ordem em que os retratos foram
    /// gravados: uma chave que não volta (tipo sem suporte, retrato
    /// corrompido, tipo trocado desde a cópia) entra em `failed` com o
    /// motivo e o laço SEGUE. Abortar no primeiro erro deixaria o resto do
    /// lote no chão por causa de uma chave só.
    ///
    /// `rolled_back` só é marcado quando `failed` está vazio: um "desfeito"
    /// que repôs 900 de 1.395 é mentira, e a pessoa agiria em cima dela.
    async fn rollback_bulk(
        &self,
        pool: &Pool,
        host: &Host,
        pw: Option<&str>,
        entry: &HistoryEntry,
        backup: &BackupKind,
    ) -> Result<RollbackOutcome> {
        let snapshots = bulk_snapshots(entry, backup)?;

        let mut restored: Vec<String> = Vec::new();
        let mut failed: Vec<FailedKey> = Vec::new();
        for KeySnapshot { key, value } in snapshots {
            match restore_value(pool, host, pw, entry.db, &key, &Some(value), None).await {
                Ok(()) => restored.push(key),
                Err(err) => failed.push(FailedKey {
                    key,
                    reason: err.to_string(),
                }),
            }
        }

        if failed.is_empty() {
            self.mark_rolled_back(entry.id)?;
        }

        Ok(RollbackOutcome::Bulk { restored, failed })
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
    op_json: Option<String>,
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
            // Uma operação que não decodifica não derruba a leitura do
            // histórico: a entrada ainda vale pelo resto do que guarda.
            op: self
                .op_json
                .as_deref()
                .and_then(|j| serde_json::from_str(j).ok()),
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
        op_json: row.get(7)?,
        before_json: row.get(8)?,
        after_json: row.get(9)?,
        actor: row.get(10)?,
        at: row.get(11)?,
        rolled_back: row.get::<_, i64>(12)? != 0,
    })
}

/// Descrição em português de um `KeyKind`, para a mensagem de
/// `HistoryError::TypeChanged`.
fn describe_kind(kind: &KeyKind) -> String {
    match kind {
        KeyKind::String => "string".to_string(),
        KeyKind::Hash => "hash".to_string(),
        KeyKind::List => "list".to_string(),
        KeyKind::ZSet => "zset".to_string(),
        KeyKind::Set => "set".to_string(),
        KeyKind::Stream => "stream".to_string(),
        KeyKind::Json => "json (RedisJSON)".to_string(),
        KeyKind::Unknown(name) => format!("desconhecido (\"{name}\")"),
    }
}

/// Se `current` (o tipo que a chave tem HOJE no Redis) é compatível com
/// `snapshot_kind` (o tipo que ela tinha no momento da cópia de segurança).
/// Só os quatro tipos editáveis entram aqui — `snapshot_kind` nunca é outra
/// coisa numa entrada que sobreviveu (ver o comentário de `ValueSnapshot`).
fn kind_matches(current: &KeyKind, snapshot_kind: &str) -> bool {
    matches!(
        (current, snapshot_kind),
        (KeyKind::String, "string")
            | (KeyKind::Hash, "hash")
            | (KeyKind::List, "list")
            | (KeyKind::ZSet, "zset")
    )
}

/// O tipo atual de `key` no Redis, ou `None` se ela não existe. Usado por
/// `restore_value` para recusar uma volta atrás que destruiria uma chave
/// que mudou de tipo depois do backup.
async fn current_kind(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    key: &str,
) -> Result<Option<KeyKind>> {
    let type_name: String = pool
        .with_retry(host, db, pw, |mut conn| async move {
            redis::cmd("TYPE").arg(key).query_async(&mut conn).await
        })
        .await
        .map_err(HistoryError::Conn)?;

    if type_name == "none" {
        Ok(None)
    } else {
        Ok(Some(KeyKind::from_type_name(&type_name)))
    }
}

/// Recusa a volta atrás se `key` existir hoje com um tipo diferente de
/// `expected_kind` — compartilhado pelos dois ramos de `restore_value`
/// (chave que já existia antes da escrita rastreada, e chave que a escrita
/// rastreada criou do zero): nos dois casos, apagar uma chave que mudou de
/// tipo depois do momento capturado destruiria um dado não relacionado só
/// para "desfazer" algo que já não é mais verdade. Se a chave não existir
/// mais, não há nada a recusar — não há dado alheio para proteger.
async fn ensure_type_unchanged(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    key: &str,
    expected_kind: &str,
) -> Result<()> {
    if let Some(current) = current_kind(pool, host, pw, db, key).await?
        && !kind_matches(&current, expected_kind)
    {
        return Err(HistoryError::TypeChanged {
            key: key.to_string(),
            expected: expected_kind.to_string(),
            found: describe_kind(&current),
        });
    }
    Ok(())
}

/// O que recriar para restaurar uma chave — já com todo byte decodificado,
/// para que montar o `Pipeline` (`build_restore_pipeline`) nunca falhe:
/// decodificar base64 é a única parte fallível (`before_json` corrompido),
/// e precisa acontecer antes do fechamento passado a `Pool::with_retry`, que
/// só pode devolver `redis::RedisError` — não `HistoryError`.
enum RestoreCommand {
    /// Chave não existia antes da escrita original — restaurar é só apagar.
    None,
    SetString(Vec<u8>),
    Hash(Vec<(String, Vec<u8>)>),
    List(Vec<Vec<u8>>),
    ZSet(Vec<(f64, Vec<u8>)>),
}

/// Converte `snapshot` em `RestoreCommand` + o instante de expiração (se
/// houver), decodificando todo base64 agora — ver o comentário de
/// `RestoreCommand`. Erro se `snapshot.kind` não for um dos quatro tipos
/// editáveis (não deveria acontecer numa entrada que sobreviveu, ver o
/// comentário de `ValueSnapshot`) ou se algum `base64` estiver corrompido.
fn build_restore_command(
    snapshot: &Option<ValueSnapshot>,
) -> Result<(RestoreCommand, Option<i64>)> {
    let Some(snap) = snapshot else {
        return Ok((RestoreCommand::None, None));
    };

    let command = match snap.kind.as_str() {
        "string" => {
            let raw = match snap.elements.first() {
                Some(element) => element.value.to_raw()?,
                None => Vec::new(),
            };
            RestoreCommand::SetString(raw)
        }
        "hash" => {
            let mut pairs = Vec::with_capacity(snap.elements.len());
            for element in &snap.elements {
                pairs.push((
                    element.field.clone().unwrap_or_default(),
                    element.value.to_raw()?,
                ));
            }
            RestoreCommand::Hash(pairs)
        }
        "list" => {
            let mut items = Vec::with_capacity(snap.elements.len());
            for element in &snap.elements {
                items.push(element.value.to_raw()?);
            }
            RestoreCommand::List(items)
        }
        "zset" => {
            let mut pairs = Vec::with_capacity(snap.elements.len());
            for element in &snap.elements {
                pairs.push((element.score.unwrap_or(0.0), element.value.to_raw()?));
            }
            RestoreCommand::ZSet(pairs)
        }
        other => return Err(HistoryError::UnsupportedKind(other.to_string())),
    };

    Ok((command, snap.expires_at_ms))
}

/// Os retratos guardados pela cópia de uma exclusão em massa, na ordem em
/// que foram gravados.
///
/// Quem decide de onde (e se) eles saem é a CÓPIA — `BackupKind` e o
/// `before_json` da linha —, nunca a contagem `deleted` da operação. Uma
/// queda no meio do `UNLINK` deixa `deleted` em 0 na linha para sempre,
/// enquanto a cópia que ela carrega está inteira e é perfeitamente usável:
/// recusar a restauração por causa daquele número seria virar as costas
/// para o backup que está ali.
fn bulk_snapshots(entry: &HistoryEntry, backup: &BackupKind) -> Result<Vec<KeySnapshot>> {
    let json = match backup {
        // Recusar é um desfecho de verdade, não uma falha a disfarçar: a
        // pessoa escolheu apagar sem cópia e a mensagem diz exatamente
        // isso. Nada é escrito no Redis.
        BackupKind::None => {
            return Err(HistoryError::NoBackup {
                prefix: entry.key.clone(),
            });
        }
        BackupKind::Inline => entry.before_json.clone().ok_or_else(|| {
            HistoryError::Corrupt(format!(
                "a entrada {} diz ter cópia embutida, mas não há retrato nenhum nela",
                entry.id
            ))
        })?,
        BackupKind::File { path } => {
            std::fs::read_to_string(path).map_err(|err| HistoryError::BackupUnreadable {
                path: path.clone(),
                source: err,
            })?
        }
    };

    Ok(serde_json::from_str(&json)?)
}

/// Restaura `key` para o estado descrito por `snapshot` (ou apaga a chave,
/// se `snapshot` é `None` — ela não existia antes da escrita original, e
/// "desfazer" é apagar o que a escrita criou).
///
/// `created_kind` só é usado quando `snapshot` é `None`: é o tipo que a
/// própria escrita rastreada criou (vem de `after_json`, em `rollback`) —
/// precisamos dele para a checagem de tipo abaixo, já que não há um
/// `snapshot.kind` para comparar nesse ramo. Ignorado quando `snapshot` é
/// `Some` (o tipo vem do próprio snapshot nesse caso).
///
/// Checagens de segurança acontecem ANTES de qualquer escrita, nos dois
/// ramos:
///
/// 1. O tipo atual da chave (se ela existir) precisa bater com o tipo
///    esperado — do snapshot, se havia um valor antes, ou de `created_kind`,
///    se a escrita criou a chave do zero. Uma chave que mudou de tipo depois
///    do momento capturado (plausível a partir das Tasks 15/16, quando a
///    LLM também escreve) não pode ser apagada só para a volta atrás
///    recriar (ou simplesmente remover) um tipo diferente por cima. Uma rede
///    de segurança que destrói dado para restaurar dado não é rede de
///    segurança — isso vale tanto para desfazer uma edição quanto para
///    desfazer uma criação.
/// 2. Só no ramo de `snapshot` presente: se ele tinha TTL, o instante de
///    expiração (`expires_at_ms`, absoluto — não um TTL relativo recontado
///    a partir de agora, ver o comentário de `ValueSnapshot::expires_at_ms`)
///    precisa ainda estar no futuro. Se já passou, a chave expirou sozinha
///    em produção entre a escrita original e a volta atrás tardia —
///    recriá-la ressuscitaria um dado que o próprio Redis já tinha
///    descartado.
///
/// Só depois dessas checagens o Redis é tocado: `DEL` seguido da recriação
/// do tipo certo, tudo num `MULTI`/`EXEC` (`Pipeline::atomic`) para que
/// nenhuma outra escrita concorrente veja a chave num estado intermediário
/// (apagada, ou só parcialmente recriada). Passa por `Pool::with_retry`: a
/// sequência inteira é idempotente (rodar `DEL` + recriação de novo produz
/// exatamente o mesmo estado final), ao contrário das operações de lista de
/// `apply_write` que usam `run_once`.
async fn restore_value(
    pool: &Pool,
    host: &Host,
    pw: Option<&str>,
    db: u8,
    key: &str,
    snapshot: &Option<ValueSnapshot>,
    created_kind: Option<&str>,
) -> Result<()> {
    if let Some(snap) = snapshot {
        ensure_type_unchanged(pool, host, pw, db, key, &snap.kind).await?;

        if let Some(expires_at_ms) = snap.expires_at_ms
            && expires_at_ms <= now_unix_ms()
        {
            return Err(HistoryError::ExpiredBeforeRollback {
                key: key.to_string(),
            });
        }
    } else if let Some(expected_kind) = created_kind {
        // `created_kind` é `None` quando `after_json` não estava disponível
        // (a releitura de "melhor esforço" em `write_with_backup` falhou) —
        // sem saber que tipo foi criado, não há como checar com segurança, e
        // a checagem é pulada: lacuna conhecida e documentada, não um bug
        // silencioso novo (ver o relatório da task).
        ensure_type_unchanged(pool, host, pw, db, key, expected_kind).await?;
    }

    let (command, expires_at_ms) = build_restore_command(snapshot)?;
    let key_owned = key.to_string();

    pool.with_retry(host, db, pw, move |mut conn| {
        let pipe = build_restore_pipeline(&key_owned, &command, expires_at_ms);
        async move { pipe.query_async::<()>(&mut conn).await }
    })
    .await
    .map_err(HistoryError::Conn)
}

/// Monta o `Pipeline` a partir de um `RestoreCommand` já resolvido — etapa
/// infalível de propósito (ver o comentário de `RestoreCommand`), para que
/// possa rodar dentro do fechamento de `Pool::with_retry` sem precisar
/// devolver `HistoryError` dali.
fn build_restore_pipeline(
    key: &str,
    command: &RestoreCommand,
    expires_at_ms: Option<i64>,
) -> redis::Pipeline {
    let mut pipe = redis::pipe();
    pipe.atomic();
    pipe.cmd("DEL").arg(key);

    match command {
        RestoreCommand::None => {}
        RestoreCommand::SetString(raw) => {
            pipe.cmd("SET").arg(key).arg(raw.clone());
        }
        RestoreCommand::Hash(pairs) => {
            if !pairs.is_empty() {
                let cmd = pipe.cmd("HSET");
                cmd.arg(key);
                for (field, value) in pairs {
                    cmd.arg(field.clone());
                    cmd.arg(value.clone());
                }
            }
        }
        RestoreCommand::List(items) => {
            if !items.is_empty() {
                let cmd = pipe.cmd("RPUSH");
                cmd.arg(key);
                for item in items {
                    cmd.arg(item.clone());
                }
            }
        }
        RestoreCommand::ZSet(pairs) => {
            if !pairs.is_empty() {
                let cmd = pipe.cmd("ZADD");
                cmd.arg(key);
                for (score, member) in pairs {
                    cmd.arg(*score);
                    cmd.arg(member.clone());
                }
            }
        }
    }

    // Instante absoluto, não relativo — ver o comentário de
    // `ValueSnapshot::expires_at_ms`. Já validado no futuro por quem chamou
    // (`restore_value`), antes de chegar aqui.
    if let Some(expires_at_ms) = expires_at_ms {
        pipe.cmd("PEXPIREAT").arg(key).arg(expires_at_ms);
    }

    pipe
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bulk::UNLINK_BATCH;
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
        let history = History::open(&dir.path().join("suga-redis.sqlite")).unwrap();
        (dir, history)
    }

    #[tokio::test]
    async fn a_entrada_guarda_a_operacao_e_nao_so_a_frase() {
        // A frase nasce aqui em português; quem a mostra é a tela, no
        // idioma de quem está lendo. Para isso ela precisa da operação, e
        // não do texto pronto — que fica como registro do que foi feito.
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:guarda_a_operacao";
        del(&mut conn, &[key]).await;

        let (_dir, history) = open_history();
        let op = WriteOp::SetHashField {
            field: "saldo".to_string(),
            value: text("10"),
        };

        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        let entry = history.get(id).unwrap();
        let guardada = entry.op.expect("a entrada precisa carregar a operação");
        match guardada {
            WriteOp::SetHashField { field, .. } => assert_eq!(field, "saldo"),
            outra => panic!("operação errada no histórico: {outra:?}"),
        }
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

        let current: String = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        assert_eq!(
            current, "valor-novo",
            "a escrita real precisa ter sido aplicada"
        );

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

        assert_eq!(
            restored1, "valor1",
            "campo alterado precisa voltar ao valor original"
        );
        assert_eq!(
            restored2, "valor2",
            "campo não envolvido na escrita precisa sobreviver à volta atrás"
        );

        let entry = history.get(id).unwrap();
        assert!(
            entry.rolled_back,
            "a entrada precisa ser marcada como revertida"
        );
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

        let value: String = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        let pttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();

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
        let huge_value: String = (0..600_000)
            .map(|i| (b'0' + (i % 10) as u8) as char)
            .collect();
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
        let after_write: String = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(after_write, "valor-pequeno-substituto");

        history.rollback(&pool, &host, None, id).await.unwrap();

        let restored: String = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();

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

    #[tokio::test]
    async fn rollback_restores_the_shortened_ttl_after_elapsed_time() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_elapsed_ttl";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-com-ttl-curto")
            .query_async(&mut conn)
            .await
            .unwrap();
        // 5s de TTL no momento do backup.
        let _: () = redis::cmd("PEXPIRE")
            .arg(key)
            .arg(5000)
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

        // Passa um tempo real antes de desfazer — é exatamente o cenário
        // que o produto existe para cobrir: a pessoa só percebe o problema
        // e clica em "desfazer" bem depois da escrita.
        tokio::time::sleep(std::time::Duration::from_millis(2000)).await;

        history.rollback(&pool, &host, None, id).await.unwrap();

        let pttl: i64 = redis::cmd("PTTL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();

        del(&mut conn, &[key]).await;

        // Se a volta atrás tivesse regravado um TTL RELATIVO (`PEXPIRE
        // 5000`) em vez do instante absoluto capturado no backup, o PTTL
        // aqui viria perto de 5000ms de novo — esticando a vida real da
        // chave pelo tempo que a pessoa levou para desfazer. O valor
        // correto é o que sobrava dos 5s originais menos os ~2s que
        // passaram: bem abaixo de 5000, mas ainda positivo.
        assert!(
            pttl > 0 && pttl < 4000,
            "esperava TTL restante (~3000ms, não os 5000ms originais), veio {pttl}"
        );
    }

    #[tokio::test]
    async fn rollback_of_an_already_expired_key_restores_nothing() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_already_expired";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-vai-expirar")
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("PEXPIRE")
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

        // A escrita rastreada (`DeleteKey`) já apagou a chave antes deste
        // sleep — não é o próprio Redis que a expira aqui, já não há nada
        // para expirar. O sleep só empurra o relógio de parede para além do
        // `expires_at_ms` (300ms) guardado no backup, para exercitar a
        // checagem `expires_at_ms <= now` de `restore_value` isoladamente,
        // sem depender do mecanismo de expiração do Redis.
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;

        let result = history.rollback(&pool, &host, None, id).await;

        let exists: bool = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        assert!(
            matches!(result, Err(HistoryError::ExpiredBeforeRollback { .. })),
            "esperava ExpiredBeforeRollback, veio {result:?}"
        );
        assert!(
            !exists,
            "a chave não pode ser recriada — ela já tinha expirado sozinha em produção"
        );

        let entry = history.get(id).unwrap();
        assert!(
            !entry.rolled_back,
            "uma volta atrás recusada não pode marcar a entrada como revertida"
        );
    }

    #[tokio::test]
    async fn rollback_refuses_when_the_key_changed_type() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_type_changed";

        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("valor-string")
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

        // Entre a escrita original e a volta atrás, outra coisa recria a
        // chave com um tipo diferente (uma lista, não a string original).
        let _: () = redis::cmd("RPUSH")
            .arg(key)
            .arg("item-de-outro-tipo")
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = history.rollback(&pool, &host, None, id).await;

        let current_type: String = redis::cmd("TYPE")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        assert!(
            matches!(result, Err(HistoryError::TypeChanged { .. })),
            "esperava TypeChanged, veio {result:?}"
        );
        assert_eq!(
            current_type, "list",
            "a chave recriada por outra coisa não pode ser apagada pela volta atrás recusada"
        );
    }

    #[tokio::test]
    async fn rollback_of_a_creation_deletes_the_key_when_the_type_still_matches() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_creation_matches";

        // Garante que a chave não existe antes — a escrita vai criá-la do
        // zero, então `before_json` será `None`.
        del(&mut conn, &[key]).await;

        let (_dir, history) = open_history();
        let op = WriteOp::SetString {
            value: text("valor-criado-do-zero"),
        };
        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();

        let entry = history.get(id).unwrap();
        assert!(
            entry.before_json.is_none(),
            "a chave não existia antes — before_json precisa ser None"
        );

        history.rollback(&pool, &host, None, id).await.unwrap();

        let exists: bool = redis::cmd("EXISTS")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        del(&mut conn, &[key]).await;

        assert!(
            !exists,
            "a volta atrás de uma criação (tipo ainda compatível) precisa apagar a chave criada"
        );
    }

    #[tokio::test]
    async fn rollback_of_a_creation_refuses_when_the_key_was_replaced_by_a_different_type() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:rollback_creation_type_changed";

        del(&mut conn, &[key]).await;

        let (_dir, history) = open_history();
        let op = WriteOp::SetString {
            value: text("valor-criado-do-zero"),
        };
        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();

        // Depois da criação rastreada (uma string), outra coisa substitui a
        // chave por um tipo diferente (uma lista) com o mesmo nome. A volta
        // atrás desta entrada é "apagar a string criada" — mas o que está
        // lá agora não é a string criada, é dado de outra origem; apagar
        // sem checar destruiria esse dado não relacionado. Este é
        // exatamente o ramo de `restore_value` que a checagem de tipo do
        // outro teste (`rollback_refuses_when_the_key_changed_type`) NÃO
        // cobre: lá `before_json` tinha um valor; aqui ele é `None`.
        let _: () = redis::cmd("DEL")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        let _: () = redis::cmd("RPUSH")
            .arg(key)
            .arg("item-de-outro-tipo")
            .query_async(&mut conn)
            .await
            .unwrap();

        let result = history.rollback(&pool, &host, None, id).await;

        let current_type: String = redis::cmd("TYPE")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        let entry = history.get(id).unwrap();
        del(&mut conn, &[key]).await;

        assert!(
            matches!(result, Err(HistoryError::TypeChanged { .. })),
            "esperava TypeChanged, veio {result:?}"
        );
        assert_eq!(
            current_type, "list",
            "a chave substituída por outra coisa não pode ser apagada pela volta atrás recusada"
        );
        assert!(
            !entry.rolled_back,
            "uma volta atrás recusada não pode marcar a entrada como revertida"
        );
    }

    #[tokio::test]
    async fn backup_and_restore_of_a_large_collection_survives_every_element() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:large_collection_roundtrip";

        // Bem além de `ValueLimit::default().max_elements` (500) — a tela de
        // detalhes cortaria essa coleção; o backup não pode.
        let total_items = 600;
        let mut cmd = redis::cmd("RPUSH");
        cmd.arg(key);
        for i in 0..total_items {
            cmd.arg(format!("item_{i}"));
        }
        let _: () = cmd.query_async(&mut conn).await.unwrap();

        let (_dir, history) = open_history();
        let op = WriteOp::SetListItem {
            index: 0,
            value: text("item_0_mudado"),
        };
        let id = history
            .write_with_backup(&pool, &host, None, host.db, key, &op, Actor::Human)
            .await
            .unwrap();

        history.rollback(&pool, &host, None, id).await.unwrap();

        let len: i64 = redis::cmd("LLEN")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        let all_items: Vec<String> = redis::cmd("LRANGE")
            .arg(key)
            .arg(0)
            .arg(-1)
            .query_async(&mut conn)
            .await
            .unwrap();

        del(&mut conn, &[key]).await;

        assert_eq!(
            len, total_items as i64,
            "a volta atrás não pode perder elementos de uma coleção grande"
        );
        let expected: Vec<String> = (0..total_items).map(|i| format!("item_{i}")).collect();
        assert_eq!(
            all_items, expected,
            "todo elemento precisa sobreviver, inclusive os que ficam além de max_elements (500)"
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
                    None,
                    Actor::Human,
                    now - i * 1000,
                )
                .unwrap();
            host_b_ids.push(id);
        }

        let removed_a = history.prune(&host.id, 30, 500).unwrap();
        assert_eq!(
            removed_a, 5,
            "só as 5 entradas com mais de 30 dias deveriam sumir"
        );

        for id in &recent_ids {
            assert!(
                history.get(*id).is_ok(),
                "entrada recente não deveria ser removida"
            );
        }
        for id in &old_ids {
            assert!(
                matches!(history.get(*id), Err(HistoryError::NotFound(_))),
                "entrada com mais de 30 dias deveria ter sido removida"
            );
        }

        let removed_b = history.prune(&other_host.id, 30, 500).unwrap();
        assert_eq!(
            removed_b, 10,
            "das 510 entradas, as 10 mais antigas deveriam sumir"
        );

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
            .insert_entry(
                &host,
                host.db,
                "chave",
                "resumo",
                None,
                None,
                Actor::Human,
                now,
            )
            .unwrap();
        let llm_id = history
            .insert_entry(
                &host,
                host.db,
                "chave",
                "resumo",
                None,
                None,
                Actor::Llm,
                now,
            )
            .unwrap();

        assert_eq!(history.get(human_id).unwrap().actor, Actor::Human);
        assert_eq!(history.get(llm_id).unwrap().actor, Actor::Llm);
    }

    /// Entradas gravadas antes da coluna `op_json` existir não têm operação
    /// nenhuma — só a frase em português. O despacho pela `op` não pode
    /// engasgar com elas: sem operação, segue o caminho de sempre.
    #[tokio::test]
    async fn an_entry_without_an_op_still_rolls_back() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_test:entrada_antiga";
        del(&mut conn, &[key]).await;

        let (_dir, history) = open_history();
        // A forma que `ValueSnapshot` tinha (e tem) em `before_json`,
        // escrita à mão: a entrada que este teste imita nasceu antes de
        // existir código para gravá-la com operação.
        let antes = r#"{"kind":"string","expires_at_ms":null,"elements":[{"field":null,"value":{"text":"antigo","base64":null},"score":null}]}"#;
        let id = history
            .insert_entry(
                &host,
                host.db,
                key,
                "Definir o valor de history_rs_test:entrada_antiga",
                None,
                Some(antes),
                Actor::Human,
                now_unix_ms(),
            )
            .unwrap();
        let _: () = redis::cmd("SET")
            .arg(key)
            .arg("novo")
            .query_async(&mut conn)
            .await
            .unwrap();

        let relatorio = history.rollback(&pool, &host, None, id).await.unwrap();

        assert!(
            matches!(relatorio, RollbackOutcome::Key),
            "entrada sem operação segue o caminho de chave única: {relatorio:?}"
        );
        let valor: String = redis::cmd("GET")
            .arg(key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(
            valor, "antigo",
            "a volta atrás de sempre precisa continuar valendo"
        );
        assert!(history.get(id).unwrap().rolled_back);

        del(&mut conn, &[key]).await;
    }

    // ---------------------------------------------------------------
    // Exclusão em massa: uma entrada de histórico com N retratos.
    //
    // Banco 14: os outros já estão tomados pelo resto da suíte, que roda
    // em paralelo contra o mesmo contêiner (ver `docs/CONTEXTO.md`). Cada
    // teste daqui usa seu próprio prefixo de chave pelo mesmo motivo — as
    // asserções são de igualdade exata.
    // ---------------------------------------------------------------

    fn bulk_host() -> Host {
        Host {
            db: 14,
            ..test_host()
        }
    }

    /// Cria `quantas` chaves string sob `prefix` — `MSET` sobrescreve o
    /// que tiver sobrado de uma execução anterior — e devolve os nomes.
    async fn seed_keys(
        conn: &mut MultiplexedConnection,
        prefix: &str,
        quantas: usize,
    ) -> Vec<String> {
        let keys: Vec<String> = (0..quantas).map(|n| format!("{prefix}{n}")).collect();
        let mut mset = redis::cmd("MSET");
        for key in &keys {
            mset.arg(key).arg(format!("valor-{key}"));
        }
        let _: () = mset.query_async(conn).await.unwrap();
        keys
    }

    async fn exists(conn: &mut MultiplexedConnection, keys: &[String]) -> u64 {
        redis::cmd("EXISTS")
            .arg(keys)
            .query_async(conn)
            .await
            .unwrap()
    }

    async fn drop_keys(conn: &mut MultiplexedConnection, keys: &[String]) {
        let _: () = redis::cmd("DEL").arg(keys).query_async(conn).await.unwrap();
    }

    /// Os retratos guardados numa cópia inline, já como JSON.
    fn snapshots_of(json: &str) -> Vec<serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(json)
            .unwrap()
            .as_array()
            .expect("a cópia precisa ser um vetor de retratos")
            .clone()
    }

    #[tokio::test]
    async fn inline_backup_holds_every_snapshot() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:inline:";
        let keys = seed_keys(&mut conn, prefix, 3).await;

        let (_dir, history) = open_history();
        let cancel = AtomicBool::new(false);
        let prefixes = vec![prefix.to_string()];

        let outcome = history
            .delete_prefix_with_backup(
                &pool,
                &host,
                None,
                host.db,
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    backup: BackupChoice::Inline,
                    cancel: &cancel,
                },
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.deleted, 3);
        assert_eq!(
            exists(&mut conn, &keys).await,
            0,
            "as três chaves precisam ter saído do banco"
        );

        let entry = history.get(outcome.entry_id).unwrap();
        let retratos = snapshots_of(
            entry
                .before_json
                .as_deref()
                .expect("uma cópia inline precisa estar em before_json"),
        );
        assert_eq!(
            retratos.len(),
            3,
            "três chaves apagadas, três retratos: {retratos:?}"
        );
        for (retrato, key) in retratos.iter().zip(&keys) {
            assert_eq!(retrato["key"], *key);
            assert_eq!(
                retrato["elements"][0]["value"]["text"],
                format!("valor-{key}")
            );
        }

        match entry.op.expect("a entrada precisa carregar a operação") {
            WriteOp::DeletePrefix {
                prefixes: gravados,
                deleted,
                enumerated,
                backup,
            } => {
                assert_eq!(gravados, prefixes);
                assert_eq!(deleted, 3);
                assert_eq!(enumerated, 3);
                assert_eq!(backup, BackupKind::Inline);
            }
            outra => panic!("operação errada no histórico: {outra:?}"),
        }
    }

    #[tokio::test]
    async fn binary_values_survive_the_backup() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let key = "history_rs_bulk:binario:0".to_string();
        let keys = vec![key.clone()];
        // Nem todo valor do Redis é UTF-8 válido. Estes bytes não são.
        let bruto: Vec<u8> = vec![0xff, 0xfe, 0x00, 0x41, 0x80];
        let _: () = redis::cmd("SET")
            .arg(&key)
            .arg(bruto.as_slice())
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let cancel = AtomicBool::new(false);
        let prefixes = vec!["history_rs_bulk:binario:".to_string()];

        let outcome = history
            .delete_prefix_with_backup(
                &pool,
                &host,
                None,
                host.db,
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    backup: BackupChoice::Inline,
                    cancel: &cancel,
                },
                |_| {},
            )
            .await
            .unwrap();

        let entry = history.get(outcome.entry_id).unwrap();
        let retratos = snapshots_of(entry.before_json.as_deref().unwrap());
        let valor = &retratos[0]["elements"][0]["value"];

        assert!(
            valor["text"].is_null(),
            "um valor que não é UTF-8 não pode virar texto estragado: {valor}"
        );
        let encoded = valor["base64"]
            .as_str()
            .expect("sem texto, a cópia precisa trazer o base64");
        assert_eq!(
            BASE64.decode(encoded).unwrap(),
            bruto,
            "os bytes precisam voltar idênticos do base64"
        );
    }

    #[tokio::test]
    async fn no_backup_leaves_before_json_null() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:sem_copia:";
        let keys = seed_keys(&mut conn, prefix, 2).await;

        let (_dir, history) = open_history();
        let cancel = AtomicBool::new(false);
        let prefixes = vec![prefix.to_string()];

        let outcome = history
            .delete_prefix_with_backup(
                &pool,
                &host,
                None,
                host.db,
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    backup: BackupChoice::None,
                    cancel: &cancel,
                },
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.deleted, 2);
        assert_eq!(exists(&mut conn, &keys).await, 0);

        let entry = history.get(outcome.entry_id).unwrap();
        assert!(
            entry.before_json.is_none(),
            "quem recusou a cópia não pode acabar com uma: {:?}",
            entry.before_json
        );
        match entry.op.expect("a entrada precisa carregar a operação") {
            WriteOp::DeletePrefix { backup, .. } => assert_eq!(backup, BackupKind::None),
            outra => panic!("operação errada no histórico: {outra:?}"),
        }
    }

    #[tokio::test]
    async fn ceiling_refuses_before_deleting() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:teto:";
        let keys = seed_keys(&mut conn, prefix, 3).await;

        let (_dir, history) = open_history();
        let cancel = AtomicBool::new(false);
        let prefixes = vec![prefix.to_string()];
        let alvo = Alvo {
            host: &host,
            pw: None,
            db: host.db,
        };

        // Teto de 10 bytes: três retratos não cabem nem de longe. O
        // fechamento de exclusão é o de verdade, para o teste provar que a
        // recusa acontece ANTES de ele ser chamado.
        let erro = history
            .delete_prefix_com_teto(
                &pool,
                alvo,
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    backup: BackupChoice::Inline,
                    cancel: &cancel,
                },
                10,
                || unlink_keys(&pool, alvo, &keys, &cancel, |_| {}),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(erro, HistoryError::BackupTooLarge { .. }),
            "o teto precisa recusar com BackupTooLarge, veio: {erro:?}"
        );
        assert_eq!(
            exists(&mut conn, &keys).await,
            3,
            "recusada a cópia, as chaves CONTINUAM no banco"
        );
        assert!(
            history.list(&host.id, 10).unwrap().is_empty(),
            "sem exclusão não há o que registrar no histórico"
        );

        drop_keys(&mut conn, &keys).await;
    }

    #[tokio::test]
    async fn file_backup_writes_and_points() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:arquivo:";
        let keys = seed_keys(&mut conn, prefix, 2).await;

        let (_dir, history) = open_history();
        let destino = tempfile::TempDir::new().unwrap();
        let cancel = AtomicBool::new(false);
        let prefixes = vec![prefix.to_string()];

        let outcome = history
            .delete_prefix_with_backup(
                &pool,
                &host,
                None,
                host.db,
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    backup: BackupChoice::File(destino.path()),
                    cancel: &cancel,
                },
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.deleted, 2);
        let entry = history.get(outcome.entry_id).unwrap();
        assert!(
            entry.before_json.is_none(),
            "a cópia foi para o arquivo; before_json não pode repeti-la"
        );

        let caminho = match entry.op.expect("a entrada precisa carregar a operação") {
            WriteOp::DeletePrefix {
                backup: BackupKind::File { path },
                ..
            } => path,
            outra => panic!("a operação precisa apontar para o arquivo: {outra:?}"),
        };

        let conteudo =
            std::fs::read_to_string(&caminho).expect("o arquivo da cópia precisa existir");
        assert_eq!(
            snapshots_of(&conteudo).len(),
            2,
            "o arquivo precisa trazer os dois retratos"
        );
        assert!(
            std::path::Path::new(&caminho).starts_with(destino.path()),
            "o arquivo precisa ter sido gravado no diretório escolhido: {caminho}"
        );
    }

    #[tokio::test]
    async fn entry_records_what_actually_went() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:cancelado:";
        // Mais de um lote de `UNLINK_BATCH`: o cancelamento só é conferido
        // ENTRE lotes, então é preciso haver um segundo lote para parar.
        let keys = seed_keys(&mut conn, prefix, UNLINK_BATCH + 100).await;

        let (_dir, history) = open_history();
        let cancel = AtomicBool::new(true);
        let prefixes = vec![prefix.to_string()];

        let outcome = history
            .delete_prefix_with_backup(
                &pool,
                &host,
                None,
                host.db,
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    backup: BackupChoice::None,
                    cancel: &cancel,
                },
                |_| {},
            )
            .await
            .unwrap();

        assert_eq!(outcome.deleted, UNLINK_BATCH as u64);

        match history
            .get(outcome.entry_id)
            .unwrap()
            .op
            .expect("a entrada precisa carregar a operação")
        {
            WriteOp::DeletePrefix {
                deleted,
                enumerated,
                ..
            } => {
                assert_eq!(deleted, UNLINK_BATCH as u64);
                assert_eq!(enumerated, keys.len() as u64);
                assert!(
                    deleted < enumerated,
                    "cancelada no meio, a entrada precisa dizer que saiu menos do que foi achado"
                );
            }
            outra => panic!("operação errada no histórico: {outra:?}"),
        }

        drop_keys(&mut conn, &keys).await;
    }

    /// Review Focus: a conexão morre no meio da exclusão. O que já saiu,
    /// saiu — e a linha precisa dizer esse número ANTES de o erro subir,
    /// senão o histórico promete de volta chaves que não existem mais.
    /// O fechamento de exclusão é forjado porque não há como derrubar a
    /// conexão no meio de um `UNLINK` de verdade sob demanda.
    #[tokio::test]
    async fn a_partial_delete_is_recorded_before_the_error_climbs() {
        let pool = Pool::new();
        let host = bulk_host();
        let (_dir, history) = open_history();
        let cancel = AtomicBool::new(false);
        let prefixes = vec!["history_rs_bulk:parcial:".to_string()];
        let keys: Vec<String> = (0..5)
            .map(|n| format!("history_rs_bulk:parcial:{n}"))
            .collect();

        let erro = history
            .delete_prefix_com_teto(
                &pool,
                Alvo {
                    host: &host,
                    pw: None,
                    db: host.db,
                },
                BulkDelete {
                    prefixes: &prefixes,
                    keys: &keys,
                    // Sem cópia: este teste é sobre a contagem, e assim ele
                    // não precisa de chave nenhuma no servidor.
                    backup: BackupChoice::None,
                    cancel: &cancel,
                },
                MAX_BACKUP_BYTES,
                || async {
                    Err(BulkError {
                        deleted: 2,
                        source: ConnError::Unreachable {
                            host: "host de teste".to_string(),
                            detail: "conexão caiu no meio do lote".to_string(),
                        },
                    })
                },
            )
            .await
            .unwrap_err();

        assert!(
            matches!(erro, HistoryError::Conn(ConnError::Unreachable { .. })),
            "o erro da conexão precisa subir, veio: {erro:?}"
        );

        let entradas = history.list(&host.id, 10).unwrap();
        assert_eq!(entradas.len(), 1, "o parcial precisa ter ficado registrado");
        match entradas[0]
            .op
            .clone()
            .expect("a entrada precisa carregar a operação")
        {
            WriteOp::DeletePrefix {
                deleted,
                enumerated,
                ..
            } => {
                assert_eq!(deleted, 2, "a linha precisa guardar o parcial do erro");
                assert_eq!(enumerated, 5);
            }
            outra => panic!("operação errada no histórico: {outra:?}"),
        }
    }

    // ---------------------------------------------------------------
    // Volta atrás de uma exclusão em massa: o outro lado da cópia.
    // ---------------------------------------------------------------

    /// Cria um `set` — tipo somente leitura, que a volta atrás não sabe
    /// recriar — para servir de chave que falha no meio de um lote.
    async fn seed_set(conn: &mut MultiplexedConnection, key: &str) {
        let _: () = redis::cmd("DEL").arg(key).query_async(conn).await.unwrap();
        let _: () = redis::cmd("SADD")
            .arg(key)
            .arg("a")
            .arg("b")
            .query_async(conn)
            .await
            .unwrap();
    }

    /// Apaga `keys` inteiro, com a cópia pedida — o preâmbulo comum de
    /// todo teste de volta atrás daqui.
    async fn delete_all(
        history: &History,
        pool: &Pool,
        host: &Host,
        prefixes: &[String],
        keys: &[String],
        backup: BackupChoice<'_>,
    ) -> BulkOutcome {
        let cancel = AtomicBool::new(false);
        history
            .delete_prefix_with_backup(
                pool,
                host,
                None,
                host.db,
                BulkDelete {
                    prefixes,
                    keys,
                    backup,
                    cancel: &cancel,
                },
                |_| {},
            )
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn rollback_restores_every_key() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta:";
        let keys = seed_keys(&mut conn, prefix, 3).await;
        // A última leva prazo: o valor precisa voltar, e o prazo também.
        let _: () = redis::cmd("PEXPIRE")
            .arg(&keys[2])
            .arg(300_000)
            .query_async(&mut conn)
            .await
            .unwrap();

        let (_dir, history) = open_history();
        let prefixes = vec![prefix.to_string()];
        let apagada = delete_all(
            &history,
            &pool,
            &host,
            &prefixes,
            &keys,
            BackupChoice::Inline,
        )
        .await;
        assert_eq!(
            exists(&mut conn, &keys).await,
            0,
            "as três precisam ter saído antes da volta atrás"
        );

        let relatorio = history
            .rollback(&pool, &host, None, apagada.entry_id)
            .await
            .unwrap();

        match relatorio {
            RollbackOutcome::Bulk { restored, failed } => {
                assert_eq!(restored.len(), 3);
                assert!(failed.is_empty(), "nenhuma chave devia falhar: {failed:?}");
            }
            outro => panic!("a exclusão em massa precisa relatar o lote: {outro:?}"),
        }

        assert_eq!(
            exists(&mut conn, &keys).await,
            3,
            "as três precisam ter voltado"
        );
        for key in &keys {
            let valor: String = redis::cmd("GET")
                .arg(key)
                .query_async(&mut conn)
                .await
                .unwrap();
            assert_eq!(valor, format!("valor-{key}"));
        }
        let sem_prazo: i64 = redis::cmd("PTTL")
            .arg(&keys[0])
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(sem_prazo, -1, "quem não tinha prazo não pode ganhar um");
        let prazo: i64 = redis::cmd("PTTL")
            .arg(&keys[2])
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(
            (200_000..=300_000).contains(&prazo),
            "o prazo precisa voltar junto do valor, veio {prazo}"
        );
        assert!(
            history.get(apagada.entry_id).unwrap().rolled_back,
            "o lote inteiro voltou: a entrada é um desfeito de verdade"
        );

        drop_keys(&mut conn, &keys).await;
    }

    #[tokio::test]
    async fn rollback_restores_from_the_file() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta_arquivo:";
        let keys = seed_keys(&mut conn, prefix, 2).await;

        let (_dir, history) = open_history();
        let destino = tempfile::TempDir::new().unwrap();
        let prefixes = vec![prefix.to_string()];
        let apagada = delete_all(
            &history,
            &pool,
            &host,
            &prefixes,
            &keys,
            BackupChoice::File(destino.path()),
        )
        .await;
        assert!(
            history.get(apagada.entry_id).unwrap().before_json.is_none(),
            "a cópia está no arquivo: é de lá que a restauração tem de sair"
        );

        let relatorio = history
            .rollback(&pool, &host, None, apagada.entry_id)
            .await
            .unwrap();

        match relatorio {
            RollbackOutcome::Bulk { restored, failed } => {
                assert_eq!(restored.len(), 2);
                assert!(failed.is_empty(), "nenhuma chave devia falhar: {failed:?}");
            }
            outro => panic!("a exclusão em massa precisa relatar o lote: {outro:?}"),
        }

        for key in &keys {
            let valor: String = redis::cmd("GET")
                .arg(key)
                .query_async(&mut conn)
                .await
                .unwrap();
            assert_eq!(
                valor,
                format!("valor-{key}"),
                "a cópia em arquivo precisa restaurar o mesmo que a embutida"
            );
        }

        drop_keys(&mut conn, &keys).await;
    }

    #[tokio::test]
    async fn rollback_without_backup_is_refused() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta_sem_copia:";
        let keys = seed_keys(&mut conn, prefix, 2).await;

        let (_dir, history) = open_history();
        let prefixes = vec![prefix.to_string()];
        let apagada =
            delete_all(&history, &pool, &host, &prefixes, &keys, BackupChoice::None).await;

        let erro = history
            .rollback(&pool, &host, None, apagada.entry_id)
            .await
            .unwrap_err();

        assert!(
            matches!(erro, HistoryError::NoBackup { .. }),
            "sem cópia não há o que restaurar, e a recusa precisa dizer isso: {erro:?}"
        );
        assert!(
            erro.to_string().contains("sem cópia de segurança"),
            "a mensagem é o que a pessoa lê: {erro}"
        );
        assert_eq!(
            exists(&mut conn, &keys).await,
            0,
            "uma recusa não pode escrever nada"
        );
        assert!(
            !history.get(apagada.entry_id).unwrap().rolled_back,
            "uma recusa não desfez coisa nenhuma"
        );
    }

    #[tokio::test]
    async fn partial_rollback_does_not_mark_done() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta_parcial:";
        let textos = seed_keys(&mut conn, prefix, 2).await;
        let set_key = format!("{prefix}conjunto");
        seed_set(&mut conn, &set_key).await;
        // O `set` no MEIO do lote: a falha dele não pode abortar o resto.
        let keys = vec![textos[0].clone(), set_key.clone(), textos[1].clone()];

        let (_dir, history) = open_history();
        let prefixes = vec![prefix.to_string()];
        let apagada = delete_all(
            &history,
            &pool,
            &host,
            &prefixes,
            &keys,
            BackupChoice::Inline,
        )
        .await;

        let relatorio = history
            .rollback(&pool, &host, None, apagada.entry_id)
            .await
            .unwrap();

        match relatorio {
            RollbackOutcome::Bulk { failed, .. } => {
                assert_eq!(failed.len(), 1, "só o `set` devia falhar: {failed:?}");
                assert_eq!(failed[0].key, set_key, "o relatório nomeia quem não voltou");
            }
            outro => panic!("a exclusão em massa precisa relatar o lote: {outro:?}"),
        }

        assert!(
            !history.get(apagada.entry_id).unwrap().rolled_back,
            "desfeito pela metade não é desfeito"
        );
        assert_eq!(
            exists(&mut conn, &textos).await,
            2,
            "a falha de uma chave não pode abortar as outras"
        );

        drop_keys(&mut conn, &keys).await;
    }

    #[tokio::test]
    async fn rollback_reports_what_it_restored() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta_relato:";
        let textos = seed_keys(&mut conn, prefix, 2).await;
        let set_key = format!("{prefix}conjunto");
        seed_set(&mut conn, &set_key).await;
        let keys = vec![textos[0].clone(), textos[1].clone(), set_key.clone()];

        let (_dir, history) = open_history();
        let prefixes = vec![prefix.to_string()];
        let apagada = delete_all(
            &history,
            &pool,
            &host,
            &prefixes,
            &keys,
            BackupChoice::Inline,
        )
        .await;

        let relatorio = history
            .rollback(&pool, &host, None, apagada.entry_id)
            .await
            .unwrap();

        match relatorio {
            RollbackOutcome::Bulk { restored, failed } => {
                assert_eq!(restored.len(), 2, "duas voltaram");
                assert_eq!(failed.len(), 1, "uma não: {failed:?}");
                assert_eq!(failed[0].key, set_key);
                assert!(
                    failed[0].reason.contains("set"),
                    "o motivo precisa dizer por que a chave não voltou: {}",
                    failed[0].reason
                );
            }
            outro => panic!("a exclusão em massa precisa relatar o lote: {outro:?}"),
        }

        drop_keys(&mut conn, &keys).await;
    }

    /// O arquivo da cópia vive fora do banco de histórico e pode ter sido
    /// movido, apagado ou ficado num disco que não está montado. A volta
    /// atrás precisa dizer isso — e não escrever nada.
    #[tokio::test]
    async fn a_missing_backup_file_is_an_error_not_a_silent_half_restore() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta_arquivo_sumido:";
        let keys = seed_keys(&mut conn, prefix, 2).await;

        let (_dir, history) = open_history();
        let destino = tempfile::TempDir::new().unwrap();
        let prefixes = vec![prefix.to_string()];
        let apagada = delete_all(
            &history,
            &pool,
            &host,
            &prefixes,
            &keys,
            BackupChoice::File(destino.path()),
        )
        .await;
        drop(destino);

        let erro = history
            .rollback(&pool, &host, None, apagada.entry_id)
            .await
            .unwrap_err();

        assert!(
            matches!(erro, HistoryError::BackupUnreadable { .. }),
            "o arquivo sumiu, e a volta atrás precisa dizer isso: {erro:?}"
        );
        assert_eq!(
            exists(&mut conn, &keys).await,
            0,
            "sem a cópia não há o que escrever"
        );
        assert!(
            !history.get(apagada.entry_id).unwrap().rolled_back,
            "nada foi desfeito"
        );
    }

    /// Uma queda no meio do `UNLINK` deixa `deleted` em 0 para sempre, com
    /// a cópia inteira e perfeitamente usável do lado. Quem decide o que
    /// restaurar é a cópia, nunca essa contagem.
    #[tokio::test]
    async fn a_row_that_counted_no_deletion_still_restores_its_backup() {
        let pool = Pool::new();
        let host = bulk_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        let prefix = "history_rs_bulk:volta_sem_contagem:";
        let keys = seed_keys(&mut conn, prefix, 2).await;

        let (_dir, history) = open_history();
        let prefixes = vec![prefix.to_string()];
        let apagada = delete_all(
            &history,
            &pool,
            &host,
            &prefixes,
            &keys,
            BackupChoice::Inline,
        )
        .await;
        let copia = history
            .get(apagada.entry_id)
            .unwrap()
            .before_json
            .expect("a cópia inline precisa estar na linha");

        // A mesma cópia numa linha que nunca chegou a contar o que apagou.
        let travada = WriteOp::DeletePrefix {
            prefixes: prefixes.clone(),
            deleted: 0,
            enumerated: keys.len() as u64,
            backup: BackupKind::Inline,
        };
        let id = history
            .insert_entry(
                &host,
                host.db,
                prefix,
                &op_summary(prefix, &travada),
                serde_json::to_string(&travada).ok().as_deref(),
                Some(&copia),
                Actor::Human,
                now_unix_ms(),
            )
            .unwrap();

        let relatorio = history.rollback(&pool, &host, None, id).await.unwrap();

        match relatorio {
            RollbackOutcome::Bulk { restored, failed } => {
                assert_eq!(restored.len(), 2, "a cópia está lá e é usável");
                assert!(failed.is_empty(), "nenhuma chave devia falhar: {failed:?}");
            }
            outro => panic!("a exclusão em massa precisa relatar o lote: {outro:?}"),
        }
        assert_eq!(
            exists(&mut conn, &keys).await,
            2,
            "as duas precisam ter voltado"
        );

        drop_keys(&mut conn, &keys).await;
    }

    /// O nome do arquivo da cópia carrega a data, e montá-la sem
    /// dependência de calendário é a parte deste módulo que mais fácil
    /// erraria por um dia sem ninguém notar.
    #[test]
    fn the_backup_file_name_carries_the_moment() {
        assert_eq!(timestamp_for_name(0), "1970-01-01-000000");
        // 2026-11-24T15:04:05Z = 1_795_532_645 s desde a época.
        assert_eq!(timestamp_for_name(1_795_532_645_000), "2026-11-24-150405");
    }

    /// O formato do nome é mandado pelo plano: `AAAA-MM-DD-hhmmss-<host>-
    /// db<N>-<prefixo>.json`. Igualdade exata porque é a forma inteira que
    /// está sendo fixada, não um pedaço dela.
    #[test]
    fn the_backup_file_name_follows_the_mandated_shape() {
        let name = backup_file_name(
            &bulk_host(),
            14,
            &["wallet:".to_string(), "session:".to_string()],
            1_795_532_645_000,
        );

        assert_eq!(name, "2026-11-24-150405-Redis_de_teste-db14-wallet_.json");
    }

    /// Dois prefixos diferentes que saneiam para o mesmo nome. O nome é o
    /// mesmo de propósito — o que não pode é o ARQUIVO ser o mesmo, porque
    /// a segunda cópia apagaria a primeira.
    #[test]
    fn two_prefixes_that_sanitize_alike_land_in_different_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let host = bulk_host();
        let um = backup_file_name(&host, 14, &["app:cache:".to_string()], 1_795_532_645_000);
        let outro = backup_file_name(&host, 14, &["app/cache/".to_string()], 1_795_532_645_000);
        assert_eq!(
            um, outro,
            "os dois prefixos precisam mesmo colidir no nome; é esse o caso sob teste"
        );

        let primeiro = write_backup_file(dir.path(), &um, "[\"primeira\"]").unwrap();
        let segundo = write_backup_file(dir.path(), &outro, "[\"segunda\"]").unwrap();

        assert_ne!(primeiro, segundo, "duas cópias, dois arquivos");
        assert_eq!(
            std::fs::read_to_string(&primeiro).unwrap(),
            "[\"primeira\"]",
            "a primeira cópia não pode ter sido sobrescrita"
        );
        assert_eq!(std::fs::read_to_string(&segundo).unwrap(), "[\"segunda\"]");
    }

    /// O saneamento corta em 40 caracteres, então dois prefixos que só
    /// diferem depois do 40º caem no mesmo nome.
    #[test]
    fn prefixes_that_share_the_first_40_characters_land_in_different_files() {
        let dir = tempfile::TempDir::new().unwrap();
        let host = bulk_host();
        let comum = "a".repeat(40);
        let um = backup_file_name(&host, 14, &[format!("{comum}:um:")], 1_795_532_645_000);
        let outro = backup_file_name(&host, 14, &[format!("{comum}:dois:")], 1_795_532_645_000);
        assert_eq!(
            um, outro,
            "os dois prefixos precisam mesmo colidir no nome; é esse o caso sob teste"
        );

        let primeiro = write_backup_file(dir.path(), &um, "[\"primeira\"]").unwrap();
        let segundo = write_backup_file(dir.path(), &outro, "[\"segunda\"]").unwrap();

        assert_ne!(primeiro, segundo, "duas cópias, dois arquivos");
        assert_eq!(
            std::fs::read_to_string(&primeiro).unwrap(),
            "[\"primeira\"]",
            "a primeira cópia não pode ter sido sobrescrita"
        );
    }

    /// O pior caso concreto: a pessoa repete a exclusão por engano no mesmo
    /// segundo, a segunda passada não acha nada para apagar e grava `[]`.
    /// Se isso caísse sobre o arquivo da primeira, a linha de histórico dela
    /// continuaria apontando para um caminho que diz ter cópia e não tem —
    /// o fracasso invisível-até-precisar que esta tarefa inteira existe
    /// para impedir.
    #[test]
    fn a_second_backup_in_the_same_second_does_not_overwrite_the_first() {
        let dir = tempfile::TempDir::new().unwrap();
        let name = backup_file_name(
            &bulk_host(),
            14,
            &["wallet:".to_string()],
            1_795_532_645_000,
        );

        let primeiro = write_backup_file(dir.path(), &name, "[\"a cópia que importa\"]").unwrap();
        let segundo = write_backup_file(dir.path(), &name, "[]").unwrap();

        assert_eq!(
            std::fs::read_to_string(&primeiro).unwrap(),
            "[\"a cópia que importa\"]",
            "a cópia de verdade precisa continuar inteira no disco"
        );
        assert_eq!(
            segundo.file_name().unwrap().to_str().unwrap(),
            "2026-11-24-150405-Redis_de_teste-db14-wallet_-2.json",
            "a segunda cópia ganha sufixo, mantendo a forma do nome"
        );
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
