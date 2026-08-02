//! Message templates with per-recipient variables (deliverable L-010-03).
//!
//! Spec §10.2 gives a campaign one message template holding `{{prenom}}`,
//! `{{ville}}` and the like, resolved per recipient from the contact
//! attributes of spec §11.1. This module is that resolution, and nothing else:
//! no database, no session, no clock.
//!
//! # The invariant, and why it is checked rather than intended
//!
//! CA-010-06: **no text holding an unresolved `{{…}}` is ever emitted.** Three
//! mechanisms hold it, in that order, and each one exists because the one
//! before it is not enough:
//!
//! 1. **Parsing happens once, up front.** A template that opens a placeholder
//!    it never closes, or names nothing, or nests, is refused by
//!    [`Template::parse`] — at campaign validation, when a human is looking,
//!    rather than half-way through 500 000 recipients. What comes out is a
//!    sequence of literals and named variables in which a placeholder is no
//!    longer a piece of text that could survive by accident.
//! 2. **Every variable is resolved or the recipient is rejected.**
//!    [`MissingVariablePolicy`] is an enum with two arms and no third option,
//!    so "leave the placeholder in" is not expressible. A value that *itself*
//!    holds `{{` is refused too ([`RenderError::PlaceholderInValue`]): the
//!    recipient cannot tell a placeholder that failed to resolve from one that
//!    arrived inside their own data.
//! 3. **The rendered text is counted before it is returned.** A `{{` may reach
//!    the output through exactly one door — the escape `{{{{` the operator
//!    wrote on purpose — and [`Template::parse`] knows how many times that door
//!    was used. If the count does not match, [`Template::render`] returns
//!    [`RenderError::UnresolvedPlaceholder`] instead of the text.
//!
//! The third is unreachable as long as the first two are correct, and that is
//! the point: it is what turns "we resolve every variable" from a claim about
//! the code into a property of the output. It costs one scan of a text that is
//! at most a few hundred characters.
//!
//! # Syntax
//!
//! | Source | Renders as |
//! |--------|------------|
//! | `{{prenom}}`, `{{ prenom }}` | the value of `prenom` |
//! | `{{{{` | a literal `{{` |
//! | `}}` outside a placeholder | a literal `}}` |
//!
//! `}}` needs no escape: it only means something after a `{{`, and a closing
//! pair on its own is ordinary text. `{{` does, and doubling it is the same
//! convention `format!` uses for its own braces.

use std::collections::BTreeMap;

/// What opens a placeholder.
const OPENING: &str = "{{";

/// What closes one.
const CLOSING: &str = "}}";

/// Characters a variable name may not hold.
///
/// Braces only, and that is deliberate: attribute keys come from spreadsheet
/// headers (`contacts::import`), so they hold accents, spaces and punctuation,
/// and a stricter charset would refuse `{{Nom complet}}` for a column the
/// operator can see. A brace inside a name, on the other hand, is always a
/// malformed template — `{{a{{b}}}}` — and never a header.
const ILLEGAL_IN_NAME: [char; 2] = ['{', '}'];

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

    /// A value would itself have put a placeholder in the message.
    #[error("the value of {variable} would put a placeholder in the message")]
    PlaceholderInValue {
        /// Name of the variable, never its value.
        variable: String,
    },

    /// The rendered text still holds a placeholder.
    #[error("the rendered message still holds an unresolved placeholder")]
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

    /// Reads the attributes of a contact (spec §11.1,
    /// [`contacts::model::Contact::attributes`]).
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
    /// placeholder: the trimming belongs to [`Template::parse`] and to
    /// [`Self::insert`], each of which runs **once** — per campaign and per
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
    /// How many literal `{{` the source asked for, through `{{{{`.
    ///
    /// The expected reading of the rendered text: see the module header, third
    /// mechanism.
    literal_openings: usize,
}

