use serde::{Deserialize, Deserializer, Serialize};

// ── Trail ───────────────────────────────────────────────────────────────────

/// Typed evidence attached to a mechanical `act` trail entry.
/// The harness parses this out of `detail["evidence"]` and verifies it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActEvidence {
    GitTag { value: String, repo: String },
    GitPush { repo: String, #[serde(rename = "ref")] git_ref: String },
    BoiDispatch { spec_id: String },
    FileWritten { path: String },
}

// ── Trigger spec (event policy triggers) ─────────────────────────────────────

#[derive(Debug, Clone)]
pub struct TriggerSpec {
    pub event: String,
    pub condition: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum TriggerSpecRepr {
    Bare(String),
    Full {
        event: String,
        #[serde(default)]
        condition: Option<String>,
    },
}

impl<'de> Deserialize<'de> for TriggerSpec {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        match TriggerSpecRepr::deserialize(d)? {
            TriggerSpecRepr::Bare(s) => Ok(TriggerSpec { event: s, condition: None }),
            TriggerSpecRepr::Full { event, condition } => Ok(TriggerSpec { event, condition }),
        }
    }
}

impl Serialize for TriggerSpec {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct;
        if self.condition.is_none() {
            s.serialize_str(&self.event)
        } else {
            let mut st = s.serialize_struct("TriggerSpec", 2)?;
            st.serialize_field("event", &self.event)?;
            st.serialize_field("condition", self.condition.as_ref().unwrap())?;
            st.end()
        }
    }
}
