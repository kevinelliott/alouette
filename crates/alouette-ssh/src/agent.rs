//! Sign in with keys held by an SSH agent: `SSH_AUTH_SOCK` on Unix, the OpenSSH agent pipe
//! or Pageant on Windows.

use russh::client::Handle;
use russh::keys::agent::client::AgentClient;
use russh::keys::{HashAlg, PublicKey};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::config::HostConfig;
use crate::connect::Client;

/// Ok(true) when a key from the agent worked.
pub async fn try_agent(
    handle: &mut Handle<Client>,
    user: &str,
    rsa_hash: Option<HashAlg>,
    host: &HostConfig,
) -> Result<bool, String> {
    #[cfg(unix)]
    {
        let agent = AgentClient::connect_env().await.map_err(|e| e.to_string())?;
        with_agent(agent, handle, user, rsa_hash, host).await
    }
    #[cfg(windows)]
    {
        if let Ok(agent) = AgentClient::connect_named_pipe(r"\\.\pipe\openssh-ssh-agent").await
            && with_agent(agent, handle, user, rsa_hash, host).await?
        {
            return Ok(true);
        }
        let agent = AgentClient::connect_pageant().await.map_err(|e| e.to_string())?;
        with_agent(agent, handle, user, rsa_hash, host).await
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (handle, user, rsa_hash, host);
        Err("no SSH agent on this platform".into())
    }
}

async fn with_agent<S: AsyncRead + AsyncWrite + Unpin + Send + 'static>(
    mut agent: AgentClient<S>,
    handle: &mut Handle<Client>,
    user: &str,
    rsa_hash: Option<HashAlg>,
    host: &HostConfig,
) -> Result<bool, String> {
    let ids = agent.request_identities().await.map_err(|e| e.to_string())?;
    // With IdentitiesOnly, only agent keys that match a configured key file are offered.
    let wanted: Vec<PublicKey> = if host.identities_only {
        host.identity_files.iter().filter_map(|f| russh::keys::load_public_key(f.with_extension("pub")).ok()).collect()
    } else {
        vec![]
    };
    for id in ids {
        let key = id.public_key().into_owned();
        if host.identities_only && !wanted.iter().any(|w| w.key_data() == key.key_data()) {
            continue;
        }
        let hash = if key.algorithm().is_rsa() { rsa_hash } else { None };
        match handle.authenticate_publickey_with(user, key, hash, &mut agent).await {
            Ok(r) if r.success() => return Ok(true),
            Ok(_) => {}
            Err(e) => tracing::debug!("agent key refused: {e}"),
        }
    }
    Ok(false)
}
