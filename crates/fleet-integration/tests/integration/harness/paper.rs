//! The Paper harness for the live trading cases (Plan.md P16.17, P16.18;
//! ADR-0049): the image `just paper-image` builds, `afkfleet-paper:test`,
//! never pulled, and what its test plugin writes into the server's log.
//!
//! - [`Paper`]: a container of it, its game port published on `127.0.0.1`
//!   and a port Docker picks, RCON reached inside the container with the
//!   image's own `rcon`, its log read whole, and the server frozen and
//!   thawed (`docker pause`) for a fate that comes late or never.
//! - [`Record`]: one of the plugin's `harness-…` lines (a click with the
//!   window and state the server had and the listing it met, a purchase, a
//!   screen opened or closed, a drop, a drink, …) or the answer of `harness
//!   state`, read into its kind and fields. The cases count from these, the
//!   server's own records, never from the bot's word alone.

use core::time::Duration;
use std::collections::BTreeMap;

use testcontainers::core::{ExecCommand, IntoContainerPort as _, WaitFor};
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, GenericImage, ImageExt as _};

use crate::managed_check::minecraft::ended_or_released;

/// The harness's image, as `just paper-image` builds it.
pub(crate) const IMAGE: &str = "afkfleet-paper";
/// Its tag.
pub(crate) const TAG: &str = "test";
/// The game port inside the container.
const GAME_PORT: u16 = 25_565;
/// Paper's start from the built image: no download, a flat world.
pub(crate) const STARTUP: Duration = Duration::from_secs(90);
/// One `docker` command of the CLI's own: an image's inspection, a pause.
const DOCKER: Duration = Duration::from_secs(20);

/// A running Paper harness; see the [module docs](self).
pub(crate) struct Paper {
    container: ContainerAsync<GenericImage>,
    port: u16,
}

impl Paper {
    /// Starts the harness and waits until Paper is done loading, or says
    /// why not. The image must have been built: `just test-slow` and `just
    /// long-run` build it first, and nothing pulls one of that name.
    pub(crate) async fn start() -> Result<Self, String> {
        let built = docker(&[
            "image",
            "inspect",
            "--format",
            "{{.Id}}",
            &format!("{IMAGE}:{TAG}"),
        ])
        .await
        .map_err(|_| "docker didn't answer: is Docker Desktop running?".to_owned())?;
        if !built {
            return Err(
                "the Paper harness's image isn't built: run `just paper-image` (`just test-slow` does)"
                    .to_owned(),
            );
        }
        let container = GenericImage::new(IMAGE, TAG)
            .with_wait_for(WaitFor::message_on_stdout("Done ("))
            .with_mapped_port(0, GAME_PORT.tcp())
            .with_host_config_modifier(|config| {
                // Only the game port, and only on loopback: an offline-mode
                // server lets anyone join under any name.
                config.publish_all_ports = Some(false);
                for binding in config
                    .port_bindings
                    .iter_mut()
                    .flat_map(|bindings| bindings.values_mut())
                    .flatten()
                    .flatten()
                {
                    binding.host_ip = Some("127.0.0.1".to_owned());
                }
            })
            .with_startup_timeout(STARTUP)
            .start()
            .await
            .map_err(|_| "the Paper harness didn't start".to_owned())?;
        let port = container
            .get_host_port_ipv4(GAME_PORT.tcp())
            .await
            .map_err(|_| "the harness's game port isn't published".to_owned())?;
        Ok(Self { container, port })
    }

    /// The address the bots are sent to.
    pub(crate) fn address(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// Runs `command` through the image's `rcon` inside the container,
    /// within `within`, and returns its output.
    ///
    /// # Errors
    /// A fixed text when it didn't run or end in time.
    pub(crate) async fn rcon(
        &self,
        command: &str,
        within: Duration,
    ) -> Result<String, &'static str> {
        let run = async {
            let mut result = self
                .container
                .exec(ExecCommand::new(["rcon", command]))
                .await
                .map_err(|_| "rcon couldn't run")?;
            let stdout = result
                .stdout_to_vec()
                .await
                .map_err(|_| "rcon's output couldn't be read")?;
            Ok(String::from_utf8_lossy(&stdout).trim().to_owned())
        };
        tokio::time::timeout(within, run)
            .await
            .map_err(|_| "rcon didn't end in time")?
    }

