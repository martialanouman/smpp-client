//! Message templates with per-recipient variables (deliverable L-010-03).
//!
//! Spec §10.2 gives a campaign one message template holding `{{prenom}}`,
//! `{{ville}}` and the like, resolved per recipient from the contact
//! attributes of spec §11.1. This module is that resolution, and nothing else:
//! no database, no session, no clock.
//!
//! # The invariant, and why it is checked rather than intended
//!
//! CA-010-06: **no message holding a `{{…}}` is ever emitted.** Stated over the
//! finished text, not over the resolution: a recipient reading
//! `Bonjour {{prenom}}` cannot tell whether a variable failed to resolve, or a
//! spreadsheet cell happened to hold braces, or the operator escaped them on
//! purpose — and all three are the same defect from where they are standing.
//!
//! Three mechanisms hold it, and each one exists because the one before it is
//! not enough:
//!
//! 1. **Parsing happens once, up front.** A template that opens a placeholder
//!    it never closes, or names nothing, or nests, is refused by
//!    [`Template::parse`] — at campaign validation, when a human is looking,
//!    rather than half-way through 500 000 recipients. So is a template whose
//!    own text would read as a placeholder once rendered
//!    ([`TemplateError::WouldReadAsPlaceholder`]): an escaped `{{` with a `}}`
//!    anywhere after it produces `{{…}}` for every single recipient, and that
//!    is a mistake to catch on the editor's screen.
//! 2. **Every variable is resolved or the recipient is rejected, and a value
//!    may hold no brace at all.** [`MissingVariablePolicy`] is an enum with two
//!    arms and no third option, so "leave the placeholder in" is not
//!    expressible. A value carrying `{` or `}` is refused outright
//!    ([`RenderError::BraceInValue`]) — see below for why that is a single
//!    character and not the `{{` pair.
//! 3. **The finished text is read back before it is returned.** If it holds a
//!    `{{` with a `}}` anywhere after it, [`Template::render`] returns
//!    [`RenderError::UnresolvedPlaceholder`] instead of the message.
//!
//! The third is unreachable while the first two are correct, and that is the
//! point: it turns "we resolve every variable" from a claim about the code into
//! a property of the output. It costs one scan of a text that is at most a few
//! hundred characters.
//!
//! ## Why a value may hold no brace at all, rather than no `{{`
//!
//! Refusing only the pair was **wrong**, and a review found the counterexample:
//!
//! ```text
//! template "{{{{{{a}}"   +   a = "ville}}"   ->   "{{ville}}"
//! ```
//!
//! The escape contributes the `{{`, the value contributes the `}}`, neither
//! half holds a pair, and the recipient reads a placeholder. Any rule that
//! looks at the two halves separately misses it; any rule that counts `{{`
//! misses it twice, since the total is exactly what the escape asked for.
//!
//! So a substituted value contributes **no brace**, and every brace in a
//! message comes from the template — where the operator can see it, at
//! validation time, for every recipient at once. The cost is stated rather than
//! hidden: a contact whose *ville* is `Yamoussoukro {ancienne capitale}` is
//! rejected, by name, and the operator fixes the data. That is a rare row; a
//! message reading `{{ville}}` is a defect in front of a customer.
//!
//! # Syntax
//!
//! | Source | Renders as |
//! |--------|------------|
//! | `{{prenom}}`, `{{ prenom }}` | the value of `prenom` |
//! | `{{{{` | a literal `{{`, provided no `}}` follows |
//! | `}}` outside a placeholder | a literal `}}` |
//!
//! `}}` needs no escape: it only means something after a `{{`, and a closing
//! pair on its own is ordinary text. `{{` does, and doubling it is the same
//! convention `format!` uses for its own braces — but the escape lets a message
//! hold `{{`, never `{{…}}`.

use std::collections::BTreeMap;

/// What opens a placeholder.
const OPENING: &str = "{{";

/// What closes one.
const CLOSING: &str = "}}";

