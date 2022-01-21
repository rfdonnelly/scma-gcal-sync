use hyper::client::HttpConnector;
use hyper_rustls::HttpsConnector;
use tracing::info;
use yup_oauth2::{
    authenticator::Authenticator, InstalledFlowAuthenticator, InstalledFlowReturnMethod,
};

pub struct GAuth {
    auth: Authenticator<HttpsConnector<HttpConnector>>,
}

impl GAuth {
    pub async fn new(
        client_secret_json_path: &str,
        oauth_token_json_path: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let secret = yup_oauth2::read_application_secret(client_secret_json_path).await?;

        info!(oauth_client_id=?secret.client_id, "Authenticating");
        let auth =
            InstalledFlowAuthenticator::builder(secret, InstalledFlowReturnMethod::HTTPRedirect)
                .persist_tokens_to_disk(oauth_token_json_path)
                .build()
                .await?;

        Ok(Self { auth })
    }

    pub fn auth(&self) -> &Authenticator<HttpsConnector<HttpConnector>> {
        &self.auth
    }
}

impl From<GAuth> for Authenticator<HttpsConnector<HttpConnector>> {
    fn from(gauth: GAuth) -> Self {
        gauth.auth
    }
}
