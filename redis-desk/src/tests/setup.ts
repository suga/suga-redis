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

/**
 * O Node 22+ já define `globalThis.localStorage` — um `getter` nativo que
 * fica inerte sem `--localstorage-file` e, por existir antes, impede o
 * jsdom de instalar o dele. Sem isso, qualquer `localStorage.getItem`
 * explode com "Cannot read properties of undefined". Um `Storage` de
 * verdade, em memória, é o equivalente honesto no teste.
 */
class ArmazenamentoEmMemoria implements Storage {
	#dados = new Map<string, string>();

	get length() {
		return this.#dados.size;
	}

	clear(): void {
		this.#dados.clear();
	}

	getItem(chave: string): string | null {
		return this.#dados.get(chave) ?? null;
	}

	key(indice: number): string | null {
		return Array.from(this.#dados.keys())[indice] ?? null;
	}

	removeItem(chave: string): void {
		this.#dados.delete(chave);
	}

	setItem(chave: string, valor: string): void {
		this.#dados.set(chave, String(valor));
	}
}

Object.defineProperty(globalThis, "localStorage", {
	value: new ArmazenamentoEmMemoria(),
	configurable: true,
	writable: true,
});