    /// Everything the server wrote to its console so far, within `within`.
    ///
    /// # Errors
    /// A fixed text when Docker didn't answer in time.
    pub(crate) async fn log(&self, within: Duration) -> Result<String, &'static str> {
        let read = tokio::time::timeout(within, self.container.stdout_to_vec())
            .await
            .map_err(|_| "the server's log didn't come in time")?
            .map_err(|_| "the server's log couldn't be read")?;
        Ok(String::from_utf8_lossy(&read).into_owned())
    }

    /// Freezes the server's container, or thaws it: a server that answers
    /// nothing, whatever the bots send.
    ///
    /// # Errors
    /// A fixed text when Docker refused or didn't answer.
    pub(crate) async fn frozen(&self, frozen: bool) -> Result<(), &'static str> {
        let verb = if frozen { "pause" } else { "unpause" };
        match docker(&[verb, self.container.id()]).await {
            Ok(true) => Ok(()),
            Ok(false) => Err("docker refused to pause or thaw the harness"),
            Err(why) => Err(why),
        }
    }

    /// Removes the container and its volumes within `within`; past it the
    /// handle is released undropped, as [`Minecraft::remove`] does.
    ///
    /// [`Minecraft::remove`]: crate::managed_check::minecraft::Minecraft::remove
    ///
    /// # Errors
    /// A fixed text when Docker refused, or didn't answer in time.
    pub(crate) async fn remove(self, within: Duration) -> Result<(), &'static str> {
        match ended_or_released(tokio::time::sleep(within), self.container.rm()).await {
            Some(Ok(())) => Ok(()),
            Some(Err(_)) => Err("Docker refused to remove it"),
            None => Err("Docker didn't remove it in time"),
        }
    }
}

/// Runs `docker` with `args` on a blocking thread, within [`DOCKER`], and
/// says whether it succeeded.
async fn docker(args: &[&str]) -> Result<bool, &'static str> {
    let args: Vec<String> = args.iter().map(|arg| (*arg).to_owned()).collect();
    let run = tokio::task::spawn_blocking(move || {
        std::process::Command::new("docker")
            .args(&args)
            .output()
            .map(|output| output.status.success())
    });
    match tokio::time::timeout(DOCKER, run).await {
        Ok(Ok(Ok(succeeded))) => Ok(succeeded),
        Ok(Ok(Err(_)) | Err(_)) => Err("docker couldn't run"),
        Err(_) => Err("docker didn't answer in time"),
    }
}

/// One of the plugin's lines: `harness-<kind>` and its `key=value` fields,
/// or the answer of `harness state` (kind `state`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    /// What it records: `click`, `bought`, `open`, `state`, …
    pub(crate) kind: String,
    fields: BTreeMap<String, String>,
}

impl Record {
    /// Reads `line`, if it holds a record: the first `harness-<kind> ` (or a
    /// `state player=` answer) and the fields after it. A value is a word,
    /// a text in double quotes, or a list in brackets or braces.
    pub(crate) fn parse(line: &str) -> Option<Self> {
        let (kind, rest) = if let Some(at) = line.find("harness-") {
            let after = line.get(at + "harness-".len()..)?;
            let (kind, rest) = after.split_once(' ').unwrap_or((after, ""));
            (kind.to_owned(), rest)
        } else {
            let at = line.find("state player=")?;
            ("state".to_owned(), line.get(at + "state ".len()..)?)
        };
        if kind.is_empty() || !kind.chars().all(|c| c.is_ascii_lowercase() || c == '-') {
            return None;
        }
        Some(Self {
            kind,
            fields: fields(rest),
        })
    }

    /// The field `key`, unquoted; `None` when the line has none.
    pub(crate) fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    /// The field `key` as a number.
    pub(crate) fn number(&self, key: &str) -> Option<i64> {
        self.get(key)?.parse().ok()
    }

    /// Whether the record is about `player`.
    pub(crate) fn of(&self, player: &str) -> bool {
        self.get("player").or_else(|| self.get("buyer")) == Some(player)
    }
}

/// The `key=value` fields of `text`.
fn fields(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut rest = text.trim_start();
    while let Some((key, after)) = rest.split_once('=') {
        let key = key.trim();
        let (value, next) = value(after);
        if !key.is_empty() && !key.contains(' ') {
            fields.insert(key.to_owned(), value.to_owned());
        }
        rest = next.trim_start();
    }
    fields
}

/// The value at the start of `text` and what follows it.
fn value(text: &str) -> (&str, &str) {
    let close = match text.chars().next() {
        Some('"') => Some('"'),
        Some('[') => Some(']'),
        Some('{') => Some('}'),
        _ => None,
    };
    if let Some(close) = close
        && let Some(end) = text.get(1..).and_then(|inner| inner.find(close))
    {
        let inner = text.get(1..=end).unwrap_or_default();
        let next = text.get(end + 2..).unwrap_or_default();
        // A quoted text without its quotes; a list keeps its brackets.
        return if close == '"' {
            (inner, next)
        } else {
            (text.get(..end + 2).unwrap_or_default(), next)
        };
    }
    text.split_once(' ').unwrap_or((text, ""))
}

/// Every record in `log`, oldest first.
pub(crate) fn records(log: &str) -> Vec<Record> {
    log.lines().filter_map(Record::parse).collect()
}

/// The records of `kind` in `log` about `player`.
pub(crate) fn of_kind<'a>(records: &'a [Record], kind: &str, player: &str) -> Vec<&'a Record> {
    records
        .iter()
        .filter(|record| record.kind == kind && record.of(player))
        .collect()
}