/// The two characters no substituted value may hold, and no variable name
/// either.
///
/// For a **name**, that is deliberate rather than restrictive: attribute keys
/// come from spreadsheet headers (`contacts::import`), so they hold accents,
/// spaces and punctuation, and a stricter charset would refuse `{{Nom complet}}`
/// for a column the operator can see. A brace inside a name, on the other hand,
/// is always a malformed template — `{{a{{b}}}}` — and never a header.
///
/// For a **value**, see the module header: a single brace is enough to form a
/// placeholder with what the template put next to it.
const BRACES: [char; 2] = ['{', '}'];

/// Why a template source could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TemplateError {
    /// A `{{` was opened and never closed.
    #[error("a placeholder opened at byte {offset} is never closed")]
    UnterminatedPlaceholder {
        /// Byte offset of the opening `{{`.
        offset: usize,
    },

    /// A placeholder holds no name.
    #[error("the placeholder at byte {offset} names no variable")]
    EmptyVariableName {
        /// Byte offset of the opening `{{`.
        offset: usize,
    },

    /// A placeholder name holds a character a name may not hold.
    #[error("the placeholder at byte {offset} holds an illegal variable name")]
    IllegalVariableName {
        /// Byte offset of the opening `{{`.
        offset: usize,
    },

    /// The template escapes a `{{` and a `}}` comes after it.
    ///
    /// Every message this template produces would hold `{{…}}`, which is what
    /// CA-010-06 forbids — so it is refused here, once, rather than for each of
    /// 500 000 recipients. Escaping is still available for a `{{` that no `}}`
    /// follows.
    #[error("the escaped braces at byte {offset} would read as a placeholder")]
    WouldReadAsPlaceholder {
        /// Byte offset of the escape, in the source.
        offset: usize,
    },
}

/// Why a message could not be built for one recipient.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RenderError {
    /// The stored attributes are not a JSON object.
    #[error("the recipient's attributes are not a JSON object")]
    MalformedAttributes,

    /// A variable has no value and the policy rejects the recipient.
    #[error("the recipient has no value for {variable}")]
    MissingVariable {
        /// Name of the variable, never its value.
        variable: String,
    },

    /// A value holds a brace, which the template could turn into a placeholder.
    ///
    /// A single `{` or `}`, not the pair: see the module header for the
    /// counterexample that forced it.
    #[error("the value of {variable} holds a brace")]
    BraceInValue {
        /// Name of the variable, never its value.
        variable: String,
    },

    /// The rendered text reads as a placeholder.
    #[error("the rendered message would read as holding a placeholder")]
    UnresolvedPlaceholder,
}

/// What to do with a variable the recipient has no value for.
///
/// An enum with exactly two arms, and no `LeaveAsIs`: spec §10.2 gives the
/// operator a choice between a default value and rejecting the line, and a
/// third arm that let the placeholder through would be the one bug CA-010-06
/// exists to prevent. A boolean would say the same thing while hiding where
/// the default text comes from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum MissingVariablePolicy {
    /// Put this text in place of the placeholder.
    ///
    /// The empty string is a legitimate substitute — "greet by name when we
    /// have one" — and is why this carries a `String` rather than being paired
    /// with a separate "blank it" arm.
    Substitute(String),

    /// Reject the recipient: no message is built, and none is sent.
    ///
    /// The default. A campaign whose template asks for a first name and whose
    /// import brought none should stop and say so, not send half a greeting to
    /// a hundred thousand people.
    #[default]
    Reject,
}

/// The variables of one recipient.
///
/// # A blank value is not a value
///
/// The importer writes an empty cell as `""` (`contacts::import::mapping`), so
/// a recipient whose *Prénom* column was empty carries `{"prenom": ""}` and not
/// an absent key. Treating that as a value would make
/// [`MissingVariablePolicy::Reject`] unreachable for exactly the rows it exists
/// to catch, and would send "Bonjour ," to every one of them. So a value that
/// is blank once trimmed is dropped on the way in, and this type has one notion
/// of "has a value" rather than two.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Variables {
    values: BTreeMap<String, String>,
}

