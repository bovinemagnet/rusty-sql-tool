//! What an object *is*, as opposed to the fact that it exists (Phase 2 §10).
//!
//! GPUI-independent by design, in the same spirit as `QueryResult`: the model decides its own
//! layout so a definition can be asserted in tests with no window, and rendered differently or
//! exported later.

use crate::database::{DatabaseObject, ObjectKind};

/// The definition of one database object. An enum rather than a struct of optional sections, so
/// states such as a sequence carrying foreign keys cannot be constructed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ObjectDefinition {
    /// §5: the object exists but PostgreSQL offers no meaningful definition for its kind.
    Unsupported { kind: ObjectKind, reason: String },
}

/// One block of a rendered definition. `Rows` is pre-aligned monospace text; `Sql` is handed to
/// the editor's highlighter (§7); `Note` explains an absence (§5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DefinitionSection {
    Rows { heading: String, lines: Vec<String> },
    Sql { heading: String, sql: String },
    Note { text: String },
}

impl ObjectDefinition {
    /// The definition laid out for display. Takes the object because a foreign key's referenced
    /// table is qualified only when it lives in a different schema from the object being shown.
    pub fn sections(&self, _object: &DatabaseObject) -> Vec<DefinitionSection> {
        match self {
            Self::Unsupported { reason, .. } => vec![DefinitionSection::Note {
                text: reason.clone(),
            }],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::ObjectKind;

    fn object(name: &str, kind: ObjectKind) -> DatabaseObject {
        DatabaseObject {
            schema: "public".into(),
            name: name.into(),
            kind,
        }
    }

    /// §5: an object PostgreSQL cannot describe says so, rather than rendering an empty panel.
    #[test]
    fn an_unsupported_object_renders_a_note_rather_than_nothing() {
        let definition = ObjectDefinition::Unsupported {
            kind: ObjectKind::Type,
            reason: "PostgreSQL provides no definition for this object".into(),
        };

        let sections = definition.sections(&object("mood", ObjectKind::Type));

        assert_eq!(
            sections,
            vec![DefinitionSection::Note {
                text: "PostgreSQL provides no definition for this object".into(),
            }]
        );
    }
}
