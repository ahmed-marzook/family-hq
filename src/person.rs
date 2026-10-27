//! The people a command can be about.
//!
//! For now a `Person` is built from the webhook. Once the `users` table
//! exists (Stage 1.2), it becomes a row from there, with an id.

/// Someone the bot can identify on WhatsApp.
#[derive(Debug, Clone, PartialEq)]
pub struct Person {
    /// Their WhatsApp profile name, e.g. "Bilal".
    pub name: String,
    /// Phone-number ID, e.g. `447700900123@s.whatsapp.net`.
    pub jid: String,
    /// WhatsApp's newer "LID" for the same person, e.g. `100000000000001@lid`.
    /// Group member lists use these, so we need both to recognise someone.
    pub lid: Option<String>,
}

impl Person {
    /// The phone number from the JID: `447700900123@s.whatsapp.net` →
    /// `447700900123`. `split_once` returns `None` if there's no `@`, so
    /// `map_or` falls back to the whole JID.
    pub fn phone_number(&self) -> &str {
        self.jid
            .split_once('@')
            .map_or(&self.jid, |(number, _)| number)
    }
}

/// Who a todo is for.
///
/// An enum rather than one struct with optional fields, because the two
/// cases know different things: a typed `@name` is only text until it's
/// looked up, while the sender is fully known from the webhook.
#[derive(Debug, Clone, PartialEq)]
pub enum Assignee {
    /// `+todo @Khadija ...`: the name as typed. Not checked against anyone yet.
    Mentioned(String),
    /// `+todo ...` with no `@name`: the person who sent it.
    Sender(Person),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(jid: &str) -> Person {
        Person {
            name: "Test".to_string(),
            jid: jid.to_string(),
            lid: None,
        }
    }

    #[test]
    fn phone_number_strips_the_domain() {
        assert_eq!(
            person("447700900123@s.whatsapp.net").phone_number(),
            "447700900123"
        );
    }

    #[test]
    fn phone_number_without_domain_is_unchanged() {
        assert_eq!(person("447700900123").phone_number(), "447700900123");
    }
}
