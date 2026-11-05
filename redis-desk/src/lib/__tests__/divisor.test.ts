import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import Divisor from "$lib/Divisor.svelte";

function alca(): HTMLElement {
	return screen.getByRole("separator");
}

/**
 * Dispara o evento no `window`: é lá que o arrasto escuta, não na alça.
 * `buttons: 1` por padrão no `mousemove` é o botão ainda pressionado — o
 * caso comum de um arrasto em andamento; os testes do solto implícito
 * passam `0` explicitamente.
 */
function mouse(tipo: "mousemove" | "mouseup", clientX: number, buttons = 1) {
	window.dispatchEvent(new MouseEvent(tipo, { clientX, buttons, bubbles: true }));
}

function arrastar(de: number, para: number) {
	alca().dispatchEvent(
		new MouseEvent("mousedown", { clientX: de, bubbles: true }),
	);
	mouse("mousemove", para, 1);
	mouse("mouseup", para, 0);
}

beforeEach(() => {
	localStorage.clear();
	window.innerWidth = 1200;
});

afterEach(() => {
	cleanup();
});

test("arrastar para a direita alarga, e avisa em px", async () => {
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:arrasto", inicial: 240, onLargura },
	});
	// O aviso da montagem (largura inicial) não é o que este teste cobre.
	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(240));
	onLargura.mockClear();

	arrastar(100, 150);

	expect(onLargura).toHaveBeenLastCalledWith(290);
	// O `mouseup` normal também tem que devolver o cursor da janela.
	expect(document.body.style.cursor).toBe("");
});

test("não deixa encolher abaixo do mínimo", async () => {
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:minimo", inicial: 240, min: 200, onLargura },
	});
	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(240));
	onLargura.mockClear();

	arrastar(100, -500);

	expect(onLargura).toHaveBeenLastCalledWith(200);
});

test("não deixa passar de metade da janela", async () => {
	window.innerWidth = 800; // metade = 400
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:teto", inicial: 240, onLargura },
	});
	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(240));
	onLargura.mockClear();

	arrastar(100, 1000); // pediria 240 + 900 = 1140, bem acima de 400

	expect(onLargura).toHaveBeenLastCalledWith(400);
});

test("guarda a largura e a devolve na montagem seguinte", async () => {
	const primeiro = vi.fn();
	const { unmount } = render(Divisor, {
		props: { chave: "largura:persiste", inicial: 240, onLargura: primeiro },
	});
	await waitFor(() => expect(primeiro).toHaveBeenCalledWith(240));

	arrastar(100, 150); // 240 + 50 = 290

	unmount();

	const segundo = vi.fn();
	render(Divisor, {
		props: { chave: "largura:persiste", inicial: 240, onLargura: segundo },
	});

	await waitFor(() => expect(segundo).toHaveBeenCalledWith(290));
});

test.each(["abc", "-40", "99999"])(
	"ignora largura guardada inválida (%s) e cai no inicial",
	async (guardada) => {
		localStorage.setItem("largura:invalida", guardada);
		const onLargura = vi.fn();
		render(Divisor, {
			props: { chave: "largura:invalida", inicial: 240, onLargura },
		});

		await waitFor(() => expect(onLargura).toHaveBeenCalledWith(240));
	},
);

test("solta o arrasto sozinho quando o botão já não está mais pressionado", async () => {
	// Numa janela do Tauri não maximizada, soltar o botão fora da janela não
	// dispara `mouseup` dentro da página: o próximo `mousemove` a cruzá-la
	// chega sem o botão (`buttons === 0`), e é esse o único sinal de que o
	// arrasto acabou.
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:solto-implicito", inicial: 240, onLargura },
	});
	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(240));

	alca().dispatchEvent(
		new MouseEvent("mousedown", { clientX: 100, bubbles: true }),
	);
	mouse("mousemove", 150, 1); // ainda arrastando: 240 + 50 = 290
	expect(onLargura).toHaveBeenLastCalledWith(290);
	expect(document.body.style.cursor).toBe("col-resize");

	mouse("mousemove", 180, 0); // o botão já não está pressionado: solta sozinho

	// O cursor de redimensionar não pode ficar preso na janela inteira
	// depois que o arrasto acabou, implícito ou não.
	expect(document.body.style.cursor).toBe("");

	onLargura.mockClear();
	mouse("mousemove", 500, 1); // um "arrasto" novo não deveria mais mexer em nada
	expect(onLargura).not.toHaveBeenCalled();

	// E o que ficou valendo foi a largura de quando soltou, não a do último
	// movimento ignorado.
	expect(localStorage.getItem("largura:solto-implicito")).toBe("290");
});

test("seta para a direita alarga em 16px, seta para a esquerda encolhe", async () => {
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:teclado", inicial: 240, onLargura },
	});
	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(240));
	onLargura.mockClear();

	await alca().focus();
	alca().dispatchEvent(
		new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
	);
	expect(onLargura).toHaveBeenLastCalledWith(256);

	alca().dispatchEvent(
		new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
	);
	expect(onLargura).toHaveBeenLastCalledWith(240);

	expect(localStorage.getItem("largura:teclado")).toBe("240");
});

test("a seta não encolhe abaixo do mínimo nem passa do teto", async () => {
	window.innerWidth = 600; // metade = 300
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:teclado-limites", inicial: 292, min: 280, onLargura },
	});
	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(292));
	onLargura.mockClear();

	// 292 + 16 = 308, acima do teto de 300.
	alca().dispatchEvent(
		new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true }),
	);
	expect(onLargura).toHaveBeenLastCalledWith(300);

	// Dali, bastam duas setas para a esquerda para passar do mínimo: 300 →
	// 284 → 268, e 268 < 280 deveria travar em 280.
	alca().dispatchEvent(
		new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
	);
	alca().dispatchEvent(
		new KeyboardEvent("keydown", { key: "ArrowLeft", bubbles: true }),
	);
	expect(onLargura).toHaveBeenLastCalledWith(280);
});

test("não aplica largura guardada em janela estreita", async () => {
	// 300px era válido quando guardado numa janela larga; a janela volta
	// estreita o bastante (abaixo de 52rem, onde o layout empilha) para que
	// metade dela já não comporte os 300px — aplicá-los aqui devolveria uma
	// coluna maior que a própria janela.
	localStorage.setItem("largura:estreita", "300");
	window.innerWidth = 500; // metade = 250 < 300 guardado
	const onLargura = vi.fn();
	render(Divisor, {
		props: { chave: "largura:estreita", inicial: 220, onLargura },
	});

	await waitFor(() => expect(onLargura).toHaveBeenCalledWith(220));
});
