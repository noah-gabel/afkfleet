//! What the smoke tests read from docker's output, kept apart from Docker
//! so their own unit tests run with every `just test`.

use fleet_testkit::docker::Finished;

/// Everything the stack and every call printed, which no secret may be in
/// (ADR-0020, S28, S127; ADR-0022, S19), and the secrets to look for.
#[derive(Debug, Default)]
pub(crate) struct Transcript {
    texts: Vec<String>,
    secrets: Vec<String>,
}

impl Transcript {
    /// Keeps `finished`'s stdout and stderr.
    pub(crate) fn keep(&mut self, finished: &Finished) {
        self.texts.push(finished.stdout.clone());
        self.texts.push(finished.stderr.clone());
    }

    /// Keeps `finished`'s stderr alone: its stdout is a secret.
    pub(crate) fn keep_stderr(&mut self, finished: &Finished) {
        self.texts.push(finished.stderr.clone());
    }

    /// Keeps `text`, a log.
    pub(crate) fn keep_text(&mut self, text: String) {
        self.texts.push(text);
    }

    /// Adds `secret`, which nothing kept may hold.
    pub(crate) fn forbid(&mut self, secret: &str) {
        if !secret.is_empty() {
            self.secrets.push(secret.to_owned());
        }
    }

    /// Fails, naming the secret's number and the text's but never either,
    /// when anything kept holds a forbidden secret or a query parameter of
    /// the live events' or the share search's.
    pub(crate) fn assert_clean(&self) {
        for (s, secret) in self.secrets.iter().enumerate() {
            for (t, text) in self.texts.iter().enumerate() {
                assert!(
                    !text.contains(secret.as_str()),
                    "secret #{s} is in kept text #{t}"
                );
            }
        }
        for (t, text) in self.texts.iter().enumerate() {
            for parameter in ["ticket=", "q="] {
                assert!(
                    !has_parameter(text, parameter),
                    "kept text #{t} holds a {parameter} parameter"
                );
            }
        }
    }
}

/// Whether `text` holds the query parameter `parameter` (`ticket=`): after
/// a `?` or an `&`, or `&` in JSON.
fn has_parameter(text: &str, parameter: &str) -> bool {
    ["?", "&", "\\u0026"]
        .iter()
        .any(|before| text.contains(&format!("{before}{parameter}")))
}

/// `docker stats --format '{{.MemPerc}} {{.PIDs}}'`'s line: the memory used
/// as a percentage of the limit, and the process count.
pub(crate) fn stats(line: &str) -> Option<(f64, u64)> {
    let (memory, pids) = line.trim().split_once(' ')?;
    Some((
        memory.trim_end_matches('%').parse().ok()?,
        pids.trim().parse().ok()?,
    ))
}

/// `docker inspect --format '{{range .Mounts}}{{.Type}}:{{.Destination}} {{end}}'`'s
/// mounts, as (type, destination).
pub(crate) fn mounts(line: &str) -> Vec<(String, String)> {
    line.split_whitespace()
        .filter_map(|mount| mount.split_once(':'))
        .map(|(kind, destination)| (kind.to_owned(), destination.to_owned()))
        .collect()
}

/// `/proc/<pid>/status`'s capability sets, by name (`CapEff`), as hex.
pub(crate) fn capabilities(status: &str) -> Vec<(String, String)> {
    status
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.starts_with("Cap"))
        .map(|(name, value)| (name.to_owned(), value.trim().to_owned()))
        .collect()
}

/// A series' value in a Prometheus render, `None` when it isn't there.
pub(crate) fn series(render: &str, name: &str) -> Option<f64> {
    render.lines().find_map(|line| {
        line.strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(' '))
            .and_then(|value| value.trim().parse().ok())
    })
}

/// One line of `afkfleet-server agent list` (its header's `id`, `name` and
/// `state` columns).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentLine {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) state: String,
}

