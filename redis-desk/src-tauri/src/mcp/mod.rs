//! Servidor MCP em stdio: o modo `--mcp` do binário.
//!
//! O processo do servidor MCP é SEPARADO do processo da janela. Os dois
//! abrem o mesmo `hosts.json` e o mesmo SQLite (por isso as pragmas de
//! `core::sqlite`), e cada um mantém suas próprias conexões com o Redis.
//! Isso é de propósito: o Claude local consegue ler um banco sem o
//! aplicativo estar aberto.
//!
//! Nada aqui escreve no Redis. As ferramentas de escrita (Task 16) não
//! conseguem agir sozinhas: elas pedem ao aplicativo aberto, e o dono libera
//! na mão.

pub mod tools;

use std::sync::Arc;

use rmcp::ServiceExt;
use rmcp::transport::stdio;

use crate::commands::{AppState, data_dir};
use tools::Tools;

/// Sobe o servidor MCP em stdio e só volta quando o outro lado desligar.
///
/// NADA pode ser impresso em stdout por este processo além do protocolo:
/// stdout É o canal. Qualquer diagnóstico vai para stderr.
pub async fn run_stdio() -> std::io::Result<()> {
    let dir = data_dir()?;
    let state = AppState::open(&dir)
        .map_err(|err| std::io::Error::other(format!("ao abrir os dados do app: {err}")))?;

    let servico = Tools::new(Arc::new(state))
        .serve(stdio())
        .await
        .map_err(|err| std::io::Error::other(format!("ao subir o servidor MCP: {err}")))?;

    servico
        .waiting()
        .await
        .map_err(|err| std::io::Error::other(format!("o servidor MCP terminou com erro: {err}")))?;
    Ok(())
}
