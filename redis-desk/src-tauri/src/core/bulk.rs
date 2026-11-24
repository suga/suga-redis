//! Enumeração de chaves sob um ou mais prefixos — o primeiro passo da
//! exclusão em massa pedida pelo menu de contexto da árvore. Varre, nomeia e
//! classifica; apagar é responsabilidade de outra função, que decide o que
//! fazer com a lista que sai daqui.

use super::conn::{ConnError, Pool};
use super::keys::{Alvo, KeyKind, SCAN_COUNT, ScanTick, escapar_glob};

/// Teto de chaves que uma enumeração devolve.
///
/// Existe pela mesma razão do teto de filhos da trie (`keys::MAX_CHILDREN`):
/// um prefixo com milhões de chaves por baixo não pode virar um `Vec` do
/// tamanho do banco inteiro só porque alguém clicou em "excluir". Batido o
/// teto, a enumeração para e avisa — quem chamou decide se mostra o aviso ou
/// recusa a exclusão.
pub const MAX_ENUMERATED: usize = 100_000;

/// O resultado de enumerar um ou mais prefixos: os nomes encontrados, quantos
/// de cada tipo, e se a lista parou no teto antes de esgotar o prefixo.
#[derive(Debug, Clone, PartialEq)]
pub struct Enumeration {
    pub keys: Vec<String>,
    /// Quantas de cada tipo, para o diálogo avisar o que não volta atrás.
    pub kinds: Vec<(KeyKind, u64)>,
    /// Parou no teto: há mais do que isto sob o prefixo.
    pub truncated: bool,
}

/// Como a varredura decide se uma chave pertence ao pedido.
///
/// Existe porque o `MATCH` do `SCAN` não encurta o trabalho do servidor —
/// ele só filtra o que volta pela rede, e o cursor percorre o espaço de
/// chaves inteiro de todo jeito (ver `keys::expand_branch` e o fato
/// registrado em `docs/CONTEXTO.md`). Por isso, com VÁRIOS prefixos, pedir
/// `MATCH` de cada um custaria uma varredura por prefixo; filtrar em Rust
/// depois de UMA varredura sem `MATCH` custa o mesmo que uma varredura só,
/// filtrada no servidor.
#[derive(Clone)]
enum Modo {
    /// Um único prefixo: o próprio servidor filtra via `MATCH`, então toda
    /// chave que volta já serve.
    Match(String),
    /// Nenhum prefixo: pedido explícito do banco inteiro, sem filtro.
    SemFiltro,
    /// Vários prefixos: sem `MATCH`, filtrando por `starts_with` aqui.
    Filtra(Vec<String>),
}

fn combina(modo: &Modo, chave: &str) -> bool {
    match modo {
        Modo::Match(_) | Modo::SemFiltro => true,
        Modo::Filtra(prefixos) => prefixos
            .iter()
            .any(|prefixo| chave.starts_with(prefixo.as_str())),
    }
}

/// Filtra um lote do `SCAN`, decidindo o que é candidato novo e se o teto
/// foi batido. Pura e síncrona — não fala com o Redis — para o teste de
/// deduplicação poder alimentar um lote com repetição à mão, sem depender de
/// conseguir fazer um servidor de verdade devolver a mesma chave duas vezes.
///
/// `vistas` é o conjunto de chaves JÁ aceitas, atravessando lotes: o `SCAN`
/// garante "pelo menos uma vez", nunca "no máximo uma vez" — a mesma chave
/// pode voltar no mesmo lote ou num lote seguinte depois de um rehash do
/// servidor. Sem isto, ela entraria duas vezes na enumeração, inflando a
/// contagem que o diálogo de exclusão promete ser exata (uma chave apagada
/// duas vezes também duplicaria a cópia de segurança de uma chave só).
///
/// O teto conta chaves DISTINTAS: uma repetição não consome vaga, e o aviso
/// de corte só liga quando uma chave realmente nova não coube mais.
fn filtra_lote(
    modo: &Modo,
    lote: Vec<String>,
    vistas: &mut std::collections::HashSet<String>,
    teto: usize,
) -> (Vec<String>, bool) {
    let mut candidatos = Vec::new();
    let mut truncated = false;

    for chave in lote {
        if vistas.len() >= teto {
            truncated = true;
            break;
        }
        if !combina(modo, &chave) {
            continue;
        }
        if !vistas.insert(chave.clone()) {
            // Já vista — neste lote ou num anterior. Não conta de novo nem
            // consome vaga do teto.
            continue;
        }
        candidatos.push(chave);
    }

    (candidatos, truncated)
}