/// `agent list`'s lines after its header; `None` when the header isn't
/// fleet-server's own.
pub(crate) fn agents(list: &str) -> Option<Vec<AgentLine>> {
    let mut lines = list.lines().filter(|line| !line.trim().is_empty());
    if lines.next()? != fleet_server::cli::agent::LIST_HEADER {
        return None;
    }
    lines
        .map(|line| {
            let mut columns = line.split('\t');
            Some(AgentLine {
                id: columns.next()?.to_owned(),
                name: columns.next()?.to_owned(),
                state: columns.next()?.to_owned(),
            })
        })
        .collect()
}

/// The regular files of a ustar stream (`docker cp … -`), by name without a
/// leading `./`, with their bytes.
pub(crate) fn tar_files(tar: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(head) = tar.get(at..at + 512) {
        if head.iter().all(|byte| *byte == 0) {
            break;
        }
        let field = |offset: usize, length: usize| {
            let bytes = head.get(offset..offset + length).unwrap_or_default();
            let end = bytes
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(bytes.len());
            String::from_utf8_lossy(bytes.get(..end).unwrap_or_default()).into_owned()
        };
        let size = usize::from_str_radix(field(124, 12).trim(), 8).unwrap_or(0);
        let name = field(0, 100);
        let kind = head.get(156).copied().unwrap_or(0);
        if kind == b'0' || kind == 0 {
            let data = tar
                .get(at + 512..at + 512 + size)
                .unwrap_or_default()
                .to_vec();
            found.push((name.trim_start_matches("./").to_owned(), data));
        }
        at += 512 + size.div_ceil(512) * 512;
    }
    found
}

