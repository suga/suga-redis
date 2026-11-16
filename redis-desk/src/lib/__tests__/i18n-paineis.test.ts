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
	history: vi.fn(async () => []),
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
