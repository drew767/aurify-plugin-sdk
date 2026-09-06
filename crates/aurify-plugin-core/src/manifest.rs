//! The plugin manifest: everything the client reads before installing anything.
//!
//! Anything not declared here is unavailable to the plugin. Silence means "no", never
//! "at the plugin's discretion" — a slot, an operation or a permission that is not in
//! the manifest cannot be reached at runtime even if the process would answer.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The only manifest schema version this crate understands.
pub const SCHEMA_VERSION: u32 = 1;

/// Where a plugin may write into the client. A closed list by construction: the client
/// renders only what it knows, so a plugin cannot declare a place of its own.
pub const SLOT_TYPES: &[&str] = &["ai-tab", "apps-card", "settings-entry", "message-action"];

/// The one slot every plugin must declare. Without a card in the Apps section there is
/// nowhere to remove the plugin from.
pub const REQUIRED_SLOT: &str = "apps-card";

/// Declarative components the client can draw. Closed for the same reason as slots.
pub const COMPONENT_TYPES: &[&str] = &["text", "list", "toggle", "button", "progress"];

pub const MAX_TITLE_CHARS: usize = 128;
pub const MAX_SUMMARY_CHARS: usize = 512;
pub const MAX_VERSION_CHARS: usize = 64;
pub const MAX_ID_CHARS: usize = 64;
pub const MIN_ID_CHARS: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Kind {
    /// Draws its own screen inside the client window and integrates nowhere else.
    App,
    /// Writes into existing client screens and runs a program on the person's machine.
    Plugin,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Slot {
    #[serde(rename = "type")]
    pub slot_type: String,
    /// Screen shown in this slot. Required for every slot type except `message-action`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screen: Option<String>,
    /// Operation run by the slot. Required for `message-action`, which has no screen:
    /// the action is a button, and the button does one thing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Icon name from the client's icon set. Unknown names fall back to the kind icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
}

/// One drawn thing on a declared screen. The client renders it with its own components,
/// so theme, font and language reach the plugin's screen on their own.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Component {
    #[serde(rename = "type")]
    pub component_type: String,
    /// Path into the screen's model, e.g. `items` or `status.message`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Operation run when the component is activated (button pressed, toggle flipped,
    /// list item chosen).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// For `list`: which model field of an item is its title, subtitle, and so on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Screen {
    pub id: String,
    pub title: String,
    /// Operation whose result is this screen's model. One dispatch path for data and
    /// actions alike, instead of a second protocol for "load".
    pub model_operation: String,
    #[serde(default)]
    pub components: Vec<Component>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// JSON Schema of the arguments, if the operation takes any. Not validated by this
    /// crate; carried for the client and for tooling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// What the plugin needs from the machine. Shown to the person before the download
/// starts, not after it.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resources {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_bytes: Option<u64>,
    /// `none`, `optional` or `required`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu: Option<String>,
}

/// How the client asks the process whether it is alive. Without it "not working" and
/// "still starting" are the same thing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
    #[serde(default = "default_probe_timeout")]
    pub timeout_seconds: u32,
}

fn default_probe_timeout() -> u32 {
    30
}

impl Default for Probe {
    fn default() -> Self {
        Self { timeout_seconds: default_probe_timeout() }
    }
}

/// What to remove besides the package itself. Otherwise a removed plugin leaves
/// gigabytes nobody owns.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Uninstall {
    /// Paths relative to the plugin's data directory.
    #[serde(default)]
    pub remove_paths: Vec<String>,
}

/// Where an app's screen comes from. Apps have no package: the code lives with the
/// author and never reaches the machine.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppEntry {
    pub url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub schema_version: u32,
    /// Stable identifier, `^[a-z][a-z0-9-]{2,63}$`. Never changes across versions.
    pub id: String,
    pub version: String,
    pub kind: Kind,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default)]
    pub slots: Vec<Slot>,
    #[serde(default)]
    pub screens: Vec<Screen>,
    #[serde(default)]
    pub operations: Vec<Operation>,
    /// Data kinds the plugin will ask for through the client's permission popup.
    /// Anything not listed cannot be asked for.
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub resources: Resources,
    #[serde(default)]
    pub probe: Probe,
    #[serde(default)]
    pub uninstall: Uninstall,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<AppEntry>,
}

impl Manifest {
    pub fn from_json(json: &str) -> Result<Self, String> {
        serde_json::from_str(json).map_err(|error| format!("manifest is not valid JSON: {error}"))
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("manifest serialises")
    }

