//! fussycat's provider adapters: the core's `Provider` port over
//! rig-core, the only place the LLM library is used (eval S8). A host
//! fetches keys and passes them in: this crate starts no processes and
//! prints nothing.

mod adapter;
mod anthropic;
mod failure;
mod listing;
mod llamacpp;
mod messages;
mod ollama;
mod retry;
#[cfg(test)]
mod testkit;
mod venice;

use std::str::FromStr;
use std::time::Duration;

use rig_core::http_client::DynHttpClient;

pub use adapter::Adapter;
pub use listing::Listed;
pub use retry::Policy as RetryPolicy;

/// The client a model's calls go through, giving up on a connection
/// silent for `read`.
fn http(read: Duration) -> DynHttpClient {
    DynHttpClient::new(rig_reqwest::ReqwestClient::from(client_with(
        CONNECT_TIMEOUT,
        read,
    )))
}

/// A reachable host connects in milliseconds. Without a limit, an
/// unreachable one waits out the OS's own retries (a minute or two).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest silence on a connection to a model on your own hardware.
/// fussycat asks for a whole reply, not a stream, so the provider sends
/// nothing until it has finished writing: the gap is the whole generation,
/// and a slow local model takes minutes over a long answer. It restarts
/// with every read, so a reply that keeps arriving is never cut off.
const LOCAL_READ_TIMEOUT: Duration = Duration::from_secs(600);
/// The same for a hosted API, which answers in seconds (18 s was the
/// slowest of 45 Venice runs, reasoning at its highest). A call that has
/// sent nothing for 3 minutes has stalled (one sat 6 minutes, open and
/// silent), and a cron run shouldn't wait out the local limit for it.
const API_READ_TIMEOUT: Duration = Duration::from_secs(180);

/// The client for model listings. Model calls get [`http`], with their
/// provider's limit or the author's. A timeout is a transport error, which
/// rig judges retryable, like a dropped connection.
fn client() -> reqwest::Client {
    client_with(CONNECT_TIMEOUT, LOCAL_READ_TIMEOUT)
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
    LlamaCpp,
    Ollama,
    Venice,
}

/// Where the servers on your own hardware are: `--ollama-url`,
/// `--llamacpp-url`.
#[derive(Debug, Clone, Copy)]
pub struct Endpoints<'a> {
    pub ollama: &'a str,
    pub llamacpp: &'a str,
}

/// Whether a provider takes an API key, and the environment variable
/// holding the command that prints it. The variable holds the command,
/// never the key.
#[derive(Clone, Copy)]
enum Key {
    None,
    Required(&'static str),
    /// A server you run may or may not have been started with a key.
    Optional(&'static str),
}

/// What fussycat knows about a provider before calling it.
struct Facts {
    /// As spelled in `--model PROVIDER:NAME` and `--list-models`.
    name: &'static str,
    /// Whether every response says what it cost, so `--max-cost` can be
    /// kept. Ollama runs on your own hardware and reports no price;
    /// Anthropic reports tokens, not dollars.
    reports_cost: bool,
    key: Key,
    /// How long a model call may go silent before it's given up on, unless
    /// the author says otherwise.
    read_timeout: Duration,
}

impl ProviderName {
    const ALL: [Self; 4] = [Self::Anthropic, Self::LlamaCpp, Self::Ollama, Self::Venice];

    const fn facts(self) -> Facts {
        match self {
            Self::Anthropic => Facts {
                name: "anthropic",
                reports_cost: false,
                key: Key::Required("FUSSYCAT_ANTHROPIC_KEY_COMMAND"),
                read_timeout: API_READ_TIMEOUT,
            },
            Self::LlamaCpp => Facts {
                name: "llamacpp",
                reports_cost: false,
                key: Key::Optional("FUSSYCAT_LLAMACPP_KEY_COMMAND"),
                read_timeout: LOCAL_READ_TIMEOUT,
            },
            Self::Ollama => Facts {
                name: "ollama",
                reports_cost: false,
                key: Key::None,
                read_timeout: LOCAL_READ_TIMEOUT,
            },
            Self::Venice => Facts {
                name: "venice",
                reports_cost: true,
                key: Key::Required("FUSSYCAT_VENICE_KEY_COMMAND"),
                read_timeout: API_READ_TIMEOUT,
            },
        }
    }

    /// As spelled in `--model PROVIDER:NAME` and `--list-models`.
    #[must_use]
    pub fn name(self) -> &'static str {
        self.facts().name
    }

    fn known() -> String {
        Self::ALL.map(Self::name).join(", ")
    }

    /// Whether every response says what it cost, so `--max-cost` can be
    /// kept.
    #[must_use]
    pub fn reports_cost(self) -> bool {
        self.facts().reports_cost
    }

    /// How long a model call may go silent before it's given up on (and
    /// retried, like a dropped connection): 3 minutes for a hosted API, 10
    /// for a model on your own hardware.
    #[must_use]
    pub fn read_timeout(self) -> Duration {
        self.facts().read_timeout
    }

    /// The environment variable holding the command that prints this
    /// provider's API key, if it takes one.
    #[must_use]
    pub fn key_command_env(self) -> Option<&'static str> {
        match self.facts().key {
            Key::None => None,
            Key::Required(env) | Key::Optional(env) => Some(env),
        }
    }

