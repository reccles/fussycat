//! Provider adapters: the only place the LLM library is used (eval S8).

mod anthropic;
mod failure;
mod listing;
mod messages;
mod ollama;
mod retry;
#[cfg(test)]
mod testkit;
mod venice;

use std::str::FromStr;
use std::time::Duration;

use fussycat_core::provider::{Provider, ProviderError, Request, Response};
use rig_core::http_client::DynHttpClient;

pub use anthropic::Anthropic;
pub use listing::Listed;
pub use ollama::Ollama;
pub use retry::Policy as RetryPolicy;
pub use venice::Venice;

fn http() -> DynHttpClient {
    DynHttpClient::new(rig_reqwest::ReqwestClient::from(client()))
}

/// A reachable host connects in milliseconds. Without a limit, an
/// unreachable one waits out the OS's own retries (a minute or two).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest silence on a connection. fussycat asks for a whole reply,
/// not a stream, so the provider sends nothing until it has finished
/// writing: the gap is the whole generation, and a slow local model takes
/// minutes over a long answer. Ten minutes is also Anthropic's limit for
/// a reply that isn't streamed. It restarts with every read, so a reply
/// that keeps arriving is never cut off.
const READ_TIMEOUT: Duration = Duration::from_secs(600);

/// The one HTTP client every request goes through (model calls and model
/// listings), so a stalled connection fails the call instead of hanging
/// the run. A timeout is a transport error, which rig judges retryable,
/// like a dropped connection.
fn client() -> reqwest::Client {
    client_with(CONNECT_TIMEOUT, READ_TIMEOUT)
}

fn client_with(connect: Duration, read: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(connect)
        .read_timeout(read)
        .build()
        // As `reqwest::Client::new()` does: only a TLS backend that can't
        // start fails here.
        .expect("an HTTP client")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderName {
    Anthropic,
    Ollama,
    Venice,
}

impl ProviderName {
    const ALL: [Self; 3] = [Self::Anthropic, Self::Ollama, Self::Venice];

    /// As spelled in `--model PROVIDER:NAME` and `--list-models`.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic",
            Self::Ollama => "ollama",
            Self::Venice => "venice",
        }
    }

    fn known() -> String {
        Self::ALL.map(Self::name).join(", ")
    }

    /// Whether every response says what it cost, so `--max-cost` can be
    /// kept. Ollama runs on your own hardware and reports no price;
    /// Anthropic reports tokens, not dollars.
    #[must_use]
    pub fn reports_cost(self) -> bool {
        match self {
            Self::Anthropic | Self::Ollama => false,
            Self::Venice => true,
        }
    }

    /// The environment variable holding the command that prints this
    /// provider's API key, if it needs one. The variable
    /// holds the command, never the key.
    #[must_use]
    pub fn key_command_env(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("FUSSYCAT_ANTHROPIC_KEY_COMMAND"),
            Self::Ollama => None,
            Self::Venice => Some("FUSSYCAT_VENICE_KEY_COMMAND"),
        }
    }

    /// Why `who` (this provider, or one of its models) can't go on without
    /// a key.
    #[must_use]
    pub fn needs_a_key(self, who: &dyn std::fmt::Display) -> String {
        let env = self.key_command_env().unwrap_or("a key command");
        format!(
            "{who} needs {env} set: a command that prints its API key, \
             e.g. a password manager's CLI"
        )
    }

    /// What the provider serves. Free: listing calls no model. Only
    /// Anthropic needs a key to list, so only it asks `key` for one.
    pub async fn list_models(
        self,
        ollama_url: &str,
        key: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Result<Vec<Listed>, String> {
        match self {
            Self::Anthropic => {
                let key = key()?.ok_or_else(|| self.needs_a_key(&self))?;
                anthropic::list(&key).await
            }
            Self::Ollama => ollama::list(ollama_url).await,
            Self::Venice => venice::list().await,
        }
    }
}

impl FromStr for ProviderName {
    type Err = String;

    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|provider| provider.name() == name)
            .ok_or_else(|| format!("unknown provider `{name}` (providers: {})", Self::known()))
    }
}

