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
import ValuePanel from "$lib/ValuePanel.svelte";
import type { Bytes, ValueView } from "$lib/api";

const lidos = vi.mocked(readValue);
const maisLidos = vi.mocked(readValueMore);

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

test("carregar mais emenda as páginas de uma string, em vez de reler o começo", async () => {
	const user = userEvent.setup();
	// Um valor de 30 bytes que chega em duas páginas de 15.
	const comeco = "AAAAAAAAAAAAAAA";
	const fim = "BBBBBBBBBBBBBBB";
	lidos.mockResolvedValue(
		view({
			key: "grande:valor",
			kind: "string",
			value: { kind: "string", data: texto(comeco) },
			total_elements: 30,
			truncated: true,
		}),
	);
	maisLidos.mockResolvedValue(
		view({
			key: "grande:valor",
			kind: "string",
			value: { kind: "string", data: texto(fim) },
			total_elements: 30,
			truncated: false,
		}),
	);

	render(ValuePanel, { props: { hostId: "h1", db: 0, keyName: "grande:valor" } });

	expect(await screen.findByTestId("truncado")).toHaveTextContent("Mostrando 15 de 30");
	await user.click(screen.getByRole("button", { name: "Carregar mais" }));

	// A próxima página começa onde a primeira parou, contada em BYTES.
	expect(maisLidos).toHaveBeenCalledWith("h1", 0, "grande:valor", 15);
	// E o painel passa a mostrar o valor inteiro, não só a última fatia.
	await waitFor(() =>
		expect(screen.getByText(comeco + fim, { exact: false })).toBeInTheDocument(),
	);
});
