use anyhow::{Context, Result};
use inkue_control::{Command, Credentials, LiveStatus};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{fs::File, path::Path, time::Duration};

pub struct LiveDesk {
    credentials: Credentials,
    client: ureq::Agent,
}

#[derive(Debug)]
pub(crate) struct DeskResponseError {
    pub status: u16,
    message: String,
}

impl std::fmt::Display for DeskResponseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "desk returned HTTP {}: {}",
            self.status, self.message
        )
    }
}

impl std::error::Error for DeskResponseError {}

impl LiveDesk {
    pub fn connect(file: &Path) -> Result<Self> {
        let credentials: Credentials =
            serde_json::from_reader(File::open(file).context("open operator session file")?)
                .context("invalid operator session file")?;
        anyhow::ensure!(
            !credentials.token.is_empty(),
            "invalid operator session file"
        );
        let client = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(4)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Self {
            credentials,
            client,
        })
    }

    fn post(
        &self,
        route: &str,
        request: &impl Serialize,
    ) -> Result<ureq::http::Response<ureq::Body>> {
        let url = format!("http://127.0.0.1:{}{route}", self.credentials.port);
        self.client
            .post(url)
            .header(
                "Authorization",
                format!("Bearer {}", self.credentials.token),
            )
            .send_json(request)
            .context("contact local desk")
    }

    pub fn send(&self, command: &Command) -> Result<Value> {
        let mut reply = self.post("/v1/command", command)?;
        let status = reply.status();
        let value: Value = reply.body_mut().read_json()?;
        if !status.is_success() {
            return Err(DeskResponseError {
                status: status.as_u16(),
                message: value["error"]
                    .as_str()
                    .unwrap_or("request failed")
                    .to_owned(),
            }
            .into());
        }
        Ok(value)
    }

    pub(crate) fn review<T: DeserializeOwned>(&self, request: &impl Serialize) -> Result<T> {
        let mut reply = self.post("/v1/review", request)?;
        let status = reply.status().as_u16();
        if !reply.status().is_success() {
            let error = reply
                .body_mut()
                .with_config()
                .limit(4096)
                .read_json::<Value>()
                .ok();
            let message = error.as_ref().and_then(|value| value["error"].as_str())
                .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
                .map(|text| text.replace(&self.credentials.token, "[redacted]"))
                .unwrap_or_else(|| match status {
                    401 | 403 => "Review access denied. Use the review session file from the desk.".into(),
                    410 => "The review session closed. Start a new review session at the desk.".into(),
                    409 => "The desk could not apply the request. Read the current value before retrying.".into(),
                    _ => "Review request failed.".into(),
                });
            return Err(DeskResponseError { status, message }.into());
        }
        reply
            .body_mut()
            .read_json()
            .context("invalid desk review response")
    }

    pub fn status(&self) -> Result<LiveStatus> {
        serde_json::from_value(self.send(&Command::LiveStatus)?).context("invalid desk status")
    }
}