/// The reason Paper logs for an operator's kick.
pub(crate) const KICKED_BY_AN_OPERATOR: &str = "Kicked by an operator";
/// The reason the plugin kicks with at `harness kickon`.
pub(crate) const KICKED_MID_CLICK: &str = "The harness kicked you mid-click.";

/// The disconnects and warnings Paper's log holds for `players`, but those
/// whose reasons are in `caused`: what a case caused itself (an operator's
/// kick, `harness kickon`) and a player who leaves when its bot is stopped.
/// Every other one is a finding: a kick for clicking too fast, a timeout,
/// a warning naming a bot.
pub(crate) fn unasked(log: &str, players: &[&str], caused: &[&str]) -> Vec<String> {
    log.lines()
        .filter(|line| players.iter().any(|player| line.contains(player)))
        .filter(|line| {
            line.contains("lost connection")
                || line.contains("harness-kick ")
                || line.contains("WARN")
                || line.contains("ERROR")
        })
        .filter(|line| !caused.iter().any(|reason| line.contains(reason)))
        .filter(|line| !line.contains("lost connection: Disconnected"))
        .map(str::to_owned)
        .collect()
}

/// The item keys and counts of a `state`'s `items={…}`: `potion:turtle_master` and the rest.
pub(crate) fn items(state: &Record) -> BTreeMap<String, u32> {
    state
        .get("items")
        .map(|items| items.trim_start_matches('{').trim_end_matches('}'))
        .unwrap_or_default()
        .split(", ")
        .filter_map(|entry| {
            let (key, count) = entry.rsplit_once('=')?;
            Some((key.to_owned(), count.parse().ok()?))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_line_reads_into_its_kind_and_fields() {
        let line = r#"[12:00:01 INFO]: [afkfleet-harness] harness-click player=AfkBot1 screen="Auction House" raw=13 slot=13 click=LEFT action=PICKUP_ALL hotbar=-1 cancelled=true window=4 state=7 listing=12 tick=812 nanos=99"#;

        let record = Record::parse(line).unwrap();

        assert_eq!(record.kind, "click");
        assert!(record.of("AfkBot1"));
        assert_eq!(record.get("screen"), Some("Auction House"));
        assert_eq!(record.number("raw"), Some(13));
        assert_eq!(record.number("window"), Some(4));
        assert_eq!(record.number("state"), Some(7));
        assert_eq!(record.get("listing"), Some("12"));
        assert_eq!(record.number("tick"), Some(812));
    }

    #[test]
    fn a_state_answer_reads_its_lists_and_items() {
        let line = "state player=AfkBot1 screen=none cursor=none held=1 item=minecraft:shield raised=true sneaking=false sprinting=false effects=[] items={minecraft:potion:turtle_master=2, minecraft:shield=1}";

        let record = Record::parse(line).unwrap();

        assert_eq!(record.kind, "state");
        assert_eq!(record.get("screen"), Some("none"));
        assert_eq!(record.get("raised"), Some("true"));
        assert_eq!(record.get("effects"), Some("[]"));
        assert_eq!(
            items(&record),
            BTreeMap::from([
                ("minecraft:potion:turtle_master".to_owned(), 2),
                ("minecraft:shield".to_owned(), 1)
            ])
        );
    }

    #[test]
    fn a_purchase_names_its_buyer_and_other_lines_are_no_records() {
        let log = "Done (3.2s)!\n[x] harness-bought id=3 buyer=AfkBot1 seller=Seller1 price=900.00 tick=1 nanos=2\nAfkBot1 joined the game\n";

        let read = records(log);

        assert_eq!(read.len(), 1);
        assert!(read[0].of("AfkBot1"));
        assert_eq!(read[0].get("price"), Some("900.00"));
        assert_eq!(of_kind(&read, "bought", "AfkBot1").len(), 1);
        assert_eq!(of_kind(&read, "bought", "AfkBot2").len(), 0);
    }

    #[test]
    fn a_kick_the_case_caused_is_no_finding_and_any_other_is() {
        let log = "[12:00:00 INFO]: AfkBot1 lost connection: Kicked by an operator\n\
                   [12:00:01 INFO]: AfkBot2 lost connection: Kicked for exceeding packet rate limit\n\
                   [12:00:02 WARN]: AfkBot1 moved too quickly!\n\
                   [12:00:03 INFO]: AfkBot1 lost connection: Disconnected\n\
                   [12:00:04 INFO]: Someone lost connection: Timed out\n";

        let found = unasked(log, &["AfkBot1", "AfkBot2"], &[KICKED_BY_AN_OPERATOR]);

        assert_eq!(found.len(), 2, "{found:#?}");
        assert!(found[0].contains("packet rate"));
        assert!(found[1].contains("moved too quickly"));
    }
}
