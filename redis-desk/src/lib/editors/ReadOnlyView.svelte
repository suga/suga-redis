<script lang="ts">
	/**
	 * Mostra um valor de qualquer tipo, sem deixar editar nada.
	 *
	 * Serve tanto aos tipos que este produto trata como somente leitura
	 * (`set`, `stream`, RedisJSON) quanto aos editáveis antes de a Task 12
	 * pôr um editor no lugar.
	 */
	import type { Bytes, Value } from "$lib/api";

	interface Props {
		value: Value;
	}

	let { value }: Props = $props();

	/**
	 * Um elemento binário não tem texto a mostrar. Em vez do base64 cru —
	 * que ninguém lê e que ocuparia a tela inteira — vai um aviso do
	 * tamanho, que é a informação de fato útil ali.
	 */
	function mostrar(bytes: Bytes): string {
		if (bytes.text !== null) return bytes.text;
		const tamanho = bytes.base64 ? Math.floor((bytes.base64.length * 3) / 4) : 0;
		return `(binário, ${tamanho.toLocaleString("pt-BR")} bytes)`;
	}

	function binario(bytes: Bytes): boolean {
		return bytes.text === null;
	}
</script>

{#if value.kind === "string"}
	<pre class="mono bloco" class:binario={binario(value.data)}>{mostrar(value.data)}</pre>
{:else if value.kind === "read_only_text"}
	<pre class="mono bloco">{value.data}</pre>
{:else if value.kind === "hash"}
	<table>
		<tbody>
			{#each value.data as [campo, bytes], i (campo + i)}
				<tr>
					<th class="mono" scope="row">{campo}</th>
					<td class="mono" class:binario={binario(bytes)}>{mostrar(bytes)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{:else if value.kind === "zset"}
	<table>
		<tbody>
			{#each value.data as [bytes, score], i (i)}
				<tr>
					<th class="mono score" scope="row">{score}</th>
					<td class="mono" class:binario={binario(bytes)}>{mostrar(bytes)}</td>
				</tr>
			{/each}
		</tbody>
	</table>
{:else if value.kind === "stream"}
	<table>
		<tbody>
			{#each value.data as entrada (entrada.id)}
				<tr>
					<th class="mono" scope="row">{entrada.id}</th>
					<td>
						{#each entrada.fields as [campo, bytes], i (campo + i)}
							<div class="mono campo">
								<span class="campo-nome">{campo}</span>
								<span class:binario={binario(bytes)}>{mostrar(bytes)}</span>
							</div>
						{/each}
					</td>
				</tr>
			{/each}
		</tbody>
	</table>
{:else}
	<!-- list e set: um elemento por linha, numerados só na lista, onde a
	     posição é parte do dado. -->
	<ol class:sem-indice={value.kind === "set"}>
		{#each value.data as bytes, i (i)}
			<li class="mono" class:binario={binario(bytes)}>{mostrar(bytes)}</li>
		{/each}
	</ol>
{/if}

<style>
	.bloco {
		margin: 0;
		padding: 0.6rem 0.7rem;
		background: var(--panel-dark);
		border: 1px solid var(--rule);
		border-radius: var(--radius);
		white-space: pre-wrap;
		word-break: break-word;
	}

	table {
		width: 100%;
		border-collapse: collapse;
	}

	tr {
		border-bottom: 1px solid var(--rule);
	}

	th,
	td {
		padding: 0.28rem 0.5rem;
		text-align: left;
		vertical-align: top;
		font-weight: 400;
		word-break: break-word;
	}

	th {
		width: 1%;
		white-space: nowrap;
		color: var(--text-dim);
		border-right: 1px solid var(--rule);
	}

	.score {
		text-align: right;
	}

	.campo {
		display: flex;
		gap: 0.6rem;
	}

	.campo-nome {
		color: var(--text-dim);
	}

	ol {
		margin: 0;
		padding: 0;
		list-style: none;
		counter-reset: item;
	}

	li {
		display: flex;
		gap: 0.6rem;
		padding: 0.28rem 0.5rem;
		border-bottom: 1px solid var(--rule);
		word-break: break-word;
	}

	ol:not(.sem-indice) li::before {
		counter-increment: item;
		content: counter(item);
		min-width: 2.5rem;
		color: var(--text-faint);
		text-align: right;
	}

	/* Conteúdo que não é texto fica em cinza: é um aviso, não o valor. */
	.binario {
		color: var(--text-faint);
		font-style: italic;
	}
</style>
