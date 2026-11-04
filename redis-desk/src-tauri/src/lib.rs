//! Biblioteca do redis-desk: o núcleo, a fachada de comandos e a montagem
//! do app Tauri. O binário (`main.rs`) é só quem escolhe entre os dois modos.

pub mod commands;
pub mod core;

/// Sobe a janela do app. Este é o modo normal, sem argumento nenhum.
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| commands::setup(app))
        .invoke_handler(tauri::generate_handler![
            commands::cmd_list_hosts,
            commands::cmd_add_host,
            commands::cmd_update_host,
            commands::cmd_remove_host,
            commands::cmd_test_host,
            commands::cmd_list_dbs,
            commands::cmd_load_tree,
            commands::cmd_rescan,
            commands::cmd_scan_status,
            commands::cmd_read_value,
            commands::cmd_read_value_more,
            commands::cmd_write_value,
            commands::cmd_list_history,
            commands::cmd_rollback,
            commands::cmd_search,
        ])
        .run(tauri::generate_context!())
        .expect("erro ao subir a janela do redis-desk");
}

/// Roda como servidor MCP em stdio, sem janela nenhuma.
///
/// Ainda não implementado: é a Task 15 do plano. Quem chamar `--mcp` hoje
/// recebe esta recusa explícita, em vez de uma janela abrindo por engano ou
/// de um processo que fica mudo esperando um protocolo que ninguém fala.
pub fn run_mcp() -> std::io::Result<()> {
    Err(std::io::Error::other(
        "o modo servidor MCP (--mcp) ainda não foi implementado",
    ))
}
