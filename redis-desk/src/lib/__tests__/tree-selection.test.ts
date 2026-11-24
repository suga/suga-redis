// A regra de CSS da marca é conferida na fonte (ver o último teste).
// @ts-expect-error type error without @types/node package
import { readFileSync } from "node:fs";

import { createEvent, fireEvent, render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, treeLevel: vi.fn() };
});

import { treeLevel } from "$lib/api";
import KeyTree from "$lib/KeyTree.svelte";
import type { Branch } from "$lib/api";

const nivel = vi.mocked(treeLevel);

function galho(label: string, prefixoPai: string, count: number): Branch {
	return { label, full_prefix: `${prefixoPai}${label}:`, count, leaf_key: null };
}

function folha(label: string, prefixoPai: string): Branch {
	return {
		label,
		full_prefix: `${prefixoPai}${label}:`,
		count: 1,
		leaf_key: `${prefixoPai}${label}`,
	};
}

/** Cada prefixo responde com os seus filhos, como a trie em memória faria. */
function arvore(mapa: Record<string, Branch[]>) {
	nivel.mockImplementation(async (_h, _db, prefix) =>
		mapa[prefix] ? { children: mapa[prefix], truncated: false } : null,
	);
}

/** Três prefixos e uma chave solta: a seleção só pode pegar os três. */
const RAIZ: Branch[] = [
	galho("catalogo", "", 24),
	galho("pedido", "", 12),
	galho("sessao", "", 7),
	folha("solta", ""),
];

function montar(onSelect = vi.fn()) {
	const onMenuPrefixo = vi.fn();
	const onMenuChave = vi.fn();
	render(KeyTree, {
		props: {
			hostId: "h1",
			db: 0,
			root: RAIZ,
			version: 0,
			selectedKey: null,
			onSelect,
			filter: "",
			onMenuPrefixo,
			onMenuChave,
		},
	});
	return { onMenuPrefixo, onMenuChave };
}

function linha(nome: RegExp) {
	return screen.getByRole("button", { name: nome });
}

/** O clique com a tecla de juntar segurada. */
async function cliqueComando(user: ReturnType<typeof userEvent.setup>, el: HTMLElement) {
	await user.keyboard("{Meta>}");
	await user.click(el);
	await user.keyboard("{/Meta}");
}

async function cliqueShift(user: ReturnType<typeof userEvent.setup>, el: HTMLElement) {
	await user.keyboard("{Shift>}");
	await user.click(el);
	await user.keyboard("{/Shift}");
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("contextmenu numa linha de prefixo chama preventDefault e onMenuPrefixo", () => {
	const { onMenuPrefixo } = montar();

	const evento = createEvent.contextMenu(linha(/catalogo/), {
		clientX: 120,
		clientY: 240,
	});
	fireEvent(linha(/catalogo/), evento);

	// Sem o preventDefault, o menu do sistema operacional abre por cima do nosso.
	expect(evento.defaultPrevented).toBe(true);
	expect(onMenuPrefixo).toHaveBeenCalledWith(["catalogo:"], 120, 240);
});

test("Shift+F10 na linha com foco abre o menu ancorado nela", async () => {
	const user = userEvent.setup();
	const { onMenuPrefixo } = montar();

	const alvo = linha(/pedido/);
	// O jsdom não faz layout: sem uma caixa fingida, toda linha fica em
	// 0,0 e o teste não distinguiria "ancorado nela" de "ancorado em nada".
	alvo.getBoundingClientRect = () =>
		({ left: 40, bottom: 130 }) as DOMRect;
	alvo.focus();

	await user.keyboard("{Shift>}{F10}{/Shift}");

	expect(onMenuPrefixo).toHaveBeenCalledWith(["pedido:"], 40, 130);
});

test("contextmenu numa linha de chave chama onMenuChave", () => {
	const { onMenuPrefixo, onMenuChave } = montar();

	fireEvent.contextMenu(linha(/solta/), { clientX: 10, clientY: 20 });

	expect(onMenuChave).toHaveBeenCalledWith("solta", 10, 20);
	expect(onMenuPrefixo).not.toHaveBeenCalled();
});

test("⌘+clique junta prefixos à seleção", async () => {
	const user = userEvent.setup();
	montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	expect(linha(/pedido/)).not.toHaveClass("marcada");
	// Juntar à seleção não é abrir: nenhum galho foi lido.
	expect(nivel).not.toHaveBeenCalled();

	// E junta nos dois sentidos: o mesmo clique tira de volta.
	await cliqueComando(user, linha(/catalogo/));
	expect(linha(/catalogo/)).not.toHaveClass("marcada");
});

test("⌘+clique numa folha não junta nada", async () => {
	const user = userEvent.setup();
	const onSelect = vi.fn();
	montar(onSelect);

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/solta/));

	// A chave solta não entra na seleção — e também não leva a seleção junto.
	expect(linha(/solta/)).not.toHaveClass("marcada");
	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(onSelect).not.toHaveBeenCalled();
});

