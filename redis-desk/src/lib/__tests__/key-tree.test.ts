import { render, screen, waitFor } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

// Só os comandos são trocados: `isEditable`, `textBytes` e os tipos seguem
// sendo os de verdade, porque reimplementá-los no dublê deixaria o teste
// concordando com uma cópia em vez de com o código que roda.
vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return {
		...real,
		readValue: vi.fn(),
		readValueMore: vi.fn(),
	};
});

import { readValue, readValueMore } from "$lib/api";
import KeyTree from "$lib/KeyTree.svelte";
import ValuePanel from "$lib/ValuePanel.svelte";
import type { Bytes, TreeNode, ValueView } from "$lib/api";

const lidos = vi.mocked(readValue);
const maisLidos = vi.mocked(readValueMore);

function galho(label: string, prefix: string, children: TreeNode[]): TreeNode {
	return {
		label,
		full_prefix: prefix,
		count: children.reduce((soma, filho) => soma + filho.count, 0),
		children,
		leaf_key: null,
	};
}

function folha(label: string, key: string): TreeNode {
	return { label, full_prefix: key, count: 1, children: [], leaf_key: key };
}

/**
 *   sessao:
 *     ativa:  (2 folhas)
 *     morta:  (1 folha)
 *   fatura:   (1 folha)
 */
function arvore(): TreeNode[] {
	return [
		galho("sessao", "sessao", [
			galho("ativa", "sessao:ativa", [
				folha("alpha", "sessao:ativa:alpha"),
				folha("beta", "sessao:ativa:beta"),
			]),
			galho("morta", "sessao:morta", [folha("gama", "sessao:morta:gama")]),
		]),
		galho("fatura", "fatura", [folha("2026", "fatura:2026")]),
	];
}

function texto(valor: string): Bytes {
	return { text: valor, base64: null, truncated: false };
}

function view(partes: Partial<ValueView> = {}): ValueView {
	return {
		key: "sessao:ativa:alpha",
		kind: "string",
		ttl_ms: null,
		size_bytes: 12,
		value: { kind: "string", data: texto("valor") },
		total_elements: 1,
		truncated: false,
		...partes,
	};
}

beforeEach(() => {
	vi.clearAllMocks();
	lidos.mockResolvedValue(view());
});

afterEach(() => {
	vi.useRealTimers();
});

test("clicar num galho abre os filhos e mostra a contagem", async () => {
	const user = userEvent.setup();
	render(KeyTree, {
		props: { nodes: arvore(), selectedKey: null, onSelect: vi.fn(), filter: "" },
	});

	// Fechado: a contagem aparece, os filhos não.
	expect(screen.getByRole("button", { name: /sessao/ })).toHaveTextContent("3");
	expect(screen.queryByRole("button", { name: /ativa/ })).not.toBeInTheDocument();

	await user.click(screen.getByRole("button", { name: /sessao/ }));

	expect(screen.getByRole("button", { name: /ativa/ })).toHaveTextContent("2");
	expect(screen.getByRole("button", { name: /morta/ })).toHaveTextContent("1");
});

test("clicar numa folha chama read_value com a chave inteira", async () => {
	const user = userEvent.setup();
	const onSelect = vi.fn();
	render(KeyTree, {
		props: { nodes: arvore(), selectedKey: null, onSelect, filter: "" },
	});

	await user.click(screen.getByRole("button", { name: /sessao/ }));
	await user.click(screen.getByRole("button", { name: /ativa/ }));
	await user.click(screen.getByRole("button", { name: /alpha/ }));

	// A chave inteira, não o rótulo do galho.
	expect(onSelect).toHaveBeenCalledWith("sessao:ativa:alpha");
});

test("filtro de nome esconde os galhos sem acerto e mantém o caminho dos que têm", () => {
	render(KeyTree, {
		props: { nodes: arvore(), selectedKey: null, onSelect: vi.fn(), filter: "gama" },
	});

	// O caminho inteiro até o acerto sobrevive e já vem aberto.
	expect(screen.getByRole("button", { name: /sessao/ })).toBeInTheDocument();
	expect(screen.getByRole("button", { name: /morta/ })).toBeInTheDocument();
	expect(screen.getByRole("button", { name: /gama/ })).toBeInTheDocument();

	// Os galhos sem acerto nenhum somem inteiros.
	expect(screen.queryByRole("button", { name: /ativa/ })).not.toBeInTheDocument();
	expect(screen.queryByRole("button", { name: /fatura/ })).not.toBeInTheDocument();
});

test('valor truncado mostra "mostrando 500 de 12.430" e o botão de carregar mais', async () => {
	const user = userEvent.setup();
	const itens = Array.from({ length: 500 }, (_, i) => texto(`item-${i}`));
	lidos.mockResolvedValue(
		view({
			key: "fila:pedidos",
			kind: "list",
			value: { kind: "list", data: itens },
			total_elements: 12430,
			truncated: true,
		}),
	);
	maisLidos.mockResolvedValue(
		view({
			key: "fila:pedidos",
			kind: "list",
			value: { kind: "list", data: [texto("item-500")] },
			total_elements: 12430,
			truncated: true,
		}),
	);

	render(ValuePanel, {
		props: { hostId: "h1", db: 0, keyName: "fila:pedidos" },
	});

	expect(await screen.findByTestId("truncado")).toHaveTextContent(
		"Mostrando 500 de 12.430",
	);

	await user.click(screen.getByRole("button", { name: "Carregar mais" }));

	// A próxima fatia começa onde a primeira parou.
	expect(maisLidos).toHaveBeenCalledWith("h1", 0, "fila:pedidos", 500);
	await waitFor(() =>
		expect(screen.getByTestId("truncado")).toHaveTextContent(
			"Mostrando 501 de 12.430",
		),
	);
});

test("tipo só leitura não mostra botão de salvar", async () => {
	lidos.mockResolvedValue(
		view({
			key: "tags:ativas",
			kind: "set",
			value: { kind: "set", data: [texto("a"), texto("b")] },
			total_elements: 2,
		}),
	);

	render(ValuePanel, {
		props: { hostId: "h1", db: 0, keyName: "tags:ativas" },
	});

	expect(await screen.findByText("Somente leitura")).toBeInTheDocument();
	expect(screen.queryByRole("button", { name: "Salvar" })).not.toBeInTheDocument();
});

test("ttl aparece em contagem regressiva e some quando a chave não expira", async () => {
	vi.useFakeTimers({ shouldAdvanceTime: true });

	lidos.mockResolvedValue(view({ ttl_ms: 65_000 }));
	const { unmount } = render(ValuePanel, {
		props: { hostId: "h1", db: 0, keyName: "sessao:ativa:alpha" },
	});

	await vi.waitFor(() =>
		expect(screen.getByTestId("ttl")).toHaveTextContent("1 min 5 s"),
	);

	await vi.advanceTimersByTimeAsync(5_000);
	expect(screen.getByTestId("ttl")).toHaveTextContent("1 min 0 s");

	unmount();

	// Chave sem prazo: não há contagem nenhuma na tela.
	lidos.mockResolvedValue(view({ ttl_ms: null }));
	render(ValuePanel, {
		props: { hostId: "h1", db: 0, keyName: "sessao:ativa:alpha" },
	});

	await vi.waitFor(() => expect(screen.getByTestId("kind")).toBeInTheDocument());
	expect(screen.queryByTestId("ttl")).not.toBeInTheDocument();
});
