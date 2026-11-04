//! Biblioteca do suga-redis: o núcleo, a fachada de comandos e a montagem
//! do app Tauri. O binário (`main.rs`) é só quem escolhe entre os dois modos.

pub mod approval;
pub mod commands;
pub mod core;
pub mod mcp;

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