impl Variables {
    /// No variable at all.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads the attributes of a contact (spec §11.1, the `attributes` column
    /// of `contacts::model::Contact` — named rather than linked, since this
    /// crate does not depend on `contacts` and must not start).
    ///
    /// `None` — a contact imported without attribute columns — is not a
    /// failure: it is a recipient for whom every variable is missing, and the
    /// policy decides what that means.
    ///
    /// # What becomes a value, and what does not
    ///
    /// A string, a number and a boolean become values, rendered the way JSON
    /// writes them. `null`, arrays and objects do **not**: there is no text
    /// form of them a recipient should read, and inventing one — `[1,2]` in the
    /// middle of a message — would be worse than the missing-variable policy
    /// the operator chose. They are dropped, so the variable is missing and the
    /// policy applies.
    ///
    /// Keys are trimmed, because a spreadsheet header often carries a trailing
    /// space and `{{prenom}}` should still find `"prenom "`.
    ///
    /// # Errors
    ///
    /// [`RenderError::MalformedAttributes`] if the text is not a JSON object —
    /// including a bare scalar or an array, which are valid JSON documents and
    /// still not what spec §11.1 stores. That is a defect in the stored row,
    /// not a missing variable, and silently reading it as "no attributes" would
    /// send a campaign's worth of unpersonalised messages.
    ///
    /// ```
    /// use messaging::template::Variables;
    ///
    /// let variables = Variables::from_attributes(Some(r#"{"prenom":"Awa"}"#))?;
    /// assert_eq!(variables.get("prenom"), Some("Awa"));
    /// # Ok::<(), messaging::template::RenderError>(())
    /// ```
    pub fn from_attributes(raw: Option<&str>) -> Result<Self, RenderError> {
        let Some(raw) = raw else {
            return Ok(Self::new());
        };

        let document: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| RenderError::MalformedAttributes)?;

        let serde_json::Value::Object(object) = document else {
            return Err(RenderError::MalformedAttributes);
        };

        let mut variables = Self::new();

        for (key, value) in object {
            let rendered = match value {
                serde_json::Value::String(text) => text,
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                serde_json::Value::Null
                | serde_json::Value::Array(_)
                | serde_json::Value::Object(_) => continue,
            };

            variables.insert(&key, &rendered);
        }

        Ok(variables)
    }

    /// Records one value, for a source of variables that is not a JSON
    /// document — a manually entered recipient (spec §10.2), or a test.
    ///
    /// A blank value records nothing: see the type's documentation.
    #[must_use]
    pub fn with(mut self, name: &str, value: &str) -> Self {
        self.insert(name, value);
        self
    }

    /// The value of one variable, or `None` when it has none.
    ///
    /// Names are matched **exactly**: `{{Prénom}}` does not find `prenom`. Case
    /// folding would have to be Unicode-aware to be worth anything, and a
    /// campaign editor that offers the keys the import wrote makes the guess
    /// unnecessary.
    ///
    /// Deliberately no `trim()` here, although `{{ prenom }}` is a legal
    /// placeholder: the trimming belongs to [`Template::parse`] and to the
    /// private `insert` below, each of which runs **once** — per campaign and per
    /// attribute — while this runs once per variable per recipient. Trimming
    /// here as well would be a second home for the same rule, and the two
    /// would hide each other's absence: a `parse` that stopped trimming would
    /// leave every test green.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Records `value` under `name`, unless either is blank.
    fn insert(&mut self, name: &str, value: &str) {
        let name = name.trim();

        if name.is_empty() || value.trim().is_empty() {
            return;
        }

        self.values.insert(name.to_owned(), value.to_owned());
    }
}

