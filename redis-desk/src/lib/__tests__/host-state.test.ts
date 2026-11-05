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

import { connect, disconnect, removeHost, testHost } from "$lib/api";
import HostList from "$lib/HostList.svelte";
import type { Host } from "$lib/api";

const conectou = vi.mocked(connect);
const desconectou = vi.mocked(disconnect);
const testou = vi.mocked(testHost);
const removeu = vi.mocked(removeHost);

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

function montar(
	hosts: Host[] = [host()],
	falhaExterna: { hostId: string; motivo: string } | null = null,
) {
	const onSelect = vi.fn();
	const onDisconnected = vi.fn();
	const onChanged = vi.fn();
	render(HostList, {
		props: {
			hosts,
			selectedId: null,
			falhaExterna,
			onSelect,
			onNew: vi.fn(),
			onEdit: vi.fn(),
			onChanged,
			onDisconnected,
		},
	});
	return { onSelect, onDisconnected, onChanged };
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

test("conectado diz também em que banco", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(12);
	montar([host({ db: 3 })]);

	await user.click(screen.getByRole("button", { name: "Conectar" }));

	// O banco é o que distingue um host de cluster/serverless (onde só o 0
	// existe) de um host comum — ver `ConnError::ClusterSingleDb`.
	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent(
		"conectado · 12 ms · banco 3",
	);
});

test("clicar no nome do host também conecta, em vez de abrir sem conexão", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(7);
	const alvo = host();
	const { onSelect } = montar([alvo]);

	await user.click(screen.getByRole("button", { name: alvo.name }));

	// Sem isto a árvore aparecia com a linha do host dizendo "nunca
	// conectado" ao lado — e, num banco vazio, "Nenhuma chave neste banco"
	// debaixo de "nunca conectado" faz concluir que a conexão não aconteceu.
	expect(conectou).toHaveBeenCalledWith("h1");
	expect(onSelect).toHaveBeenCalledWith(alvo);
	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent("conectado · 7 ms");
});

test("uma falha vista pela área de trabalho derruba o 'conectado' da linha", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(12);
	const { rerender } = render(HostList, {
		props: {
			hosts: [host()],
			selectedId: "h1",
			falhaExterna: null,
			onSelect: vi.fn(),
			onNew: vi.fn(),
			onEdit: vi.fn(),
			onChanged: vi.fn(),
			onDisconnected: vi.fn(),
		},
	});

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent("conectado");

	// O pool derrubou o socket morto e a nova tentativa falhou: a área de
	// trabalho já diz isso. A linha do host não pode seguir afirmando
	// "conectado" sobre o mesmo banco, duas colunas ao lado.
	await rerender({
		falhaExterna: { hostId: "h1", motivo: "não foi possível alcançar o host" },
	});

	expect(await screen.findByTestId("estado-conexao")).toHaveTextContent(
		"não foi possível alcançar o host",
	);
	expect(screen.getByRole("button", { name: "Conectar" })).toBeInTheDocument();
});

test("uma falha de outro host não mexe na linha deste", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(12);
	const { rerender } = render(HostList, {
		props: {
			hosts: [host()],
			selectedId: "h1",
			falhaExterna: null,
			onSelect: vi.fn(),
			onNew: vi.fn(),
			onEdit: vi.fn(),
			onChanged: vi.fn(),
			onDisconnected: vi.fn(),
		},
	});

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await screen.findByTestId("estado-conexao");

	await rerender({ falhaExterna: { hostId: "outro", motivo: "caiu" } });

	expect(screen.getByTestId("estado-conexao")).toHaveTextContent("conectado · 12 ms");
});

test("o resultado do Testar aparece mesmo com o host conectado", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(12);
	testou.mockResolvedValueOnce(5);
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await user.click(await screen.findByRole("button", { name: "Testar" }));

	// Escondido fora do estado "desligado", o botão virava um nada que pisca
	// o rótulo — e o pior caso era "falhou", justo quando se pergunta "dá
	// para alcançar?".
	expect(await screen.findByText(/Respondeu em 5 ms/)).toBeInTheDocument();
});

test("o resultado do Testar aparece mesmo com o host em falha", async () => {
	const user = userEvent.setup();
	conectou.mockRejectedValueOnce({ kind: "unreachable", message: "host fora do ar" });
	testou.mockRejectedValueOnce({ kind: "unreachable", message: "continua fora do ar" });
	montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await user.click(await screen.findByRole("button", { name: "Testar" }));

	expect(await screen.findByText("continua fora do ar")).toBeInTheDocument();
});

test("uma remoção que falha tem lugar próprio na tela", async () => {
	const user = userEvent.setup();
	conectou.mockResolvedValueOnce(12);
	removeu.mockRejectedValueOnce({ kind: "io", message: "não deu para remover" });
	const { onChanged } = montar();

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	await user.click(await screen.findByRole("button", { name: "Remover" }));

	// Antes a falha era escrita no slot do botão Testar, que ficava
	// escondido justamente em "conectado": a remoção falhava em silêncio.
	expect(await screen.findByTestId("erro-remocao")).toHaveTextContent("não deu para remover");
	expect(onChanged).not.toHaveBeenCalled();
});
