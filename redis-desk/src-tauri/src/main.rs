// Impede uma janela de console extra no Windows em release, NÃO REMOVER!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use suga_redis_lib::{Modo, ajuda, modo_de, run, run_mcp};

/// Um binário, dois modos de trabalho e duas bandeiras de texto. A decisão
/// mora em `modo_de`, na biblioteca, onde dá para testá-la.
fn main() {
    match modo_de(std::env::args()) {
        Modo::Janela => run(),
        Modo::Mcp => {
            if let Err(err) = run_mcp() {
                eprintln!("suga-redis: {err}");
                std::process::exit(1);
            }
        }
        // Em stdout: são saídas pedidas por quem chamou, não diagnóstico.
        Modo::Versao => println!("Suga-Redis {}", env!("CARGO_PKG_VERSION")),
        Modo::Ajuda => println!("{}", ajuda()),
    }
}
