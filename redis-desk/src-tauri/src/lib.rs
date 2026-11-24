//! Biblioteca do suga-redis: o núcleo, a fachada de comandos e a montagem
//! do app Tauri. O binário (`main.rs`) é só quem escolhe entre os dois modos.

pub mod approval;
pub mod commands;
pub mod core;
pub mod mcp;

/// O que o binário foi chamado para fazer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modo {
    Janela,
    Mcp,
    Versao,
    Ajuda,
}

/// Decide o modo a partir dos argumentos.
///
/// Mora aqui, e não no `main`, para poder ser testada: a regra antiga era
/// "qualquer coisa que não seja `--mcp` abre a janela", e um `--version`
/// digitado no terminal abria o aplicativo e travava o terminal junto.
pub fn modo_de<I: IntoIterator<Item = String>>(args: I) -> Modo {
    for arg in args {
        match arg.as_str() {
            "--mcp" => return Modo::Mcp,
            "--version" | "-V" => return Modo::Versao,
            "--help" | "-h" => return Modo::Ajuda,
            _ => {}
        }
    }
    Modo::Janela
}

/// O texto de `--help`, no idioma do sistema.
pub fn ajuda() -> String {
    ajuda_em(
        std::env::var("LC_ALL")
            .or_else(|_| std::env::var("LANG"))
            .ok()
            .as_deref(),
    )
}

/// O mesmo texto, para uma etiqueta de idioma qualquer.
///
/// A janela guarda a escolha de quem usa no `localStorage`, e a linha de
/// comando não tem como alcançá-la. Em vez de inventar um terceiro lugar
/// para guardar a mesma preferência, ela segue a convenção do Unix: `LANG`
/// (ou `LC_ALL`, que manda mais). Idioma que não falamos cai no inglês,
/// como no resto do aplicativo.
pub fn ajuda_em(etiqueta: Option<&str>) -> String {
    let base = etiqueta
        .unwrap_or("")
        .split(['_', '-', '.'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();

    let versao = env!("CARGO_PKG_VERSION");
    match base.as_str() {
        "pt" => format!(
            "Suga-Redis {versao}
Cliente de mesa para Redis, com servidor MCP embutido.

USO:
    suga-redis              abre a janela do aplicativo
    suga-redis --mcp        roda como servidor MCP em stdio
    suga-redis --version    mostra a versão
    suga-redis --help       mostra esta ajuda

O modo --mcp conversa pelo protocolo MCP na entrada e saída padrão; não é
feito para ser lido por gente. Veja docs/mcp-setup.md para ligá-lo ao Claude."
        ),
        "es" => format!(
            "Suga-Redis {versao}
Cliente de escritorio para Redis, con servidor MCP incorporado.

USO:
    suga-redis              abre la ventana de la aplicación
    suga-redis --mcp        corre como servidor MCP en stdio
    suga-redis --version    muestra la versión
    suga-redis --help       muestra esta ayuda

El modo --mcp habla el protocolo MCP por la entrada y la salida estándar; no
está hecho para que lo lea una persona. Mira docs/mcp-setup.md para
conectarlo a Claude."
        ),
        _ => format!(
            "Suga-Redis {versao}
A desktop Redis client, with an MCP server built in.

USAGE:
    suga-redis              opens the application window
    suga-redis --mcp        runs as an MCP server over stdio
    suga-redis --version    prints the version
    suga-redis --help       prints this help

The --mcp mode speaks the MCP protocol over standard input and output; it is
not meant to be read by a person. See docs/mcp-setup.md to wire it to Claude."
        ),
    }
}

/// Sobe a janela do app. Este é o modo normal, sem argumento nenhum.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        // O par updater/process anda junto: o primeiro busca e instala a
        // versão nova, o segundo reabre o app depois — sem ele a pessoa
        // atualiza e fica olhando uma janela que não existe mais.
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .setup(|app| commands::setup(app))
        .invoke_handler(tauri::generate_handler![
            commands::cmd_list_hosts,
            commands::cmd_add_host,
            commands::cmd_update_host,
            commands::cmd_remove_host,
            commands::cmd_connect,
            commands::cmd_disconnect,
            commands::cmd_test_host,
            commands::cmd_list_dbs,
            commands::cmd_scan_tree,
            commands::cmd_tree_level,
            commands::cmd_load_more_children,
            commands::cmd_rescan,
            commands::cmd_scan_status,
            commands::cmd_read_value,
            commands::cmd_read_value_more,
            commands::cmd_write_value,
            commands::cmd_enumerate_prefix,
            commands::cmd_delete_prefix,
            commands::cmd_cancel_delete,
            commands::cmd_list_history,
            commands::cmd_rollback,
            commands::cmd_search,
            commands::cmd_logs,
            commands::cmd_pending_approvals,
            commands::cmd_resolve_approval,
            commands::cmd_deny_approval,
        ])
        .run(tauri::generate_context!())
        .expect("erro ao subir a janela do Suga-Redis");
}

/// Roda como servidor MCP em stdio, sem janela nenhuma.
///
/// Monta o runtime do tokio aqui dentro (e não com `#[tokio::main]` no
/// `main`) porque o modo janela tem o seu próprio, montado pelo Tauri: dois
/// runtimes vivos no mesmo processo é exatamente o que este `if` evita.
pub fn run_mcp() -> std::io::Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(mcp::run_stdio())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modo(args: &[&str]) -> Modo {
        modo_de(args.iter().map(|a| a.to_string()))
    }

    #[test]
    fn a_ajuda_segue_o_idioma_do_sistema() {
        // A janela guarda a escolha de quem usa; a linha de comando não tem
        // como alcançá-la. Então ela segue a convenção do Unix e olha a
        // variável de ambiente — e cai no inglês quando não reconhece.
        assert!(ajuda_em(Some("pt_BR.UTF-8")).contains("abre a janela"));
        assert!(ajuda_em(Some("es_ES.UTF-8")).contains("abre la ventana"));
        assert!(ajuda_em(Some("en_US.UTF-8")).contains("opens the application window"));
        assert!(ajuda_em(Some("fr_FR.UTF-8")).contains("opens the application window"));
        assert!(ajuda_em(None).contains("opens the application window"));
    }

    #[test]
    fn sem_argumento_abre_a_janela() {
        assert_eq!(modo(&["suga-redis"]), Modo::Janela);
    }

    #[test]
    fn bandeira_de_texto_nao_abre_janela() {
        // `--version` e `--help` já abriram o aplicativo por engano: o
        // `main` tratava tudo que não fosse `--mcp` como "modo janela", e
        // quem rodava isso num terminal ficava com ele preso.
        assert_eq!(modo(&["suga-redis", "--version"]), Modo::Versao);
        assert_eq!(modo(&["suga-redis", "-V"]), Modo::Versao);
        assert_eq!(modo(&["suga-redis", "--help"]), Modo::Ajuda);
        assert_eq!(modo(&["suga-redis", "-h"]), Modo::Ajuda);
        assert_eq!(modo(&["suga-redis", "--mcp"]), Modo::Mcp);
    }

    #[test]
    fn a_ajuda_cita_os_dois_modos_e_a_versao() {
        let texto = ajuda();
        assert!(texto.contains("--mcp"));
        assert!(texto.contains(env!("CARGO_PKG_VERSION")));
    }
}
