//! Cadastro de hosts Redis: persiste os hosts em `hosts.json` no diretório de
//! dados do app e guarda a senha de cada host no chaveiro do sistema
//! operacional (serviço `redis-desk`, conta `<id do host>`), nunca em arquivo.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Nome do serviço usado para localizar as senhas no chaveiro do sistema.
const KEYRING_SERVICE: &str = "redis-desk";

/// Nome do arquivo onde os hosts (sem senha) são persistidos.
const HOSTS_FILE: &str = "hosts.json";

/// Erros possíveis ao manipular o cadastro de hosts.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("falha ao ler ou gravar hosts.json: {0}")]
    Io(#[from] std::io::Error),

    #[error("falha ao (des)serializar hosts.json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("falha ao acessar o chaveiro do sistema: {0}")]
    Keyring(#[from] keyring::Error),

    #[error("host não encontrado: {0}")]
    HostNotFound(String),
}

/// Resultado padrão das operações deste módulo.
pub type Result<T> = std::result::Result<T, ConfigError>;

/// Ambiente ao qual um host Redis pertence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Env {
    Prod,
    Homolog,
    Dev,
}

/// Um host Redis cadastrado pelo usuário (sem a senha, que fica no chaveiro).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Host {
    pub id: String,
    pub name: String,
    pub addr: String,
    pub port: u16,
    pub db: u8,
    pub username: Option<String>,
    pub tls: bool,
    pub env: Env,
}

/// Cadastro de hosts Redis: mantém a lista em memória, persiste em
/// `hosts.json` e delega as senhas ao chaveiro do sistema.
pub struct HostStore {
    hosts_path: PathBuf,
    hosts: Vec<Host>,
    // As entradas do chaveiro são cacheadas por id para que o backend de
    // teste (`keyring::mock`), que não persiste nada fora da própria
    // `Entry`, funcione corretamente dentro da vida de um `HostStore`. Em
    // um chaveiro real do sistema operacional isso é irrelevante: a senha
    // persiste no chaveiro independentemente de reaproveitarmos a `Entry`.
    keyring_entries: RefCell<HashMap<String, keyring::Entry>>,
}

impl HostStore {
    /// Abre (ou cria) o cadastro de hosts no diretório de dados informado.
    pub fn open(dir: &Path) -> Result<HostStore> {
        fs::create_dir_all(dir)?;
        let hosts_path = dir.join(HOSTS_FILE);

        let hosts = if hosts_path.exists() {
            let content = fs::read_to_string(&hosts_path)?;
            if content.trim().is_empty() {
                Vec::new()
            } else {
                serde_json::from_str(&content)?
            }
        } else {
            Vec::new()
        };

        Ok(HostStore {
            hosts_path,
            hosts,
            keyring_entries: RefCell::new(HashMap::new()),
        })
    }

    /// Lista os hosts cadastrados.
    pub fn list(&self) -> Vec<Host> {
        self.hosts.clone()
    }

    /// Adiciona um novo host, gerando um id (UUID v4) para ele. Se uma senha
    /// for informada, ela é gravada no chaveiro sob a conta do id gerado.
    pub fn add(&mut self, mut host: Host, password: Option<&str>) -> Result<String> {
        let id = Uuid::new_v4().to_string();
        host.id = id.clone();

        if let Some(password) = password {
            self.set_password(&id, password)?;
        }

        self.hosts.push(host);
        self.persist()?;

        Ok(id)
    }

    /// Atualiza um host existente mantendo o id. Se uma senha for informada,
    /// ela substitui a senha anterior no chaveiro; caso contrário a senha
    /// existente (se houver) é mantida.
    pub fn update(&mut self, host: Host, password: Option<&str>) -> Result<()> {
        let index = self
            .hosts
            .iter()
            .position(|h| h.id == host.id)
            .ok_or_else(|| ConfigError::HostNotFound(host.id.clone()))?;

        if let Some(password) = password {
            self.set_password(&host.id, password)?;
        }

        self.hosts[index] = host;
        self.persist()
    }

    /// Remove um host e sua senha do chaveiro (se houver).
    pub fn remove(&mut self, id: &str) -> Result<()> {
        let index = self
            .hosts
            .iter()
            .position(|h| h.id == id)
            .ok_or_else(|| ConfigError::HostNotFound(id.to_string()))?;

        self.hosts.remove(index);
        self.delete_password(id)?;
        self.persist()
    }