    /// Every rule the client enforces, as messages a person can act on. Empty means valid.
    ///
    /// Rules live here rather than in a JSON Schema so that the same list runs in the
    /// plugin's own build, in the client before install, and in the catalog review —
    /// three places that would otherwise drift apart.
    pub fn validate(&self) -> Vec<String> {
        let mut errors = Vec::new();

        if self.schema_version != SCHEMA_VERSION {
            errors.push(format!(
                "schemaVersion must be {SCHEMA_VERSION}, got {}",
                self.schema_version
            ));
        }
        if !is_valid_id(&self.id) {
            errors.push(format!(
                "id must match ^[a-z][a-z0-9-]{{{},{}}}$, got {:?}",
                MIN_ID_CHARS - 1,
                MAX_ID_CHARS - 1,
                self.id
            ));
        }
        if self.version.is_empty() || self.version.chars().count() > MAX_VERSION_CHARS {
            errors.push(format!("version must be 1..{MAX_VERSION_CHARS} characters"));
        }
        if self.title.is_empty() || self.title.chars().count() > MAX_TITLE_CHARS {
            errors.push(format!("title must be 1..{MAX_TITLE_CHARS} characters"));
        }
        if let Some(summary) = &self.summary {
            if summary.chars().count() > MAX_SUMMARY_CHARS {
                errors.push(format!("summary must be at most {MAX_SUMMARY_CHARS} characters"));
            }
        }
        for scope in &self.permissions {
            if !is_valid_scope(scope) {
                errors.push(format!("permission {scope:?} must match ^[a-z][a-z0-9_.]{{2,63}}$"));
            }
        }
        if let Some(gpu) = &self.resources.gpu {
            if !matches!(gpu.as_str(), "none" | "optional" | "required") {
                errors.push(format!("resources.gpu must be none, optional or required, got {gpu:?}"));
            }
        }

        match self.kind {
            Kind::App => self.validate_app(&mut errors),
            Kind::Plugin => self.validate_plugin(&mut errors),
        }
        errors
    }

    fn validate_app(&self, errors: &mut Vec<String>) {
        match &self.entry {
            Some(entry) if entry.url.starts_with("https://") => {}
            Some(_) => errors.push("entry.url must start with https://".to_string()),
            None => errors.push("an app must declare entry.url".to_string()),
        }
        if !self.slots.is_empty() {
            errors.push("an app declares no slots: it draws its own screen and integrates nowhere".to_string());
        }
    }

    fn validate_plugin(&self, errors: &mut Vec<String>) {
        if self.entry.is_some() {
            errors.push("a plugin has no entry.url: its code runs on the machine, not at a URL".to_string());
        }

        let screen_ids: HashSet<&str> = self.screens.iter().map(|s| s.id.as_str()).collect();
        let operation_names: HashSet<&str> = self.operations.iter().map(|o| o.name.as_str()).collect();

        if screen_ids.len() != self.screens.len() {
            errors.push("screen ids must be unique".to_string());
        }
        if operation_names.len() != self.operations.len() {
            errors.push("operation names must be unique".to_string());
        }

        if !self.slots.iter().any(|s| s.slot_type == REQUIRED_SLOT) {
            errors.push(format!("a plugin must declare a {REQUIRED_SLOT:?} slot: it is the only place it can be removed from"));
        }
        for (index, slot) in self.slots.iter().enumerate() {
            if !SLOT_TYPES.contains(&slot.slot_type.as_str()) {
                errors.push(format!("slots[{index}].type {:?} is not one of {SLOT_TYPES:?}", slot.slot_type));
                continue;
            }
            if slot.slot_type == "message-action" {
                match &slot.operation {
                    Some(op) if operation_names.contains(op.as_str()) => {}
                    Some(op) => errors.push(format!("slots[{index}].operation {op:?} is not a declared operation")),
                    None => errors.push(format!("slots[{index}] (message-action) must name an operation")),
                }
            } else {
                match &slot.screen {
                    Some(screen) if screen_ids.contains(screen.as_str()) => {}
                    Some(screen) => errors.push(format!("slots[{index}].screen {screen:?} is not a declared screen")),
                    None => errors.push(format!("slots[{index}] ({}) must name a screen", slot.slot_type)),
                }
            }
        }

        for screen in &self.screens {
            if !operation_names.contains(screen.model_operation.as_str()) {
                errors.push(format!(
                    "screen {:?}: modelOperation {:?} is not a declared operation",
                    screen.id, screen.model_operation
                ));
            }
            for (index, component) in screen.components.iter().enumerate() {
                if !COMPONENT_TYPES.contains(&component.component_type.as_str()) {
                    errors.push(format!(
                        "screen {:?}: components[{index}].type {:?} is not one of {COMPONENT_TYPES:?}",
                        screen.id, component.component_type
                    ));
                }
                if let Some(op) = &component.operation {
                    if !operation_names.contains(op.as_str()) {
                        errors.push(format!(
                            "screen {:?}: components[{index}].operation {op:?} is not a declared operation",
                            screen.id
                        ));
                    }
                }
                let needs_bind = matches!(component.component_type.as_str(), "list" | "toggle" | "progress");
                if needs_bind && component.bind.is_none() {
                    errors.push(format!(
                        "screen {:?}: components[{index}] ({}) must bind to a model path",
                        screen.id, component.component_type
                    ));
                }
            }
        }
    }
}