/// A parsed message template.
///
/// Parsed **once** per campaign and rendered once per recipient: a campaign of
/// 500 000 recipients (CA-010-01) parses one template and allocates one output
/// string per message, and the type exists so that shape is the only one
/// available.
///
/// ```
/// use messaging::template::{MissingVariablePolicy, Template, Variables};
///
/// let template = Template::parse("Bonjour {{prenom}}, à {{ville}}.")?;
/// assert_eq!(template.referenced_variables(), vec!["prenom", "ville"]);
///
/// let recipient = Variables::from_attributes(Some(r#"{"prenom":"Awa"}"#))?;
/// let rendered = template.render(
///     &recipient,
///     &MissingVariablePolicy::Substitute(String::from("chez vous")),
/// )?;
///
/// assert_eq!(rendered, "Bonjour Awa, à chez vous.");
/// # Ok::<(), messaging::MessagingError>(())
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    segments: Vec<Segment>,
}

/// One piece of a parsed template.
///
/// The escape is a **segment of its own** rather than two characters inside a
/// literal, so the braces the operator asked for are known by position and not
/// merely by number. Counting them was the bug of the first implementation: the
/// total said one `{{` was expected, and a `{{` formed somewhere else entirely
/// spent the allowance.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Text to copy as is. Holds no `{{`, by construction.
    Literal(String),
    /// An escaped `{{`, written `{{{{` in the source.
    EscapedOpening,
    /// A variable to resolve.
    Variable(String),
}

impl Template {
    /// Parses a template source.
    ///
    /// Every malformed placeholder is caught here, with the byte offset of the
    /// `{{` that caused it, so the campaign editor can point at it. Nothing is
    /// repaired: a template that cannot be read is not a template that should
    /// be sent to anybody.
    ///
    /// # Errors
    ///
    /// [`TemplateError::UnterminatedPlaceholder`] when a `{{` is never closed,
    /// [`TemplateError::EmptyVariableName`] when a placeholder names nothing,
    /// [`TemplateError::IllegalVariableName`] when a name holds a brace —
    /// which is what a nested `{{a{{b}}}}` looks like from here — and
    /// [`TemplateError::WouldReadAsPlaceholder`] when an escaped `{{` has a
    /// `}}` after it.
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut rest = source;
        // Byte offset of `rest` within `source`, so errors point at the source.
        let mut consumed = 0;
        // Where the first escape sits, in the source and in the text this
        // template renders when every value is taken out. Both are needed: the
        // check below is about the rendered text, the error is about the
        // source the operator typed.
        let mut first_escape: Option<(usize, usize)> = None;
        let mut skeleton = String::new();

        while let Some(relative) = rest.find(OPENING) {
            let opening = consumed + relative;

            literal.push_str(&rest[..relative]);
            skeleton.push_str(&rest[..relative]);

            // Everything after the `{{`. Slicing on an ASCII marker never
            // splits a character.
            let after_opening = &source[opening + OPENING.len()..];

            if after_opening.starts_with(OPENING) {
                if !literal.is_empty() {
                    segments.push(Segment::Literal(core::mem::take(&mut literal)));
                }

                first_escape.get_or_insert((skeleton.len(), opening));
                skeleton.push_str(OPENING);
                segments.push(Segment::EscapedOpening);

                consumed = opening + 2 * OPENING.len();
                rest = &source[consumed..];
                continue;
            }

            let closing = after_opening
                .find(CLOSING)
                .ok_or(TemplateError::UnterminatedPlaceholder { offset: opening })?;

            let name = after_opening[..closing].trim();

            if name.is_empty() {
                return Err(TemplateError::EmptyVariableName { offset: opening });
            }

            if name.contains(BRACES) {
                return Err(TemplateError::IllegalVariableName { offset: opening });
            }

            if !literal.is_empty() {
                segments.push(Segment::Literal(core::mem::take(&mut literal)));
            }

            segments.push(Segment::Variable(name.to_owned()));

            consumed = opening + OPENING.len() + closing + CLOSING.len();
            rest = &source[consumed..];
        }