/// Enumera toda chave sob um ou mais prefixos, com o `TYPE` de cada uma.
///
/// Prefixo é casamento por `starts_with`, nada mais: uma chave que também é
/// prefixo de outra (`wallet` ao lado de `wallet:1`) aparece nas duas
/// posições — como galho e como folha, a árvore já faz isso, e apagar em
/// massa precisa do mesmo tratamento, ou a própria chave escaparia da
/// exclusão.
pub async fn enumerate(
    pool: &Pool,
    alvo: Alvo<'_>,
    prefixes: &[String],
    progress: impl Fn(ScanTick),
) -> Result<Enumeration, ConnError> {
    enumerate_com_teto(pool, alvo, prefixes, progress, MAX_ENUMERATED).await
}

/// `enumerate` com o teto como parâmetro, para o teste do caso de corte não
/// precisar criar `MAX_ENUMERATED` chaves de verdade.
async fn enumerate_com_teto(
    pool: &Pool,
    alvo: Alvo<'_>,
    prefixes: &[String],
    progress: impl Fn(ScanTick),
    teto: usize,
) -> Result<Enumeration, ConnError> {
    let Alvo { host, pw, db } = alvo;
    let progress = &progress;

    let modo = match prefixes {
        [] => Modo::SemFiltro,
        [unico] => Modo::Match(format!("{}*", escapar_glob(unico))),
        varios => Modo::Filtra(varios.to_vec()),
    };

    pool.with_retry(host, db, pw, move |mut conn| {
        let modo = modo.clone();
        async move {
            let total_estimate: u64 = redis::cmd("DBSIZE").query_async(&mut conn).await?;

            let mut keys: Vec<String> = Vec::new();
            let mut kinds: Vec<(KeyKind, u64)> = Vec::new();
            let mut vistas: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut truncated = false;
            let mut cursor = 0u64;

            loop {
                let mut comando = redis::cmd("SCAN");
                comando.arg(cursor).arg("COUNT").arg(SCAN_COUNT);
                if let Modo::Match(padrao) = &modo {
                    comando.arg("MATCH").arg(padrao);
                }
                let (next, lote): (u64, Vec<String>) = comando.query_async(&mut conn).await?;

                let (candidatos, cortou) = filtra_lote(&modo, lote, &mut vistas, teto);
                if cortou {
                    truncated = true;
                }

                if !candidatos.is_empty() {
                    // `TYPE` dos candidatos do lote, num pipeline só — não
                    // uma ida e volta por chave.
                    let mut pipe = redis::pipe();
                    pipe.ignore_errors();
                    for nome in &candidatos {
                        pipe.cmd("TYPE").arg(nome);
                    }
                    let respostas: Vec<redis::RedisResult<redis::Value>> =
                        pipe.query_async(&mut conn).await?;

                    for (nome, resposta) in candidatos.into_iter().zip(respostas) {
                        let tipo = resposta
                            .as_ref()
                            .ok()
                            .and_then(|valor| redis::from_redis_value_ref::<String>(valor).ok())
                            .unwrap_or_else(|| "none".to_string());

                        if tipo == "none" {
                            // Sumiu entre o SCAN e o TYPE (expirou ou foi
                            // apagada por outra conexão nesse meio-tempo) —
                            // não entra na lista, mesma regra de `scan_all`.
                            continue;
                        }

                        let kind = KeyKind::from_type_name(&tipo);
                        if let Some((_, contagem)) = kinds.iter_mut().find(|(k, _)| *k == kind) {
                            *contagem += 1;
                        } else {
                            kinds.push((kind, 1));
                        }
                        keys.push(nome);
                    }
                }

                progress(ScanTick {
                    scanned: keys.len() as u64,
                    total_estimate,
                });

                if truncated || next == 0 {
                    break;
                }
                cursor = next;
            }

            Ok(Enumeration {
                keys,
                kinds,
                truncated,
            })
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::{Env, Host};

    /// `filtra_lote` é pura e síncrona — não precisa do Redis de teste para
    /// provar a deduplicação. Forçar um servidor de verdade a devolver a
    /// mesma chave duas vezes (o caso real do `SCAN` durante um rehash) não
    /// é algo que dá para reproduzir de forma confiável num teste; alimentar
    /// a repetição à mão aqui é a mesma ideia do teste do teto, que também
    /// controla o parâmetro em vez de recriar a condição real.
    #[test]
    fn filtra_lote_dedups_a_key_seen_twice_in_the_same_batch() {
        let modo = Modo::SemFiltro;
        let mut vistas = std::collections::HashSet::new();

        let (candidatos, truncated) = filtra_lote(
            &modo,
            vec!["a".to_string(), "a".to_string(), "b".to_string()],
            &mut vistas,
            10,
        );

        assert_eq!(candidatos, vec!["a".to_string(), "b".to_string()]);
        assert!(!truncated);
    }

    #[test]
    fn filtra_lote_dedups_a_key_seen_again_in_a_later_batch() {
        // O caso real: o cursor do `SCAN` percorre de novo um bucket já
        // visitado depois de um rehash do servidor, e a mesma chave volta
        // num lote seguinte — não dentro do mesmo lote.
        let modo = Modo::SemFiltro;
        let mut vistas = std::collections::HashSet::new();

        let (primeiro, _) = filtra_lote(&modo, vec!["a".to_string()], &mut vistas, 10);
        assert_eq!(primeiro, vec!["a".to_string()]);

        let (segundo, truncated) = filtra_lote(
            &modo,
            vec!["a".to_string(), "c".to_string()],
            &mut vistas,
            10,
        );
        assert_eq!(
            segundo,
            vec!["c".to_string()],
            "\"a\" já tinha sido aceita no lote anterior"
        );
        assert!(!truncated);
    }

    #[test]
    fn filtra_lote_repeats_do_not_spend_a_slot_of_the_ceiling() {
        // O teto conta chaves DISTINTAS: uma repetição não pode empurrar uma
        // chave nova para fora da lista.
        let modo = Modo::SemFiltro;
        let mut vistas = std::collections::HashSet::new();

        let (candidatos, truncated) = filtra_lote(
            &modo,
            vec![
                "a".to_string(),
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
            ],
            &mut vistas,
            2,
        );

        assert_eq!(candidatos, vec!["a".to_string(), "b".to_string()]);
        assert!(truncated, "\"c\" não coube no teto de 2 chaves distintas");
    }

    /// Banco 13: nenhum outro teste do projeto usa este número (ver
    /// `docs/CONTEXTO.md`), então os testes deste módulo podem rodar em
    /// paralelo sem disputar banco com o resto da suíte.
    fn test_host() -> Host {
        Host {
            id: "bulk-test-host".to_string(),
            name: "Redis de teste".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6399,
            db: 13,
            username: None,
            tls: false,
            env: Env::Dev,
        }
    }

    fn alvo(host: &Host) -> Alvo<'_> {
        Alvo {
            host,
            pw: None,
            db: host.db,
        }
    }

    /// Cria chaves, roda o corpo e apaga tudo — mesmo se o corpo falhar numa
    /// asserção, para não deixar lixo no banco compartilhado. Cópia do
    /// auxiliar de `core::keys::tests`, contra o banco 13 em vez do 1.
    async fn com_chaves<F, Fut, T>(chaves: &[&str], corpo: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();
        for chave in chaves {
            let _: () = redis::cmd("SET")
                .arg(chave)
                .arg("x")
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let saida = corpo().await;

        for chave in chaves {
            let _: () = redis::cmd("DEL")
                .arg(chave)
                .query_async(&mut conn)
                .await
                .unwrap();
        }
        saida
    }

    fn prefixos(lista: &[&str]) -> Vec<String> {
        lista.iter().map(|p| p.to_string()).collect()
    }

    fn ordenadas(mut nomes: Vec<String>) -> Vec<String> {
        nomes.sort();
        nomes
    }

    #[tokio::test]
    async fn enumerates_only_the_prefix() {
        let chaves = ["only_rs_test:a:1", "only_rs_test:a:2", "only_rs_test:b:1"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration =
                enumerate(&pool, alvo(&host), &prefixos(&["only_rs_test:a:"]), |_| {})
                    .await
                    .unwrap();

            assert_eq!(
                ordenadas(enumeration.keys),
                vec![
                    "only_rs_test:a:1".to_string(),
                    "only_rs_test:a:2".to_string()
                ]
            );
            assert!(!enumeration.truncated);
        })
        .await;
    }

    #[tokio::test]
    async fn the_prefix_itself_is_included() {
        // Review Focus 1: `wallet` é prefixo de `wallet:1` E uma chave por
        // si só. As duas precisam sair enumeradas, ou a própria chave
        // escaparia da exclusão em massa.
        let chaves = [
            "prefix_itself_rs_test:wallet",
            "prefix_itself_rs_test:wallet:1",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["prefix_itself_rs_test:wallet"]),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                ordenadas(enumeration.keys),
                vec![
                    "prefix_itself_rs_test:wallet".to_string(),
                    "prefix_itself_rs_test:wallet:1".to_string(),
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn wildcard_in_prefix_does_not_leak() {
        // `*` é curinga do MATCH. Sem escapá-lo, o prefixo `lit*` passaria a
        // casar com qualquer coisa que comece com `lit`, trazendo chaves de
        // outro galho como se fossem dele.
        let chaves = ["wildcard_rs_test:lit*:1", "wildcard_rs_test:outro:1"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["wildcard_rs_test:lit*"]),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                enumeration.keys,
                vec!["wildcard_rs_test:lit*:1".to_string()]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn degenerate_names_survive() {
        // Review Focus 2: nomes degenerados sob o prefixo vazio — inclusive
        // uma chave de nome vazio — não podem se perder nem se fundir uns
        // com os outros.
        let chaves = ["", ":", "a:", "::b"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(&pool, alvo(&host), &prefixos(&[""]), |_| {})
                .await
                .unwrap();

            for chave in chaves {
                assert!(
                    enumeration.keys.iter().any(|k| k == chave),
                    "chave degenerada \"{chave}\" sumiu da enumeração: {:?}",
                    enumeration.keys
                );
            }
        })
        .await;
    }

    #[tokio::test]
    async fn empty_prefix_takes_the_whole_db() {
        // Sem prefixo nenhum (lista vazia) ou com um prefixo vazio — as duas
        // formas de pedir o banco inteiro. O banco 13 é compartilhado com
        // outros testes deste arquivo rodando em paralelo, então a asserção
        // é "as chaves criadas aparecem", não "só elas existem".
        let chaves = ["whole_db_rs_test:x", "whole_db_rs_test:y"];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let sem_prefixo_nenhum = enumerate(&pool, alvo(&host), &[], |_| {}).await.unwrap();
            for chave in chaves {
                assert!(sem_prefixo_nenhum.keys.iter().any(|k| k == chave));
            }

            let com_prefixo_vazio = enumerate(&pool, alvo(&host), &prefixos(&[""]), |_| {})
                .await
                .unwrap();
            for chave in chaves {
                assert!(com_prefixo_vazio.keys.iter().any(|k| k == chave));
            }
        })
        .await;
    }

    #[tokio::test]
    async fn several_prefixes_scan_once() {
        let chaves = [
            "several_rs_test:a:1",
            "several_rs_test:b:1",
            "several_rs_test:c:1",
        ];
        com_chaves(&chaves, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate(
                &pool,
                alvo(&host),
                &prefixos(&["several_rs_test:a:", "several_rs_test:c:"]),
                |_| {},
            )
            .await
            .unwrap();

            assert_eq!(
                ordenadas(enumeration.keys),
                vec![
                    "several_rs_test:a:1".to_string(),
                    "several_rs_test:c:1".to_string()
                ]
            );
        })
        .await;
    }

    #[tokio::test]
    async fn kinds_are_counted() {
        let pool = Pool::new();
        let host = test_host();
        let mut conn = pool.get(&host, host.db, None).await.unwrap();

        let prefix = "kinds_rs_test:";
        let string1 = format!("{prefix}string:1");
        let string2 = format!("{prefix}string:2");
        let hash1 = format!("{prefix}hash:1");

        redis::cmd("SET")
            .arg(&string1)
            .arg("x")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("SET")
            .arg(&string2)
            .arg("x")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();
        redis::cmd("HSET")
            .arg(&hash1)
            .arg("campo")
            .arg("x")
            .query_async::<()>(&mut conn)
            .await
            .unwrap();

        let resultado = enumerate(&pool, alvo(&host), &prefixos(&[prefix]), |_| {}).await;

        for chave in [&string1, &string2, &hash1] {
            let _: () = redis::cmd("DEL")
                .arg(chave)
                .query_async(&mut conn)
                .await
                .unwrap();
        }

        let enumeration = resultado.unwrap();
        assert_eq!(enumeration.keys.len(), 3);

        let contagem = |alvo_kind: KeyKind| -> u64 {
            enumeration
                .kinds
                .iter()
                .find(|(k, _)| *k == alvo_kind)
                .map(|(_, n)| *n)
                .unwrap_or(0)
        };
        assert_eq!(contagem(KeyKind::String), 2);
        assert_eq!(contagem(KeyKind::Hash), 1);
    }

    #[tokio::test]
    async fn stops_at_the_ceiling() {
        let chaves: Vec<String> = (0..7).map(|i| format!("ceiling_rs_test:{i:02}")).collect();
        let refs: Vec<&str> = chaves.iter().map(|s| s.as_str()).collect();

        com_chaves(&refs, || async {
            let pool = Pool::new();
            let host = test_host();

            let enumeration = enumerate_com_teto(
                &pool,
                alvo(&host),
                &prefixos(&["ceiling_rs_test:"]),
                |_| {},
                5,
            )
            .await
            .unwrap();

            assert!(enumeration.truncated, "devia avisar que parou no teto");
            assert_eq!(enumeration.keys.len(), 5);
        })
        .await;
    }
}