    /// Whether it can't be called without a key. llama.cpp takes one only
    /// if its server was started with one.
    #[must_use]
    pub fn key_required(self) -> bool {
        matches!(self.facts().key, Key::Required(_))
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
        endpoints: Endpoints<'_>,
        key: impl FnOnce() -> Result<Option<String>, String>,
    ) -> Result<Vec<Listed>, String> {
        match self {
            Self::Anthropic => {
                let key = key()?.ok_or_else(|| self.needs_a_key(&self))?;
                anthropic::list(&key).await
            }
            Self::LlamaCpp => llamacpp::list(endpoints.llamacpp).await,
            Self::Ollama => ollama::list(endpoints.ollama).await,
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
    /// [`key_command`](ProviderName::key_command_env) printed (which never
    /// prints an empty one). A call silent for `read_timeout` is given up
    /// on; `None` is [the provider's own](ProviderName::read_timeout). A
    /// missing key is a usage error, found before any model call.
    pub fn connect(
        &self,
        endpoints: Endpoints<'_>,
        key: Option<String>,
        retry: RetryPolicy,
        read_timeout: Option<Duration>,
    ) -> Result<Adapter, String> {
        let http = http(read_timeout.unwrap_or(self.provider.read_timeout()));
        let required = |key: Option<String>| key.ok_or_else(|| self.provider.needs_a_key(self));
        Ok(match self.provider {
            ProviderName::Anthropic => anthropic::connect(required(key)?, &self.name, retry, http),
            ProviderName::LlamaCpp => {
                llamacpp::connect(endpoints.llamacpp, key, &self.name, retry, http)
            }
            ProviderName::Ollama => ollama::connect(endpoints.ollama, &self.name, retry, http),
            ProviderName::Venice => venice::connect(required(key)?, &self.name, retry, http),
        })
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
            Err(
                "unknown provider `Venice` (providers: anthropic, llamacpp, ollama, venice)".into()
            )
        );
    }

    /// A hosted API answers in seconds, so it gets minutes; a model on
    /// your own hardware may take many over a long reply.
    #[test]
    fn a_hosted_api_is_given_up_on_sooner_than_a_local_model() {
        assert_eq!(ProviderName::Venice.read_timeout(), API_READ_TIMEOUT);
        assert_eq!(ProviderName::Anthropic.read_timeout(), API_READ_TIMEOUT);
        assert_eq!(ProviderName::Ollama.read_timeout(), LOCAL_READ_TIMEOUT);
        assert!(API_READ_TIMEOUT < LOCAL_READ_TIMEOUT);
    }

    /// A llama.cpp server takes a key only if it was started with one, so
    /// a run without its key command still connects.
    #[test]
    fn llamacpp_takes_a_key_only_if_its_server_wants_one() {
        assert_eq!(
            ProviderName::LlamaCpp.key_command_env(),
            Some("FUSSYCAT_LLAMACPP_KEY_COMMAND")
        );
        assert!(!ProviderName::LlamaCpp.key_required());
        assert!(ProviderName::Venice.key_required());
        assert!(!ProviderName::Ollama.key_required());
        let model: ModelRef = "llamacpp:qwen3.8-27b".parse().unwrap();
        let endpoints = Endpoints {
            ollama: "http://unused",
            llamacpp: "http://unused",
        };
        assert!(
            model
                .connect(endpoints, None, RetryPolicy::default(), None)
                .is_ok()
        );
    }

    #[test]
    fn venice_without_a_key_is_refused_before_any_call() {
        let venice: ModelRef = "venice:claude-haiku-5-5".parse().unwrap();
        let unused = Endpoints {
            ollama: "http://unused",
            llamacpp: "http://unused",
        };
        let Err(error) = venice.connect(unused, None, RetryPolicy::default(), None) else {
            panic!("connected without a key");
        };
        assert!(
            error.starts_with("venice:claude-haiku-5-5 needs FUSSYCAT_VENICE_KEY_COMMAND set"),
            "{error}"
        );
    }
}
