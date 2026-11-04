import { render, screen, waitFor, within } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return { ...real, loadLevel: vi.fn() };
});

import { loadLevel } from "$lib/api";
import KeyTree from "$lib/KeyTree.svelte";
import type { Branch, TreeLevel } from "$lib/api";

const carregou = vi.mocked(loadLevel);

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

function nivel(prefix: string, children: Branch[], partes: Partial<TreeLevel> = {}): TreeLevel {
	return {
		prefix,
		children,
		scanned: 3_759_873,
		total_estimate: 3_759_873,
		capped: false,
		...partes,
	};
}

/** Cada prefixo responde com o seu nível, como o backend faria. */
function bancoDeNiveis(mapa: Record<string, TreeLevel>) {
	carregou.mockImplementation(async (_h, _db, prefix) => {
		const achado = mapa[prefix];
		if (!achado) throw new Error(`nível inesperado: ${prefix}`);
		return achado;
	});
}

function montar() {
	return render(KeyTree, {
		props: { hostId: "h1", db: 0, selectedKey: null, onSelect: vi.fn(), filter: "" },
	});
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("a raiz chega sozinha, com a contagem de cada prefixo", async () => {
	bancoDeNiveis({
		"": nivel("", [galho("wallet", "", 3_500_000), galho("cache", "", 211)]),
	});
	montar();

	expect(await screen.findByRole("button", { name: /wallet/ })).toHaveTextContent(
		"3.500.000",
	);
	expect(carregou).toHaveBeenCalledWith("h1", 0, "");
});

test("abrir um galho busca só o nível dele, não a árvore inteira", async () => {
	const user = userEvent.setup();
	bancoDeNiveis({
		"": nivel("", [galho("wallet", "", 3_500_000)]),
		"wallet:": nivel("wallet:", [galho("user", "wallet:", 900), folha("total", "wallet:")]),
	});
	montar();

	await user.click(await screen.findByRole("button", { name: /wallet/ }));

	// Pede exatamente o prefixo clicado — e nada além dele.
	await waitFor(() => expect(carregou).toHaveBeenCalledWith("h1", 0, "wallet:"));
	expect(carregou).toHaveBeenCalledTimes(2);
	expect(await screen.findByRole("button", { name: /user/ })).toBeInTheDocument();
});

test("clicar numa folha entrega a chave inteira", async () => {
	const user = userEvent.setup();
	const onSelect = vi.fn();
	bancoDeNiveis({ "": nivel("", [folha("solta", "")]) });
	render(KeyTree, {
		props: { hostId: "h1", db: 0, selectedKey: null, onSelect, filter: "" },
	});

	await user.click(await screen.findByRole("button", { name: /solta/ }));
	expect(onSelect).toHaveBeenCalledWith("solta");
});

test("nível cortado avisa, em vez de fingir que aquilo é tudo", async () => {
	bancoDeNiveis({
		"": nivel("", [galho("pedido", "", 1)], { capped: true }),
	});
	montar();

	expect(await screen.findByTestId("cortado-nivel")).toHaveTextContent(
		/não cabem na árvore|Busca/i,
	);
});

test("erro ao abrir um galho fica no galho, sem derrubar a árvore", async () => {
	const user = userEvent.setup();
	carregou.mockImplementation(async (_h, _db, prefix) => {
		if (prefix === "") return nivel("", [galho("wallet", "", 10)]);
		throw { kind: "unreachable", message: 'host "Valkey" não responde' };
	});
	montar();

	await user.click(await screen.findByRole("button", { name: /wallet/ }));

	expect(await screen.findByRole("alert")).toHaveTextContent("Valkey");
	// A raiz continua navegável.
	expect(screen.getByRole("button", { name: /wallet/ })).toBeInTheDocument();
});