        literal.push_str(rest);
        skeleton.push_str(rest);

        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }

        // Would every message this template produces read as a placeholder?
        //
        // `skeleton` is the rendered text with the values taken out, and that
        // is enough to answer: a value contributes no brace (see the module
        // header), so a `}}` in a message is a `}}` of the skeleton, and the
        // only `{{` a message can hold is an escape of the skeleton. Checking
        // the **first** escape covers the others: a `}}` after any of them is a
        // `}}` after the first.
        if let Some((at, offset)) = first_escape {
            if skeleton[at + OPENING.len()..].contains(CLOSING) {
                return Err(TemplateError::WouldReadAsPlaceholder { offset });
            }
        }

        Ok(Self { segments })
    }

    /// The variables this template references, each once, in the order they
    /// first appear.
    ///
    /// What the campaign validator compares against the columns an import
    /// produced, and what the editor lists next to the field.
    #[must_use]
    pub fn referenced_variables(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();

        for segment in &self.segments {
            if let Segment::Variable(name) = segment {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
        }

        names
    }

    /// Renders the message for one recipient.
    ///
    /// # Errors
    ///
    /// [`RenderError::MissingVariable`] when a variable has no value and the
    /// policy is [`MissingVariablePolicy::Reject`],
    /// [`RenderError::BraceInValue`] when a value or a substitute holds a
    /// brace, and [`RenderError::UnresolvedPlaceholder`] if the finished text
    /// reads as holding a placeholder — the guard of the module header, which
    /// no correct rendering reaches.
    pub fn render(
        &self,
        variables: &Variables,
        on_missing: &MissingVariablePolicy,
    ) -> Result<String, RenderError> {
        let mut rendered = String::new();

        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => rendered.push_str(text),
                Segment::EscapedOpening => rendered.push_str(OPENING),
                Segment::Variable(name) => {
                    let value = match variables.get(name) {
                        Some(value) => value,
                        None => match on_missing {
                            MissingVariablePolicy::Substitute(substitute) => substitute,
                            MissingVariablePolicy::Reject => {
                                return Err(RenderError::MissingVariable {
                                    variable: name.clone(),
                                })
                            }
                        },
                    };

                    if value.contains(BRACES) {
                        return Err(RenderError::BraceInValue {
                            variable: name.clone(),
                        });
                    }

                    rendered.push_str(value);
                }
            }
        }

        // THE INVARIANT OF CA-010-06, read back out of the finished text rather
        // than argued about the code that built it.
        //
        // Positional, not numerical: it asks what **follows** an opening, so a
        // `{{` the operator escaped and a `}}` that arrived from anywhere else
        // are caught together. Counting the openings was the first
        // implementation and it was wrong — the module header carries the
        // counterexample.
        if holds_placeholder_shape(&rendered) {
            return Err(RenderError::UnresolvedPlaceholder);
        }

        Ok(rendered)
    }
}

/// Whether a text holds a `{{` with a `}}` anywhere after it.
///
/// What a recipient reads as an unresolved placeholder, and the whole of the
/// criterion. Deliberately not "a well-formed placeholder": `{{ }}` and
/// `{{a{{b}}` are the same defect in front of a customer.
fn holds_placeholder_shape(text: &str) -> bool {
    text.find(OPENING)
        .is_some_and(|opening| text[opening + OPENING.len()..].contains(CLOSING))
}

#[cfg(test)]
mod tests {
    use super::{MissingVariablePolicy, RenderError, Template, TemplateError, Variables};

    fn render(source: &str, variables: &Variables) -> Result<String, RenderError> {
        Template::parse(source)
            .expect("the template parses")
            .render(variables, &MissingVariablePolicy::Substitute(String::new()))
    }

