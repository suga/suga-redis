import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

vi.mock("$lib/api", async (importOriginal) => {
	const real = await importOriginal<typeof import("$lib/api")>();
	return {
		...real,
		connect: vi.fn(),
		disconnect: vi.fn(),
		testHost: vi.fn(),
		removeHost: vi.fn(),
	};
});

import { connect, disconnect } from "$lib/api";
import HostList from "$lib/HostList.svelte";
import type { Host } from "$lib/api";

const conectou = vi.mocked(connect);
const desconectou = vi.mocked(disconnect);

function host(partes: Partial<Host> = {}): Host {
	return {
		id: "h1",
		name: "Redis de testes",
		addr: "127.0.0.1",
		port: 6379,
		db: 0,
		username: null,
		tls: false,
		env: "dev",
		...partes,
	};
}

function montar(hosts: Host[] = [host()]) {
	const onSelect = vi.fn();
	const onDisconnected = vi.fn();
	render(HostList, {
		props: {
			hosts,
			selectedId: null,
			onSelect,
			onNew: vi.fn(),
			onEdit: vi.fn(),
			onChanged: vi.fn(),
			onDisconnected,
		},
	});
	return { onSelect, onDisconnected };
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("começa desligado e diz isso", () => {
	montar();

	expect(screen.getByTestId("estado-conexao")).toHaveTextContent("nunca conectado");
});

test("mostra conectando enquanto a chamada está em voo", async () => {
	const user = userEvent.setup();
	let liberar: (ms: number) => void = () => {};
	conectou.mockReturnValueOnce(
		new Promise<number>((resolve) => {
			liberar = resolve;
		}),
	);
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));

	expect(screen.getByTestId("estado-conexao")).toHaveTextContent(/conectando/i);
	liberar(1);
});

test("mostra conectado com o tempo em ms", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(12);
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));

	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent("conectado · 12 ms");
});

test("mostra o motivo quando falha, e continua oferecendo Conectar", async () => {
	const user = userEvent.setup();
	conectou.mockRejectedValueOnce({
		kind: "unreachable",
		message: "não foi possível alcançar o host",
	});
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));

	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent(
		"não foi possível alcançar o host",
	);
	expect(screen.getByRole("button", { name: "Conectar" })).toBeInTheDocument();
});

test("abre a área de trabalho mesmo quando a conexão falha", async () => {
	const user = userEvent.setup();
	conectou.mockRejectedValueOnce({
		kind: "unreachable",
		message: "não foi possível alcançar o host",
	});
	const alvo = host();
	const { onSelect } = montar([alvo]);

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await screen.findByTestId("estado-conexao");

	// O motivo viaja junto: é o que a casca repassa ao Workspace para ele
	// não tentar varrer um host já sabido inalcançável.
	expect(onSelect).toHaveBeenCalledWith(alvo, "não foi possível alcançar o host");
});

test("troca o botão para Desconectar quando conectado", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(9);
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));

	expect(await screen.findByRole("button", { name: "Desconectar" })).toBeInTheDocument();
	expect(screen.queryByRole("button", { name: "Conectar" })).not.toBeInTheDocument();
});

test("desconectar devolve o host ao estado desligado", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(9);
	desconectou.mockResolvedValueOnce(undefined);
	const alvo = host();
	const { onDisconnected } = montar([alvo]);

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await user.click(await screen.findByRole("button", { name: "Desconectar" }));

	expect(desconectou).toHaveBeenCalledWith("h1");
	expect(onDisconnected).toHaveBeenCalledWith(alvo);
	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent("nunca conectado");
	expect(screen.getByRole("button", { name: "Conectar" })).toBeInTheDocument();
});

test("desconectar fica desabilitado enquanto a chamada está em voo", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(9);
	let liberar: () => void = () => {};
	desconectou.mockReturnValueOnce(
		new Promise<void>((resolve) => {
			liberar = resolve;
		}),
	);
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await user.click(await screen.findByRole("button", { name: "Desconectar" }));

	// Em voo: o botão não pode aceitar um segundo clique, que disparia uma
	// segunda chamada a `disconnect` concorrente com a primeira.
	expect(screen.getByRole("button", { name: "Desconectando…" })).toBeDisabled();
	liberar();
});

test("desconectar que falha mostra o motivo, em vez de ficar mudo", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(9);
	desconectou.mockRejectedValueOnce({
		kind: "unknown_host",
		message: "host não encontrado",
	});
	const alvo = host();
	const { onDisconnected } = montar([alvo]);

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await user.click(await screen.findByRole("button", { name: "Desconectar" }));

	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent("host não encontrado");
	// Não desconectou de fato: a casca não pode ser avisada de que o
	// workspace deste host deve fechar.
	expect(onDisconnected).not.toHaveBeenCalled();
	// E a linha continua oferecendo uma ação, não presa em "conectado" sem
	// explicação — mesma régua de uma falha de `connect`.
	expect(screen.getByRole("button", { name: "Conectar" })).toBeInTheDocument();
});

test("não usa vermelho nem caixa alta no estado", async () => {
	const user = userEvent.setup();
	conectou.mockRejectedValueOnce({
		kind: "unreachable",
		message: "não foi possível alcançar o host",
	});
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));

	const estado = await screen.findByTestId("estado-conexao");
	// Nunca a classe usada pela falha do botão Testar, que é vermelha.
	expect(estado.className).not.toMatch(/falha/);
	// Nenhuma sequência de duas ou mais maiúsculas seguidas — caixa alta é
	// reservada à flag do ambiente.
	expect(estado.textContent).not.toMatch(/[A-ZÀ-Ú]{2,}/);
});
