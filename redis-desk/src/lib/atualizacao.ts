/**
 * A busca por uma versão nova, e a instalação dela.
 *
 * O app pergunta ao GitHub se existe versão mais nova que a sua, e só
 * instala depois que a pessoa disser que sim. Nada aqui atualiza sozinho:
 * este é um cliente de Redis de produção, e trocar o programa debaixo de
 * alguém no meio de uma edição é a última coisa que ele deveria fazer.
 *
 * O pacote baixado vem assinado com a chave do projeto, e o plugin recusa o
 * que não confere antes de instalar. É isso que torna seguro apontar o
 * updater para uma URL pública: o endereço qualquer um alcança, mas só quem
 * tem a chave privada consegue produzir algo que o app aceite.
 *
 * As dependências entram por parâmetro (`deps`) porque o teste precisa
 * rodar sem o Tauri por baixo — fora do app, `check` e `relaunch` não
 * existem.
 */

import { check } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";

/** O que o plugin devolve quando existe versão nova. */
export interface Pacote {
	version: string;
	body?: string;
	downloadAndInstall(
		aoEvento: (evento: EventoDeDownload) => void,
	): Promise<void>;
}

/** Os eventos de progresso do plugin, nos nomes que ele usa. */
export type EventoDeDownload =
	| { event: "Started"; data: { contentLength?: number } }
	| { event: "Progress"; data: { chunkLength: number } }
	| { event: "Finished" };

export interface Achado {
	versao: string;
	/** As notas da release. Vazio quando a release não trouxe texto. */
	notas: string;
	pacote: Pacote;
}

export interface Deps {
	check: () => Promise<Pacote | null>;
	relaunch: () => Promise<void>;
}

const PADRAO: Deps = {
	check: () => check() as Promise<Pacote | null>,
	relaunch,
};

/**
 * Pergunta se existe versão nova. `null` quando já está na mais recente.
 *
 * Nunca estoura: estar sem rede, atrás de um proxy ou com o GitHub fora do
 * ar é situação normal, e nenhuma delas pode impedir alguém de abrir o
 * programa e trabalhar. A falha vira `null` — o app segue como está.
 */
export async function procurar(deps: Deps = PADRAO): Promise<Achado | null> {
	try {
		const pacote = await deps.check();
		if (!pacote) return null;
		return {
			versao: pacote.version,
			notas: pacote.body?.trim() ?? "",
			pacote,
		};
	} catch {
		return null;
	}
}

/**
 * A fração já baixada, de 0 a 1 — ou `null` quando não dá para saber.
 *
 * Nem todo servidor manda `Content-Length`, e sem ele não existe
 * porcentagem honesta. Inventar uma (uma barra que anda sozinha até 90%) é
 * mentir para quem olha; `null` deixa a interface mostrar quanto já veio em
 * vez de uma barra que não significa nada.
 */
export function fracao(baixado: number, total: number | null): number | null {
	if (total === null || total <= 0) return null;
	return Math.min(1, baixado / total);
}

export interface Progresso {
	baixado: number;
	total: number | null;
	fracao: number | null;
}

/**
 * Baixa, instala e reabre o app.
 *
 * O `relaunch` é parte do mesmo gesto: no macOS o app precisa ser reaberto
 * para a versão nova valer, e deixar isso para a pessoa significa alguém
 * achando que a atualização não funcionou.
 */
export async function instalar(
	achado: Achado,
	aoProgresso: (p: Progresso) => void,
	deps: Deps = PADRAO,
): Promise<void> {
	let baixado = 0;
	let total: number | null = null;

	await achado.pacote.downloadAndInstall((evento) => {
		switch (evento.event) {
			case "Started":
				total = evento.data.contentLength ?? null;
				break;
			case "Progress":
				baixado += evento.data.chunkLength;
				break;
			case "Finished":
				baixado = total ?? baixado;
				break;
		}
		aoProgresso({ baixado, total, fracao: fracao(baixado, total) });
	});

	await deps.relaunch();
}

/** `12.3 MB` a partir de um número de bytes, para a linha de progresso. */
export function tamanho(bytes: number): string {
	const mb = bytes / (1024 * 1024);
	if (mb >= 1) return `${mb.toFixed(1)} MB`;
	return `${Math.round(bytes / 1024)} KB`;
}
