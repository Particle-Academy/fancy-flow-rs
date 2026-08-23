//! Declarations of authorable node types.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use fancy_json::{Map, Value};

use crate::schema::{string_at, PortDescriptor};

/// One field in a kind's config schema — the form spec shared with the editor.
///
/// `field_type` is one of: `text`, `textarea`, `number`, `select`, `switch`,
/// `json`, `expression`, `credential`. Attributes irrelevant to a given type
/// stay `None`.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfigField {
    /// Which widget renders it.
    pub field_type: String,
    /// The config key it writes.
    pub key: String,
    /// Human label.
    pub label: String,
    /// Whether a value must be present.
    pub required: bool,
    /// The default, when one is declared.
    ///
    /// `None` is "no default declared", which is **not** the same as a default
    /// of `null`; that distinction is what stops a required field being
    /// silently satisfied by an absent one.
    pub default: Option<Value>,
    /// Help text.
    pub description: Option<String>,
    /// `{value, label}` pairs for a select.
    pub options: Vec<(String, String)>,
    /// Numeric bounds.
    pub min: Option<f64>,
    /// Numeric bounds.
    pub max: Option<f64>,
    /// Numeric step.
    pub step: Option<f64>,
    /// Placeholder text.
    pub placeholder: Option<String>,
    /// A worked example.
    pub example: Option<String>,
    /// Which credential type a `credential` field selects.
    pub credential_type: Option<String>,
    /// Rows for a textarea.
    pub rows: Option<i64>,
    /// Language for a code editor.
    pub language: Option<String>,
}

impl ConfigField {
    /// A plain text field.
    #[must_use]
    pub fn new(field_type: &str, key: &str, label: &str) -> Self {
        Self {
            field_type: field_type.to_string(),
            key: key.to_string(),
            label: label.to_string(),
            required: false,
            default: None,
            description: None,
            options: Vec::new(),
            min: None,
            max: None,
            step: None,
            placeholder: None,
            example: None,
            credential_type: None,
            rows: None,
            language: None,
        }
    }

    /// Mark it required, builder-style.
    #[must_use]
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Give it a default, builder-style.
    #[must_use]
    pub fn default(mut self, value: Value) -> Self {
        self.default = Some(value);
        self
    }

    /// Give a select its options, builder-style.
    #[must_use]
    pub fn options(mut self, options: &[(&str, &str)]) -> Self {
        self.options = options
            .iter()
            .map(|(v, l)| ((*v).to_string(), (*l).to_string()))
            .collect();
        self
    }

    /// Whether a default is declared.
    #[must_use]
    pub fn has_default(&self) -> bool {
        self.default.is_some()
    }

    /// Write it back out.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("type", Value::from(self.field_type.as_str()));
        map.insert("key", Value::from(self.key.as_str()));
        map.insert("label", Value::from(self.label.as_str()));
        if self.required {
            map.insert("required", Value::Bool(true));
        }
        if let Some(default) = &self.default {
            map.insert("default", default.clone());
        }
        for (key, value) in [
            ("description", self.description.as_deref()),
            ("placeholder", self.placeholder.as_deref()),
            ("example", self.example.as_deref()),
            ("credentialType", self.credential_type.as_deref()),
            ("language", self.language.as_deref()),
        ] {
            if let Some(value) = value {
                map.insert(key, Value::from(value));
            }
        }
        for (key, value) in [("min", self.min), ("max", self.max), ("step", self.step)] {
            if let Some(value) = value {
                map.insert(key, Value::from(value));
            }
        }
        if let Some(rows) = self.rows {
            map.insert("rows", Value::from(rows));
        }
        if !self.options.is_empty() {
            let options = self
                .options
                .iter()
                .map(|(value, label)| {
                    let mut option = Map::new();
                    option.insert("value", Value::from(value.as_str()));
                    option.insert("label", Value::from(label.as_str()));
                    Value::Object(option)
                })
                .collect();
            map.insert("options", Value::Array(options));
        }
        Value::Object(map)
    }
}

