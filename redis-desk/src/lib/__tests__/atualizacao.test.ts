import { expect, test, vi } from "vitest";

vi.mock("@tauri-apps/plugin-updater", () => ({ check: vi.fn() }));
vi.mock("@tauri-apps/plugin-process", () => ({ relaunch: vi.fn() }));

import {
	fracao,
	instalar,
	procurar,
	tamanho,
	type Deps,
	type EventoDeDownload,
	type Pacote,
} from "$lib/atualizacao";

function pacote(parcial: Partial<Pacote> = {}): Pacote {
	return {
		version: "0.2.0",
		body: "  corrige o acento  ",
		downloadAndInstall: vi.fn().mockResolvedValue(undefined),
		...parcial,
	};
}

function deps(parcial: Partial<Deps> = {}): Deps {
	return {
		check: vi.fn().mockResolvedValue(null),
		relaunch: vi.fn().mockResolvedValue(undefined),
		...parcial,
	};
}

test("sem versão nova, não há nada a oferecer", async () => {
	expect(await procurar(deps())).toBeNull();
});

test("a versão nova chega com as notas aparadas", async () => {
	const achado = await procurar(deps({ check: vi.fn().mockResolvedValue(pacote()) }));

	expect(achado?.versao).toBe("0.2.0");
	expect(achado?.notas).toBe("corrige o acento");
});

test("release sem texto não vira a palavra 'undefined' na tela", async () => {
	const semCorpo = pacote({ body: undefined });
	const achado = await procurar(deps({ check: vi.fn().mockResolvedValue(semCorpo) }));

	expect(achado?.notas).toBe("");
});

test("estar sem rede não impede de usar o programa", async () => {
	// A procura acontece sozinha ao abrir o app. Se ela estourasse, um
	// aeroporto sem wi-fi viraria um app que não abre.
	const offline = deps({
		check: vi.fn().mockRejectedValue(new Error("error sending request")),
	});

	expect(await procurar(offline)).toBeNull();
});

test("sem Content-Length não se inventa porcentagem", () => {
	expect(fracao(500, null)).toBeNull();
	expect(fracao(500, 0)).toBeNull();
	expect(fracao(500, 1000)).toBe(0.5);
});

test("a fração nunca passa de 1", () => {
	// Já aconteceu de a soma dos pedaços passar do tamanho anunciado; uma
	// barra em 103% é um bug visível.
	expect(fracao(1200, 1000)).toBe(1);
});

test("o progresso soma os pedaços e termina cheio", async () => {
	const eventos: EventoDeDownload[] = [
		{ event: "Started", data: { contentLength: 1000 } },
		{ event: "Progress", data: { chunkLength: 400 } },
		{ event: "Progress", data: { chunkLength: 600 } },
		{ event: "Finished" },
	];
	const alvo = pacote({
		downloadAndInstall: vi.fn(async (aoEvento) => {
			for (const evento of eventos) aoEvento(evento);
		}),
	});
	const d = deps();
	const visto: (number | null)[] = [];

	await instalar(
		{ versao: "0.2.0", notas: "", pacote: alvo },
		(p) => visto.push(p.fracao),
		d,
	);

	expect(visto).toEqual([0, 0.4, 1, 1]);
	expect(d.relaunch).toHaveBeenCalledOnce();
});

test("o app reabre sozinho depois de instalar", async () => {
	// Sem isto a pessoa clica em atualizar, a janela fecha, e nada volta:
	// parece que o programa quebrou.
	const d = deps();
	await instalar({ versao: "0.2.0", notas: "", pacote: pacote() }, () => {}, d);

	expect(d.relaunch).toHaveBeenCalledOnce();
});

test("o tamanho é legível em qualquer escala", () => {
	expect(tamanho(7_340_032)).toBe("7.0 MB");
	expect(tamanho(204_800)).toBe("200 KB");
});