fn is_valid_id(id: &str) -> bool {
    let length = id.len();
    if !(MIN_ID_CHARS..=MAX_ID_CHARS).contains(&length) {
        return false;
    }
    let mut chars = id.chars();
    let first = chars.next().unwrap_or(' ');
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_valid_scope(scope: &str) -> bool {
    let length = scope.len();
    if !(3..=64).contains(&length) {
        return false;
    }
    let mut chars = scope.chars();
    let first = chars.next().unwrap_or(' ');
    first.is_ascii_lowercase()
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_json() -> String {
        serde_json::json!({
            "schemaVersion": 1,
            "id": "local-ai",
            "version": "0.3.0",
            "kind": "plugin",
            "title": "Local AI",
            "slots": [
                {"type": "apps-card", "screen": "main"},
                {"type": "ai-tab", "screen": "main", "label": "Local models", "icon": "hard-drive"},
                {"type": "message-action", "operation": "summarize", "label": "Summarize"}
            ],
            "screens": [{
                "id": "main",
                "title": "Local models",
                "modelOperation": "main.model",
                "components": [
                    {"type": "list", "bind": "packages", "item": {"title": "displayName", "subtitle": "version"}},
                    {"type": "button", "label": "Refresh", "operation": "main.model"}
                ]
            }],
            "operations": [{"name": "main.model"}, {"name": "summarize"}],
            "permissions": ["identity.basic", "generation.jobs"],
            "resources": {"diskBytes": 300000000, "gpu": "optional"}
        })
        .to_string()
    }

    #[test]
    fn a_complete_plugin_manifest_validates() {
        let manifest = Manifest::from_json(&plugin_json()).unwrap();
        assert_eq!(manifest.validate(), Vec::<String>::new());
    }

    #[test]
    fn a_plugin_without_an_apps_card_cannot_be_removed_and_is_refused() {
        let mut manifest = Manifest::from_json(&plugin_json()).unwrap();
        manifest.slots.retain(|s| s.slot_type != "apps-card");
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.contains("apps-card")), "{errors:?}");
    }

    #[test]
    fn references_must_point_at_declared_things() {
        let mut manifest = Manifest::from_json(&plugin_json()).unwrap();
        manifest.slots[1].screen = Some("missing".into());
        manifest.screens[0].components[1].operation = Some("nope".into());
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.contains("\"missing\" is not a declared screen")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("\"nope\" is not a declared operation")), "{errors:?}");
    }

    #[test]
    fn unknown_slot_and_component_types_are_refused() {
        let mut manifest = Manifest::from_json(&plugin_json()).unwrap();
        manifest.slots.push(Slot { slot_type: "toolbar".into(), screen: Some("main".into()), operation: None, label: None, icon: None });
        manifest.screens[0].components.push(Component { component_type: "canvas".into(), bind: None, label: None, operation: None, item: None });
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.contains("\"toolbar\" is not one of")), "{errors:?}");
        assert!(errors.iter().any(|e| e.contains("\"canvas\" is not one of")), "{errors:?}");
    }

    #[test]
    fn ids_and_scopes_follow_the_catalog_constraints() {
        let mut manifest = Manifest::from_json(&plugin_json()).unwrap();
        manifest.id = "Local AI".into();
        manifest.permissions.push("Chats/Read".into());
        let errors = manifest.validate();
        assert!(errors.iter().any(|e| e.starts_with("id must match")), "{errors:?}");
        assert!(errors.iter().any(|e| e.starts_with("permission \"Chats/Read\"")), "{errors:?}");
    }

    #[test]
    fn an_app_needs_an_https_entry_and_no_slots() {
        let app = Manifest::from_json(&serde_json::json!({
            "schemaVersion": 1, "id": "weather", "version": "1", "kind": "app", "title": "Weather",
            "entry": {"url": "http://weather.example/app"}
        }).to_string()).unwrap();
        let errors = app.validate();
        assert_eq!(errors, vec!["entry.url must start with https://".to_string()]);
    }
}
