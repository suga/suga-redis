<script lang="ts">
	import ErroTecnico from "$lib/ErroTecnico.svelte";
	/**
	 * Cadastro e edição de host.
	 *
	 * A validação acontece aqui e não deixa o pedido sair: um endereço vazio
	 * viraria um erro de conexão lá na frente, longe do campo que a pessoa
	 * precisa consertar.
	 */
	import { untrack } from "svelte";
	import { addHost, errorMessage, updateHost, type Env, type Host } from "$lib/api";
	import { t } from "$lib/i18n.svelte";

	interface Props {
		/** `null` cadastra um host novo; um host edita aquele. */
		host: Host | null;
		onSaved?: (hostId: string) => void;
		onCancel?: () => void;
	}

	let { host, onSaved, onCancel }: Props = $props();

	/**
	 * Os campos partem do host com que o formulário ABRIU e passam a viver
	 * por conta própria — é a pessoa quem edita daí em diante, e um
	 * recarregamento da lista no fundo não pode puxar o texto debaixo dos
	 * dedos dela. `untrack` diz isso ao compilador; trocar de host é
	 * remontagem, feita com `{#key}` por quem usa este componente.
	 */
	const inicial = untrack(() => host);

	const AMBIENTES = [
		{ value: "dev", chave: "ambienteOpcao.dev" },
		{ value: "homolog", chave: "ambienteOpcao.homolog" },
		{ value: "prod", chave: "ambienteOpcao.prod" },
	] as const satisfies { value: Env; chave: Parameters<typeof t>[0] }[];

	let name = $state(inicial?.name ?? "");
	let addr = $state(inicial?.addr ?? "");
	let port = $state(inicial?.port ?? 6379);
	let db = $state(inicial?.db ?? 0);
	let username = $state(inicial?.username ?? "");
	let password = $state("");
	let tls = $state(inicial?.tls ?? false);
	let env = $state<Env>(inicial?.env ?? "dev");

	let erro = $state("");
	/** Falha vinda do backend, que vem com detalhe técnico junto. */
	let erroTecnico = $state("");
	let salvando = $state(false);

	/** A primeira coisa errada, ou `null` se está tudo certo. */
	function primeiroProblema(): string | null {
		if (!name.trim()) return t("form.nomeObrigatorio");
		if (!addr.trim()) return t("form.enderecoObrigatorio");
		if (!Number.isInteger(port) || port < 1 || port > 65535) {
			return t("form.portaInvalida");
		}
		if (!Number.isInteger(db) || db < 0 || db > 15) {
			return t("form.bancoInvalido");
		}
		return null;
	}

	async function salvar(event: Event) {
		event.preventDefault();
		erro = "";
		erroTecnico = "";

		const problema = primeiroProblema();
		if (problema) {
			erro = problema;
			erroTecnico = "";
			return;
		}

		const dados: Host = {
			id: host?.id ?? "",
			name: name.trim(),
			addr: addr.trim(),
			port,
			db,
			username: username.trim() || null,
			tls,
			env,
		};

		salvando = true;
		try {
			if (host) {
				await updateHost(dados, password || null);
				onSaved?.(host.id);
			} else {
				onSaved?.(await addHost(dados, password || null));
			}
		} catch (err) {
			erro = "";
			erroTecnico = errorMessage(err);
		} finally {
			salvando = false;
		}
	}
</script>

<form onsubmit={salvar}>
	<h2>{host ? t("host.editar") : t("acao.novoHost")}</h2>

	<label>
		<span>{t("campo.nome")}</span>
		<input bind:value={name} autocomplete="off" />
	</label>

	<div class="row">
		<label class="grow">
			<span>{t("campo.endereco")}</span>
			<input bind:value={addr} class="mono" autocomplete="off" />
		</label>
		<label class="narrow">
			<span>{t("campo.porta")}</span>
			<input type="number" bind:value={port} class="mono" min="1" max="65535" />
		</label>
		<label class="narrow">
			<span>{t("campo.banco")}</span>
			<input type="number" bind:value={db} class="mono" min="0" max="15" />
		</label>
	</div>

	<div class="row">
		<label class="grow">
			<span>{t("campo.usuario")}</span>
			<input bind:value={username} autocomplete="off" />
		</label>
		<label class="grow">
			<span>{t("campo.senha")}</span>
			<input
				type="password"
				bind:value={password}
				autocomplete="new-password"
				placeholder={host ? t("campo.manterSenha") : ""}
			/>
		</label>
	</div>

	<div class="row">
		<label class="grow">
			<span>{t("campo.ambiente")}</span>
			<select bind:value={env}>
				{#each AMBIENTES as ambiente (ambiente.value)}
					<option value={ambiente.value}>{t(ambiente.chave)}</option>
				{/each}
			</select>
		</label>
		<label class="check">
			<input type="checkbox" bind:checked={tls} />
			<span>{t("campo.tls")}</span>
		</label>
	</div>

	{#if erro}
		<p class="erro" role="alert">{erro}</p>
	{/if}
	{#if erroTecnico}
		<ErroTecnico assunto="erro.aoSalvarHost" detalhe={erroTecnico} />
	{/if}

	<div class="acoes">
		{#if onCancel}
			<button type="button" onclick={onCancel}>{t("acao.cancelar")}</button>
		{/if}
		<button type="submit" class="primary" disabled={salvando}>{t("acao.salvar")}</button>
	</div>
</form>

<style>
	form {
		display: flex;
		flex-direction: column;
		gap: 0.7rem;
		max-width: 34rem;
	}

	h2 {
		margin: 0;
		font-size: 1.2rem;
		font-weight: 600;
	}

	label {
		display: flex;
		flex-direction: column;
		gap: 0.2rem;
	}

	label > span {
		color: var(--text-dim);
		font-size: 0.9rem;
	}

	.row {
		display: flex;
		gap: 0.6rem;
		align-items: flex-end;
	}

	.grow {
		flex: 1;
	}

	.narrow {
		width: 6rem;
	}

	.check {
		flex-direction: row;
		align-items: center;
		gap: 0.4rem;
		padding-bottom: 0.4rem;
	}

	.check > span {
		color: var(--text);
		font-size: 1rem;
	}

	/* Deliberadamente NÃO usa `--prod`: neste app o vermelho quer dizer
	   "você está em produção". Gastá-lo num campo mal preenchido ensinaria
	   a pessoa a ignorá-lo justamente onde ele precisa ser levado a sério. */
	.erro {
		margin: 0;
		padding: 0.4rem 0.6rem;
		border: 1px solid #5e3a38;
		border-radius: var(--radius);
		background: #2e2326;
		color: #e49a94;
	}

	.acoes {
		display: flex;
		justify-content: flex-end;
		gap: 0.5rem;
		margin-top: 0.3rem;
	}
</style>