    #[test]
    fn a_template_without_a_variable_renders_unchanged() {
        assert_eq!(
            render("Bonjour, votre code est prêt.", &Variables::new()),
            Ok(String::from("Bonjour, votre code est prêt."))
        );
    }

    #[test]
    fn a_variable_is_replaced_by_its_value() {
        let variables = Variables::new()
            .with("prenom", "Awa")
            .with("ville", "Abidjan");

        assert_eq!(
            render("Bonjour {{prenom}}, à {{ville}} demain.", &variables),
            Ok(String::from("Bonjour Awa, à Abidjan demain."))
        );
    }

    #[test]
    fn the_spaces_around_a_name_are_not_part_of_it() {
        let variables = Variables::new().with("prenom", "Awa");

        assert_eq!(
            render("Bonjour {{ prenom }}.", &variables),
            Ok(String::from("Bonjour Awa."))
        );
    }

    #[test]
    fn a_missing_variable_takes_the_substitute_of_the_policy() {
        let rendered = Template::parse("Bonjour {{prenom}}.")
            .expect("the template parses")
            .render(
                &Variables::new(),
                &MissingVariablePolicy::Substitute(String::from("cher client")),
            );

        assert_eq!(rendered, Ok(String::from("Bonjour cher client.")));
    }

    #[test]
    fn a_missing_variable_rejects_the_recipient_under_the_reject_policy() {
        let rendered = Template::parse("Bonjour {{prenom}}.")
            .expect("the template parses")
            .render(&Variables::new(), &MissingVariablePolicy::Reject);

        assert_eq!(
            rendered,
            Err(RenderError::MissingVariable {
                variable: String::from("prenom"),
            })
        );
    }

    #[test]
    fn the_default_policy_rejects_rather_than_substitutes() {
        assert_eq!(
            MissingVariablePolicy::default(),
            MissingVariablePolicy::Reject
        );
    }

    /// An import writes an empty cell as `""`. A blank value that counted as
    /// present would make the reject policy unreachable for exactly the rows it
    /// exists to catch.
    #[test]
    fn a_blank_value_counts_as_missing() {
        let variables = Variables::new().with("prenom", "   ");

        assert_eq!(variables.get("prenom"), None);
        assert_eq!(
            Template::parse("Bonjour {{prenom}}.")
                .expect("the template parses")
                .render(&variables, &MissingVariablePolicy::Reject),
            Err(RenderError::MissingVariable {
                variable: String::from("prenom"),
            })
        );
    }

    #[test]
    fn a_doubled_opening_brace_renders_one_literal_pair() {
        assert_eq!(
            render("Écrire {{{{ pour ouvrir une variable.", &Variables::new()),
            Ok(String::from("Écrire {{ pour ouvrir une variable."))
        );
    }

    /// The escape emits `{{`; it does not license `{{…}}`. A template that
    /// would produce one for every recipient is refused where the operator can
    /// see it, not once per recipient.
    #[test]
    fn an_escape_with_a_closing_pair_after_it_is_refused_at_parse_time() {
        assert_eq!(
            Template::parse("Écrire {{{{prenom}} pour personnaliser."),
            Err(TemplateError::WouldReadAsPlaceholder { offset: 8 })
        );
        // The `}}` may be anywhere after it, and need not close anything.
        assert_eq!(
            Template::parse("{{{{ fin }}"),
            Err(TemplateError::WouldReadAsPlaceholder { offset: 0 })
        );
    }

    /// The other way round is ordinary text: a `}}` **before** the escape
    /// forms no shape.
    #[test]
    fn a_closing_pair_before_an_escape_is_ordinary_text() {
        assert_eq!(
            render("}} puis {{{{", &Variables::new()),
            Ok(String::from("}} puis {{"))
        );
    }

    #[test]
    fn a_closing_pair_outside_a_placeholder_is_ordinary_text() {
        assert_eq!(
            render("Fin de la promotion }} samedi.", &Variables::new()),
            Ok(String::from("Fin de la promotion }} samedi."))
        );
    }

