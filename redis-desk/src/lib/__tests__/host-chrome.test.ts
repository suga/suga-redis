import { render, screen } from "@testing-library/svelte";
import userEvent from "@testing-library/user-event";
import { beforeEach, expect, test, vi } from "vitest";

// `api.ts` importa `@tauri-apps/api`, que só existe dentro da janela do app.
// Trocar o módulo inteiro aqui é o que deixa os componentes rodarem em jsdom.
vi.mock("$lib/api", () => ({
	addHost: vi.fn(async () => "id-novo"),
	updateHost: vi.fn(async () => undefined),
	removeHost: vi.fn(async () => undefined),
	testHost: vi.fn(async () => 3),
	listHosts: vi.fn(async () => []),
	connect: vi.fn(async () => 7),
	disconnect: vi.fn(async () => undefined),
	errorMessage: (err: unknown) => String(err),
}));

import { addHost } from "$lib/api";
import EnvBand from "$lib/EnvBand.svelte";
import HostForm from "$lib/HostForm.svelte";
import HostList from "$lib/HostList.svelte";
import type { Env, Host } from "$lib/api";

/** Cores fixadas na seção Global Constraints do plano. */
const COR = {
	prod: "#8E2B24",
	homolog: "#C98A20",
	dev: "#4E7A4A",
} as const;

function host(env: Env, name = "Redis"): Host {
	return {
		id: `id-${env}`,
		name,
		addr: "127.0.0.1",
		port: 6379,
		db: 0,
		username: null,
		tls: false,
		env,
	};
}

/** A cor que a barra está realmente pintando, lida da variável CSS. */
function corDaBarra(): string {
	return screen
		.getByTestId("env-band")
		.style.getPropertyValue("--env-band")
		.trim();
}

beforeEach(() => {
	vi.clearAllMocks();
});

test("host de prod pinta a barra de vermelho e carimba PRODUÇÃO", () => {
	render(EnvBand, { props: { host: host("prod") } });

	expect(corDaBarra()).toBe(COR.prod);
	expect(screen.getByTestId("env-band")).toHaveTextContent("PRODUÇÃO");
});

test("host de dev usa a cor calma e não carimba PRODUÇÃO", () => {
	render(EnvBand, { props: { host: host("dev") } });

	expect(corDaBarra()).toBe(COR.dev);
	expect(screen.getByTestId("env-band")).toHaveTextContent("DESENVOLVIMENTO");
	expect(screen.getByTestId("env-band")).not.toHaveTextContent("PRODUÇÃO");
});

test("trocar de host troca a cor da janela", async () => {
	const { rerender } = render(EnvBand, { props: { host: host("dev") } });
	expect(corDaBarra()).toBe(COR.dev);

	await rerender({ host: host("prod") });

	expect(corDaBarra()).toBe(COR.prod);
	expect(screen.getByTestId("env-band")).toHaveTextContent("PRODUÇÃO");
});

test("cadastrar host sem endereço mostra erro e não chama add_host", async () => {
	const user = userEvent.setup();
	render(HostForm, { props: { host: null } });

	// Nome preenchido, endereço vazio: o formulário não pode deixar passar.
	await user.type(screen.getByLabelText("Nome"), "Redis de produção");
	await user.clear(screen.getByLabelText("Endereço"));
	await user.click(screen.getByRole("button", { name: "Salvar" }));

	expect(await screen.findByRole("alert")).toHaveTextContent(
		"Informe o endereço do host.",
	);
	expect(addHost).not.toHaveBeenCalled();
});

/** Endereço de cluster gerenciado: longo o bastante para estourar a coluna. */
const ENDERECO_LONGO =
	"redis-cliente-exemplo-um-nome-bem-comprido.a1b2c3.ng.0001.use1.cache.amazonaws.com";

function montarLista(env: Env = "prod") {
	const alvo: Host = { ...host(env, "Cliente Exemplo"), addr: ENDERECO_LONGO };
	const onSelect = vi.fn();
	const { unmount } = render(HostList, {
		props: {
			hosts: [alvo],
			selectedId: null,
			falhaExterna: null,
			onSelect,
			onNew: vi.fn(),
			onEdit: vi.fn(),
			onChanged: vi.fn(),
			onDisconnected: vi.fn(),
		},
	});
	return { alvo, onSelect, unmount };
}

test("cada host carrega a flag do seu ambiente", () => {
	const prod = montarLista("prod");
	expect(screen.getByTestId("flag-ambiente")).toHaveTextContent("PRODUÇÃO");
	prod.unmount();

	montarLista("homolog");
	expect(screen.getByTestId("flag-ambiente")).toHaveTextContent("HOMOLOGAÇÃO");
});

test("o botão conectar abre o host, igual clicar no nome", async () => {
	const user = userEvent.setup();
	const { alvo, onSelect } = montarLista();

	await user.click(screen.getByRole("button", { name: "Conectar" }));
	expect(onSelect).toHaveBeenCalledWith(alvo);

	onSelect.mockClear();
	await user.click(screen.getByRole("button", { name: /Cliente Exemplo/ }));
	expect(onSelect).toHaveBeenCalledWith(alvo);
});

test("endereço comprido continua alcançável inteiro, mesmo cortado na tela", () => {
	const { alvo } = montarLista();

	// O corte em si é CSS (`text-overflow`), e o jsdom não faz layout para
	// conferi-lo. O que dá para garantir aqui é o que o corte não pode
	// custar: o endereço inteiro, que é o que distingue dois clusters do
	// mesmo ambiente, continua disponível ao passar o mouse.
	expect(screen.getByTestId("endereco")).toHaveAttribute(
		"title",
		`${alvo.addr}:${alvo.port}/${alvo.db}`,
	);
});