    /// Lê a senha do host no chaveiro. `Ok(None)` se não houver senha salva.
    pub fn password(&self, id: &str) -> Result<Option<String>> {
        self.ensure_entry(id)?;
        let entries = self.keyring_entries.borrow();
        let entry = entries.get(id).expect("entry was just created");
        match entry.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(err) => Err(err.into()),
        }
    }

    fn set_password(&self, id: &str, password: &str) -> Result<()> {
        self.ensure_entry(id)?;
        let entries = self.keyring_entries.borrow();
        let entry = entries.get(id).expect("entry was just created");
        entry.set_password(password)?;
        Ok(())
    }

    fn delete_password(&self, id: &str) -> Result<()> {
        self.ensure_entry(id)?;
        let entries = self.keyring_entries.borrow();
        let entry = entries.get(id).expect("entry was just created");
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// Garante que exista uma `keyring::Entry` cacheada para o id informado.
    fn ensure_entry(&self, id: &str) -> Result<()> {
        if !self.keyring_entries.borrow().contains_key(id) {
            let entry = keyring::Entry::new(KEYRING_SERVICE, id)?;
            self.keyring_entries.borrow_mut().insert(id.to_string(), entry);
        }
        Ok(())
    }

    /// Grava `hosts.json` por arquivo temporário + rename, para não
    /// corromper o cadastro se o app morrer no meio da escrita.
    fn persist(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.hosts)?;
        let tmp_path = self.hosts_path.with_extension("json.tmp");
        fs::write(&tmp_path, content)?;
        fs::rename(&tmp_path, &self.hosts_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn use_mock_keyring() {
        keyring::set_default_credential_builder(keyring::mock::default_credential_builder());
    }

    fn test_host() -> Host {
        Host {
            id: String::new(),
            name: "Cache principal".to_string(),
            addr: "127.0.0.1".to_string(),
            port: 6379,
            db: 0,
            username: None,
            tls: false,
            env: Env::Prod,
        }
    }

    #[test]
    fn add_then_list_returns_the_host() {
        use_mock_keyring();
        let dir = TempDir::new().unwrap();
        let mut store = HostStore::open(dir.path()).unwrap();

        let id = store.add(test_host(), None).unwrap();
        assert!(!id.is_empty());

        let hosts = store.list();
        assert_eq!(hosts.len(), 1);
        let host = &hosts[0];
        assert_eq!(host.id, id);
        assert_eq!(host.name, "Cache principal");
        assert_eq!(host.addr, "127.0.0.1");
        assert_eq!(host.port, 6379);
        assert_eq!(host.db, 0);
        assert_eq!(host.username, None);
        assert_eq!(host.tls, false);
        assert_eq!(host.env, Env::Prod);
    }

    #[test]
    fn password_is_stored_in_the_keyring_not_the_file() {
        use_mock_keyring();
        let dir = TempDir::new().unwrap();
        let mut store = HostStore::open(dir.path()).unwrap();

        let id = store.add(test_host(), Some("segredo")).unwrap();

        assert_eq!(store.password(&id).unwrap(), Some("segredo".to_string()));

        let content = std::fs::read_to_string(dir.path().join("hosts.json")).unwrap();
        assert!(!content.contains("segredo"));
    }

    #[test]
    fn update_changes_fields_and_keeps_the_id() {
        use_mock_keyring();
        let dir = TempDir::new().unwrap();
        let mut store = HostStore::open(dir.path()).unwrap();

        let id = store.add(test_host(), None).unwrap();

        let mut updated = test_host();
        updated.id = id.clone();
        updated.env = Env::Dev;
        store.update(updated, None).unwrap();

        let hosts = store.list();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].id, id);
        assert_eq!(hosts[0].env, Env::Dev);
    }

    #[test]
    fn remove_deletes_host_and_password() {
        use_mock_keyring();
        let dir = TempDir::new().unwrap();
        let mut store = HostStore::open(dir.path()).unwrap();

        let id = store.add(test_host(), Some("segredo")).unwrap();
        store.remove(&id).unwrap();

        assert_eq!(store.list().len(), 0);
        assert_eq!(store.password(&id).unwrap(), None);
    }

    #[test]
    fn reopening_the_store_keeps_the_hosts() {
        use_mock_keyring();
        let dir = TempDir::new().unwrap();
        let id = {
            let mut store = HostStore::open(dir.path()).unwrap();
            store.add(test_host(), None).unwrap()
        };

        let reopened_store = HostStore::open(dir.path()).unwrap();
        let hosts = reopened_store.list();
        assert_eq!(hosts.len(), 1);
        assert_eq!(hosts[0].id, id);
    }
}
