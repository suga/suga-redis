/**
 * A tela em outro idioma.
 *
 * Um teste por componente convertido, com um texto representativo: o
 * objetivo não é reconferir cada palavra do catálogo — disso cuidam os
 * testes de `i18n.test.ts` — e sim provar que o componente lê do catálogo
 * em vez de carregar o português no corpo.
 */
import { render, screen } from "@testing-library/svelte";
import { afterEach, expect, test, vi } from "vitest";

import type { Host } from "$lib/api";
import HostList from "$lib/HostList.svelte";
import { definirIdioma } from "$lib/i18n.svelte";

afterEach(() => {
	definirIdioma("pt-BR");
});

function host(partes: Partial<Host> = {}): Host {
	return {
		id: "h1",
		name: "Redis de testes",
		addr: "127.0.0.1",
		port: 6379,
		db: 0,
		username: null,
		tls: false,
		env: "prod",
		...partes,
	};
}

function montarHostList() {
	render(HostList, {
		props: {
			hosts: [host()],
			selectedId: null,
			falhaExterna: null,
			onSelect: vi.fn(),
			onNew: vi.fn(),
			onEdit: vi.fn(),
			onChanged: vi.fn(),
			onDisconnected: vi.fn(),
		},
	});
}

test("a coluna de hosts fala inglês", () => {
	definirIdioma("en");
	montarHostList();

	expect(screen.getByRole("button", { name: "Connect" })).toBeInTheDocument();
	expect(screen.getByRole("button", { name: "New host" })).toBeInTheDocument();
	expect(screen.getByTestId("estado-conexao")).toHaveTextContent("never connected");
});

test("o carimbo de ambiente também é traduzido", () => {
	definirIdioma("es");
	montarHostList();

	// O carimbo é o dispositivo de segurança central: se ele não acompanhar
	// o idioma, quem lê espanhol perde justamente o aviso que mais importa.
	expect(screen.getByTestId("flag-ambiente")).toHaveTextContent("PRODUCCIÓN");
});

test("o diálogo de confirmação fala inglês", async () => {
	definirIdioma("en");
	const ConfirmDialog = (await import("$lib/ConfirmDialog.svelte")).default;

	render(ConfirmDialog, {
		props: {
			summary: "Set field \"saldo\" of wallet:1001",
			busy: false,
			onConfirm: vi.fn(),
			onCancel: vi.fn(),
		},
	});

	expect(screen.getByRole("heading")).toHaveTextContent("Confirm the change");
	expect(screen.getByRole("button", { name: "Cancel" })).toBeInTheDocument();
	// O carimbo do diálogo é o mesmo dispositivo da barra do topo.
	expect(screen.getByText("PRODUCTION")).toBeInTheDocument();
});

test("a faixa sem host aberto fala espanhol", async () => {
	definirIdioma("es");
	const EnvBand = (await import("$lib/EnvBand.svelte")).default;

	render(EnvBand, { props: { host: null, latencyMs: null } });

	expect(screen.getByTestId("env-band")).toHaveTextContent("Ningún host abierto");
});

test("o formulário de host fala inglês", async () => {
	definirIdioma("en");
	const HostForm = (await import("$lib/HostForm.svelte")).default;

	render(HostForm, { props: { host: null, onSaved: vi.fn(), onCancel: vi.fn() } });

	expect(screen.getByRole("heading")).toHaveTextContent("New host");
	expect(screen.getByText("Address")).toBeInTheDocument();
	expect(screen.getByRole("button", { name: "Save" })).toBeInTheDocument();
	// A lista de ambientes é onde se escolhe o que a janela vai gritar depois.
	expect(screen.getByRole("option", { name: "Production" })).toBeInTheDocument();
});

test("o diálogo de nova chave fala espanhol", async () => {
	definirIdioma("es");
	const NewKeyDialog = (await import("$lib/NewKeyDialog.svelte")).default;

	render(NewKeyDialog, { props: { busy: false, onCreate: vi.fn(), onCancel: vi.fn() } });

	expect(screen.getByRole("heading")).toHaveTextContent("Nueva clave");
	expect(screen.getByRole("button", { name: "Crear" })).toBeInTheDocument();
});

test("o editor de string e a validação de JSON falam inglês", async () => {
	definirIdioma("en");
	const StringEditor = (await import("$lib/editors/StringEditor.svelte")).default;
	const { validarJson } = await import("$lib/json");

	render(StringEditor, {
		props: {
			view: {
				key: "user:1001",
				kind: "string",
				ttl_ms: null,
				size_bytes: 7,
				value: {
					kind: "string",
					data: { text: '{"a":1}', base64: null, truncated: false },
				},
				total_elements: 1,
				truncated: false,
			},
			onSubmit: vi.fn(),
			busy: false,
		},
	});

	expect(screen.getByRole("button", { name: "Format JSON" })).toBeInTheDocument();
	expect(screen.getByRole("button", { name: "Save" })).toBeInTheDocument();
	// A mensagem do validador é texto de tela como qualquer outro.
	expect(validarJson('{"a": 1,}')).toBe("Invalid JSON at line 1, column 9.");
});

test("o painel de valor fala espanhol", async () => {
	definirIdioma("es");
	const ValuePanel = (await import("$lib/ValuePanel.svelte")).default;

	render(ValuePanel, {
		props: { hostId: "h1", db: 0, keyName: "user:1001", onKeyGone: vi.fn() },
	});

	expect(screen.getByText("Cargando la clave…")).toBeInTheDocument();
});

test("dá para trocar o idioma pela tela, e a escolha fica guardada", async () => {
	const userEvent = (await import("@testing-library/user-event")).default;
	const user = userEvent.setup();
	localStorage.clear();
	montarHostList();

	// O seletor vive na coluna de hosts: é a única parte da janela que está
	// na tela antes de qualquer host abrir.
	const seletor = screen.getByRole("combobox", { name: "Idioma" });
	await user.selectOptions(seletor, "en");

	expect(screen.getByRole("button", { name: "Connect" })).toBeInTheDocument();
	expect(localStorage.getItem("idioma")).toBe("en");
});

test("o aviso de valor binário também segue o idioma", async () => {
	definirIdioma("en");
	const ReadOnlyView = (await import("$lib/editors/ReadOnlyView.svelte")).default;

	render(ReadOnlyView, {
		props: {
			value: {
				kind: "string",
				data: { text: null, base64: "A".repeat(4096), truncated: false },
			},
		},
	});

	// O número vinha com o separador do português, fixo, enquanto o resto
	// da tela já seguia o idioma escolhido.
	expect(screen.getByText("(binary, 3,072 bytes)")).toBeInTheDocument();
});