    #[test]
    fn an_unterminated_placeholder_is_rejected_at_parse_time() {
        assert_eq!(
            Template::parse("Bonjour {{prenom"),
            Err(TemplateError::UnterminatedPlaceholder { offset: 8 })
        );
    }

    #[test]
    fn a_placeholder_without_a_name_is_rejected_at_parse_time() {
        assert_eq!(
            Template::parse("Bonjour {{}}."),
            Err(TemplateError::EmptyVariableName { offset: 8 })
        );
        assert_eq!(
            Template::parse("Bonjour {{   }}."),
            Err(TemplateError::EmptyVariableName { offset: 8 })
        );
    }

    #[test]
    fn a_nested_placeholder_is_rejected_at_parse_time() {
        assert_eq!(
            Template::parse("Bonjour {{a{{b}}}}."),
            Err(TemplateError::IllegalVariableName { offset: 8 })
        );
    }

    #[test]
    fn the_referenced_variables_are_listed_once_each_in_order() {
        let template =
            Template::parse("{{ville}} {{prenom}} {{ville}}").expect("the template parses");

        assert_eq!(template.referenced_variables(), vec!["ville", "prenom"]);
    }

    /// The name a placeholder carries is normalised at parse time, so
    /// `{{ ville }}` and `{{ville}}` are the same variable — for the editor
    /// listing them as much as for the lookup.
    #[test]
    fn a_spaced_name_and_a_bare_one_are_the_same_variable() {
        let template = Template::parse("{{ ville }} {{ville}}").expect("the template parses");

        assert_eq!(template.referenced_variables(), vec!["ville"]);
    }

    #[test]
    fn absent_attributes_leave_every_variable_without_a_value() {
        let variables = Variables::from_attributes(None).expect("no attributes is not a failure");

        assert_eq!(variables.get("prenom"), None);
    }

    #[test]
    fn attributes_that_are_not_an_object_are_rejected() {
        for raw in ["[1, 2]", "\"Awa\"", "42", "null", "not json at all"] {
            assert_eq!(
                Variables::from_attributes(Some(raw)),
                Err(RenderError::MalformedAttributes),
                "{raw} must be rejected"
            );
        }
    }