/// `--model PROVIDER:NAME`. Split at the first colon only, so Ollama's own
/// tags survive: `ollama:gpt-oss:20b`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelRef {
    pub provider: ProviderName,
    pub name: String,
    spelled: String,
}

impl FromStr for ModelRef {
    type Err = String;

    fn from_str(spelled: &str) -> Result<Self, Self::Err> {
        let usage = || {
            format!(
                "`{spelled}`: expected PROVIDER:MODEL, e.g. ollama:gpt-oss:20b or \
                 venice:claude-haiku-5-5 (providers: {})",
                ProviderName::known()
            )
        };
        let (provider, name) = spelled.split_once(':').ok_or_else(usage)?;
        let provider = provider.parse().map_err(|_| usage())?;
        if name.trim().is_empty() {
            return Err(usage());
        }
        Ok(Self {
            provider,
            name: name.to_owned(),
            spelled: spelled.to_owned(),
        })
    }
}

impl std::fmt::Display for ProviderName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.spelled)
    }
}

impl ModelRef {
    /// The provider for this model, with the `key` its
    /// [`key_command`](ProviderName::key_command_env) printed. A missing key
    /// is a usage error, found before any model call.
    pub fn connect(
        &self,
        ollama_url: &str,
        key: Option<String>,
        retry: RetryPolicy,
    ) -> Result<AnyProvider, String> {
        let key = || {
            key.filter(|key| !key.trim().is_empty())
                .ok_or_else(|| self.provider.needs_a_key(self))
        };
        Ok(match self.provider {
            ProviderName::Anthropic => {
                AnyProvider::Anthropic(Box::new(Anthropic::new(key()?, &self.name, retry)))
            }
            ProviderName::Ollama => AnyProvider::Ollama(Ollama::new(ollama_url, &self.name, retry)),
            ProviderName::Venice => {
                AnyProvider::Venice(Box::new(Venice::new(key()?, &self.name, retry)))
            }
        })
    }
}

/// The provider chosen on the command line.
pub enum AnyProvider {
    Anthropic(Box<Anthropic>),
    Ollama(Ollama),
    Venice(Box<Venice>),
}

impl Provider for AnyProvider {
    async fn complete(&self, request: &Request) -> Result<Response, ProviderError> {
        match self {
            Self::Anthropic(anthropic) => anthropic.complete(request).await,
            Self::Ollama(ollama) => ollama.complete(request).await,
            Self::Venice(venice) => venice.complete(request).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_name_their_provider_and_keep_their_own_colons() {
        let ollama: ModelRef = "ollama:gpt-oss:20b".parse().unwrap();
        assert_eq!(
            (ollama.provider, ollama.name.as_str()),
            (ProviderName::Ollama, "gpt-oss:20b")
        );
        assert_eq!(ollama.to_string(), "ollama:gpt-oss:20b");
        let venice: ModelRef = "venice:claude-haiku-5-5".parse().unwrap();
        assert_eq!(venice.provider, ProviderName::Venice);
    }

    #[test]
    fn a_model_without_a_known_provider_is_rejected() {
        for spelled in [
            "gpt-oss:20b",
            "gpt-oss",
            "openai:gpt-6",
            "venice:",
            "venice: ",
        ] {
            let error = spelled.parse::<ModelRef>().unwrap_err();
            assert!(
                error.contains("expected PROVIDER:MODEL"),
                "{spelled}: {error}"
            );
        }
    }

    #[test]
    fn a_provider_is_named_exactly() {
        assert_eq!("venice".parse(), Ok(ProviderName::Venice));
        assert_eq!(
            "Venice".parse::<ProviderName>(),
            Err("unknown provider `Venice` (providers: anthropic, ollama, venice)".into())
        );
    }

    #[test]
    fn venice_without_a_key_is_refused_before_any_call() {
        let venice: ModelRef = "venice:claude-haiku-5-5".parse().unwrap();
        for key in [None, Some(String::new())] {
            let Err(error) = venice.connect("http://unused", key, RetryPolicy::default()) else {
                panic!("connected without a key");
            };
            assert!(
                error.starts_with("venice:claude-haiku-5-5 needs FUSSYCAT_VENICE_KEY_COMMAND set"),
                "{error}"
            );
        }
    }
}
