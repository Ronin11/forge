//! Accept legacy snapshots while exposing generation-aware cursors as strings.
pub fn deserialize<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum Cursor {
        Text(String),
        Legacy(u64),
    }
    use serde::Deserialize;
    Ok(match Cursor::deserialize(d)? {
        Cursor::Text(s) => s,
        Cursor::Legacy(n) => format!("0:{n}"),
    })
}
