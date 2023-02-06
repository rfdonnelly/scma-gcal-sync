use crate::Connector;

use anyhow::Context;
use tracing::info;
use yup_oauth2::{
    authenticator::Authenticator, InstalledFlowAuthenticator, InstalledFlowReturnMethod,
    ServiceAccountAuthenticator,
};

pub struct GAuth {
    auth: Authenticator<Connector>,
}

impl GAuth {
    pub async fn with_oauth(
        client_secret_json_path: &str,
        oauth_token_json_path: &str,
    ) -> anyhow::Result<Self> {
        let secret = yup_oauth2::read_application_secret(client_secret_json_path)
            .await
            .with_context(|| {
                format!(
                    "could not read OAuth application secret from file `{client_secret_json_path}`"
                )
            })?;

        info!(client_id=?secret.client_id, "Authenticating using OAuth");
        let auth =
            InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
                .persist_tokens_to_disk(oauth_token_json_path)
                .build()
                .await?;

        Ok(Self { auth })
    }

    pub async fn with_service_account(client_secret_json_path: &str) -> anyhow::Result<Self> {
        let secret = yup_oauth2::read_service_account_key(client_secret_json_path)
            .await
            .with_context(|| {
                format!(
                    "could not read Google service account key from file `{client_secret_json_path}`"
                )
            })?;

        info!(client_id=?secret.client_id, client_email=?secret.client_email, "Authenticating using service account");
        let auth = ServiceAccountAuthenticator::builder(secret).build().await?;

        Ok(Self { auth })
    }

    pub fn auth(&self) -> &Authenticator<Connector> {
        &self.auth
    }
}

impl From<GAuth> for Authenticator<Connector> {
    fn from(gauth: GAuth) -> Self {
        gauth.auth
    }
}