/// One piece of a parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Text to copy as is.
    Literal(String),
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
    /// which is what a nested `{{a{{b}}}}` looks like from here.
    pub fn parse(source: &str) -> Result<Self, TemplateError> {
        let mut segments = Vec::new();
        let mut literal = String::new();
        let mut literal_openings = 0;
        let mut rest = source;
        // Byte offset of `rest` within `source`, so errors point at the source.
        let mut consumed = 0;

        while let Some(relative) = rest.find(OPENING) {
            let opening = consumed + relative;

            literal.push_str(&rest[..relative]);

            // Everything after the `{{`. Slicing on an ASCII marker never
            // splits a character.
            let after_opening = &source[opening + OPENING.len()..];

            if after_opening.starts_with(OPENING) {
                literal.push_str(OPENING);
                literal_openings += 1;
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

            if name.contains(ILLEGAL_IN_NAME) {
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

        if !literal.is_empty() {
            segments.push(Segment::Literal(literal));
        }

        Ok(Self {
            segments,
            literal_openings,
        })
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
    /// [`RenderError::PlaceholderInValue`] when a value or a substitute would
    /// itself put `{{` in the message, and
    /// [`RenderError::UnresolvedPlaceholder`] if the finished text does not
    /// read the way the source asked for — the guard of the module header,
    /// which no correct rendering reaches.
    pub fn render(
        &self,
        variables: &Variables,
        on_missing: &MissingVariablePolicy,
    ) -> Result<String, RenderError> {
        let mut rendered = String::new();

        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => rendered.push_str(text),
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

                    if value.contains(OPENING) {
                        return Err(RenderError::PlaceholderInValue {
                            variable: name.clone(),
                        });
                    }

                    rendered.push_str(value);
                }
            }
        }

        // THE INVARIANT OF CA-010-06, checked rather than asserted. The only
        // `{{` a correct rendering can hold are the ones `{{{{` asked for, and
        // this is where that is established about the text itself rather than
        // about the code that built it.
        //
        // A `count_openings` above the expected number also catches the one
        // case the loop cannot: a literal ending in `{` followed by a value
        // starting with `{`. That text reads as a placeholder to whoever
        // receives it, which is what the criterion is about, so refusing it is
        // the right answer and not an over-reach.
        if count_openings(&rendered) != self.literal_openings {
            return Err(RenderError::UnresolvedPlaceholder);
        }

        Ok(rendered)
    }
}

/// How many non-overlapping `{{` a text holds.
fn count_openings(text: &str) -> usize {
    text.matches(OPENING).count()
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
            render("Écrire {{{{prenom}} pour personnaliser.", &Variables::new()),
            Ok(String::from("Écrire {{prenom}} pour personnaliser."))
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
    fn a_value_holding_a_placeholder_rejects_the_recipient() {
        let variables = Variables::new().with("prenom", "{{ville}}");

        assert_eq!(
            render("Bonjour {{prenom}}.", &variables),
            Err(RenderError::PlaceholderInValue {
                variable: String::from("prenom"),
            })
        );
    }

    #[test]
    fn a_substitute_holding_a_placeholder_rejects_the_recipient() {
        let rendered = Template::parse("Bonjour {{prenom}}.")
            .expect("the template parses")
            .render(
                &Variables::new(),
                &MissingVariablePolicy::Substitute(String::from("{{prenom}}")),
            );

        assert_eq!(
            rendered,
            Err(RenderError::PlaceholderInValue {
                variable: String::from("prenom"),
            })
        );
    }

    /// The guard of the module header, on the one input that reaches it: a
    /// value ending in `{` against a literal beginning with one. Neither half
    /// holds a placeholder, their concatenation reads as one, and the
    /// recipient cannot tell the difference.
    #[test]
    fn a_placeholder_formed_across_a_join_is_refused() {
        let variables = Variables::new().with("prenom", "Awa{");

        assert_eq!(
            render("{{prenom}}{suite", &variables),
            Err(RenderError::UnresolvedPlaceholder)
        );
    }

    #[test]
    fn a_placeholder_formed_between_two_values_is_refused() {
        let variables = Variables::new().with("a", "x{").with("b", "{y");

        assert_eq!(
            render("{{a}}{{b}}", &variables),
            Err(RenderError::UnresolvedPlaceholder)
        );
    }

    /// The escape is the **only** door: an escaped source may hold `{{` in its
    /// output, and the guard must not refuse it.
    #[test]
    fn an_escaped_source_may_hold_braces_in_its_output() {
        assert_eq!(
            render("{{{{}} {{{{{{{{", &Variables::new()),
            Ok(String::from("{{}} {{{{"))
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