/// An authorable node type — its shape, ports and config schema.
///
/// `inputs` / `outputs` are nullable to preserve the "not declared" vs
/// "declared empty" distinction the engine reads.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeKind {
    /// The canonical id.
    pub name: String,
    /// `trigger`, `logic`, `data`, `ai`, `io`, `output`, `human`, `layout`,
    /// `annotation`, `structural`, `custom`.
    pub category: String,
    /// Display label.
    pub label: String,
    /// What it does.
    pub description: Option<String>,
    /// Icon name for the canvas.
    pub icon: Option<String>,
    /// Accent colour.
    pub accent: Option<String>,
    /// The config form.
    pub config_schema: Vec<ConfigField>,
    /// Config applied when a node of this kind is created.
    pub default_config: Map,
    /// Declared input ports; `None` means undeclared.
    pub inputs: Option<Vec<PortDescriptor>>,
    /// Declared output ports; `None` means undeclared.
    pub outputs: Option<Vec<PortDescriptor>>,
    /// Every previous spelling this kind still answers to.
    pub aliases: Vec<String>,
    /// Declares that this kind halts the run to wait for a person, and for what.
    ///
    /// `approval`, `input`, or a node's own (`signature`, `payment`). Only a
    /// declaration; the executor still emits the pause. Its value is that it is
    /// readable **without running the graph**, so a host learns it needs a
    /// resume path before the first run parks itself forever.
    pub pauses_for_human: Option<String>,
    /// What re-running this node costs: `none`, `idempotent`, `unsafe-to-replay`.
    ///
    /// A durable run RETRIES; `unsafe-to-replay` is the node saying a second
    /// attempt is not a repeat of the first. Only the durable driver reads it.
    pub side_effects: Option<String>,
}

impl NodeKind {
    /// A kind with the three fields every kind needs.
    #[must_use]
    pub fn new(name: &str, category: &str, label: &str) -> Self {
        Self {
            name: name.to_string(),
            category: category.to_string(),
            label: label.to_string(),
            description: None,
            icon: None,
            accent: None,
            config_schema: Vec::new(),
            default_config: Map::new(),
            inputs: None,
            outputs: None,
            aliases: Vec::new(),
            pauses_for_human: None,
            side_effects: None,
        }
    }

    /// Describe it, builder-style.
    #[must_use]
    pub fn describe(mut self, description: &str) -> Self {
        self.description = Some(description.to_string());
        self
    }

    /// Declare input ports, builder-style.
    #[must_use]
    pub fn inputs(mut self, ports: Vec<PortDescriptor>) -> Self {
        self.inputs = Some(ports);
        self
    }

    /// Declare output ports, builder-style.
    ///
    /// An EMPTY vec is meaningful: it says "this kind publishes nothing",
    /// which is what a terminal node wants and is not the same as leaving them
    /// undeclared.
    #[must_use]
    pub fn outputs(mut self, ports: Vec<PortDescriptor>) -> Self {
        self.outputs = Some(ports);
        self
    }

    /// Attach the config form, builder-style.
    #[must_use]
    pub fn config(mut self, fields: Vec<ConfigField>) -> Self {
        self.config_schema = fields;
        self
    }

    /// Add previous spellings, builder-style.
    #[must_use]
    pub fn aliases(mut self, aliases: Vec<String>) -> Self {
        self.aliases = aliases;
        self
    }

    /// Declare that this kind waits for a person, builder-style.
    #[must_use]
    pub fn pauses_for(mut self, awaiting: &str) -> Self {
        self.pauses_for_human = Some(awaiting.to_string());
        self
    }

    /// Declare the replay cost, builder-style.
    #[must_use]
    pub fn side_effects(mut self, effects: &str) -> Self {
        self.side_effects = Some(effects.to_string());
        self
    }

