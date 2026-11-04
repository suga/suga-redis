// Impede uma janela de console extra no Windows em release, NÃO REMOVER!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

/// Argumento que põe o binário em modo servidor MCP.
const MCP_FLAG: &str = "--mcp";

/// Um binário, dois modos. Sem argumento, sobe a janela; com `--mcp`, roda
/// como servidor MCP em stdio e nunca abre janela nenhuma — o Claude local
/// conversa com este processo pela entrada e saída padrão, e qualquer coisa
/// escrita em stdout que não seja protocolo quebraria essa conversa.
fn main() {
    if std::env::args().any(|arg| arg == MCP_FLAG) {
        if let Err(err) = suga_redis_lib::run_mcp() {
            eprintln!("suga-redis: {err}");
            std::process::exit(1);
        }
        return;
    }

    suga_redis_lib::run()
}
