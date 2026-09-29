//! The job vocabulary (docs/JOBS.md): what starts a job, a run workflow's
//! budget and failure policy, and the side effect one of its operations
//! performs.

use super::*;

/// What starts a job (docs/JOBS.md, "Trigger"). Serialized by its
/// lowercase name; an unknown value is a TOML deserialize error, refused
/// with the file and line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TriggerOn {
    Manual,
    Schedule,
    Message,
    Webhook,
    Event,
}

impl TriggerOn {
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerOn::Manual => "manual",
            TriggerOn::Schedule => "schedule",
            TriggerOn::Message => "message",
            TriggerOn::Webhook => "webhook",
            TriggerOn::Event => "event",
        }
    }

    /// The field `on` requires alongside it: `schedule` a cron
    /// expression, `message` a contact group, `webhook` a name, `event`
    /// a Forge event type; `manual` needs none.
    fn field(self) -> Option<&'static str> {
        match self {
            TriggerOn::Manual => None,
            TriggerOn::Schedule => Some("cron"),
            TriggerOn::Message => Some("contact"),
            TriggerOn::Webhook => Some("name"),
            TriggerOn::Event => Some("type"),
        }
    }
}

impl std::fmt::Display for TriggerOn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What starts a job, and its one field (docs/JOBS.md, "Trigger").
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Trigger {
    pub on: TriggerOn,
    pub cron: Option<String>,
    pub contact: Option<String>,
    pub name: Option<String>,
    pub r#type: Option<String>,
    /// Seconds to wait after the firing event before the job is due
    /// (docs/JOBS.md, "Delayed jobs"): parsed from a duration string
    /// (`s`, `m`, `h`, `d`) at workflow load time. `None` fires the job
    /// immediately, as before this field existed.
    pub delay: Option<i64>,
}

impl Trigger {
    /// Whether this trigger fires for an inbound message from `contact`
    /// (docs/JOBS.md, "Triggers"): `on = "message"` and a `contact` that
    /// is `"*"` or equals it.
    pub fn matches_message(&self, contact: &str) -> bool {
        self.on == TriggerOn::Message
            && self
                .contact
                .as_deref()
                .is_some_and(|c| c == "*" || c == contact)
    }

    /// Whether this trigger fires for the webhook `name` (docs/JOBS.md,
    /// "Triggers"): `on = "webhook"` and a `name` equal to it.
    pub fn matches_webhook(&self, name: &str) -> bool {
        self.on == TriggerOn::Webhook && self.name.as_deref() == Some(name)
    }

    /// Whether this trigger fires for an event of `event_type`
    /// (docs/JOBS.md, "Triggers"): `on = "event"` and a `type` equal to it.
    pub fn matches_event(&self, event_type: &str) -> bool {
        self.on == TriggerOn::Event && self.r#type.as_deref() == Some(event_type)
    }

    /// The value of the one field `on` names, for display.
    pub fn value(&self) -> Option<&str> {
        match self.on {
            TriggerOn::Manual => None,
            TriggerOn::Schedule => self.cron.as_deref(),
            TriggerOn::Message => self.contact.as_deref(),
            TriggerOn::Webhook => self.name.as_deref(),
            TriggerOn::Event => self.r#type.as_deref(),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct TriggerRaw {
    on: TriggerOn,
    #[serde(default, deserialize_with = "deserialize_cron")]
    cron: Option<String>,
    #[serde(default)]
    contact: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default, deserialize_with = "deserialize_delay")]
    delay: Option<i64>,
}

/// Parses `cron` with `croner` at load time, so a schedule trigger that can
/// never fire is refused the same way an unknown `on` value is: as a TOML
/// deserialize error, with the file and the line (docs/JOBS.md, "Triggers").
/// The worker's schedule tick (`src/worker.rs`) can then assume every
/// `Trigger::cron` it sees already parses.
fn deserialize_cron<'de, D>(deserializer: D) -> std::result::Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let cron: Option<String> = Option::deserialize(deserializer)?;
    if let Some(expr) = &cron {
        Cron::from_str(expr)
            .map_err(|e| serde::de::Error::custom(format!("invalid cron {expr:?}: {e}")))?;
    }
    Ok(cron)
}

/// A duration string (a number followed by `s`, `m`, `h`, or `d`) in
/// seconds: `"5m"` is 300, `"1h"` is 3600, `"0s"` is 0. Shared by
/// `[trigger] delay` (validated at workflow load time, below) and `forge
/// job start --delay` (docs/JOBS.md, "Delayed jobs").
pub fn parse_duration(s: &str) -> std::result::Result<i64, String> {
    let bad = || {
        format!("invalid duration {s:?}: expected a number followed by s, m, h, or d, e.g. \"5m\"")
    };
    if s.is_empty() {
        return Err(bad());
    }
    let (digits, unit) = s.split_at(s.len() - 1);
    let secs_per_unit = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return Err(bad()),
    };
    let n: i64 = digits.parse().map_err(|_| bad())?;
    if n < 0 {
        return Err(bad());
    }
    Ok(n * secs_per_unit)
}