    /// Every id this kind answers to, canonical first.
    ///
    /// Anything keyed by kind name — executor bindings, node-type maps, policy
    /// allowlists — must key on ALL of these: a host that bound an executor
    /// under the bare name has to keep working, or a rename is a breaking
    /// change in disguise.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let mut seen = alloc::vec![self.name.clone()];
        for alias in &self.aliases {
            if !seen.contains(alias) {
                seen.push(alias.clone());
            }
        }
        seen
    }

    /// The default config a new node of this kind starts with.
    ///
    /// `default_config` first, then any `config_schema` field that declares a
    /// default and is not already set.
    #[must_use]
    pub fn resolved_default_config(&self) -> Map {
        let mut config = self.default_config.clone();
        for field in &self.config_schema {
            if let Some(default) = &field.default {
                if !config.contains_key(&field.key) {
                    config.insert(field.key.as_str(), default.clone());
                }
            }
        }
        config
    }

    /// Write the kind out as the editor's registry entry.
    #[must_use]
    pub fn to_value(&self) -> Value {
        let mut map = Map::new();
        map.insert("name", Value::from(self.name.as_str()));
        map.insert("category", Value::from(self.category.as_str()));
        map.insert("label", Value::from(self.label.as_str()));
        for (key, value) in [
            ("description", self.description.as_deref()),
            ("icon", self.icon.as_deref()),
            ("accent", self.accent.as_deref()),
        ] {
            if let Some(value) = value {
                map.insert(key, Value::from(value));
            }
        }
        if !self.config_schema.is_empty() {
            map.insert(
                "configSchema",
                Value::Array(
                    self.config_schema
                        .iter()
                        .map(ConfigField::to_value)
                        .collect(),
                ),
            );
        }
        if !self.default_config.is_empty() {
            map.insert("defaultConfig", Value::Object(self.default_config.clone()));
        }
        for (key, ports) in [("inputs", &self.inputs), ("outputs", &self.outputs)] {
            if let Some(ports) = ports {
                map.insert(
                    key,
                    Value::Array(ports.iter().map(PortDescriptor::to_value).collect()),
                );
            }
        }
        if !self.aliases.is_empty() {
            map.insert(
                "aliases",
                Value::Array(
                    self.aliases
                        .iter()
                        .map(|a| Value::from(a.as_str()))
                        .collect(),
                ),
            );
        }
        for (key, value) in [
            ("pausesForHuman", self.pauses_for_human.as_deref()),
            ("sideEffects", self.side_effects.as_deref()),
        ] {
            if let Some(value) = value {
                map.insert(key, Value::from(value));
            }
        }
        Value::Object(map)
    }

    /// Read a kind from a registry document.
    #[must_use]
    pub fn from_value(raw: &Value) -> Option<Self> {
        let name = string_at(raw, "name")?;
        let label = string_at(raw, "label").unwrap_or_else(|| name.clone());
        let mut kind = Self::new(
            &name,
            &string_at(raw, "category").unwrap_or_else(|| "custom".to_string()),
            &label,
        );
        kind.description = string_at(raw, "description");
        kind.icon = string_at(raw, "icon");
        kind.accent = string_at(raw, "accent");
        kind.pauses_for_human = string_at(raw, "pausesForHuman");
        kind.side_effects = string_at(raw, "sideEffects");
        if let Some(config) = raw.get("defaultConfig").and_then(Value::as_object) {
            kind.default_config = config.clone();
        }
        kind.inputs = ports_at(raw, "inputs");
        kind.outputs = ports_at(raw, "outputs");
        if let Some(aliases) = raw.get("aliases").and_then(Value::as_array) {
            kind.aliases = aliases
                .iter()
                .filter_map(|a| a.as_str().map(ToString::to_string))
                .collect();
        }
        Some(kind)
    }
}

fn ports_at(raw: &Value, key: &str) -> Option<Vec<PortDescriptor>> {
    // Only an ARRAY declares ports. An absent key and a non-array both mean
    // "undeclared", which the engine then falls back from; an empty array means
    // "declared, and there are none".
    let items = raw.get(key)?.as_array()?;
    Some(items.iter().map(PortDescriptor::from_value).collect())
}
