//! Watch 7, `egress_model` and `egress_refusals`. A refusal of a
//! configured provider's own model host (`egress::provider_rules`) means
//! an attempt could not reach its model: an ERROR. Every other refusal is
//! counted per host per day into one line, however many connections it
//! was (4,124 Datadog refusals filled the journal one line each).

use super::{Check, DAY, Status, check};
use crate::agent::Provider;
use crate::store::Store;
use std::collections::BTreeMap;

/// The one field of `audit::Outputs` read here.
#[derive(serde::Deserialize)]
struct Refusals {
    #[serde(default)]
    refused: Vec<crate::egress::Refused>,
}

pub(super) fn watch(store: &Store, providers: &BTreeMap<String, Provider>, now: i64) -> Vec<Check> {
    let rows = match store.refusals_since(now - DAY) {
        Ok(r) => r,
        Err(e) => return vec![check("egress_model", Status::Warn, format!("{e:#}"), "")],
    };
    let rules: Vec<(&str, crate::egress::Rule)> = providers
        .iter()
        .flat_map(|(name, p)| {
            crate::egress::provider_rules(p)
                .into_iter()
                .map(move |r| (name.as_str(), r))
        })
        .collect();
    // (day, host) -> count, and (provider, host, repo) -> count.
    let mut other: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut model: BTreeMap<(String, String, String), u64> = BTreeMap::new();
    for row in rows {
        let Ok(o) = serde_json::from_str::<Refusals>(&row.outputs_json) else {
            continue;
        };
        for r in o.refused {
            match rules
                .iter()
                .find(|(_, rule)| rule.matches(&r.host, r.port).is_some())
            {
                Some((provider, _)) => {
                    *model
                        .entry((provider.to_string(), r.host, row.repo.clone()))
                        .or_default() += r.count
                }
                None => {
                    *other
                        .entry((super::super::ymd(row.at), r.host))
                        .or_default() += r.count
                }
            }
        }
    }
    let model_row = if model.is_empty() {
        check(
            "egress_model",
            Status::Ok,
            "no configured provider's model host was refused in the last 24h",
            "",
        )
    } else {
        check(
            "egress_model",
            Status::Fail,
            format!(
                "the proxy refused a provider's own model host in the last 24h: {}",
                model
                    .iter()
                    .map(|((p, h, repo), n)| format!("{h} x{n} ({p}, {repo})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            "an attempt could not reach its model: compare the provider's base_url and env with \
             what egress::provider_rules allows",
        )
    };
    let mut counted: Vec<_> = other.into_iter().collect();
    counted.sort_by(|a, b| a.0.0.cmp(&b.0.0).then(b.1.cmp(&a.1)));
    let other_row = check(
        "egress_refusals",
        Status::Ok,
        if counted.is_empty() {
            "no other host refused in the last 24h".to_string()
        } else {
            format!(
                "refused per host per day: {}",
                counted
                    .iter()
                    .map(|((day, host), n)| format!("{day} {host} x{n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
        "",
    );
    vec![model_row, other_row]
}

#[cfg(test)]
mod tests {
    use super::super::fixture::{NOW, attempt_with, store, task};
    use super::*;

    fn providers() -> BTreeMap<String, Provider> {
        [("anthropic".to_string(), Provider::default())].into()
    }

    fn refused(s: &Store, host: &str, count: u64) {
        let t = task(s, "forge", "b", "failed");
        let o = format!(r#"{{"refused":[{{"host":"{host}","port":443,"count":{count}}}]}}"#);
        attempt_with(s, t, "code", "agent_failed", "", NOW - 60, "[]", &o);
    }

    #[test]
    fn a_refused_model_host_is_an_error_naming_it() {
        let (_d, s) = store();
        refused(&s, "api.anthropic.com", 3);
        let rows = watch(&s, &providers(), NOW);
        assert_eq!(rows[0].name, "egress_model");
        assert_eq!(rows[0].status, Status::Fail, "{}", rows[0].detail);
        assert!(
            rows[0].detail.contains("api.anthropic.com x3 (anthropic"),
            "{}",
            rows[0].detail
        );
    }

    #[test]
    fn other_refusals_are_one_ok_line_counted_per_host_per_day() {
        let (_d, s) = store();
        refused(&s, "http-intake.logs.datadoghq.com", 4000);
        refused(&s, "http-intake.logs.datadoghq.com", 124);
        let rows = watch(&s, &providers(), NOW);
        assert_eq!(rows[0].status, Status::Ok, "{}", rows[0].detail);
        assert_eq!(rows[1].status, Status::Ok);
        assert!(
            rows[1]
                .detail
                .contains("2026-10-09 http-intake.logs.datadoghq.com x4124"),
            "{}",
            rows[1].detail
        );
    }
}
