// Matchers de DOM (`toHaveTextContent`, `toBeInTheDocument`, ...) para o
// `expect` do Vitest.
import "@testing-library/jest-dom/vitest";

/**
 * O jsdom não faz layout, e por isso não traz `ResizeObserver`. Um dublê que
 * nunca dispara é o equivalente honesto: sem layout, nenhum elemento muda de
 * tamanho mesmo. Os componentes que medem a janela (`KeyTree`) já tratam
 * altura zero desenhando tudo, então o teste exercita o caminho certo.
 */
if (!("ResizeObserver" in globalThis)) {
	globalThis.ResizeObserver = class {
		observe() {}
		unobserve() {}
		disconnect() {}
	} as unknown as typeof ResizeObserver;
}