test("Shift+clique pega a faixa", async () => {
	const user = userEvent.setup();
	montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueShift(user, linha(/sessao/));

	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/pedido/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	// A chave solta está fora da faixa, e não entraria nem dentro dela.
	expect(linha(/solta/)).not.toHaveClass("marcada");
});

test("⌘+A seleciona os prefixos de primeiro nível", async () => {
	const user = userEvent.setup();
	arvore({ "catalogo:": [galho("eventos", "catalogo:", 2)] });
	montar();

	await user.click(linha(/catalogo/));
	const dentro = await screen.findByRole("button", { name: /eventos/ });

	linha(/pedido/).focus();
	await user.keyboard("{Meta>}a{/Meta}");

	expect(linha(/catalogo/)).toHaveClass("marcada");
	expect(linha(/pedido/)).toHaveClass("marcada");
	expect(linha(/sessao/)).toHaveClass("marcada");
	// Primeiro nível é primeiro nível: o que está dentro de um prefixo
	// marcado já vai junto, e marcá-lo também faria o menu contar duas vezes.
	expect(dentro).not.toHaveClass("marcada");
	expect(linha(/solta/)).not.toHaveClass("marcada");
});

test("contextmenu numa linha JÁ selecionada manda a seleção inteira", async () => {
	const user = userEvent.setup();
	const { onMenuPrefixo } = montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	fireEvent.contextMenu(linha(/sessao/), { clientX: 5, clientY: 6 });

	expect(onMenuPrefixo).toHaveBeenCalledWith(["catalogo:", "sessao:"], 5, 6);
	expect(linha(/catalogo/)).toHaveClass("marcada");
});

test("contextmenu numa linha fora da seleção limpa a seleção e manda só ela", async () => {
	const user = userEvent.setup();
	const { onMenuPrefixo } = montar();

	await cliqueComando(user, linha(/catalogo/));
	await cliqueComando(user, linha(/sessao/));

	fireEvent.contextMenu(linha(/pedido/), { clientX: 5, clientY: 6 });

	// Quem errou o alvo perde a seleção, em vez de apagar o que não queria.
	expect(onMenuPrefixo).toHaveBeenCalledWith(["pedido:"], 5, 6);
	expect(linha(/catalogo/)).not.toHaveClass("marcada");
	expect(linha(/sessao/)).not.toHaveClass("marcada");
});

test("fechar um galho tira da seleção o que sumiu da tela", async () => {
	const user = userEvent.setup();
	arvore({ "catalogo:": [galho("eventos", "catalogo:", 2)] });
	montar();

	await user.click(linha(/catalogo/));
	await cliqueComando(user, await screen.findByRole("button", { name: /eventos/ }));
	expect(linha(/eventos/)).toHaveClass("marcada");

	// Fecha e reabre: o que saiu da tela saiu da seleção, para o menu não
	// falar de prefixos que ninguém mais vê.
	await user.click(linha(/catalogo/));
	await user.click(linha(/catalogo/));

	expect(await screen.findByRole("button", { name: /eventos/ })).not.toHaveClass(
		"marcada",
	);
});

test("a marca de selecionado não usa a classe de produção", async () => {
	const user = userEvent.setup();
	montar();

	await cliqueComando(user, linha(/catalogo/));

	const marcada = linha(/catalogo/);
	expect(marcada).toHaveClass("marcada");
	expect(marcada.className).not.toMatch(/prod/i);
	// O rótulo sai como está escrito no banco: caixa alta é ambiente.
	expect(marcada).toHaveTextContent("catalogo");

	// O vitest não aplica o CSS do componente — `getComputedStyle` aqui não
	// enxerga a regra —, então ela é conferida na fonte: `--prod` e caixa
	// alta significam só o ambiente nesta tela, e emprestá-los para a
	// seleção deixaria o aviso de produção ambíguo.
	const fonte = readFileSync("src/lib/KeyTree.svelte", "utf8");
	const inicio = fonte.indexOf(".marcada {");
	expect(inicio).toBeGreaterThan(-1);
	const corpo = fonte.slice(inicio, fonte.indexOf("}", inicio));
	expect(corpo).not.toMatch(/--prod/);
	expect(corpo).not.toMatch(/uppercase/);
});
