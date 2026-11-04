/**
 * Realce e validação de JSON para o editor de string.
 *
 * Sem biblioteca: o que precisamos é um analisador de UM formato conhecido,
 * e trazer um destacador de sintaxe inteiro para colorir quatro tipos de
 * token custaria mais do que resolve.
 */

/** Um pedaço do texto, com o papel dele — ou `null` para pontuação e espaço. */
export interface TokenJson {
	classe: "chave" | "texto" | "numero" | "booleano" | "nulo" | null;
	texto: string;
}

/**
 * Casa, nesta ordem: string (seguida ou não de `:`, que a torna uma chave),
 * número, booleano e nulo.
 *
 * A string vem PRIMEIRO de propósito. O motor varre da esquerda para a
 * direita, então uma aspa inicia o casamento da string e consome até a aspa
 * de fechamento — e com isso um `true` ou um `42` escrito DENTRO de um valor
 * de texto não é colorido como se fosse um literal.
 */
const PADRAO = /("(?:\\.|[^"\\])*")(\s*:)?|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)|(true|false)|(null)/g;

/** Parte o texto nos pedaços que a tela vai colorir. */
export function realcarJson(texto: string): TokenJson[] {
	const tokens: TokenJson[] = [];
	let ultimo = 0;

	for (const casamento of texto.matchAll(PADRAO)) {
		const em = casamento.index ?? 0;
		if (em > ultimo) {
			tokens.push({ classe: null, texto: texto.slice(ultimo, em) });
		}

		const [inteiro, str, doisPontos, numero, booleano, nulo] = casamento;
		if (str !== undefined) {
			tokens.push({ classe: doisPontos ? "chave" : "texto", texto: str });
			if (doisPontos) tokens.push({ classe: null, texto: doisPontos });
		} else if (numero !== undefined) {
			tokens.push({ classe: "numero", texto: numero });
		} else if (booleano !== undefined) {
			tokens.push({ classe: "booleano", texto: booleano });
		} else if (nulo !== undefined) {
			tokens.push({ classe: "nulo", texto: nulo });
		}

		ultimo = em + inteiro.length;
	}

	if (ultimo < texto.length) {
		tokens.push({ classe: null, texto: texto.slice(ultimo) });
	}
	return tokens;
}

/** O texto tem cara de JSON? Só então vale cobrar que ele seja JSON válido. */
export function pareceJson(texto: string): boolean {
	const limpo = texto.trim();
	return limpo.startsWith("{") || limpo.startsWith("[");
}

/**
 * Onde o JSON deixa de fazer sentido.
 *
 * Existe porque a mensagem do `JSON.parse` NÃO é um contrato: até o Node 20
 * ela trazia `at position N`, e do 22 em diante traz um trecho do texto no
 * lugar. Um validador que lesse essa frase para achar a posição quebraria
 * sozinho na próxima atualização do runtime.
 *
 * Então a divisão é esta: quem decide se o JSON vale é sempre o
 * `JSON.parse`; este varredor só diz ONDE está o problema. Se os dois
 * discordarem, a posição é descartada e sobra a mensagem sem ela — nunca o
 * contrário, porque errar o veredito seria pior que errar o dedo apontando.
 */
function localizarFalha(texto: string): number | null {
	let i = 0;
	const n = texto.length;

	const espaco = () => {
		while (i < n && " \t\n\r".includes(texto[i])) i++;
	};

	const cadeia = (): boolean => {
		i++; // a aspa de abertura
		while (i < n) {
			if (texto[i] === "\\") {
				i += 2;
				continue;
			}
			if (texto[i] === '"') {
				i++;
				return true;
			}
			i++;
		}
		return false; // acabou o texto com a string aberta
	};

	const numero = (): boolean => {
		const comeco = i;
		if (texto[i] === "-") i++;
		while (i < n && texto[i] >= "0" && texto[i] <= "9") i++;
		if (texto[i] === ".") {
			i++;
			while (i < n && texto[i] >= "0" && texto[i] <= "9") i++;
		}
		if (texto[i] === "e" || texto[i] === "E") {
			i++;
			if (texto[i] === "+" || texto[i] === "-") i++;
			while (i < n && texto[i] >= "0" && texto[i] <= "9") i++;
		}
		return i > comeco;
	};

	const valor = (): boolean => {
		espaco();
		if (i >= n) return false;
		const c = texto[i];
		if (c === "{") return objeto();
		if (c === "[") return lista();
		if (c === '"') return cadeia();
		if (c === "-" || (c >= "0" && c <= "9")) return numero();
		for (const literal of ["true", "false", "null"]) {
			if (texto.startsWith(literal, i)) {
				i += literal.length;
				return true;
			}
		}
		return false;
	};

	const objeto = (): boolean => {
		i++; // a chave de abertura
		espaco();
		if (texto[i] === "}") {
			i++;
			return true;
		}
		for (;;) {
			espaco();
			if (texto[i] !== '"') return false;
			if (!cadeia()) return false;
			espaco();
			if (texto[i] !== ":") return false;
			i++;
			if (!valor()) return false;
			espaco();
			if (texto[i] === ",") {
				i++;
				continue;
			}
			if (texto[i] === "}") {
				i++;
				return true;
			}
			return false;
		}
	};

	const lista = (): boolean => {
		i++; // o colchete de abertura
		espaco();
		if (texto[i] === "]") {
			i++;
			return true;
		}
		for (;;) {
			if (!valor()) return false;
			espaco();
			if (texto[i] === ",") {
				i++;
				continue;
			}
			if (texto[i] === "]") {
				i++;
				return true;
			}
			return false;
		}
	};

	if (!valor()) return i;
	espaco();
	return i < n ? i : null;
}

/**
 * O que há de errado com o JSON, em português e apontando ONDE.
 *
 * A posição vem em linha e coluna: um deslocamento em bytes corridos não
 * ajuda ninguém a achar o erro num valor de trinta linhas. `null` quer
 * dizer que está tudo certo.
 */
export function validarJson(texto: string): string | null {
	if (!pareceJson(texto)) return null;

	try {
		JSON.parse(texto);
		return null;
	} catch {
		const posicao = localizarFalha(texto);
		if (posicao === null) {
			return "JSON inválido: o conteúdo não pôde ser interpretado.";
		}

		const ate = texto.slice(0, posicao);
		const linha = ate.split("\n").length;
		const coluna = posicao - ate.lastIndexOf("\n");
		return `JSON inválido na linha ${linha}, coluna ${coluna}.`;
	}
}

/** O mesmo JSON, indentado. Devolve `null` se não der para interpretar. */
export function formatarJson(texto: string): string | null {
	if (!pareceJson(texto)) return null;
	try {
		return JSON.stringify(JSON.parse(texto), null, 2);
	} catch {
		return null;
	}
}
