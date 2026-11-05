import { cleanup, render, screen, waitFor } from "@testing-library/svelte";
import { afterEach, beforeEach, expect, test, vi } from "vitest";

import Divisor from "$lib/Divisor.svelte";

function alca(): HTMLElement {
	return screen.getByRole("separator");
}

/** Dispara o evento no `window`: é lá que o arrasto escuta, não na alça. */
function mouse(tipo: "mousemove" | "mouseup", clientX: number) {
	window.dispatchEvent(new MouseEvent(tipo, { clientX, bubbles: true }));
}

function arrastar(de: number, para: number) {
	alca().dispatchEvent(
		new MouseEvent("mousedown", { clientX: de, bubbles: true }),
	);
	mouse("mousemove", para);
	mouse("mouseup", para);
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