/// Parses `delay` with `parse_duration` at load time, so a delay that
/// cannot be parsed is refused the same way an invalid cron is: as a TOML
/// deserialize error, with the file and the line (docs/JOBS.md, "Delayed
/// jobs").
fn deserialize_delay<'de, D>(deserializer: D) -> std::result::Result<Option<i64>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let delay: Option<String> = Option::deserialize(deserializer)?;
    delay
        .map(|s| parse_duration(&s).map_err(serde::de::Error::custom))
        .transpose()
}

/// Validate a `[trigger]` table: the one field `on` requires is present
/// and non-empty, and no other trigger field is set.
pub(super) fn build_trigger(path: &Path, raw: TriggerRaw) -> Result<Trigger> {
    let fields: [(&str, &Option<String>); 4] = [
        ("cron", &raw.cron),
        ("contact", &raw.contact),
        ("name", &raw.name),
        ("type", &raw.r#type),
    ];
    let want = raw.on.field();
    for (field, val) in fields {
        let wanted = Some(field) == want;
        if wanted && val.as_deref().is_none_or(str::is_empty) {
            bail!(
                "{}: [trigger] on = \"{}\" needs `{field}`",
                path.display(),
                raw.on.as_str()
            );
        }
        if !wanted && val.is_some() {
            bail!(
                "{}: [trigger] on = \"{}\" does not take `{field}`",
                path.display(),
                raw.on.as_str()
            );
        }
    }
    if let Some(t) = raw.r#type.as_deref()
        && !crate::report::EVENT_TYPES.contains(&t)
    {
        bail!(
            "{}: [trigger] type = {t:?} is not a Forge event type (one of: {})",
            path.display(),
            crate::report::EVENT_TYPES.join(", ")
        );
    }
    Ok(Trigger {
        on: raw.on,
        cron: raw.cron,
        contact: raw.contact,
        name: raw.name,
        r#type: raw.r#type,
        delay: raw.delay,
    })
}

/// A side effect on the world an operation performs (docs/JOBS.md,
/// "Effect"). Serialized by its lowercase name; an unknown value is a
/// TOML deserialize error, refused with the file and line.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectKind {
    Message,
    Row,
    File,
    Http,
}

impl EffectKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EffectKind::Message => "message",
            EffectKind::Row => "row",
            EffectKind::File => "file",
            EffectKind::Http => "http",
        }
    }
}

impl std::fmt::Display for EffectKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What to do when a job's assertions fail (docs/JOBS.md, "Limits").
/// `retry:N` carries its count; the rest are unit values. Serialized as
/// the string form (`ask:contact`, `retry:2`, ...); an unknown value is
/// refused with the file and line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OnFailure {
    AskContact,
    AskOperator,
    Retry(u32),
    Drop,
}

impl OnFailure {
    pub fn as_str(&self) -> String {
        match self {
            OnFailure::AskContact => "ask:contact".to_string(),
            OnFailure::AskOperator => "ask:operator".to_string(),
            OnFailure::Retry(n) => format!("retry:{n}"),
            OnFailure::Drop => "drop".to_string(),
        }
    }

    pub fn parse(s: &str) -> Option<OnFailure> {
        match s {
            "ask:contact" => Some(OnFailure::AskContact),
            "ask:operator" => Some(OnFailure::AskOperator),
            "drop" => Some(OnFailure::Drop),
            _ => s
                .strip_prefix("retry:")
                .and_then(|n| n.parse::<u32>().ok())
                .map(OnFailure::Retry),
        }
    }
}

impl std::fmt::Display for OnFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.as_str())
    }
}

impl Serialize for OnFailure {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        s.serialize_str(&self.as_str())
    }
}

impl<'de> Deserialize<'de> for OnFailure {
    fn deserialize<D>(d: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(d)?;
        OnFailure::parse(&s).ok_or_else(|| {
            serde::de::Error::custom(format!(
                "on_failure {s:?} is not ask:contact, ask:operator, retry:N, or drop"
            ))
        })
    }
}

/// A run workflow's budget and failure policy (docs/JOBS.md, "Limits").
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub budget_usd: f64,
    pub per_day: u32,
    pub on_failure: OnFailure,
    /// The bound on a directive step's inputs (the input document plus
    /// every earlier step's output, as text) in bytes; default 32 kB
    /// (docs/JOBS.md, "Steps").
    #[serde(default = "default_input_bytes")]
    pub input_bytes: usize,
}

pub fn default_input_bytes() -> usize {
    32 * 1024
}

