//! O lado da janela: escuta o socket unix e põe os pedidos na fila.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use super::{ApprovalQueue, ApprovalRequest};

/// Só o dono lê e escreve. Qualquer outra permissão deixaria outro usuário
/// da máquina mandar pedidos de escrita — ou, pior, respondê-los.
const SOCKET_MODE: u32 = 0o600;

/// Onde o socket fica.
///
/// No mesmo diretório dos dados, e não num diretório de runtime do sistema,
/// porque é o único caminho que os dois processos já calculam igual (ver
/// `commands::data_dir`) — e um socket que os dois lados não acham no mesmo
/// lugar é o mesmo que socket nenhum.
pub fn socket_path() -> std::io::Result<PathBuf> {
    Ok(crate::commands::data_dir()?.join("suga-redis.sock"))
}

/// Escuta pedidos de escrita até o processo acabar.
pub async fn serve(queue: Arc<ApprovalQueue>, socket: &Path) -> std::io::Result<()> {
    if let Some(parent) = socket.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Um socket de uma execução anterior que não foi fechada direito impede
    // o `bind`. Remover é seguro: se houvesse outro app vivo escutando ali,
    // o `bind` seguinte é que falharia, e é ele quem manda.
    if socket.exists() {
        std::fs::remove_file(socket)?;
    }

    let listener = UnixListener::bind(socket)?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(SOCKET_MODE))?;

    loop {
        let (stream, _) = listener.accept().await?;
        let queue = queue.clone();
        tokio::spawn(async move {
            // Uma conexão problemática não pode derrubar quem escuta: o
            // aplicativo tem que continuar aceitando pedidos.
            let _ = atender(queue, stream).await;
        });
    }
}

/// Uma conexão: um pedido, um veredito.
async fn atender(queue: Arc<ApprovalQueue>, stream: UnixStream) -> std::io::Result<()> {
    let (leitura, mut escrita) = stream.into_split();
    let mut linhas = BufReader::new(leitura).lines();

    let Some(linha) = linhas.next_line().await? else {
        return Ok(());
    };
    let request: ApprovalRequest = serde_json::from_str(&linha)
        .map_err(|err| std::io::Error::other(format!("pedido malformado: {err}")))?;

    let id = request.id.clone();
    let espera = queue.submit(request);

    // Corre o veredito contra o desaparecimento de quem pediu. Sem esta
    // corrida, uma LLM que desistiu (ou um processo MCP que morreu)
    // deixaria o pedido na tela para sempre, esperando uma resposta que
    // ninguém mais vai receber.
    let verdict = tokio::select! {
        resultado = espera => match resultado {
            Ok(verdict) => verdict,
            Err(_) => return Ok(()),
        },
        _ = linhas.next_line() => {
            queue.forget(&id);
            return Ok(());
        }
    };

    let resposta = serde_json::to_string(&verdict)
        .map_err(|err| std::io::Error::other(format!("veredito não serializável: {err}")))?;
    escrita.write_all(resposta.as_bytes()).await?;
    escrita.write_all(b"\n").await?;
    escrita.flush().await?;
    Ok(())
}

/// Garante que o veredito tem uma forma estável no fio, independente de
/// mudanças na representação interna.
#[cfg(test)]
mod wire_tests {
    use super::super::ApprovalVerdict;

    #[test]
    fn verdict_travels_as_snake_case() {
        assert_eq!(
            serde_json::to_string(&ApprovalVerdict::Allowed).unwrap(),
            "\"allowed\""
        );
        assert_eq!(
            serde_json::to_string(&ApprovalVerdict::TimedOut).unwrap(),
            "\"timed_out\""
        );
    }
}
