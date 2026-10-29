use super::*;
use crate::providers::testkit::first_turn;

#[test]
fn usage_is_passed_through_as_reported() {
    let mut reported = rig_core::completion::Usage::default();
    reported.input_tokens = Some(629);
    reported.output_tokens = Some(2);
    reported.cached_input_tokens = Some(628);
    assert_eq!(
        usage(&reported),
        Usage {
            input: Some(629),
            output: Some(2),
            cache_read: Some(628),
            cache_write: None,
            cost_usd: None,
        }
    );
}

/// A reply as rig hands it over: some text, and how it ended.
fn reply(ending: serde_json::Value) -> CompletionResponse {
    let mut reply = serde_json::json!({
        "choice": [{"type": "text", "text": "It is twelve o'"}],
        "usage": {"input_tokens": 10, "output_tokens": 2},
        "origin": {"api": "chat", "provider": "ollama", "model": "gpt-oss:20b"},
        "raw": {}
    });
    if let (Some(reply), serde_json::Value::Object(ending)) = (reply.as_object_mut(), ending) {
        reply.extend(ending);
    }
    serde_json::from_value(reply).unwrap()
}

/// rig reads each provider's spelling (`max_tokens`, `length`, a full
/// context window); the core only needs to know the reply was cut off.
#[test]
fn the_stop_reason_survives_the_trip() {
    use serde_json::json;
    let stop = |ending| from_rig(&reply(ending), 0).map(|response| response.stop);
    assert_eq!(stop(json!({"finish_reason": "stop"})), Ok(Stop::End));
    assert_eq!(stop(json!({})), Ok(Stop::End), "unreported: as before");
    assert_eq!(
        stop(json!({"finish_reason": "length"})),
        Ok(Stop::OutputLimit)
    );
}

/// A refusal or filtered reply is the provider failing the turn, not an
/// answer: rig never replays it, so the run can't go on.
#[test]
fn a_turn_the_provider_failed_is_a_provider_error() {
    use serde_json::json;
    let refused = from_rig(
        &reply(json!({"finish_reason": "content_filter", "error": "I can't help with that"})),
        0,
    );
    assert_eq!(
        refused,
        Err(ProviderError::new(
            "the model's turn failed: I can't help with that"
        ))
    );
    let filtered = from_rig(&reply(json!({"finish_reason": "content_filter"})), 0);
    assert!(
        filtered
            .as_ref()
            .is_err_and(|error| error.message.contains("content_filter")),
        "{filtered:?}"
    );
}

#[test]
fn a_result_for_an_unknown_call_is_an_error() {
    let mut request = first_turn();
    request.log.push(Entry::ToolResults(vec![ToolResult {
        call_id: "ghost".into(),
        name: "now".into(),
        output: Ok(String::new()),
    }]));
    assert_eq!(
        to_rig(&request).unwrap_err(),
        ProviderError::new("tool result for unknown call `ghost`")
    );
}

/// A1: a cache point at each seam (tools, stable, log), none after the live
/// blocks, so a provider with breakpoints never caches what changes.
#[test]
fn the_seams_are_cache_points_and_live_is_left_out() {
    use crate::providers::testkit::{second_turn, with_live};
    use rig_core::completion::CachePoint::{Messages, System, Tools};
    assert_eq!(
        to_rig(&first_turn()).unwrap().cache_points,
        [Tools, System, Messages(1)]
    );
    // Task, the assistant's call, its result: three; the live message isn't counted.
    let live = with_live(second_turn(), "<live>noon</live>");
    assert_eq!(
        to_rig(&live).unwrap().cache_points,
        [Tools, System, Messages(3)]
    );
    let mut bare = first_turn();
    bare.tools.clear();
    bare.stable.clear();
    assert_eq!(to_rig(&bare).unwrap().cache_points, [Messages(1)]);
}