/// How much of an operation's stdout and stderr the kernel keeps on its
/// `ops` row: `tail`, the last 40 lines, or `full`, the whole thing capped
/// at 1 MB.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Output {
    #[default]
    Tail,
    Full,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(home: &Path, rel: &str, text: &str) {
        std::fs::write(home.join("workflows").join(rel), text).unwrap();
    }

    #[test]
    fn an_unknown_trigger_on_is_refused_with_the_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "carrier-pigeon.toml",
            "name = \"carrier-pigeon\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"carrier-pigeon\"\n",
        );
        let err = get(dir.path(), "carrier-pigeon").unwrap_err().to_string();
        assert!(err.contains("carrier-pigeon.toml"), "{err}");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("unknown variant"), "{err}");
    }

    #[test]
    fn an_invalid_cron_is_refused_at_parse_time_with_the_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "off-the-rails.toml",
            "name = \"off-the-rails\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"schedule\"\ncron = \"not a cron\"\n",
        );
        let err = get(dir.path(), "off-the-rails").unwrap_err().to_string();
        assert!(err.contains("off-the-rails.toml"), "{err}");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("invalid cron"), "{err}");
    }

    #[test]
    fn an_event_trigger_names_a_forge_event_type_or_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "on-landing.toml",
            "name = \"on-landing\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"event\"\ntype = \"landing\"\n",
        );
        let err = get(dir.path(), "on-landing").unwrap_err().to_string();
        assert!(err.contains("on-landing.toml"), "{err}");
        assert!(err.contains("not a Forge event type"), "{err}");
        assert!(err.contains("task_done"), "{err}");
        std::fs::remove_file(dir.path().join("workflows/on-landing.toml")).unwrap();
        write(
            dir.path(),
            "on-done.toml",
            "name = \"on-done\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"event\"\ntype = \"task_done\"\n",
        );
        let w = get(dir.path(), "on-done").unwrap().unwrap();
        let t = w.trigger.unwrap();
        assert!(t.matches_event("task_done"));
        assert!(!t.matches_event("deploy_finished"));
    }

    #[test]
    fn parse_duration_reads_s_m_h_d_and_rejects_junk() {
        assert_eq!(parse_duration("0s"), Ok(0));
        assert_eq!(parse_duration("5s"), Ok(5));
        assert_eq!(parse_duration("5m"), Ok(300));
        assert_eq!(parse_duration("1h"), Ok(3600));
        assert_eq!(parse_duration("2d"), Ok(172_800));
        assert!(parse_duration("").is_err());
        assert!(parse_duration("5").is_err());
        assert!(parse_duration("m").is_err());
        assert!(parse_duration("5mins").is_err());
        assert!(parse_duration("5x").is_err());
        assert!(parse_duration("-5m").is_err());
    }

    #[test]
    fn a_trigger_delay_parses_into_seconds() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "quote-later.toml",
            "name = \"quote-later\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"manual\"\ndelay = \"5m\"\n",
        );
        let w = get(dir.path(), "quote-later").unwrap().unwrap();
        assert_eq!(w.trigger.as_ref().unwrap().delay, Some(300));
    }

    #[test]
    fn an_invalid_trigger_delay_is_refused_at_parse_time_with_the_file_and_line() {
        let dir = tempfile::tempdir().unwrap();
        load_all(dir.path()).unwrap();
        write(
            dir.path(),
            "quote-never.toml",
            "name = \"quote-never\"\nkind = \"run\"\nsteps = [{ action = \"code\" }]\n[trigger]\non = \"manual\"\ndelay = \"soon\"\n",
        );
        let err = get(dir.path(), "quote-never").unwrap_err().to_string();
        assert!(err.contains("quote-never.toml"), "{err}");
        assert!(err.contains("line"), "{err}");
        assert!(err.contains("invalid duration"), "{err}");
    }

    #[test]
    fn on_failure_round_trips_and_rejects_junk() {
        for (s, want) in [
            ("ask:contact", OnFailure::AskContact),
            ("ask:operator", OnFailure::AskOperator),
            ("retry:3", OnFailure::Retry(3)),
            ("drop", OnFailure::Drop),
        ] {
            assert_eq!(OnFailure::parse(s), Some(want.clone()));
            assert_eq!(want.as_str(), s);
        }
        assert_eq!(OnFailure::parse("retry:"), None);
        assert_eq!(OnFailure::parse("retry:x"), None);
        assert_eq!(OnFailure::parse("ask"), None);
        for on in [
            TriggerOn::Manual,
            TriggerOn::Schedule,
            TriggerOn::Message,
            TriggerOn::Webhook,
            TriggerOn::Event,
        ] {
            let json = serde_json::to_string(&on).unwrap();
            assert_eq!(json, format!("\"{}\"", on.as_str()));
        }
        for e in [
            EffectKind::Message,
            EffectKind::Row,
            EffectKind::File,
            EffectKind::Http,
        ] {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(json, format!("\"{}\"", e.as_str()));
        }
    }
}
