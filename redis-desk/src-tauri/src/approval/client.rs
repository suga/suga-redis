//! O lado do servidor MCP: pede, espera, e aceita o não.

use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::{ApprovalError, ApprovalRequest, ApprovalVerdict, Result};

/// Manda o pedido ao aplicativo e espera o veredito.
///
/// Sem aplicativo aberto não há socket para conectar, e a resposta é
/// `AppNotRunning` — uma mensagem que ENSINA o que fazer, porque quem a lê é
/// o Claude e é ele quem vai explicá-la para a pessoa.
///
/// Estourar a espera devolve `TimedOut`, não um erro de E/S: ninguém
/// respondeu é um desfecho previsto deste protocolo, não uma falha dele.
pub async fn request_approval(
    socket: &Path,
    request: ApprovalRequest,
    timeout: Duration,
) -> Result<ApprovalVerdict> {
    let stream = match UnixStream::connect(socket).await {
        Ok(stream) => stream,
        Err(err)
            if matches!(
                err.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(ApprovalError::AppNotRunning);
        }
        Err(err) => return Err(err.into()),
    };

    let (leitura, mut escrita) = stream.into_split();
    let pedido =
        serde_json::to_string(&request).map_err(|err| ApprovalError::Protocol(err.to_string()))?;
    escrita.write_all(pedido.as_bytes()).await?;
    escrita.write_all(b"\n").await?;
    escrita.flush().await?;

    let mut linhas = BufReader::new(leitura).lines();
    match tokio::time::timeout(timeout, linhas.next_line()).await {
        Err(_) => Ok(ApprovalVerdict::TimedOut),
        Ok(Ok(Some(linha))) => {
            serde_json::from_str(&linha).map_err(|err| ApprovalError::Protocol(err.to_string()))
        }
        Ok(Ok(None)) => Err(ApprovalError::AppWentAway),
        Ok(Err(err)) => Err(err.into()),
    }
}