/// The stamp a restore or a renewal named, right after `aside-` (a file set
/// aside) or `forget-aside ` (the command it prints), the first one in
/// `text`: never a backup's own time.
pub(crate) fn stamp_in(text: &str) -> Option<String> {
    let is_stamp = |word: &str| {
        word.len() == 16
            && word.get(8..9) == Some("T")
            && word.ends_with('Z')
            && word
                .chars()
                .enumerate()
                .all(|(at, c)| at == 8 || at == 15 || c.is_ascii_digit())
    };
    let mut starts: Vec<usize> = ["aside-", "forget-aside "]
        .iter()
        .flat_map(|marker| {
            text.match_indices(marker)
                .map(move |(at, _)| at + marker.len())
        })
        .collect();
    starts.sort_unstable();
    starts
        .into_iter()
        .filter_map(|start| text.get(start..start + 16))
        .find(|word| is_stamp(word))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(stdout: &str, stderr: &str) -> Finished {
        Finished {
            success: true,
            code: Some(0),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        }
    }

    #[test]
    fn a_clean_transcript_passes() {
        let mut transcript = Transcript::default();
        transcript.keep(&finished("GET /api/v1/events 502", "uri=/api/v1/events"));
        transcript.forbid("hunter2-secret");

        transcript.assert_clean();
    }

    #[test]
    #[should_panic(expected = "secret #0 is in kept text #1")]
    fn a_secret_anywhere_fails_without_being_named() {
        let mut transcript = Transcript::default();
        transcript.keep(&finished("", "the password was hunter2-secret"));
        transcript.forbid("hunter2-secret");

        transcript.assert_clean();
    }

    #[test]
    fn the_secret_stdout_isnt_kept() {
        let mut transcript = Transcript::default();
        transcript.keep_stderr(&finished("bundle-secret", "a token is created"));
        transcript.forbid("bundle-secret");

        transcript.assert_clean();
    }

    #[test]
    #[should_panic(expected = "holds a ticket= parameter")]
    fn a_ticket_in_a_logged_uri_fails() {
        let mut transcript = Transcript::default();
        transcript.keep_text(r#"{"uri":"/api/v1/events?ticket=afk_wt_x"}"#.to_owned());

        transcript.assert_clean();
    }

    #[test]
    #[should_panic(expected = "holds a q= parameter")]
    fn a_q_after_another_parameter_fails() {
        let mut transcript = Transcript::default();
        transcript.keep_text(r#"{"uri":"/x?limit=5&q=pass"}"#.to_owned());

        transcript.assert_clean();
    }

    #[test]
    fn stats_read_percent_and_pids() {
        assert_eq!(stats("12.34% 17\n"), Some((12.34, 17)));
        assert_eq!(stats("--"), None);
    }

    #[test]
    fn mounts_read_type_and_destination() {
        assert_eq!(
            mounts("volume:/data bind:/backups bind:/etc/afkfleet/server.toml \n"),
            [
                ("volume".to_owned(), "/data".to_owned()),
                ("bind".to_owned(), "/backups".to_owned()),
                ("bind".to_owned(), "/etc/afkfleet/server.toml".to_owned()),
            ]
        );
    }

    #[test]
    fn capabilities_read_every_set() {
        let status =
            "Name:\tcaddy\nCapInh:\t0000000000000000\nCapBnd:\t0000000000000400\nNoNewPrivs:\t1\n";

        assert_eq!(
            capabilities(status),
            [
                ("CapInh".to_owned(), "0000000000000000".to_owned()),
                ("CapBnd".to_owned(), "0000000000000400".to_owned()),
            ]
        );
    }

    #[test]
    fn a_series_reads_its_value() {
        let render = "# TYPE a counter\na_total{x=\"y\"} 3\nb 1.5\n";

        assert_eq!(series(render, "a_total{x=\"y\"}"), Some(3.0));
        assert_eq!(series(render, "b"), Some(1.5));
        assert_eq!(series(render, "c"), None);
    }

    #[test]
    fn agents_read_ids_names_and_states_after_the_header() {
        let list = format!(
            "{}\n01900000-0000-7000-8000-000000000001\tlocal\tenabled\t0\t50\t0.1.0\t-\t-\t-\n01900000-0000-7000-8000-000000000002\tops-remote\tdisabled\t0\t50\t-\t-\t-\t-\n",
            fleet_server::cli::agent::LIST_HEADER
        );

        let found = agents(&list).unwrap();

        assert_eq!(
            found,
            [
                AgentLine {
                    id: "01900000-0000-7000-8000-000000000001".to_owned(),
                    name: "local".to_owned(),
                    state: "enabled".to_owned(),
                },
                AgentLine {
                    id: "01900000-0000-7000-8000-000000000002".to_owned(),
                    name: "ops-remote".to_owned(),
                    state: "disabled".to_owned(),
                },
            ]
        );
        assert_eq!(agents("id\tname\n"), None, "another header");
    }

    /// A ustar header for `name` of `size` bytes, `kind` '0' or '5'.
    fn header(name: &str, size: usize, kind: u8) -> Vec<u8> {
        let mut head = vec![0_u8; 512];
        head.get_mut(..name.len())
            .unwrap()
            .copy_from_slice(name.as_bytes());
        let octal = format!("{size:011o}");
        head.get_mut(124..135)
            .unwrap()
            .copy_from_slice(octal.as_bytes());
        *head.get_mut(156).unwrap() = kind;
        head
    }

    #[test]
    fn tar_files_read_each_regular_file_and_skip_folders() {
        let mut tar = header("./", 0, b'5');
        tar.extend(header("./vault-key-1", 3, b'0'));
        let mut data = b"key".to_vec();
        data.resize(512, 0);
        tar.extend(data);
        tar.extend(vec![0_u8; 1024]);

        assert_eq!(
            tar_files(&tar),
            [("vault-key-1".to_owned(), b"key".to_vec())]
        );
    }

    #[test]
    fn a_stamp_is_found_in_a_line_or_a_file_name() {
        assert_eq!(
            stamp_in("moved aside: afkfleet.db as afkfleet.db.aside-20261107T120000Z\n").as_deref(),
            Some("20261107T120000Z")
        );
        assert_eq!(
            stamp_in("then: sh setup.sh forget-aside 20261108T090000Z").as_deref(),
            Some("20261108T090000Z")
        );
        assert_eq!(stamp_in("afkfleet-backup-20261106T000000Z.db"), None);
        assert_eq!(stamp_in("afkfleet.db.aside-2026110T120000Z"), None);
    }
}
