//! Bot command surface. The strings after a command name are passed
//! verbatim into the time-expression parser.

use teloxide::utils::command::BotCommands;

#[derive(BotCommands, Clone, Debug)]
#[command(
    rename_rule = "lowercase",
    description = "Alert bot commands",
    parse_with = "default"
)]
pub enum Command {
    #[command(description = "begrüßt dich und richtet dein Profil ein")]
    Start,

    #[command(description = "zeigt alle Befehle und Beispiele")]
    Help,

    #[command(description = "neuer Reminder — in Gruppen auch durch sichtbare Admins löschbar")]
    Alert(String),

    #[command(description = "Gruppen-Reminder — Bot-Adminrolle erforderlich")]
    Galert(String),

    #[command(description = "listet aktive Reminder im aktuellen Chat")]
    List,

    #[command(description = "löscht einen Reminder per ID")]
    Cancel(i64),

    #[command(description = "setzt deine Zeitzone, z.B. Europe/Berlin")]
    Tz(String),

    #[command(description = "wechselt die Sprache: de oder en")]
    Lang(String),
}