    #[test]
    fn a_json_object_gives_its_string_entries() {
        let variables = Variables::from_attributes(Some(r#"{"prenom":"Awa","ville":"Abidjan"}"#))
            .expect("a JSON object is accepted");

        assert_eq!(variables.get("prenom"), Some("Awa"));
        assert_eq!(variables.get("ville"), Some("Abidjan"));
    }

    #[test]
    fn a_json_scalar_that_is_not_a_string_is_rendered_as_written() {
        let variables = Variables::from_attributes(Some(r#"{"age":30,"vip":true}"#))
            .expect("a JSON object is accepted");

        assert_eq!(variables.get("age"), Some("30"));
        assert_eq!(variables.get("vip"), Some("true"));
    }

    #[test]
    fn a_null_or_structured_attribute_has_no_value() {
        let variables =
            Variables::from_attributes(Some(r#"{"a":null,"b":[1],"c":{"d":1},"e":""}"#))
                .expect("a JSON object is accepted");

        for name in ["a", "b", "c", "e"] {
            assert_eq!(variables.get(name), None, "{name} must have no value");
        }
    }

    /// CA-010-06: a value that itself looks like a placeholder would put
    /// `{{…}}` in front of the recipient just as surely as a resolution
    /// failure would.
    #[test]
    fn a_value_holding_a_brace_rejects_the_recipient() {
        let variables = Variables::new().with("prenom", "{{ville}}");

        assert_eq!(
            render("Bonjour {{prenom}}.", &variables),
            Err(RenderError::BraceInValue {
                variable: String::from("prenom"),
            })
        );
    }

    #[test]
    fn a_substitute_holding_a_brace_rejects_the_recipient() {
        let rendered = Template::parse("Bonjour {{prenom}}.")
            .expect("the template parses")
            .render(
                &Variables::new(),
                &MissingVariablePolicy::Substitute(String::from("{{prenom}}")),
            );

        assert_eq!(
            rendered,
            Err(RenderError::BraceInValue {
                variable: String::from("prenom"),
            })
        );
    }

    /// A value ending in `{` against a literal beginning with one: neither
    /// half holds a pair, their concatenation does. Caught by the value rule —
    /// a single brace is enough to refuse the row.
    #[test]
    fn a_brace_formed_across_a_join_is_refused() {
        let variables = Variables::new().with("prenom", "Awa{");

        assert_eq!(
            render("{{prenom}}{suite", &variables),
            Err(RenderError::BraceInValue {
                variable: String::from("prenom"),
            })
        );
    }

    #[test]
    fn a_brace_formed_between_two_values_is_refused() {
        let variables = Variables::new().with("a", "x{").with("b", "{y");

        assert_eq!(
            render("{{a}}{{b}}", &variables),
            Err(RenderError::BraceInValue {
                variable: String::from("a"),
            })
        );
    }

    /// A lone brace in a value is refused even when nothing around it could
    /// form a pair: the rule is the character, not the pair, and a rule that
    /// tried to be cleverer is what let the review's counterexample through.
    #[test]
    fn a_single_brace_in_a_value_is_enough_to_reject_the_recipient() {
        for value in ["Yamoussoukro {ancienne capitale}", "a{", "}b"] {
            let variables = Variables::new().with("ville", value);

            assert_eq!(
                render("Bonjour, à {{ville}}.", &variables),
                Err(RenderError::BraceInValue {
                    variable: String::from("ville"),
                }),
                "{value} must be refused"
            );
        }
    }

    /// The escape is the **only** door: an escaped source may hold `{{` in its
    /// output, and the guard must not refuse it.
    #[test]
    fn an_escaped_source_may_hold_braces_in_its_output() {
        assert_eq!(
            render("{{{{ et {{{{{{{{", &Variables::new()),
            Ok(String::from("{{ et {{{{"))
        );
    }

    /// Regression, found in review: the escape granted a budget of one `{{`,
    /// and a **value** carrying the matching `}}` spent it somewhere else. The
    /// recipient read `{{ville}}`, which is exactly what CA-010-06 forbids.
    ///
    /// The counting guard could not see it — the total was right — and neither
    /// could a check for `{{` inside the value, the value holding only `}}`.
    #[test]
    fn an_escaped_opening_and_a_value_cannot_form_a_placeholder_between_them() {
        let variables = Variables::new().with("a", "ville}}");

        assert!(
            render("{{{{{{a}}", &variables).is_err(),
            "the recipient would read a placeholder"
        );

        let variables = Variables::new().with("a", "prenom}}");

        assert!(
            render("Ex: {{{{{{a}} ok", &variables).is_err(),
            "the recipient would read a placeholder"
        );
    }

    /// The same counterexample, saying **which** mechanism owns it: the value
    /// rule, at the first pass. The test above asserts only that the message is
    /// refused, on purpose — it is what shows the final guard catching the case
    /// when the value rule is weakened back to its old form.
    #[test]
    fn the_counterexample_is_refused_by_the_value_rule() {
        let variables = Variables::new().with("a", "ville}}");

        assert_eq!(
            render("{{{{{{a}}", &variables),
            Err(RenderError::BraceInValue {
                variable: String::from("a"),
            })
        );
    }

    /// An error names the variable and never its value: an attribute is
    /// personal data (CLAUDE.md §8).
    #[test]
    fn a_rejection_never_quotes_the_value() {
        let variables = Variables::new().with("prenom", "{{secret}}");
        let rejection = render("Bonjour {{prenom}}.", &variables).expect_err("must be rejected");

        assert!(!rejection.to_string().contains("secret"), "{rejection}");
    }
}
