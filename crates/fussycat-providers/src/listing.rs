//! `--list-models PROVIDER`: one model per line, starting with the exact
//! `--model` value, then `key=value` facts. Unreported facts are left out,
//! never guessed. Pipe-friendly: `fussycat --list-models venice | grep tools=yes`.

use std::fmt;

use serde::de::DeserializeOwned;

use super::failure::{quote, replied, with_root_cause};

/// What a provider says about one of its models.
#[derive(Debug, Clone, PartialEq)]
pub struct Listed {
    /// The full `--model` value: `venice:claude-haiku-5-5`.
    pub model: String,
    /// The context window, in tokens.
    pub context: Option<u64>,
    /// Whether it can call tools. Without them a model can only answer.
    pub tools: Option<bool>,
    pub price: Option<Price>,
}

/// In US dollars per million tokens, as the provider lists it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_read: Option<f64>,
}

impl fmt::Display for Listed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.model)?;
        if let Some(tokens) = self.context {
            write!(f, " context={tokens}")?;
        }
        if let Some(tools) = self.tools {
            write!(f, " tools={}", if tools { "yes" } else { "no" })?;
        }
        if let Some(price) = self.price {
            write!(f, " in=${}/M out=${}/M", price.input, price.output)?;
            if let Some(cached) = price.cache_read {
                write!(f, " cached=${cached}/M")?;
            }
        }
        Ok(())
    }
}

/// GET `url` as JSON, failures described like a model call's.
pub async fn get<T: DeserializeOwned>(url: &str) -> Result<T, String> {
    fetch(super::client().get(url), url).await
}

/// [`get`] for a request that needs more than its URL (a key header);
/// `url` names it in errors.
pub async fn fetch<T: DeserializeOwned>(
    request: reqwest::RequestBuilder,
    url: &str,
) -> Result<T, String> {
    let reply = request
        .send()
        .await
        .map_err(|error| with_root_cause(&error))?;
    let status = reply.status();
    let body = reply
        .bytes()
        .await
        .map_err(|error| with_root_cause(&error))?;
    if !status.is_success() {
        return Err(replied(status.as_u16(), &String::from_utf8_lossy(&body)));
    }
    serde_json::from_slice(&body)
        .map_err(|error| quote(&format!("{}: unexpected reply: {error}", shown(url))))
}

/// `url` fit for an error: without its `user:password@`. reqwest moves
/// those into a header and keeps them out of its own errors; ours too.
fn shown(url: &str) -> String {
    // Without a host (`me:pw@x` parses, as scheme `me`), the userinfo can't
    // be found to strip, so the URL isn't shown at all.
    let Some(mut url) = reqwest::Url::parse(url).ok().filter(reqwest::Url::has_host) else {
        return "(a URL without a host)".into();
    };
    // Only `file:` refuses these, and it has no userinfo to strip.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.into()
}

/// Sorted by model, so two listings diff cleanly.
pub fn sorted(mut listed: Vec<Listed>) -> Vec<Listed> {
    listed.sort_by(|a, b| a.model.cmp(&b.model));
    listed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_starts_with_the_model_value_and_leaves_out_the_unknown() {
        let full = Listed {
            model: "venice:claude-haiku-5-5".into(),
            context: Some(1_000_000),
            tools: Some(true),
            price: Some(Price {
                input: 0.1,
                output: 0.5,
                cache_read: Some(0.01),
            }),
        };
        assert_eq!(
            full.to_string(),
            "venice:claude-haiku-5-5 context=1000000 tools=yes in=$0.1/M out=$0.5/M cached=$0.01/M"
        );
        let bare = Listed {
            model: "ollama:gemma2:2b".into(),
            context: None,
            tools: Some(false),
            price: None,
        };
        assert_eq!(bare.to_string(), "ollama:gemma2:2b tools=no");
    }

    #[test]
    fn a_url_in_an_error_never_shows_its_password() {
        assert_eq!(
            shown("https://me:hunter2@ollama.example:8443/api/tags"),
            "https://ollama.example:8443/api/tags"
        );
        assert_eq!(
            shown("https://token@ollama.example/api/tags"),
            "https://ollama.example/api/tags"
        );
        assert_eq!(
            shown("http://localhost:11434/api/tags"),
            "http://localhost:11434/api/tags"
        );
        for odd in ["me:hunter2@nope", "not a url"] {
            assert_eq!(shown(odd), "(a URL without a host)");
        }
    }
}
