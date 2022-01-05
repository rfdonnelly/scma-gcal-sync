const LOGIN_URL: &str = "https://www.rockclimbing.org/index.php/component/comprofiler/login";
const EVENTS_URL: &str = "https://www.rockclimbing.org/index.php/event-list/events-list";

pub struct Web<'a> {
    username: &'a str,
    password: &'a str,
}

impl<'a> Web<'a> {
    pub fn new(username: &'a str, password: &'a str) -> Self {
        Self {
            username: username,
            password: password,
        }
    }

    pub async fn read(&self) -> Result<(), Box<dyn std::error::Error>> {
        let client = create_client()?;
        login(&client, self.username, self.password).await?;

        let rsp = client.get(EVENTS_URL).send().await?;
        let body = rsp.text().await?;
        println!("{:#?}", body);

        Ok(())
    }
}

fn create_client() -> Result<reqwest::Client, Box<dyn std::error::Error>> {
    Ok(
        reqwest::Client::builder()
            .cookie_store(true)
            .user_agent("Mozilla/5.0")
            .build()?
    )
}

async fn login<S>(client: &reqwest::Client, username: S, password: S) -> Result<(), Box<dyn std::error::Error>>
where
    S: AsRef<str>
{
    let login_params = [("username", username.as_ref()), ("passwd", password.as_ref())];
    let rsp = client.post(LOGIN_URL).form(&login_params).send().await?;

    if !rsp.status().is_success() {
        Err("login failed".into())
    } else if rsp.url().path() != "/" {
        Err("bad username or password".into())
    } else {
        Ok(())
    }
}
