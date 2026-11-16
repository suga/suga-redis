/**
 * As quatro abas e o pedido da LLM em outro idioma.
 *
 * Arquivo próprio porque estes componentes precisam de `$lib/api` dublado
 * inteiro, e um `vi.mock` vale para o arquivo todo.
 */
import { render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", () => ({
	errorMessage: (err: unknown) => String(err),
	search: vi.fn(async () => ({ hits: [], index_age_secs: -1, stale_keys_removed: 0 })),
	listHistory: vi.fn(async () => []),
	rollback: vi.fn(),
	logs: vi.fn(async () => []),
	approve: vi.fn(),
	deny: vi.fn(),
	NEVER_SCANNED: -1,
}));

import { definirIdioma } from "$lib/i18n.svelte";

beforeEach(() => {
	vi.clearAllMocks();
});

afterEach(() => {
	definirIdioma("pt-BR");
});

test("a busca fala inglês", async () => {
	definirIdioma("en");
	const SearchPanel = (await import("$lib/SearchPanel.svelte")).default;

	render(SearchPanel, { props: { hostId: "h1", db: 0, onOpenKey: vi.fn() } });

	expect(screen.getByRole("button", { name: "Search" })).toBeInTheDocument();
	expect(
		screen.getByPlaceholderText("text inside any key or value"),
	).toBeInTheDocument();
});

test("o histórico fala espanhol", async () => {
	definirIdioma("es");
	const HistoryPanel = (await import("$lib/HistoryPanel.svelte")).default;

	render(HistoryPanel, { props: { hostId: "h1", onRolledBack: vi.fn() } });

	expect(await screen.findByTestId("vazio")).toHaveTextContent(
		"Ningún cambio todavía.",
	);
});

test("o log fala inglês", async () => {
	definirIdioma("en");
	const LogPanel = (await import("$lib/LogPanel.svelte")).default;

	render(LogPanel, { props: {} });

	expect(screen.getByRole("button", { name: "Refresh" })).toBeInTheDocument();
});

test("o pedido de escrita da LLM fala espanhol", async () => {
	definirIdioma("es");
	const ApprovalDialog = (await import("$lib/ApprovalDialog.svelte")).default;

	render(ApprovalDialog, {
		props: {
			request: {
				id: "p1",
				host_id: "h1",
				host_name: "pagamentos-prod",
				env: "prod",
				db: 0,
				key: "config:limite-diario",
				summary: "Definir el valor de config:limite-diario",
				op: { op: "set_string", value: { text: "8000", base64: null, truncated: false } },
				asked_at: 0,
			},
			onSettled: vi.fn(),
		},
	});

	// A frase do título é o que diz a quem lê que quem pediu não foi ela.
	expect(screen.getByRole("heading")).toHaveTextContent("Claude pidió una escritura");
	expect(screen.getByRole("button", { name: "Rechazar" })).toBeInTheDocument();
});

test("o pedido da LLM descreve a escrita no idioma de quem lê", async () => {
	definirIdioma("en");
	const ApprovalDialog = (await import("$lib/ApprovalDialog.svelte")).default;

	render(ApprovalDialog, {
		props: {
			request: {
				id: "p1",
				host_id: "h1",
				host_name: "pagamentos-prod",
				env: "prod",
				db: 0,
				key: "config:limite-diario",
				// A frase que veio do Rust está em português; a tela não a usa.
				summary: "Definir o valor de config:limite-diario",
				op: { op: "set_string", value: { text: "8000", base64: null, truncated: false } },
				asked_at: 0,
			},
			onSettled: vi.fn(),
		},
	});

	expect(screen.getByTestId("resumo-pedido")).toHaveTextContent(
		"Set the value of config:limite-diario",
	);
});

test("o histórico redige a frase no idioma de quem lê, e respeita o que é velho", async () => {
	const { listHistory } = await import("$lib/api");
	vi.mocked(listHistory).mockResolvedValue([
		{
			id: 2,
			host_id: "h1",
			host_name: "pagamentos-prod",
			env: "prod",
			db: 0,
			key: "wallet:1001",
			summary: "Definir o campo \"saldo\" de wallet:1001",
			op: { op: "set_hash_field", field: "saldo", value: { text: "10", base64: null, truncated: false } },
			before_json: null,
			after_json: null,
			actor: "human",
			at: 0,
			rolled_back: false,
		},
		{
			// Entrada gravada antes da coluna existir: só resta a frase.
			id: 1,
			host_id: "h1",
			host_name: "pagamentos-prod",
			env: "prod",
			db: 0,
			key: "user:1001",
			summary: "Definir o valor de user:1001",
			op: null,
			before_json: null,
			after_json: null,
			actor: "human",
			at: 0,
			rolled_back: false,
		},
	]);

	definirIdioma("en");
	const HistoryPanel = (await import("$lib/HistoryPanel.svelte")).default;
	render(HistoryPanel, { props: { hostId: "h1", onRolledBack: vi.fn() } });

	const entradas = await screen.findAllByTestId("entrada");
	expect(entradas[0]).toHaveTextContent("Set the field “saldo” of wallet:1001");
	// A antiga não tem como ser redigida de novo: mostra o que foi gravado.
	expect(entradas[1]).toHaveTextContent("Definir o valor de user:1001");
});

test("o botão de varrer dentro do aviso de índice velho também fala o idioma", async () => {
	const { search } = await import("$lib/api");
	vi.mocked(search).mockResolvedValue({
		hits: [],
		// Índice velho: é o estado que faz aparecer o aviso com o botão.
		index_age_secs: 10_800,
		stale_keys_removed: 0,
		scanned: 0,
		elapsed_ms: 1,
	});

	definirIdioma("en");
	const SearchPanel = (await import("$lib/SearchPanel.svelte")).default;
	const user = (await import("@testing-library/user-event")).default.setup();
	render(SearchPanel, { props: { hostId: "h1", db: 0, onOpenKey: vi.fn() } });

	await user.type(screen.getByPlaceholderText("text inside any key or value"), "x");
	await user.click(screen.getByRole("button", { name: "Search" }));

	expect(await screen.findByRole("button", { name: "Rescan" })).toBeInTheDocument();
});

test("o tipo de cada linha do log é traduzido", async () => {
	const { logs } = await import("$lib/api");
	vi.mocked(logs).mockResolvedValue([
		{
			at_ms: 0,
			kind: "failure",
			host: "pagamentos-prod",
			db: 0,
			operation: "connect",
			duration_ms: 12,
			detail: "connection refused",
		},
	]);

	definirIdioma("es");
	const LogPanel = (await import("$lib/LogPanel.svelte")).default;
	render(LogPanel, { props: {} });

	expect(await screen.findByText("fallo")).toBeInTheDocument();
});
